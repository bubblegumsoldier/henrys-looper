"""Engine abstraction (contract v0, docs/contracts-v0.md).

Track indices are 0-based (Ableton order), scene indices are 0-based.
All methods are synchronous. Callbacks registered via on_beat/on_clip_state
are invoked from a background thread of the engine; the Runner serializes.
"""

from __future__ import annotations

import time
from abc import ABC, abstractmethod
from typing import Callable, Iterable


class EngineError(RuntimeError):
    """Generic engine failure (timeouts, invalid commands, ...)."""


class EngineConnectionError(EngineError):
    """Engine could not be reached / connected (message is user-facing, German)."""


MONITORING_MODES = ("in", "auto", "off")
CLIP_STATES = ("recording", "playing", "stopped")


class Engine(ABC):
    def __init__(self) -> None:
        self._has_clip_cbs: list[Callable[[int, int, bool], None]] = []

    @abstractmethod
    def connect(self) -> None:
        """Connect; blocks at most 2 s; raises EngineConnectionError."""

    @abstractmethod
    def close(self) -> None:
        """Disconnect, idempotent, at most 2 s."""

    @abstractmethod
    def get_tempo(self) -> float: ...

    @abstractmethod
    def set_tempo(self, bpm: float) -> None: ...

    @abstractmethod
    def get_beats_per_bar(self) -> int: ...

    @abstractmethod
    def get_track_count(self) -> int:
        """Number of tracks in the set; valid track indices are 0 .. count-1.

        The Runner calls this once in start() to reject a score that references a track
        the set does not have (AbletonOSC simply does not answer for such indices).
        """

    @abstractmethod
    def get_track_name(self, index: int) -> str | None:
        """Name of track `index`, or None if it is unknown / not answered.

        Used for a non-fatal plausibility check only, so implementations must never
        raise on a missing or slow answer - they return None instead.
        """

    @abstractmethod
    def set_time_signature(self, numerator: int, denominator: int) -> None:
        """Set the song time signature; get_beats_per_bar() returns `numerator` afterwards."""

    @abstractmethod
    def set_quantization_bar(self) -> None:
        """Global (clip trigger) quantization = 1 bar."""

    @abstractmethod
    def start_transport(self) -> None: ...

    @abstractmethod
    def stop_transport(self) -> None: ...

    @abstractmethod
    def arm(self, track: int, on: bool) -> None: ...

    @abstractmethod
    def set_monitoring(self, track: int, mode: str) -> None:
        """mode: "in" | "auto" | "off"."""

    @abstractmethod
    def fire_slot(self, track: int, scene: int) -> None:
        """Record (empty slot + armed) or play / end recording; quantized by the engine."""

    @abstractmethod
    def slot_has_clip(self, track: int, scene: int) -> bool: ...

    @abstractmethod
    def stop_track(self, track: int) -> None: ...

    @abstractmethod
    def stop_all(self) -> None:
        """Stop all clips (transport keeps running)."""

    @abstractmethod
    def on_beat(self, cb: Callable[[int], None]) -> None:
        """Register callback receiving the absolute song beat (int), once per beat."""

    @abstractmethod
    def on_clip_state(self, cb: Callable[[int, int, str], None]) -> None:
        """Register callback (track, scene, "recording"|"playing"|"stopped")."""

    # ---------------------------------------------------------------- slot occupancy
    # Non-blocking slot bookkeeping for the Runner: the only blocking call is prime_slots(),
    # which the Runner uses once in start() (before the count-in, outside musical timing).
    # Afterwards the Runner keeps its own cache current via on_slot_has_clip callbacks.

    def _has_clip_callbacks(self) -> list[Callable[[int, int, bool], None]]:
        cbs = getattr(self, "_has_clip_cbs", None)
        if cbs is None:
            cbs = self._has_clip_cbs = []
        return cbs

    def on_slot_has_clip(self, cb: Callable[[int, int, bool], None]) -> None:
        """Register callback (track, scene, has_clip) fired whenever a slot gains/loses its clip."""
        self._has_clip_callbacks().append(cb)

    def _emit_slot_has_clip(self, track: int, scene: int, has_clip: bool) -> None:
        for cb in list(self._has_clip_callbacks()):
            try:
                cb(int(track), int(scene), bool(has_clip))
            except Exception:  # pragma: no cover - callbacks must not break the engine
                pass

    def prime_slots(self, slots: Iterable[tuple[int, int]], timeout: float = 1.5
                    ) -> dict[tuple[int, int], bool | None]:
        """Query has_clip for many slots at once and subscribe to their changes.

        Blocks at most `timeout` seconds in total. Unanswered slots map to None.
        Default implementation: sequential slot_has_clip() until the deadline.
        """
        result: dict[tuple[int, int], bool | None] = {}
        deadline = time.monotonic() + max(0.0, float(timeout))
        for track, scene in slots:
            key = (int(track), int(scene))
            if time.monotonic() >= deadline:
                result[key] = None
                continue
            try:
                result[key] = bool(self.slot_has_clip(*key))
            except EngineError:
                result[key] = None
        return result
