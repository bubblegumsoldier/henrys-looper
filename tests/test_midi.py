import mido

from looper.midi import MidiInput, midi_id, normalize_message


def test_normalize_note_and_cc():
    ev = normalize_message(mido.Message("note_on", channel=0, note=36, velocity=127), "MPD218")
    assert ev == {"type": "midi", "device": "MPD218", "channel": 1, "kind": "note_on",
                  "number": 36, "value": 127, "id": "ch1.note36"}
    ev = normalize_message(mido.Message("control_change", channel=9, control=64, value=127), "Pedal")
    assert ev["kind"] == "cc" and ev["channel"] == 10 and ev["id"] == "ch10.cc64"
    ev = normalize_message(mido.Message("note_off", channel=0, note=36, velocity=0), "x")
    assert ev["kind"] == "note_off" and ev["value"] == 0
    assert normalize_message(mido.Message("pitchwheel", channel=0, pitch=0), "x") is None
    assert midi_id(1, "cc", 64) == "ch1.cc64" and midi_id(2, "note_on", 5) == "ch2.note5"


def test_midi_input_runs_without_devices():
    logs = []
    events = []
    midi = MidiInput(events.append, ports=[], on_log=lambda lvl, msg: logs.append((lvl, msg)))
    assert midi.ports == []
    assert any("Kein MIDI-Eingang" in msg for _, msg in logs)
    midi.close()
    midi.close()


def test_midi_input_with_all_available_ports_does_not_raise():
    midi = MidiInput(lambda ev: None)     # currently no controller attached; must still be fine
    assert isinstance(midi.ports, list)
    midi.close()
