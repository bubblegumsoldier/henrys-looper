"""YAML score v0 -> validated, fully resolved CompiledScore (contract v0).

Pipeline: YAML (ruamel, keeps line/column marks) -> JSON Schema (schema.json) ->
semantic checks (track/section references with typo suggestions) -> resolution
(`repeat:` copied, missing tracks -> "stop"). All errors are collected and raised
together as ScoreError with German, line-numbered messages.
"""

from __future__ import annotations

import difflib
import json
import re
from dataclasses import asdict, dataclass, field
from pathlib import Path
from typing import Any

import jsonschema
from ruamel.yaml import YAML
from ruamel.yaml.comments import CommentedMap, CommentedSeq
from ruamel.yaml.error import MarkedYAMLError

SCHEMA_PATH = Path(__file__).with_name("schema.json")
TRACK_STATES = ("record", "overdub", "play", "stop", "hear_through")
MIDI_ACTIONS = ("next_section", "stop_all")
DEFAULT_QUANTIZE = "loop"
DEFAULT_AUTORELEASE = False
DEFAULT_BEATS_PER_BAR = 4
DEFAULT_TIME_SIGNATURE = "4/4"
DEFAULT_GROUP_LAYERS = 4      # minimum number of layer child tracks a group track needs
DEFAULT_GROUP_RESERVE = 2     # spare layer tracks kept free on top of `layers`
GROUP_ONLY_KEYS = ("layers", "reserve", "monitor")
TIME_SIGNATURE_DENOMINATORS = (1, 2, 4, 8, 16, 32)
SECTION_KEYS = ("id", "repeat", "bars", "autorelease", "quantize", "tracks")

_TYPE_NAMES = {
    "object": "ein Mapping (Schlüssel: Wert)",
    "array": "eine Liste",
    "string": "Text",
    "integer": "eine ganze Zahl",
    "number": "eine Zahl",
    "boolean": "true oder false",
    "null": "null",
}


# --------------------------------------------------------------------------- #
# Data model
# --------------------------------------------------------------------------- #
@dataclass
class Track:
    """A logical track of the score.

    Two kinds, distinguished by which Ableton object they point at:
      * ``single`` - one Ableton track, addressed by its 0-based ``ableton_track`` index;
      * ``group``  - an Ableton *group* track addressed by its ``group`` name, holding one
        child track per overdub layer (``<group> L1``, ``L2``, ...) plus an optional live
        monitor child (``<group> LIVE``). ``layers``/``reserve``/``monitor`` size that pool.

    ``type`` stays the discriminator of contract v0; ``is_group`` is what code should test,
    because a legacy ``type: group`` without a ``group:`` name has no group semantics.
    """

    name: str
    ableton_track: int | None = None
    type: str = "single"
    group: str | None = None
    layers: int | None = None
    reserve: int | None = None
    monitor: bool | None = None

    @property
    def is_group(self) -> bool:
        return self.group is not None

    @property
    def pool_size(self) -> int:
        """Layer child tracks the group needs: `layers` minimum plus `reserve` always-free ones."""
        return int(self.layers or DEFAULT_GROUP_LAYERS) + int(
            DEFAULT_GROUP_RESERVE if self.reserve is None else self.reserve)

    def to_dict(self) -> dict[str, Any]:
        if self.is_group:
            return {"name": self.name, "type": "group", "group": self.group,
                    "layers": self.layers, "reserve": self.reserve, "monitor": self.monitor}
        return {"name": self.name, "ableton_track": self.ableton_track, "type": self.type}


@dataclass
class Section:
    id: str
    index: int
    bars: int
    autorelease: bool
    quantize: str
    tracks: dict[str, str]
    source_line: int | None = None


@dataclass
class CompiledScore:
    title: str
    bpm: float
    beats_per_bar: int
    tracks: list[Track]
    midi: dict[str, dict[str, Any]]
    sections: list[Section]
    time_signature: str = DEFAULT_TIME_SIGNATURE

    @property
    def time_signature_parts(self) -> tuple[int, int]:
        """(numerator, denominator) of `time_signature`."""
        num, den = self.time_signature.split("/")
        return int(num), int(den)

    def to_dict(self) -> dict[str, Any]:
        return {
            "title": self.title,
            "bpm": self.bpm,
            "beats_per_bar": self.beats_per_bar,
            "time_signature": self.time_signature,
            "tracks": [t.to_dict() for t in self.tracks],
            "midi": {k: dict(v) for k, v in self.midi.items()},
            "sections": [asdict(s) for s in self.sections],
        }

    def to_json(self, indent: int | None = 2) -> str:
        return json.dumps(self.to_dict(), indent=indent, ensure_ascii=False)

    def track(self, name: str) -> Track:
        for t in self.tracks:
            if t.name == name:
                return t
        raise KeyError(name)

    def section_index(self, section_id: str) -> int:
        for s in self.sections:
            if s.id == section_id:
                return s.index
        raise KeyError(section_id)


