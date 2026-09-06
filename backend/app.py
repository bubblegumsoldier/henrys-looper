"""FastAPI backend for the looper prototype (M3).

Wires the ``looper`` package (compiler, runner, engines, MIDI input) to a REST +
WebSocket API as specified in ``docs/contracts-v0.md``. Falls back to the stub
package in ``backend/_stub_looper`` when ``looper`` is not importable or when
``LOOPER_USE_STUB=1`` is set.

Run: ``.venv\\Scripts\\python.exe -m uvicorn backend.app:app --port 8000 --reload``
"""
from __future__ import annotations

import asyncio
import dataclasses
import importlib
import json
import logging
import os
import threading
from contextlib import asynccontextmanager
from pathlib import Path
from typing import Any

from fastapi import FastAPI, HTTPException, WebSocket, WebSocketDisconnect
from fastapi.responses import FileResponse
from fastapi.staticfiles import StaticFiles
from pydantic import BaseModel

log = logging.getLogger("looper.backend")
logging.basicConfig(level=logging.INFO, format="%(asctime)s %(levelname)s %(name)s: %(message)s")

ROOT = Path(__file__).resolve().parent.parent
EXAMPLE_SCORE = ROOT / "examples" / "example-song.yaml"
UI_DIST = ROOT / "ui" / "dist"

DEFAULT_YAML = """\
title: Beispiel-Song
bpm: 100
tracks:
  gitarre: {ableton_track: 0}          # type: single (Default) | group
  voice:   {ableton_track: 1}
midi:
  next_section: {cc: 64}               # oder note: 36; channel optional (Default 1)
  stop_all:     {note: 37}
sections:
  - id: intro
    bars: 4
    autorelease: false                 # false = warten auf next_section, dann am Sektionsende wechseln
    quantize: bar                      # bar | loop
    tracks:
      gitarre: record
  - id: verse
    bars: 4
    autorelease: true
    tracks:
      gitarre: play
      voice: record
  - id: outro
    repeat: verse
    tracks:                            # optionale Overrides
      gitarre: overdub
"""


# --------------------------------------------------------------------------
# looper package (real or stub)
# --------------------------------------------------------------------------

def _load_looper() -> tuple[Any, bool]:
    """Import the real ``looper`` package or the stub. Returns (namespace, is_stub)."""
    use_stub = os.environ.get("LOOPER_USE_STUB") == "1"
    if not use_stub:
        # The real package may be incomplete while M2 is still working on it:
        # require every contract module before committing to it.
        try:
            for mod in ("looper", "looper.score.compile", "looper.runner", "looper.engine.base", "looper.engine.sim", "looper.engine.ableton_osc"):
                importlib.import_module(mod)
        except Exception as e:  # noqa: BLE001 — ImportError or broken module
            log.warning("Paket 'looper' nicht (vollständig) importierbar (%s) — Stub wird verwendet.", e)
            use_stub = True
    base = "backend._stub_looper" if use_stub else "looper"
    ns: dict[str, Any] = {}
    compile_mod = importlib.import_module(f"{base}.score.compile")
    ns["compile_score"] = compile_mod.compile_score
    ns["ScoreError"] = compile_mod.ScoreError
    ns["Runner"] = importlib.import_module(f"{base}.runner").Runner
    ns["SimEngine"] = importlib.import_module(f"{base}.engine.sim").SimEngine
    ns["AbletonOscEngine"] = importlib.import_module(f"{base}.engine.ableton_osc").AbletonOscEngine
    engine_base = importlib.import_module(f"{base}.engine.base")
    ns["EngineConnectionError"] = getattr(engine_base, "EngineConnectionError", Exception)
    ns["EngineError"] = getattr(engine_base, "EngineError", Exception)
    try:
        ns["MidiInput"] = importlib.import_module(f"{base}.midi").MidiInput
    except Exception as e:  # noqa: BLE001 — MIDI is optional (mido / rtmidi may be missing)
        log.warning("MidiInput nicht verfügbar: %s", e)
        ns["MidiInput"] = None
    return type("Looper", (), ns), use_stub


L, USING_STUB = _load_looper()


def to_jsonable(obj: Any) -> Any:
    """Normalize CompiledScore / error objects to plain JSON data."""
    if obj is None or isinstance(obj, (str, int, float, bool)):
        return obj
    if isinstance(obj, dict):
        return {str(k): to_jsonable(v) for k, v in obj.items()}
    if isinstance(obj, (list, tuple)):
        return [to_jsonable(v) for v in obj]
    for meth in ("to_dict", "model_dump", "dict"):
        fn = getattr(obj, meth, None)
        if callable(fn):
            try:
                return to_jsonable(fn())
            except TypeError:
                pass
    if dataclasses.is_dataclass(obj):
        return to_jsonable(dataclasses.asdict(obj))
    if hasattr(obj, "__dict__"):
        return to_jsonable({k: v for k, v in vars(obj).items() if not k.startswith("_")})
    return str(obj)


