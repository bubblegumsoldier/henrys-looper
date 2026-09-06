"""Group-track pool: make sure every `group:` track of the score has its child tracks.

Model (handover 7, decided after the OSC spike of 2026-09-06): one logical track of the
score is an Ableton **group track**. The effects sit on the group, every overdub layer
gets its **own bare child track** (so layers stack without limit and all of them sound at
once), and one child is a pure live monitor. Naming convention, binding:

    <group> L1, <group> L2, ...   layer tracks (exactly one clip each, in slot 0)
    <group> LIVE                  monitor track (never records)

`ensure_pool()` only ever *grows* the pool. It never deletes and never renames tracks it
did not create - deleting the last child would make Live dissolve the group, and Henry's
own naming is the only thing that identifies a track reliably (Live silently renumbers
default names like `3-Audio` whenever indices move).

Groups themselves cannot be created over OSC. A missing group is therefore an error with
an instruction for Henry, not something the app fixes.
"""

from __future__ import annotations

import re
from dataclasses import asdict, dataclass, field
from typing import Any

from .engine.base import Engine, EngineError, SessionStructure, TrackInfo
from .score.compile import CompiledScore, Track

MONITOR_SUFFIX = "LIVE"
LAYER_PREFIX = "L"


def layer_name(group: str, number: int) -> str:
    return f"{group} {LAYER_PREFIX}{number}"


def monitor_name(group: str) -> str:
    return f"{group} {MONITOR_SUFFIX}"


def _layer_re(group: str) -> re.Pattern[str]:
    return re.compile(rf"^{re.escape(group.strip())}\s+{LAYER_PREFIX}(\d+)$", re.IGNORECASE)


def layer_number(group: str, name: str) -> int | None:
    """Layer number of `name` under the convention, or None if it is not a layer track."""
    match = _layer_re(group).match((name or "").strip())
    return int(match.group(1)) if match else None


def is_monitor(group: str, name: str) -> bool:
    return (name or "").strip().casefold() == monitor_name(group).casefold()


def missing_group_message(group: str) -> str:
    return (f"Gruppe '{group}' existiert nicht im Ableton-Set. Lege sie in Ableton an "
            f"(Spuren markieren, Strg+G) und benenne sie '{group}'.")


def empty_group_message(group: str) -> str:
    return (f"Gruppe '{group}' hat keine Kindspur. Lege in Ableton eine Audiospur in der "
            f"Gruppe an (Spuren markieren, Strg+G) und benenne sie '{layer_name(group, 1)}'.")


# --------------------------------------------------------------------------- #
# Resolved pool
# --------------------------------------------------------------------------- #
@dataclass
class GroupPool:
    """The child tracks of one Ableton group track, resolved to current indices."""

    track: str                       # score track name
    group: str                       # Ableton group track name
    group_index: int
    layers: list[tuple[int, int]] = field(default_factory=list)   # (layer number, track index)
    monitor_index: int | None = None
    strays: list[str] = field(default_factory=list)   # children that follow no convention

    @property
    def layer_indices(self) -> list[int]:
        return [index for _, index in self.layers]

    @property
    def layer_names(self) -> list[str]:
        return [layer_name(self.group, number) for number, _ in self.layers]

    def to_dict(self) -> dict[str, Any]:
        return {"track": self.track, "group": self.group, "group_index": self.group_index,
                "layers": [{"number": n, "index": i} for n, i in self.layers],
                "monitor_index": self.monitor_index, "strays": list(self.strays)}


def resolve_group(structure: SessionStructure, track: Track) -> GroupPool:
    """Find the group of `track` and its child tracks. Raises EngineError (German) if absent."""
    group_name = str(track.group)
    group = structure.by_name(group_name)
    if group is None:
        raise EngineError(missing_group_message(group_name))
    if not group.is_group:
        raise EngineError(f"Spur '{group.name}' (Index {group.index}) ist keine Gruppenspur. "
                          f"{missing_group_message(group_name)}")
    children = structure.children(group.index)
    if not children:
        raise EngineError(empty_group_message(group_name))

    pool = GroupPool(track.name, group.name, group.index)
    for child in children:
        number = layer_number(group.name, child.name)
        if number is not None:
            pool.layers.append((number, child.index))
        elif is_monitor(group.name, child.name):
            pool.monitor_index = child.index
        else:
            pool.strays.append(child.name)
    pool.layers.sort()
    return pool


