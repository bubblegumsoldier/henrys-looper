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
- `tracks`: Entweder eine Einzelspur (`{ableton_track: 0}`, 0-basierter Index) oder ein Gruppen-Track (`{group: voice}`, Name der Ableton-Gruppenspur) — siehe „Gruppen-Setup" weiter unten.
- `sections`: Jede Sektion beschreibt den kompletten Soll-Zustand aller Spuren (keine Deltas). Nicht genannte Spuren werden zu `stop`.
  - `bars`: Länge in Takten, auch ungerade.
  - `autorelease`: `true` = nach `bars` Takten automatisch weiter. `false` = Sektion loopt, bis `next_section` kommt; der Wechsel ist dann „armiert" (sichtbar im UI) und passiert quantisiert.
  - `quantize`: `loop` (Default) = am Ende des laufenden Zyklus, `bar` = an der nächsten Taktgrenze.
  - Track-States: `record` (neuer Clip im nächsten freien Slot), `overdub` (ebenfalls neuer Clip), `play`, `stop`, `hear_through` (Monitoring In ohne Aufnahme).
  - `repeat: <id>`: kopiert eine frühere Sektion; `tracks`/`bars`/`autorelease` darunter überschreiben.
- `midi`: Mapping von Aktionen auf CC oder Note. Die IDs (`ch<kanal>.cc<n>`, `ch<kanal>.note<n>`) sind dieselben, die der MIDI-Monitor im UI live anzeigt.

Die letzte Sektion loopt endlos; `next_section` dort wird ignoriert. Vor Sektion 1 kommt ein Einzähl-Takt.

Ehrlicher Hinweis zu `overdub` und `hear_through`: Ableton spielt pro Spur genau einen Clip. `overdub` auf einer **Einzelspur** bedeutet deshalb „neuer Layer ersetzt den laufenden Clip", nicht „Layer obendrauf". Für echte Schichten nimmst du einen Gruppen-Track (siehe unten); `examples/henry-3-4.yaml` zeigt die alte Variante von Hand (`voice`, `voice2`, `voice_live` auf demselben Input).

Fehlermeldungen des Compilers kommen mit Zeilennummer und Tippfehler-Vorschlag („Sektion 'bridge' referenziert Track 'bss' — meintest du 'bass'?").

## Gruppen-Setup (echte Overdub-Layer)

Ein Gruppen-Track bildet **eine Ableton-Gruppenspur** ab: Die Effekte liegen auf der Gruppe, jeder Overdub-Layer bekommt eine eigene nackte Kindspur, und eine Kindspur ist reiner Live-Monitor. Dadurch stapelst du beliebig viele Layer, und alle klingen gleichzeitig.

```yaml
tracks:
  gitarre: {ableton_track: 0}                          # Einzelspur wie bisher
  voice:   {group: voice, layers: 4, reserve: 2}       # Ableton-Gruppenspur namens 'voice'
```

- `group`: Name der Gruppenspur in Ableton (schließt `ableton_track` aus).
- `layers`: Mindestzahl an Layer-Spuren (Default 4).
- `reserve`: wie viele Layer-Spuren darüber hinaus immer frei bleiben (Default 2). Beim Laden legt die App also `layers + reserve` Layer-Spuren an; geht der Vorrat während des Stücks zur Neige, legt der Runner **zwischen** Sektionen nach (nie mitten im Takt).
- `monitor`: Default `true` — die Monitor-Kindspur `<gruppe> LIVE`.

**Namenskonvention (verbindlich):** Layer-Spuren heißen `voice L1`, `voice L2`, … · die Monitor-Spur `voice LIVE`. Nur so findet die App die Spuren wieder — Ableton benennt Default-Namen (`3-Audio`) beim Verschieben still um. Kindspuren mit anderen Namen lässt die App in Ruhe.

**Das machst du einmal von Hand in Ableton** (OSC kann keine Gruppen anlegen):

1. Eine Audiospur anlegen, Input zuweisen (z. B. Ext. In 1), Monitoring sinnvoll setzen.
2. Spur markieren → **Strg+G** → die entstandene Gruppenspur exakt `voice` nennen (identisch zu `group:` in der Partitur).
3. Deine Effekte (Reverb, EQ, Kompressor …) auf die **Gruppenspur** ziehen, nicht auf die Kindspur.
4. Die erste Kindspur `voice L1` nennen — nackt lassen, also keine Geräte darauf.
5. Optional, aber empfohlen: eine zweite Kindspur `voice LIVE` mit demselben Input; sie nimmt nie auf und ist dein durchgehender Live-Monitor. Fehlt sie, legt die App sie beim Laden an.

Alles Weitere (`voice L2`, `voice L3`, …) legt die App beim **Laden** der Partitur selbst an: sie dupliziert eine nackte Kindspur (oder legt eine neue Audiospur in der Gruppe an), benennt sie sofort, übernimmt das Eingangsrouting und schaltet Arm aus. Sie **löscht und verkleinert nie** — das letzte Kind zu löschen würde die Gruppe in Ableton auflösen. Was vorbereitet wurde, steht danach im UI-Log („Gruppe voice: 2 Layer-Spuren vorhanden, 3 angelegt (voice L2, voice L3, voice L4)").

Fehlt die Gruppe, bricht das Laden mit einer klaren Meldung ab: „Gruppe 'voice' existiert nicht im Ableton-Set. Lege sie in Ableton an (Spuren markieren, Strg+G) und benenne sie 'voice'."

Vollständiges Beispiel: `examples/henry-3-4-group.yaml`.

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
looper/          Python-Bibliothek: score/ (Schema + Compiler), engine/ (base, ableton_osc, sim), session.py (Gruppen-Pool), runner.py, midi.py, __main__.py (CLI)
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
- Gruppen-Layer sind implementiert, aber noch nicht mit echtem Ableton-Set getestet (Stand M4). Die Gruppenspur selbst musst du von Hand anlegen — OSC kann das nicht.
- Keine Sprünge/Szenarien zur Laufzeit (`goto` ist nicht implementiert), keine Variablen oder Laufzeitlogik im YAML.
- Kein Stem-Export; die Aufnahmen liegen als Clips in deinem Ableton-Set.
- Später geplant: Smart-Start, Szenarien/Sprünge, eine eigene Audio-Engine hinter demselben `Engine`-Interface (damit YAML-Format und UI unverändert bleiben).

## Lizenz

GPL-3.0, © 2026 Henry Müssemann. Siehe `LICENSE`.

AbletonOSC ist ein eigenständiges Projekt von [ideoforms](https://github.com/ideoforms/AbletonOSC) mit eigener Lizenz und ist nicht Teil dieses Repositories. Ableton Live ist ein Produkt von Ableton AG; dieses Projekt steht in keiner Verbindung zu Ableton.