# --------------------------------------------------------------------------
# Application state
# --------------------------------------------------------------------------

class Hub:
    """Owns engine, runner, MIDI input and the WebSocket broadcast."""

    ENGINES = ("sim", "ableton")

    def __init__(self) -> None:
        self.lock = threading.RLock()
        self.loop: asyncio.AbstractEventLoop | None = None
        self.queue: asyncio.Queue[dict] = asyncio.Queue()
        self.clients: set[WebSocket] = set()
        self.engine_name = "sim"
        self.engine: Any = None
        self.runner: Any = None
        self.score: Any = None
        self.score_json: dict | None = None
        self.yaml_text: str = ""
        self.midi: Any = None
        self.last_state: dict = self._idle_state()

    # -- events ------------------------------------------------------------
    def emit(self, event: dict) -> None:
        """Thread-safe: called from runner/engine/MIDI threads."""
        if event.get("type") == "state":
            event = dict(event)
            event.setdefault("engine", self.engine_name)
            self.last_state = event
        if event.get("type") == "log":
            getattr(log, {"warn": "warning"}.get(event.get("level", "info"), event.get("level", "info")), log.info)(event.get("message"))
        loop = self.loop
        if loop is None or loop.is_closed():
            return
        loop.call_soon_threadsafe(self.queue.put_nowait, event)

    def log(self, level: str, message: str) -> None:
        self.emit({"type": "log", "level": level, "message": message})

    async def broadcaster(self) -> None:
        while True:
            event = await self.queue.get()
            data = json.dumps(to_jsonable(event), ensure_ascii=False)
            dead = []
            for ws in list(self.clients):
                try:
                    await ws.send_text(data)
                except Exception:  # noqa: BLE001
                    dead.append(ws)
            for ws in dead:
                self.clients.discard(ws)

    # -- state -------------------------------------------------------------
    def _idle_state(self) -> dict:
        return {
            "type": "state", "running": False, "section_index": None, "section_id": None,
            "bars_total": None, "bar": 0, "beat": 0, "pending": None, "tracks": {},
            "connected": False, "engine": getattr(self, "engine_name", "sim"),
        }

    def state(self) -> dict:
        with self.lock:
            if self.runner is not None:
                try:
                    st = dict(to_jsonable(self.runner.state()))
                    st.setdefault("type", "state")
                    st.setdefault("engine", self.engine_name)
                    self.last_state = st
                    return st
                except Exception as e:  # noqa: BLE001
                    log.exception("runner.state() fehlgeschlagen: %s", e)
            st = self._idle_state()
            st["engine"] = self.engine_name
            return st

    def is_running(self) -> bool:
        return bool(self.state().get("running"))

    # -- engine / runner ---------------------------------------------------
    def _make_engine(self, name: str) -> Any:
        if name == "sim":
            try:
                bpm = float(self.score_json["bpm"]) if self.score_json else 100.0
                bpb = int(self.score_json.get("beats_per_bar", 4)) if self.score_json else 4
                return L.SimEngine(bpm=bpm, beats_per_bar=bpb)
            except TypeError:
                return L.SimEngine()
        return L.AbletonOscEngine()

    def _close_engine(self) -> None:
        if self.engine is not None:
            try:
                self.engine.close()
            except Exception as e:  # noqa: BLE001
                log.warning("engine.close() fehlgeschlagen: %s", e)
        self.engine = None
        self.runner = None

    def _build_runner(self) -> None:
        """(Re)create engine + runner for the current score. Caller holds the lock."""
        self._close_engine()
        self.engine = self._make_engine(self.engine_name)
        if self.score is not None:
            self.runner = L.Runner(self.engine, self.score, self.emit)
        self.emit(self.state())

    def set_engine(self, name: str) -> None:
        if name not in self.ENGINES:
            raise HTTPException(400, f"Unbekannte Engine '{name}'. Erlaubt: sim, ableton.")
        with self.lock:
            if self.is_running():
                raise HTTPException(409, "Engine-Wechsel nur im Stillstand möglich (erst Stop All).")
            if name == self.engine_name and self.engine is not None:
                return
            previous = self.engine_name
            self.engine_name = name
            self._build_runner()
            if name == "ableton":
                # Probe the connection right away so the UI gets immediate feedback.
                try:
                    self.engine.connect()
                    self.log("info", "Verbindung zu AbletonOSC hergestellt.")
                except L.EngineConnectionError as e:
                    msg = f"Ableton-Engine nicht verfügbar: {e}"
                    self.log("error", msg)
                    self.engine_name = previous
                    self._build_runner()
                    raise HTTPException(503, msg)
                except Exception as e:  # noqa: BLE001
                    msg = f"Ableton-Engine: unerwarteter Fehler beim Verbinden: {e}"
                    log.exception(msg)
                    self.log("error", msg)
                    self.engine_name = previous
                    self._build_runner()
                    raise HTTPException(503, msg)
            else:
                self.log("info", "Engine gewechselt: sim.")
            self.emit(self.state())

    def compile(self, yaml_text: str) -> dict:
        try:
            score = L.compile_score(yaml_text)
        except L.ScoreError as e:
            return {"ok": False, "errors": to_jsonable(getattr(e, "errors", [{"line": None, "column": None, "message": str(e), "suggestion": None}]))}
        return {"ok": True, "score": to_jsonable(score), "_score_obj": score}

    def load(self, yaml_text: str) -> dict:
        with self.lock:
            if self.is_running():
                raise HTTPException(409, "Laden nur im Stillstand möglich (erst Stop All).")
            result = self.compile(yaml_text)
            if not result["ok"]:
                return result
            self.score = result.pop("_score_obj")
            self.score_json = result["score"]
            self.yaml_text = yaml_text
            self._build_runner()
            self.log("info", f"Partitur '{self.score_json.get('title')}' geladen ({len(self.score_json.get('sections', []))} Sektionen).")
            result["state"] = self.state()
            return result

    def transport(self, action: str) -> dict:
        with self.lock:
            if self.runner is None:
                raise HTTPException(409, "Keine Partitur geladen.")
            try:
                if action == "start":
                    self.runner.start()
                elif action == "stop_all":
                    self.runner.stop_all()
                elif action == "next":
                    self.runner.next_section()
                else:
                    raise HTTPException(404, f"Unbekannte Transport-Aktion '{action}'.")
            except HTTPException:
                raise
            except L.EngineConnectionError as e:
                msg = f"Engine nicht erreichbar: {e}"
                self.log("error", msg)
                raise HTTPException(503, msg)
            except NotImplementedError as e:
                raise HTTPException(501, str(e))
            except L.EngineError as e:
                # e.g. the score references a track the Ableton set does not have: the runner
                # refused to start and stayed idle -> a client error with its German message,
                # not a 500. The UI shows `detail` as an error banner.
                msg = str(e)
                self.log("error", msg)
                raise HTTPException(400, msg)
            except Exception as e:  # noqa: BLE001
                log.exception("Transport '%s' fehlgeschlagen", action)
                self.log("error", f"Transport '{action}' fehlgeschlagen: {e}")
                raise HTTPException(500, f"Transport '{action}' fehlgeschlagen: {e}")
            st = self.state()
            self.emit(st)
            return {"ok": True, "state": st}

    # -- MIDI --------------------------------------------------------------
    def start_midi(self) -> None:
        if L.MidiInput is None:
            self.log("warn", "MIDI-Eingang nicht verfügbar (MidiInput fehlt).")
            return
        try:
            try:
                self.midi = L.MidiInput(self.on_midi, on_log=lambda level, msg: self.log(level, f"MIDI: {msg}"))
            except TypeError:
                self.midi = L.MidiInput(self.on_midi)
            start = getattr(self.midi, "start", None) or getattr(self.midi, "open", None)
            if callable(start):
                start()
            self.log("info", "MIDI-Eingang gestartet." + (" (Stub: Fake-Events alle 3 s)" if USING_STUB else ""))
        except Exception as e:  # noqa: BLE001
            self.midi = None
            self.log("warn", f"MIDI-Eingang konnte nicht gestartet werden: {e}")

    def stop_midi(self) -> None:
        if self.midi is None:
            return
        for meth in ("close", "stop"):
            fn = getattr(self.midi, meth, None)
            if callable(fn):
                try:
                    fn()
                except Exception:  # noqa: BLE001
                    pass
                break
        self.midi = None

    def on_midi(self, event: dict) -> None:
        """Broadcast every MIDI event and map it onto transport actions."""
        ev = dict(to_jsonable(event))
        ev.setdefault("type", "midi")
        self.emit(ev)
        # Mapping from the score (note_on / cc with value > 0 triggers)
        if ev.get("kind") == "note_off" or (ev.get("value") in (0, None) and ev.get("kind") != "note_on"):
            return
        midi_map = (self.score_json or {}).get("midi") or {}
        for action, spec in midi_map.items():
            if isinstance(spec, dict) and spec.get("id") == ev.get("id") and self.runner is not None:
                try:
                    if action == "next_section":
                        self.runner.next_section()
                    elif action == "stop_all":
                        self.runner.stop_all()
                except Exception as e:  # noqa: BLE001
                    self.log("error", f"MIDI-Aktion '{action}' fehlgeschlagen: {e}")


