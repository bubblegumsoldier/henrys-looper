"""ensure_pool() against SimEngine's simulated group tracks (M4).

The simulation shifts every index behind an inserted track and renumbers default names
(`3-Audio` -> `4-Audio`) exactly like Live does, so these tests fail if the pool code ever
remembers an index or a default name across a structural change.
"""

import pytest

from looper.engine.base import EngineError
from looper.engine.sim import SimEngine
from looper.score.compile import compile_score
from looper.session import (ensure_pool, layer_name, monitor_name, resolve_group,
                            resolve_pools)


def score_for(layers: int = 2, reserve: int = 1, monitor: bool = True, group: str = "voice"):
    mon = "true" if monitor else "false"
    return compile_score(f"""
bpm: 100
tracks:
  gitarre: {{ableton_track: 0}}
  voice:   {{group: {group}, layers: {layers}, reserve: {reserve}, monitor: {mon}}}
sections:
  - {{id: a, bars: 2, tracks: {{voice: record}}}}
""")


def henry_set(children: list[str] | None = None, devices_on_group: int = 4) -> SimEngine:
    """Henry's layout: 0 gitarre, 1 voice (group, effects), then the named children."""
    children = ["voice LIVE", "voice L1"] if children is None else children
    tracks = [
        {"name": "gitarre", "input_type": "Ext. In", "input_channel": "2"},
        {"name": "voice", "is_group": True, "num_devices": devices_on_group},
    ]
    tracks += [{"name": name, "group_index": 1, "input_type": "Ext. In", "input_channel": "1"}
               for name in children]
    return SimEngine(tracks=tracks)


def names(engine: SimEngine) -> list[str]:
    return [t.name for t in engine.get_session_structure()]


def test_missing_group_names_the_manual_step():
    engine = SimEngine(tracks=[{"name": "gitarre"}, {"name": "voice"}])   # 'voice' is no group
    with pytest.raises(EngineError) as info:
        ensure_pool(engine, score_for())
    msg = str(info.value)
    assert "Gruppe 'voice' existiert nicht im Ableton-Set" in msg
    assert "Strg+G" in msg and "benenne sie 'voice'" in msg


def test_group_without_any_child_is_refused():
    engine = SimEngine(tracks=[{"name": "gitarre"}, {"name": "voice", "is_group": True}])
    with pytest.raises(EngineError) as info:
        ensure_pool(engine, score_for())
    msg = str(info.value)
    assert "hat keine Kindspur" in msg and "voice L1" in msg


def test_too_few_layers_are_created_named_and_routed():
    engine = henry_set()
    report = ensure_pool(engine, score_for(layers=2, reserve=1))     # target 3 layer tracks
    assert report.changed
    result = report.groups[0]
    assert result.existing_layers == ["voice L1"]
    assert result.created_layers == ["voice L2", "voice L3"]
    assert result.monitor == "voice LIVE" and result.monitor_created is False
    assert result.warnings == []
    assert names(engine) == ["gitarre", "voice", "voice LIVE", "voice L1", "voice L2", "voice L3"]

    structure = engine.get_session_structure()
    for name in ("voice L2", "voice L3"):
        info = structure.by_name(name)
        assert info.group_index == 1 and info.num_devices == 0
        assert (info.input_type, info.input_channel) == ("Ext. In", "1")   # copied from L1
        assert engine.get_monitoring(info.index) == "auto"
        assert engine.is_armed(info.index) is False
    assert engine.get_monitoring(structure.by_name("voice LIVE").index) == "in"
    assert "Gruppe voice" in result.message and "2 angelegt" in result.message


def test_enough_layers_change_nothing():
    engine = henry_set(children=["voice LIVE", "voice L1", "voice L2", "voice L3"])
    before = names(engine)
    report = ensure_pool(engine, score_for(layers=2, reserve=1))
    assert report.changed is False
    assert report.groups[0].created_layers == []
    assert report.groups[0].existing_layers == ["voice L1", "voice L2", "voice L3"]
    assert names(engine) == before          # never shrinks, never renames


