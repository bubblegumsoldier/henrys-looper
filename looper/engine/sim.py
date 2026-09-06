"""SimEngine - simulated beat clock and clip slots, no Ableton required.

Semantics (contract v0):
  * a daemon thread ticks once per beat at `bpm` (scaled by `speed` for tests);
  * fire_slot on an empty slot  -> "recording" from the next bar line (has_clip = True);
  * fire_slot on a recording slot -> "playing" from the next bar line;
  * fire_slot on a stopped clip -> "playing" from the next bar line;
  * only one clip per track plays: firing another slot stops the current one;
  * stop_track / stop_all are quantized to the next bar line as well (like Ableton);
  * has_clip changes are reported via on_slot_has_clip; set_clip() is a test hook that
    emulates a clip being created/deleted manually in Live.

Group layer (M4): pass `tracks=[...]` to simulate a real set layout with group tracks and
child tracks. create_track_in_group() / duplicate_track() then behave like Live does:
the new track is INSERTED, every index behind it shifts (slots, arm and monitoring shift
with it) and default names (`3-Audio`) are renumbered silently. Only that makes a test
against the simulation meaningful - code that remembers indices or default names breaks
here exactly as it would in Live.
"""

from __future__ import annotations

import re
import threading
import time
from dataclasses import dataclass
from typing import Callable, Iterable

from .base import (CLIP_STATES, MONITORING_MODES, Engine, EngineError, SessionStructure,
                   TrackInfo)

DEFAULT_TRACK_COUNT = 32    # generous: a simulated set never blocks a score by missing tracks
DEFAULT_TRACK_NAME_RE = re.compile(r"^\d+-(Audio|MIDI)$")


@dataclass
class _SimTrack:
    name: str
    is_group: bool = False
    group_index: int | None = None
    num_devices: int = 0
    input_type: str | None = None
    input_channel: str | None = None


class _Slot:
    __slots__ = ("has_clip", "state")

    def __init__(self) -> None:
        self.has_clip = False
        self.state = "stopped"

    def copy(self) -> "_Slot":
        other = _Slot()
        other.has_clip = self.has_clip
        other.state = self.state
        return other


