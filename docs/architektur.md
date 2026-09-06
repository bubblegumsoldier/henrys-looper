# Architektur und verbindliche Entscheidungen

Zentrales Nachschlagewerk. Wer neu auf dieses Projekt schaut — Mensch oder Agent — liest
**dieses Dokument zuerst**. Es sammelt die Entscheidungen, die getroffen wurden, samt
Begründung und den Messwerten, auf denen sie beruhen. Stand 2026-09-06.

## Wo was steht

| Dokument | Inhalt |
|---|---|
| `docs/architektur.md` | dieses Dokument: getroffene Entscheidungen, Regeln, Datenmodell |
| `docs/plan-standalone-rust.md` | der Plan mit Phasen und Zielen. Bleibt gültig |
| `docs/phase0-audio-messung.md` | Messbericht: Latenz, Aussetzer, WASAPI gegen ASIO |
| `docs/contracts-v0.md` | Partitur-Format und Event-Formate. Konzeptionell gültig, Ableton-Teile veraltet |
| `docs/handover-tag1.md` | Vision und Geschichte des ersten Ansatzes. Historisch |

Der Code liegt in `engine/`. Ausführliche Herleitungen stehen als Modulkommentar im Code,
besonders in `engine/src/engine/process.rs` (Latenzkompensation). Die Commit-Messages sind
bewusst lang und erklären das Warum.

## Stand

| Phase | Inhalt | Status |
|---|---|---|
| 0 | Audio-Nachweis unter Windows | **abgenommen**, am Instrument geprüft |
| 1 | Loop-Kern, ein Track | **abgenommen**, Gitarre eingespielt, Kalibrierung bestätigt |
| 2 | Mehrere Tracks, unbegrenzte Layer | gebaut, 52 Tests grün, Abnahme am Instrument offen |
| 3 | Partitur und Runner | offen |
| 4 | UI | vorgezogen, siehe unten |
| 5 | Bühnentauglichkeit, MIDI | offen |
| 6 | Effekte | offen |

Der Python- und React-Code des Ableton-Prototypen liegt unangetastet im Repo
(`backend/`, `looper/`, `spike/`, `ui/`). Er wird ersetzt, nicht angebunden.

---

## 1. Warum eine eigene Engine

Der Vorgänger steuerte Ableton Live über AbletonOSC und scheiterte an drei Dingen, die nicht
im eigenen Code lagen: kein verlässliches Zustands-Feedback, fremde Looping-Semantik (eine
Spur spielt genau einen Clip, jeder Overdub braucht eine eigene Spur) und Quantisierung nur
auf Taktebene statt auf Samples.

**Die Prozessgrenze war der eigentliche Fehler.** Jede Entscheidung in diesem Projekt zielt
darauf, sie nicht wieder einzuführen. Deshalb ist alles ein einziger Rust-Prozess: Audio,
Compiler, Server.

## 2. Audio-Backend: ASIO, ohne Rückfallebene

Gemessen in Phase 0 am Focusrite Scarlett 2i2 (Details in `docs/phase0-audio-messung.md`):

| | ASIO | WASAPI |
|---|---:|---:|
| Roundtrip bei Zielpuffer | **17,23 ms** (128 Frames) | 111–141 ms (480 Frames) |
| Streuung über 60 Läufe | **0,000 Samples** | 0–22 Samples |
| Puffergröße wählbar | 16 bis 1024 Frames | 480, fest |
| Gerätestruktur | ein Duplex-Gerät | zwei getrennte Geräte |

**cpal kann kein WASAPI-Exclusive.** Der Share-Mode ist in cpal 0.18.2 an allen drei Stellen
hart auf `AUDCLNT_SHAREMODE_SHARED` verdrahtet, `IAudioClient3` fehlt ganz. WASAPI ist damit
keine Rückfallebene, sondern gar keine Option. Wer kein ASIO-Gerät hat, kann die Software
nicht sinnvoll betreiben.

Formel für die Roundtrip-Latenz, über fünf Puffergrößen validiert:

> **Roundtrip ≈ 3,98 × Puffergröße + 331 Samples**

Vier Puffer (Ein- und Ausgang, jeweils doppelt gepuffert) plus 331 Samples für Wandler und
USB-Transport. Zielwert bleibt **128 Frames**; 64 ist Reserve, aber die Callback-Spitze steigt
dort von 6 % auf 23 % des Budgets.

## 3. Zeitachse: selbst gezählte Frames, niemals cpal-Zeitstempel

**cpal speist `StreamInstant` unter ASIO aus `timeGetTime()`** — einem 32-Bit-Millisekunden-
zähler. In der Messung lieferte das teils null, teils zufällige Kleinwerte, teils exakt 2³²
durch Überlauf. Selbst ohne Überlauf wäre eine Millisekunde Auflösung untauglich, das sind
48 Samples.

