# henrys-looper

Eine Looping-Software für vorgeplante Live-Arrangements. Statt bei jedem Loop-Wechsel zum
Pedal zu greifen, schreibst du vorher eine Partitur in YAML — Sektionen mit dem Soll-Zustand
jeder Spur — und die Software steppt live hindurch. Manueller Eingriff reduziert sich auf
einen Release-Button.

**Status: in Entwicklung.** Eigene Audio-Engine in Rust, getestet auf einer Windows-Maschine
mit Focusrite Scarlett 2i2. Formate und API können sich jederzeit ändern.

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

## Arrangement in der App

Die Partitur lässt sich vollständig grafisch arrangieren (Modus **Arrangement**): ein Raster aus
Sektionen × Spuren, rechts der Inspektor. **Grid | Text (YAML)** sind zwei Sichten desselben
Dokuments — was im Raster passiert, steht sofort im Text (kanonisch formatiert, Kommentare
bleiben am Element), Getipptes erscheint nach einer kurzen Pause im Raster. ⌘Z/⇧⌘Z gelten für
beide Sichten gemeinsam. Ist der Text fehlerhaft, zeigt das Raster den letzten gültigen Stand
schreibgeschützt.

- Clip anklicken (⇧ = Rechteck, ⌘ = einzeln dazu), dann `R` Record · `O` Overdub · `P` Play ·
  `S` Stop · `M` Monitor; Pfeiltasten wandern, `⌫` setzt auf die Vorlage zurück.
- Sektionsmarker: ziehen = verschieben, rechte Kante ziehen = Takte, Doppelklick = umbenennen
  (`repeat:` zieht mit, `transport.goto.<n>` wird beim Verschieben umnummeriert); ⌘D dupliziert.
- Spurkopf: ziehen = umsortieren, Doppelklick = umbenennen (Zustände und `track.<name>.*`
  ziehen mit), Mithören/Busse als Schalter, Pan am Knopf.
- **File** (Neu, Öffnen, Zuletzt, Sichern) und **Load** (mit Einzähler) in der Werkzeugleiste.
  Läuft eine Partitur, ist das Arrangement gesperrt; ein Klick auf einen Sektionsmarker springt.

Die Logik dahinter (Bearbeitungsoperationen, Verweise, Undo) testet `npm test` in `ui/` (vitest).

## Benutzen

Alles unter `engine\target\release\looper-engine.exe`.

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

Der Python- und React-Code dieses Ansatzes (`backend/`, `looper/`, `spike/`, `ui/`) liegt
weiter im Repo. Das Partitur-Modell und die Oberfläche werden übernommen, der OSC-Pfad nicht.
Er wird **ersetzt, nicht angebunden**: Prozessgrenzen waren das Problem des alten Ansatzes.

## Lizenz

GPL-3.0, siehe [LICENSE](LICENSE).