def simulated_set(score: CompiledScore) -> list[dict] | None:
    """A plausible SimEngine layout for `score` - group scores must be rehearsable without Ableton.

    Single tracks sit at their `ableton_track` index, every group track gets its group plus a
    fully populated pool. Returns None when the score has no group track (then the SimEngine
    default of "plenty of nameless tracks" is the better simulation).
    """
    if not any(t.is_group for t in score.tracks):
        return None
    singles = {int(t.ableton_track): t.name for t in score.tracks
               if not t.is_group and t.ableton_track is not None}
    size = max(singles) + 1 if singles else 0
    tracks: list[dict] = [{"name": singles.get(i, f"{i + 1}-Audio"), "input_type": "Ext. In",
                           "input_channel": "1"} for i in range(size)]
    for track in score.tracks:
        if not track.is_group:
            continue
        group_index = len(tracks)
        tracks.append({"name": str(track.group), "is_group": True, "num_devices": 2})
        child = {"group_index": group_index, "input_type": "Ext. In", "input_channel": "1"}
        if track.monitor:
            tracks.append({"name": monitor_name(str(track.group)), **child})
        tracks += [{"name": layer_name(str(track.group), n), **child}
                   for n in range(1, track.pool_size + 1)]
    return tracks


def resolve_pools(engine: Engine, score: CompiledScore) -> dict[str, GroupPool]:
    """Resolve every group track of the score against one fresh structure snapshot."""
    group_tracks = [t for t in score.tracks if t.is_group]
    if not group_tracks:
        return {}
    structure = engine.get_session_structure()
    return {t.name: resolve_group(structure, t) for t in group_tracks}


# --------------------------------------------------------------------------- #
# Report
# --------------------------------------------------------------------------- #
@dataclass
class GroupPoolResult:
    track: str
    group: str
    group_index: int
    existing_layers: list[str] = field(default_factory=list)
    created_layers: list[str] = field(default_factory=list)
    monitor: str | None = None
    monitor_created: bool = False
    layer_indices: list[int] = field(default_factory=list)
    monitor_index: int | None = None
    strays: list[str] = field(default_factory=list)
    warnings: list[str] = field(default_factory=list)

    @property
    def message(self) -> str:
        parts = [f"Gruppe {self.group}: {len(self.existing_layers)} Layer-Spur"
                 f"{'en' if len(self.existing_layers) != 1 else ''} vorhanden"]
        if self.created_layers:
            parts.append(f"{len(self.created_layers)} angelegt ({', '.join(self.created_layers)})")
        else:
            parts.append("nichts angelegt")
        if self.monitor_created:
            parts.append(f"Monitor-Spur '{self.monitor}' angelegt")
        elif self.monitor:
            parts.append(f"Monitor-Spur '{self.monitor}' vorhanden")
        else:
            parts.append("ohne Monitor-Spur")
        return ", ".join(parts) + "."

    def to_dict(self) -> dict[str, Any]:
        data = asdict(self)
        data["message"] = self.message
        return data


@dataclass
class PoolReport:
    groups: list[GroupPoolResult] = field(default_factory=list)

    @property
    def changed(self) -> bool:
        return any(g.created_layers or g.monitor_created for g in self.groups)

    @property
    def messages(self) -> list[str]:
        out = [g.message for g in self.groups]
        for g in self.groups:
            out.extend(g.warnings)
        return out

    def to_dict(self) -> dict[str, Any]:
        return {"changed": self.changed, "groups": [g.to_dict() for g in self.groups],
                "messages": self.messages}


# --------------------------------------------------------------------------- #
# ensure_pool
# --------------------------------------------------------------------------- #
def ensure_pool(engine: Engine, score: CompiledScore) -> PoolReport:
    """Make sure every group track of the score has `layers + reserve` layer child tracks.

    Grows only. After every structural change the structure is read back and the new track
    is verified by its name - indices shift and Live renames default names silently.
    """
    report = PoolReport()
    group_tracks = [t for t in score.tracks if t.is_group]
    if not group_tracks:
        return report
    structure = engine.get_session_structure()
    for track in group_tracks:
        result, structure = _ensure_group(engine, structure, track)
        report.groups.append(result)
    return report


def _ensure_group(engine: Engine, structure: SessionStructure,
                  track: Track) -> tuple[GroupPoolResult, SessionStructure]:
    pool = resolve_group(structure, track)
    result = GroupPoolResult(track.name, pool.group, pool.group_index,
                             existing_layers=list(pool.layer_names), strays=list(pool.strays))
    target = track.pool_size
    used_numbers = {number for number, _ in pool.layers}

    while len(pool.layers) < target:
        number = next(n for n in range(1, target + len(used_numbers) + 2) if n not in used_numbers)
        used_numbers.add(number)
        name = layer_name(pool.group, number)
        structure = _add_child(engine, structure, pool, name, result)
        pool = resolve_group(structure, track)
        result.created_layers.append(name)

    if track.monitor and pool.monitor_index is None:
        name = monitor_name(pool.group)
        structure = _add_child(engine, structure, pool, name, result, monitor=True)
        pool = resolve_group(structure, track)
        result.monitor = name
        result.monitor_created = True
    elif pool.monitor_index is not None:
        result.monitor = monitor_name(pool.group)

    result.layer_indices = pool.layer_indices
    result.monitor_index = pool.monitor_index
    _apply_child_settings(engine, pool, track, result)
    return result, structure


