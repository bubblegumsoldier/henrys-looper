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
 │ Editor │ Blockvorschau │ ◄─────────► │ Steuer-Thread ──► Runner             │
 │        │ MIDI-Monitor  │   emit      │       │ lock-freie Queues            │
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

- Windows (macOS wäre portierbar, siehe `docs/architektur.md`)
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

## Benutzen

Alles unter `engine\target\release\looper-engine.exe`.

```powershell
# Geräte und unterstützte Puffergrößen anzeigen
looper-engine list

# Loopen: mehrere Tracks mit eigenen Eingangskanälen
looper-engine live --host asio --device Focusrite --buffer 128 `
    --track stimme:1 --track gitarre:2 --bpm 100 --bars 8

# Latenzkompensation prüfen (Loopback-Kabel von Ausgang 1 in Eingang 1)
looper-engine calibrate --host asio --buffer 128 --click-gain 4.0
```

Tasten in `live` (jeweils mit Enter): `1`–`N` Track wählen · `r` neuer Loop · `o` Overdub ·
`s` Stopp · `p` Wiedergabe · `c` Track leeren · `a` alles leeren · `m` Mithören ·
`e <nr>` Layer stumm · `w <nr>` Layer weg · `l <nr> <wert>` Layer-Lautstärke · `k` Klick ·
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
