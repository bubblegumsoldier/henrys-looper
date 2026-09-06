"""Runner - steps through the compiled score, driven purely by engine beat callbacks.

State machine: idle -> running (count-in bar) -> section 0 -> ... -> last section (loops)
                idle <- stop_all()  |  next_section() in the last section (quantized stop)

Timing model (no time.sleep for musical timing):
  * Every section change happens on a bar line. Quantized engine commands (fire_slot,
    stop_track, stop_all) are sent LEAD_BEATS before that bar line, because Ableton (and
    SimEngine) execute them on the *next* bar line (Global Quantization = 1 bar) - same
    trick as the M1 spike. Immediate commands (disarm, monitoring) go out exactly on the
    bar line.
  * autorelease: true  -> transition at the end of the section's `bars`.
  * autorelease: false -> the section loops (bar counter restarts) until next_section()
    arms pending "next"; the change is then executed at the end of the current cycle
    (quantize=loop) or on the next bar line (quantize=bar).
  * The last section keeps looping; next_section() there arms pending "stop": all clips
    are stopped at the boundary (same quantize rule), then the runner goes idle.

Hard rule (Ableton test 2026-09-05: a blocking slot_has_clip in the beat callback made a
transition one bar late): nothing in the beat/transition path may wait for the engine.
The only blocking engine query is prime_slots() in start(), before the count-in. The beat
callback is measured; exceeding BEAT_BUDGET_S produces a "warn" log event.

Preflight (Ableton test 2026-09-06: a score referenced tracks 2 and 3 in a 2-track set;
AbletonOSC never answers for a non-existent track, so the Runner ran into timeouts and missed a
bar line): start() asks the engine for the track count *before* it touches tempo, quantization
or the transport. A referenced track that does not exist aborts the start with an EngineError
naming every missing index at once - the runner stays idle. Track names are compared as well,
but a mismatch is only a "warn" log event (wrong numbering is the usual cause).

Slot policy (v0): `record`/`overdub` always create a NEW clip in the next free slot of
the track (first slot without clip, counted from the last slot used in this run). Slot
occupancy is a Runner-side cache: primed once in start() for SLOT_SCAN_SCENES slots per
recording track, then kept current via on_slot_has_clip / on_clip_state callbacks; a slot
fired for recording is marked occupied immediately. The slots for the *next* section are
pre-planned when the current section starts and re-checked (cache lookups only) at
dispatch time. `overdub` on a single track therefore means "new layer replaces the playing
clip" - Ableton plays one clip per track.

Group tracks (M4): a score track with `group:` maps onto an Ableton group track whose child
tracks are the layers (`<group> L1`, `L2`, ... - see looper/session.py). Each layer track
holds exactly ONE clip, in slot 0, so every recorded layer keeps sounding while the next one
is being recorded. `record`/`overdub` take the next free layer track (and finish a layer that
is still recording), `play` keeps every occupied layer looping, `stop` stops all of them,
`hear_through` leaves only the monitor child audible. When the pool runs low the Runner adds
layer tracks from a worker thread BETWEEN sections - never inside the beat callback and never
within TOPUP_GUARD_BEATS of a boundary, because creating a track blocks for ~150 ms and shifts
every track index behind it.
"""

from __future__ import annotations

import threading
import time
from typing import Callable

from .engine.base import Engine, EngineError
from .score.compile import CompiledScore, Section
from .session import GroupPool, add_layer, resolve_pools

LEAD_BEATS = 2              # like the spike: fire inside the last bar, ~2 beats before the bar line
TOPUP_GUARD_BEATS = 2       # no structural change this close to a boundary (M4)
TOPUP_POLL_S = 0.05         # worker thread only - never musical timing
MAX_SCENES = 64             # safety bound when searching the next free slot
SLOT_SCAN_SCENES = 16       # slots per recording track primed in start()
SLOT_SCAN_TIMEOUT_S = 1.5   # blocking only in start(), before the count-in
BEAT_BUDGET_S = 0.010       # max. duration of the beat callback before a warning is logged
BEAT_WARN_COOLDOWN_S = 1.0  # rate limit for budget warnings
STOP = None                 # planned-transition target meaning "stop everything, go idle"


def _enumerate_de(parts: list[str]) -> str:
    """['a', 'b', 'c'] -> "a, b und c" (German enumeration for user-facing messages)."""
    if len(parts) <= 1:
        return parts[0] if parts else ""
    return f"{', '.join(parts[:-1])} und {parts[-1]}"


def _set_size_de(count: int) -> str:
    if count <= 0:
        return "das Ableton-Set hat aber gar keine Spuren"
    if count == 1:
        return "das Ableton-Set hat aber nur 1 Spur (Index 0)"
    return f"das Ableton-Set hat aber nur {count} Spuren (0–{count - 1})"


