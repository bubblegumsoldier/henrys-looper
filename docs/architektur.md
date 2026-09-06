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
| `docs/contracts-v0.md` | Partitur-Format und Event-Formate. Konzeptionell gültig, Ableton-Teile veraltet. Das dort festgelegte MIDI-Id-Format `ch1.note36` gilt unverändert; der Umfang der Bindungen ist seit Abschnitt 11 viel größer |
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
| 3 | Partitur und Runner | **gebaut**, 273 Tests grün, Abnahme am Instrument offen. Compiler und Runner stehen, `score`-Subcommand bedient beides. Siehe Abschnitt 10 |
| 4 | UI | vorgezogen, siehe unten |
| 5 | Bühnentauglichkeit, MIDI | **gebaut**, 344 Tests grün, ohne Gerät geprüft. MIDI-Eingang (`midir`), Parameterbaum, zweistufiges Mapping (Controller-Profil + Partitur), Wertesprung-Schutz, `midi`-Subkommando — und in der App: Gerät verbinden, eingehende Ereignisse auf die laufende Sitzung, Learn durch Anklicken des Bedienelements, Monitor, Belegungsübersicht. Abnahme mit Controller offen. Siehe Abschnitt 11 |
| 6 | Effekte | DSP, Kommandos und CLI gebaut, Abnahme am Instrument offen. UI folgt |
| 7 | Stereo | **gebaut**, Abnahme am Instrument offen. Aufnahme mono oder stereo je Quelle, Kette und Mixbus stereo, Panorama je Track |
| 8 | Latenzkompensation je Track | **gebaut**, 252 Tests grün, Abnahme am Instrument offen. Globaler Wert als Vorgabe, eigener Wert je Track aus Messwert plus Zuschlag, `calibrate --for-track`. Siehe Abschnitt 4 |

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
Monitor stimmt die Rechnung nicht mehr, deshalb ist der Wert justierbar.

### `R` gehört zum Track, nicht zur Engine (gebaut)

Die Herleitung oben verfolgt **ein** Signal auf **einem** Weg herein. Zwei Quellen müssen sich
diesen Weg nicht teilen: Mikrofon und Gitarre tun es (derselbe AD-Wandler desselben Interfaces),
ein Plugin-Host wie Cantabile über einen ASIO-Router nicht. Dessen Audio ist bereits digital,
durchläuft keinen Wandler und kommt deshalb **früher** an. Mit einem globalen Wert sitzt dann eine
der beiden Quellen dauerhaft daneben — unhörbar, bis man die Spuren übereinanderlegt.

Deshalb hat **jeder Track seinen eigenen Wert**, und der globale ist nur noch die **Vorgabe** für
alle, die nichts eigenes sagen. Die allermeisten Setups haben eine einzige Quelle; dort pflegt
niemand acht Zahlen.

**Der Track-Wert besteht aus zwei Zahlen, und das ist keine Bequemlichkeit:**

| Anteil | woher | wer schreibt ihn |
|---|---|---|
| `measured` | Loopback-Messung dieses Eingangs, in Frames. Leer = globale Vorgabe | `calibrate` |
| `trim` | manueller Zuschlag, vorzeichenbehaftet | der Mensch, nach Gehör |
| **wirksam** | `measured (oder Vorgabe) + trim`, nie unter 0 | die Engine |

Der Grund für die Trennung ist die **Grenze der Messung**: Wir messen den Weg von einem Ausgang
dieser Maschine bis zu einem ihrer Eingänge. Was ein externer Host *intern* an Latenz hat — sein
eigener ASIO-Puffer, die von seinen Plugins gemeldete Verzögerung —, liegt nicht auf diesem Weg.
Das Loopback, das ein Router für einen Cantabile-Return bereitstellt, geht am Plugin *vorbei*,
nicht hindurch. Dieser Anteil kann nur von Hand kommen. Läge er im selben Feld wie der Messwert,
würfe ihn die nächste Kalibrierung still weg — und man merkte es Wochen später an einem Take, der
nicht mehr dort sitzt, wo die anderen sitzen. Mit zwei Feldern schreibt `calibrate` das erste, das
Ohr das zweite, und die Summe ist, was die Engine abzieht.

**Wo der Wert konfiguriert wird:**

| Ort | global (Vorgabe) | je Track |
|---|---|---|
| CLI | `--latency-frames 827` | `--track-latency cantabile:512+96` (auch `NAME:+96` für nur Zuschlag) |
| CLI zur Laufzeit | — | Taste `i <frames\|-> [zuschlag]` |
| Partitur | — | `cantabile: {input: [5,6], latency: 512, latency_trim: 96}` |
| App | `StartConfig.latency_frames` | `TrackConfig.latency_frames` / `latency_trim`, Kommando `track_latency` |

Der alte Name `--latency-samples` bleibt als Alias erhalten, und `latency_samples` wird in
`StartConfig` weiter gelesen: Die Zahl zählte immer Frames, nur der Name sagte es nicht.

**Laufzeitänderung wirkt nach vorn, nie rückwärts.** Ein geänderter Wert entscheidet, wohin die
*nächsten* Eingangsframes geschrieben werden. Was schon in einem Ebenen-Puffer steht, wurde mit dem
alten Wert abgelegt und bleibt dort — eine Ebene, die sich verschiebt, weil jemand eine Zahl
korrigiert hat, wäre schlimmer als die falsche Zahl. Ein Test hält beides fest.

Zwei Nebenrechnungen folgen daraus: `check_loop` prüft gegen den **größten** Wert aller Tracks
(`process::max_latency`), nicht gegen die Vorgabe — der Schreibzeiger läuft dem Lesezeiger auf dem
ungünstigsten Track um so viel hinterher. Und `musical_input_pos` ist je Track verschieden, weshalb
ein Kommando, das nicht in der Vergangenheit landen darf, gegen den richtigen geprüft wird.