class SimEngine(Engine):
    name = "sim"

    def __init__(self, bpm: float = 100.0, beats_per_bar: int = 4, speed: float = 1.0,
                 track_count: int | None = None, track_names: list[str] | None = None,
                 tracks: list[dict] | None = None) -> None:
        if bpm <= 0 or beats_per_bar <= 0 or speed <= 0:
            raise ValueError("bpm, beats_per_bar und speed müssen > 0 sein")
        super().__init__()
        # simulated set layout: generous by default so any score just runs; tests narrow it down.
        # Names are unknown (None) unless given - the simulation must not invent a naming scheme
        # that the Runner would then report as a mismatch with the score.
        # `tracks=[{name, is_group, group_index, num_devices, input_type, input_channel}, ...]`
        # additionally simulates the structure (groups, children, index shifts).
        self._structure: list[_SimTrack] | None = None
        if tracks is not None:
            self._structure = [_SimTrack(**dict(t)) for t in tracks]
            track_names = [t.name for t in self._structure]
            track_count = len(self._structure)
        self.track_names: list[str] = list(track_names) if track_names else []
        if track_count is None:
            track_count = len(self.track_names) if self.track_names else DEFAULT_TRACK_COUNT
        self.track_count = max(0, int(track_count))
        self._bpm = float(bpm)
        self._beats_per_bar = int(beats_per_bar)
        self._denominator = 4
        self._speed = float(speed)
        self._lock = threading.RLock()
        self._connected = False
        self._closed = False
        self._beat_cbs: list[Callable[[int], None]] = []
        self._clip_cbs: list[Callable[[int, int, str], None]] = []
        self._slots: dict[tuple[int, int], _Slot] = {}
        self._armed: dict[int, bool] = {}
        self._monitoring: dict[int, str] = {}
        # pending quantized actions, applied on the next bar line
        self._pending: list[tuple] = []
        self._thread: threading.Thread | None = None
        self._stop_evt = threading.Event()
        self._beat = -1
        self.quantization_bar = False

    # ----------------------------------------------------------- lifecycle
    def connect(self) -> None:
        with self._lock:
            if self._closed:
                raise EngineError("SimEngine wurde bereits geschlossen.")
            self._connected = True

    def close(self) -> None:
        with self._lock:
            if self._closed:
                return
            self._closed = True
            self._connected = False
        self.stop_transport()

    @property
    def connected(self) -> bool:
        return self._connected

    # ------------------------------------------------------------- tempo
    def get_tempo(self) -> float:
        return self._bpm

    def set_tempo(self, bpm: float) -> None:
        if bpm <= 0:
            raise EngineError(f"Ungültiges Tempo: {bpm}")
        with self._lock:
            self._bpm = float(bpm)

    def get_beats_per_bar(self) -> int:
        return self._beats_per_bar

    def set_time_signature(self, numerator: int, denominator: int) -> None:
        if int(numerator) < 1 or int(denominator) < 1:
            raise EngineError(f"Ungültige Taktart: {numerator}/{denominator}")
        with self._lock:
            self._beats_per_bar = int(numerator)
            self._denominator = int(denominator)

    @property
    def time_signature(self) -> tuple[int, int]:
        return self._beats_per_bar, self._denominator

    def set_quantization_bar(self) -> None:
        self.quantization_bar = True

    # --------------------------------------------------------- transport
    def start_transport(self) -> None:
        with self._lock:
            if self._thread is not None and self._thread.is_alive():
                return
            self._stop_evt.clear()
            self._beat = -1
            self._thread = threading.Thread(target=self._clock, name="sim-clock", daemon=True)
            self._thread.start()

    def stop_transport(self) -> None:
        with self._lock:
            thread = self._thread
            self._thread = None
        self._stop_evt.set()
        if thread is not None and thread is not threading.current_thread():
            thread.join(timeout=2.0)
        with self._lock:
            for slot in self._slots.values():
                if slot.state != "stopped":
                    slot.state = "stopped"
            self._pending.clear()

    @property
    def transport_running(self) -> bool:
        t = self._thread
        return t is not None and t.is_alive()

    # ------------------------------------------------------------ tracks
    def get_track_count(self) -> int:
        return self.track_count

    def get_track_name(self, index: int) -> str | None:
        index = int(index)
        if 0 <= index < len(self.track_names):
            return self.track_names[index]
        return None            # simulated tracks are nameless unless the test says otherwise

    def arm(self, track: int, on: bool) -> None:
        with self._lock:
            self._armed[int(track)] = bool(on)

    def is_armed(self, track: int) -> bool:
        return self._armed.get(int(track), False)

    def set_monitoring(self, track: int, mode: str) -> None:
        if mode not in MONITORING_MODES:
            raise EngineError(f"Ungültiger Monitoring-Modus: {mode!r}")
        with self._lock:
            self._monitoring[int(track)] = mode

    # ------------------------------------------------------- group layer (M4)
    def _structure_list(self) -> list[_SimTrack]:
        """Lazily promote the flat name/count model to a structure (all tracks top-level)."""
        if self._structure is None:
            self._structure = [
                _SimTrack(self.track_names[i] if i < len(self.track_names) else f"{i + 1}-Audio")
                for i in range(self.track_count)
            ]
        return self._structure

    def _sync_track_lists(self) -> None:
        structure = self._structure or []
        self.track_names = [t.name for t in structure]
        self.track_count = len(structure)

    def _renumber_default_names(self) -> None:
        """Live silently renumbers default names when indices move (3-Audio -> 4-Audio)."""
        for i, track in enumerate(self._structure or []):
            match = DEFAULT_TRACK_NAME_RE.match(track.name)
            if match:
                track.name = f"{i + 1}-{match.group(1)}"

    @staticmethod
    def _shift_key(key: int, at: int) -> int:
        return key + 1 if key >= at else key

    def _insert_track(self, index: int, track: _SimTrack) -> None:
        """Insert like Live does: everything at/behind `index` shifts by one. Caller holds lock."""
        structure = self._structure_list()
        index = max(0, min(int(index), len(structure)))
        structure.insert(index, track)
        for i, t in enumerate(structure):
            if i != index and t.group_index is not None and t.group_index >= index:
                t.group_index += 1
        self._slots = {(self._shift_key(t, index), s): slot for (t, s), slot in self._slots.items()}
        self._armed = {self._shift_key(k, index): v for k, v in self._armed.items()}
        self._monitoring = {self._shift_key(k, index): v for k, v in self._monitoring.items()}
        self._pending = [self._shift_action(a, index) for a in self._pending]
        self._renumber_default_names()
        self._sync_track_lists()

    def _shift_action(self, action: tuple, index: int) -> tuple:
        if action[0] == "fire":
            return ("fire", self._shift_key(action[1], index), action[2])
        if action[0] == "stop_track":
            return ("stop_track", self._shift_key(action[1], index))
        return action

    def get_session_structure(self) -> SessionStructure:
        with self._lock:
            clips: dict[int, int] = {}
            for (track, _scene), slot in self._slots.items():
                if slot.has_clip:
                    clips[track] = clips.get(track, 0) + 1
            return SessionStructure([
                TrackInfo(i, t.name, t.is_group, t.group_index, t.num_devices,
                          t.input_type, t.input_channel, clips.get(i, 0))
                for i, t in enumerate(self._structure_list())
            ])

    def create_track_in_group(self, group_index: int, after_index: int) -> int:
        with self._lock:
            structure = self._structure_list()
            group_index = int(group_index)
            if not 0 <= group_index < len(structure) or not structure[group_index].is_group:
                raise EngineError(f"Spur {group_index} ist keine Gruppenspur — es kann keine "
                                  f"Kindspur darin angelegt werden.")
            children = [i for i, t in enumerate(structure) if t.group_index == group_index]
            if not children:
                raise EngineError(f"Gruppe '{structure[group_index].name}' hat keine Kindspur — "
                                  f"Ableton kann in eine leere Gruppe nichts einfügen.")
            # spike: the first child slot is not addressable, the track lands ON the asked index
            index = max(int(after_index) + 1, group_index + 2)
            index = min(index, max(children) + 1)
            self._insert_track(index, _SimTrack(f"{index + 1}-Audio", False, group_index))
            self._renumber_default_names()
            self._sync_track_lists()
            return index

    def duplicate_track(self, index: int) -> int:
        with self._lock:
            structure = self._structure_list()
            index = int(index)
            if not 0 <= index < len(structure):
                raise EngineError(f"Spur {index} gibt es nicht — sie kann nicht dupliziert werden.")
            source = structure[index]
            if source.is_group:
                raise EngineError(f"Spur {index} ('{source.name}') ist eine Gruppenspur — für den "
                                  f"Layer-Pool wird eine Kindspur dupliziert, keine Gruppe.")
            copy = _SimTrack(source.name, False, source.group_index, source.num_devices,
                             source.input_type, source.input_channel)
            armed, monitoring = self._armed.get(index, False), self._monitoring.get(index)
            clips = {s: slot.copy() for (t, s), slot in self._slots.items() if t == index}
            self._insert_track(index + 1, copy)
            for scene, slot in clips.items():          # a duplicate inherits arm, input and clips
                self._slots[(index + 1, scene)] = slot
            self._armed[index + 1] = armed
            if monitoring is not None:
                self._monitoring[index + 1] = monitoring
            return index + 1

    def set_track_name(self, index: int, name: str) -> None:
        with self._lock:
            structure = self._structure_list()
            if not 0 <= int(index) < len(structure):
                raise EngineError(f"Spur {index} gibt es nicht — sie kann nicht umbenannt werden.")
            structure[int(index)].name = str(name)
            self._sync_track_lists()

    def get_input_routing(self, index: int) -> tuple[str | None, str | None]:
        with self._lock:
            structure = self._structure_list()
            if not 0 <= int(index) < len(structure):
                return None, None
            track = structure[int(index)]
            return track.input_type, track.input_channel

    def set_input_routing(self, index: int, type_name: str | None,
                          channel: str | None = None) -> None:
        with self._lock:
            structure = self._structure_list()
            if not 0 <= int(index) < len(structure):
                raise EngineError(f"Spur {index} gibt es nicht — Eingang nicht setzbar.")
            track = structure[int(index)]
            if type_name:
                track.input_type = str(type_name)
            if channel:
                track.input_channel = str(channel)

    def get_monitoring(self, track: int) -> str:
        return self._monitoring.get(int(track), "auto")

    def _slot(self, track: int, scene: int) -> _Slot:
        key = (int(track), int(scene))
        slot = self._slots.get(key)
        if slot is None:
            slot = self._slots[key] = _Slot()
        return slot

    def fire_slot(self, track: int, scene: int) -> None:
        with self._lock:
            self._pending.append(("fire", int(track), int(scene)))

    def slot_has_clip(self, track: int, scene: int) -> bool:
        with self._lock:
            return self._slot(track, scene).has_clip

    def slot_state(self, track: int, scene: int) -> str:
        with self._lock:
            return self._slot(track, scene).state

    def prime_slots(self, slots: Iterable[tuple[int, int]], timeout: float = 1.5
                    ) -> dict[tuple[int, int], bool | None]:
        with self._lock:
            return {(int(t), int(s)): self._slot(t, s).has_clip for t, s in slots}

    def set_clip(self, track: int, scene: int, has_clip: bool) -> None:
        """Test hook: a clip appears in / disappears from a slot (as if edited by hand in Live)."""
        states: list[tuple[int, int, str]] = []
        with self._lock:
            slot = self._slot(track, scene)
            changed = slot.has_clip != bool(has_clip)
            slot.has_clip = bool(has_clip)
            if not has_clip:
                self._set_state((int(track), int(scene)), "stopped", states)
            clip_cbs = list(self._clip_cbs)
        if changed:
            self._emit_slot_has_clip(int(track), int(scene), bool(has_clip))
        for t, sc, st in states:
            for cb in clip_cbs:
                cb(t, sc, st)

    def stop_track(self, track: int) -> None:
        with self._lock:
            self._pending.append(("stop_track", int(track)))

    def stop_all(self) -> None:
        with self._lock:
            self._pending.append(("stop_all",))

    # --------------------------------------------------------- callbacks
    def on_beat(self, cb: Callable[[int], None]) -> None:
        with self._lock:
            self._beat_cbs.append(cb)

    def on_clip_state(self, cb: Callable[[int, int, str], None]) -> None:
        with self._lock:
            self._clip_cbs.append(cb)

    # ------------------------------------------------------------- clock
    def _clock(self) -> None:
        start = time.perf_counter()
        n = 0
        while not self._stop_evt.is_set():
            # schedule next beat by wall clock (this sleep *is* the clock, not musical logic)
            with self._lock:
                seconds_per_beat = 60.0 / self._bpm / self._speed
            target = start + n * seconds_per_beat
            delay = target - time.perf_counter()
            if delay > 0 and self._stop_evt.wait(delay):
                return
            self._tick(n)
            n += 1

    def _tick(self, beat: int) -> None:
        changes: list[tuple[int, int, str]] = []
        new_clips: list[tuple[int, int]] = []
        with self._lock:
            self._beat = beat
            if beat % self._beats_per_bar == 0 and self._pending:
                pending, self._pending = self._pending, []
                for action in pending:
                    changes.extend(self._apply(action, new_clips))
            beat_cbs = list(self._beat_cbs)
            clip_cbs = list(self._clip_cbs)
        for cb in beat_cbs:
            try:
                cb(beat)
            except Exception:  # pragma: no cover - callbacks must not kill the clock
                pass
        for track, scene in new_clips:
            self._emit_slot_has_clip(track, scene, True)
        for track, scene, state in changes:
            for cb in clip_cbs:
                try:
                    cb(track, scene, state)
                except Exception:  # pragma: no cover
                    pass

    def _set_state(self, key: tuple[int, int], state: str, changes: list) -> None:
        assert state in CLIP_STATES
        slot = self._slots[key]
        if slot.state != state:
            slot.state = state
            changes.append((key[0], key[1], state))

    def _apply(self, action: tuple, new_clips: list[tuple[int, int]]) -> list[tuple[int, int, str]]:
        changes: list[tuple[int, int, str]] = []
        kind = action[0]
        if kind == "fire":
            _, track, scene = action
            slot = self._slot(track, scene)
            # one clip per track: stop other slots of this track
            for key, other in self._slots.items():
                if key[0] == track and key[1] != scene and other.state != "stopped":
                    self._set_state(key, "stopped", changes)
            if not slot.has_clip:
                if self._armed.get(track, False):
                    slot.has_clip = True
                    new_clips.append((track, scene))
                    self._set_state((track, scene), "recording", changes)
                # firing an empty slot on an unarmed track does nothing (like Ableton)
            elif slot.state == "recording":
                self._set_state((track, scene), "playing", changes)
            else:
                self._set_state((track, scene), "playing", changes)
        elif kind == "stop_track":
            _, track = action
            for key in self._slots:
                if key[0] == track:
                    self._set_state(key, "stopped", changes)
        elif kind == "stop_all":
            for key in self._slots:
                self._set_state(key, "stopped", changes)
        return changes