@dataclass
class ScoreIssue:
    line: int | None
    column: int | None
    message: str
    suggestion: str | None = None

    def to_dict(self) -> dict[str, Any]:
        return {"line": self.line, "column": self.column, "message": self.message,
                "suggestion": self.suggestion}

    def format(self) -> str:
        where = f"Zeile {self.line}" if self.line is not None else "Partitur"
        if self.line is not None and self.column is not None:
            where += f", Spalte {self.column}"
        text = f"{where}: {self.message}"
        if self.suggestion:
            text += f" — {self.suggestion}"
        return text


class ScoreError(ValueError):
    """Raised by compile_score; `errors` is a list of dicts {line, column, message, suggestion}."""

    def __init__(self, issues: list[ScoreIssue]):
        self.issues = sorted(issues, key=lambda i: (i.line is None, i.line or 0, i.column or 0))
        self.errors = [i.to_dict() for i in self.issues]
        super().__init__("\n".join(i.format() for i in self.issues))


# --------------------------------------------------------------------------- #
# Helpers
# --------------------------------------------------------------------------- #
def _load_schema() -> dict:
    with SCHEMA_PATH.open(encoding="utf-8") as fh:
        return json.load(fh)


_SCHEMA = _load_schema()
_VALIDATOR = jsonschema.Draft202012Validator(_SCHEMA)


def _suggest(word: str, candidates, cutoff: float = 0.5) -> str | None:
    if not isinstance(word, str):
        return None
    matches = difflib.get_close_matches(word, [c for c in candidates if isinstance(c, str)],
                                        n=1, cutoff=cutoff)
    return f"meintest du '{matches[0]}'?" if matches else None


def _pos(node, key=None) -> tuple[int | None, int | None]:
    """1-based (line, column) of `key` inside `node`, or of `node` itself."""
    try:
        if key is None:
            line, col = node.lc.line, node.lc.col
        elif isinstance(node, CommentedMap):
            line, col = node.lc.key(key)
        elif isinstance(node, CommentedSeq):
            line, col = node.lc.item(key)
        else:
            return None, None
        return line + 1, col + 1
    except (AttributeError, KeyError, IndexError, TypeError):
        return None, None


def _walk(root, path):
    node = root
    for part in path:
        try:
            node = node[part]
        except (KeyError, IndexError, TypeError):
            return None
    return node


def _pos_for_path(root, path) -> tuple[int | None, int | None]:
    """Position of the element addressed by `path`: the key line for mapping entries."""
    path = list(path)
    if not path:
        return _pos(root)
    parent = _walk(root, path[:-1])
    if parent is None:
        return None, None
    return _pos(parent, path[-1])


def _join_path(path) -> str:
    out = ""
    for part in path:
        out += f"[{part}]" if isinstance(part, int) else (f".{part}" if out else str(part))
    return out or "(Wurzel)"


