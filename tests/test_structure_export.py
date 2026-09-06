"""AbletonOscEngine.get_session_structure() against a fake AbletonOSC on free ports.

Facts reproduced from the spike of 2026-09-06 (Live 11.3.30):
  * /live/song/export/structure writes %TEMP%/abletonosc-song-structure.json;
  * tracks[].group_track carries the parent index (null on top level);
  * the file can still be half-written when we read it -> retry until it parses;
  * without the export the engine falls back to per-track gets (is_foldable / is_grouped).
The temp directory is redirected via ABLETONOSC_STRUCTURE_DIR so the real %TEMP% stays clean.
"""

import copy
import json
import socket
import threading
import time

import pytest
from pythonosc.dispatcher import Dispatcher
from pythonosc.osc_server import ThreadingOSCUDPServer
from pythonosc.udp_client import SimpleUDPClient

from looper.engine.ableton_osc import STRUCTURE_FILENAME, AbletonOscEngine
from looper.engine.base import EngineError

HOST = "127.0.0.1"

# 0 gitarre | 1 voice (group, 4 devices) | 2 voice LIVE | 3 voice L1 | 4 drums
SET = [
    {"index": 0, "name": "gitarre", "is_foldable": False, "group_track": None, "devices": [],
     "clips": [], "input_routing_type": "Ext. In", "input_routing_channel": "2"},
    {"index": 1, "name": "voice", "is_foldable": True, "group_track": None,
     "devices": [{"name": "Reverb"}, {"name": "EQ Eight"}, {"name": "Compressor"},
                 {"name": "Saturator"}], "clips": []},
    {"index": 2, "name": "voice LIVE", "is_foldable": False, "group_track": 1, "devices": [],
     "clips": [], "input_routing_type": {"display_name": "Ext. In"},
     "input_routing_channel": {"display_name": "1"}},
    {"index": 3, "name": "voice L1", "is_foldable": False, "group_track": 1, "devices": [],
     "clips": [None, {"name": "voice L1"}], "input_routing_type": "Ext. In",
     "input_routing_channel": "1"},
    {"index": 4, "name": "drums", "is_foldable": False, "group_track": None, "devices": [],
     "clips": []},
]


def free_udp_port() -> int:
    with socket.socket(socket.AF_INET, socket.SOCK_DGRAM) as s:
        s.bind((HOST, 0))
        return s.getsockname()[1]


