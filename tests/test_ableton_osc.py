"""AbletonOscEngine against a fake AbletonOSC responder on free ports (never 11000/11001)."""

import socket
import threading
import time

import pytest
from pythonosc.dispatcher import Dispatcher
from pythonosc.osc_server import ThreadingOSCUDPServer
from pythonosc.udp_client import SimpleUDPClient

from conftest import wait_for
from looper.engine.ableton_osc import AbletonOscEngine, _playing_status_to_state
from looper.engine.base import EngineConnectionError

HOST = "127.0.0.1"


def free_udp_port() -> int:
    with socket.socket(socket.AF_INET, socket.SOCK_DGRAM) as s:
        s.bind((HOST, 0))
        return s.getsockname()[1]


class FakeAbleton:
    """Minimal AbletonOSC stand-in: listens on `port`, replies to `reply_port` with (*indices, *values)."""

    def __init__(self, port: int, reply_port: int, tempo: float = 120.0,
                 track_names: list[str] | None = None):
        self.tempo = tempo
        self.quantization = 4
        self.signature = [4, 4]
        self.track_names = list(track_names) if track_names is not None else ["gitarre", "voice"]
        self.received: list[tuple] = []
        self.client = SimpleUDPClient(HOST, reply_port)
        self._beat_thread: threading.Thread | None = None
        self._beat_stop = threading.Event()
        d = Dispatcher()
        d.set_default_handler(self._handle)
        self.server = ThreadingOSCUDPServer((HOST, port), d)
        threading.Thread(target=self.server.serve_forever, daemon=True).start()

    def _handle(self, address: str, *args):
        self.received.append((address, *args))
        if address == "/live/song/get/tempo":
            self.client.send_message(address, [self.tempo])
        elif address == "/live/song/get/signature_numerator":
            self.client.send_message(address, [self.signature[0]])
        elif address == "/live/song/get/signature_denominator":
            self.client.send_message(address, [self.signature[1]])
        elif address == "/live/song/set/signature_numerator":
            self.signature[0] = int(args[0])
        elif address == "/live/song/set/signature_denominator":
            self.signature[1] = int(args[0])
        elif address == "/live/song/get/num_tracks":
            self.client.send_message(address, [len(self.track_names)])
        elif address == "/live/track/get/name":
            index = int(args[0])
            if 0 <= index < len(self.track_names):
                self.client.send_message(address, [index, self.track_names[index]])
            # like AbletonOSC: a track that does not exist gets no reply at all
        elif address == "/live/song/get/clip_trigger_quantization":
            self.client.send_message(address, [self.quantization])
        elif address == "/live/song/set/clip_trigger_quantization":
            self.quantization = int(args[0])
        elif address == "/live/song/set/tempo":
            self.tempo = float(args[0])
        elif address == "/live/song/start_listen/beat":
            self._beat_stop.clear()
            self._beat_thread = threading.Thread(target=self._beats, daemon=True)
            self._beat_thread.start()
        elif address == "/live/song/stop_listen/beat":
            self._beat_stop.set()
        elif address == "/live/clip_slot/get/has_clip":
            self.client.send_message(address, [args[0], args[1], 0])
        elif address == "/live/clip_slot/fire":
            # Ableton would report the slot state on the next bar line; we answer immediately
            self.client.send_message("/live/clip_slot/get/playing_status", [args[0], args[1], 2])
        elif address == "/live/song/get/nonexistent":
            self.client.send_message("/live/error", ["AttributeError: nonexistent"])

    def _beats(self):
        beat = 0
        while not self._beat_stop.wait(0.015):
            self.client.send_message("/live/song/get/beat", [beat])
            beat += 1

    def close(self):
        self._beat_stop.set()
        self.server.shutdown()
        self.server.server_close()


@pytest.fixture
def ports():
    return free_udp_port(), free_udp_port()


