"""Runner with a group track (M4): layers stack on child tracks and all of them sound.

Everything runs against SimEngine's simulated group structure - Ableton is never contacted.
"""

import threading

import pytest

from conftest import wait_for
from looper.engine.base import EngineError
from looper.engine.sim import SimEngine
from looper.runner import Runner
from looper.score.compile import compile_score
from looper.session import ensure_pool
from test_runner import Recorder

GROUP_YAML = """
bpm: 120
tracks:
  voice: {group: voice, layers: %d, reserve: %d}
sections:
  - {id: s1, bars: 2, autorelease: true,  tracks: {voice: record}}
  - {id: s2, bars: 2, autorelease: true,  tracks: {voice: overdub}}
  - {id: s3, bars: 2, autorelease: true,  tracks: {voice: overdub}}
  - {id: s4, bars: 2, autorelease: false, tracks: {voice: play}}
"""


def group_engine(children: list[str] | None = None, speed: float = 30.0) -> SimEngine:
    children = ["voice LIVE", "voice L1"] if children is None else children
    tracks = [{"name": "voice", "is_group": True, "num_devices": 3}]
    tracks += [{"name": name, "group_index": 0, "input_type": "Ext. In", "input_channel": "1"}
               for name in children]
    return SimEngine(bpm=120, beats_per_bar=4, speed=speed, tracks=tracks)


def index_of(engine: SimEngine, name: str) -> int:
    info = engine.get_session_structure().by_name(name)
    assert info is not None, f"Spur '{name}' fehlt: {[t.name for t in engine.get_session_structure()]}"
    return info.index


def test_three_overdubs_fill_three_layer_tracks_that_all_play():
    score = compile_score(GROUP_YAML % (4, 0))
    engine = group_engine()
    ensure_pool(engine, score)                    # 4 layer tracks, monitor already there
    rec = Recorder()
    runner = Runner(engine, score, rec)
    runner.start()
    try:
        assert wait_for(lambda: runner.state()["section_id"] == "s4", timeout=15)
        assert wait_for(lambda: all(engine.slot_state(index_of(engine, n), 0) == "playing"
                                    for n in ("voice L1", "voice L2", "voice L3")), timeout=10)
        # three layers occupied, each with exactly one clip in slot 0, all sounding together
        for name in ("voice L1", "voice L2", "voice L3"):
            idx = index_of(engine, name)
            assert engine.slot_has_clip(idx, 0) and not engine.slot_has_clip(idx, 1)
            assert engine.is_armed(idx) is False
        assert engine.slot_has_clip(index_of(engine, "voice L4"), 0) is False
        st = runner.state()
        assert st["groups"] == {"voice": {"layers_used": 3, "layers_free": 1}}
        assert st["tracks"] == {"voice": "play"}
        # the monitor child is on Monitoring In and never records
        monitor = index_of(engine, "voice LIVE")
        assert engine.get_monitoring(monitor) == "in" and not engine.slot_has_clip(monitor, 0)
    finally:
        runner.stop_all()
        engine.close()


def test_stop_stops_every_layer_of_the_group():
    score = compile_score(GROUP_YAML % (4, 0) + "  - {id: s5, bars: 1, tracks: {voice: stop}}\n")
    engine = group_engine()
    ensure_pool(engine, score)
    runner = Runner(engine, score, Recorder())
    runner.start()
    try:
        assert wait_for(lambda: runner.state()["section_id"] == "s4", timeout=20)
        runner.next_section()                       # s4 waits for the release button
        assert wait_for(lambda: runner.state()["section_id"] == "s5", timeout=20)
        assert wait_for(lambda: all(engine.slot_state(index_of(engine, n), 0) == "stopped"
                                    for n in ("voice L1", "voice L2", "voice L3")), timeout=10)
        assert runner.state()["groups"]["voice"]["layers_used"] == 3   # clips stay, just stopped
    finally:
        runner.stop_all()
        engine.close()


def test_hear_through_uses_the_monitor_track():
    score = compile_score("""
bpm: 120
tracks:
  voice: {group: voice, layers: 2, reserve: 0}
sections:
  - {id: s1, bars: 2, autorelease: true, tracks: {voice: record}}
  - {id: s2, bars: 2, autorelease: false, tracks: {voice: hear_through}}
""")
    engine = group_engine()
    ensure_pool(engine, score)
    runner = Runner(engine, score, Recorder())
    runner.start()
    try:
        assert wait_for(lambda: runner.state()["section_id"] == "s2", timeout=15)
        monitor = index_of(engine, "voice LIVE")
        assert wait_for(lambda: engine.get_monitoring(monitor) == "in")
        assert wait_for(lambda: engine.slot_state(index_of(engine, "voice L1"), 0) == "stopped")
        assert engine.is_armed(monitor) is False
    finally:
        runner.stop_all()
        engine.close()


