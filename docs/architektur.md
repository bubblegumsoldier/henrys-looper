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

## 0. Die Vision (präzisiert 2026-09-06)

**Ein digitaler Looper mit Tonstudio-Standard.** Das Ziel ist die Aufnahme hochwertig
produzierter Musik *on the spot* — in einer Qualität, die neben Radio und Spotify bestehen
kann. Die Live-Performance ist ein willkommenes Add-on, nicht der Zweck.

Diese Präzisierung ist folgenreich, weil sie mehrere frühere Entscheidungen umdreht:

| Punkt | vorher | jetzt |
|---|---|---|
| Tracks | mono, „Gitarre und Stimme brauchen kein Stereo" | **Stereo ist Pflicht — gebaut.** Loop-Puffer sind mono *oder* stereo, je nach Quelle; Effektkette, Panorama und Mixbus sind immer stereo. Siehe Abschnitt 6 |
| VST-Hosting | ausdrücklich kein Ziel | **zentral.** Ohne fremde Instrumente und Effekte gibt es keinen Studio-Standard |
| Stems | „kein eigenes Export-Feature in v1" | **Pflicht.** Wer produziert, muss aus dem Programm herauskommen |
| Ausgänge | ein Ausgang für alles | **getrennte Busse**, Klick nur auf den Monitorweg |
| Maßstab | „klingt gut genug für die Bühne" | **Es muss aufnahmetauglich sein.** Der Vergleich ist eine DAW, nicht ein Hardware-Looper |

Der Unterschied in einem Satz: Ein Bühnen-Looper darf Kompromisse machen, die man live nicht
hört. Ein Aufnahmewerkzeug darf das nicht — was einmal in der Datei steht, hört man später
auf guten Boxen.

Was **bleibt**: die Partitur als Alleinstellungsmerkmal. Vorher aufschreiben, was passieren
soll, und live nur noch auslösen. Plugin-Hosting kann jede DAW; ein Looper, der eine
geschriebene Partitur abarbeitet, ist neu. Das ist der eigentliche Wert des Projekts, alles
andere ist die Eintrittskarte.

## Stand

| Phase | Inhalt | Status |
|---|---|---|
| 0 | Audio-Nachweis unter Windows | **abgenommen**, am Instrument geprüft |
| 1 | Loop-Kern, ein Track | **abgenommen**, Gitarre eingespielt, Kalibrierung bestätigt |
| 2 | Mehrere Tracks, unbegrenzte Layer | gebaut, 52 Tests grün, Abnahme am Instrument offen |
| 3 | Partitur und Runner | offen |
| 4 | UI | vorgezogen, siehe unten |
| 5 | Bühnentauglichkeit, MIDI | offen |
| 6 | Effekte | DSP, Kommandos und CLI gebaut, Abnahme am Instrument offen. UI folgt |
| 7 | Stereo | **gebaut**, 233 Tests grün, Abnahme am Instrument offen. Aufnahme mono oder stereo je Quelle, Kette und Mixbus stereo, Panorama je Track |

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

### Quantisierung: Loop-Grenze statt Taktgrenze

Aufnahme (`r`) und Overdub (`o`) rasten standardmäßig auf die **nächste Loop-Grenze** ein, nicht
mehr auf die nächste Taktgrenze. Bei acht Takten Loop-Länge hieß Taktquantisierung: bis 8/8 warten,
dann hetzen. Loop-Quantisierung heißt: einmal früh drücken, dann in Ruhe zum Einsatz kommen. Der
Modus ist umschaltbar (`--quantize bar|loop`, `StartConfig.quantize`, Kommando `set_quantize`).

**Stopp und Wiedergabe bleiben auf der Taktgrenze.** Das sind Korrekturen, keine Takes — ein Stopp,
der acht Takte wartet, wirkt kaputt. Die Wiedergabe verliert dabei nichts, weil ein Loop als
`(Position − origin) mod loop_len` gelesen wird und deshalb phasenrichtig weiterläuft, egal wann man
sie einschaltet.

**Welches Raster gilt:**

