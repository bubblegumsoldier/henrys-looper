"""Stub AbletonOscEngine: never talks to Ableton, always fails to connect.

Used only to exercise the backend's error path (EngineConnectionError -> log +
HTTP 503) while the real ``looper`` package is being built.
"""
from __future__ import annotations

import time
from typing import Callable

from .base import Engine, EngineConnectionError


class AbletonOscEngine(Engine):
    def __init__(self, host: str = "127.0.0.1", port: int = 11000, reply_port: int = 11001) -> None:
        self.host, self.port, self.reply_port = host, port, reply_port

    def connect(self) -> None:
        time.sleep(0.5)  # pretend to wait for a reply
        raise EngineConnectionError(
            "Keine Antwort von AbletonOSC (UDP 11000/11001). Läuft Ableton Live und ist "
            "AbletonOSC als Control Surface aktiviert? (Stub-Engine, keine echte Verbindung)"
        )

    def close(self) -> None:
        pass

    def get_tempo(self) -> float:
        return 120.0

    def set_tempo(self, bpm: float) -> None:
        pass

    def get_beats_per_bar(self) -> int:
        return 4

    def set_quantization_bar(self) -> None:
        pass

    def start_transport(self) -> None:
        pass

    def stop_transport(self) -> None:
        pass

    def arm(self, track: int, on: bool) -> None:
        pass

    def set_monitoring(self, track: int, mode: str) -> None:
        pass

    def fire_slot(self, track: int, scene: int) -> None:
        pass

    def slot_has_clip(self, track: int, scene: int) -> bool:
        return False

    def stop_track(self, track: int) -> None:
        pass

    def stop_all(self) -> None:
        pass

    def on_beat(self, cb: Callable[[int], None]) -> None:
        pass

    def on_clip_state(self, cb: Callable[[int, int, str], None]) -> None:
        pass
