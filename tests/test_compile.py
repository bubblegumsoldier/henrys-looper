import json

import pytest

from looper.score.compile import ScoreError, compile_score

MINIMAL_HEAD = """
bpm: 120
tracks:
  gitarre: {ableton_track: 0}
  bass:    {ableton_track: 1, type: group}
"""


def compile_err(text: str) -> ScoreError:
    with pytest.raises(ScoreError) as info:
        compile_score(text)
    return info.value


# ------------------------------------------------------------------ good scores
def test_example_song_matches_contract_shape(example_yaml):
    score = compile_score(example_yaml)
    d = score.to_dict()
    json.dumps(d)  # JSON-serializable
    assert set(d) == {"title", "bpm", "beats_per_bar", "time_signature", "tracks", "midi", "sections"}
    assert d["bpm"] == 100 and d["beats_per_bar"] == 4 and d["time_signature"] == "4/4"
    assert d["tracks"] == [{"name": "gitarre", "ableton_track": 0, "type": "single"},
                           {"name": "voice", "ableton_track": 1, "type": "single"}]
    assert d["midi"] == {"next_section": {"id": "ch1.cc64"}, "stop_all": {"id": "ch1.note37"}}
    assert [s["id"] for s in d["sections"]] == ["intro", "verse", "outro"]
    assert [s["index"] for s in d["sections"]] == [0, 1, 2]
    for s in d["sections"]:
        assert set(s) == {"id", "index", "bars", "autorelease", "quantize", "tracks", "source_line"}
        assert set(s["tracks"]) == {"gitarre", "voice"}   # every section lists all tracks
        assert isinstance(s["source_line"], int)
    intro, verse, outro = d["sections"]
    assert intro["tracks"] == {"gitarre": "record", "voice": "hear_through"} and intro["autorelease"] is False
    assert verse["tracks"] == {"gitarre": "play", "voice": "record"} and verse["autorelease"] is True
    # repeat: verse with overrides -> bars copied, autorelease + tracks overridden
    assert outro["bars"] == 4 and outro["autorelease"] is False
    assert outro["tracks"] == {"gitarre": "overdub", "voice": "play"}


def test_source_lines_point_to_section_items(example_yaml):
    score = compile_score(example_yaml)
    lines = example_yaml.splitlines()
    for sec in score.sections:
        assert lines[sec.source_line - 1].lstrip().startswith("- id: " + sec.id)


def test_missing_tracks_become_stop_and_defaults_apply():
    score = compile_score(MINIMAL_HEAD + """
sections:
  - id: a
    bars: 3
    tracks: {gitarre: record}
""")
    sec = score.sections[0]
    assert sec.tracks == {"gitarre": "record", "bass": "stop"}
    assert sec.bars == 3                      # odd bar counts are allowed
    assert sec.autorelease is False and sec.quantize == "loop"
    assert score.title == "" and score.tracks[1].type == "group"


def test_repeat_chain_and_yaml_anchors():
    score = compile_score(MINIMAL_HEAD + """
sections:
  - &base
    id: a
    bars: 2
    quantize: bar
    tracks: {gitarre: record}
  - id: b
    repeat: a
    tracks: {bass: play}
  - id: c
    repeat: b
    bars: 7
  - <<: *base
    id: d
""")
    a, b, c, d = score.sections
    assert (b.bars, b.quantize, b.tracks) == (2, "bar", {"gitarre": "record", "bass": "play"})
    assert (c.bars, c.tracks) == (7, {"gitarre": "record", "bass": "play"})
    assert (d.bars, d.quantize, d.tracks) == (2, "bar", {"gitarre": "record", "bass": "stop"})
    assert [s.index for s in score.sections] == [0, 1, 2, 3]


def test_midi_ids_with_channel_and_note():
    score = compile_score(MINIMAL_HEAD + """
midi:
  next_section: {note: 36, channel: 10}
  stop_all: {cc: 7}
sections: [{id: a, bars: 1}]
""")
    assert score.midi == {"next_section": {"id": "ch10.note36"}, "stop_all": {"id": "ch1.cc7"}}


def test_time_signature_unquoted_and_quoted():
    score = compile_score(MINIMAL_HEAD + "time_signature: 3/4\nsections: [{id: a, bars: 1}]\n")
    assert (score.beats_per_bar, score.time_signature, score.time_signature_parts) == (3, "3/4", (3, 4))
    score = compile_score(MINIMAL_HEAD + 'time_signature: "6/8"\nsections: [{id: a, bars: 1}]\n')
    assert (score.beats_per_bar, score.time_signature) == (6, "6/8")
    assert score.to_dict()["time_signature"] == "6/8" and score.to_dict()["beats_per_bar"] == 6


def test_beats_per_bar_alias_and_default():
    score = compile_score(MINIMAL_HEAD + "beats_per_bar: 7\nsections: [{id: a, bars: 1}]\n")
    assert (score.beats_per_bar, score.time_signature) == (7, "7/4")
    score = compile_score(MINIMAL_HEAD + "sections: [{id: a, bars: 1}]\n")
    assert (score.beats_per_bar, score.time_signature) == (4, "4/4")
    # both given and consistent is fine
    score = compile_score(MINIMAL_HEAD + "time_signature: 3/4\nbeats_per_bar: 3\nsections: [{id: a, bars: 1}]\n")
    assert (score.beats_per_bar, score.time_signature) == (3, "3/4")


