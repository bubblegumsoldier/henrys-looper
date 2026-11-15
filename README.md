# henrys-looper

Eine Looping-Software für vorgeplante Live-Arrangements. Statt bei jedem Loop-Wechsel zum
Pedal zu greifen, schreibst du vorher eine Partitur in YAML — Sektionen mit dem Soll-Zustand
jeder Spur — und die Software steppt live hindurch. Manueller Eingriff reduziert sich auf
einen Release-Button.

**Status: in Entwicklung.** Eigene Audio-Engine in Rust mit Desktop-App (Tauri) für macOS
(CoreAudio) und Windows (ASIO), getestet mit einer Focusrite Scarlett 2i2. Formate und API
können sich jederzeit ändern. Die Oberfläche ist englisch, Doku und Code-Kommentare deutsch.

## Die Desktop-App

**Henrys Looper** ist ein Fenster mit vier Bereichen, umgeschaltet in der Kopfleiste:

| Modus | Wofür |
|---|---|
| **Setup** (Zahnrad) | Audio-Gerät, Puffer, Spuren und Eingänge, Klick, Latenz, MIDI-Geräte, Plugins. *Start engine* öffnet das Interface. |
| **Live** | Die Bühnenansicht: je Spur eine Kachel mit *Record · Overdub · Play · Stop · Monitor*, Pegel, Ebenen. Ohne geladene Partitur ein freier Looper; mit Partitur oben Sektion, Takt, Schlag, *Release* und *Stop all*. |
| **Arrangement** | Die Partitur grafisch (Raster Sektionen × Spuren, Inspektor) oder als YAML-Text; *File* (New/Open/Recent/Save) und *Load*. Darunter aufklappbar der **Mixer** (Fader, Pan, Mute/Solo, Effekte, VST3/AU-Plugins, Busse). Läuft eine Partitur, ist das Arrangement gesperrt. |
| **MIDI Learn** | Jedes bedienbare Element lässt sich per Klick und Druck auf ein Pad/einen Regler belegen; die Seitenleiste zeigt alle Belegungen je Gerät. |

Auf dem Mac starten (aus dem Repo-Wurzelverzeichnis):

```sh
. "$HOME/.cargo/env"                     # falls Rust nicht im PATH ist
(cd ui && npm install)                   # einmalig
ui/node_modules/.bin/tauri dev           # Entwicklungsmodus mit Hot Reload
ui/node_modules/.bin/tauri build         # Bundle bauen
open "target/release/bundle/macos/Henrys Looper.app"
```

Typischer Ablauf: Setup → Gerät wählen → *Start engine* → Arrangement → *File › Open*
(z. B. `examples/henry-3-4.rust.yaml`) → *Load* → Live → *Start* (Leertaste) → *Release*
(Leertaste) an den Stellen, die bis Release loopen → *Stop all* (⇧S) → *Back to start*.

Ohne Partitur (freies Loopen) steht die Uhr nach dem Engine-Start auf Takt 1: kein Klick, nichts
zählt. *Start* (Leertaste) lässt sie laufen, *Stop* hält sie an; die Loops bleiben erhalten und
spielen beim nächsten Start von vorn. `R` bei stehender Uhr startet sie und nimmt ab Takt 1 auf.
Ein Stopp verwirft eine noch nicht fertige erste Aufnahme und beendet einen Overdub an der
Stoppstelle.

**Tastenkürzel**

