# henrys-looper

Ein „Dirigent" für vorgeplante Live-Loop-Arrangements. Du schreibst vorher eine Partitur in YAML (Sektionen mit dem Soll-Zustand jeder Spur), die Software steppt live hindurch und steuert Ableton Live 11 über [AbletonOSC](https://github.com/ideoforms/AbletonOSC). Ableton nimmt auf, loopt und hält das Tempo; die App sagt nur, wann welche Spur was tun soll. Manueller Eingriff reduziert sich auf einen Release-Button (Taste, UI oder MIDI-Pedal).

**Status: Prototyp, Tag 1, experimentell.** Nur auf einer Windows-Maschine getestet, API und YAML-Format können sich jederzeit ändern.

## Wie es funktioniert

Der Compiler liest die YAML-Partitur, validiert sie gegen ein JSON-Schema, löst Referenzen auf (`repeat`, Track-Namen, MIDI-IDs) und erzeugt ein statisches JSON. Der Runner steppt durch dieses JSON, getrieben von den Beat-Events der Engine: Jeder Sektionswechsel liegt auf einer Taktgrenze; quantisierte Befehle (Clip feuern, Spur stoppen) gehen kurz vor der Taktgrenze raus und Ableton führt sie mit Global Quantization = 1 Bar aus. Die Engine ist ein Interface mit zwei Implementierungen: `AbletonOscEngine` (UDP 11000 hin, 11001 zurück) und `SimEngine` (simulierter Beat-Clock ohne Ableton, für Tests und UI-Entwicklung). MIDI-Controller liest die App direkt per `mido`, nicht über Ableton.

```
 Browser (React/Vite, CodeMirror)          Python                              Ableton Live 11
 ┌──────────────────────────────┐   REST   ┌──────────────────────────┐   OSC   ┌────────────────┐
 │ YAML-Editor │ Block-Vorschau │ ◄──────► │ FastAPI  ──►  Runner     │ ──────► │ AbletonOSC     │
 │             │ MIDI-Monitor   │   WS     │ (backend) ◄── Engine     │ ◄────── │ (Remote Script)│
 └──────────────────────────────┘          └──────────────────────────┘ 11000/  └────────────────┘
                                                      ▲ mido            11001
                                             MIDI-Controller (direkt)
```

## Voraussetzungen

