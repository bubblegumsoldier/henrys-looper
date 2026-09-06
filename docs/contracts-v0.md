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
  session.py                # Gruppen-Layer (M4): ensure_pool / resolve_pools / add_layer
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

### Struktur-Methoden (M4, Gruppen-Layer)

Optionaler Teil des Interfaces: `Engine` liefert Default-Implementierungen, die mit einer deutschen `EngineError` ablehnen. Implementiert von `AbletonOscEngine` und `SimEngine`.

```python
@dataclass
class TrackInfo:
    index: int; name: str; is_group: bool; group_index: int | None   # group_index = Elternspur
    num_devices: int; input_type: str | None; input_channel: str | None; num_clips: int

class SessionStructure:                 # iterierbar, len(), []
    tracks: list[TrackInfo]
    def at(index) -> TrackInfo | None; def by_name(name) -> TrackInfo | None
    def group(name) -> TrackInfo | None; def children(group_index) -> list[TrackInfo]

def get_session_structure() -> SessionStructure
def create_track_in_group(group_index: int, after_index: int) -> int   # verifizierter neuer Index
def duplicate_track(index: int) -> int                                 # Kopie bei index+1
def set_track_name(index: int, name: str) -> None                      # mit Readback
def get_input_routing(index: int) -> tuple[str | None, str | None]
def set_input_routing(index: int, type_name: str | None, channel: str | None = None) -> None
```

Verifizierte OSC-Fakten (Spike 2026-09-06, Live 11.3.30), auf denen `AbletonOscEngine` aufbaut:

* `/live/song/export/structure` schreibt `%TEMP%\abletonosc-song-structure.json` (`tracks[].group_track` = Elternindex, `null` bei Top-Level). Ein Call statt vieler Gets. Die Datei kann beim Lesen noch im Schreiben sein → die Engine löscht sie vorher und liest mit Retry, bis JSON parst. Fallback: Einzelabfragen mit der Indexregel (`is_foldable` eröffnet eine Gruppe, folgende `is_grouped` gehören dazu). Testhook: `ABLETONOSC_STRUCTURE_DIR` verlegt das Verzeichnis.
* `/live/song/create_audio_track <index>` mit Index innerhalb der Gruppe landet in der Gruppe, auf dem angeforderten Index; der erste Kind-Slot ist nicht adressierbar (Anfrage `g+1` landet auf `g+2`), deshalb fragt die Engine mindestens `g+2` an.
* `/live/song/duplicate_track <index>` → Kopie bei `index+1`, dieselbe Gruppe, erbt Eingang, Monitoring, Arm, Geräte **und Clips** (deshalb wird nur eine Spur ohne Geräte und ohne Clips dupliziert).
* Live benennt Default-Namen still um (`3-Audio` → `4-Audio`). Spuren werden **nie** über Default-Namen oder gemerkte Indizes identifiziert: nach jeder Strukturänderung wird neu eingelesen und der neue Index gegen Default-Namen bzw. Namens-Readback verifiziert, sonst `EngineError` (deutsch).
* `available_input_routing_types` ändert sich bei jedem Anlegen/Umbenennen → wird pro Spur frisch abgefragt.
* Gruppen selbst kann OSC **nicht** anlegen; das macht Henry von Hand. Das letzte Kind einer Gruppe darf nie gelöscht werden (Live löst die Gruppe auf) — die App löscht grundsätzlich nichts.

## Gruppen-Layer (M4)

Ein Partitur-Track vom Typ `group` bildet eine **Ableton-Gruppenspur** ab. Die Effekte liegen auf der Gruppe, jeder Overdub-Layer bekommt eine eigene Kindspur (unbegrenztes Stapeln, alle Layer klingen gleichzeitig), eine Kindspur ist reiner Live-Monitor.

**Namenskonvention (verbindlich):** Layer-Spuren `<gruppe> L1`, `<gruppe> L2`, … · Monitor-Spur `<gruppe> LIVE`. Kindspuren mit anderen Namen werden ignoriert (und im Report gemeldet), nie umbenannt.

```python
# looper/session.py
def ensure_pool(engine: Engine, score: CompiledScore) -> PoolReport
def resolve_pools(engine: Engine, score: CompiledScore) -> dict[str, GroupPool]
def add_layer(engine: Engine, track: Track) -> tuple[str, int, list[str]]   # genau eine Spur mehr
```