### Kalibrierung je Eingang

**Neu messen nach jedem Wechsel von Gerät, Samplerate oder Puffergröße.** Dafür gibt es
`calibrate`: Die Engine nimmt ihren eigenen Klick über ein Loopback auf und rechnet die
Abweichung von den Schlaggrenzen aus. Herleitung: `R_wahr = R + Abweichung`.

Gemessen wird **ein Eingang**, nämlich der des mit `--for-track` gewählten Tracks (Standard: der
erste, also das alte Verhalten). Für eine Quelle, die über einen ASIO-Router hereinkommt, braucht
es dazu **kein Kabel** — der Router schleift den Ausgang selbst auf den Eingang zurück. Das
Ergebnis wird als fertige Kommandozeile ausgegeben, jetzt mit `--track-latency NAME:WERT`. Der
Zuschlag bleibt dabei außen vor, und die Ausgabe sagt das auch.

Fallstrick bei `calibrate`: **Die Onset-Erkennung ist pegelabhängig.** Bei −27 dBFS Loop-Peak
zeigte sie 32 Samples Abweichung, bei −18 dBFS nur noch 7,2 — dieselbe Engine, dieselbe
Latenz. Ein Signal, das langsam ansteigt, braucht länger durch eine feste Schwelle, und jedes
Sample dieses Anstiegs landet im Ergebnis. Klick laut einpegeln (`--click-gain`), sonst kalibriert
man ein Messartefakt ein. **Die Messung sagt es inzwischen selbst:** unterhalb von −18 dBFS
Loop-Peak steht die Warnung samt dieser Zahlen in jedem Lauf und noch einmal im Ergebnis.
Eine Kreuzkorrelation wäre der pegelunabhängige Weg, ist aber nicht gebaut.

### Was das UI zeigt

Auf der Track-Karte und im Setup steht je Track der wirksame Wert in Frames und Millisekunden,
daneben die beiden Felder, aus denen er entsteht. Ein Track ohne eigenen Messwert lässt das erste
Feld **leer** (mit der Vorgabe als Platzhalter) und wird als „geerbt (827)" beschriftet — eine Zahl,
die aussieht wie eine Einstellung, aber geerbt ist, war genau der Fehler, gegen den dieses Kapitel
gebaut ist.

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

## 10. Der Runner (Phase 3)

Der Compiler (`engine/src/score/`) macht aus YAML eine statische `CompiledScore`. Der **Runner**
(`engine/src/engine/runner.rs`) spielt sie. Er läuft im Steuer-Thread, kennt kein Terminal und
keine Oberfläche, und alles, was er tut, verlässt ihn als `Command` mit Zeitstempel. Deshalb ist
Phase 3 vollständig offline beweisbar — die 21 Tests in `runner/tests.rs` treiben denselben
`EngineCore` über `sim.rs`, mit dem auch die Sample-Genauigkeit von Phase 1 bewiesen wurde.

### Das deklarative Modell: Soll gegen Ist, und was **kein** Kommando erzeugt

Eine Sektion beschreibt den vollständigen Sollzustand **aller** Tracks, keine Deltas. Der Runner
merkt sich, was er zuletzt von einem Track verlangt hat, und schickt nur die Differenz:

| vorher | jetzt | Kommandos |
|---|---|---|
| egal | `record` | `StartRecord`, `StopRecord`, `StartPlay` — ein Take über die Sektion |
| egal | `overdub` | `StartOverdub`, `StopRecord`, `StartPlay` — eine Ebene, genau ein Loop-Durchlauf |
| klingt schon | `play` | **keins** |
| still, mit Inhalt | `play` | `StartPlay` |
| klingt | `stop` / `hear_through` | `StopPlay` |
| still | `stop` | **keins** |

Ein Track, der über fünf Sektionen auf `play` steht, bekommt **null** Kommandos; einer, der
durchgehend `stop` steht, ebenso. Das ist keine Sparsamkeit, sondern Klangschutz: Jedes
überflüssige Kommando ist ein Zustandswechsel im Audio-Thread an einer musikalisch exponierten
Stelle — die Sorte Fehler, die man erst nach zwanzig Minuten Spielen als Knacksen bemerkt.

**Mithören** folgt demselben Prinzip: an bei `record`, `overdub` und `hear_through`, aus bei `play`
und `stop`, und nur, wenn die Partitur es dem Track überhaupt erlaubt (`monitor:`). Ein Wechsel
wird nur geschickt, wenn er den Zustand wirklich ändert.

### Einzähler

Ein Takt Klick vor der ersten Sektion, konfigurierbar über `--count-in`. Ein Takt ist der kürzeste
Vorlauf, der noch ein *Takt* ist: Der Musiker hört ein vollständiges Muster der Taktart, die er
gleich spielt (in 3/4 bei 141 BPM sind das 1,28 s). Zwei Takte wären Warten, ein halber würde die
Taktart nicht etablieren. Der Einzähler beginnt auf der ersten Taktgrenze, die die Kommando-Queue
noch rechtzeitig erreicht, also ist der tatsächliche Vorlauf ein voller Takt **plus** der Rest des
angefangenen — nie weniger. Er läuft genau einmal, vor Sektion 0.

### Autorelease, Pending, Quantisierung

* `autorelease: true`: Der Folgewechsel liegt sample-genau `bars` Takte später und wird **sofort
  beim Betreten der Sektion** armiert und abgeschickt. Der Runner wartet auf keine Taktgrenze.
