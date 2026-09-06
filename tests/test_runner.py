import threading

import pytest

from conftest import wait_for
from looper.engine.sim import SimEngine
from looper.runner import Runner
from looper.score.compile import compile_score


class Recorder:
    def __init__(self):
        self.events: list[dict] = []
        self.lock = threading.Lock()

    def __call__(self, ev: dict) -> None:
        with self.lock:
            self.events.append(dict(ev))

    def of(self, kind: str) -> list[dict]:
        with self.lock:
            return [e for e in self.events if e["type"] == kind]


def make(example_yaml, speed=40):
    score = compile_score(example_yaml)
    eng = SimEngine(bpm=score.bpm, beats_per_bar=score.beats_per_bar, speed=speed)
    rec = Recorder()
    return score, eng, rec, Runner(eng, score, rec)


def test_runner_steps_through_example_song(example_yaml):
    score, eng, rec, runner = make(example_yaml)
    runner.start()
    st = runner.state()
    assert st["running"] and st["connected"] and st["engine"] == "sim"
    assert st["section_id"] == "intro" and st["pending"] is None
    assert eng.get_tempo() == 100 and eng.quantization_bar

    # count-in bar, then section 1: gitarre records into slot 0, voice hears through
    assert wait_for(lambda: runner.state()["bar"] >= 2)
    assert eng.is_armed(0) and not eng.is_armed(1)
    assert eng.slot_state(0, 0) == "recording"
    assert eng.get_monitoring(1) == "in"
    assert runner.state()["tracks"] == {"gitarre": "record", "voice": "hear_through"}

    # autorelease=false: the section loops until release
    assert wait_for(lambda: runner.state()["bar"] == 1 and runner.state()["section_id"] == "intro"
                    and any(e["bar"] == 4 for e in rec.of("beat")))
    assert eng.slot_state(0, 0) == "recording"
    runner.next_section()
    assert runner.state()["pending"] == "next"
    runner.next_section()                              # idempotent while pending
    assert runner.state()["pending"] == "next"

    # section 2 (quantize=loop -> at the end of the 4-bar cycle): gitarre plays, voice records
    assert wait_for(lambda: runner.state()["section_id"] == "verse")
    assert runner.state()["pending"] is None
    assert wait_for(lambda: eng.slot_state(0, 0) == "playing" and eng.slot_state(1, 0) == "recording")
    assert not eng.is_armed(0) and eng.is_armed(1)
    assert eng.get_monitoring(1) == "auto"

    # section 3 after 4 bars (autorelease): overdub = new layer in gitarre slot 1, voice loops
    assert wait_for(lambda: runner.state()["section_id"] == "outro")
    assert wait_for(lambda: eng.slot_state(0, 1) == "recording" and eng.slot_state(1, 0) == "playing")
    assert eng.slot_has_clip(0, 0) and eng.slot_has_clip(0, 1) and eng.slot_has_clip(1, 0)

    # last section loops until next_section arms "stop": clips stop at the end of the cycle, runner idle
    assert wait_for(lambda: any("Letzte Sektion" in e["message"] for e in rec.of("log")))
    assert runner.state()["section_id"] == "outro"
    runner.next_section()
    assert runner.state()["pending"] == "stop"
    assert wait_for(lambda: runner.state()["running"] is False)
    st = runner.state()
    assert st["tracks"] == {"gitarre": "stop", "voice": "stop"} and st["pending"] is None
    assert wait_for(lambda: not eng.transport_running)
    assert not eng.is_armed(0) and not eng.is_armed(1)
    assert eng.slot_state(0, 1) == "stopped" and eng.slot_state(1, 0) == "stopped"
    runner.stop_all()                                    # harmless after the quantized stop
    eng.close()

    # event sequence: state snapshots walk intro -> verse -> outro, beats stay within the section
    section_walk = []
    for e in rec.of("state"):
        if not section_walk or section_walk[-1] != e["section_id"]:
            section_walk.append(e["section_id"])
    assert section_walk == ["intro", "verse", "outro"]
    beats = rec.of("beat")
    assert beats and all(0 <= b["bar"] <= 4 and 1 <= b["beat"] <= 4 for b in beats)
    assert all(set(e) >= {"running", "section_index", "section_id", "bars_total", "bar", "beat",
                          "pending", "tracks", "connected", "engine", "beats_per_bar",
                          "time_signature"} for e in rec.of("state"))
    assert all(e["beats_per_bar"] == 4 and e["time_signature"] == "4/4" for e in rec.of("state"))
    pending_states = [e for e in rec.of("state") if e["pending"] == "next"]
    assert pending_states and pending_states[0]["section_id"] == "intro"


