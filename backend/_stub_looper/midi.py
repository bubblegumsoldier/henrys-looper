"""Stub MidiInput: emits a fake normalized MIDI event every 3 seconds."""
from __future__ import annotations

import itertools
import threading
from typing import Callable

_FAKE = [
    ("note_on", 36, 127), ("note_off", 36, 0), ("cc", 64, 127), ("cc", 64, 0), ("note_on", 37, 100), ("note_off", 37, 0),
]


class MidiInput:
    def __init__(self, on_event: Callable[[dict], None], device: str | None = None, interval: float = 3.0) -> None:
        self._on_event = on_event
        self._device = device or "Stub-Pad"
        self._interval = interval
        self._stop = threading.Event()
        self._thread: threading.Thread | None = None

    def start(self) -> None:
        if self._thread:
            return
        self._thread = threading.Thread(target=self._run, name="stub-midi", daemon=True)
        self._thread.start()

    def close(self) -> None:
        self._stop.set()
        if self._thread and self._thread.is_alive():
            self._thread.join(timeout=1.0)
        self._thread = None

    def _run(self) -> None:
        for kind, number, value in itertools.cycle(_FAKE):
            if self._stop.wait(self._interval):
                return
            channel = 1
            tag = "cc" if kind == "cc" else "note"
            self._on_event({
                "type": "midi", "device": self._device, "channel": channel, "kind": kind,
                "number": number, "value": value, "id": f"ch{channel}.{tag}{number}",
            })