def test_pool_is_idempotent():
    engine = henry_set()
    ensure_pool(engine, score_for(layers=2, reserve=1))
    after_first = names(engine)
    second = ensure_pool(engine, score_for(layers=2, reserve=1))
    assert second.changed is False and names(engine) == after_first


def test_missing_monitor_track_is_created():
    engine = henry_set(children=["voice L1"])
    report = ensure_pool(engine, score_for(layers=1, reserve=0))
    result = report.groups[0]
    assert result.monitor_created is True and result.monitor == "voice LIVE"
    structure = engine.get_session_structure()
    monitor = structure.by_name(monitor_name("voice"))
    assert monitor.group_index == 1 and engine.get_monitoring(monitor.index) == "in"
    assert engine.is_armed(monitor.index) is False


def test_monitor_false_does_not_create_one():
    engine = henry_set(children=["voice L1"])
    report = ensure_pool(engine, score_for(layers=1, reserve=0, monitor=False))
    assert report.groups[0].monitor_created is False
    assert monitor_name("voice") not in names(engine)


def test_index_shift_is_tracked_across_many_inserts():
    """Every new track shifts the ones behind it; the pool must re-read, not remember."""
    engine = SimEngine(tracks=[
        {"name": "voice", "is_group": True},
        {"name": "voice LIVE", "group_index": 0, "input_type": "Ext. In", "input_channel": "1"},
        {"name": "voice L1", "group_index": 0, "input_type": "Ext. In", "input_channel": "1"},
        {"name": "gitarre", "input_type": "Ext. In", "input_channel": "2"},   # BEHIND the group
    ])
    ensure_pool(engine, score_for(layers=4, reserve=2))                # target 6 layer tracks
    structure = engine.get_session_structure()
    layers = [structure.by_name(layer_name("voice", n)) for n in range(1, 7)]
    assert all(info is not None and info.group_index == 0 for info in layers)
    # the group stayed contiguous and 'gitarre' was pushed to the very end
    child_indices = sorted(i.index for i in structure.children(0))
    assert child_indices == list(range(1, 8))
    assert structure.tracks[-1].name == "gitarre"
    assert len(structure) == 9


def test_a_layer_with_a_clip_is_never_duplicated():
    """A duplicate inherits clips - a new, empty layer track must come from create_audio_track."""
    engine = henry_set(children=["voice LIVE", "voice L1"])
    live = engine.get_session_structure().by_name("voice LIVE")
    engine.set_clip(live.index, 0, True)                      # monitor busy too
    engine.set_clip(live.index + 1, 0, True)                  # voice L1 already recorded
    ensure_pool(engine, score_for(layers=2, reserve=0))
    structure = engine.get_session_structure()
    new = structure.by_name("voice L2")
    assert new is not None and new.group_index == 1
    assert engine.slot_has_clip(new.index, 0) is False        # fresh track, no inherited clip


def test_stray_children_are_reported_but_left_alone():
    engine = henry_set(children=["voice LIVE", "voice L1", "3-Audio"])
    report = ensure_pool(engine, score_for(layers=1, reserve=0))
    assert report.groups[0].strays and any("Audio" in s for s in report.groups[0].strays)
    assert report.groups[0].created_layers == []
    assert "voice L1" in names(engine)


def test_score_without_group_tracks_does_nothing():
    engine = SimEngine(tracks=[{"name": "gitarre"}])
    score = compile_score("bpm: 100\ntracks: {g: {ableton_track: 0}}\nsections: [{id: a, bars: 1}]\n")
    report = ensure_pool(engine, score)
    assert report.groups == [] and report.changed is False and report.to_dict()["messages"] == []


def test_resolve_pools_binds_names_to_current_indices():
    engine = henry_set()
    score = score_for(layers=2, reserve=1)
    ensure_pool(engine, score)
    pools = resolve_pools(engine, score)
    pool = pools["voice"]
    assert pool.group == "voice" and pool.group_index == 1
    assert pool.layer_names == ["voice L1", "voice L2", "voice L3"]
    assert pool.layer_indices == [3, 4, 5] and pool.monitor_index == 2
    assert resolve_group(engine.get_session_structure(), score.track("voice")).layers == pool.layers