class Runner:
    def __init__(self, engine: Engine, score: CompiledScore, on_event: Callable[[dict], None]) -> None:
        self.engine = engine
        self.score = score
        self._on_event = on_event
        self._lock = threading.RLock()
        self._callbacks_registered = False

        self._bpb = max(1, int(score.beats_per_bar))
        _, den = score.time_signature_parts
        self._time_signature = (self._bpb, den if den >= 1 else 4)
        self._lead = max(1, min(LEAD_BEATS, self._bpb - 1)) if self._bpb > 1 else 1

        self._running = False
        self._connected = False
        self._countin = False
        self._first_beat: int | None = None
        self._section_index = 0
        self._section_start = 0          # absolute song beat where the current cycle began
        self._last_beat: int | None = None
        self._pending: str | None = None                  # None | "next" | "stop"
        self._planned: tuple[int | None, int] | None = None   # (target section index | STOP, boundary beat)
        self._dispatched_boundary: int | None = None
        self._last_section_notice = False

        names = [t.name for t in score.tracks]
        self._score_tracks = {t.name: t for t in score.tracks}
        self._track_state: dict[str, str] = {n: "stop" for n in names}
        self._track_scene: dict[str, int | None] = {n: None for n in names}
        self._next_free: dict[str, int] = {n: 0 for n in names}
        self._ableton: dict[str, int] = {t.name: t.ableton_track for t in score.tracks
                                         if not t.is_group}
        self._rec_tracks = [n for n in names if n in self._ableton
                            and any(sec.tracks.get(n) in ("record", "overdub") for sec in score.sections)]

        # group tracks: pool of child (layer) tracks, resolved by name in start()
        self._group_names = [t.name for t in score.tracks if t.is_group]
        self._pools: dict[str, GroupPool] = {}
        self._layer_used: dict[str, list[int]] = {n: [] for n in self._group_names}
        self._layer_recording: dict[str, int | None] = {n: None for n in self._group_names}
        self._topup_thread: threading.Thread | None = None
        self._topup_stop = threading.Event()

        # slot occupancy cache (track index, scene) -> has_clip; see module docstring
        self._slot_occupied: dict[tuple[int, int], bool] = {}
        self._slot_unknown_warned: set[tuple[int, int]] = set()
        self._preplanned: dict[str, int] = {}             # track name -> scene for the next section

        # beat-callback budget bookkeeping
        self._last_budget_warn = 0.0
        self._slow_flush: float | None = None

    # ------------------------------------------------------------------ api
    @property
    def engine_name(self) -> str:
        return getattr(self.engine, "name", type(self.engine).__name__.lower())

    def start(self) -> None:
        with self._lock:
            if self._running:
                return
            if not self.score.sections:
                raise EngineError("Die Partitur hat keine Sektionen.")
            if not self._connected:
                self.engine.connect()      # raises EngineConnectionError (German message)
                self._connected = True
            # Before anything is touched in Live: does the set have every referenced track?
            # (raises EngineError; nothing started, transport untouched, runner stays idle)
            track_warnings = self._check_tracks()
            track_warnings.extend(self._resolve_groups())   # raises EngineError if a group is missing
            if not self._callbacks_registered:
                self.engine.on_beat(self._on_beat)
                self.engine.on_clip_state(self._on_clip_state)
                self.engine.on_slot_has_clip(self._on_slot_has_clip)
                self._callbacks_registered = True
            self.engine.set_tempo(float(self.score.bpm))
            self.engine.set_time_signature(*self._time_signature)
            self.engine.set_quantization_bar()
            self.engine.stop_all()
            for name in self._track_state:
                self._track_state[name] = "stop"
            self._section_index = 0
            self._pending = None
            self._planned = None
            self._dispatched_boundary = None
            self._first_beat = None
            self._last_beat = None
            self._last_section_notice = False
            self._slow_flush = None
            self._next_free = {n: 0 for n in self._next_free}
            self._layer_used = {n: [] for n in self._group_names}
            self._layer_recording = {n: None for n in self._group_names}
            self._topup_stop.clear()
            events = [self._log("info", f"Start: {self.score.bpm} BPM, {self._bpb} Beats/Takt, "
                                        f"Einzähler 1 Takt, dann Sektion 1 '{self.score.sections[0].id}'.")]
            events.extend(track_warnings)
            events.extend(self._prime_slots())            # the only blocking query, before the count-in
            self._preplanned, ev = self._preplan(0)
            events.extend(ev)
            self._countin = True
            self._running = True
            self.engine.start_transport()
            events.append(self._state_dict())
        self._flush(events)

    def next_section(self) -> None:
        with self._lock:
            events = []
            if not self._running:
                events.append(self._log("warn", "next_section ignoriert: Runner läuft nicht."))
            elif self._countin:
                events.append(self._log("warn", "next_section ignoriert: noch im Einzähler."))
            elif self._pending is None:
                sec = self._current()
                how = "am Ende des Zyklus" if sec.quantize == "loop" else "an der nächsten Taktgrenze"
                if self._section_index >= len(self.score.sections) - 1:
                    self._pending = "stop"
                    events.append(self._log("info", f"Ende armiert: letzte Sektion '{sec.id}' — "
                                                    f"alle Clips stoppen {how}."))
                else:
                    self._pending = "next"
                    events.append(self._log("info", f"Wechsel armiert: '{sec.id}' → "
                                                    f"'{self.score.sections[self._section_index + 1].id}' {how}."))
                events.append(self._state_dict())
            # already pending -> idempotent
        self._flush(events)

    def stop_all(self) -> None:
        with self._lock:
            events = self._stop_locked(send_stop_all=True, message="Stop: alle Clips gestoppt, Transport aus.")
        self._flush(events)

    def goto(self, section_id: str) -> None:
        raise NotImplementedError("goto ist in v0 nicht implementiert.")

    def state(self) -> dict:
        with self._lock:
            return self._state_dict()

    def handle_midi(self, event: dict) -> None:
        """Match a normalized MIDI event against the score's mapping and trigger actions."""
        if event.get("type") != "midi":
            return
        kind, value, mid = event.get("kind"), int(event.get("value", 0)), event.get("id")
        pressed = (kind == "note_on" and value > 0) or (kind == "cc" and value >= 64)
        if not pressed or not mid:
            return
        for action, binding in self.score.midi.items():
            if binding.get("id") == mid:
                if action == "next_section":
                    self.next_section()
                elif action == "stop_all":
                    self.stop_all()

    # ------------------------------------------------------------ helpers
    def _current(self) -> Section:
        return self.score.sections[self._section_index]

    def _state_dict(self) -> dict:
        sec = self._current()
        if self._last_beat is None or not self._running:
            bar, beat = 0, 0
        elif self._countin:
            bar, beat = 0, (self._last_beat % self._bpb) + 1
        else:
            rel = max(0, self._last_beat - self._section_start)
            bar, beat = rel // self._bpb + 1, rel % self._bpb + 1
        return {
            "type": "state",
            "running": self._running,
            "section_index": self._section_index,
            "section_id": sec.id,
            "bars_total": sec.bars,
            "bar": bar,
            "beat": beat,
            "beats_per_bar": self._bpb,
            "time_signature": f"{self._time_signature[0]}/{self._time_signature[1]}",
            "pending": self._pending,
            "tracks": dict(self._track_state),
            "connected": self._connected,
            "engine": self.engine_name,
            "countin": self._countin,
            "groups": self._group_state(),
        }

    @staticmethod
    def _log(level: str, message: str) -> dict:
        return {"type": "log", "level": level, "message": message}

    def _run(self, cmds: list[tuple]) -> list[dict]:
        """Execute engine commands; failures become log events instead of exceptions."""
        events = []
        for cmd in cmds:
            fn, args = cmd[0], cmd[1:]
            try:
                fn(*args)
            except EngineError as exc:
                events.append(self._log("error", f"Engine-Befehl {fn.__name__}{args} fehlgeschlagen: {exc}"))
            except Exception as exc:  # pragma: no cover
                events.append(self._log("error", f"Engine-Befehl {fn.__name__}{args}: {exc!r}"))
        return events

    def _flush(self, events: list[dict]) -> None:
        for ev in events:
            try:
                self._on_event(ev)
            except Exception:  # pragma: no cover - consumer errors must not break timing
                pass

    def _stop_locked(self, send_stop_all: bool, message: str) -> list[dict]:
        """Go idle: (optionally) stop all clips, disarm, stop transport. Caller holds the lock."""
        was_running = self._running
        self._running = False
        self._countin = False
        self._pending = None
        self._planned = None
        self._dispatched_boundary = None
        self._preplanned = {}
        self._topup_stop.set()
        for name in self._track_state:
            self._track_state[name] = "stop"
        for name in self._group_names:
            self._layer_recording[name] = None
        events = []
        if self._connected:
            cmds: list[tuple] = [(self.engine.stop_all,)] if send_stop_all else []
            cmds += [(self.engine.arm, idx, False)
                     for idx in list(self._ableton.values()) + self._group_layers()]
            cmds.append((self.engine.stop_transport,))
            events.extend(self._run(cmds))
        if was_running:
            events.append(self._log("info", message))
        events.append(self._state_dict())
        return events

    # -------------------------------------------------------- track check
    def _check_tracks(self) -> list[dict]:
        """Verify every `ableton_track` of the score exists in the set (caller holds the lock).

        Raises EngineError listing *all* missing tracks at once - a missing track must never
        surface during playback (Ableton test 2026-09-06: AbletonOSC does not answer for a
        non-existent track, the Runner ran into timeouts and missed a bar line).
        Returns warning events for name mismatches (score name vs. Ableton name), which are
        informative only: renumbered tracks are the most common cause of a wrong recording.
        """
        get_count = getattr(self.engine, "get_track_count", None)
        if not callable(get_count):
            return []                                  # engine without the capability: skip quietly
        try:
            count = int(get_count())
        except EngineError as exc:
            return [self._log("warn", f"Spuranzahl konnte nicht abgefragt werden ({exc}) — die Partitur "
                                      f"wird ungeprüft gestartet.")]
        except Exception as exc:  # pragma: no cover - defensive, an engine bug must not block start
            return [self._log("warn", f"Spuranzahl konnte nicht abgefragt werden ({exc!r}) — die Partitur "
                                      f"wird ungeprüft gestartet.")]

        referenced = sorted(self._ableton.items(), key=lambda kv: kv[1])
        missing = [(name, idx) for name, idx in referenced if not 0 <= idx < count]
        if missing:
            listed = _enumerate_de(["Spur {} ('{}')".format(idx, name) for name, idx in missing])
            raise EngineError(f"Partitur referenziert {listed}, {_set_size_de(count)}. "
                              f"Lege die Spuren in Ableton an oder passe 'ableton_track' an.")

        events: list[dict] = []
        get_name = getattr(self.engine, "get_track_name", None)
        if not callable(get_name):
            return events
        for name, idx in referenced:
            try:
                live_name = get_name(idx)
            except Exception:  # pragma: no cover - the check is a nicety, never a blocker
                continue
            if live_name is None:
                continue
            if str(live_name).strip().casefold() != name.strip().casefold():
                events.append(self._log("warn", f"Spur {idx} heißt in Ableton '{live_name}', in der "
                                                f"Partitur '{name}' — Nummerierung prüfen."))
        return events

    # --------------------------------------------------------- group pools
    def _resolve_groups(self) -> list[dict]:
        """Bind every group track of the score to its Ableton child tracks (caller holds the lock).

        Raises EngineError (German) when the group or its children are missing - the pool is
        prepared by looper.session.ensure_pool() when the score is loaded, so a failure here
        means the set was changed in between.
        """
        if not self._group_names:
            return []
        self._pools = resolve_pools(self.engine, self.score)   # EngineError bubbles up
        events: list[dict] = []
        for name in self._group_names:
            pool = self._pools[name]
            track = self._score_tracks[name]
            if not pool.layers:
                raise EngineError(f"Gruppe '{pool.group}' hat keine Layer-Spur nach der Konvention "
                                  f"'{pool.group} L1', '{pool.group} L2', … Partitur laden legt "
                                  f"sie an; bitte erneut laden.")
            events.append(self._log("info", f"Gruppe '{pool.group}' (Spur {pool.group_index}): "
                                            f"{len(pool.layers)} Layer-Spuren "
                                            f"({', '.join(pool.layer_names)})"
                                            + (f", Monitor auf Spur {pool.monitor_index}"
                                               if pool.monitor_index is not None else
                                               ", ohne Monitor-Spur")))
            if track.monitor and pool.monitor_index is None:
                events.append(self._log("warn", f"Gruppe '{pool.group}': Monitor-Spur "
                                                f"'{pool.group} LIVE' fehlt — hear_through bleibt "
                                                f"ohne Wirkung."))
            if pool.strays:
                events.append(self._log("info", f"Gruppe '{pool.group}': ignoriere Kindspuren ohne "
                                                f"Konventionsnamen ({', '.join(pool.strays)})."))
            if len(pool.layers) < track.pool_size:
                events.append(self._log("warn", f"Gruppe '{pool.group}': nur {len(pool.layers)} von "
                                                f"{track.pool_size} Layer-Spuren — der Runner legt "
                                                f"während des Stücks zwischen den Sektionen nach."))
        return events

    def _group_layers(self) -> list[int]:
        return [idx for pool in self._pools.values() for idx in pool.layer_indices]

    def _free_layers(self, name: str) -> list[int]:
        pool = self._pools.get(name)
        if pool is None:
            return []
        used = self._layer_used.get(name, [])
        return [idx for idx in pool.layer_indices if idx not in used]

    def _group_state(self) -> dict[str, dict[str, int]]:
        out: dict[str, dict[str, int]] = {}
        for name in self._group_names:
            pool = self._pools.get(name)
            if pool is None:
                out[name] = {"layers_used": 0, "layers_free": 0}
                continue
            used = len([i for i in pool.layer_indices if i in self._layer_used.get(name, [])])
            out[name] = {"layers_used": used, "layers_free": len(pool.layer_indices) - used}
        return out

    # ---------------------------------------------------------- slot cache
    def _slot_label(self, track: int, scene: int) -> str:
        name = next((n for n, idx in self._ableton.items() if idx == track), None)
        if name is None:
            for gname, pool in self._pools.items():
                for number, index in pool.layers:
                    if index == track:
                        name = f"{gname}/{pool.group} L{number}"
                        break
                if pool.monitor_index == track:
                    name = f"{gname}/{pool.group} LIVE"
        return f"{name or f'track{track}'}/Slot {scene}"

    def _prime_slots(self) -> list[dict]:
        """Fill the occupancy cache once (blocking, only in start() before the count-in)."""
        events: list[dict] = []
        self._slot_occupied = {}
        self._slot_unknown_warned = set()
        slots = [(self._ableton[n], s) for n in self._rec_tracks for s in range(SLOT_SCAN_SCENES)]
        # group layers carry exactly one clip, in slot 0 - that is the whole scan for them
        slots += [(idx, 0) for idx in self._group_layers()]
        if not slots:
            return events
        t0 = time.perf_counter()
        try:
            result = self.engine.prime_slots(slots, timeout=SLOT_SCAN_TIMEOUT_S)
        except EngineError as exc:
            result = {}
            events.append(self._log("warn", f"Slot-Abfrage fehlgeschlagen ({exc}); alle Slots werden "
                                            f"als frei angenommen."))
        unknown = 0
        for key in slots:
            value = result.get(key)
            if value is None:
                unknown += 1
            else:
                self._slot_occupied[key] = bool(value)
        ms = (time.perf_counter() - t0) * 1000
        occupied = [self._slot_label(t, s) for (t, s), v in sorted(self._slot_occupied.items()) if v]
        events.append(self._log("info", f"Slot-Abfrage: {len(slots)} Slots in {ms:.0f} ms — belegt: "
                                        f"{', '.join(occupied) if occupied else 'keine'}."))
        if unknown:
            events.append(self._log("warn", f"{unknown} von {len(slots)} Slots ohne Antwort (Timeout "
                                            f"{SLOT_SCAN_TIMEOUT_S:.1f} s) — sie werden als frei angenommen; "
                                            f"ein dort vorhandener Clip würde abgespielt statt neu aufgenommen."))
        # a layer track that still holds a clip from an earlier run counts as used
        for name in self._group_names:
            pool = self._pools.get(name)
            if pool is None:
                continue
            self._layer_used[name] = [idx for idx in pool.layer_indices
                                      if self._slot_occupied.get((idx, 0), False)]
            if self._layer_used[name]:
                events.append(self._log("info", f"Gruppe '{pool.group}': "
                                                f"{len(self._layer_used[name])} Layer-Spur(en) haben "
                                                f"schon einen Clip und bleiben belegt."))
        return events

    def _find_free_scene(self, name: str) -> tuple[int, list[dict]]:
        """Cache lookup only - never asks the engine (stays far below BEAT_BUDGET_S)."""
        events: list[dict] = []
        track = self._ableton[name]
        scene = self._next_free[name]
        while scene < MAX_SCENES and self._slot_occupied.get((track, scene), False):
            scene += 1
        key = (track, scene)
        if key not in self._slot_occupied and key not in self._slot_unknown_warned:
            self._slot_unknown_warned.add(key)
            events.append(self._log("warn", f"{self._slot_label(track, scene)} wurde nicht abgefragt — "
                                            f"nehme ihn als frei an."))
        return scene, events

    def _preplan(self, idx: int) -> tuple[dict[str, int], list[dict]]:
        """Pick the record/overdub slots for section `idx` ahead of time (cache lookups only)."""
        plan: dict[str, int] = {}
        events: list[dict] = []
        if idx >= len(self.score.sections):
            return plan, events
        target = self.score.sections[idx]
        parts = []
        for name, st in target.tracks.items():
            if st not in ("record", "overdub"):
                continue
            if name in self._pools:
                free = self._free_layers(name)
                parts.append(f"{name} → Layer-Spur {free[0]}" if free
                             else f"{name} → keine freie Layer-Spur!")
                continue
            scene, ev = self._find_free_scene(name)
            events.extend(ev)
            plan[name] = scene
            parts.append(f"{name} → Slot {scene}")
        if parts:
            events.append(self._log("info", f"Vorausplanung Sektion {idx + 1} '{target.id}': "
                                            + ", ".join(parts)))
        return plan, events

    # --------------------------------------------------------- transitions
    def _plan_quantized(self, target: Section) -> tuple[list[tuple], list[dict]]:
        """Commands to send LEAD beats before the bar line (arm + quantized fires/stops)."""
        cmds: list[tuple] = []
        events: list[dict] = []
        for name, new in target.tracks.items():
            prev = self._track_state[name]
            if name in self._pools:
                cmds.extend(self._plan_group_quantized(name, prev, new, events))
                continue
            t = self._ableton[name]
            if new in ("record", "overdub"):
                scene, ev = self._find_free_scene(name)
                events.extend(ev)
                planned = self._preplanned.get(name)
                if planned is not None and planned != scene:
                    events.append(self._log("info", f"  {name}: abweichend von Vorausplanung "
                                                    f"(Slot {planned}) → Slot {scene}"))
                self._slot_occupied[(t, scene)] = True      # fired for recording -> occupied from now on
                cmds += [(self.engine.arm, t, True), (self.engine.set_monitoring, t, "auto"),
                         (self.engine.fire_slot, t, scene)]
                self._track_scene[name] = scene
                self._next_free[name] = scene + 1
                what = "Overdub-Layer" if new == "overdub" else "Record"
                events.append(self._log("info", f"  {name}: {what} → Slot {scene} (ab Taktgrenze)"))
            elif new == "play":
                scene = self._track_scene[name]
                if prev in ("record", "overdub") and scene is not None:
                    cmds.append((self.engine.fire_slot, t, scene))
                    events.append(self._log("info", f"  {name}: Aufnahme beenden → Loop (Slot {scene})"))
                elif prev in ("stop", "hear_through"):
                    if scene is not None:
                        cmds.append((self.engine.fire_slot, t, scene))
                        events.append(self._log("info", f"  {name}: Play Slot {scene}"))
                    else:
                        events.append(self._log("warn", f"  {name}: 'play', aber noch kein Clip aufgenommen."))
            elif new == "stop":
                if prev != "stop":
                    cmds.append((self.engine.stop_track, t))
                    events.append(self._log("info", f"  {name}: Stop"))
            elif new == "hear_through":
                if prev in ("record", "overdub", "play"):
                    cmds.append((self.engine.stop_track, t))
                events.append(self._log("info", f"  {name}: Hear-Through (Monitoring In)"))
        return cmds, events

    def _plan_group_quantized(self, name: str, prev: str, new: str,
                              events: list[dict]) -> list[tuple]:
        """Quantized commands for one group track (one clip per layer track, always slot 0)."""
        pool = self._pools[name]
        cmds: list[tuple] = []
        recording = self._layer_recording.get(name)
        if new in ("record", "overdub"):
            if recording is not None:
                # a layer that is still recording is closed first -> it keeps looping
                cmds.append((self.engine.fire_slot, recording, 0))
                events.append(self._log("info", f"  {name}: Layer auf Spur {recording} beenden → Loop"))
                self._layer_recording[name] = None
            free = self._free_layers(name)
            if not free:
                events.append(self._log("error", f"  {name}: keine freie Layer-Spur mehr in Gruppe "
                                                 f"'{pool.group}' — diese Aufnahme entfällt. Erhöhe "
                                                 f"'layers'/'reserve' in der Partitur."))
                return cmds
            layer = free[0]
            number = next((n for n, i in pool.layers if i == layer), "?")
            cmds += [(self.engine.arm, layer, True), (self.engine.set_monitoring, layer, "auto"),
                     (self.engine.fire_slot, layer, 0)]
            self._layer_used.setdefault(name, []).append(layer)
            self._layer_recording[name] = layer
            self._slot_occupied[(layer, 0)] = True
            what = "Overdub-Layer" if new == "overdub" else "Record"
            events.append(self._log("info", f"  {name}: {what} → '{pool.group} L{number}' "
                                            f"(Spur {layer}, ab Taktgrenze); "
                                            f"{len(self._free_layers(name))} Layer frei"))
        elif new == "play":
            if recording is not None:
                cmds.append((self.engine.fire_slot, recording, 0))
                events.append(self._log("info", f"  {name}: Aufnahme beenden → Loop (Spur {recording})"))
                self._layer_recording[name] = None
            if prev in ("stop", "hear_through"):
                used = list(self._layer_used.get(name, []))
                if used:
                    cmds += [(self.engine.fire_slot, idx, 0) for idx in used]
                    events.append(self._log("info", f"  {name}: Play — {len(used)} Layer "
                                                    f"(Spuren {', '.join(map(str, used))})"))
                else:
                    events.append(self._log("warn", f"  {name}: 'play', aber noch kein Layer "
                                                    f"aufgenommen."))
        elif new == "stop":
            if prev != "stop":
                cmds += [(self.engine.stop_track, idx) for idx in pool.layer_indices]
                events.append(self._log("info", f"  {name}: Stop (alle Layer der Gruppe "
                                                f"'{pool.group}')"))
        elif new == "hear_through":
            if prev in ("record", "overdub", "play"):
                cmds += [(self.engine.stop_track, idx) for idx in pool.layer_indices]
            events.append(self._log("info", f"  {name}: Hear-Through über "
                                            + (f"'{pool.group} LIVE' (Spur {pool.monitor_index})"
                                               if pool.monitor_index is not None
                                               else "— keine Monitor-Spur vorhanden")))
        return cmds

    def _plan_group_immediate(self, name: str, new: str) -> list[tuple]:
        """Arm/monitoring for one group track, sent exactly on the bar line."""
        pool = self._pools[name]
        cmds: list[tuple] = []
        recording = self._layer_recording.get(name)
        for idx in pool.layer_indices:
            if idx != recording:
                cmds.append((self.engine.arm, idx, False))
        if pool.monitor_index is not None:
            # the monitor child is always the live ear of the group; hear_through makes it explicit
            cmds += [(self.engine.arm, pool.monitor_index, False),
                     (self.engine.set_monitoring, pool.monitor_index, "in")]
        return cmds

    def _plan_immediate(self, target: Section) -> list[tuple]:
        """Commands to send exactly on the bar line (disarm / monitoring)."""
        cmds: list[tuple] = []
        for name, new in target.tracks.items():
            prev = self._track_state[name]
            if name in self._pools:
                cmds.extend(self._plan_group_immediate(name, new))
                continue
            t = self._ableton[name]
            if new in ("play", "stop"):
                cmds.append((self.engine.arm, t, False))
                if prev == "hear_through":
                    cmds.append((self.engine.set_monitoring, t, "auto"))
            elif new == "hear_through":
                cmds += [(self.engine.arm, t, False), (self.engine.set_monitoring, t, "in")]
        return cmds

    # ---------------------------------------------------------- callbacks
    def _on_clip_state(self, track: int, scene: int, state: str) -> None:
        with self._lock:
            if state == "recording":
                self._slot_occupied[(track, scene)] = True
        self._flush([self._log("info", f"Clip {self._slot_label(track, scene)}: {state}")])

    def _on_slot_has_clip(self, track: int, scene: int, has_clip: bool) -> None:
        """Keep the occupancy cache current (has_clip listeners: manual edits in Live, late replies)."""
        events: list[dict] = []
        with self._lock:
            key = (track, scene)
            prev = self._slot_occupied.get(key)
            self._slot_occupied[key] = bool(has_clip)
            if prev is not None and prev != bool(has_clip):
                label = self._slot_label(track, scene)
                if has_clip:
                    events.append(self._log("info", f"Slot {label}: Clip angelegt (wird übersprungen)."))
                else:
                    mine = any(idx == track and self._track_scene[n] == scene
                               for n, idx in self._ableton.items())
                    events.append(self._log("warn" if mine else "info",
                                            f"Slot {label}: Clip entfernt"
                                            + (" — der Track hat nichts mehr zum Abspielen." if mine else ".")))
        self._flush(events)

    def _on_beat(self, song_beat: int) -> None:
        t0 = time.perf_counter()
        events: list[dict] = []
        with self._lock:
            if not self._running:
                return
            self._last_beat = song_beat
            if self._slow_flush is not None:
                slow, self._slow_flush = self._slow_flush, None
                events.extend(self._budget_warn(f"Event-Verarbeitung nach dem letzten Beat dauerte "
                                                f"{slow * 1000:.0f} ms — der Konsument (Backend/UI) bremst "
                                                f"den Runner."))
            if self._countin:
                if self._first_beat is None:
                    self._first_beat = song_beat
                boundary = (self._first_beat // self._bpb + 1) * self._bpb
                if song_beat >= boundary:
                    self._countin = False
                    self._section_start = boundary
                    events.extend(self._enter_section(0, boundary))
                else:
                    if self._dispatched_boundary != boundary and 0 < boundary - song_beat <= self._lead:
                        self._dispatched_boundary = boundary
                        events.extend(self._dispatch_quantized(0))
                    events.append({"type": "beat", "song_beat": song_beat, "bar": 0,
                                   "beat": song_beat % self._bpb + 1})
            if not self._countin:
                events.extend(self._section_beat(song_beat))
            work = time.perf_counter() - t0
            if work > BEAT_BUDGET_S:
                events.extend(self._budget_warn(f"Beat-Callback dauerte {work * 1000:.0f} ms (Budget "
                                                f"{BEAT_BUDGET_S * 1000:.0f} ms) — Taktgrenzen könnten "
                                                f"verpasst werden."))
        self._flush(events)
        flush = time.perf_counter() - t0 - work
        if flush > BEAT_BUDGET_S:
            with self._lock:
                self._slow_flush = flush

    def _budget_warn(self, message: str) -> list[dict]:
        now = time.monotonic()
        if now - self._last_budget_warn < BEAT_WARN_COOLDOWN_S:
            return []
        self._last_budget_warn = now
        return [self._log("warn", message)]

    def _section_beat(self, song_beat: int) -> list[dict]:
        """Section logic for one beat (caller holds the lock): plan, dispatch, cross boundaries."""
        events: list[dict] = []
        sec = self._current()
        cycle_end = self._section_start + sec.bars * self._bpb
        last = self._section_index >= len(self.score.sections) - 1

        # 1) plan a transition if due
        if self._planned is None:
            if not last and sec.autorelease and self._pending is None:
                self._planned = (self._section_index + 1, cycle_end)
            elif self._pending in ("next", "stop"):
                if sec.quantize == "bar":
                    k = -(-(song_beat + self._lead - self._section_start) // self._bpb)  # ceil
                    boundary = max(self._section_start + k * self._bpb, song_beat + 1)
                else:
                    boundary = cycle_end
                target = STOP if self._pending == "stop" else self._section_index + 1
                self._planned = (target, boundary)

        # 2) send quantized commands LEAD beats before the boundary
        if self._planned is not None:
            target, boundary = self._planned
            if self._dispatched_boundary != boundary and 0 < boundary - song_beat <= self._lead:
                self._dispatched_boundary = boundary
                events.extend(self._dispatch_quantized(target))

        # 3) cross the boundary / loop the cycle
        if self._planned is not None and song_beat >= self._planned[1]:
            target, boundary = self._planned
            self._planned = None
            self._pending = None
            if target is STOP:
                events.extend(self._finish(boundary))
                return events
            self._section_start = boundary
            events.extend(self._enter_section(target, boundary))
        elif song_beat >= cycle_end:
            self._section_start = cycle_end
            if last and not self._last_section_notice:
                self._last_section_notice = True
                events.append(self._log("info", f"Letzte Sektion '{sec.id}' loopt weiter — "
                                                f"next_section (N) oder stop_all zum Beenden."))
            elif not last and not sec.autorelease:
                events.append(self._log("info", f"Sektion '{sec.id}' wiederholt (warte auf Release)."))

        rel = song_beat - self._section_start
        events.append({"type": "beat", "song_beat": song_beat,
                       "bar": rel // self._bpb + 1, "beat": rel % self._bpb + 1})
        return events

    def _dispatch_quantized(self, target_idx: int | None) -> list[dict]:
        if target_idx is STOP:
            events = [self._log("info", "Vorbereitung Ende: alle Clips stoppen (ab Taktgrenze)")]
            events.extend(self._run([(self.engine.stop_all,)]))
            return events
        target = self.score.sections[target_idx]
        events = [self._log("info", f"Vorbereitung Sektion {target_idx + 1} '{target.id}':")]
        cmds, ev = self._plan_quantized(target)
        events.extend(ev)
        events.extend(self._run(cmds))
        return events

    def _enter_section(self, idx: int, boundary: int) -> list[dict]:
        target = self.score.sections[idx]
        events = []
        if self._dispatched_boundary != boundary:
            # beat callback skipped the lead window (e.g. lost UDP packet): send late rather than never
            self._dispatched_boundary = boundary
            events.append(self._log("warn", "Taktgrenze ohne Vorlauf erreicht — Befehle werden verspätet "
                                            "gesendet (Wechsel wirkt einen Takt später)."))
            events.extend(self._dispatch_quantized(idx))
        events.extend(self._run(self._plan_immediate(target)))
        self._section_index = idx
        for name, st in target.tracks.items():
            self._track_state[name] = st
        events.append(self._log("info", f"▶ Sektion {idx + 1}/{len(self.score.sections)} '{target.id}' "
                                        f"({target.bars} Takte, "
                                        f"{'autorelease' if target.autorelease else 'warten auf Release'}, "
                                        f"quantize={target.quantize})"))
        self._preplanned, ev = self._preplan(idx + 1)     # look ahead while there is time
        events.extend(ev)
        events.extend(self._maybe_topup())                # between sections only, on a worker thread
        events.append(self._state_dict())
        return events

    # -------------------------------------------------------- layer top-up
    def _safe_for_structure(self) -> bool:
        """True while a blocking structure change cannot collide with a boundary.

        Caller holds the lock. Requires: running, past the count-in, no armed switch, and more
        than TOPUP_GUARD_BEATS + lead beats left before the next boundary.
        """
        if not self._running or self._countin or self._last_beat is None or self._pending:
            return False
        sec = self._current()
        boundary = self._section_start + sec.bars * self._bpb
        if self._planned is not None:
            boundary = min(boundary, self._planned[1])
        return boundary - self._last_beat > TOPUP_GUARD_BEATS + self._lead

    def _maybe_topup(self) -> list[dict]:
        """Start the worker if any group has fewer free layers than its `reserve`."""
        if self._topup_thread is not None and self._topup_thread.is_alive():
            return []
        short = [n for n in self._group_names
                 if len(self._free_layers(n)) < int(self._score_tracks[n].reserve or 0)]
        if not short:
            return []
        self._topup_stop.clear()
        self._topup_thread = threading.Thread(target=self._topup_worker, args=(short,),
                                              name="looper-topup", daemon=True)
        self._topup_thread.start()
        return [self._log("info", f"Layer-Pool wird nachgelegt: {', '.join(short)} "
                                  f"(zwischen den Sektionen, nicht im Beat-Callback).")]

    def _topup_worker(self, names: list[str]) -> None:
        """Blocking engine work off the beat path: add layer tracks until `reserve` is free again."""
        for name in names:
            while not self._topup_stop.is_set():
                with self._lock:
                    if not self._running or name not in self._pools:
                        break
                    track = self._score_tracks[name]
                    if len(self._free_layers(name)) >= int(track.reserve or 0):
                        break
                    safe = self._safe_for_structure()
                if not safe:
                    if self._topup_stop.wait(TOPUP_POLL_S):
                        return
                    continue
                try:
                    layer, index, warnings = add_layer(self.engine, track)
                except EngineError as exc:
                    self._flush([self._log("error", f"Layer-Pool von '{name}' konnte nicht "
                                                    f"erweitert werden: {exc}")])
                    break
                except Exception as exc:  # pragma: no cover - defensive, must never kill the run
                    self._flush([self._log("error", f"Layer-Pool von '{name}': {exc!r}")])
                    break
                events, ok = self._absorb_new_layer(name, layer, index)
                events.extend(self._log("warn", w) for w in warnings)
                self._flush(events)
                if not ok:
                    break        # the structure is unclear now - do not keep hammering it

    def _absorb_new_layer(self, name: str, layer: str, index: int) -> tuple[list[dict], bool]:
        """Adopt a freshly created layer track: shift every index behind it, re-resolve by name.

        The (blocking) structure read happens WITHOUT the runner lock - holding it would stall
        the beat callback for as long as AbletonOSC needs to answer. Returns (events, ok).
        """
        try:
            pools = resolve_pools(self.engine, self.score)
        except EngineError as exc:
            return [self._log("error", f"Struktur nach dem Anlegen von '{layer}' nicht "
                                       f"lesbar: {exc}")], False
        with self._lock:
            self._shift_indices(index)
            self._pools = pools
            free = len(self._free_layers(name))
            events = [self._log("info", f"Layer-Spur '{layer}' angelegt (Spur {index}) — "
                                        f"{free} Layer frei in Gruppe '{self._pools[name].group}'.")]
            events.append(self._state_dict())
            return events, True

    def _shift_indices(self, at: int) -> None:
        """A track was inserted at `at`: every remembered index >= at moves up by one."""
        def shift(i: int) -> int:
            return i + 1 if i >= at else i

        self._ableton = {n: shift(i) for n, i in self._ableton.items()}
        self._slot_occupied = {(shift(t), s): v for (t, s), v in self._slot_occupied.items()}
        self._slot_unknown_warned = {(shift(t), s) for t, s in self._slot_unknown_warned}
        self._layer_used = {n: [shift(i) for i in v] for n, v in self._layer_used.items()}
        self._layer_recording = {n: (None if i is None else shift(i))
                                 for n, i in self._layer_recording.items()}

    def _finish(self, boundary: int) -> list[dict]:
        """Boundary of a pending 'stop' in the last section: clips were stopped (quantized), go idle."""
        events = []
        if self._dispatched_boundary != boundary:
            self._dispatched_boundary = boundary
            events.append(self._log("warn", "Taktgrenze ohne Vorlauf erreicht — Stop wird verspätet gesendet."))
            events.extend(self._dispatch_quantized(STOP))
        events.extend(self._stop_locked(send_stop_all=False,
                                        message="Ende: alle Clips gestoppt, Transport aus."))
        return events