hub = Hub()


# --------------------------------------------------------------------------
# FastAPI app
# --------------------------------------------------------------------------

@asynccontextmanager
async def lifespan(_app: FastAPI):
    hub.loop = asyncio.get_running_loop()
    task = asyncio.create_task(hub.broadcaster())
    hub.log("info", f"Backend gestartet — looper-Paket: {'STUB (backend/_stub_looper)' if USING_STUB else 'echt (looper/)'}.")
    # Default score
    yaml_text = DEFAULT_YAML
    if EXAMPLE_SCORE.exists():
        try:
            yaml_text = EXAMPLE_SCORE.read_text(encoding="utf-8")
        except OSError as e:
            log.warning("Beispiel-Partitur nicht lesbar: %s", e)
    hub.yaml_text = yaml_text
    try:
        res = hub.load(yaml_text)
        if not res["ok"]:
            hub.log("error", f"Default-Partitur kompiliert nicht: {res['errors'][0].get('message') if res['errors'] else '?'}")
            with hub.lock:
                hub._build_runner()
    except Exception as e:  # noqa: BLE001
        log.exception("Default-Partitur konnte nicht geladen werden")
        hub.log("error", f"Default-Partitur konnte nicht geladen werden: {e}")
    await asyncio.to_thread(hub.start_midi)
    try:
        yield
    finally:
        hub.stop_midi()
        with hub.lock:
            try:
                if hub.runner is not None and hub.is_running():
                    hub.runner.stop_all()
            except Exception:  # noqa: BLE001
                pass
            hub._close_engine()
        task.cancel()