| Wo | Taste | Wirkung |
|---|---|---|
| Live | `1`–`8` | Spur wählen |
| Live | `R` `O` `P` `S` `M` | Record · Overdub · Play · Stop · Monitor der gewählten Spur |
| Live | `I` | Spur-Detail auf/zu |
| Live | `F` | Effekte der Spur umgehen (Bypass) |
| Live | `X` / `V` / `D` + Ziffer | Effekt an/aus · Preset laden · Delay-Notenwert |
| Live | `K` | Klick an/aus |
| Live/Arrangement | Leertaste oder `N` | Partitur starten bzw. *Release* |
| Live/Arrangement, ohne Partitur | Leertaste | Uhr starten/stoppen (*Start*/*Stop*, MIDI `transport.toggle`) |
| Live/Arrangement | `⇧S` | *Stop all* |
| Arrangement | `⌘N` `⌘O` `⌘S` `⇧⌘S` | New · Open · Save · Save as |
| Arrangement | `⌘Z` / `⇧⌘Z` · `⌘D` · `+`/`-` · `⌫` · `Esc` | Undo/Redo · Sektion duplizieren · Zoom · zurücksetzen · Auswahl aufheben |
| Arrangement | Clip gewählt: `R` `O` `P` `S` `M` | Zielzustand setzen |
| überall | `⌘`-Klick | Element per MIDI lernen |

Beispielpartituren im aktuellen Format: `examples/henry-3-4.rust.yaml`,
`examples/solo-loop-4-4.yaml`, `examples/waltz-three-tracks-3-4.yaml`. Die Dateien in
`examples/legacy-python/` sind Altformat (Ableton-Prototyp) und laden in der App nicht.

## Wo anfangen zu lesen

**➜ [`docs/architektur.md`](docs/architektur.md)** — alle getroffenen Entscheidungen samt
Begründung und Messwerten. Der richtige Einstieg für jeden, der neu draufschaut.

| Dokument | Inhalt |
|---|---|
| [`docs/architektur.md`](docs/architektur.md) | Entscheidungen, Regeln, Datenmodell, Stand |
| [`docs/plan-standalone-rust.md`](docs/plan-standalone-rust.md) | Plan mit Phasen und Zielen |
| [`docs/phase0-audio-messung.md`](docs/phase0-audio-messung.md) | Messbericht: Latenz, Aussetzer, ASIO gegen WASAPI |
| [`docs/contracts-v0.md`](docs/contracts-v0.md) | Partitur- und Event-Formate |

## Wie es funktioniert

Ein einziger Rust-Prozess hält alles: Audio-I/O, Loop-Puffer, Klick, Partitur und später den
Webserver. Drei Threads, klar getrennt.

```
 Oberfläche (Tauri + WebView)              Rust-Prozess
 ┌────────────────────────┐   invoke    ┌──────────────────────────────────────┐
 │ Arrangement │ Live     │ ◄─────────► │ Steuer-Thread ──► Runner             │
 │ Setup       │ MIDI     │   emit      │       │ lock-freie Queues            │
 └────────────────────────┘             │       ▼                              │
                                        │ Audio-Thread (Echtzeit)              │
                                        │ Mixer · Tracks · Layer · Klick       │
                                        └───────────┬──────────────────────────┘
                                                    │ cpal / ASIO
                                              Audio-Interface
```

Der Kniff ist **musikalische Zeit statt Wanduhr**: Der Audio-Thread führt eine fortlaufende
Sample-Position. Kommandos tragen Zeitstempel auf dieser Achse — „Track 2, Aufnahme starten
bei Takt 9, Schlag 1". Der Steuer-Thread schickt sie beliebig früh, der Audio-Thread führt
sie sample-genau aus, auch mitten im Puffer.

## Voraussetzungen

- Windows (für macOS siehe [unten](#macos))
- **Ein Audio-Interface mit ASIO-Treiber.** WASAPI ist keine Alternative — die Begründung mit
  Messwerten steht im Messbericht.
- Rust (MSVC-Toolchain) plus **MSVC Build Tools mit C++-Workload**
- **LLVM** und das **Steinberg ASIO-SDK** für den ASIO-Build

```powershell
winget install --id Microsoft.VisualStudio.2022.BuildTools -e --override "--quiet --wait --add Microsoft.VisualStudio.Workload.VCTools --includeRecommended"
winget install --id LLVM.LLVM -e
```

ASIO-SDK von `https://www.steinberg.net/asiosdk` (leitet ohne Registrierung direkt auf das
ZIP), entpacken, dann `CPAL_ASIO_DIR` auf das Verzeichnis setzen. Seit Oktober 2025 ist das
SDK zusätzlich unter GPL3 lizenziert, passend zu diesem Repo.

## Bauen

```powershell
$env:CPAL_ASIO_DIR="C:\SDKs\ASIOSDK"
$env:LIBCLANG_PATH="$env:ProgramFiles\LLVM\bin"
cd engine
cargo build --release --features asio
cargo test
```

## macOS

Auf dem Mac läuft alles über CoreAudio; ASIO, LLVM und das SDK braucht es dort nicht.

**Voraussetzungen:** Xcode Command Line Tools (`xcode-select --install`), Rust über
[rustup](https://rustup.rs), Node.js ≥ 18. Ein- und Ausgang müssen auf demselben Gerät liegen
(eine Clock) — sollen es zwei sein, in „Audio-MIDI-Setup“ ein Aggregatgerät anlegen.

```sh
cd ui && npm install && cd ..        # Oberfläche samt Tauri-CLI
cargo test                           # alle Tests
ui/node_modules/.bin/tauri dev       # App im Entwicklungsmodus (aus dem Repo-Wurzelverzeichnis)
ui/node_modules/.bin/tauri build     # target/release/bundle/macos/Henrys Looper.app
open "target/release/bundle/macos/Henrys Looper.app"

cargo build --release -p looper-engine
target/release/looper-engine list                       # Host coreaudio, Geräte
target/release/looper-engine thru --device Scarlett --buffer 128 --gain 0.1
```

Vorgaben auf dem Mac: Host `coreaudio`, 48 kHz, 128 Frames. Die Latenzvorgabe von **596 Frames
ist geschätzt, nicht gemessen** (CoreAudio-Angaben einer Scarlett 2i2 plus Tiefe des
Eingangspuffers); vor ernsthaften Aufnahmen mit `calibrate` und Loopback-Kabel nachmessen.
Beim ersten „Engine starten“ der gebündelten App fragt macOS nach dem Mikrofonzugriff; unter
`tauri dev` gilt die Freigabe des Terminals. Kürzel nehmen ⌘ statt Strg (⌘+Enter übersetzt, ⌘+⇧+Enter lädt,
⌘+N/⌘+O/⌘+S/⌘+⇧+S für Neu/Öffnen/Speichern/Speichern unter, ⌘+Klick zum MIDI-Lernen).
Partituren liegen als `.yaml` standardmäßig in `~/Documents/Henrys Looper/`. Logdatei: `~/Library/Logs/de.getmika.henrys-looper/`,
MIDI-Profile: `~/Library/Application Support/henrys-looper/midi/` (eine YAML-Datei je Gerät;
bekannte Controller verbinden sich beim Start und beim Anstecken selbst, jede Belegung wird sofort
gespeichert, Kommentare in der Datei bleiben erhalten). Ohne Controller testen:
`cargo run -p looper-engine --example midi-virtual -- "Testpad"` legt einen virtuellen
CoreMIDI-Eingang an und sendet Befehle von stdin (`note 36`, `cc 3 64`, `quit`).

## Plugins (macOS)

VST3- und Audio-Unit-Effekte in acht Slots je Track (Track-Drawer) und je Bus (Einrichtung >
Plugins). Gesucht wird in `/Library/Audio/Plug-Ins/VST3`, `~/Library/Audio/Plug-Ins/VST3` und den
Ordnern in `HENRYS_LOOPER_PLUGIN_PATH`; Audio Units kommen vom System. Jedes Plugin wird beim Scan
zuerst in einem eigenen Prozess getestet. Ohne App:

```sh
target/debug/looper-engine plugins scan          # alles finden und testen (Cache plugins.json)
target/debug/looper-engine plugins probe AUReverb2
target/debug/looper-engine plugins edit AUDelay --seconds 5
```

Details: `docs/architektur.md`, Abschnitt 14.

## Arrangement in der App

Die Partitur lässt sich vollständig grafisch arrangieren (Modus **Arrangement**): ein Raster aus
Sektionen × Spuren, rechts der Inspektor. **Grid | Text (YAML)** sind zwei Sichten desselben
Dokuments — was im Raster passiert, steht sofort im Text (kanonisch formatiert, Kommentare
bleiben am Element), Getipptes erscheint nach einer kurzen Pause im Raster. ⌘Z/⇧⌘Z gelten für
beide Sichten gemeinsam. Ist der Text fehlerhaft, zeigt das Raster den letzten gültigen Stand
schreibgeschützt.

- Clip anklicken (⇧ = Rechteck, ⌘ = einzeln dazu), dann `R` Record · `O` Overdub · `K` Stack ·
  `P` Play · `S` Stop · `M` Monitor; Pfeiltasten wandern, `⌫` setzt auf die Vorlage zurück.
  **Stack** (`stack` im YAML) legt in jedem Durchlauf eine neue Ebene an, bis die Sektion endet —
  mit `autorelease: false` also bis Release; leere Durchläufe bleiben nicht liegen.
- Sektionsmarker: ziehen = verschieben, rechte Kante ziehen = Takte, Doppelklick = umbenennen
  (`repeat:` zieht mit, `transport.goto.<n>` wird beim Verschieben umnummeriert); ⌘D dupliziert.
- Spurkopf: ziehen = umsortieren, Doppelklick = umbenennen (Zustände und `track.<name>.*`
  ziehen mit), Mithören/Busse als Schalter, Pan am Knopf.
- **File** (Neu, Öffnen, Zuletzt, Sichern) und **Load** in der Werkzeugleiste. Das Feld
  **Count-in** daneben ist das Partiturfeld `count_in:` (Takte Klick vor der ersten Sektion, 1–8;
  fehlt es, sind es 2 Takte) und wird mit dem Stück gespeichert. Im Live-Modus steht der Einzähler
  neben Start/Stop all („Count-in 2 bars") und lässt sich vor dem Start umschalten. Geladen, aber
  nicht gestartet, ist die Partitur still — der Klick beginnt mit dem Einzähler und endet mit dem
  Ende oder „Stop all".
  Läuft eine Partitur, ist das Arrangement gesperrt; ein Klick auf einen Sektionsmarker springt.

**Mix speichern.** Ist eine Partitur geladen, landet jede Mixer-Änderung (Fader, Mute,
Effekte, Bus-/Klick-/Mithörpegel — per Maus, Taste oder MIDI) nach einem Moment im Abschnitt
`mixer:` der Partitur; die Datei gilt als geändert, ⌘S speichert den Mix mit dem Stück, ⌘Z nimmt
eine Fader-Fahrt samt Klang zurück. Beim Laden wird der Abschnitt angewendet, auch nach dem
Neuaufbau der Engine. Solo wird absichtlich nicht gespeichert. Ohne Partitur merkt sich die App
den Mix je Spurname in `mixer.json` (Konfigurationsordner) und die Buspegel im Setup. Plugins
gehören ebenfalls dem Spurnamen (`plugin-rack.json`), nicht der Spurnummer. Pan landet unter
`tracks:`, Instrument und Effekt-Plugins einer Spur unter `mixer:` — nur ihre Ids, der Klang
bleibt auf dem Rechner. Details: `docs/architektur.md`, Abschnitt 15.

**Instrument-Spuren.** Eine Spur kann statt eines Eingangs ein Instrument-Plugin haben, das ein
MIDI-Keyboard spielt; geloopt wird sein Klang. In der Partitur (Beispiel
`examples/piano-loop-4-4.yaml`):

```yaml
tracks:
  keys:  {input: instrument, midi: {device: "KeyLab 49", channel: 1}}
  drums: {input: instrument, midi: {device: MPD218, channel: 10, notes: 36-51}}
mixer:
  tracks:
    keys:
      instrument: "au:aumu:dls :appl"                          # Plugin-Id, kein Zustand
      inserts: [{plugin: "au:aufx:dely:appl", bypass: false}]  # Effekte in Slot-Reihenfolge
```

`device` fehlt = alle Geräte, `channel: 1..16|all`, `notes` fehlt = alle Tasten. Im Arrangement
wählt der Inspektor der Spur *Source: Instrument*, das Plugin, Gerät, Kanal und Tasten. Eine
Instrument-Spur ist immer zu hören, wenn sie gespielt wird (`monitor: false` macht sie stumm);
der Monitor-Schalter wirkt auf sie nicht. Fehlt ein genanntes Plugin auf dem Rechner, läuft die
Spur stumm bzw. ohne diesen Effekt weiter, eine Meldung sagt es, und der Eintrag bleibt in der
Datei. `loop: audio|midi` und `grid: none|1/4|1/8|1/16|1/8t|1/16t` (je Spur, je Clip als
`keys: {state: play, grid: 1/16}`) sind für MIDI-Loops und ihre Quantisierung reserviert: sie
werden geprüft und gespeichert, tun aber noch nichts („not supported yet"). Die
Kommandozeile spielt keine Partitur mit Instrument-Spuren (`--check` zeigt sie).

Die Logik dahinter (Bearbeitungsoperationen, Verweise, Undo) testet `npm test` in `ui/` (vitest).

## Benutzen

Die Kommandozeile liegt nach `cargo build --release -p looper-engine` unter `target/release/looper-engine` (Windows: `target\release\looper-engine.exe`); ihre Ausgaben sind englisch.

```powershell
# Geräte und unterstützte Puffergrößen anzeigen
looper-engine list

# Loopen: mehrere Tracks mit eigenen Eingangskanälen
looper-engine live --host asio --device Focusrite --buffer 128 `
    --track stimme:1 --track gitarre:2 --bpm 100 --bars 8

# Eine Quelle über einen Plugin-Host kommt früher an als ein Mikrofon: eigener Wert je Track
looper-engine live --host asio --buffer 128 `
    --track stimme:1 --track cantabile:5-6 `
    --latency-frames 827 --track-latency cantabile:512+96

# Latenzkompensation prüfen (Loopback-Kabel von Ausgang 1 in Eingang 1)
looper-engine calibrate --host asio --buffer 128 --click-gain 4.0

# ... für den zweiten Track; über einen ASIO-Router braucht es dafür kein Kabel
looper-engine calibrate --host asio --buffer 128 --click-gain 4.0 `
    --track stimme:1 --track cantabile:5-6 --for-track 2
```

Tasten in `live` (jeweils mit Enter): `1`–`N` Track wählen · `r` neuer Loop · `o` Overdub ·
`s` Stopp · `p` Wiedergabe · `c` Track leeren · `a` alles leeren · `m` Mithören ·
`e <nr>` Layer stumm · `w <nr>` Layer weg · `l <nr> <wert>` Layer-Lautstärke ·
`n <wert>` Panorama · `i <frames|-> [zuschlag]` Latenzkompensation dieses Tracks · `k` Klick ·
`t` Tempo · `q` Ende.

### Messwerkzeuge aus Phase 0

`thru` (Durchhören), `click` (Metronom), `latency` (Roundtrip messen), `soak` (Dauerlauf mit
Aussetzerzählung). Sie bleiben erhalten, um nach Hardware-Wechseln nachzumessen.

## Vorgeschichte

Ein erster Prototyp steuerte Ableton Live über AbletonOSC. Er ist an fehlendem
Zustands-Feedback, fremder Looping-Semantik und grober Quantisierung gescheitert — die
Analyse steht in [`docs/plan-standalone-rust.md`](docs/plan-standalone-rust.md), die
ursprüngliche Vision in [`docs/handover-tag1.md`](docs/handover-tag1.md).

Der Python-Code dieses Ansatzes (`backend/`, `looper/`, `spike/`, `tests/`) und seine
Partituren (`examples/legacy-python/`) liegen als **Altbestand** weiter im Repo; die App benutzt
nichts davon. Das Partitur-Modell wurde übernommen, der OSC-Pfad nicht. Die heutige Oberfläche in
`ui/` ist neu geschrieben und gehört zur Desktop-App.

## Lizenz

GPL-3.0, siehe [LICENSE](LICENSE).
