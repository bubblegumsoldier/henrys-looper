"""Timing regression tests (Ableton test 2026-09-05): nothing in the beat path may block.

Observed bug: a synchronous slot_has_clip (2 s timeout) inside the beat callback made the
section 2 -> 3 transition one bar late while another OSC client flooded AbletonOSC.
Fake AbletonOSC responders run on free ports only - Ableton itself is never contacted.
"""

import socket
import threading
import time

import pytest
from pythonosc.dispatcher import Dispatcher
from pythonosc.osc_server import ThreadingOSCUDPServer
from pythonosc.udp_client import SimpleUDPClient

from conftest import wait_for
from looper import runner as runner_mod
from looper.engine.ableton_osc import AbletonOscEngine
from looper.engine.sim import SimEngine
from looper.runner import Runner
from looper.score.compile import compile_score

HOST = "127.0.0.1"
BEAT_S = 0.02        # fake beat clock: 20 ms per beat (3/4 -> 60 ms per bar)

THREE_FOUR = """
bpm: 141
time_signature: 3/4
tracks:
  gitarre: {ableton_track: 0}
  voice:   {ableton_track: 1}
  voice2:  {ableton_track: 2}
sections:
  - {id: gitarre_rec,   bars: 4, autorelease: true, tracks: {gitarre: record}}
  - {id: voice_rec,     bars: 4, autorelease: true, tracks: {gitarre: play, voice: record}}
  - {id: voice_overdub, bars: 4, autorelease: true, tracks: {gitarre: play, voice: play, voice2: record}}
  - {id: alles,         bars: 4, autorelease: false, tracks: {gitarre: play, voice: play, voice2: play}}
"""


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

    def messages(self) -> list[str]:
        return [e["message"] for e in self.of("log")]


def section_boundaries(rec: Recorder) -> dict[str, int]:
    """song_beat on which each section was entered (the beat event right after its first state)."""
    with rec.lock:
        events = list(rec.events)
    out: dict[str, int] = {}
    for i, e in enumerate(events):
        if e["type"] == "state" and e["running"] and not e["countin"] and e["section_id"] not in out:
            beat = next((x for x in events[i:] if x["type"] == "beat"), None)
            if beat is not None:
                out[e["section_id"]] = beat["song_beat"]
    return out


def free_udp_port() -> int:
    with socket.socket(socket.AF_INET, socket.SOCK_DGRAM) as s:
        s.bind((HOST, 0))
        return s.getsockname()[1]


class SlowFakeAbleton:
    """AbletonOSC stand-in with its own beat clock (while "playing").

    has_clip gets are answered after `has_clip_delay` seconds or never (None), like a busy
    AbletonOSC. Every received message is tagged with the beat that was current on arrival.
    """

    def __init__(self, port: int, reply_port: int, has_clip_delay: float | None,
                 clips: set[tuple[int, int]] = frozenset()):
        self.has_clip_delay = has_clip_delay
        self.clips = set(clips)
        self.beat = -1
        self.received: list[tuple[int, tuple]] = []
        self.lock = threading.Lock()
        self.client = SimpleUDPClient(HOST, reply_port)
        self._stop = threading.Event()
        self._stop.set()
        d = Dispatcher()
        d.set_default_handler(self._handle)
        self.server = ThreadingOSCUDPServer((HOST, port), d)
        threading.Thread(target=self.server.serve_forever, daemon=True).start()

    def _handle(self, address: str, *args):
        with self.lock:
            self.received.append((self.beat, (address, *args)))
        if address == "/live/song/get/tempo":
            self.client.send_message(address, [141.0])
        elif address == "/live/song/get/signature_numerator":
            self.client.send_message(address, [3])
        elif address == "/live/song/get/clip_trigger_quantization":
            self.client.send_message(address, [4])
        elif address == "/live/song/start_playing":
            if self._stop.is_set():
                self._stop.clear()
                threading.Thread(target=self._beats, daemon=True).start()
        elif address == "/live/song/stop_playing":
            self._stop.set()
        elif address == "/live/clip_slot/get/has_clip":
            if self.has_clip_delay is None:
                return                                   # never answered
            t, s = int(args[0]), int(args[1])
            threading.Timer(self.has_clip_delay, self.client.send_message,
                            [address, [t, s, int((t, s) in self.clips)]]).start()
        elif address == "/live/clip_slot/fire":
            t, s = int(args[0]), int(args[1])
            self.clips.add((t, s))
            self.client.send_message("/live/clip_slot/get/playing_status", [t, s, 2])

    def _beats(self):
        n = 0
        while not self._stop.wait(BEAT_S):
            with self.lock:
                self.beat = n
            self.client.send_message("/live/song/get/beat", [n])
            n += 1

    def fires(self) -> list[tuple[int, int, int]]:
        with self.lock:
            return [(b, int(m[1]), int(m[2])) for b, m in self.received if m[0] == "/live/clip_slot/fire"]

    def addresses(self) -> list[tuple]:
        with self.lock:
            return [m for _, m in self.received]

    def close(self):
        self._stop.set()
        self.server.shutdown()
        self.server.server_close()


