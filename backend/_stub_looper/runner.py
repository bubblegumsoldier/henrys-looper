"""Stub Runner: steps through compiled sections on the engine's beat clock."""
from __future__ import annotations

import threading
from typing import Callable

from .engine.base import Engine


class Runner:
    def __init__(self, engine: Engine, score: dict, on_event: Callable[[dict], None]) -> None:
        self.engine = engine
        self.score = score
        self._emit = on_event
        self._lock = threading.RLock()
        self._bpb = int(score.get("beats_per_bar", 4))
        self._sections: list[dict] = score["sections"]
        self._track_idx = {t["name"]: t["ableton_track"] for t in score["tracks"]}
        self._running = False
        self._connected = False
        self._section: int | None = None       # None while counting in
        self._section_start: int | None = None  # absolute song beat where the section began
        self._bar = 0
        self._beat = 0
        self._pending: str | None = None
        self._tracks: dict[str, str] = {t["name"]: "stop" for t in score["tracks"]}
        self._cb_registered = False

    # -- public API --------------------------------------------------------
    def start(self) -> None:
        with self._lock:
            if self._running:
                self._log("warn", "Runner läuft bereits.")
                return
            if not self._connected:
                self.engine.connect()  # may raise EngineConnectionError
                self._connected = True
            self.engine.set_tempo(float(self.score["bpm"]))
            self.engine.set_quantization_bar()
            if not self._cb_registered:
                self.engine.on_beat(self._on_beat)
                self._cb_registered = True
            self._running = True
            self._section = None
            self._section_start = None
            self._bar, self._beat = 0, 0
            self._pending = None
            self._tracks = {n: "stop" for n in self._tracks}
            self.engine.start_transport()
            self._log("info", f"Start: '{self.score.get('title')}' @ {self.score['bpm']} BPM — Einzähler bis zur nächsten Taktgrenze.")
        self._emit(self.state())

    def next_section(self) -> None:
        with self._lock:
            if not self._running or self._section is None:
                self._log("warn", "Next Section ignoriert: keine Sektion aktiv.")
                return
            if self._pending == "next":
                return
            self._pending = "next"
            self._log("info", f"Wechsel armiert nach Sektion '{self._sections[self._section]['id']}'.")
        self._emit(self.state())

    def stop_all(self) -> None:
        with self._lock:
            self.engine.stop_all()
            self.engine.stop_transport()
            was_running = self._running
            self._running = False
            self._section = None
            self._section_start = None
            self._bar, self._beat = 0, 0
            self._pending = None
            self._tracks = {n: "stop" for n in self._tracks}
        if was_running:
            self._log("info", "Stop All.")
        self._emit(self.state())

    def goto(self, section_id: str) -> None:
        raise NotImplementedError("goto ist in v0 nicht implementiert.")

    def state(self) -> dict:
        with self._lock:
            sec = self._sections[self._section] if self._section is not None else None
            return {
                "type": "state",
                "running": self._running,
                "section_index": self._section,
                "section_id": sec["id"] if sec else None,
                "bars_total": sec["bars"] if sec else None,
                "bar": self._bar,
                "beat": self._beat,
                "pending": self._pending,
                "tracks": dict(self._tracks),
                "connected": self._connected,
                "engine": getattr(self.engine, "name", type(self.engine).__name__.replace("Engine", "").replace("Osc", "").lower()),
            }

    # -- internals ---------------------------------------------------------
    def _log(self, level: str, message: str) -> None:
        self._emit({"type": "log", "level": level, "message": message})

    def _on_beat(self, song_beat: int) -> None:
        state_changed = False
        with self._lock:
            if not self._running:
                return
            beat_in_bar = song_beat % self._bpb + 1
            if self._section is None:
                # counting in: wait for a bar boundary
                self._beat = beat_in_bar
                self._bar = 0
                # song_beat 0 is a boundary too, but we always give one full count-in bar
                if beat_in_bar == 1 and song_beat > 0:
                    self._enter_section(0, song_beat)
                state_changed = True
            else:
                rel = song_beat - (self._section_start or 0)
                bar = rel // self._bpb + 1
                beat = rel % self._bpb + 1
                sec = self._sections[self._section]
                if beat == 1 and rel > 0:
                    # bar boundary inside a running section
                    end_reached = bar > sec["bars"]
                    do_switch = False
                    if self._pending == "next" and (sec["quantize"] == "bar" or end_reached):
                        do_switch = True
                    elif end_reached and sec["autorelease"]:
                        do_switch = True
                    if do_switch:
                        nxt = self._section + 1
                        if nxt < len(self._sections):
                            self._enter_section(nxt, song_beat)
                        else:
                            self._log("info", "Letzte Sektion beendet — Stop All.")
                            self.engine.stop_all()
                            self.engine.stop_transport()
                            self._running = False
                            self._section = None
                            self._bar, self._beat = 0, 0
                            self._pending = None
                            self._tracks = {n: "stop" for n in self._tracks}
                        state_changed = True
                    elif end_reached:
                        # loop the section
                        self._section_start = song_beat
                        self._bar, self._beat = 1, 1
                        state_changed = True
                    else:
                        self._bar, self._beat = bar, beat
                        state_changed = True
                else:
                    self._bar, self._beat = bar, beat
                    state_changed = True
            bar_out, beat_out = self._bar, self._beat
        self._emit({"type": "beat", "song_beat": song_beat, "bar": bar_out, "beat": beat_out})
        if state_changed:
            self._emit(self.state())

    def _enter_section(self, index: int, song_beat: int) -> None:
        sec = self._sections[index]
        self._section = index
        self._section_start = song_beat
        self._bar, self._beat = 1, 1
        self._pending = None
        for name, st in sec["tracks"].items():
            track = self._track_idx[name]
            prev = self._tracks.get(name)
            if st in ("record", "overdub"):
                self.engine.arm(track, True)
                self.engine.set_monitoring(track, "auto")
                self.engine.fire_slot(track, 0)
            elif st == "play":
                self.engine.arm(track, False)
                if prev in ("record", "overdub") or not self.engine.slot_has_clip(track, 0):
                    self.engine.fire_slot(track, 0)  # ends recording / starts playing
                elif prev != "play":
                    self.engine.fire_slot(track, 0)
            elif st == "hear_through":
                self.engine.arm(track, False)
                self.engine.set_monitoring(track, "in")
                self.engine.stop_track(track)
            else:
                self.engine.arm(track, False)
                self.engine.stop_track(track)
            self._tracks[name] = st
        self._log("info", f"Sektion {index + 1}/{len(self._sections)} '{sec['id']}' gestartet ({sec['bars']} Takte).")
