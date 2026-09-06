"""Backend: a score referencing a track the set does not have is a 400, not a 500.

TestClient is used without its context manager on purpose: that skips the lifespan
(no MIDI device is opened, no default score is loaded in the background).
"""

import pytest
from fastapi.testclient import TestClient

from backend.app import app, hub
from test_track_check import MISSING_TRACK_YAML, TWO_TRACK_YAML


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


@pytest.fixture
def events(monkeypatch):
    """Collect everything the hub emits (WebSocket broadcast is not running here)."""
    collected: list[dict] = []
    original = hub.emit

    def spy(event: dict) -> None:
        collected.append(dict(event))
        original(event)

    monkeypatch.setattr(hub, "emit", spy)
    return collected


def test_missing_track_gives_400_and_runner_stays_usable(client, events):
    assert client.post("/api/load", json={"yaml": MISSING_TRACK_YAML}).json()["ok"]
    hub.engine.track_count = 2                     # simulate Henry's 2-track set

    res = client.post("/api/transport/start")
    assert res.status_code == 400                  # not 500, not a crash
    detail = res.json()["detail"]
    assert "Spur 2 ('voice2')" in detail and "Spur 3 ('voice_live')" in detail
    assert "nur 2 Spuren (0–1)" in detail

    # the UI gets the same message as an error log event
    errors = [e for e in events if e.get("type") == "log" and e.get("level") == "error"]
    assert errors and errors[-1]["message"] == detail

    # runner stays idle and operable
    state = client.get("/api/state").json()
    assert state["running"] is False
    assert client.post("/api/transport/stop_all").status_code == 200

    # a score that fits the set still starts normally afterwards
    assert client.post("/api/load", json={"yaml": TWO_TRACK_YAML}).json()["ok"]
    res = client.post("/api/transport/start")
    assert res.status_code == 200 and res.json()["state"]["running"] is True
    assert client.post("/api/transport/stop_all").status_code == 200