`ensure_pool` sucht die Gruppe über den **expliziten** Namen, zählt vorhandene Layer-Spuren nach der Konvention und legt fehlende an, bis `layers + reserve` erreicht sind — bevorzugt per `duplicate_track` einer nackten (geräte- und cliplosen) Kindspur, sonst per `create_audio_track`. Jede neue Spur wird sofort benannt, bekommt das Eingangsrouting einer bestehenden Kindspur, Monitoring `auto` (Monitor-Spur: `in`) und Arm aus; nach jedem Schritt wird die Struktur neu eingelesen. **Es wird nie verkleinert und nie gelöscht.** Fehlt die Gruppe oder jedes Kind → `EngineError` mit Handlungsanweisung („Gruppe 'voice' existiert nicht im Ableton-Set. Lege sie in Ableton an (Spuren markieren, Strg+G) und benenne sie 'voice'.").

`PoolReport.to_dict()` → `{"changed": bool, "groups": [{"track","group","group_index","existing_layers","created_layers","monitor","monitor_created","layer_indices","monitor_index","strays","warnings","message"}], "messages": [str]}`.

`SimEngine` implementiert dasselbe mit einem Thread-Timer bei `bpm` und simuliert: `fire_slot` auf leeren Slot → ab nächster Taktgrenze `recording`; erneutes `fire_slot` → ab nächster Taktgrenze `playing`; Slots merken sich `has_clip`.

## Kompilierte Partitur (Ausgabe von compile_score, JSON-serialisierbar)

```json
{
  "title": "Beispiel",
  "bpm": 100,
  "beats_per_bar": 4,
  "tracks": [
    {"name": "gitarre", "ableton_track": 0, "type": "single"},
    {"name": "voice",   "type": "group", "group": "voice",
     "layers": 5, "reserve": 3, "monitor": true}
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

**Track-Typen.** `single` (Default): genau eine Ableton-Spur, adressiert über `ableton_track` (0-basiert). `group`: eine Ableton-Gruppenspur, adressiert über `group` (Name), mit `layers` (Mindestzahl Layer-Kindspuren, Default 4), `reserve` (wie viele darüber hinaus immer frei bleiben, Default 2) und `monitor` (Default `true`). `group` und `ableton_track` schließen sich aus; `layers`/`reserve`/`monitor` gibt es nur bei Gruppen-Tracks. Legacy: `{ableton_track: 1, type: group}` (vor M4, ohne `group:`) bleibt ein Einzeltrack ohne Gruppen-Semantik.

Track-States: `record | overdub | play | stop | hear_through`.
Bei einem Gruppen-Track: `record`/`overdub` gehen in die nächste freie Layer-Kindspur (Slot 0; eine Layer-Spur trägt genau einen Clip, dadurch spielen alle Layer gleichzeitig), `play` lässt alle belegten Layer weiterlaufen, `stop` stoppt alle Layer der Gruppe, `hear_through` stoppt die Layer und lässt nur die Monitor-Kindspur (`Monitoring In`) hörbar. `repeat: <id>` ist im kompilierten JSON bereits aufgelöst (Sektion vollständig kopiert, neue `id`/`index`). Jede Sektion enthält **alle** Tracks (fehlende → `stop`, Compiler ergänzt).

`ScoreError.errors` ist eine Liste von `{"line": int|null, "column": int|null, "message": str, "suggestion": str|null}` (deutsch, z. B. „Sektion 'bridge' referenziert Track 'bss' — meintest du 'bass'?").

## YAML v0 (Beispiel — M2 definiert das Schema, M3 nutzt es als Default im Editor)

```yaml
title: Beispiel-Song
bpm: 100
time_signature: 4/4                    # optional, Default 4/4; auch 3/4, 6/8 etc. → beats_per_bar = Zähler
tracks:
  gitarre: {ableton_track: 0}                          # Einzelspur (Default)
  voice:   {group: voice, layers: 5, reserve: 3, monitor: true}   # Ableton-Gruppenspur 'voice'
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
  .start()            # connect, Spurenprüfung, Gruppen auflösen, Tempo setzen, Quantisierung,
                      # Transport starten, Sektion 0 an nächster Taktgrenze
                      # Spurenprüfung: existiert ein referenzierter ableton_track nicht -> EngineError (deutsch, nennt
                      # alle fehlenden Indizes), nichts wird gestartet; Namensabweichung -> nur "log"-Event level "warn"
                      # Gruppen-Tracks: resolve_pools() bindet Gruppe + Layer-Spuren über die Namen;
                      # fehlt die Gruppe -> EngineError (deutsch), der Runner bleibt idle
  .next_section()     # armiert Wechsel ("pending"); ausgeführt am Ende der Sektion (quantize=loop) bzw. nächsten Takt (quantize=bar)
  .stop_all()
  .goto(section_id)   # v0: darf NotImplementedError werfen
  .state() -> dict    # identisch zum "state"-Event
```

**Unbegrenztes Overdub.** Geht der Layer-Pool einer Gruppe zur Laufzeit zur Neige (weniger als `reserve` Layer-Spuren frei), legt der Runner **zwischen** Sektionen nach: nie im Beat-Callback, nie innerhalb von 2 Beats + Vorlauf vor einer Grenze und nie bei armiertem Wechsel. Die blockierende Arbeit läuft auf einem Worker-Thread (`add_layer`), danach werden alle gemerkten Indizes um die Einfügeposition verschoben und die Gruppen über ihre Namen neu aufgelöst. Jeder Schritt wird als `log`-Event gemeldet.

Runner-Events (`on_event(dict)`), alle mit `"type"`:

```json
{"type":"state","running":true,"section_index":1,"section_id":"verse","bars_total":4,"bar":2,"beat":3,
 "pending":"next"|"stop"|null,"tracks":{"gitarre":"play","voice":"record"},"connected":true,"engine":"ableton"|"sim",
 "groups":{"voice":{"layers_used":2,"layers_free":3}}}
{"type":"beat","song_beat":37,"bar":2,"beat":3}
{"type":"midi","device":"MPD218","channel":1,"kind":"note_on"|"note_off"|"cc","number":36,"value":127,"id":"ch1.note36"}
{"type":"log","level":"info"|"warn"|"error","message":"…"}
```

`state` wird bei jeder Änderung komplett gesendet (Snapshot, keine Deltas). `bar`/`beat` sind 1-basiert innerhalb der Sektion. **Taktart ist variabel**: `beats_per_bar` kommt aus dem Score (`time_signature`), der Runner setzt es in der Engine (Ableton: `signature_numerator`/`signature_denominator`) und zählt damit; das UI rendert Beat-Punkte/Zähler dynamisch nach `beats_per_bar` aus dem Score (nicht fest 4). `state` enthält zusätzlich `"beats_per_bar": 3` und `"time_signature": "3/4"`. MIDI-`id`-Format: `ch<channel>.note<n>` bzw. `ch<channel>.cc<n>` — identisch zum Mapping in der Partitur.

## Backend-API (M3, FastAPI auf 127.0.0.1:8000)

```
POST /api/compile     {yaml}  → 200 {ok:true, score:{…}} | 200 {ok:false, errors:[…]}
POST /api/load        {yaml}  → kompiliert + lädt in den Runner (ersetzt Score; nur wenn nicht running)
                                Engine "ableton" + Gruppen-Tracks: ruft danach ensure_pool() auf und
                                liefert zusätzlich {pool: PoolReport}; fehlende Gruppe → 400 {detail}
                                (deutsche Handlungsanweisung). Mit "sim" unverändert (kein pool-Feld).
POST /api/transport/start | /stop_all | /next
                              → 400 {detail} bei EngineError (z. B. fehlende Ableton-Spur; Runner bleibt idle,
                                zusätzlich ein log-Event level "error"), 503 wenn die Engine nicht erreichbar ist
GET  /api/state               → aktuelles state-Event
GET  /api/engines             → {"current":"sim"|"ableton"}   POST /api/engines {"engine":"sim"|"ableton"}
WS   /ws                      → sendet alle Runner-Events + MIDI-Events als JSON, beim Connect zuerst ein state
```

Start: `.venv\Scripts\python.exe -m uvicorn backend.app:app --port 8000`; UI: `cd ui && npm run dev` (Vite-Proxy `/api` und `/ws` → 8000).