- Windows (aktuell getestet; die CLI-Tastensteuerung nutzt `msvcrt`, hat aber einen POSIX-Fallback)
- Ableton Live 11 oder neuer, jede Edition
- [AbletonOSC](https://github.com/ideoforms/AbletonOSC) als Remote Script: Repo nach `Documents\Ableton\User Library\Remote Scripts\AbletonOSC` kopieren, Live neu starten, unter Preferences → Link/Tempo/MIDI als Control Surface „AbletonOSC" wählen. Im Log muss „Listening for OSC on port 11000" erscheinen.
- Python 3.11
- Node 20+ (für das UI)

## Schnellstart

```powershell
python -m venv .venv
.venv\Scripts\python.exe -m pip install -r requirements.txt
cd ui; npm install; cd ..
```

Backend und UI starten (zwei Terminals):

```powershell
.venv\Scripts\python.exe -m uvicorn backend.app:app --port 8000
cd ui; npm run dev        # http://localhost:5173, proxied /api und /ws nach :8000
```

Das Backend lädt beim Start `examples/example-song.yaml` und startet mit der Engine `sim`. Teste damit zuerst: Partitur im Editor ändern, kompilieren, Start, mit „Next Section" durchsteppen. Erst wenn das funktioniert, im UI auf `ableton` wechseln. Dafür muss Live laufen, AbletonOSC geladen sein und dein Set die Spuren enthalten, die die Partitur referenziert (Audio-Spuren mit zugewiesenem Input, Slots in den ersten Szenen leer). Die erste UDP-Verbindung kann einen Windows-Firewall-Dialog auslösen.

Tests: `.venv\Scripts\python.exe -m pytest`.

## Partitur-Format

Gekürzt aus `examples/example-song.yaml`:

```yaml
title: Beispiel-Song
bpm: 100
time_signature: 4/4                   # optional, Default 4/4; auch 3/4, 6/8 …

tracks:
  gitarre: {ableton_track: 0}         # 0-basierter Index der Spur im Ableton-Set
  voice:   {ableton_track: 1}

midi:                                 # optional
  next_section: {cc: 64}              # → ID ch1.cc64 (channel optional, Default 1)
  stop_all:     {note: 37}            # → ID ch1.note37

sections:
  - id: intro
    bars: 4
    autorelease: false                # loopt, bis du Release drückst
    quantize: loop                    # Wechsel am Ende des laufenden Zyklus
    tracks:
      gitarre: record
      voice: hear_through             # Monitoring In, keine Aufnahme
  - id: verse
    bars: 4
    autorelease: true                 # nach 4 Takten automatisch weiter
    tracks:
      gitarre: play
      voice: record
  - id: outro
    repeat: verse                     # kopiert verse, Overrides darunter
    autorelease: false
    tracks:
      gitarre: overdub
```

- `bpm`, `time_signature`: Tempo und Taktart, werden beim Start in Ableton gesetzt. Es läuft immer mit Klick (Abletons Metronom).
- `tracks`: Name → `ableton_track` (0-basiert). `type: single|group` wird vom Schema akzeptiert, `group` hat aber noch keine eigene Semantik.
- `sections`: Jede Sektion beschreibt den kompletten Soll-Zustand aller Spuren (keine Deltas). Nicht genannte Spuren werden zu `stop`.
  - `bars`: Länge in Takten, auch ungerade.
  - `autorelease`: `true` = nach `bars` Takten automatisch weiter. `false` = Sektion loopt, bis `next_section` kommt; der Wechsel ist dann „armiert" (sichtbar im UI) und passiert quantisiert.
  - `quantize`: `loop` (Default) = am Ende des laufenden Zyklus, `bar` = an der nächsten Taktgrenze.
  - Track-States: `record` (neuer Clip im nächsten freien Slot), `overdub` (ebenfalls neuer Clip), `play`, `stop`, `hear_through` (Monitoring In ohne Aufnahme).
  - `repeat: <id>`: kopiert eine frühere Sektion; `tracks`/`bars`/`autorelease` darunter überschreiben.
- `midi`: Mapping von Aktionen auf CC oder Note. Die IDs (`ch<kanal>.cc<n>`, `ch<kanal>.note<n>`) sind dieselben, die der MIDI-Monitor im UI live anzeigt.

Die letzte Sektion loopt endlos; `next_section` dort wird ignoriert. Vor Sektion 1 kommt ein Einzähl-Takt.

Ehrlicher Hinweis zu `overdub` und `hear_through`: Ableton spielt pro Spur genau einen Clip. `overdub` auf einer Einzelspur bedeutet deshalb „neuer Layer ersetzt den laufenden Clip", nicht „Layer obendrauf". Für echte Schichten brauchst du pro Layer eine eigene Ableton-Spur, und für durchgehendes Monitoring eine eigene Spur, die nie aufnimmt. `examples/henry-3-4.yaml` zeigt genau das (`voice`, `voice2`, `voice_live` auf demselben Input).

Fehlermeldungen des Compilers kommen mit Zeilennummer und Tippfehler-Vorschlag („Sektion 'bridge' referenziert Track 'bss' — meintest du 'bass'?").

## CLI

```
python -m looper compile <file> [--json]                  # validieren, aufgelöstes JSON ausgeben
python -m looper run <file> --engine sim|ableton          # Partitur abspielen (Default: sim)
    --speed 4          nur sim: Zeitraffer
    --beats            jeden Beat ausgeben statt nur Taktanfang
    --host/--send-port/--reply-port   OSC-Adresse (Default 127.0.0.1, 11000, 11001)
```

Tasten während `run`: `Enter`/`n`/Leertaste = nächste Sektion, `s` = Stop All, `q` oder Ctrl+C = beenden (räumt auf: Arm aus, Clips stoppen). Exit-Codes: 0 ok, 1 Partitur-Fehler, 2 Engine-Fehler, 3 Aufruf-Fehler.

## Projektstruktur

```
looper/          Python-Bibliothek: score/ (Schema + Compiler), engine/ (base, ableton_osc, sim), runner.py, midi.py, __main__.py (CLI)
backend/         FastAPI-App (REST + WebSocket), bindet looper/ ein; _stub_looper/ als Fallback ohne echtes Paket
ui/              React + Vite + CodeMirror 6: Editor, Block-Vorschau, MIDI-Monitor, Transport
examples/        Beispiel-Partituren
spike/           M1-Spike: Einzelskript, das Ableton per OSC als Looper ansteuert (eigene README)
tests/           pytest (Compiler, Runner, Sim-Engine, OSC-Engine, MIDI, CLI)
docs/            Übergabedokument und Schnittstellen-Vertrag v0
```

## Bekannte Einschränkungen / Roadmap

- Nur Windows getestet. Auf Windows sind MIDI-Geräte meist exklusiv: ein Controller kann entweder in dieser App oder in Ableton aktiv sein, nicht in beiden.
- MIDI-Controller-Steuerung ist implementiert, aber noch nicht mit echter Hardware getestet (Tag 1 gab es keinen Controller).
- Kein Smart-Start per Audio-Threshold (geht über OSC nicht); Record startet quantisiert an der Taktgrenze.
- Keine echten Gruppen-Layer (`type: group` ohne Wirkung); Layer laufen über separate Spuren.
- Keine Sprünge/Szenarien zur Laufzeit (`goto` ist nicht implementiert), keine Variablen oder Laufzeitlogik im YAML.
- Kein Stem-Export; die Aufnahmen liegen als Clips in deinem Ableton-Set.
- Später geplant: Gruppen-Layer, Smart-Start, Szenarien/Sprünge, eine eigene Audio-Engine hinter demselben `Engine`-Interface (damit YAML-Format und UI unverändert bleiben).

## Lizenz

GPL-3.0, © 2026 Henry Müssemann. Siehe `LICENSE`.

AbletonOSC ist ein eigenständiges Projekt von [ideoforms](https://github.com/ideoforms/AbletonOSC) mit eigener Lizenz und ist nicht Teil dieses Repositories. Ableton Live ist ein Produkt von Ableton AG; dieses Projekt steht in keiner Verbindung zu Ableton.
