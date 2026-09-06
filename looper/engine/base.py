"""Engine abstraction (contract v0, docs/contracts-v0.md).

Track indices are 0-based (Ableton order), scene indices are 0-based.
All methods are synchronous. Callbacks registered via on_beat/on_clip_state
are invoked from a background thread of the engine; the Runner serializes.
"""

from __future__ import annotations

import time
from abc import ABC, abstractmethod
from dataclasses import asdict, dataclass, field
from typing import Callable, Iterable


class EngineError(RuntimeError):
    """Generic engine failure (timeouts, invalid commands, ...)."""


class EngineConnectionError(EngineError):
    """Engine could not be reached / connected (message is user-facing, German)."""


MONITORING_MODES = ("in", "auto", "off")
CLIP_STATES = ("recording", "playing", "stopped")


# --------------------------------------------------------------------- session structure
@dataclass
class TrackInfo:
    """One track of the Ableton set as seen from outside (M4 group layer).

    `group_index` is the index of the enclosing group track, or None for a top-level track
    (AbletonOSC's `/live/song/export/structure` calls this field `group_track`).
    """

    index: int
    name: str
    is_group: bool = False
    group_index: int | None = None
    num_devices: int = 0
    input_type: str | None = None
    input_channel: str | None = None
    num_clips: int = 0      # a duplicated track inherits its clips - only copy bare tracks

    def to_dict(self) -> dict:
        return asdict(self)


@dataclass
class SessionStructure:
    """Snapshot of the set's track layout. Indices are only valid until the set changes."""

    tracks: list[TrackInfo] = field(default_factory=list)

    def __iter__(self):
        return iter(self.tracks)

    def __len__(self) -> int:
        return len(self.tracks)

    def __getitem__(self, index: int) -> TrackInfo:
        return self.tracks[index]

    def at(self, index: int) -> TrackInfo | None:
        for t in self.tracks:
            if t.index == index:
                return t
        return None

    def by_name(self, name: str) -> TrackInfo | None:
        """Exact match first, then case/space-insensitive (Live keeps the user's spelling)."""
        for t in self.tracks:
            if t.name == name:
                return t
        folded = name.strip().casefold()
        for t in self.tracks:
            if t.name.strip().casefold() == folded:
                return t
        return None

    def group(self, name: str) -> TrackInfo | None:
        track = self.by_name(name)
        return track if track is not None and track.is_group else None

    def children(self, group_index: int) -> list[TrackInfo]:
        return [t for t in self.tracks if t.group_index == group_index]

    def to_dict(self) -> dict:
        return {"tracks": [t.to_dict() for t in self.tracks]}


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

    # ------------------------------------------------------------- group layer (M4)
    # Structure handling is optional for an engine: the default implementations refuse with a
    # German message so a score without group tracks keeps working on a minimal engine.

    def _unsupported(self, what: str) -> EngineError:
        return EngineError(f"Diese Engine ({type(self).__name__}) kann {what} nicht — "
                           f"Gruppen-Tracks brauchen die Ableton-Engine.")

    def get_session_structure(self) -> SessionStructure:
        """Current track layout of the set (groups, children, devices, input routing)."""
        raise self._unsupported("die Spurstruktur nicht lesen")

    def create_track_in_group(self, group_index: int, after_index: int) -> int:
        """Create an audio track inside group `group_index`, behind `after_index`.

        Returns the *verified* index of the new track; raises EngineError when the set did
        not change as expected (indices shift, Live renames default names silently).
        """
        raise self._unsupported("keine Spuren anlegen")

    def duplicate_track(self, index: int) -> int:
        """Duplicate track `index` (copy lands at index+1, same group); returns its verified index."""
        raise self._unsupported("keine Spuren duplizieren")

    def set_track_name(self, index: int, name: str) -> None:
        """Rename a track and verify by readback."""
        raise self._unsupported("Spuren nicht umbenennen")

    def get_input_routing(self, index: int) -> tuple[str | None, str | None]:
        """(input_routing_type, input_routing_channel) of a track, None where unknown."""
        raise self._unsupported("das Eingangsrouting nicht lesen")

    def set_input_routing(self, index: int, type_name: str | None,
                          channel: str | None = None) -> None:
        """Set input routing type (and channel) by display name; verified by readback."""
        raise self._unsupported("das Eingangsrouting nicht setzen")

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
