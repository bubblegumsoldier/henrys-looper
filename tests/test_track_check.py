"""Preflight track check in Runner.start() (Ableton test 2026-09-06).

Henry's score referenced ableton_track 2 and 3 while the set had 2 tracks; AbletonOSC never
answers for a track that does not exist, so the runner ran into timeouts and missed a bar line.
start() must therefore refuse such a score *before* it touches tempo, quantization or transport.
"""

from unittest import mock

import pytest

from conftest import wait_for
from looper.engine.base import EngineError
from looper.engine.sim import SimEngine
from looper.runner import Runner
from looper.score.compile import compile_score
from test_runner import Recorder

TWO_TRACK_YAML = """
bpm: 100
tracks:
  gitarre: {ableton_track: 0}
  voice:   {ableton_track: 1}
sections:
  - {id: intro, bars: 2, autorelease: true, tracks: {gitarre: record}}
  - {id: verse, bars: 2, tracks: {gitarre: play, voice: record}}
"""

MISSING_TRACK_YAML = """
bpm: 100
tracks:
  gitarre:    {ableton_track: 0}
  voice:      {ableton_track: 1}
  voice2:     {ableton_track: 2}
  voice_live: {ableton_track: 3}
sections:
  - {id: intro, bars: 2, autorelease: true, tracks: {gitarre: record}}
  - {id: verse, bars: 2, tracks: {voice2: record, voice_live: play}}
"""

# every engine call start() would make after the check - none of them may happen
WATCHED = ("start_transport", "stop_transport", "set_tempo", "set_time_signature",
           "set_quantization_bar", "stop_all", "arm", "fire_slot")


def spy(engine: SimEngine) -> dict[str, mock.MagicMock]:
    """Wrap the engine's commands in mocks (instance attributes shadow the methods)."""
    spies = {}
    for name in WATCHED:
        spies[name] = mock.MagicMock(wraps=getattr(engine, name))
        setattr(engine, name, spies[name])
    return spies


def test_start_refuses_missing_tracks_and_never_touches_transport():
    score = compile_score(MISSING_TRACK_YAML)
    eng = SimEngine(bpm=score.bpm, speed=40, track_count=2)
    rec = Recorder()
    runner = Runner(eng, score, rec)
    spies = spy(eng)

    with pytest.raises(EngineError) as info:
        runner.start()

    msg = str(info.value)
    # both problems are named at once, with the score's own track names and the real set size
    assert "Spur 2 ('voice2')" in msg and "Spur 3 ('voice_live')" in msg
    assert "nur 2 Spuren (0–1)" in msg
    assert "'ableton_track'" in msg
    # nothing was started: no transport, no tempo, no quantization, no clips
    for name in WATCHED:
        assert spies[name].call_count == 0, f"{name} wurde trotz fehlender Spur aufgerufen"
    assert not eng.transport_running
    st = runner.state()
    assert st["running"] is False and st["section_id"] == "intro"
    assert rec.of("beat") == [] and rec.of("state") == []


def test_start_refuses_single_track_set_with_singular_wording():
    score = compile_score(TWO_TRACK_YAML)
    eng = SimEngine(bpm=score.bpm, speed=40, track_count=1)
    runner = Runner(eng, score, Recorder())
    with pytest.raises(EngineError) as info:
        runner.start()
    msg = str(info.value)
    assert "Spur 1 ('voice')" in msg and "nur 1 Spur (Index 0)" in msg
    assert "Spur 0" not in msg               # track 0 exists and must not be blamed
    assert not eng.transport_running


def test_name_mismatch_warns_once_and_starts():
    """Ableton track 1 is called 'gitarre', the score calls it 'voice' -> exactly one warning."""
    score = compile_score(TWO_TRACK_YAML)
    eng = SimEngine(bpm=score.bpm, speed=40, track_names=["gitarre", "gitarre"])
    rec = Recorder()
    runner = Runner(eng, score, rec)
    try:
        runner.start()
        assert runner.state()["running"] is True
        warns = [e for e in rec.of("log") if e["level"] == "warn" and "Nummerierung" in e["message"]]
        assert len(warns) == 1
        assert warns[0]["message"] == ("Spur 1 heißt in Ableton 'gitarre', in der Partitur 'voice' "
                                       "— Nummerierung prüfen.")
        assert wait_for(lambda: runner.state()["bar"] >= 1)      # the run itself is unaffected
    finally:
        runner.stop_all()
        eng.close()


def test_matching_names_start_without_warning():
    score = compile_score(TWO_TRACK_YAML)
    eng = SimEngine(bpm=score.bpm, speed=40, track_names=["Gitarre", " voice "])  # case/space tolerant
    rec = Recorder()
    runner = Runner(eng, score, rec)
    try:
        runner.start()
        assert runner.state()["running"] is True
        assert [e for e in rec.of("log") if "Nummerierung" in e["message"]] == []
        assert wait_for(lambda: runner.state()["section_id"] == "verse")
    finally:
        runner.stop_all()
        eng.close()


def test_nameless_engine_starts_without_warning():
    """The default SimEngine knows no names (and plenty of tracks): no check noise at all."""
    score = compile_score(TWO_TRACK_YAML)
    eng = SimEngine(bpm=score.bpm, speed=40)
    rec = Recorder()
    runner = Runner(eng, score, rec)
    try:
        runner.start()
        assert runner.state()["running"] is True
        assert [e for e in rec.of("log") if e["level"] == "warn"] == []
    finally:
        runner.stop_all()
        eng.close()


def test_unavailable_track_count_only_warns():
    """A failing count query must not block the start - it degrades to a warning."""
    score = compile_score(TWO_TRACK_YAML)
    eng = SimEngine(bpm=score.bpm, speed=40)
    eng.get_track_count = mock.MagicMock(side_effect=EngineError("Zeitüberschreitung"))
    rec = Recorder()
    runner = Runner(eng, score, rec)
    try:
        runner.start()
        assert runner.state()["running"] is True
        assert any(e["level"] == "warn" and "ungeprüft" in e["message"] for e in rec.of("log"))
    finally:
        runner.stop_all()
        eng.close()