@pytest.fixture
def ports():
    return free_udp_port(), free_udp_port()


# ------------------------------------------------------------------ reproduction / fix
@pytest.mark.parametrize("delay", [0.3, None], ids=["has_clip-late-300ms", "has_clip-never"])
def test_transitions_on_time_despite_slow_has_clip(ports, delay):
    """Fires for record sections must reach 'Ableton' LEAD beats before the bar line -
    even when has_clip gets are answered late or not at all (the observed live bug)."""
    send_port, reply_port = ports
    fake = SlowFakeAbleton(send_port, reply_port, has_clip_delay=delay)
    score = compile_score(THREE_FOUR)
    eng = AbletonOscEngine(HOST, send_port, reply_port)
    rec = Recorder()
    runner = Runner(eng, score, rec)
    try:
        t0 = time.monotonic()
        runner.start()
        assert time.monotonic() - t0 < 4.0             # slot scan is bounded (1.5 s) and happens before the count-in
        assert wait_for(lambda: runner.state()["section_id"] == "alles", timeout=10)
        runner.stop_all()
    finally:
        eng.close()
        fake.close()

    boundaries = section_boundaries(rec)
    lead = runner_mod.LEAD_BEATS
    for sec_id, track in (("gitarre_rec", 0), ("voice_rec", 1), ("voice_overdub", 2)):
        boundary = boundaries[sec_id]
        fire = [b for b, t, s in fake.fires() if t == track and s == 0]
        assert fire, f"kein fire für Track {track}"
        assert boundary - lead <= fire[0] < boundary, \
            f"{sec_id}: fire für Track {track} kam bei Beat {fire[0]}, Taktgrenze war Beat {boundary}"
    assert not any("ohne Vorlauf" in m for m in rec.messages())
    if delay is None:
        assert any("ohne Antwort" in e["message"] for e in rec.of("log") if e["level"] == "warn")
    else:
        assert any("Slot-Abfrage" in m and "belegt: keine" in m for m in rec.messages())


class SlowSim(SimEngine):
    """slot_has_clip takes 1.5 s (busy AbletonOSC) - the runner must never call it while running."""

    def __init__(self, *args, **kwargs):
        super().__init__(*args, **kwargs)
        self.has_clip_calls = 0

    def slot_has_clip(self, track: int, scene: int) -> bool:
        self.has_clip_calls += 1
        time.sleep(1.5)
        return super().slot_has_clip(track, scene)


def test_runner_never_queries_slots_in_beat_path():
    score = compile_score(THREE_FOUR)
    eng = SlowSim(bpm=score.bpm, beats_per_bar=3, speed=40)
    rec = Recorder()
    runner = Runner(eng, score, rec)
    t0 = time.monotonic()
    runner.start()
    assert wait_for(lambda: runner.state()["section_id"] == "alles", timeout=5)
    runner.stop_all()
    eng.close()
    assert eng.has_clip_calls == 0
    assert time.monotonic() - t0 < 3.0
    b = section_boundaries(rec)
    assert b["voice_rec"] - b["gitarre_rec"] == 12 and b["alles"] - b["voice_overdub"] == 12
    assert not any("ohne Vorlauf" in m for m in rec.messages())


