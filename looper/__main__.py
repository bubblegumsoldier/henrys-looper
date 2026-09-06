"""CLI: python -m looper compile <file> | run <file> --engine sim|ableton

run: non-blocking keyboard (msvcrt on Windows): Enter / n = next section, s = stop_all,
q = quit. Ctrl+C -> cleanup in < 3 s. All output is flushed immediately.
"""

from __future__ import annotations

import argparse
import json
import sys
import threading
import time
from pathlib import Path

from .engine.base import EngineConnectionError, EngineError
from .score.compile import ScoreError, compile_score

EXIT_OK = 0
EXIT_SCORE_ERROR = 1
EXIT_ENGINE_ERROR = 2
EXIT_USAGE = 3


def _utf8_stdio() -> None:
    for stream in (sys.stdout, sys.stderr):
        if hasattr(stream, "reconfigure"):
            try:
                stream.reconfigure(encoding="utf-8", errors="replace")
            except Exception:  # pragma: no cover
                pass


def out(msg: str = "") -> None:
    print(msg, flush=True)


def _read(path: str) -> str:
    p = Path(path)
    if not p.is_file():
        out(f"Datei nicht gefunden: {path}")
        sys.exit(EXIT_USAGE)
    return p.read_text(encoding="utf-8")


# ----------------------------------------------------------------------------- compile
def cmd_compile(args) -> int:
    text = _read(args.file)
    try:
        score = compile_score(text)
    except ScoreError as exc:
        if args.json:
            out(json.dumps({"ok": False, "errors": exc.errors}, ensure_ascii=False, indent=2))
        else:
            out(f"Partitur '{args.file}' hat {len(exc.errors)} Fehler:")
            for issue in exc.issues:
                out("  " + issue.format())
        return EXIT_SCORE_ERROR
    if args.json:
        out(json.dumps({"ok": True, "score": score.to_dict()}, ensure_ascii=False, indent=2))
    else:
        out(score.to_json())
    return EXIT_OK


# --------------------------------------------------------------------------------- run
class _Keys:
    """Non-blocking single-key reader (msvcrt on Windows, line-reader thread elsewhere)."""

    def __init__(self) -> None:
        self._queue: list[str] = []
        self._lock = threading.Lock()
        try:
            import msvcrt  # noqa: F401
            self._msvcrt = msvcrt
        except ImportError:  # pragma: no cover - POSIX fallback
            self._msvcrt = None
            threading.Thread(target=self._stdin_reader, daemon=True).start()

    def _stdin_reader(self) -> None:  # pragma: no cover
        for line in sys.stdin:
            with self._lock:
                self._queue.append(line.strip()[:1] or "\r")

    def poll(self) -> list[str]:
        keys: list[str] = []
        if self._msvcrt is not None:
            while self._msvcrt.kbhit():
                keys.append(self._msvcrt.getwch())
        else:  # pragma: no cover
            with self._lock:
                keys, self._queue = self._queue, []
        return keys


def _make_printer(verbose_beats: bool):
    last_state = {}

    def on_event(ev: dict) -> None:
        t = ev.get("type")
        if t == "beat":
            if ev["bar"] == 0:
                out(f"    Einzähler … Beat {ev['beat']}")
            elif verbose_beats or ev["beat"] == 1:
                sec = last_state.get("section_id", "?")
                total = last_state.get("bars_total", "?")
                pend = "  [Wechsel armiert]" if last_state.get("pending") else ""
                out(f"    [{sec}] Takt {ev['bar']}/{total} Beat {ev['beat']}{pend}")
        elif t == "state":
            last_state.update(ev)
            tracks = ", ".join(f"{k}={v}" for k, v in ev["tracks"].items())
            out(f"STATE running={ev['running']} sektion={ev['section_index'] + 1}:{ev['section_id']} "
                f"takt={ev['bar']}/{ev['bars_total']} pending={ev['pending']} | {tracks}")
        elif t == "log":
            prefix = {"info": "", "warn": "WARNUNG: ", "error": "FEHLER: "}.get(ev["level"], "")
            out(f"{prefix}{ev['message']}")
        elif t == "midi":
            out(f"MIDI {ev['device']}: {ev['id']} = {ev['value']}")
    return on_event