* `autorelease: false`: Die Sektion loopt. Beim Release wird der Wechsel quantisiert —
  `quantize: bar` auf die nächste Taktgrenze, `quantize: loop` auf das Ende des laufenden
  Durchlaufs — und ist bis dahin sichtbar armiert („Wechsel armiert: ‚voice_2‘ in 5 Takten").
* **Zweimal Auslösen überspringt nichts.** Ist schon ein Wechsel armiert, meldet der zweite Druck
  nur, wie weit er noch weg ist, und schickt kein einziges Kommando.
* **Ein Wechsel landet nie in einem laufenden Take.** Ein Release in Takt 3 einer achttaktigen
  Aufnahme würde einen Drei-Takte-Loop definieren, und alles, was danach dagegen aufgenommen wird,
  säße falsch. Der Wechsel wird deshalb ans Ende des Takes geschoben, mit Meldung. Verglichen wird
  in **Takten**, nicht in Samples: Ein Take darf eine Sekunde-Bruchteil-Sample über seine Taktgrenze
  hinausragen, ohne den Wechsel eine ganze Sektion weiterzuschieben.

### Warum der Runner die Loop-Geometrie vorhersagt

Ein Overdub muss auf das eigene Raster des Tracks (`origin + n · loop_len`) einrasten, und eine
Sektion wird geplant, **bevor** der Take, der dieses Raster definiert, fertig ist — der Status sagt
zu dem Zeitpunkt noch `loop_len = 0`. Der Runner braucht ihn nicht: Er hat den Take selbst
geschickt und weiß deshalb, dass der Loop genau `[start, end)` der aufnehmenden Sektion sein wird.
Solange eine Partitur läuft, schickt nichts anderes Kommandos, also ist die Vorhersage exakt.

Das ist mehr als Bequemlichkeit. Taktgrenzen werden einzeln gerundet (Abschnitt 3), also können
`bar_start(b + 8)` und `origin + n · loop_len` um ein Sample auseinanderliegen. Eine Ebene, die auf
der Taktgrenze statt auf dem Loop-Raster startet, ist am Ende des Durchlaufs ein Frame zu kurz —
ein einzelnes Null-Sample, das sich bei jedem Durchlauf wiederholt —, und ein `StartOverdub` ein
Sample vor dem `StopRecord` des laufenden Takes wird von der Engine als „Busy" abgelehnt. Beides
ist in Henrys 3/4-Partitur bei 141 BPM real und in `henrys_score_plays_from_start_to_finish`
festgehalten.

### Mithören ist das eine, was keinen Zeitstempel trägt

`Command::SetMonitor` hat kein `at` (Abschnitt „Kommandos" in `command.rs`). Fünf Takte im Voraus
geschickt wäre der Sänger fünf Takte zu früh hörbar. Der Runner hält den Wechsel deshalb zurück
und schickt ihn in `tick()`, wenn die Engine die Sektionsgrenze wirklich erreicht hat — auf eine
Runde der Steuerschleife genau, also wenige Millisekunden. Nichts, was *aufgenommen* wird, hängt
daran; eine Aufnahme trägt ihren Zeitstempel ohnehin.

### Was beim Laden mit laufender Engine passiert

**Gar nichts — es wird abgelehnt.** Die Track-Aufstellung der Engine (wie viele Tracks, wie sie
heißen, auf welchen Eingängen sie hören) wird beim Start festgelegt und unter einer laufenden
Sitzung nie geändert. `runner::check_tracks` vergleicht eine Partitur mit der laufenden Aufstellung
und liefert eine deutsche Meldung mit Handlungsanweisung, wenn sie abweicht. Der Grund: Ein still
umgehängter Eingang schickt den nächsten Take auf das falsche Mikrofon, und ein verschwundener
Track nimmt einen aufgenommenen Loop mit. Panorama und Latenz sind bewusst **nicht** Teil des
Vergleichs — beide ändern nichts an der Bedeutung eines Kommandos und sind zur Laufzeit einstellbar.
Das `score`-Subcommand baut die Engine aus der Partitur, dort kann der Fall gar nicht auftreten; die
Regel steht für die spätere Oberfläche.

Aus demselben Grund kann ein **armierter** Wechsel nicht umgelenkt werden: Seine Kommandos liegen
schon mit Zeitstempel im Wartezimmer des Audio-Threads und sind nicht zurückzuholen. `goto` wird
deshalb abgelehnt, solange etwas armiert ist, und sagt das auch. `stop_all` kann nicht ablehnen und
macht den armierten Wechsel stattdessen dort unschädlich, wo er landet (`ClearTrack` für Tracks, auf
denen er einen Take gestartet hätte, `StopPlay` für den Rest) — mit Meldung.

### CLI

```
looper-engine score examples/henry-3-4.rust.yaml --host asio --device Scarlett
```

Tempo, Taktart, Tracks, Eingänge, Sektionen und Quantisierung kommen aus der Datei; auf der
Kommandozeile stehen nur noch die Dinge, die die *Maschine* betreffen (Gerät, Puffer, Latenz,
Klick-Pegel, `--count-in`). `--check` übersetzt die Partitur und druckt ihren Aufbau, ohne ein Gerät
zu öffnen. Übersetzungsfehler kommen mit Zeile, Spalte und Vorschlag heraus, alle auf einmal — das
liefert der Compiler bereits.

Die Anzeige zeigt Sektion mit Index und Namen, Takt im Abschnitt, Durchlauf, je Track den
Sollzustand neben dem Ist-Zustand der Engine, und ob ein Wechsel armiert ist und in wie vielen
Takten. Tasten: `<leer>` oder `n` für die nächste Sektion (der Release-Knopf), `g <nr>` zum
Springen, `s` für Stopp aller Tracks, `k` Klick, `q` beenden.

### Wo der Code liegt

| Datei | Inhalt |
|---|---|
| `engine/src/engine/runner.rs` | Zustandsautomat, Soll-gegen-Ist, Einzähler, Autorelease, Pending |
| `engine/src/engine/runner/tests.rs` | 21 Offline-Tests, inklusive Henrys Partitur von Anfang bis Ende |
| `engine/src/engine/score_cli.rs` | `score`-Subcommand: Anzeige und Tastatur, sonst nichts |
| `engine/src/engine/schedule.rs` | unverändert die *einzige* Stelle, an der Kommandos entstehen — der Runner benutzt die neuen `*_at`-Einstiege, die eine schon bekannte Position hineinreichen statt sie ein zweites Mal zu quantisieren |
| `engine/src/engine/live.rs` | `start_engine` baut Engine und Streams; `live` und `score` teilen sich das, damit die Startreihenfolge (Eingang vor Ausgang) nur an einer Stelle steht |
| `app/src-tauri/src/main.rs` | die Kommandos `score_compile`, `score_load`, `score_start`, `score_next`, `score_goto`, `score_stop_all` |
| `app/src-tauri/src/host.rs` | `Session` haelt den Runner, tickt ihn je Kontrollrunde und schickt seine Kommandos weiter |
| `ui/src/components/` | Editor (CodeMirror mit Lint aus den echten Compiler-Fehlern), Blockvorschau, Transport |

### Was die Oberflaeche daraus macht

Der Runner-Zustand faehrt im **Status-Event** mit, als Feld `score` (Phase, Sektion mit Index und
Name, Takt in der Sektion, Durchlauf, armierter Wechsel samt Vorlauf, Sollzustand je Track). Kein
eigener Stream: Sektion, Takt und Pegel muessen aus demselben Augenblick stammen, sonst steht auf
dem Buehnenbildschirm eine Taktzahl aus einem Schnappschuss neben einem Pegel aus dem naechsten.

Die vier Transport-Kommandos antworten mit einem **deutschen Satz statt mit Erfolg oder Fehler** —
der Runner scheitert nicht, er erklaert. `score_goto` bei armiertem Wechsel meldet, wie weit der
armierte Wechsel noch ist, und schickt kein einziges Kommando; dieser Satz *ist* die Antwort.

Geladen wird nur auf eine **leere** Sitzung, deren Tracks zur Partitur passen (`check_tracks`).
Grund ist die Vorhersage der Loop-Geometrie: Der Runner startet mit der Annahme, jeder Track sei
still und leer, und plant Overdubs gegen das Raster der Takes, die er selbst geschickt hat. Einen
von Hand aufgenommenen Loop sieht er nicht. Tempo, Taktart und die Laenge, auf die die
Ebenen-Puffer alloziert sind (die laengste Sektion), kommen beim Laden **aus der Partitur** — das
ist dasselbe, was das `score`-Subcommand tut, indem es die Engine aus der Partitur baut.

Waehrend eine Partitur laeuft, gehoert der **Transport dem Runner und der Mix dem Menschen**:
`record`, `overdub`, `play`, `stop`, `clear`, Tempo und Quantisierung werden von Hand abgelehnt,
Panorama, Ebenenlautstaerke, Stummschaltung, Effekte und Klick bleiben bedienbar. Ein Take, den der
Runner nicht geschickt hat, wuerde seine Vorhersage ab diesem Moment falsch machen.

## 11. MIDI-Steuerung (Phase 5)

Der Code liegt in `engine/src/midi/`. Eine einzige neue Abhängigkeit: **`midir` 0.11** — unter
Windows eine dünne Hülle um `midiInOpen` aus winmm. Alles oberhalb des Bytestroms ist selbst
geschrieben, und das ist der Grund, warum von diesem ganzen Kapitel genau *eine* Datei ein Gerät
braucht.

### Der Parameterbaum: alles Bedienbare hat eine Adresse

Ohne stabile, textliche Adresse kann ein Learn-Modus nichts festhalten und keine Datei etwas
speichern. Deshalb bekommt jede bedienbare Sache einen gepunkteten Pfad:

```text
transport.start | transport.next | transport.stop_all | transport.goto.<n>
global.click | global.clear_all | global.tempo | global.quantize.bar|loop
track.<ref>.record | overdub | play | stop | clear | monitor | pan | latency_trim
track.<ref>.layer.<n>.mute | remove | gain
track.<ref>.fx.bypass
track.<ref>.fx.<high_pass|eq|comp|delay|reverb>.on
track.<ref>.fx.preset.<trocken|stimme|gitarre>
track.<ref>.fx.high_pass.hz · eq.<1-3>.<hz|q|gain|peak|low_shelf|high_shelf>
track.<ref>.fx.comp.<threshold|ratio|attack|release|knee|makeup>
track.<ref>.fx.delay.<note.<wert>|feedback|mix> · reverb.<size|damping|mix>
```

**Alles ist außen 1-basiert und innen 0-basiert**, auf jeder Ebene — `track.1` ist `tracks[0]`,
`layer.2` ist `layers[1]`, `eq.3` ist `bands[2]`. Dieselbe Regel, der die CLI und die Anzeige
ohnehin folgen: ein Musiker zählt ab eins.

**Tracks per Nummer *und* per Name**, und die Begründung ist der eigentliche Grund für die zwei
Stufen unten:

| Wo | Schreibweise | Warum |
|---|---|---|
| Controller-Profil | `track.1.record` | Ein Profil soll jedes Stück überleben. „Track 1" ist in jedem Stück der erste Track; `track.stimme` wäre im nächsten Stück tot, weil die Stimme dort `gesang` heißt |
| Partitur | `track.stimme.record` | Die Partitur kennt ihre eigenen Namen, wird von Hand geschrieben, und ein Name kann bei geänderter Track-Reihenfolge nicht still auf den falschen Track zeigen |

Die Leseregel ist mechanisch: **ein Segment aus lauter Ziffern ist eine Position, alles andere ein
Name.** Ein Track, der nur aus Ziffern besteht oder einen Punkt im Namen trägt, ist damit nicht per
Name adressierbar — bewusst in Kauf genommen, weil die Alternative Anführungszeichen in Adressen
wären.

`looper-engine midi targets` druckt den Baum; er wird aus denselben Werten erzeugt, die der Parser
liest, kann also nicht davon abweichen, was tatsächlich bindbar ist.

### Schalter, Taster, Regler

`Control` sagt, *was* ein Ziel ist, und daraus folgt, was ein MIDI-Ereignis damit tun darf:

| Art | Beispiele | Pad | Regler |
|---|---|---|---|
| **Taster** | `record`, `transport.next`, `preset.stimme` | löst aus | löst aus beim Überschreiten von 64 |
| **Schalter** | `monitor`, `fx.reverb.on`, `layer.1.mute` | umschalten oder halten | an ab 64 |
| **Wert** | `pan`, `fx.reverb.mix` | Anschlagstärke wird zum Wert | fährt den Wert |

Das Tastenverhalten ist **je Bindung** einstellbar (`press`, `release`, `toggle`, `momentary`), mit
einer Vorgabe je Ziel: *auslösen* bei einem Taster, *umschalten* bei einem Schalter. Beides ist
nötig, und zwar am selben Ziel: „Mithören" will meistens ein Umschalter sein (mit einer Gitarre in
beiden Händen hält man kein Pad), manchmal aber ein Halteschalter (Talkback, eine Phrase über den
Loop). Ein *Taster* als Umschalter wird dagegen **abgelehnt**, wenn die Datei gelesen wird:
„Aufnahme, aber umgeschaltet" hat keinen zweiten Zustand, und es still als Druck zu behandeln,
hinterlässt eine Datei, die etwas anderes tut, als sie sagt.

Die Wertebereiche sind **nicht hier erfunden**, sondern die Klemmwerte der Engine selbst, abgelesen
in `fx/dynamics.rs`, `fx/delay.rs`, `fx/reverb.rs`, `fx/mod.rs` und der Kommandoschicht. Ein Regler
am Anschlag landet damit exakt dort, wo die Engine ohnehin geklemmt hätte. Zwei Feinheiten:

* **Frequenzen und Zeiten sind logarithmisch**, dB und Anteile linear. Von 20 Hz bis 20 kHz linear
  läge der ganze Bassbereich in den ersten zwei von 127 Schritten.
* **Ein Bereich um die Null hat bei CC 64 exakt die Mitte.** Panorama, EQ-Verstärkung und
  Makeup-Gain sind so; „den Regler wieder genau in die Mitte" ist etwas, das man dauernd tut, und
  `64/127` wäre nicht die Mitte. An beiden Enden ist die Abbildung ohnehin exakt, ohne Arithmetik,
  die daneben landen könnte.

### Der Wertesprung: Pickup

Der Regler steht irgendwo, der Parameter woanders — weil ein Preset ihn verschoben hat, weil die
Maus ihn verschoben hat, oder einfach weil seit dem Programmstart niemand ihn angefasst hat. Der
erste Millimeter Bewegung lässt den Parameter dann springen. Auf einem Hall-Anteil hört man das, auf
einer Ebenenlautstärke während eines Takes ruiniert es den Take.

| Verfahren | wirkt | hier |
|---|---|---|
| **Pickup** | erst, wenn der Regler den aktuellen Wert passiert | **Standard** |
| Relativ | Regler sendet Differenzen | **braucht einen Endlos-Encoder** — der MPD218 hat Potis |
| Skalierung | Restweg auf Restbereich dehnen | kein Sprung, aber der Regler zeigt auf einen Wert, auf dem er nicht steht |

Der MPD218 hat Potentiometer mit Anschlag; relativ steht damit gar nicht zur Verfügung. Pickup
fängt sowohl das *Erreichen* des Wertes (Toleranz zwei von 127 Schritten — ein Poti zittert) als
auch das *Überfahren* ab, weil ein schnell gedrehter Regler Schritte überspringt. Weil „die erste
Bewegung tut nichts" verwirrend ist, wenn man nicht weiß, dass es an ist, ist es je Bindung
abschaltbar (`takeover: jump`). Nach einem Preset muss jeder Regler neu fangen (`Router::rearm`),
sonst schützt Pickup die erste Berührung nach dem Start und danach nie wieder.

### Zwei Stufen: das Gerät und das Stück

| Stufe | Datei | gilt für | Schlüssel ist |
|---|---|---|---|
| Controller-Profil | `%APPDATA%\henrys-looper\midi\<gerät>.yaml` | jedes Stück | die **Taste** (`ch1.note36`) |
| Partitur-Ergänzung | der `midi:`-Block der Partitur | dieses Stück | die **Adresse** (`track.stimme.record`) |

Das Profil liegt im Anwendungsdatenverzeichnis und nicht neben der Partitur, aus demselben Grund,
aus dem es die zwei Stufen überhaupt gibt: es beschreibt ein Stück Hardware an *diesem* Rechner.
Neben der Partitur würde es mit ihr auf einen Rechner reisen, an dem der MPD218 nicht hängt.
`HENRYS_LOOPER_CONFIG_DIR` verlegt das Verzeichnis (das benutzen die Tests, und wer seine
Einstellungen auf einem Stick hält).

Jede Datei ist so herum geschlüsselt, wie sie sich liest: ein Profil ist eine Liste dessen, was die
Pads tun; eine Partitur ist eine Liste dessen, was das Stück braucht. Der `device:`-Eintrag wird
**locker** gegen den Portnamen verglichen, weil Windows dekoriert: derselbe MPD218 heißt einmal
`MPD218` und einmal `2- MPD218`, je nachdem, was vorher eingesteckt war.

```yaml
# %APPDATA%\henrys-looper\midi\mpd218.yaml — Vorlage: examples/midi-mpd218.yaml
device: MPD218
bindings:
  ch10.note36: {target: track.1.record}
  ch10.note45: {target: transport.next}                 # der Release-Knopf
  ch10.note48: {target: track.1.monitor, mode: momentary}
  ch1.cc3:     {target: track.1.fx.reverb.mix, max: 0.5}
```

```yaml
# in der Partitur — überschreibt das Profil für dieses eine Stück
midi:
  next_section:               {cc: 64}                  # das alte Format, unverändert gültig
  stop_all:                   {note: 37}
  track.stimme.record:        {note: 36}
  track.stimme.fx.reverb.mix: {cc: 3, max: 0.4}
  transport.goto.3:           {note: 45}
```

**Das bestehende Format übersetzt weiter.** `next_section` und `stop_all` sind gültige Adressen von
`transport.next` und `transport.stop_all`; der kompilierte Schlüssel bleibt, wie er geschrieben
wurde, und `target` trägt daneben die kanonische Adresse. Ein Leser, der nur das alte
Zwei-Bindungs-Format kannte, findet weiterhin, wonach er sucht.

**Konflikte werden gemeldet, nicht still entschieden.** Zwei Ziele auf derselben Taste sind in einer
Datei ein Fehler mit Zeilennummer (eine Taste kann nur eine Sache tun); zwei Tasten auf demselben
Ziel sind erlaubt und nützlich. Legt die Partitur eine Taste des Profils neu, ist das *kein* Fehler
— dafür ist der `midi:`-Block da —, aber jede solche Übernahme kommt als deutscher Satz heraus: ein
Pad, das in diesem einen Stück still etwas anderes tut, ist die Sorte Überraschung, die auf der
Bühne teuer wird.

### Anwenden, und wo die Grenze liegt

Ein eingehendes Ereignis wird gegen das Mapping aufgelöst und wird zu einer `MidiAction` — derselben
Art Absicht, die auch ein Mausklick erzeugt, mit aufgelöstem Track-Index und fertigem Wert.
Bewusst **kein** `Command`: die Hälfte davon braucht einen musikalischen Zeitstempel, und aus einer
Bedienhandlung werden getimte Kommandos an genau einer Stelle (`engine/src/engine/schedule.rs`,
Abschnitt 3). Für alles Ungetimte liefert `MidiAction::command()` das fertige Kommando.

**Während die Partitur läuft, gehört der Transport dem Runner — auch für ein Pad.** Ein Pad, das
eine Aufnahme startet, die der Runner nicht geplant hat, macht seine vorhergesagte Loop-Geometrie ab
diesem Moment falsch (Abschnitt 10). Die Ablehnung ist **derselbe Satz** wie für die Maus, aus
derselben Konstante `runner::TRANSPORT_BELONGS_TO_SCORE` — zwei Formulierungen würden zwei Regeln
suggerieren. `transport.*` ist ausdrücklich *nicht* betroffen: das ist der Release-Knopf des
Runners, und ihn zu sperren hieße, genau das zu sperren, wofür der Musiker den Controller in der
Hand hat. Ein Test paart die MIDI-Liste mit der Maus-Liste und besteht darauf, dass sie sich einig
sind — und dass jede Adresse, die den Transport verschiebt, in der Paarung steht.

### Echtzeit

Der MIDI-Callback ist **nicht** der Audio-Thread — er kommt von einem Treiber-Thread, ein langsamer
Callback kann also keinen Aussetzer erzeugen. Er kann etwas fast so Schlimmes erzeugen: verspätete
Ereignisse, und das heißt auf einem Looper einen Take auf dem falschen Schlag. Deshalb gelten die
Regeln aus Abschnitt 5 hier trotzdem: der Callback dekodiert drei Bytes und schiebt sie in einen
lock-freien Ringpuffer (`rtrb`, derselbe wie beim Audio-Thread), alles Weitere macht der
Steuer-Thread. Eine volle Queue verwirft und zählt.

Behandelt werden Note On, Note Off, Control Change und Pitch Bend. **Note On mit Velocity 0 ist ein
Note Off** — fast kein Controller sendet `0x8n`, und wer `90 24 00` als Druck liest, lässt jedes Pad
für immer gedrückt. Laufender Status, über zwei Lieferungen zerrissene Nachrichten, eingestreute
Echtzeit-Bytes und SysEx sind behandelt; unter Windows zerlegt winmm den Strom zwar schon selbst,
aber ein Dekoder, der nur bei freundlicher Unterlage funktioniert, fällt bei genau dem einen
Controller aus, der nicht freundlich ist.

### Windows gibt MIDI exklusiv heraus

`midiInOpen` scheitert mit `MMSYSERR_ALLOCATED`, wenn ein anderes Programm den Port schon hält, und
das ist hier der Normalfall: läuft Ableton mit dem MPD218 als Control Surface, bekommen wir ihn
nicht. Dieser eine Fehler hat deshalb einen eigenen Satz, der den wahrscheinlichen Schuldigen nennt,
statt „Fehler beim Oeffnen". `midi list` öffnet nichts und funktioniert deswegen auch dann.

### CLI

```
looper-engine midi list                             # Eingaenge, mit dem Profil, das dazu passt
looper-engine midi monitor [--device X] [--profile P]   # was sendet dieses Pad, und was tut es
looper-engine midi learn --target track.1.record [--save]
looper-engine midi profile [--device X | --profile P]
looper-engine midi targets [--tracks N --layers M --sections K --filter TEXT]
```

`monitor` ist das, was tatsächlich benutzt wird: das Handbuch eines Control Surface ist meistens
falsch oder gar nicht da, und jede Pad-Bank des MPD218 sendet andere Noten. `targets` braucht kein
Gerät.

### Wo der Code liegt

| Datei | braucht Gerät | Inhalt |
|---|---|---|
| `engine/src/midi/event.rs` | nein | Bytes zu Ereignissen, laufender Status, die Ids |
| `engine/src/midi/target.rs` | nein | der Parameterbaum, Wertebereiche, Kurven |
| `engine/src/midi/binding.rs` | nein | Taster gegen Umschalter, Wertesprung-Schutz, das Mapping |
| `engine/src/midi/router.rs` | nein | Ereignis + Mapping → Absicht, samt Runner-Grenze |
| `engine/src/midi/profile.rs` | nur Dateisystem | Controller-Profil lesen und schreiben |
| `engine/src/midi/learn.rs` | nein | „die nächste Taste wird X" |
| `engine/src/midi/input.rs` | **ja** | midir, Geräteliste, Öffnen, Queue |
| `engine/src/midi/cli.rs` | ja, außer `targets` | das `midi`-Subkommando |
| `engine/src/midi/tests.rs` | nein | 51 Offline-Tests |
| `examples/midi-mpd218.yaml` | — | Vorlage für Henrys Controller |
| `app/src-tauri/src/midi.rs` | ja zum Öffnen | offener Port, Profil, Learn-Modus, Drahtformat |
| `app/src-tauri/src/host.rs` | — | `pump_midi`, `intent_of`, `SessionState` (die `ParamState`) |
| `ui/src/midi/addresses.ts` | — | die Adressen, wie das UI sie schreibt |
| `ui/src/midi/LearnLayer.tsx` | — | der eine Listener, und die Kennzeichen |
| `ui/src/midi/MidiPanel.tsx`, `MidiMonitor.tsx` | — | Leiste, Monitor, Belegungsübersicht |
| `ui/src/midi/store.ts` | — | der MIDI-Hub, Geschwister von `status.ts` |

---

### In der App: vom Pad in die laufende Sitzung

Alles bisher Beschriebene braucht kein laufendes Programm. Was die App dazutut, sind drei Dinge:
der offene Port, die Übersetzung in eine Bedienhandlung, und der Weg zurück auf den Bildschirm.

```text
 Controller ─► midi/input.rs ─► Queue ─► app/midi.rs ─► app/host.rs ─────────► Session::apply
 (Treiber-     dekodiert im     lock-    Router +       MidiAction ─►           (derselbe Weg
  Thread)      Callback         frei     Profil+Partitur  Action / ScoreAction   wie ein Klick)
                                                │
                                                └─► looper://midi ─► ui/midi/store.ts
```

**Der Bridge sitzt auf dem Host-Thread, nicht in `Session`.** Ein Controller bleibt über einen Stopp
und einen Neustart der Engine hinweg verbunden, und der Learn-Modus muss funktionieren, bevor
überhaupt etwas läuft. Die Queue wird in derselben Schleife geleert, die auch den Puffervorrat
bedient — mit dem kurzen Takt (4 ms), sobald ein Port offen ist: ein Pad, das vier Millisekunden zu
spät ankommt, ist unhörbar, eines mit zweihundert nicht.

**Eine aufgelöste Absicht geht durch dieselbe Tür wie ein Mausklick**, `Session::apply` für alles
Mischen und Aufnehmen, `Session::score_act` für den Transport der Partitur. Damit gelten die
Prüfungen der Maus unverändert: die Trackliste, die Ebenenliste, und die Grenze aus Abschnitt 10.
Zwei Zahlen legt die App dabei dazu, weil eine MIDI-Absicht sie nicht tragen kann: die Taktart beim
Tempo (ein Regler hat eine Dimension), und die **gemessene** Hälfte der Latenzkompensation beim
Zuschlag — ein Regler auf dem Zuschlag darf keine Messung auslöschen.

**`ParamState` kommt aus dem Status-Snapshot**, demselben, aus dem der Bildschirm gezeichnet wird.
Ohne ihn hat Pickup nichts, woran es fangen könnte, und ein Umschalter kippt seine private Kopie
eines Zustands, den auch die Maus und die Partitur ändern. Nach einem Preset, einem Partiturwechsel
und einem Engine-Start wird `Router::rearm` gerufen: dort haben sich Werte bewegt, ohne dass sich
Regler bewegt haben.

**Verbindungsverlust** meldet Windows nicht. Also wird die Portliste im Zwei-Sekunden-Takt gefragt,
solange etwas offen ist; verschwindet der Port, wird sauber geschlossen und ein deutscher Satz
gezeigt. Die Bindungen bleiben — nach dem Anstecken wieder verbinden, nichts ist verloren.

**Die Partitur-Bindungen greifen beim Laden**, genau wie in der CLI: `score_map` über das Profil
gelegt, und jede übernommene Taste kommt als deutscher Satz heraus.

### Warum MIDI ein eigenes Ereignis ist und nicht im Status mitfährt

Der Status ist eine **Abtastung eines stetigen Zustands**, zwanzig Mal je Sekunde, und ältere
Schnappschüsse werden absichtlich weggeworfen (`Status::latest`) — eine Position von vor 50 ms ist
wertlos. MIDI ist das Gegenteil: ein seltener, stoßweiser Strom **einzelner Tatsachen**, bei dem
jede zählt. Ein Pad-Druck, der zwischen zwei Schnappschüsse fällt, wäre ein Pad-Druck, den der
Monitor nie zeigt — und der Monitor ist genau dafür da, die Frage „sendet dieses Pad überhaupt
etwas" zu beantworten. Also läuft MIDI über `looper://midi`, als Warteschlange.

Was es sich vom Status leiht, ist die **Ratendisziplin**: ein gedrehter Regler erzeugt rund hundert
Ereignisse je Sekunde. Aufeinanderfolgende Werte derselben Taste werden zusammengefasst und
höchstens alle 40 ms verschickt; alles Diskrete — ein Pad, eine Ablehnung, eine gelernte Bindung —
geht sofort raus, denn darauf wartet das Auge.

### Learn durch Anklicken

> „Dass ich einen Button z. B. mit Strg anklicke und dann der nächste gedrückte MIDI-Button ist der
> gemappte Button."

Ein Schalter **„MIDI lernen"** in der Leiste unter dem Kopf schaltet den Modus global ein. Solange
er an ist, wählt ein Klick auf ein Bedienelement dieses Element als Lernziel, statt es zu bedienen;
das nächste Pad, das ankommt, wird darauf gelegt. **Strg-Klick tut dasselbe ohne den Schalter**,
aber der Schalter ist der angebotene Weg: mit Strg klickt man leicht daneben, und neben „Aufnahme"
ist „Aufnahme" — eine verrutschte Modifiertaste würde eine Aufnahme starten statt ein Pad zu
belegen. Das ist auf der Bühne ein schlechter Tausch.

**Wie die Adresse ins UI kommt.** Jedes belegbare Element trägt seine Adresse in einem
`data-midi`-Attribut (`ui/src/midi/addresses.ts` baut die Strings, `LearnLayer.tsx` liest sie).
Ein einziger Listener in der Capture-Phase sucht den nächsten Vorfahren mit diesem Attribut. Die
Alternativen waren, die Adresse als Prop durch vier Komponenten zu reichen, oder einen Katalog vom
Rust-Teil zu holen und Elemente unscharf dagegen zu matchen. Ein Attribut neben dem `onClick`, das
ohnehin dasteht, ist beides nicht: keine neue Prop, keine neue Komponente, und ein Element, das
später dazukommt, wird durch dieselbe eine Zeile belegbar.

Geprüft wird die Adresse **nicht** im UI, sondern von `Target::parse` auf der Rust-Seite, wenn der
Modus scharf gestellt wird. Ein Tippfehler ist damit der deutsche Satz, der auf `midi targets`
zeigt, und keine Bindung auf nichts.

**Belegte Elemente sind erkennbar**, auch außerhalb des Lernmodus: ein kleines `Pad 36` oder `CC 3`
in der Ecke, gezeichnet von CSS aus `data-midi-key`. Deshalb kostet eine geänderte Belegung kein
einziges Rendern. Orange statt blau heißt: diese Bindung kommt aus der Partitur und geht mit ihr
wieder weg. Der volle Schlüssel (`ch10.note36`) steht in der Belegungsübersicht — die Ecke einer
Taste ist kein Ort für eine Kanalnummer.

Escape nimmt zuerst das schwebende Ziel zurück und beim zweiten Druck den Modus: „ich habe den
falschen Knopf erwischt" ist ein anderer Gedanke als „ich bin fertig".

### Wo das im Bild sitzt

Eine schmale Leiste unter dem Kopf, in **beiden** Reitern. Ein dritter Reiter wäre eine dritte
Entscheidung auf der Bühne (Abschnitt 7) für etwas, das man **vor** dem Stück einrichtet und während
des Stücks fast nie ansieht. Zugeklappt zeigt die Leiste die zwei Dinge, die mitten im Lauf zählen:
hängt der Controller noch, und ist der Lernmodus an. Aufgeklappt wird sie zum Einrichtungsfeld:

* **Geräteliste** mit dem Profil, das dazu passt, Verbinden und Trennen, Profil speichern samt Pfad;
* **Monitor** — was kommt herein, und was macht das Mapping daraus. Vier Spalten in der Reihenfolge,
  in der man fragt: welche Taste, welche Art, welcher Wert, was kam heraus. Die letzte Spalte ist
  der Grund, warum das kein Byte-Log ist: „nicht belegt" und „abgelehnt: der Runner hat den
  Transport" sehen an einem toten Pad gleich aus und brauchen völlig verschiedene Reparaturen;
* **Belegung** — welche Taste auf welchem Ziel, mit deutschen Namen aus `Target::label`, Herkunft
  (Profil oder Partitur) und einem ✕ zum Lösen. Eine Bindung der Partitur lässt sich hier nicht
  lösen, und der Satz sagt warum.

### Was nicht belegbar ist, und warum

`global.tempo` und die EQ-Bandart haben zwar eine Adresse (eine Partitur kann sie belegen), aber im
UI kein Element, an dem ein Klick eindeutig wäre: das Tempo wird vor dem Start gesetzt und ist
ohnehin nur bei leerer Sitzung erlaubt, und die Bandart ist ein Auswahlfeld mit drei Zielen an einer
Stelle. Beide sind über die Partitur oder von Hand im Profil erreichbar.

## 12. Arbeitsweise

- Henry bedient Hardware selbst; Audio-Tests laufen nur mit seiner Zustimmung.
- **Nie gleichzeitig an derselben Sache arbeiten.** Das hat schon Schaden angerichtet.
- **Bei Audio-Werten nicht raten, messen.** Besonders bei Latenz.
- Code und Kommentare Englisch, Nutzerausgaben Deutsch, Commits Deutsch.
- Offline beweisbare Eigenschaften werden offline bewiesen, bevor sie am Instrument geprüft
  werden. Zwei Millisekunden Versatz hört man beim Spielen nicht, aber sie ruinieren jeden
  Overdub.