def test_pool_runs_low_and_is_topped_up_between_sections():
    """reserve=2: after the first recording the pool is short and the runner adds tracks."""
    score = compile_score(GROUP_YAML % (1, 2))          # target 3 layer tracks at load time
    engine = group_engine(speed=12)
    ensure_pool(engine, score)
    assert [t.name for t in engine.get_session_structure()] == [
        "voice", "voice LIVE", "voice L1", "voice L2", "voice L3"]
    rec = Recorder()
    runner = Runner(engine, score, rec)
    runner.start()
    try:
        assert wait_for(lambda: runner.state()["section_id"] == "s4", timeout=40)
        # three layers used; the worker kept `reserve` free by adding L4 and L5
        assert wait_for(lambda: runner.state()["groups"]["voice"]["layers_free"] >= 2, timeout=20)
        st = runner.state()
        assert st["groups"]["voice"]["layers_used"] == 3
        names = [t.name for t in engine.get_session_structure()]
        assert "voice L4" in names and "voice L5" in names
        # every recorded layer is still playing - the top-up must not disturb the run
        assert all(engine.slot_state(index_of(engine, n), 0) == "playing"
                   for n in ("voice L1", "voice L2", "voice L3"))
        messages = [e["message"] for e in rec.of("log")]
        assert any("Layer-Pool wird nachgelegt" in m for m in messages)
        assert any("angelegt" in m and "voice L4" in m for m in messages)
        # no timing warning: the structure work never ran inside the beat callback
        assert [m for m in messages if "Beat-Callback dauerte" in m] == []
    finally:
        runner.stop_all()
        engine.close()


def test_topup_never_runs_inside_the_beat_callback():
    """The engine calls that create tracks must come from the worker, not the clock thread."""
    score = compile_score(GROUP_YAML % (1, 2))
    engine = group_engine(speed=12)
    ensure_pool(engine, score)
    clock_thread: dict[str, str | None] = {"name": None}
    offenders: list[str] = []
    original = engine.duplicate_track
    create_original = engine.create_track_in_group

    def note(fn):
        def wrapper(*args, **kwargs):
            current = threading.current_thread().name
            if clock_thread["name"] and current == clock_thread["name"]:
                offenders.append(f"{fn.__name__} auf {current}")
            return fn(*args, **kwargs)
        return wrapper

    engine.duplicate_track = note(original)
    engine.create_track_in_group = note(create_original)
    engine.on_beat(lambda _b: clock_thread.__setitem__("name", threading.current_thread().name))

    runner = Runner(engine, score, Recorder())
    runner.start()
    try:
        assert wait_for(lambda: runner.state()["section_id"] == "s3", timeout=40)
        assert wait_for(lambda: "voice L4" in [t.name for t in engine.get_session_structure()],
                        timeout=20)
        assert offenders == []
        assert clock_thread["name"] is not None
    finally:
        runner.stop_all()
        engine.close()


def test_start_refuses_a_missing_group_and_stays_idle():
    score = compile_score(GROUP_YAML % (2, 0))
    engine = SimEngine(bpm=120, speed=30, tracks=[{"name": "voice"}])   # no group track
    rec = Recorder()
    runner = Runner(engine, score, rec)
    with pytest.raises(EngineError) as info:
        runner.start()
    assert "Gruppe 'voice' existiert nicht" in str(info.value)
    assert runner.state()["running"] is False
    assert not engine.transport_running
    assert rec.of("beat") == []


def test_existing_clips_on_layer_tracks_are_not_overwritten():
    """A layer track that still holds a clip from an earlier run stays occupied."""
    score = compile_score(GROUP_YAML % (4, 0))
    engine = group_engine()
    ensure_pool(engine, score)
    engine.set_clip(index_of(engine, "voice L1"), 0, True)
    runner = Runner(engine, score, Recorder())
    runner.start()
    try:
        assert runner.state()["groups"]["voice"] == {"layers_used": 1, "layers_free": 3}
        assert wait_for(lambda: engine.slot_state(index_of(engine, "voice L2"), 0) == "recording",
                        timeout=15)
        assert engine.slot_state(index_of(engine, "voice L1"), 0) != "recording"
    finally:
        runner.stop_all()
        engine.close()


def test_mixed_score_single_and_group_tracks():
    score = compile_score("""
bpm: 120
tracks:
  gitarre: {ableton_track: 0}
  voice:   {group: voice, layers: 2, reserve: 0}
sections:
  - {id: s1, bars: 2, autorelease: true,  tracks: {gitarre: record, voice: hear_through}}
  - {id: s2, bars: 2, autorelease: true,  tracks: {gitarre: play, voice: record}}
  - {id: s3, bars: 2, autorelease: false, tracks: {gitarre: play, voice: play}}
""")
    engine = SimEngine(bpm=120, beats_per_bar=4, speed=30, tracks=[
        {"name": "gitarre", "input_type": "Ext. In", "input_channel": "2"},
        {"name": "voice", "is_group": True},
        {"name": "voice LIVE", "group_index": 1, "input_type": "Ext. In", "input_channel": "1"},
        {"name": "voice L1", "group_index": 1, "input_type": "Ext. In", "input_channel": "1"},
    ])
    ensure_pool(engine, score)
    runner = Runner(engine, score, Recorder())
    runner.start()
    try:
        assert wait_for(lambda: runner.state()["section_id"] == "s3", timeout=20)
        assert wait_for(lambda: engine.slot_state(0, 0) == "playing")          # gitarre single
        assert wait_for(lambda: engine.slot_state(index_of(engine, "voice L1"), 0) == "playing")
        st = runner.state()
        assert st["tracks"] == {"gitarre": "play", "voice": "play"}
        assert st["groups"]["voice"]["layers_used"] == 1
    finally:
        runner.stop_all()
        engine.close()
