"""Engine interface (stub copy of the contract in docs/contracts-v0.md)."""
from __future__ import annotations

from abc import ABC, abstractmethod
from typing import Callable


class EngineError(Exception):
    """Generic engine failure."""


class EngineConnectionError(EngineError):
    """Raised by ``connect()`` when the engine backend is unreachable."""


class Engine(ABC):
    @abstractmethod
    def connect(self) -> None: ...

    @abstractmethod
    def close(self) -> None: ...

    @abstractmethod
    def get_tempo(self) -> float: ...

    @abstractmethod
    def set_tempo(self, bpm: float) -> None: ...

    @abstractmethod
    def get_beats_per_bar(self) -> int: ...

    @abstractmethod
    def set_quantization_bar(self) -> None: ...

    @abstractmethod
    def start_transport(self) -> None: ...

    @abstractmethod
    def stop_transport(self) -> None: ...

    @abstractmethod
    def arm(self, track: int, on: bool) -> None: ...

    @abstractmethod
    def set_monitoring(self, track: int, mode: str) -> None: ...

    @abstractmethod
    def fire_slot(self, track: int, scene: int) -> None: ...

    @abstractmethod
    def slot_has_clip(self, track: int, scene: int) -> bool: ...

    @abstractmethod
    def stop_track(self, track: int) -> None: ...

    @abstractmethod
    def stop_all(self) -> None: ...

    @abstractmethod
    def on_beat(self, cb: Callable[[int], None]) -> None: ...

    @abstractmethod
    def on_clip_state(self, cb: Callable[[int, int, str], None]) -> None: ...
