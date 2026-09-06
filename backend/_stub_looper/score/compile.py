"""Minimal YAML -> compiled-score compiler following the v0 contract shape.

Only enough validation to exercise the UI's error paths: syntax errors, missing
keys, unknown tracks (with did-you-mean suggestions), unknown repeat targets,
invalid track states, invalid bars, duplicate section ids.
"""
from __future__ import annotations

import difflib
from typing import Any

import yaml

TRACK_STATES = ("record", "overdub", "play", "stop", "hear_through")
QUANTIZE = ("bar", "loop")


class ScoreError(Exception):
    def __init__(self, errors: list[dict]) -> None:
        self.errors = errors
        super().__init__("; ".join(e["message"] for e in errors))


def _err(line: int | None, message: str, suggestion: str | None = None, column: int | None = None) -> dict:
    return {"line": line, "column": column, "message": message, "suggestion": suggestion}


def _suggest(name: str, candidates: list[str]) -> str | None:
    m = difflib.get_close_matches(name, candidates, n=1, cutoff=0.5)
    return f"Meintest du '{m[0]}'?" if m else None


# --- YAML with line numbers ------------------------------------------------

class _Loc(dict):
    """dict that remembers the source line (1-based) of each key."""

    def __init__(self) -> None:
        super().__init__()
        self.lines: dict[str, int] = {}
        self.line: int = 0


def _build(node: yaml.Node) -> Any:
    if isinstance(node, yaml.MappingNode):
        out = _Loc()
        out.line = node.start_mark.line + 1
        for k, v in node.value:
            key = _build(k)
            out[key] = _build(v)
            out.lines[str(key)] = k.start_mark.line + 1
        return out
    if isinstance(node, yaml.SequenceNode):
        return [_build(v) for v in node.value]
    return _scalar(node)


def _scalar(node: yaml.ScalarNode) -> Any:
    loader = yaml.SafeLoader("")
    return loader.construct_object(node, deep=True)


def _line(obj: Any, key: str | None = None) -> int | None:
    if isinstance(obj, _Loc):
        if key is not None:
            return obj.lines.get(key, obj.line)
        return obj.line
    return None


# --- compiler --------------------------------------------------------------

