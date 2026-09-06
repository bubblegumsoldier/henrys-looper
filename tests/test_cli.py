import json
import time

import looper.__main__ as cli
from conftest import EXAMPLE, FIXTURES


def test_cli_compile_ok(capsys):
    assert cli.main(["compile", str(EXAMPLE)]) == 0
    data = json.loads(capsys.readouterr().out)
    assert [s["id"] for s in data["sections"]] == ["intro", "verse", "outro"]
    assert cli.main(["compile", str(EXAMPLE), "--json"]) == 0
    assert json.loads(capsys.readouterr().out)["ok"] is True


def test_cli_compile_errors(capsys):
    assert cli.main(["compile", str(FIXTURES / "broken-song.yaml")]) == 1
    out = capsys.readouterr().out
    assert "5 Fehler" in out and "Zeile 15, Spalte 7" in out and "meintest du 'bass'?" in out
    assert cli.main(["compile", str(FIXTURES / "broken-song.yaml"), "--json"]) == 1
    data = json.loads(capsys.readouterr().out)
    assert data["ok"] is False and len(data["errors"]) == 5
    assert set(data["errors"][0]) == {"line", "column", "message", "suggestion"}


def test_cli_run_sim_with_simulated_keys(monkeypatch, capsys):
    """Drive `run --engine sim` with scripted key presses: n (next), n (ignored: last), q (quit)."""
    script = {"t0": None, "sent": set()}

    def fake_poll(self):
        now = time.monotonic()
        if script["t0"] is None:
            script["t0"] = now
        elapsed = now - script["t0"]
        for at, key in ((0.6, "n"), (2.5, "q")):
            if elapsed >= at and key not in script["sent"]:
                script["sent"].add(key)
                return [key]
        return []

    monkeypatch.setattr(cli._Keys, "poll", fake_poll)
    monkeypatch.setattr(cli._Keys, "__init__", lambda self: None)
    t0 = time.monotonic()
    code = cli.main(["run", str(EXAMPLE), "--engine", "sim", "--speed", "30"])
    total = time.monotonic() - t0
    out = capsys.readouterr().out
    assert code == 0
    assert total < 6.0
    assert "Sektion 1/3 'intro'" in out and "Sektion 2/3 'verse'" in out and "Sektion 3/3 'outro'" in out
    assert "Wechsel armiert" in out
    assert "Aufgeräumt in" in out
    cleanup = float(out.rsplit("Aufgeräumt in ", 1)[1].split(" s")[0])
    assert cleanup < 3.0