def cmd_run(args) -> int:
    from .midi import MidiInput
    from .runner import Runner

    text = _read(args.file)
    try:
        score = compile_score(text)
    except ScoreError as exc:
        out(f"Partitur '{args.file}' hat {len(exc.errors)} Fehler:")
        for issue in exc.issues:
            out("  " + issue.format())
        return EXIT_SCORE_ERROR

    if args.engine == "sim":
        from .engine.sim import SimEngine
        engine = SimEngine(bpm=score.bpm, beats_per_bar=score.beats_per_bar, speed=args.speed)
    else:
        from .engine.ableton_osc import AbletonOscEngine
        engine = AbletonOscEngine(host=args.host, send_port=args.send_port, reply_port=args.reply_port)

    on_event = _make_printer(args.beats)
    runner = Runner(engine, score, on_event)

    def on_midi(ev: dict) -> None:
        on_event(ev)
        runner.handle_midi(ev)

    midi = MidiInput(on_midi, on_log=lambda level, msg: on_event({"type": "log", "level": level, "message": msg}))
    keys = _Keys()
    out(f"Partitur '{score.title or args.file}' — {len(score.sections)} Sektionen, Engine: {args.engine}")
    out("Tasten: Enter/n = nächste Sektion, s = Stop All, q = Beenden (Ctrl+C geht auch)")
    code = EXIT_OK
    try:
        if args.engine == "ableton" and any(t.is_group for t in score.tracks):
            from .session import ensure_pool
            engine.connect()
            for message in ensure_pool(engine, score).messages:
                out(message)
        runner.start()
        while True:
            for key in keys.poll():
                k = key.lower()
                if k in ("\r", "\n", "n", " "):
                    runner.next_section()
                elif k == "s":
                    runner.stop_all()
                elif k == "q" or key == "\x03":
                    raise KeyboardInterrupt
            time.sleep(0.03)  # UI poll interval only - musical timing comes from engine beats
    except KeyboardInterrupt:
        out("\nBeende …")
    except EngineConnectionError as exc:
        out(f"Verbindung fehlgeschlagen: {exc}")
        code = EXIT_ENGINE_ERROR
    except EngineError as exc:
        out(f"Engine-Fehler: {exc}")
        code = EXIT_ENGINE_ERROR
    finally:
        t0 = time.monotonic()
        midi.close()
        try:
            runner.stop_all()
        except Exception as exc:  # pragma: no cover
            out(f"stop_all beim Beenden fehlgeschlagen: {exc}")
        try:
            engine.close()
        except Exception as exc:  # pragma: no cover
            out(f"close beim Beenden fehlgeschlagen: {exc}")
        out(f"Aufgeräumt in {time.monotonic() - t0:.2f} s.")
    return code


# -------------------------------------------------------------------------------- main
def build_parser() -> argparse.ArgumentParser:
    p = argparse.ArgumentParser(prog="python -m looper", description="Looper: Partitur kompilieren / abspielen")
    sub = p.add_subparsers(dest="command", required=True)

    c = sub.add_parser("compile", help="YAML-Partitur validieren und als JSON ausgeben")
    c.add_argument("file")
    c.add_argument("--json", action="store_true", help="{ok, score|errors}-Hülle ausgeben (für Tools)")
    c.set_defaults(func=cmd_compile)

    r = sub.add_parser("run", help="Partitur mit Engine abspielen")
    r.add_argument("file")
    r.add_argument("--engine", choices=("sim", "ableton"), default="sim")
    r.add_argument("--host", default="127.0.0.1")
    r.add_argument("--send-port", type=int, default=11000)
    r.add_argument("--reply-port", type=int, default=11001)
    r.add_argument("--speed", type=float, default=1.0, help="nur sim: Zeitraffer-Faktor")
    r.add_argument("--beats", action="store_true", help="jeden Beat ausgeben (Standard: nur Taktanfang)")
    r.set_defaults(func=cmd_run)
    return p


def main(argv: list[str] | None = None) -> int:
    _utf8_stdio()
    args = build_parser().parse_args(argv)
    return args.func(args)


if __name__ == "__main__":
    sys.exit(main())