Unter WASAPI funktionieren dieselben Zeitstempel einwandfrei — es ist also spezifisch der
ASIO-Pfad.

**Regel: Die musikalische Zeit entsteht ausschließlich aus einem fortlaufenden Sample-Zähler
im Audio-Callback.** Kommandos tragen Zeitstempel auf dieser Achse; der Steuer-Thread schickt
sie beliebig früh, der Audio-Thread führt sie sample-genau aus, auch mitten im Puffer.

Taktgrenzen werden **absolut** berechnet (`(beat_index * samples_per_beat).round()`), niemals
inkrementell aufaddiert. Ein aufaddierender Zähler driftet nachweislich; der Test dazu prüft
1000 Takte in 4/4, 3/4 und 7/8.

## 4. Latenzkompensation

Der volle Roundtrip wird beim **Aufnehmen** herausgerechnet, die Wiedergabe bleibt unangetastet.

> Ein Sample mit Eingangsindex `k` wurde bei musikalischer Position `m = k − R` gespielt.

Der Schreibzeiger wandert also **in die Vergangenheit**, weil das Signal zu spät ankommt.
Merksatz: `k − R`, niemals `k + R`.

Die Wiedergabe wird bewusst **nicht** kompensiert. Loop und Klick verlassen den Ausgang
gemeinsam und erfahren dieselbe Ausgangslatenz, liegen für das Ohr also per Konstruktion
übereinander. Eine zweite Korrektur würde den Loop `R` Samples **vor** den Klick schieben.

Das gilt, solange der Musiker den Klick **aus diesem Interface** hört. Bei einem externen
Monitor stimmt die Rechnung nicht mehr, deshalb ist der Wert justierbar
(`--latency-samples`, Standard 827 für 128 Frames an diesem Gerät).

**Neu messen nach jedem Wechsel von Gerät, Samplerate oder Puffergröße.** Dafür gibt es
`calibrate`: Die Engine nimmt ihren eigenen Klick über ein Loopback-Kabel auf und rechnet die
Abweichung von den Schlaggrenzen aus.

Fallstrick bei `calibrate`: **Die Onset-Erkennung ist pegelabhängig.** Bei −27 dBFS Loop-Peak
zeigte sie 32 Samples Abweichung, bei −18 dBFS nur noch 7,2 — dieselbe Engine, dieselbe
Latenz. Klick laut einpegeln (`--click-gain`), sonst kalibriert man ein Messartefakt ein.
Eine Kreuzkorrelation wäre der pegelunabhängige Weg, ist aber nicht gebaut.

## 5. Threads

```
Steuer-Thread          lock-freie Queues          Audio-Thread (Echtzeit)
Runner, Partitur   ──── Kommandos ──────────►    Mixer, Loops, Klick
Puffer-Allokation  ◄─── Status/Retire ──────     Sample-Position
```

**Im Audio-Callback niemals:** allozieren, sperren, loggen, formatieren, Dateien anfassen.
Nur Atomics, vorab allozierte Puffer und lock-freie Queues (`rtrb`).

Puffer werden im **Steuer-Thread** alloziert und genullt, wandern über eine Install-Queue in
den Audio-Thread (der einen Vorrat hält) und über eine Retire-Queue zurück. Entfernte Layer
werden nicht im Callback freigegeben.

## 6. Datenmodell

- **Track**: eigener Eingangskanal, eigenes Mithören, beliebig viele Layer. **Mono** — Gitarre
  und Stimme brauchen kein Stereo, das spart Speicher und Rechenzeit.
- **Layer**: `Vec<f32>` in Loop-Länge. Overdub legt einen weiteren an, alle werden summiert.
- **Ausrichtung**: Jeder Track hält genau zwei Zahlen, `origin` und `loop_len`. Jeder Layer
  wird als `index = (Position − origin) mod loop_len` adressiert. Damit ist Loop-Index *n* in
  jedem Layer derselbe musikalische Moment — **es gibt keinen Offset je Layer, der driften
  könnte.** Bewiesen durch einen Test mit Layern aus Takt 5, 14 und 23, Toleranz null Samples.
- Obergrenzen: 16 Layer je Track, 8 Tracks, mit Meldung statt Absturz.

**Bekannter Drift:** Die Loop-Länge ist eine feste Sample-Zahl, das ideale Taktraster im
Allgemeinen gebrochen. Bei 100 BPM entsteht kein Fehler (ein Schlag ist exakt 28.800 Samples),
bei 137 BPM 0,365 Samples pro Durchlauf — rund 3,9 ms pro Stunde. **Layer driften nicht
gegeneinander, nur gegen den Klick.** Als vernachlässigbar eingestuft.

## 7. UI: Tauri, nicht Browser

Vorgezogen aus Phase 4, weil die Bedienung ab Phase 2 im Weg ist (mit einer Gitarre in der
Hand tippt man keine Tastenkombinationen) und weil ein späterer Wechsel doppelte Arbeit wäre.