def test_beat_callback_budget_is_measured(monkeypatch):
    monkeypatch.setattr(runner_mod, "BEAT_BUDGET_S", 0.0)
    score = compile_score(THREE_FOUR)
    eng = SimEngine(bpm=score.bpm, beats_per_bar=3, speed=40)
    rec = Recorder()
    runner = Runner(eng, score, rec)
    runner.start()
    assert wait_for(lambda: any("Beat-Callback dauerte" in e["message"] and e["level"] == "warn"
                                for e in rec.of("log")))
    runner.stop_all()
    eng.close()


# ------------------------------------------------------------------------ slot cache
def test_slot_cache_follows_manual_clip_edits():
    yaml_text = """
bpm: 100
tracks:
  g: {ableton_track: 0}
sections:
  - {id: a, bars: 2, autorelease: true, tracks: {g: record}}
  - {id: b, bars: 2, autorelease: true, tracks: {g: play}}
  - {id: c, bars: 2, autorelease: true, tracks: {g: overdub}}
  - {id: d, bars: 2, autorelease: false, tracks: {g: play}}
"""
    score = compile_score(yaml_text)
    eng = SimEngine(bpm=100, beats_per_bar=4, speed=20)
    eng.set_clip(0, 0, True)                 # the Live set already has clips in slots 0 and 1
    eng.set_clip(0, 1, True)
    rec = Recorder()
    runner = Runner(eng, score, rec)
    runner.start()
    assert any("belegt: g/Slot 0, g/Slot 1" in m for m in rec.messages())
    assert any("Vorausplanung Sektion 1 'a': g → Slot 2" in m for m in rec.messages())

    eng.set_clip(0, 1, False)                # Henry deletes the clip in slot 1 during the count-in
    assert wait_for(lambda: runner.state()["section_id"] == "a")
    assert wait_for(lambda: eng.slot_state(0, 1) == "recording")   # re-checked at dispatch: slot 1 free again
    assert any("abweichend von Vorausplanung (Slot 2) → Slot 1" in m for m in rec.messages())
    assert any("Slot g/Slot 1: Clip entfernt" in m for m in rec.messages())

    assert wait_for(lambda: runner.state()["section_id"] == "b")
    eng.set_clip(0, 2, True)                 # a clip appears by hand in slot 2 -> the overdub skips it
    assert wait_for(lambda: runner.state()["section_id"] == "c")
    assert wait_for(lambda: eng.slot_state(0, 3) == "recording")
    assert wait_for(lambda: runner.state()["section_id"] == "d")
    runner.stop_all()
    eng.close()
    assert any("Slot g/Slot 2: Clip angelegt" in m for m in rec.messages())
    assert any("abweichend von Vorausplanung (Slot 2) → Slot 3" in m for m in rec.messages())


def test_prime_slots_batches_gets_and_times_out(ports):
    send_port, reply_port = ports
    fake = SlowFakeAbleton(send_port, reply_port, has_clip_delay=0.05, clips={(0, 1)})
    eng = AbletonOscEngine(HOST, send_port, reply_port)
    try:
        eng.connect()
        t0 = time.monotonic()
        result = eng.prime_slots([(0, 0), (0, 1), (1, 0)], timeout=1.0)
        assert time.monotonic() - t0 < 0.5           # one shared wait, not 3 x delay
        assert result == {(0, 0): False, (0, 1): True, (1, 0): False}
        assert wait_for(lambda: ("/live/clip_slot/start_listen/has_clip", 0, 1) in fake.addresses())
    finally:
        eng.close()
        fake.close()

    send_port, reply_port = free_udp_port(), free_udp_port()
    fake = SlowFakeAbleton(send_port, reply_port, has_clip_delay=None)
    eng = AbletonOscEngine(HOST, send_port, reply_port)
    try:
        eng.connect()
        t0 = time.monotonic()
        result = eng.prime_slots([(0, 0), (2, 0)], timeout=0.3)
        assert 0.25 <= time.monotonic() - t0 < 1.0
        assert result == {(0, 0): None, (2, 0): None}
    finally:
        eng.close()
        fake.close()