def test_three_four_section_advances_after_twelve_beats():
    yaml_text = """
bpm: 100
time_signature: 3/4
tracks:
  g: {ableton_track: 0}
sections:
  - {id: a, bars: 4, autorelease: true, tracks: {g: record}}
  - {id: b, bars: 2, tracks: {g: play}}
"""
    score = compile_score(yaml_text)
    eng = SimEngine(bpm=score.bpm, beats_per_bar=4, speed=40)     # constructed with 4, runner must override
    rec = Recorder()
    runner = Runner(eng, score, rec)
    runner.start()
    assert eng.get_beats_per_bar() == 3 and eng.time_signature == (3, 4)
    st = runner.state()
    assert st["beats_per_bar"] == 3 and st["time_signature"] == "3/4"
    assert wait_for(lambda: runner.state()["section_id"] == "b")
    runner.stop_all()
    eng.close()
    # count-in = 1 bar of 3 beats; section a = 4 bars * 3 beats = 12 beats, then b
    a_beats = [e for e in rec.of("beat") if e["bar"] >= 1]
    first_a = a_beats[0]["song_beat"]
    assert first_a % 3 == 0
    first_b_state = next(e for e in rec.of("state") if e["section_id"] == "b")
    b_start = next(e["song_beat"] for e in rec.of("beat") if e["song_beat"] >= first_a + 12)
    assert b_start == first_a + 12
    assert all(1 <= e["beat"] <= 3 and e["bar"] <= 4 for e in a_beats)
    assert all(e["beats_per_bar"] == 3 and e["time_signature"] == "3/4" for e in rec.of("state"))
    assert first_b_state["bar"] == 1
    # the section switch happened exactly on the 12-beat boundary, not on a 4-beat one
    switch = [e for e in rec.of("beat") if e["bar"] == 1 and e["beat"] == 1]
    assert any(e["song_beat"] == first_a + 12 for e in switch)


def test_quantize_bar_switches_on_next_bar_line():
    yaml_text = """
bpm: 100
tracks:
  g: {ableton_track: 0}
sections:
  - {id: a, bars: 8, autorelease: false, quantize: bar, tracks: {g: record}}
  - {id: b, bars: 2, tracks: {g: play}}
"""
    score, eng, rec, runner = make(yaml_text)
    runner.start()
    assert wait_for(lambda: runner.state()["section_id"] == "a" and runner.state()["bar"] == 2)
    runner.next_section()
    assert wait_for(lambda: runner.state()["section_id"] == "b")
    # switched long before the 8-bar cycle would have ended
    switch_beat = next(e["song_beat"] for e in rec.of("beat") if e["bar"] == 1 and e["beat"] == 1
                       and rec.events.index(e) > 0 and e["song_beat"] >= 8 + 4)
    assert switch_beat % 4 == 0 and switch_beat <= 4 + 4 * 4
    assert wait_for(lambda: eng.slot_state(0, 0) == "playing")
    runner.stop_all()
    eng.close()


def test_midi_mapping_triggers_next_and_stop(example_yaml):
    score, eng, rec, runner = make(example_yaml)
    runner.start()
    assert wait_for(lambda: runner.state()["bar"] >= 1 and not runner.state()["countin"])
    runner.handle_midi({"type": "midi", "device": "x", "channel": 1, "kind": "cc", "number": 64,
                        "value": 127, "id": "ch1.cc64"})
    assert runner.state()["pending"] == "next"
    runner.handle_midi({"type": "midi", "device": "x", "channel": 1, "kind": "note_on", "number": 37,
                        "value": 0, "id": "ch1.note37"})       # velocity 0 = release -> ignored
    assert runner.state()["running"]
    runner.handle_midi({"type": "midi", "device": "x", "channel": 1, "kind": "note_on", "number": 37,
                        "value": 100, "id": "ch1.note37"})
    assert runner.state()["running"] is False
    eng.close()


def test_goto_not_implemented_and_restart(example_yaml):
    score, eng, rec, runner = make(example_yaml)
    with pytest.raises(NotImplementedError):
        runner.goto("verse")
    runner.start()
    runner.stop_all()
    runner.start()                                       # restart after stop works
    assert runner.state()["running"] and runner.state()["section_id"] == "intro"
    runner.stop_all()
    eng.close()