def test_connect_beats_and_commands(ports):
    send_port, reply_port = ports
    fake = FakeAbleton(send_port, reply_port)
    eng = AbletonOscEngine(HOST, send_port, reply_port)
    beats: list[int] = []
    states: list[tuple] = []
    eng.on_beat(beats.append)
    eng.on_clip_state(lambda t, s, st: states.append((t, s, st)))
    try:
        t0 = time.monotonic()
        eng.connect()
        assert time.monotonic() - t0 < 2.0
        assert eng.connected and eng.get_beats_per_bar() == 4
        assert eng.get_tempo() == 120.0
        assert wait_for(lambda: len(beats) >= 5)
        assert beats[:5] == [0, 1, 2, 3, 4]

        eng.set_tempo(100)
        eng.set_time_signature(3, 4)
        assert eng.get_beats_per_bar() == 3
        eng.set_quantization_bar()
        assert wait_for(lambda: fake.quantization == 5 and fake.tempo == 100.0)
        assert wait_for(lambda: fake.signature == [3, 4])
        assert ("/live/song/set/signature_numerator", 3) in fake.received
        assert ("/live/song/set/signature_denominator", 4) in fake.received
        eng.arm(0, True)
        eng.set_monitoring(1, "in")
        eng.start_transport()
        assert eng.slot_has_clip(0, 0) is False
        eng.fire_slot(0, 0)
        assert wait_for(lambda: (0, 0, "recording") in states)
        eng.stop_track(0)
        eng.stop_all()
        eng.stop_transport()
        assert wait_for(lambda: ("/live/song/stop_playing",) in fake.received)
        addresses = [r[0] for r in fake.received]
        assert ("/live/track/set/arm", 0, 1) in fake.received
        assert ("/live/track/set/current_monitoring_state", 1, 0) in fake.received
        assert "/live/clip_slot/start_listen/playing_status" in addresses
        assert ("/live/clip_slot/fire", 0, 0) in fake.received
        assert ("/live/track/stop_all_clips", 0) in fake.received
        assert "/live/song/stop_all_clips" in addresses and "/live/song/start_playing" in addresses

        t0 = time.monotonic()
        eng.close()
        eng.close()                                  # idempotent
        assert time.monotonic() - t0 < 2.0
        assert wait_for(lambda: fake.quantization == 4)    # original quantization restored
        assert wait_for(lambda: "/live/song/stop_listen/beat" in [r[0] for r in fake.received])
        assert not eng.connected
    finally:
        eng.close()
        fake.close()


def test_connect_timeout_gives_german_hint(ports):
    send_port, reply_port = ports                    # nobody listens on send_port
    eng = AbletonOscEngine(HOST, send_port, reply_port)
    t0 = time.monotonic()
    with pytest.raises(EngineConnectionError) as info:
        eng.connect()
    elapsed = time.monotonic() - t0
    assert 1.5 <= elapsed < 3.0
    msg = str(info.value)
    assert "Keine Antwort von AbletonOSC" in msg and "Control Surface" in msg
    assert not eng.connected
    eng.close()                                      # safe after failed connect
    # the reply port must have been released again
    with socket.socket(socket.AF_INET, socket.SOCK_DGRAM) as s:
        s.bind((HOST, reply_port))


def test_reply_port_in_use_is_reported(ports):
    send_port, reply_port = ports
    blocker = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
    blocker.bind((HOST, reply_port))
    try:
        eng = AbletonOscEngine(HOST, send_port, reply_port)
        with pytest.raises(EngineConnectionError) as info:
            eng.connect()
        assert f"Port {reply_port} ist bereits belegt" in str(info.value)
    finally:
        blocker.close()


def test_track_count_and_names(ports):
    """num_tracks / track name over OSC; a track that Ableton ignores must yield None, not a raise."""
    send_port, reply_port = ports
    fake = FakeAbleton(send_port, reply_port, track_names=["gitarre", "voice"])
    eng = AbletonOscEngine(HOST, send_port, reply_port, reply_timeout=0.4)
    try:
        eng.connect()
        assert eng.get_track_count() == 2
        assert eng.get_track_name(0) == "gitarre"
        assert eng.get_track_name(1) == "voice"
        t0 = time.monotonic()
        assert eng.get_track_name(7) is None            # no reply at all -> timeout -> unknown
        assert time.monotonic() - t0 < 2.0
        assert ("/live/song/get/num_tracks",) in fake.received
        assert ("/live/track/get/name", 0) in fake.received
    finally:
        eng.close()
        fake.close()


def test_playing_status_conversion():
    assert _playing_status_to_state(0) == "stopped"
    assert _playing_status_to_state(1) == "playing"
    assert _playing_status_to_state(2) == "recording"
    assert _playing_status_to_state(2.0) == "recording"
    assert _playing_status_to_state("PlayingStatus.recording") == "recording"
    assert _playing_status_to_state("weird") is None
