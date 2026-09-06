"""Compiler support for group tracks (M4): `tracks: {voice: {group: voice, layers: 5, ...}}`."""

import json

import pytest

from conftest import ROOT
from looper.score.compile import ScoreError, compile_score

HEAD = """
bpm: 120
tracks:
  gitarre: {ableton_track: 0}
  voice:   {group: voice, layers: 5, reserve: 3, monitor: true}
"""


def compile_err(text: str) -> ScoreError:
    with pytest.raises(ScoreError) as info:
        compile_score(text)
    return info.value


def test_group_track_compiles_with_all_fields():
    score = compile_score(HEAD + "sections: [{id: a, bars: 2, tracks: {voice: record}}]\n")
    d = json.loads(score.to_json())
    assert d["tracks"] == [
        {"name": "gitarre", "ableton_track": 0, "type": "single"},
        {"name": "voice", "type": "group", "group": "voice", "layers": 5, "reserve": 3,
         "monitor": True},
    ]
    voice = score.track("voice")
    assert voice.is_group and voice.ableton_track is None and voice.pool_size == 8
    assert not score.track("gitarre").is_group
    # a group track takes part in the section resolution like any other track
    assert score.sections[0].tracks == {"gitarre": "stop", "voice": "record"}


def test_group_defaults_are_four_layers_two_reserve_and_monitor():
    score = compile_score("""
bpm: 100
tracks:
  voice: {group: voice}
sections: [{id: a, bars: 1}]
""")
    voice = score.track("voice")
    assert (voice.layers, voice.reserve, voice.monitor) == (4, 2, True)
    assert voice.pool_size == 6
    assert score.to_dict()["tracks"][0] == {"name": "voice", "type": "group", "group": "voice",
                                            "layers": 4, "reserve": 2, "monitor": True}


def test_monitor_false_and_reserve_zero_are_kept():
    score = compile_score("""
bpm: 100
tracks:
  voice: {group: Voice Bus, layers: 1, reserve: 0, monitor: false}
sections: [{id: a, bars: 1}]
""")
    voice = score.track("voice")
    assert (voice.group, voice.layers, voice.reserve, voice.monitor) == ("Voice Bus", 1, 0, False)
    assert voice.pool_size == 1


def test_group_and_ableton_track_together_is_an_error():
    err = compile_err("""
bpm: 100
tracks:
  voice: {group: voice, ableton_track: 1}
sections: [{id: a, bars: 1}]
""")
    assert len(err.errors) == 1
    e = err.errors[0]
    assert e["line"] == 4
    assert "'group' und 'ableton_track' schließen sich aus" in e["message"]
    assert "ableton_track" in e["suggestion"]


def test_track_without_group_and_without_index_is_an_error():
    err = compile_err("""
bpm: 100
tracks:
  voice: {}
sections: [{id: a, bars: 1}]
""")
    assert "hat weder 'ableton_track' noch 'group'" in err.errors[0]["message"]
    assert err.errors[0]["line"] == 4


def test_group_only_fields_on_a_single_track_are_rejected():
    err = compile_err("""
bpm: 100
tracks:
  voice: {ableton_track: 1, layers: 3}
sections: [{id: a, bars: 1}]
""")
    assert "'layers' gilt nur für Gruppen-Tracks" in err.errors[0]["message"]
    assert "group:" in err.errors[0]["suggestion"]


def test_two_tracks_may_not_share_one_group():
    err = compile_err("""
bpm: 100
tracks:
  voice:  {group: voice}
  voice2: {group: VOICE}
sections: [{id: a, bars: 1}]
""")
    assert "die bereits 'voice' belegt" in err.errors[0]["message"]


def test_group_layer_bounds_come_from_the_schema():
    err = compile_err("""
bpm: 100
tracks:
  voice: {group: voice, layers: 0}
sections: [{id: a, bars: 1}]
""")
    assert ">= 1" in err.errors[0]["message"]


def test_legacy_type_group_with_index_still_compiles_as_single():
    """`{ableton_track: 1, type: group}` predates M4 and had no semantics - it must not break."""
    score = compile_score("""
bpm: 100
tracks:
  bass: {ableton_track: 1, type: group}
sections: [{id: a, bars: 1}]
""")
    bass = score.track("bass")
    assert bass.type == "group" and bass.is_group is False and bass.ableton_track == 1


def test_example_group_score_compiles():
    text = (ROOT / "examples" / "henry-3-4-group.yaml").read_text(encoding="utf-8")
    score = compile_score(text)
    voice = score.track("voice")
    assert voice.is_group and voice.group == "voice" and voice.pool_size == 6
    assert [s.id for s in score.sections] == ["gitarre_rec", "voice_1", "voice_2", "voice_3",
                                              "alles", "ende"]
    assert score.beats_per_bar == 3
    assert score.sections[1].tracks == {"gitarre": "play", "voice": "record"}
    json.dumps(score.to_dict())
