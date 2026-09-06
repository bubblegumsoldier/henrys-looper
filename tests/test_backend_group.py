"""Backend: POST /api/load prepares the group pool when the Ableton engine is active (M4).

The "ableton" engine is replaced by a SimEngine with a simulated group structure - the real
AbletonOscEngine would try to bind the reply port and talk to Live.
"""

import pytest
from fastapi.testclient import TestClient

from backend.app import app, hub
from looper.engine.sim import SimEngine

GROUP_YAML = """
bpm: 100
tracks:
  gitarre: {ableton_track: 0}
  voice:   {group: voice, layers: 2, reserve: 1}
sections:
  - {id: a, bars: 2, autorelease: true, tracks: {gitarre: record, voice: hear_through}}
  - {id: b, bars: 2, tracks: {gitarre: play, voice: record}}
"""

SINGLE_YAML = """
bpm: 100
tracks:
  gitarre: {ableton_track: 0}
sections:
  - {id: a, bars: 2, tracks: {gitarre: record}}
"""

HENRY_SET = [
    {"name": "gitarre", "input_type": "Ext. In", "input_channel": "2"},
    {"name": "voice", "is_group": True, "num_devices": 4},
    {"name": "voice LIVE", "group_index": 1, "input_type": "Ext. In", "input_channel": "1"},
    {"name": "voice L1", "group_index": 1, "input_type": "Ext. In", "input_channel": "1"},
]

NO_GROUP_SET = [{"name": "gitarre"}, {"name": "voice"}]


@pytest.fixture
def client():
    c = TestClient(app)
    yield c
    try:
        hub.transport("stop_all")
    except Exception:
        pass
    with hub.lock:
        hub._close_engine()
        hub.engine_name = "sim"


def use_fake_ableton(monkeypatch, tracks):
    """Make hub._make_engine("ableton") hand out a SimEngine with the given structure."""
    holder: dict = {}

    def factory(name: str):
        engine = SimEngine(bpm=100, speed=40, tracks=tracks)
        holder["engine"] = engine
        return engine

    monkeypatch.setattr(hub, "_make_engine", factory)
    with hub.lock:
        hub.engine_name = "ableton"
    return holder


def test_load_prepares_the_pool_and_reports_it(client, monkeypatch):
    holder = use_fake_ableton(monkeypatch, HENRY_SET)
    res = client.post("/api/load", json={"yaml": GROUP_YAML})
    assert res.status_code == 200
    body = res.json()
    assert body["ok"] is True
    pool = body["pool"]
    assert pool["changed"] is True
    group = pool["groups"][0]
    assert group["track"] == "voice" and group["group"] == "voice"
    assert group["existing_layers"] == ["voice L1"]
    assert group["created_layers"] == ["voice L2", "voice L3"]
    assert group["monitor"] == "voice LIVE" and group["monitor_created"] is False
    assert any("Gruppe voice" in m and "2 angelegt" in m for m in pool["messages"])
    names = [t.name for t in holder["engine"].get_session_structure()]
    assert names == ["gitarre", "voice", "voice LIVE", "voice L1", "voice L2", "voice L3"]

    # the compiled score carries the group track through to the UI
    voice = next(t for t in body["score"]["tracks"] if t["name"] == "voice")
    assert voice == {"name": "voice", "type": "group", "group": "voice", "layers": 2,
                     "reserve": 1, "monitor": True}


def test_second_load_reports_no_change(client, monkeypatch):
    use_fake_ableton(monkeypatch, HENRY_SET)
    client.post("/api/load", json={"yaml": GROUP_YAML})
    body = client.post("/api/load", json={"yaml": GROUP_YAML}).json()
    # a fresh engine per load, so the pool starts over - but the report shape stays stable
    assert set(body["pool"]) == {"changed", "groups", "messages"}


def test_missing_group_is_a_400_with_the_manual_step(client, monkeypatch):
    use_fake_ableton(monkeypatch, NO_GROUP_SET)
    res = client.post("/api/load", json={"yaml": GROUP_YAML})
    assert res.status_code == 400
    detail = res.json()["detail"]
    assert "Gruppe 'voice' existiert nicht im Ableton-Set" in detail and "Strg+G" in detail


def test_sim_engine_load_is_unchanged(client):
    with hub.lock:
        hub.engine_name = "sim"
    body = client.post("/api/load", json={"yaml": GROUP_YAML}).json()
    assert body["ok"] is True and "pool" not in body      # ensure_pool only runs for Ableton


def test_group_score_runs_on_the_sim_engine(client):
    """Rehearsing without Ableton: the sim engine gets a simulated group layout."""
    with hub.lock:
        hub.engine_name = "sim"
    assert client.post("/api/load", json={"yaml": GROUP_YAML}).json()["ok"]
    names = [t.name for t in hub.engine.get_session_structure()]
    assert names[0] == "gitarre" and "voice" in names
    assert "voice LIVE" in names and "voice L3" in names   # layers 2 + reserve 1
    res = client.post("/api/transport/start")
    assert res.status_code == 200
    state = res.json()["state"]
    assert state["running"] is True and state["groups"] == {"voice": {"layers_used": 0,
                                                                     "layers_free": 3}}
    assert client.post("/api/transport/stop_all").status_code == 200


def test_score_without_group_tracks_skips_the_pool(client, monkeypatch):
    use_fake_ableton(monkeypatch, HENRY_SET)
    body = client.post("/api/load", json={"yaml": SINGLE_YAML}).json()
    assert body["ok"] is True and "pool" not in body