# --------------------------------------------------------------------------- #
# Schema error translation
# --------------------------------------------------------------------------- #
def _translate_schema_error(err: jsonschema.ValidationError, root) -> ScoreIssue:
    path = list(err.absolute_path)
    line, col = _pos_for_path(root, path)
    field_name = path[-1] if path else None
    where = _join_path(path)
    v = err.validator
    suggestion = None

    if v == "required":
        missing = re.findall(r"'([^']+)' is a required property", err.message)
        name = missing[0] if missing else "?"
        ctx = f" in {where}" if path else ""
        msg = f"Pflichtfeld '{name}' fehlt{ctx}."
        if name == "bars":
            suggestion = "Sektionen brauchen 'bars: <Anzahl Takte>' (oder 'repeat: <Sektion>')."
    elif v == "additionalProperties":
        unexpected = re.findall(r"'([^']+)'", err.message)
        name = unexpected[0] if unexpected else "?"
        known = list((err.schema or {}).get("properties", {}).keys())
        line, col = _pos(err.instance, name) if isinstance(err.instance, CommentedMap) else (line, col)
        msg = f"Unbekanntes Feld '{name}' in {where}."
        suggestion = _suggest(name, known) or (f"Erlaubt: {', '.join(known)}." if known else None)
    elif v == "type":
        expected = err.validator_value
        expected = [expected] if isinstance(expected, str) else list(expected)
        exp = " oder ".join(_TYPE_NAMES.get(e, e) for e in expected)
        msg = f"'{where}' muss {exp} sein, gefunden: {_short(err.instance)}."
        if expected == ["boolean"] and isinstance(err.instance, str):
            suggestion = "Schreibe true oder false (ohne Anführungszeichen)."
        if expected == ["object"] and field_name == "tracks" and isinstance(err.instance, list):
            suggestion = "Tracks als Mapping angeben, z. B. 'gitarre: {ableton_track: 0}'."
    elif v == "enum":
        allowed = list(err.validator_value)
        msg = f"Ungültiger Wert {_short(err.instance)} für '{where}'. Erlaubt: {', '.join(map(str, allowed))}."
        suggestion = _suggest(err.instance, allowed)
    elif v in ("minimum", "maximum"):
        op = ">=" if v == "minimum" else "<="
        msg = f"'{where}' muss {op} {err.validator_value} sein, gefunden: {_short(err.instance)}."
    elif v == "minItems":
        msg = f"'{where}' braucht mindestens {err.validator_value} Eintrag/Einträge."
    elif v == "minProperties":
        msg = f"'{where}' braucht mindestens {err.validator_value} Eintrag/Einträge."
    elif v == "pattern" and field_name == "time_signature":
        msg = f"Ungültige Taktart {_short(err.instance)}."
        suggestion = "Schreibe 'Zähler/Nenner', z. B. time_signature: 3/4 oder \"6/8\"."
    elif v == "pattern":
        msg = f"Ungültiger Bezeichner {_short(err.instance)} in '{where}'."
        suggestion = "Erlaubt sind Buchstaben, Ziffern, '_' und '-', beginnend mit einem Buchstaben."
    elif v == "propertyNames":
        msg = f"Ungültiger Name {_short(err.instance)} in '{where}'."
        suggestion = "Erlaubt sind Buchstaben, Ziffern, '_' und '-', beginnend mit einem Buchstaben."
    elif v in ("oneOf", "anyOf", "not"):
        if path and path[0] == "midi":
            msg = f"MIDI-Bindung '{where}' braucht genau eines von 'cc' oder 'note' (Channel optional)."
        else:
            msg = f"'{where}' ist ungültig: {err.message}"
    else:
        msg = f"'{where}': {err.message}"
    return ScoreIssue(line, col, msg, suggestion)


def _short(value, limit: int = 40) -> str:
    text = repr(value) if not isinstance(value, (CommentedMap, CommentedSeq)) else \
        ("ein Mapping" if isinstance(value, CommentedMap) else "eine Liste")
    return text if len(text) <= limit else text[: limit - 1] + "…"


def _int_or(value, default: int) -> int:
    """Schema already reported wrong types; fall back to the default so one error stays one error."""
    return int(value) if isinstance(value, int) and not isinstance(value, bool) else default


