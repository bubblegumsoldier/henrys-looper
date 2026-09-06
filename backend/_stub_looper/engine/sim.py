"""Simulated engine: a thread-based beat clock without Ableton."""
from __future__ import annotations

import threading
import time
from typing import Callable

from .base import Engine


class SimEngine(Engine):
    def __init__(self, bpm: float = 100.0, beats_per_bar: int = 4) -> None:
        self._bpm = float(bpm)
        self._bpb = int(beats_per_bar)
        self._connected = False
        self._running = False
        self._thread: threading.Thread | None = None
        self._stop = threading.Event()
        self._beat_cbs: list[Callable[[int], None]] = []
        self._clip_cbs: list[Callable[[int, int, str], None]] = []
        self._lock = threading.Lock()
        # slot state: (track, scene) -> {"has_clip": bool, "state": str, "pending": str|None}
        self._slots: dict[tuple[int, int], dict] = {}
        self._armed: set[int] = set()

    # -- connection -------------------------------------------------------
    def connect(self) -> None:
        self._connected = True

    def close(self) -> None:
        self.stop_transport()
        self._connected = False

    @property
    def connected(self) -> bool:
        return self._connected

    # -- tempo ------------------------------------------------------------
    def get_tempo(self) -> float:
        return self._bpm

    def set_tempo(self, bpm: float) -> None:
        self._bpm = float(bpm)

    def get_beats_per_bar(self) -> int:
        return self._bpb

    def set_quantization_bar(self) -> None:
        pass

    # -- transport --------------------------------------------------------
    def start_transport(self) -> None:
        if self._running:
            return
        self._stop.clear()
        self._running = True
        self._thread = threading.Thread(target=self._clock, name="sim-clock", daemon=True)
        self._thread.start()

    def stop_transport(self) -> None:
        self._running = False
        self._stop.set()
        t = self._thread
        if t and t.is_alive() and t is not threading.current_thread():
            t.join(timeout=2.0)
        self._thread = None

    def _clock(self) -> None:
        song_beat = 0
        next_tick = time.perf_counter()
        while not self._stop.is_set():
            # bar boundary: resolve pending slot transitions
            if song_beat % self._bpb == 0:
                self._resolve_pending()
            for cb in list(self._beat_cbs):
                try:
                    cb(song_beat)
                except Exception:  # noqa: BLE001 — callbacks must not kill the clock
                    pass
            song_beat += 1
            next_tick += 60.0 / self._bpm
            delay = next_tick - time.perf_counter()
            if delay > 0:
                self._stop.wait(delay)

    def _resolve_pending(self) -> None:
        with self._lock:
            changes = []
            for (track, scene), slot in self._slots.items():
                pend = slot.get("pending")
                if pend:
                    slot["state"] = pend
                    slot["pending"] = None
                    if pend == "recording":
                        slot["has_clip"] = True
                    changes.append((track, scene, pend))
        for track, scene, st in changes:
            for cb in list(self._clip_cbs):
                try:
                    cb(track, scene, st)
                except Exception:  # noqa: BLE001
                    pass

    # -- tracks / clips ---------------------------------------------------
    def arm(self, track: int, on: bool) -> None:
        if on:
            self._armed.add(track)
        else:
            self._armed.discard(track)

    def set_monitoring(self, track: int, mode: str) -> None:
        pass

    def _slot(self, track: int, scene: int) -> dict:
        return self._slots.setdefault((track, scene), {"has_clip": False, "state": "stopped", "pending": None})

    def fire_slot(self, track: int, scene: int) -> None:
        with self._lock:
            slot = self._slot(track, scene)
            if slot["state"] == "recording":
                slot["pending"] = "playing"
            elif not slot["has_clip"]:
                slot["pending"] = "recording"
            else:
                slot["pending"] = "playing"

    def slot_has_clip(self, track: int, scene: int) -> bool:
        with self._lock:
            return self._slot(track, scene)["has_clip"]

    def stop_track(self, track: int) -> None:
        with self._lock:
            for (t, _s), slot in self._slots.items():
                if t == track:
                    slot["pending"] = "stopped" if slot["state"] != "stopped" else None

    def stop_all(self) -> None:
        with self._lock:
            for slot in self._slots.values():
                slot["pending"] = None
                slot["state"] = "stopped"

    # -- callbacks --------------------------------------------------------
    def on_beat(self, cb: Callable[[int], None]) -> None:
        self._beat_cbs.append(cb)

    def on_clip_state(self, cb: Callable[[int, int, str], None]) -> None:
        self._clip_cbs.append(cb)