def compile_score(yaml_text: str) -> dict:
    try:
        node = yaml.compose(yaml_text, Loader=yaml.SafeLoader)
    except yaml.MarkedYAMLError as e:  # syntax error with position
        mark = e.problem_mark
        line = (mark.line + 1) if mark else None
        col = (mark.column + 1) if mark else None
        raise ScoreError([_err(line, f"YAML-Syntaxfehler: {e.problem or e}", "Einrückung und Doppelpunkte prüfen.", col)])
    except yaml.YAMLError as e:
        raise ScoreError([_err(None, f"YAML-Fehler: {e}")])

    if node is None:
        raise ScoreError([_err(1, "Die Partitur ist leer.", "Mindestens 'title', 'bpm', 'tracks' und 'sections' angeben.")])
    doc = _build(node)
    if not isinstance(doc, dict):
        raise ScoreError([_err(1, "Die Partitur muss ein YAML-Mapping (Schlüssel: Wert) sein.")])

    errors: list[dict] = []

    title = doc.get("title")
    if not isinstance(title, str) or not title.strip():
        errors.append(_err(_line(doc, "title") or 1, "Feld 'title' fehlt oder ist leer.", "z. B. title: Mein Song"))

    bpm = doc.get("bpm")
    if not isinstance(bpm, (int, float)) or isinstance(bpm, bool) or not (20 <= bpm <= 300):
        errors.append(_err(_line(doc, "bpm") or 1, "Feld 'bpm' fehlt oder ist keine Zahl zwischen 20 und 300.", "z. B. bpm: 100"))

    beats_per_bar = doc.get("beats_per_bar", 4)
    if not isinstance(beats_per_bar, int) or not (1 <= beats_per_bar <= 16):
        errors.append(_err(_line(doc, "beats_per_bar"), "'beats_per_bar' muss eine ganze Zahl zwischen 1 und 16 sein."))
        beats_per_bar = 4

    # tracks
    tracks_raw = doc.get("tracks")
    tracks: list[dict] = []
    track_names: list[str] = []
    if not isinstance(tracks_raw, dict) or not tracks_raw:
        errors.append(_err(_line(doc, "tracks") or 1, "Feld 'tracks' fehlt oder ist leer.", "z. B. tracks: {gitarre: {ableton_track: 0}}"))
    else:
        used_idx: dict[int, str] = {}
        for name, spec in tracks_raw.items():
            ln = _line(tracks_raw, str(name))
            if not isinstance(spec, dict) or "ableton_track" not in spec:
                errors.append(_err(ln, f"Track '{name}' braucht 'ableton_track'.", f"{name}: {{ableton_track: 0}}"))
                continue
            idx = spec.get("ableton_track")
            if not isinstance(idx, int) or isinstance(idx, bool) or idx < 0:
                errors.append(_err(_line(spec, "ableton_track") or ln, f"Track '{name}': 'ableton_track' muss eine ganze Zahl >= 0 sein."))
                continue
            if idx in used_idx:
                errors.append(_err(ln, f"Track '{name}' nutzt ableton_track {idx}, den bereits '{used_idx[idx]}' belegt."))
            used_idx[idx] = str(name)
            ttype = spec.get("type", "single")
            if ttype not in ("single", "group"):
                errors.append(_err(_line(spec, "type") or ln, f"Track '{name}': 'type' muss 'single' oder 'group' sein."))
                ttype = "single"
            tracks.append({"name": str(name), "ableton_track": idx, "type": ttype})
            track_names.append(str(name))

    # midi mapping
    midi_out: dict[str, dict] = {}
    midi_raw = doc.get("midi") or {}
    if midi_raw and not isinstance(midi_raw, dict):
        errors.append(_err(_line(doc, "midi"), "'midi' muss ein Mapping sein."))
        midi_raw = {}
    for action, spec in midi_raw.items():
        ln = _line(midi_raw, str(action))
        if action not in ("next_section", "stop_all"):
            errors.append(_err(ln, f"Unbekannte MIDI-Aktion '{action}'.", _suggest(str(action), ["next_section", "stop_all"])))
            continue
        if not isinstance(spec, dict):
            errors.append(_err(ln, f"MIDI-Aktion '{action}' braucht 'cc: <n>' oder 'note: <n>'."))
            continue
        ch = spec.get("channel", 1)
        if "cc" in spec:
            midi_out[action] = {"id": f"ch{ch}.cc{spec['cc']}"}
        elif "note" in spec:
            midi_out[action] = {"id": f"ch{ch}.note{spec['note']}"}
        else:
            errors.append(_err(ln, f"MIDI-Aktion '{action}' braucht 'cc' oder 'note'."))

    # sections
    sections_raw = doc.get("sections")
    sections: list[dict] = []
    if not isinstance(sections_raw, list) or not sections_raw:
        errors.append(_err(_line(doc, "sections") or 1, "Feld 'sections' fehlt oder ist leer.", "Mindestens eine Sektion mit 'id' und 'bars' angeben."))
        sections_raw = []

    by_id: dict[str, dict] = {}
    for i, sec in enumerate(sections_raw):
        ln = _line(sec) if isinstance(sec, dict) else None
        if not isinstance(sec, dict):
            errors.append(_err(ln, f"Sektion #{i + 1} muss ein Mapping sein."))
            continue
        sid = sec.get("id")
        if not isinstance(sid, str) or not sid:
            errors.append(_err(ln, f"Sektion #{i + 1} hat keine 'id'."))
            sid = f"section{i + 1}"
        if sid in by_id:
            errors.append(_err(_line(sec, "id") or ln, f"Sektion-ID '{sid}' ist doppelt vergeben."))

        base: dict | None = None
        if "repeat" in sec:
            ref = sec["repeat"]
            if ref not in by_id:
                errors.append(_err(_line(sec, "repeat") or ln, f"Sektion '{sid}' wiederholt unbekannte Sektion '{ref}'.",
                                   _suggest(str(ref), list(by_id.keys()))))
            else:
                base = by_id[ref]

        bars = sec.get("bars", base["bars"] if base else None)
        if not isinstance(bars, int) or isinstance(bars, bool) or bars < 1:
            errors.append(_err(_line(sec, "bars") or ln, f"Sektion '{sid}': 'bars' muss eine ganze Zahl >= 1 sein.", "z. B. bars: 4"))
            bars = 1
        autorelease = sec.get("autorelease", base["autorelease"] if base else False)
        if not isinstance(autorelease, bool):
            errors.append(_err(_line(sec, "autorelease") or ln, f"Sektion '{sid}': 'autorelease' muss true oder false sein."))
            autorelease = False
        quantize = sec.get("quantize", base["quantize"] if base else "bar")
        if quantize not in QUANTIZE:
            errors.append(_err(_line(sec, "quantize") or ln, f"Sektion '{sid}': 'quantize' muss 'bar' oder 'loop' sein.", _suggest(str(quantize), list(QUANTIZE))))
            quantize = "bar"

        states: dict[str, str] = dict(base["tracks"]) if base else {name: "stop" for name in track_names}
        tr = sec.get("tracks", {})
        if tr is None:
            tr = {}
        if not isinstance(tr, dict):
            errors.append(_err(_line(sec, "tracks") or ln, f"Sektion '{sid}': 'tracks' muss ein Mapping sein."))
            tr = {}
        for tname, st in tr.items():
            tln = _line(tr, str(tname)) or ln
            if str(tname) not in track_names:
                errors.append(_err(tln, f"Sektion '{sid}' referenziert unbekannten Track '{tname}'.", _suggest(str(tname), track_names)))
                continue
            if st not in TRACK_STATES:
                errors.append(_err(tln, f"Sektion '{sid}', Track '{tname}': unbekannter Zustand '{st}'.",
                                   _suggest(str(st), list(TRACK_STATES)) or "Erlaubt: " + ", ".join(TRACK_STATES)))
                continue
            states[str(tname)] = st

        compiled = {
            "id": sid,
            "index": len(sections),
            "bars": bars,
            "autorelease": autorelease,
            "quantize": quantize,
            "tracks": states,
            "source_line": ln,
        }
        by_id[sid] = compiled
        sections.append(compiled)

    if errors:
        raise ScoreError(errors)

    return {
        "title": title,
        "bpm": bpm,
        "beats_per_bar": beats_per_bar,
        "tracks": tracks,
        "midi": midi_out,
        "sections": sections,
    }
