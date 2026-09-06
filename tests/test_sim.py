import threading

from conftest import wait_for
from looper.engine.sim import SimEngine


def test_sim_engine_beats_and_clip_lifecycle():
    eng = SimEngine(bpm=120, beats_per_bar=4, speed=40)
    beats: list[int] = []
    states: list[tuple[int, int, str]] = []
    eng.on_beat(beats.append)
    eng.on_clip_state(lambda t, s, st: states.append((t, s, st)))
    eng.connect()
    eng.set_quantization_bar()
    eng.start_transport()
    assert wait_for(lambda: len(beats) >= 3)
    assert beats[:3] == [0, 1, 2]                      # once per beat, increasing

    # firing an empty slot on an unarmed track does nothing
    eng.fire_slot(0, 0)
    assert wait_for(lambda: len(beats) >= 8)
    assert not eng.slot_has_clip(0, 0)

    eng.arm(0, True)
    eng.fire_slot(0, 0)                                  # -> recording from next bar line
    assert wait_for(lambda: (0, 0, "recording") in states)
    assert eng.slot_has_clip(0, 0)
    rec_beat = beats[-1]
    eng.fire_slot(0, 0)                                  # -> playing from next bar line
    assert wait_for(lambda: (0, 0, "playing") in states)
    # state changes happen exactly on bar lines
    assert rec_beat % 4 in range(4)
    eng.fire_slot(0, 1)                                  # new slot on same track stops slot 0
    assert wait_for(lambda: (0, 1, "recording") in states and (0, 0, "stopped") in states)
    eng.stop_all()
    assert wait_for(lambda: eng.slot_state(0, 1) == "stopped")
    eng.close()
    eng.close()                                          # idempotent
    assert not eng.transport_running


def test_sim_engine_callbacks_only_on_bar_lines():
    eng = SimEngine(bpm=120, beats_per_bar=3, speed=40)
    events: list[tuple[int, str]] = []
    last_beat = {"b": -1}
    lock = threading.Lock()

    def on_beat(b):
        with lock:
            last_beat["b"] = b

    def on_clip(t, s, st):
        with lock:
            events.append((last_beat["b"], st))

    eng.on_beat(on_beat)
    eng.on_clip_state(on_clip)
    eng.connect()
    eng.start_transport()
    eng.arm(1, True)
    assert wait_for(lambda: last_beat["b"] >= 1)
    eng.fire_slot(1, 0)
    assert wait_for(lambda: len(events) >= 1)
    beat, state = events[0]
    assert state == "recording" and beat % 3 == 0
    eng.close()