def _parse_track(name: str, spec: CommentedMap, tracks_node: CommentedMap,
                 issues: list[ScoreIssue], seen_idx: dict[int, str],
                 seen_group: dict[str, str]) -> Track | None:
    """One entry of the `tracks:` mapping -> Track, or None if it was rejected (issue appended)."""
    raw_idx = spec.get("ableton_track")
    has_index = isinstance(raw_idx, int) and not isinstance(raw_idx, bool)
    raw_group = spec.get("group")
    has_group = isinstance(raw_group, str) and raw_group.strip() != ""

    if has_group and "ableton_track" in spec:
        line, col = _pos(spec, "group")
        issues.append(ScoreIssue(line, col,
                                 f"Track '{name}': 'group' und 'ableton_track' schließen sich aus.",
                                 "Ein Gruppen-Track nennt nur die Ableton-Gruppenspur "
                                 "(group: <Name>), eine Einzelspur nur ihren Index "
                                 "(ableton_track: <Nummer>)."))
        return None

    if has_group:
        group = raw_group.strip()
        key = group.casefold()
        if key in seen_group:
            line, col = _pos(spec, "group")
            issues.append(ScoreIssue(line, col,
                                     f"Track '{name}' nutzt die Gruppe '{group}', die bereits "
                                     f"'{seen_group[key]}' belegt.",
                                     "Jede Ableton-Gruppenspur darf nur einem Partitur-Track "
                                     "zugeordnet sein."))
            return None
        seen_group[key] = name
        layers = _int_or(spec.get("layers"), DEFAULT_GROUP_LAYERS)
        reserve = _int_or(spec.get("reserve"), DEFAULT_GROUP_RESERVE)
        monitor = spec["monitor"] if isinstance(spec.get("monitor"), bool) else True
        if isinstance(spec.get("type"), str) and spec["type"] != "group":
            line, col = _pos(spec, "type")
            issues.append(ScoreIssue(line, col,
                                     f"Track '{name}': 'group' bedeutet type 'group', "
                                     f"nicht '{spec['type']}'.",
                                     "'type' bei einem Gruppen-Track weglassen."))
            return None
        return Track(name, None, "group", group, layers, reserve, bool(monitor))

    # single track
    for key in GROUP_ONLY_KEYS:
        if key in spec:
            line, col = _pos(spec, key)
            issues.append(ScoreIssue(line, col,
                                     f"Track '{name}': '{key}' gilt nur für Gruppen-Tracks.",
                                     "Ergänze 'group: <Name der Ableton-Gruppenspur>' oder "
                                     f"entferne '{key}'."))
            return None
    if not has_index:
        if "ableton_track" in spec:
            return None                # wrong type - already reported by the schema
        line, col = _pos(tracks_node, name)
        issues.append(ScoreIssue(line, col,
                                 f"Track '{name}' hat weder 'ableton_track' noch 'group'.",
                                 "Einzelspur: 'ableton_track: <0-basierter Index>'. "
                                 "Gruppen-Track: 'group: <Name der Ableton-Gruppenspur>'."))
        return None
    idx = int(raw_idx)
    if idx in seen_idx:
        line, col = _pos(spec, "ableton_track")
        issues.append(ScoreIssue(line, col,
                                 f"Track '{name}' nutzt ableton_track {idx}, den bereits "
                                 f"'{seen_idx[idx]}' belegt.",
                                 "Jeder Ableton-Track darf nur einem Partitur-Track zugeordnet sein."))
        return None
    seen_idx[idx] = name
    return Track(name, idx, str(spec.get("type", "single")))


# --------------------------------------------------------------------------- #
# Compiler
# --------------------------------------------------------------------------- #
def parse_yaml(yaml_text: str):
    """Parse YAML with position info; raises ScoreError on syntax errors."""
    yaml = YAML(typ="rt")
    yaml.allow_duplicate_keys = False
    try:
        data = yaml.load(yaml_text)
    except MarkedYAMLError as exc:
        mark = exc.problem_mark or exc.context_mark
        line = mark.line + 1 if mark else None
        col = mark.column + 1 if mark else None
        problem = (exc.problem or str(exc)).strip()
        suggestion = None
        if "duplicate key" in problem:
            key = re.findall(r'duplicate key "([^"]+)"', problem)
            msg = f"Doppelter Schlüssel '{key[0] if key else '?'}'." if key else "Doppelter Schlüssel."
            suggestion = "Jeder Schlüssel darf pro Mapping nur einmal vorkommen."
        elif "\\t" in problem or "tab" in problem.lower():
            msg = f"YAML-Syntaxfehler: {problem}"
            suggestion = "Zum Einrücken Leerzeichen statt Tabs verwenden."
        else:
            msg = f"YAML-Syntaxfehler: {problem}"
            if exc.context:
                msg += f" ({exc.context.strip()})"
            suggestion = "Einrückung und Doppelpunkte prüfen (Wert nach 'schlüssel: ')."
        raise ScoreError([ScoreIssue(line, col, msg, suggestion)]) from None
    except Exception as exc:  # ruamel raises a few non-marked errors (e.g. composer errors)
        raise ScoreError([ScoreIssue(None, None, f"YAML konnte nicht gelesen werden: {exc}", None)]) from None
    return data