def add_layer(engine: Engine, track: Track) -> tuple[str, int, list[str]]:
    """Append exactly one more layer child track to the group of `track`.

    Blocking (structure read, create/duplicate, rename, read back) - the Runner calls this
    from a worker thread between sections, never from the beat callback. Returns
    (name, index, warnings).
    """
    structure = engine.get_session_structure()
    pool = resolve_group(structure, track)
    used = {number for number, _ in pool.layers}
    number = next(n for n in range(1, len(used) + 2) if n not in used)
    name = layer_name(pool.group, number)
    result = GroupPoolResult(track.name, pool.group, pool.group_index)
    structure = _add_child(engine, structure, pool, name, result)
    info = structure.by_name(name)
    if info is None:
        raise EngineError(f"Layer-Spur '{name}' ist nach dem Anlegen nicht auffindbar.")
    return name, info.index, result.warnings


def _bare_reference(structure: SessionStructure, pool: GroupPool) -> TrackInfo | None:
    """A child that can safely be duplicated: no devices, no clips (a copy inherits both)."""
    children = structure.children(pool.group_index)
    bare = [c for c in children if c.num_devices == 0 and getattr(c, "num_clips", 0) == 0]
    layers = [c for c in bare if layer_number(pool.group, c.name) is not None]
    candidates = layers or [c for c in bare if c.index != pool.monitor_index] or bare
    return max(candidates, key=lambda c: c.index) if candidates else None


def _routing_reference(engine: Engine, structure: SessionStructure,
                       pool: GroupPool) -> tuple[str | None, str | None]:
    """Input routing of an existing child - a new layer must hear the same thing."""
    order = pool.layer_indices + ([pool.monitor_index] if pool.monitor_index is not None else [])
    for index in order:
        info = structure.at(index)
        if info is not None and info.input_type:
            return info.input_type, info.input_channel
        try:
            in_type, in_channel = engine.get_input_routing(index)
        except EngineError:
            continue
        if in_type:
            return in_type, in_channel
    return None, None


def _add_child(engine: Engine, structure: SessionStructure, pool: GroupPool, name: str,
               result: GroupPoolResult, monitor: bool = False) -> SessionStructure:
    """Create one child track, name it immediately and verify it. Returns the new structure."""
    reference = _bare_reference(structure, pool)
    in_type, in_channel = _routing_reference(engine, structure, pool)
    last_child = max([c.index for c in structure.children(pool.group_index)], default=pool.group_index)
    if reference is not None:
        index = engine.duplicate_track(reference.index)
        how = f"Kopie von '{reference.name}'"
    else:
        index = engine.create_track_in_group(pool.group_index, last_child)
        how = "neue Audiospur"
    engine.set_track_name(index, name)

    structure = engine.get_session_structure()
    info = structure.at(index)
    if info is None or info.name.strip().casefold() != name.strip().casefold() \
            or info.group_index != pool.group_index:
        raise EngineError(f"Spur '{name}' ({how}) liegt nach dem Anlegen nicht wie erwartet in "
                          f"Gruppe '{pool.group}'. Bitte in Ableton nachsehen und erneut laden.")

    if in_type:
        try:
            engine.set_input_routing(index, in_type, in_channel)
        except EngineError as exc:
            result.warnings.append(f"Eingang von '{name}' konnte nicht auf '{in_type}' gesetzt "
                                   f"werden ({exc}) — bitte in Ableton von Hand zuweisen.")
    else:
        result.warnings.append(f"'{name}' hat keinen Eingang bekommen: keine der bestehenden "
                               f"Kindspuren von '{pool.group}' meldet ein Eingangsrouting.")
    _set_child_mode(engine, index, monitor, name, result)
    return structure


def _set_child_mode(engine: Engine, index: int, monitor: bool, name: str,
                    result: GroupPoolResult) -> None:
    try:
        engine.set_monitoring(index, "in" if monitor else "auto")
        engine.arm(index, False)
    except EngineError as exc:
        result.warnings.append(f"Monitoring/Arm von '{name}' konnte nicht gesetzt werden ({exc}).")


def _apply_child_settings(engine: Engine, pool: GroupPool, track: Track,
                          result: GroupPoolResult) -> None:
    """Layer tracks: Monitoring auto, disarmed. Monitor track: Monitoring In, disarmed."""
    for number, index in pool.layers:
        _set_child_mode(engine, index, False, layer_name(pool.group, number), result)
    if pool.monitor_index is not None:
        _set_child_mode(engine, pool.monitor_index, True, monitor_name(pool.group), result)