class FakeStructureAbleton:
    """Answers tempo/num_tracks/name/is_grouped/is_foldable and writes the structure file."""

    def __init__(self, port: int, reply_port: int, out_dir, tracks=None,
                 export: bool = True, partial_writes: int = 0):
        self.tracks = copy.deepcopy(SET if tracks is None else tracks)   # tests must not share dicts
        self.out_dir = out_dir
        self.export = export
        self.partial_writes = partial_writes
        self.received: list[tuple] = []
        self.client = SimpleUDPClient(HOST, reply_port)
        d = Dispatcher()
        d.set_default_handler(self._handle)
        self.server = ThreadingOSCUDPServer((HOST, port), d)
        threading.Thread(target=self.server.serve_forever, daemon=True).start()

    @property
    def path(self):
        return self.out_dir / STRUCTURE_FILENAME

    def _handle(self, address: str, *args):
        self.received.append((address, *args))
        reply = self.client.send_message
        if address == "/live/song/get/tempo":
            reply(address, [120.0])
        elif address == "/live/song/get/signature_numerator":
            reply(address, [4])
        elif address == "/live/song/get/num_tracks":
            reply(address, [len(self.tracks)])
        elif address == "/live/song/export/structure":
            if self.export:
                threading.Thread(target=self._write, daemon=True).start()
        elif address == "/live/song/create_audio_track":
            self._insert(int(args[0]), {"name": "", "is_foldable": False,
                                        "group_track": self._parent_at(int(args[0])),
                                        "devices": [], "clips": []})
        elif address == "/live/song/duplicate_track":
            source = self.tracks[int(args[0])]
            self._insert(int(args[0]) + 1, {**{k: v for k, v in source.items() if k != "index"},
                                            "clips": list(source.get("clips") or [])})
        elif address == "/live/track/set/name":
            self.tracks[int(args[0])]["name"] = str(args[1])
        elif address == "/live/track/set/input_routing_type":
            self.tracks[int(args[0])]["input_routing_type"] = str(args[1])
        elif address == "/live/track/set/input_routing_channel":
            self.tracks[int(args[0])]["input_routing_channel"] = str(args[1])
        elif address == "/live/track/get/available_input_routing_types":
            reply(address, [args[0], "Ext. In", "Resampling", "No Input"])
        elif address == "/live/track/get/available_input_routing_channels":
            reply(address, [args[0], "1/2", "1", "2"])
        elif address.startswith("/live/track/get/"):
            self._track_get(address, int(args[0]))

    def _parent_at(self, index: int):
        """A track created inside a group inherits that group (index rule of the spike)."""
        previous = self.tracks[index - 1] if 0 < index <= len(self.tracks) else None
        if previous is None:
            return None
        return previous["index"] if previous.get("is_foldable") else previous.get("group_track")

    def _insert(self, index: int, track: dict) -> None:
        """Insert like Live: indices shift and default names are silently renumbered."""
        index = max(0, min(index, len(self.tracks)))
        self.tracks.insert(index, track)
        for i, t in enumerate(self.tracks):
            parent = t.get("group_track")
            if i != index and parent is not None and parent >= index:
                t["group_track"] = parent + 1
            t["index"] = i
            if not t.get("name") or t["name"].endswith("-Audio"):
                t["name"] = f"{i + 1}-Audio"

    def _track_get(self, address: str, index: int):
        if not 0 <= index < len(self.tracks):
            return                                   # like AbletonOSC: no reply at all
        track = self.tracks[index]
        field = address.rsplit("/", 1)[-1]
        values = {
            "name": track.get("name", ""),
            "is_foldable": int(bool(track.get("is_foldable"))),
            "is_grouped": int(track.get("group_track") is not None),
            "num_devices": len(track.get("devices") or []),
            "input_routing_type": track.get("input_routing_type") or "No Input",
            "input_routing_channel": track.get("input_routing_channel") or "",
        }
        value = values.get(field)
        if isinstance(value, dict):
            value = value.get("display_name")
        if value is not None:
            self.client.send_message(address, [index, value])

    def _write(self):
        payload = json.dumps({"tracks": self.tracks})
        for _ in range(self.partial_writes):         # half-written file, as observed in the spike
            self.path.write_text(payload[: len(payload) // 3], encoding="utf-8")
            time.sleep(0.05)
        self.path.write_text(payload, encoding="utf-8")

    def close(self):
        self.server.shutdown()
        self.server.server_close()


@pytest.fixture
def structure_dir(tmp_path, monkeypatch):
    monkeypatch.setenv("ABLETONOSC_STRUCTURE_DIR", str(tmp_path))
    return tmp_path


def make(structure_dir, **kwargs):
    send_port, reply_port = free_udp_port(), free_udp_port()
    fake = FakeStructureAbleton(send_port, reply_port, structure_dir, **kwargs)
    engine = AbletonOscEngine(HOST, send_port, reply_port, reply_timeout=0.5)
    return fake, engine


def assert_henry_layout(structure):
    assert [t.name for t in structure] == ["gitarre", "voice", "voice LIVE", "voice L1", "drums"]
    group = structure.group("voice")
    assert group is not None and group.index == 1 and group.num_devices == 4
    assert [c.index for c in structure.children(1)] == [2, 3]
    assert structure.by_name("voice L1").group_index == 1
    assert structure.by_name("drums").group_index is None
    assert structure.at(0).is_group is False


def test_export_structure_is_read_and_parsed(structure_dir):
    fake, engine = make(structure_dir)
    try:
        engine.connect()
        structure = engine.get_session_structure()
        assert_henry_layout(structure)
        live = structure.by_name("voice LIVE")
        assert (live.input_type, live.input_channel) == ("Ext. In", "1")   # dict form
        assert structure.by_name("gitarre").input_channel == "2"           # plain string form
        assert structure.by_name("voice L1").num_clips == 1                # None entries ignored
        # one call instead of many gets: no per-track queries were needed
        assert ("/live/song/export/structure",) in fake.received
        assert [r for r in fake.received if r[0].startswith("/live/track/get/")] == []
        assert structure.to_dict()["tracks"][0]["name"] == "gitarre"
    finally:
        engine.close()
        fake.close()


def test_half_written_file_is_retried_until_it_parses(structure_dir):
    fake, engine = make(structure_dir, partial_writes=3)
    try:
        engine.connect()
        t0 = time.monotonic()
        structure = engine.get_session_structure()
        assert time.monotonic() - t0 < 3.0
        assert_henry_layout(structure)
    finally:
        engine.close()
        fake.close()


def test_a_stale_file_is_never_used(structure_dir):
    """The old file is removed before the request - a silent export must not look successful."""
    fake, engine = make(structure_dir, export=False)
    (structure_dir / STRUCTURE_FILENAME).write_text(
        json.dumps({"tracks": [{"index": 0, "name": "VERALTET"}]}), encoding="utf-8")
    try:
        engine.connect()
        structure = engine.get_session_structure(timeout=0.3)
        assert [t.name for t in structure] != ["VERALTET"]
        assert_henry_layout(structure)                 # fallback via per-track gets
        assert any(r[0] == "/live/track/get/is_foldable" for r in fake.received)
    finally:
        engine.close()
        fake.close()


def test_fallback_uses_the_index_rule(structure_dir):
    fake, engine = make(structure_dir, export=False)
    try:
        engine.connect()
        structure = engine.get_session_structure(timeout=0.3)
        assert_henry_layout(structure)
        assert structure.by_name("voice").num_devices == 4
        assert structure.by_name("gitarre").input_type == "Ext. In"
        assert not (structure_dir / STRUCTURE_FILENAME).exists()
    finally:
        engine.close()
        fake.close()


def test_structure_of_an_empty_set(structure_dir):
    fake, engine = make(structure_dir, tracks=[])
    try:
        engine.connect()
        assert len(engine.get_session_structure()) == 0
    finally:
        engine.close()
        fake.close()


def test_create_track_in_group_returns_the_verified_index(structure_dir):
    fake, engine = make(structure_dir)
    try:
        engine.connect()
        index = engine.create_track_in_group(group_index=1, after_index=3)
        assert index == 4                                   # behind 'voice L1', still in the group
        structure = engine.get_session_structure()
        assert structure.at(4).group_index == 1 and structure.at(4).name == "5-Audio"
        assert structure.at(5).name == "drums"              # everything behind it shifted
        engine.set_track_name(4, "voice L2")
        assert engine.get_session_structure().by_name("voice L2").index == 4
    finally:
        engine.close()
        fake.close()


def test_create_skips_the_unaddressable_first_child_slot(structure_dir):
    """Spike: asking for g+1 landed on g+2 - the engine must ask for a reachable index."""
    fake, engine = make(structure_dir)
    try:
        engine.connect()
        index = engine.create_track_in_group(group_index=1, after_index=1)
        assert index >= 3 and engine.get_session_structure().at(index).group_index == 1
        assert [r for r in fake.received if r[0] == "/live/song/create_audio_track"] == [
            ("/live/song/create_audio_track", 3)]
    finally:
        engine.close()
        fake.close()


def test_duplicate_track_verifies_group_and_name(structure_dir):
    fake, engine = make(structure_dir)
    try:
        engine.connect()
        index = engine.duplicate_track(3)                   # 'voice L1'
        assert index == 4
        structure = engine.get_session_structure()
        assert structure.at(4).name == "voice L1" and structure.at(4).group_index == 1
        engine.set_track_name(4, "voice L2")
        assert [t.name for t in engine.get_session_structure()] == [
            "gitarre", "voice", "voice LIVE", "voice L1", "voice L2", "drums"]
    finally:
        engine.close()
        fake.close()


def test_duplicating_a_group_is_refused(structure_dir):
    fake, engine = make(structure_dir)
    try:
        engine.connect()
        with pytest.raises(EngineError) as info:
            engine.duplicate_track(1)
        assert "Gruppenspur" in str(info.value)
    finally:
        engine.close()
        fake.close()


def test_rename_without_readback_is_an_error(structure_dir):
    """A silent Ableton must not look like a successful rename."""
    fake, engine = make(structure_dir)
    try:
        engine.connect()
        with pytest.raises(EngineError) as info:
            engine.set_track_name(99, "voice L9")           # no such track -> no reply
        assert "konnte nicht in 'voice L9' umbenannt werden" in str(info.value)
    finally:
        engine.close()
        fake.close()


def test_input_routing_is_matched_against_the_fresh_list(structure_dir):
    fake, engine = make(structure_dir)
    try:
        engine.connect()
        engine.set_input_routing(0, "ext. in", "1")         # case-insensitive match
        assert engine.get_input_routing(0) == ("Ext. In", "1")
        with pytest.raises(EngineError) as info:
            engine.set_input_routing(0, "Gitarrenkabel")
        assert "steht auf Spur 0 nicht zur Verfügung" in str(info.value)
        assert "Ext. In" in str(info.value)
        # the list is queried again for every call (it changes when tracks are added/renamed)
        queries = [r for r in fake.received
                   if r[0] == "/live/track/get/available_input_routing_types"]
        assert len(queries) == 2
    finally:
        engine.close()
        fake.close()


def test_unsupported_engine_methods_raise_german_errors():
    """A minimal engine may not support the group layer - the message must say so."""

    class Bare(AbletonOscEngine):
        pass

    engine = Bare(HOST, free_udp_port(), free_udp_port())
    with pytest.raises(EngineError) as info:
        engine.set_track_name(0, "x")            # not connected -> send fails, German message
    assert "nicht verbunden" in str(info.value)
    engine.close()