def compile_score(yaml_text: str) -> CompiledScore:
    data = parse_yaml(yaml_text)
    issues: list[ScoreIssue] = []

    if not isinstance(data, CommentedMap):
        raise ScoreError([ScoreIssue(1, 1, "Die Partitur muss ein Mapping mit 'bpm', 'tracks' und "
                                     "'sections' sein.", "Beispiel: examples/example-song.yaml")])

    # 1) schema (an enum error on a field that already has a type error is just noise)
    schema_errors = list(_VALIDATOR.iter_errors(data))
    type_error_paths = {tuple(e.absolute_path) for e in schema_errors if e.validator == "type"}
    for err in schema_errors:
        if err.validator == "enum" and tuple(err.absolute_path) in type_error_paths:
            continue
        issues.append(_translate_schema_error(err, data))

    # 2) semantic checks + resolution (defensive: skip malformed parts already reported)
    tracks_node = data.get("tracks")
    track_names: list[str] = []
    tracks: list[Track] = []
    if isinstance(tracks_node, CommentedMap):
        seen_idx: dict[int, str] = {}
        seen_group: dict[str, str] = {}
        for name, spec in tracks_node.items():
            if not isinstance(spec, CommentedMap):
                continue
            track = _parse_track(str(name), spec, tracks_node, issues, seen_idx, seen_group)
            if track is not None:
                track_names.append(track.name)
                tracks.append(track)

    midi: dict[str, dict[str, Any]] = {}
    midi_node = data.get("midi")
    if isinstance(midi_node, CommentedMap):
        for action, binding in midi_node.items():
            if action not in MIDI_ACTIONS or not isinstance(binding, CommentedMap):
                continue
            channel = binding.get("channel", 1)
            if "cc" in binding and isinstance(binding["cc"], int):
                midi[str(action)] = {"id": f"ch{channel}.cc{binding['cc']}"}
            elif "note" in binding and isinstance(binding["note"], int):
                midi[str(action)] = {"id": f"ch{channel}.note{binding['note']}"}

    sections_node = data.get("sections")
    sections: list[Section] = []
    if isinstance(sections_node, CommentedSeq):
        raw: list[tuple[int, CommentedMap]] = [(i, s) for i, s in enumerate(sections_node)
                                               if isinstance(s, CommentedMap)]
        ids: dict[str, int] = {}
        for i, sec in raw:
            sid = sec.get("id")
            if not isinstance(sid, str):
                continue
            if sid in ids:
                line, col = _pos(sec, "id")
                issues.append(ScoreIssue(line, col, f"Sektions-ID '{sid}' ist doppelt vergeben "
                                         f"(bereits Sektion {ids[sid] + 1}).",
                                         "IDs müssen eindeutig sein; 'repeat: <id>' nutzt die erste Definition."))
                continue
            ids[sid] = i

        by_id: dict[str, CommentedMap] = {s.get("id"): s for _, s in raw if isinstance(s.get("id"), str)}
        resolved_cache: dict[str, dict] = {}

        def resolve(sec: CommentedMap, chain: tuple[str, ...]) -> dict | None:
            """Return {'bars','autorelease','quantize','tracks'} for `sec`, or None on error."""
            sid = sec.get("id")
            if isinstance(sid, str) and sid in resolved_cache:
                return resolved_cache[sid]
            base: dict = {}
            target_id = sec.get("repeat")
            if target_id is not None:
                if not isinstance(target_id, str) or target_id not in by_id:
                    line, col = _pos(sec, "repeat")
                    issues.append(ScoreIssue(line, col,
                                             f"Sektion '{sid}' wiederholt unbekannte Sektion '{target_id}'.",
                                             _suggest(target_id, list(by_id.keys()))
                                             or f"Bekannte Sektionen: {', '.join(by_id) or '-'}."))
                    return None
                if target_id == sid or target_id in chain:
                    line, col = _pos(sec, "repeat")
                    issues.append(ScoreIssue(line, col,
                                             f"Sektion '{sid}': 'repeat: {target_id}' ist zirkulär.",
                                             "Eine Sektion kann sich nicht selbst (auch nicht indirekt) wiederholen."))
                    return None
                target = resolve(by_id[target_id], chain + (str(sid),))
                if target is None:
                    return None
                base = {"bars": target["bars"], "autorelease": target["autorelease"],
                        "quantize": target["quantize"], "tracks": dict(target["tracks"])}
            else:
                bars = sec.get("bars")
                if not isinstance(bars, int) or isinstance(bars, bool):
                    if bars is None:
                        line, col = _pos(sec, "id")
                        issues.append(ScoreIssue(line, col, f"Sektion '{sid}' hat weder 'bars' noch 'repeat'.",
                                                 "Gib 'bars: <Anzahl Takte>' an oder wiederhole eine Sektion "
                                                 "mit 'repeat: <id>'."))
                    return None  # type errors were already reported by the schema
                base = {"bars": bars, "autorelease": DEFAULT_AUTORELEASE,
                        "quantize": DEFAULT_QUANTIZE, "tracks": {}}
            # overrides
            if isinstance(sec.get("bars"), int) and not isinstance(sec.get("bars"), bool):
                base["bars"] = int(sec["bars"])
            if isinstance(sec.get("autorelease"), bool):
                base["autorelease"] = bool(sec["autorelease"])
            if isinstance(sec.get("quantize"), str):
                base["quantize"] = str(sec["quantize"])
            tnode = sec.get("tracks")
            if isinstance(tnode, CommentedMap):
                for tname, state in tnode.items():
                    if tname not in track_names:
                        line, col = _pos(tnode, tname)
                        issues.append(ScoreIssue(line, col,
                                                 f"Sektion '{sid}' referenziert Track '{tname}'.",
                                                 _suggest(tname, track_names)
                                                 or f"Bekannte Tracks: {', '.join(track_names) or '-'}."))
                        continue
                    if isinstance(state, str) and state in TRACK_STATES:
                        base["tracks"][str(tname)] = state
            if isinstance(sid, str):
                resolved_cache[sid] = base
            return base

        for i, sec in raw:
            sid = sec.get("id")
            if not isinstance(sid, str) or ids.get(sid) != i:
                continue  # invalid or duplicate id (reported)
            res = resolve(sec, ())
            if res is None:
                continue
            full_tracks = {name: res["tracks"].get(name, "stop") for name in track_names}
            line, _ = _pos(sections_node, i)
            sections.append(Section(id=sid, index=len(sections), bars=int(res["bars"]),
                                    autorelease=bool(res["autorelease"]), quantize=str(res["quantize"]),
                                    tracks=full_tracks, source_line=line))

    beats_per_bar, time_signature = _resolve_time_signature(data, issues)

    if issues:
        raise ScoreError(_dedupe(issues))

    bpm = data.get("bpm")
    return CompiledScore(
        title=str(data.get("title", "")),
        bpm=float(bpm) if isinstance(bpm, float) and not bpm.is_integer() else int(bpm),
        beats_per_bar=beats_per_bar,
        tracks=tracks,
        midi=midi,
        sections=sections,
        time_signature=time_signature,
    )