**Warum Tauri und nicht axum plus Browser:** Die Audio-Engine läuft im selben Prozess wie der
Server — keine Prozessgrenze, also nicht der Fehler des Ableton-Wegs in neuer Form. Dazu ein
Doppelklick statt „Server starten, Browser öffnen, Port merken", was für die Bühne zählt.

### Spike-Ergebnis (Tauri 2.11.4, verifiziert)

Die offene Frage war, ob ASIO seinen Treiber über COM laden kann, während Tauri COM für
WebView2 initialisiert. **Antwort: ja, ohne Konflikt.**

| Thread | COM-Apartment |
|---|---|
| Tauri-Hauptthread | MAINSTA (von WebView2) |
| Audio-Thread vor cpal | implicit MTA |
| Audio-Thread nach cpal | STA |

Apartments gelten pro Thread, sie kollidieren nicht. cpal initialisiert COM `thread_local` als
STA und toleriert `RPC_E_CHANGED_MODE` — ausdrücklich wegen ASIO, siehe den Kommentar in
`cpal/src/host/com.rs`.

Klick über ASIO bei 128 Frames, 30 Sekunden, während die Oberfläche unter Volllast rendert:

```
observed_frames: 128    callbacks: 11245    xruns: 0
max_callback_us:  28    von budget_us: 2666      (1 % Auslastung)
```

WebView2 rendert in eigenen Prozessen und kann den Audio-Thread nicht blockieren, es
konkurriert nur um CPU-Zeit.

### Regeln, die daraus folgen

1. **Die Audio-Engine läuft auf einem eigenen Thread**, nie auf dem Tauri-Hauptthread. Der ist
   MAINSTA und hängt an der Windows-Message-Pump; ein blockierender Aufruf dort friert das
   Fenster ein.
2. **cpals `Stream` ist `!Send`** und darf den Audio-Thread nie verlassen. Der Audio-Thread
   besitzt die Engine dauerhaft; Tauri schickt Kommandos hinein und bekommt Status heraus.
3. **Tauri-Kommandos sind `async`** und lagern blockierende Arbeit über `spawn_blocking` aus.
4. **Status wird gepusht** (`app.emit`), nicht per Polling abgefragt.
5. **Niemals prozessweit `CoInitializeEx(MTA)`** aufrufen, bevor Tauri hochkommt — das könnte
   ASIOs STA-Wunsch brechen.
6. `stdout` ist im Release-Build weg (`windows_subsystem = "windows"`). Diagnose über Events
   oder eine Logdatei, nicht über `println!`.
7. `tauri-build` verlangt zwingend `icons/icon.ico`, auch bei deaktiviertem Bundle.

### Anbindung des vorhandenen Frontends

`ui/` ist in gutem Zustand (Vite 5, React 18, CodeMirror 6, `tsc --noEmit` läuft sauber) und
überraschend engine-agnostisch. Ableton klebt nur an zwei Stellen: einem hart verdrahteten
Engine-Dropdown in `Transport.tsx` und den optionalen Feldern `pool`/`groups`, die das UI
bereits null-sicher behandelt.

**Die gesamte Server-Kommunikation steckt in zwei Dateien:** `ui/src/api.ts` (acht HTTP-Routen)
und `ui/src/useWebSocket.ts` (Status-Stream). Die Umstellung auf Tauris `invoke` und `emit`
fasst nur diese beiden an, die Komponenten bleiben unberührt.

Offener Widerspruch: `docs/contracts-v0.md` verspricht `beats_per_bar` und `time_signature` im
`state`-Event, das UI liest sie dort nie und holt die Taktart aus der kompilierten Partitur.
Beim Festlegen der Statusmeldungen entscheiden, welche Seite recht behält.

## 8. Was ausdrücklich nicht gemacht wird

- **VST-Hosting.** Kein Ziel dieses Plans.
- **WASAPI als Rückfallebene.** Siehe Abschnitt 2.
- **Anbindung des Python-Codes.** Er wird ersetzt. Prozessgrenzen waren das Problem.
- **Stereo-Tracks.** Mono ist die Entscheidung.
- **Effekte vor Phase 6.** Aufgenommen wird trocken.

## 9. Arbeitsweise

- Henry bedient Hardware selbst; Audio-Tests laufen nur mit seiner Zustimmung.
- **Nie gleichzeitig an derselben Sache arbeiten.** Das hat schon Schaden angerichtet.
- **Bei Audio-Werten nicht raten, messen.** Besonders bei Latenz.
- Code und Kommentare Englisch, Nutzerausgaben Deutsch, Commits Deutsch.
- Offline beweisbare Eigenschaften werden offline bewiesen, bevor sie am Instrument geprüft
  werden. Zwei Millisekunden Versatz hört man beim Spielen nicht, aber sie ruinieren jeden
  Overdub.