| Track | Raster | Warum |
|---|---|---|
| hat schon einen Loop | `origin + n · loop_len` | Das ist die Geometrie, in der die Ebenen adressiert werden. Taktgrenzen werden einzeln gerundet und treffen sie im Allgemeinen nicht. |
| ist leer | Takt 0, `bars`, `2·bars` … ab Engine-Start | Es gibt noch kein `origin`. Dasselbe Raster, das der Klick seit dem Start markiert — deshalb passen zwei Tracks zusammen, die Minuten auseinander aufgenommen wurden. |

Ein *neuer* Loop (`r`) nimmt immer das globale Raster, auch auf einem belegten Track: er wirft die
alte Geometrie weg und definiert ein neues `origin`.

Die ganze Rechnung steht in `engine/src/engine/schedule.rs`, mit Modulkommentar und Tests.
**Dieses Modul ist die einzige Stelle, an der aus einer Bedienhandlung getimte Kommandos werden** —
CLI und Tauri-App teilen es sich. Vorher stand es zweimal da, einmal je Frontend.

### Sichtbarer Vorlauf

Der Zustand „scharf" sagt nicht, wann. Der Scheduler merkt sich je Track, was ansteht und wann, und
das Status-Event trägt es als `pending_kind`, `pending_label`, `pending_bars`, `pending_beats`. Die
Track-Karte zeigt es groß („Aufnahme in 5 Takten"), im letzten Takt in Schlägen und rot blinkend;
die CLI hängt denselben Text an den Zustand.

## 4. Latenzkompensation

Der volle Roundtrip wird beim **Aufnehmen** herausgerechnet, die Wiedergabe bleibt unangetastet.

> Ein Frame mit Eingangsindex `k` wurde bei musikalischer Position `m = k − R` gespielt.

Der Schreibzeiger wandert also **in die Vergangenheit**, weil das Signal zu spät ankommt.
Merksatz: `k − R`, niemals `k + R`.

**`R` zählt Frames, nicht Samples.** Das war vor dem Stereo-Umbau dasselbe und ist es seither
nicht mehr. Ein Frame ist ein Zeitpunkt: ein Sample in einem Mono-Puffer, zwei in einem
Stereo-Puffer. `R` wurde als *Zeit* gemessen, ist also eine Frame-Zahl; die beiden Kanäle einer
Stereoquelle durchlaufen denselben Wandler im selben Moment und landen deshalb bei demselben `m`.
Würde man `R` als Sample-Zahl lesen, verschöbe sich eine Stereo-Aufnahme um den halben Wert — und
zwar um einen *ungeraden* Versatz, der die beiden Kanäle vertauscht. Die Engine multipliziert eine
Position an genau zwei Stellen mit der Kanalzahl, beide in `track.rs` (`Track::read` und
`Track::write`), und sonst nirgends.

Die Wiedergabe wird bewusst **nicht** kompensiert. Loop und Klick verlassen den Ausgang
gemeinsam und erfahren dieselbe Ausgangslatenz, liegen für das Ohr also per Konstruktion
übereinander. Eine zweite Korrektur würde den Loop `R` Samples **vor** den Klick schieben.

Das gilt, solange der Musiker den Klick **aus diesem Interface** hört. Bei einem externen
Monitor stimmt die Rechnung nicht mehr, deshalb ist der Wert justierbar
(`--latency-samples`, Standard 827 für 128 Frames an diesem Gerät).

**Neu messen nach jedem Wechsel von Gerät, Samplerate oder Puffergröße.** Dafür gibt es
`calibrate`: Die Engine nimmt ihren eigenen Klick über ein Loopback-Kabel auf und rechnet die
Abweichung von den Schlaggrenzen aus.

**Offen: Kompensation pro Track statt global.** Der Wert gilt derzeit für alle Tracks
gemeinsam. Das stimmt, solange jedes Signal denselben Weg nimmt — Mikrofon oder Instrument
durch den AD-Wandler desselben Interfaces. Sobald ein Plugin-Host wie Cantabile über einen
ASIO-Router danebenläuft, ist das falsch: Dessen Audio ist bereits digital und durchläuft
keinen Wandler, hat also eine **kürzere** Eingangslatenz als das Mikrofon. Mit einem globalen
Wert sitzt dann eine der beiden Quellen dauerhaft daneben — unhörbar, bis man die Spuren
übereinanderlegt. Der Wert muss von global auf **pro Track** wandern, bevor mit externen
Klangerzeugern ernsthaft aufgenommen wird.

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

### Frames, nicht Samples

Ein **Frame** ist ein Zeitpunkt: ein Sample in einem Mono-Puffer, zwei in einem Stereo-Puffer. Ein
**Sample** ist eine einzelne Zahl in einem Kanal. `origin`, `loop_len`, `filled`, jede Take-Grenze
und die Latenzkompensation `m = k − R` zählen **Frames**. Die Kanalzahl kommt an genau zwei Stellen
ins Spiel, `Track::read` und `Track::write` in `engine/src/engine/track.rs`:

```text
Offset im Puffer = Frame-Index * Kanalzahl + Kanal
```

Sonst nirgends. Das ist der Grund, warum die ganze Loop-Geometrie unten für einen Mono- und einen
Stereo-Track wörtlich dieselbe ist — und es ist der eine Fehler, den ein Stereo-Umbau macht, wenn
er einen macht.

### Die Entscheidung: Puffer folgen der Quelle, alles danach ist stereo

- **Track**: eigener Eingangskanal *oder ein Kanalpaar*, eigenes Mithören, eigenes Panorama,
  beliebig viele Layer.
  - **Ein** Eingang (Mikrofon, Gitarre) → **Mono-Puffer**. Ihn in einen Stereopuffer zu schreiben
    verdoppelt den Speicher, ohne einen Ton hinzuzufügen.
  - **Zwei** Eingänge (Klavier, Fläche aus einem Plugin-Host) → **Stereo-Puffer**, interleaved.
    Welche zwei Eingänge zusammengehören, ist eine Verkabelungsfrage und wird nie geraten.
- **Ab dem Puffer ist alles stereo, ohne Ausnahme**: Effektkette, Panorama, Mixbus, Ausgang. Ein
  Mono-Track wird auf dem Weg in die Kette auf beide Kanäle aufgefächert — schon weil ein Hall auf
  einer Mono-Gitarre stereo sein muss, sonst bleibt die Gitarre ein Punkt in der Kopfmitte, wie
  groß der Raum auch eingestellt ist. Das entspricht der Arbeitsweise einer DAW: mono aufnehmen,
  stereo bearbeiten.
- **Signalweg je Track**:
  ```text
  Ebenen-Summe + Mithören ─► (mono: auf beide Kanäle) ─► Kette ─► Panorama ─► Mixbus (L/R)
  ```
  Der Panner sitzt **nach** der Kette, wie in einem DAW-Kanalzug: der Hall entsteht mittig und wird
  dann platziert, statt ein schon einseitiges Signal zu bekommen. Der Klick geht an Kette und
  Panner vorbei und mit gleichem Pegel in beide Buskanäle — er ist Referenz, kein Teil des
  Arrangements.
- **Layer**: interleavter `Vec<f32>` in Loop-Länge mal Kanalzahl. Overdub legt einen weiteren an,
  alle werden summiert. Der Puffer-Pool im Steuer-Thread hält **zwei Vorräte**, einen je Kanalzahl;
  eine Session aus lauter Mono-Tracks legt nie einen Stereo-Puffer an.
- **Ausrichtung**: Jeder Track hält genau zwei Zahlen, `origin` und `loop_len`. Jeder Layer
  wird als `index = (Position − origin) mod loop_len` adressiert. Damit ist Loop-Index *n* in
  jedem Layer derselbe musikalische Moment — **es gibt keinen Offset je Layer, der driften
  könnte.** Bewiesen durch einen Test mit Layern aus Takt 5, 14 und 23, Toleranz null Frames,
  seit dem Umbau zusätzlich in der Stereo-Fassung mit unterschiedlichem Material links und rechts.
- Obergrenzen: 16 Layer je Track, 8 Tracks, mit Meldung statt Absturz.

### Das Panoramagesetz: Mitte ist Einheitsverstärkung

`pan` läuft von −1 (ganz links) über 0 (Mitte) bis +1 (ganz rechts). In der Mitte passieren **beide
Seiten mit Faktor 1,0**; beim Drehen wird die Gegenseite linear bis auf exakt null heruntergezogen.
Das ist das „0-dB-Gesetz" und eine bewusste Entscheidung gegen das übliche Gesetz konstanter
Leistung (−3 dB in der Mitte):

- **Der Bypass bleibt bitgleich.** Abschnitt 9 verspricht, dass eine umgangene Effektkette das
  Signal unverändert durchreicht — das ist der Panikschalter auf der Bühne. Eine Mittenverstärkung
  von 0,70710678 macht daraus „fast unverändert".
- **Mono- und Stereo-Tracks folgen derselben Regel.** Bei einem Stereo-Track ist der Regler eine
  *Balance*, und eine Balance muss in der Mitte Einheitsverstärkung haben, sonst säße jede
  Stereoquelle 3 dB unter jeder Monoquelle. Mit dem Leistungsgesetz bräuchte man zwei verschiedene
  Gesetze und der Musiker müsste wissen, welches gerade gilt.
- **Hier pant nichts automatisch.** Der 3-dB-Einbruch, den das 0-dB-Gesetz beim Schwenken von hart
  nach Mitte erzeugt, hört man, wenn ein Regler während eines Songs fährt. In diesem Looper wird ein
  Track einmal platziert, bevor gegen ihn gespielt wird.

Der Preis, offen gesagt: ein hart gepannter Mono-Track ist in seinem Lautsprecher so laut wie
vorher in beiden, die *Summe* verliert also 3 dB, wenn man ihn aus der Mitte nimmt. Das korrigiert
man mit dem Ebenen-Regler, den jeder Track ohnehin hat.

### Pegel

Jeder Pegel existiert in zwei Formen, und beide wandern über den Status: das **Maximum beider
Kanäle** (eine Zahl, für einen Balken und für die Frage „übersteuert es") und **ein Wert je Kanal**
(für eine echte Stereoanzeige). Der Ausgangspegel eines Tracks wird **nach dem Panner, aber vor der
Effektkette** gemessen: die Kette bleibt draußen, damit die Anzeige sagt, wie laut die *Aufnahme*
ist und nicht, wie viel Makeup-Gain das Preset zugibt; der Panner ist drin, weil ein hart gepannter
Track, der auf beiden Anzeigen gleich viel zeigt, schlicht lügt.

**Bekannter Drift:** Die Loop-Länge ist eine feste Frame-Zahl, das ideale Taktraster im
Allgemeinen gebrochen. Bei 100 BPM entsteht kein Fehler (ein Schlag ist exakt 28.800 Frames),
bei 137 BPM 0,365 Frames pro Durchlauf — rund 3,9 ms pro Stunde. **Layer driften nicht
gegeneinander, nur gegen den Klick.** Als vernachlässigbar eingestuft.

### Wo die Kanalzahl konfiguriert wird

| Ort | mono | stereo |
|---|---|---|
| CLI | `--track stimme:1` | `--track klavier:3-4` (optional `@-0.4` fürs Panorama) |
| Partitur (`engine/src/score/`) | `stimme: {input: 1}` | `klavier: {input: [3, 4], pan: 0.2}` |
| App (`StartConfig.tracks`) | `{"input_channel": 1}` | `{"input_channel": 3, "input_channel_right": 4}` |

Die beiden Hälften eines Paares müssen **nicht** nebeneinander liegen: ein ASIO-Router legt einen
Stereo-Return dorthin, wohin er will. Das Panorama ist überall optional und steht ohne Angabe in
der Mitte; zur Laufzeit ändert es die Taste `n <wert>` in der CLI beziehungsweise der Regler auf
der Track-Karte.

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
8. **Die Pfade in `tauri.conf.json` haben zwei verschiedene Bezugspunkte.** `frontendDist` wird
   relativ zur Konfigurationsdatei aufgelöst, also ab `app/src-tauri/` — dort ist `../../ui/dist`
   richtig. Die Befehle `beforeDevCommand` und `beforeBuildCommand` laufen dagegen aus `app/`,
   eine Ebene höher, dort ist `--prefix ../ui` richtig. Beide Angaben stehen im selben Block
   und sehen deshalb inkonsistent aus, sind es aber nicht. Verifiziert mit `cargo tauri dev`
   und `cargo tauri build --no-bundle`. Wer das „korrigiert", bricht den Start mit
   `ENOENT ... repos\ui\package.json`.

### Anbindung des vorhandenen Frontends

`ui/` ist in gutem Zustand (Vite 5, React 18, CodeMirror 6, `tsc --noEmit` läuft sauber) und
überraschend engine-agnostisch. Ableton klebt nur an zwei Stellen: einem hart verdrahteten
Engine-Dropdown in `Transport.tsx` und den optionalen Feldern `pool`/`groups`, die das UI
bereits null-sicher behandelt.

**Die gesamte Server-Kommunikation steckt in zwei Dateien:** `ui/src/api.ts` (acht HTTP-Routen)
und `ui/src/useWebSocket.ts` (Status-Stream). Die Umstellung auf Tauris `invoke` und `emit`
fasst nur diese beiden an, die Komponenten bleiben unberührt.

Der frühere Widerspruch um `beats_per_bar` und `time_signature` im Status ist entschieden:
Das Status-Event führt `beats_per_bar` und `beat_unit` als **Zahlen** (kein `"3/4"`-String),
und die Live-Ansicht liest die Taktart von dort statt aus der kompilierten Partitur.

## 8. Was ausdrücklich nicht gemacht wird

- **WASAPI als Rückfallebene.** Siehe Abschnitt 2.
- **Anbindung des Python-Codes.** Er wird ersetzt. Prozessgrenzen waren das Problem.
- **Effekte in die Aufnahme rechnen.** Aufgenommen wird trocken; Effekte sitzen auf der
  Wiedergabe und dem Mithörweg. Was eingebrannt ist, bekommt man nie wieder heraus.

**Nicht mehr ausgeschlossen** (siehe Abschnitt 0): Stereo-Tracks — inzwischen gebaut, siehe
Abschnitt 6 —, Stem-Export und **VST-Hosting**. Bei letzterem ist die Formatfrage offen: **CLAP** wäre
technisch der ruhigere Weg — C-API statt COM-artigem C++, MIT-Lizenz, ausdrücklich
spezifiziertes Threading-Modell. Nur gibt es die Bibliotheken, um die es geht (Kontakt,
Superior Drummer), dort nicht; Toontrack listet VST, AU und AAX, Native Instruments hat CLAP
nie angekündigt. Damit läuft es auf **VST3** hinaus. Zu bedenken bleibt: Ein fremdes Plugin
darf im Audio-Callback allozieren, sperren und abstürzen. Die Echtzeit-Garantien dieses
Projekts enden an dieser Grenze, und ohne Prozesstrennung reißt ein Absturz die ganze Engine
mit — samt laufender Aufnahme.

## 9. Effekte (Phase 6)

Der DSP liegt in `engine/src/engine/fx/`, ein Modul je Effekt plus `mod.rs` mit der Kette. Keine
neue Abhängigkeit: Biquads, Kompressor, Delay und ein Freeverb-artiger Hall sind selbst
geschrieben, weil wir damit Allokation und Denormals kontrollieren.

### Wo die Kette hängt

```
Eingang ─┬───────────────────────────────────────────────────► Loop-Puffer   (trocken!)
         └─► Mithören ─┐
                       ├─► Effektkette (stereo) ─► Panorama ─┐
Ebenen-Summe ──────────┘                                     ├─► Mixbus L/R ─► Ausgang
Klick ───────────────────────────────────────────────────────┘   (mittig)
```

**Aufgenommen wird trocken, ohne Ausnahme.** `EngineCore::record_input` liest das rohe
Eingangs-Slice und sieht die Kette nicht einmal. Ein Effekt, der in einer Aufnahme steckt, ist
nicht mehr herauszubekommen; einer auf der Wiedergabe lässt sich zwischen zwei Loop-Durchläufen
neu einstellen. Die Tests `the_recording_stays_dry_with_the_whole_chain_turned_up` und
`a_stereo_recording_stays_dry_with_the_whole_chain_turned_up` halten das für beide Kanalzahlen
fest.

### Die Kette ist stereo, der Track nicht unbedingt

Ein Loop-Puffer ist mono oder stereo (Abschnitt 6), alles danach ist stereo. Zwei Effekte wären
dabei falsch, wenn man sie einfach zweimal laufen ließe, und beide sind es nicht:

- **Der Kompressor hat einen Detektor und eine Verstärkung für beide Kanäle.** Der Detektor sieht
  den lauteren der beiden. Zwei unabhängige Kompressoren würden bei jedem Pegelsprung die lautere
  Seite herunterziehen und die andere stehen lassen — was man dann hört, ist nicht Kompression,
  sondern ein Instrument, das über die Bühne wandert.
- **Der Hall regt beide Bänke mit demselben Signal an**, `(L + R) · 0,5`. Ein Raum trennt die
  linke und die rechte Hälfte dessen nicht, was in ihm gespielt wird; die 0,5 sorgt dafür, dass ein
  aufgefächertes Mono-Signal den Raum genauso laut anregt wie vor dem Umbau.

Stereo wird der Hall durch **versetzte Kammlängen**: die rechte Bank ist dieselbe Filterbank mit
jeder Verzögerung um 23 Samples verlängert — Freeverbs eigenes `stereospread`. Damit haben die
beiden Kanäle unkorrelierte Echomuster, und unkorreliertes Rauschen auf beiden Ohren ist genau das,
was das Gehirn als „Raum um mich herum" liest; identisches Rauschen liest es als Punktquelle in der
Kopfmitte. Der Versatz ist mit einer halben Millisekunde klein genug, dass am *trockenen* Signal
nichts zur Seite gezogen wird.

Das Delay hat **eine Leitung je Kanal mit gemeinsamem Tap**: eine gemeinsame Leitung würde jede
Wiederholung in die Mitte holen, zwei getrennte Taps wären ein Chorus statt eines tempo-synchronen
Delays.

**Mithören läuft durch dieselbe Ketteninstanz wie die Wiedergabe.** Wer mit Hall singt, singt
anders; und zwei parallele Ketten würden beim ersten Parameterwechsel auseinanderlaufen. Deshalb
werden Ebenensumme und Mithörsignal *vor* der Kette summiert (`Track::render`). Der Klick bleibt
außen vor - er ist Referenz, keine Musik.

### Reihenfolge und Presets

Hochpass, drei Bänder parametrischer EQ, Kompressor, Delay, Hall - die Reihenfolge aus dem Plan.
Das oberste EQ-Band ist als Kuhschwanz ausgelegt: "Luft" ist das ganze obere Ende, nicht eine
Frequenz. Der Kompressor hat ein weiches Knie (6 bis 8 dB), weil eine Stimme *an* der Schwelle
lebt und ein hartes Knie mehrmals pro Sekunde umschalten würde.

Drei Presets, jeder Wert im Code begründet: `stimme` (80 Hz Hochpass, Präsenz +2,5 dB bei 3 kHz,
3:1 ab -18 dBFS mit +6 dB Makeup, 18 % Hall), `gitarre` (100 Hz Hochpass, -4 dB Quäkband bei
3 kHz, 2,5:1 mit langsamem Attack, 10 % Hall), `trocken` (alles aus, Kette umgangen).

**Ein frischer Track ist umgangen und damit bitgleich durchgereicht** - deshalb hat sich für die
123 Tests aus Phase 2 nichts geändert, und deshalb hat der Stereo-Umbau sie ebenfalls nicht
angefasst: ein Mono-Track in der Mitte liegt mit Faktor 1,0 auf beiden Buskanälen, also steht auf
jeder Seite dasselbe wie vorher im Mono-Ausgang.

### Delay: tempo-synchron, nicht in Millisekunden

Die Verzögerung kommt aus `Timeline::samples_per_quarter()`, also aus derselben Achse, die auch
die Loops ausrichtet - bei 120 BPM ist eine Viertel exakt 24 000 Frames, nicht "etwa 500 ms". Das
ist der musikalische Ertrag der eigenen Engine. Jede der beiden Leitungen ist auf **2 Sekunden**
ausgelegt (eine Viertel bei 30 BPM), zusammen 768 kB je Track. Ein Tempowechsel blendet den alten
auf den neuen Tap über (20 ms) statt zu springen.

### Echtzeit

Alle Puffer entstehen in `Chain::new`, also im Steuer-Thread beim Bau des Tracks - rund 880 kB je
Track (zwei Delay-Leitungen plus zwei Hall-Bänke). Im Callback wird nichts alloziert, gesperrt oder
geloggt.
Parameter kommen als `Copy`-Kommandos durch die vorhandene Queue; die Koeffizientenrechnung
(`sin`, `cos`, `powf`) läuft einmal je Kommando, nie je Sample.

**Denormals** werden zweifach behandelt: jeder Rückkopplungsspeicher läuft durch `flush` (alles
unter 1e-30 wird echt null), und die Kette hört nach Ablauf ihres längsten Schwanzes ganz auf zu
rechnen, solange der Eingang exakt null ist. Ein stiller Hall ist sonst der teuerste Zustand, den
ein Hall haben kann.

### Gemessene Rechenlast

`looper-engine fxbench --seconds 6`, Release-Build, 48 kHz, Budget 128 Frames = 2,667 ms.
Neu gemessen nach dem Stereo-Umbau, Median aus fünf Läufen. Die Zahlen sind **ns je Frame**, also
je Zeitpunkt — dieselbe Einheit, in der das Callback-Budget abgerechnet wird, und damit direkt mit
der Mono-Messung vergleichbar, die sie ersetzen:

| | ns/Frame | % je Track | % bei 8 Tracks | vorher (mono) |
|---|---:|---:|---:|---:|
| Hochpass | 9,3 | 0,04 | 0,36 | 4,9 |
| EQ (3 Bänder) | 24,0 | 0,12 | 0,92 | 12,4 |
| Kompressor | 35,5 | 0,17 | 1,36 | 15,6 |
| Delay | 8,4 | 0,04 | 0,32 | 4,7 |
| Hall | 50,0 | 0,24 | 1,92 | 22,7 |
| Kette „stimme" | 133,8 | 0,64 | 5,14 | – |
| **Kette komplett** | **140,0** | **0,67** | **5,37** | **84,1** |
| Kette still (Leerlauf) | 3,2 | 0,02 | 0,12 | 2,0 |
| Kette umgangen | 3,7 | 0,02 | 0,14 | 2,3 |

Acht volle Ketten kosten jetzt rund **5,4 % des Callback-Budgets** statt 3,2 %. Der Faktor ist
**1,66**, nicht 2: Kompressor und Hall rechnen ihren teuersten Teil weiterhin nur einmal (ein
Detektor, eine Hall-Anregung), nur die Filter und die Verzögerungsleitungen liegen doppelt.
Ein globaler Hall-Bus würde davon 1,7 Prozentpunkte sparen und bleibt damit **nicht nötig** - er
wurde weiterhin bewusst nicht gebaut. Die beiden billigen Zustände sind billig geblieben: sieben
von acht Tracks sind meistens still und kosten zusammen 0,12 %.

## 10. Arbeitsweise

- Henry bedient Hardware selbst; Audio-Tests laufen nur mit seiner Zustimmung.
- **Nie gleichzeitig an derselben Sache arbeiten.** Das hat schon Schaden angerichtet.
- **Bei Audio-Werten nicht raten, messen.** Besonders bei Latenz.
- Code und Kommentare Englisch, Nutzerausgaben Deutsch, Commits Deutsch.
- Offline beweisbare Eigenschaften werden offline bewiesen, bevor sie am Instrument geprüft
  werden. Zwei Millisekunden Versatz hört man beim Spielen nicht, aber sie ruinieren jeden
  Overdub.
