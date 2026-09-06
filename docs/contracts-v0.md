# Schnittstellen-Vertrag v0 (M2 ↔ M3)

Verbindlich für die Subagents von M2 (`looper/`-Paket) und M3 (`backend/`, `ui/`). Abweichungen nur nach Rücksprache mit dem Main Agent. Kontext: `docs/handover-tag1.md`.

## Paket-Layout

```
looper/                     # M2 — reine Python-Bibliothek, kein FastAPI, kein UI
  __init__.py
  engine/base.py            # Engine (ABC), EngineError, EngineConnectionError
  engine/ableton_osc.py     # AbletonOscEngine
  engine/sim.py             # SimEngine — simulierter Beat-Clock ohne Ableton (für Tests + UI-Entwicklung)
  score/schema.json         # JSON Schema v0
  score/compile.py          # compile_score(yaml_text: str) -> CompiledScore   (raises ScoreError)
  runner.py                 # Runner
  midi.py                   # MidiInput (mido) → normalisierte MIDI-Events
  __main__.py               # CLI: python -m looper compile <file> | run <file> --engine ableton|sim
examples/example-song.yaml  # M2
backend/app.py              # M3 — FastAPI, importiert aus looper
ui/                         # M3 — React + Vite + CodeMirror 6
spike/                      # M1, bleibt unangetastet
```

## Engine-Interface (looper/engine/base.py)

Track-Indizes 0-basiert (Ableton-Reihenfolge), Scene-Index 0-basiert. Alle Methoden synchron; Callbacks kommen aus einem Hintergrund-Thread der Engine — der Runner serialisiert selbst.

```python
class Engine(ABC):
    def connect(self) -> None: ...            # blockiert max. 2 s; EngineConnectionError mit klarer deutscher Meldung
    def close(self) -> None: ...              # idempotent, max. 2 s
    def get_tempo(self) -> float: ...
    def set_tempo(self, bpm: float) -> None: ...
    def get_beats_per_bar(self) -> int: ...
    def get_track_count(self) -> int: ...                          # Spuren im Set (gültige Indizes 0 .. count-1)
    def get_track_name(self, index: int) -> str | None: ...        # None = unbekannt/keine Antwort (wirft nie)
    def set_quantization_bar(self) -> None: ...   # Global Quantization = 1 Bar
    def start_transport(self) -> None: ...
    def stop_transport(self) -> None: ...
    def arm(self, track: int, on: bool) -> None: ...
    def set_monitoring(self, track: int, mode: str) -> None: ...  # "in" | "auto" | "off"
    def fire_slot(self, track: int, scene: int) -> None: ...      # Record (leer + armed) bzw. Play / Record-Ende → quantisiert durch Ableton
    def slot_has_clip(self, track: int, scene: int) -> bool: ...
    def stop_track(self, track: int) -> None: ...
    def stop_all(self) -> None: ...                                # alle Clips stoppen (Transport läuft weiter)
    def on_beat(self, cb: Callable[[int], None]) -> None: ...      # absoluter Song-Beat (int), 1× pro Beat
    def on_clip_state(self, cb: Callable[[int, int, str], None]) -> None: ...  # (track, scene, "recording"|"playing"|"stopped")
```

`SimEngine` implementiert dasselbe mit einem Thread-Timer bei `bpm` und simuliert: `fire_slot` auf leeren Slot → ab nächster Taktgrenze `recording`; erneutes `fire_slot` → ab nächster Taktgrenze `playing`; Slots merken sich `has_clip`.

## Kompilierte Partitur (Ausgabe von compile_score, JSON-serialisierbar)

```json
{
  "title": "Beispiel",
  "bpm": 100,
  "beats_per_bar": 4,
  "tracks": [
    {"name": "gitarre", "ableton_track": 0, "type": "single"},
    {"name": "voice",   "ableton_track": 1, "type": "single"}
  ],
  "midi": {
    "next_section": {"id": "ch1.cc64"},
    "stop_all":     {"id": "ch1.note37"}
  },
  "sections": [
    {"id": "intro", "index": 0, "bars": 4, "autorelease": false, "quantize": "bar",
     "tracks": {"gitarre": "record", "voice": "stop"}, "source_line": 12}
  ]
}
```

Track-States: `record | overdub | play | stop | hear_through`. `repeat: <id>` ist im kompilierten JSON bereits aufgelöst (Sektion vollständig kopiert, neue `id`/`index`). Jede Sektion enthält **alle** Tracks (fehlende → `stop`, Compiler ergänzt).