# ------------------------------------------------------------------- bad scores
def test_time_signature_contradiction_and_invalid_values():
    err = compile_err(MINIMAL_HEAD + "time_signature: 3/4\nbeats_per_bar: 4\nsections: [{id: a, bars: 1}]\n")
    assert len(err.errors) == 1
    e = err.errors[0]
    assert e["line"] == 7 and "widerspricht" in e["message"] and "time_signature" in e["suggestion"]
    err = compile_err(MINIMAL_HEAD + "time_signature: dreiviertel\nsections: [{id: a, bars: 1}]\n")
    assert err.errors[0]["line"] == 6 and "Ungültige Taktart" in err.errors[0]["message"]
    err = compile_err(MINIMAL_HEAD + "time_signature: 3/5\nsections: [{id: a, bars: 1}]\n")
    assert "Zweierpotenz" in err.errors[0]["message"]
    err = compile_err(MINIMAL_HEAD + "time_signature: 0/4\nsections: [{id: a, bars: 1}]\n")
    assert "zwischen 1 und 16" in err.errors[0]["message"]
    err = compile_err(MINIMAL_HEAD + "time_signature: 4\nsections: [{id: a, bars: 1}]\n")
    assert "muss Text sein" in err.errors[0]["message"]


def test_yaml_syntax_error_has_line_and_column():
    err = compile_err("bpm: 100\ntracks:\n  gitarre: {ableton_track: 0\nsections: []\n")
    assert len(err.errors) == 1
    e = err.errors[0]
    assert e["line"] == 4 and e["column"] is not None
    assert "YAML-Syntaxfehler" in e["message"]
    assert set(e) == {"line", "column", "message", "suggestion"}


def test_duplicate_key_reported():
    err = compile_err("bpm: 100\nbpm: 120\ntracks: {g: {ableton_track: 0}}\nsections: [{id: a, bars: 1}]\n")
    assert err.errors[0]["line"] == 2
    assert "Doppelter Schlüssel 'bpm'" in err.errors[0]["message"]


def test_unknown_track_reference_suggests_correction():
    err = compile_err(MINIMAL_HEAD + """
sections:
  - id: bridge
    bars: 4
    tracks:
      bss: play
""")
    assert len(err.errors) == 1
    e = err.errors[0]
    assert e["message"] == "Sektion 'bridge' referenziert Track 'bss'."
    assert e["suggestion"] == "meintest du 'bass'?"
    assert e["line"] == 11 and e["column"] == 7


def test_unknown_repeat_target_suggests_correction():
    err = compile_err(MINIMAL_HEAD + """
sections:
  - id: verse
    bars: 4
  - id: outro
    repeat: vers
""")
    assert err.errors[0]["suggestion"] == "meintest du 'verse'?"
    assert err.errors[0]["line"] == 11
    assert "outro" in err.errors[0]["message"] and "vers" in err.errors[0]["message"]


def test_typo_in_field_name_and_state_value():
    err = compile_err(MINIMAL_HEAD + """
sections:
  - id: a
    bars: 4
    autorelase: true
    tracks:
      gitarre: recrod
""")
    by_line = {e["line"]: e for e in err.errors}
    assert by_line[10]["suggestion"] == "meintest du 'autorelease'?"
    assert by_line[12]["suggestion"] == "meintest du 'record'?"
    assert "Erlaubt: record, overdub, play, stop, hear_through" in by_line[12]["message"]


def test_multiple_errors_collected_in_broken_fixture(broken_yaml):
    err = compile_err(broken_yaml)
    lines = [e["line"] for e in err.errors]
    assert lines == [9, 11, 15, 17, 18]              # sorted, all collected at once
    assert all(e["suggestion"] for e in err.errors)   # every error carries a suggestion
    text = str(err)
    assert "Zeile 15, Spalte 7: Sektion 'bridge' referenziert Track 'bss'. — meintest du 'bass'?" in text


def test_missing_required_fields_and_ranges():
    err = compile_err("title: x\nsections: []\n")
    msgs = " | ".join(e["message"] for e in err.errors)
    assert "Pflichtfeld 'bpm' fehlt" in msgs and "Pflichtfeld 'tracks' fehlt" in msgs
    assert "mindestens 1" in msgs
    err = compile_err(MINIMAL_HEAD + "sections: [{id: a, bars: 0}]\n")
    assert ">= 1" in err.errors[0]["message"]
    # a wrong type does not additionally produce an enum error for the same field
    err = compile_err(MINIMAL_HEAD + "sections: [{id: a, bars: 1, tracks: {gitarre: 1}}]\n")
    assert len(err.errors) == 1 and "muss Text sein" in err.errors[0]["message"]


def test_duplicate_section_id_and_ableton_track():
    err = compile_err("""
bpm: 100
tracks:
  a: {ableton_track: 0}
  b: {ableton_track: 0}
sections:
  - {id: x, bars: 1}
  - {id: x, bars: 2}
""")
    msgs = " | ".join(e["message"] for e in err.errors)
    assert "ableton_track 0" in msgs and "Sektions-ID 'x' ist doppelt" in msgs


def test_cyclic_repeat_and_missing_bars():
    err = compile_err(MINIMAL_HEAD + """
sections:
  - id: a
    repeat: b
  - id: b
    repeat: a
  - id: c
""")
    msgs = " | ".join(e["message"] for e in err.errors)
    assert "zirkulär" in msgs
    assert "Sektion 'c' hat weder 'bars' noch 'repeat'" in msgs


def test_midi_binding_needs_exactly_one_of_cc_or_note():
    err = compile_err(MINIMAL_HEAD + "midi: {next_section: {cc: 1, note: 2}, foo: {cc: 3}}\nsections: [{id: a, bars: 1}]\n")
    msgs = " | ".join(e["message"] for e in err.errors)
    assert "genau eines von 'cc' oder 'note'" in msgs
    assert "Unbekanntes Feld 'foo'" in msgs


def test_non_mapping_root_is_rejected():
    err = compile_err("- 1\n- 2\n")
    assert "muss ein Mapping" in err.errors[0]["message"]