def _resolve_time_signature(data: CommentedMap, issues: list[ScoreIssue]) -> tuple[int, str]:
    """`time_signature: "3/4"` (preferred) and/or alias `beats_per_bar: 3` -> (beats_per_bar, "3/4").

    Both given and contradicting -> error at the `beats_per_bar` line. Type/pattern errors were
    already reported by the schema; malformed values fall back to the default here.
    """
    ts_raw = data.get("time_signature")
    bpb_raw = data.get("beats_per_bar")
    bpb_ok = isinstance(bpb_raw, int) and not isinstance(bpb_raw, bool) and bpb_raw >= 1
    numerator: int | None = None
    denominator = 4
    if isinstance(ts_raw, str) and re.fullmatch(r"[0-9]{1,2}/[0-9]{1,2}", ts_raw):
        num, den = (int(x) for x in ts_raw.split("/"))
        line, col = _pos(data, "time_signature")
        if not 1 <= num <= 16:
            issues.append(ScoreIssue(line, col, f"Taktart '{ts_raw}': Zähler muss zwischen 1 und 16 liegen.",
                                     "z. B. 3/4, 4/4, 6/8, 7/8."))
        elif den not in TIME_SIGNATURE_DENOMINATORS:
            issues.append(ScoreIssue(line, col, f"Taktart '{ts_raw}': Nenner muss eine Zweierpotenz sein "
                                     f"({', '.join(map(str, TIME_SIGNATURE_DENOMINATORS))}).",
                                     "z. B. 3/4, 6/8, 12/16."))
        else:
            numerator, denominator = num, den
            if bpb_ok and int(bpb_raw) != num:
                line, col = _pos(data, "beats_per_bar")
                issues.append(ScoreIssue(line, col,
                                         f"'beats_per_bar: {bpb_raw}' widerspricht 'time_signature: {ts_raw}'.",
                                         "Nur eines von beiden angeben (bevorzugt time_signature) oder "
                                         "beats_per_bar = Zähler der Taktart setzen."))
    elif bpb_ok:
        numerator = int(bpb_raw)
    if numerator is None:
        numerator = DEFAULT_BEATS_PER_BAR
    return numerator, f"{numerator}/{denominator}"


def _dedupe(issues: list[ScoreIssue]) -> list[ScoreIssue]:
    seen = set()
    out = []
    for issue in issues:
        key = (issue.line, issue.column, issue.message)
        if key not in seen:
            seen.add(key)
            out.append(issue)
    return out