`ScoreError.errors` ist eine Liste von `{"line": int|null, "column": int|null, "message": str, "suggestion": str|null}` (deutsch, z. B. „Sektion 'bridge' referenziert Track 'bss' — meintest du 'bass'?").

## YAML v0 (Beispiel — M2 definiert das Schema, M3 nutzt es als Default im Editor)

```yaml
title: Beispiel-Song
bpm: 100
time_signature: 4/4                    # optional, Default 4/4; auch 3/4, 6/8 etc. → beats_per_bar = Zähler
tracks:
  gitarre: {ableton_track: 0}          # type: single (Default) | group
  voice:   {ableton_track: 1}
midi:
  next_section: {cc: 64}               # oder note: 36; channel optional (Default 1)
  stop_all:     {note: 37}
sections:
  - id: intro
    bars: 4
    autorelease: false                 # false = warten auf next_section, dann am Sektionsende wechseln
    quantize: bar                      # bar | loop
    tracks:
      gitarre: record
  - id: verse
    bars: 4
    autorelease: true
    tracks:
      gitarre: play
      voice: record
  - id: outro
    repeat: verse
    tracks:                            # optionale Overrides
      gitarre: overdub
```

## Runner (looper/runner.py)

```python
Runner(engine: Engine, score: CompiledScore, on_event: Callable[[dict], None])
  .start()            # connect, Spurenprüfung, Tempo setzen, Quantisierung, Transport starten, Sektion 0 an nächster Taktgrenze
                      # Spurenprüfung: existiert ein referenzierter ableton_track nicht -> EngineError (deutsch, nennt
                      # alle fehlenden Indizes), nichts wird gestartet; Namensabweichung -> nur "log"-Event level "warn"
  .next_section()     # armiert Wechsel ("pending"); ausgeführt am Ende der Sektion (quantize=loop) bzw. nächsten Takt (quantize=bar)
  .stop_all()
  .goto(section_id)   # v0: darf NotImplementedError werfen
  .state() -> dict    # identisch zum "state"-Event
```

Runner-Events (`on_event(dict)`), alle mit `"type"`:

```json
{"type":"state","running":true,"section_index":1,"section_id":"verse","bars_total":4,"bar":2,"beat":3,
 "pending":"next"|"stop"|null,"tracks":{"gitarre":"play","voice":"record"},"connected":true,"engine":"ableton"|"sim"}
{"type":"beat","song_beat":37,"bar":2,"beat":3}
{"type":"midi","device":"MPD218","channel":1,"kind":"note_on"|"note_off"|"cc","number":36,"value":127,"id":"ch1.note36"}
{"type":"log","level":"info"|"warn"|"error","message":"…"}
```

`state` wird bei jeder Änderung komplett gesendet (Snapshot, keine Deltas). `bar`/`beat` sind 1-basiert innerhalb der Sektion. **Taktart ist variabel**: `beats_per_bar` kommt aus dem Score (`time_signature`), der Runner setzt es in der Engine (Ableton: `signature_numerator`/`signature_denominator`) und zählt damit; das UI rendert Beat-Punkte/Zähler dynamisch nach `beats_per_bar` aus dem Score (nicht fest 4). `state` enthält zusätzlich `"beats_per_bar": 3` und `"time_signature": "3/4"`. MIDI-`id`-Format: `ch<channel>.note<n>` bzw. `ch<channel>.cc<n>` — identisch zum Mapping in der Partitur.

## Backend-API (M3, FastAPI auf 127.0.0.1:8000)

```
POST /api/compile     {yaml}  → 200 {ok:true, score:{…}} | 200 {ok:false, errors:[…]}
POST /api/load        {yaml}  → kompiliert + lädt in den Runner (ersetzt Score; nur wenn nicht running)
POST /api/transport/start | /stop_all | /next
                              → 400 {detail} bei EngineError (z. B. fehlende Ableton-Spur; Runner bleibt idle,
                                zusätzlich ein log-Event level "error"), 503 wenn die Engine nicht erreichbar ist
GET  /api/state               → aktuelles state-Event
GET  /api/engines             → {"current":"sim"|"ableton"}   POST /api/engines {"engine":"sim"|"ableton"}
WS   /ws                      → sendet alle Runner-Events + MIDI-Events als JSON, beim Connect zuerst ein state
```

Start: `.venv\Scripts\python.exe -m uvicorn backend.app:app --port 8000`; UI: `cd ui && npm run dev` (Vite-Proxy `/api` und `/ws` → 8000).