# --------------------------------------------------------------- reply matching / traffic
def test_request_matches_indices_and_ignores_foreign_traffic(ports):
    send_port, _ = ports
    eng = AbletonOscEngine(HOST, send_port, free_udp_port())
    eng._client = SimpleUDPClient(HOST, send_port)        # nobody listens; replies are injected below
    seen: list[tuple] = []
    eng.on_slot_has_clip(lambda t, s, v: seen.append((t, s, v)))
    result: dict = {}

    def ask():
        try:
            result["value"] = eng.slot_has_clip(2, 0)
        except Exception as exc:  # pragma: no cover
            result["error"] = exc

    th = threading.Thread(target=ask, daemon=True)
    th.start()
    assert wait_for(lambda: bool(eng._pending.get("/live/clip_slot/get/has_clip")))

    # listener updates for other slots, foreign gets and garbage must not satisfy the request
    eng._on_has_clip("/live/clip_slot/get/has_clip", 2, 1, 1)
    eng._on_has_clip("/live/clip_slot/get/has_clip", 0, 0, 1)
    eng._on_any_reply("/live/track/get/name", 2, "voice2")
    eng._on_any_reply("/live/song/get/current_song_time", 12.5)
    eng._on_has_clip("/live/clip_slot/get/has_clip", "garbage")
    eng._on_has_clip("/live/clip_slot/get/has_clip", "x", "y", 1)
    eng._on_playing_status("/live/clip_slot/get/playing_status", 1, 0, 1)
    eng._on_playing_status("/live/clip_slot/get/playing_status")
    time.sleep(0.05)
    assert th.is_alive() and "value" not in result

    eng._on_has_clip("/live/clip_slot/get/has_clip", 2, 0, 0)       # the real reply
    th.join(2.0)
    assert not th.is_alive() and result.get("value") is False
    # listener information from the same messages was still processed (cache updates)
    assert (2, 1, True) in seen and (0, 0, True) in seen and (2, 0, False) in seen
    eng.close()


# ------------------------------------------------------------ next_section in last section
@pytest.mark.parametrize("quantize", ["loop", "bar"])
def test_next_in_last_section_stops_at_boundary(quantize):
    yaml_text = f"""
bpm: 100
tracks:
  g: {{ableton_track: 0}}
sections:
  - {{id: a, bars: 1, autorelease: true, tracks: {{g: record}}}}
  - {{id: b, bars: 4, autorelease: false, quantize: {quantize}, tracks: {{g: play}}}}
"""
    score = compile_score(yaml_text)
    eng = SimEngine(bpm=100, beats_per_bar=4, speed=20)
    rec = Recorder()
    runner = Runner(eng, score, rec)
    runner.start()
    assert wait_for(lambda: runner.state()["section_id"] == "b" and runner.state()["bar"] == 2)
    assert wait_for(lambda: eng.slot_state(0, 0) == "playing")
    runner.next_section()
    st = runner.state()
    assert st["pending"] == "stop" and st["running"]
    runner.next_section()                                # idempotent while pending
    assert runner.state()["pending"] == "stop"
    assert wait_for(lambda: not runner.state()["running"])
    assert wait_for(lambda: eng.slot_state(0, 0) == "stopped" and not eng.transport_running)
    assert not eng.is_armed(0)
    eng.close()

    beats = [e for e in rec.of("beat") if e["bar"] >= 1]
    last = beats[-1]
    assert last["beat"] == 4                             # idle exactly on a bar line
    if quantize == "loop":
        assert last["bar"] == 4                          # ... at the end of the 4-bar cycle
    else:
        assert last["bar"] < 4                           # ... on the next bar line
    msgs = rec.messages()
    assert any("Ende armiert" in m for m in msgs)
    assert any("Vorbereitung Ende" in m for m in msgs)
    assert any(m.startswith("Ende:") for m in msgs)
    final = rec.of("state")[-1]
    assert final["running"] is False and final["pending"] is None and final["tracks"] == {"g": "stop"}
