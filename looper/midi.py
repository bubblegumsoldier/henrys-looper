"""MidiInput - reads MIDI controllers directly via mido/python-rtmidi (not through Ableton).

Emits normalized events (contract v0):
  {"type":"midi","device":"MPD218","channel":1,"kind":"note_on"|"note_off"|"cc",
   "number":36,"value":127,"id":"ch1.note36"}
Runs cleanly without any MIDI device (no ports -> no events, no error).
Windows: MIDI devices are opened exclusively - a controller used here must not be
active in Ableton at the same time.
"""

from __future__ import annotations

import threading
from typing import Callable, Iterable


def midi_id(channel: int, kind: str, number: int) -> str:
    """Build the mapping id used in the score: ch<channel>.note<n> | ch<channel>.cc<n>."""
    prefix = "cc" if kind == "cc" else "note"
    return f"ch{int(channel)}.{prefix}{int(number)}"


def normalize_message(msg, device: str) -> dict | None:
    """Convert a mido message into the contract event dict (None for unsupported types)."""
    mtype = getattr(msg, "type", None)
    if mtype in ("note_on", "note_off"):
        kind, number, value = mtype, msg.note, msg.velocity
    elif mtype == "control_change":
        kind, number, value = "cc", msg.control, msg.value
    else:
        return None
    channel = int(msg.channel) + 1  # mido is 0-based, the score uses 1..16
    return {
        "type": "midi",
        "device": device,
        "channel": channel,
        "kind": kind,
        "number": int(number),
        "value": int(value),
        "id": midi_id(channel, kind, number),
    }


def list_input_names() -> list[str]:
    try:
        import mido
        return list(mido.get_input_names())
    except Exception:
        return []


class MidiInput:
    """Opens all available MIDI inputs (or the given `ports`) and forwards events to `on_event`."""

    def __init__(self, on_event: Callable[[dict], None], ports: Iterable[str] | None = None,
                 on_log: Callable[[str, str], None] | None = None) -> None:
        self._on_event = on_event
        self._on_log = on_log or (lambda level, msg: None)
        self._lock = threading.Lock()
        self._ports: dict[str, object] = {}
        self._closed = False
        try:
            import mido
        except Exception as exc:  # pragma: no cover - mido is a hard dependency
            self._on_log("warn", f"MIDI nicht verfügbar (mido fehlt): {exc}")
            return
        try:
            names = list(ports) if ports is not None else list(mido.get_input_names())
        except Exception as exc:
            self._on_log("warn", f"MIDI-Geräte konnten nicht aufgelistet werden: {exc}")
            names = []
        for name in names:
            try:
                port = mido.open_input(name, callback=self._make_callback(name))
            except Exception as exc:
                self._on_log("warn", f"MIDI-Eingang '{name}' konnte nicht geöffnet werden "
                                     f"(exklusiv in Ableton belegt?): {exc}")
                continue
            self._ports[name] = port
            self._on_log("info", f"MIDI-Eingang geöffnet: {name}")
        if not self._ports:
            self._on_log("info", "Kein MIDI-Eingang gefunden — Steuerung per Tastatur/UI.")

    @property
    def ports(self) -> list[str]:
        return list(self._ports.keys())

    def _make_callback(self, name: str):
        def _cb(msg):
            event = normalize_message(msg, name)
            if event is None or self._closed:
                return
            try:
                self._on_event(event)
            except Exception:  # pragma: no cover - never let a consumer kill the MIDI thread
                pass
        return _cb

    def close(self) -> None:
        with self._lock:
            if self._closed:
                return
            self._closed = True
            ports, self._ports = self._ports, {}
        for name, port in ports.items():
            try:
                port.close()
            except Exception:  # pragma: no cover
                pass

    def __enter__(self) -> "MidiInput":
        return self

    def __exit__(self, *exc) -> None:
        self.close()