app = FastAPI(title="Henrys Looper Backend", version="0.1.0", lifespan=lifespan)


class YamlBody(BaseModel):
    yaml: str


class EngineBody(BaseModel):
    engine: str


@app.post("/api/compile")
def api_compile(body: YamlBody) -> dict:
    res = hub.compile(body.yaml)
    res.pop("_score_obj", None)
    return res


@app.post("/api/load")
def api_load(body: YamlBody) -> dict:
    return hub.load(body.yaml)


@app.get("/api/score")
def api_score() -> dict:
    """Currently loaded score (compiled JSON + source YAML) — used by the UI on startup."""
    return {"yaml": hub.yaml_text, "score": hub.score_json, "stub": USING_STUB}


@app.post("/api/transport/{action}")
def api_transport(action: str) -> dict:
    return hub.transport(action)


@app.get("/api/state")
def api_state() -> dict:
    return hub.state()


@app.get("/api/engines")
def api_engines_get() -> dict:
    st = hub.state()
    return {"current": hub.engine_name, "connected": bool(st.get("connected")), "available": list(Hub.ENGINES)}


@app.post("/api/engines")
def api_engines_post(body: EngineBody) -> dict:
    hub.set_engine(body.engine)
    st = hub.state()
    return {"current": hub.engine_name, "connected": bool(st.get("connected")), "available": list(Hub.ENGINES)}


@app.websocket("/ws")
async def ws_endpoint(ws: WebSocket) -> None:
    await ws.accept()
    hub.clients.add(ws)
    try:
        await ws.send_text(json.dumps(to_jsonable(hub.state()), ensure_ascii=False))
        while True:
            # Clients may send pings or transport commands as JSON.
            raw = await ws.receive_text()
            try:
                msg = json.loads(raw)
            except json.JSONDecodeError:
                continue
            if isinstance(msg, dict) and msg.get("type") == "transport" and msg.get("action") in ("start", "stop_all", "next"):
                try:
                    await asyncio.to_thread(hub.transport, msg["action"])
                except HTTPException as e:
                    hub.log("error", str(e.detail))
    except WebSocketDisconnect:
        pass
    except Exception:  # noqa: BLE001
        pass
    finally:
        hub.clients.discard(ws)


# Optional: serve the built UI (ui/dist) when it exists, so the backend alone works too.
if UI_DIST.exists():
    app.mount("/assets", StaticFiles(directory=UI_DIST / "assets"), name="assets")

    @app.get("/")
    def index() -> FileResponse:
        return FileResponse(UI_DIST / "index.html")
