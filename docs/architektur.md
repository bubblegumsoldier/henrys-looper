# Architektur und verbindliche Entscheidungen

Zentrales Nachschlagewerk. Wer neu auf dieses Projekt schaut — Mensch oder Agent — liest
**dieses Dokument zuerst**. Es sammelt die Entscheidungen, die getroffen wurden, samt
Begründung und den Messwerten, auf denen sie beruhen. Stand 2026-09-06.

> **Hinweis (Stand 15.11.2026):** Alle Meldungen, die die App oder `looper-engine` ausgeben, sind inzwischen **englisch**. Wo dieses Dokument von einem „deutschen Satz“ spricht, ist derselbe Satz auf Englisch gemeint; die Begründungen gelten unverändert.

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
| Ausgänge | ein Ausgang für alles | **getrennte Busse — gebaut**, Klick nur auf den Monitorweg. Siehe Abschnitt 12 |
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
| 9 | Getrennte Ausgangsbusse | **gebaut**, 379 Tests grün, ohne Gerät geprüft. Zwei Stereo-Busse (Main in den Saal, Monitor auf den Kopfhörer), je Track schaltbar für Loop *und* Mithörsignal getrennt, Lautstärke je Bus, freie Zuordnung zu Ausgangspaaren, **der Klick nur auf Monitor**. Abnahme am Instrument offen. Siehe Abschnitt 12 |

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
der acht Takte wartet, wirkt kaputt. (Bis zum Stopp am Taktende, unten, galt das für den Stopp nur
während einer Aufnahme; eine spielende Spur verstummte sofort.) Die Wiedergabe verliert dabei nichts, weil ein Loop als
`(Position − origin) mod loop_len` gelesen wird und deshalb phasenrichtig weiterläuft, egal wann man
sie einschaltet. (In einer Partitur beginnt ein Loop, der mit einer Sektion einsetzt, dagegen vorn —
Abschnitt 10, „Ein Loop, der einsetzt, beginnt vorn".)

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

### Stopp am Taktende, Stopp sofort (freies Loopen)

Der Stopp-Knopf einer Spur ist geteilt. **Die große Fläche (Taste `S`, MIDI `track.<n>.stop`) stoppt
am Ende des laufenden Takts**: der Loop spielt bis zum Taktstrich und ist ab genau diesem Sample
still, die 10-ms-Blende liegt davor wie bei jedem getimten Stopp (Abschnitt 19). Ein Loop, der
mitten in der Phrase abbricht, klingt nach Fehler; einer, der auf dem Taktstrich endet, nach Schluss.
**Das kleine rote Quadrat rechts daneben (`⇧S`, `track.<n>.stop_now`) stoppt sofort**, wie der
Knopf es vorher immer tat (ein `StopPlay` einen Vorlauf nach dem Druck). Keine Rückfrage — die Farbe
(`--err`) und der Tooltip sind die Warnung.

* **Wartet** ein Stopp, zeigt die Kachel ihn wie jeden Vorlauf („Next → Stop in 2 beats", der
  Knopf schraffiert und gestrichelt). **Noch einmal drücken nimmt ihn zurück**; ebenso Play,
  Record, Overdub und Clear auf derselben Spur — der neuere Druck ist, was der Musiker jetzt will.
  Play auf der noch spielenden Spur heißt dann nur „weiterspielen". Sofort-Stopp während ein Stopp
  wartet: sofort still, und der wartende wird mit zurückgenommen, damit er nicht ein Play trifft,
  das vor seinem Taktstrich gedrückt wird.
* **Zurücknehmen** ist das Engine-Kommando `CancelStop { track, at, take }`: es nimmt den
  *neuesten* noch wartenden `StopPlay { track, at }` aus dem Warteraum (mit `take` das neueste
  Paar `StopRecord` + `StartPlay` auf `at`, siehe unten). Ungetimt, kein Allokieren (Suche und
  `Vec::remove` im Warteraum). Ist der Stopp schon ausgeführt — bei einem Audio-Loop heißt das: seine
  Blende hat begonnen —, tut es nichts; ein Stopp wird nie halb zurückgenommen. Die Steuerseite
  schickt es deshalb nur, solange bis zum Taktstrich mehr als zwei Vorläufe bleiben, sonst sagt sie
  „too late to cancel".
* **Während einer Aufnahme** bleibt es bei dem, was Stopp schon immer tat: der Take endet auf dem
  nächsten Taktstrich (`StopRecord` + `StartPlay`, derselbe Weg wie jedes Take-Ende), danach spielt
  der Loop. Für beide Hälften des Knopfs — ein Take, der mitten im Takt endet, ergäbe einen Loop,
  der keine ganze Zahl Takte lang ist. Noch einmal `S` vor dem Taktstrich: der Take läuft bis zu
  seinem eigenen Ende. Fällt der vorgezogene Endpunkt auf das eigene Ende des Takes (Druck im
  letzten Takt), liegen zwei gleiche Paare im Warteraum; `CancelStop` nimmt das neuere, das Take-Ende
  bleibt. Play, Record, Overdub, Clear lassen ein vorgezogenes Take-Ende in Ruhe.
* **Kein Taktraster**: steht die Uhr (oder zählt sie ein), sind alle Loops ohnehin still — der Stopp
  wirkt sofort, und Knopf und Tooltip sagen „at once" statt „at bar end". Einen Loop außerhalb des
  Taktrasters gibt es im freien Modus nicht: Takes beginnen auf Takt- oder Loopgrenzen, sind ganze
  Takte lang, und der Transportstart verankert alle Loops auf Takt 1. Ein Rückfall „Ende des
  Loop-Durchlaufs" ist deshalb nicht nötig und nicht gebaut.
* **MIDI-Loops**: der Stopp ist derselbe getimte `StopPlay`, den auch die Partitur schickt; er gibt
  die klingenden Noten des Loops genau auf dem Taktstrich frei, eine Note *auf* dem Taktstrich wird
  nicht mehr angeschlagen (`midi_tests.rs`, `a_stop_at_the_bar_end_*`). Mit einem Instrument, das
  Latenz meldet, wird der Loop um diese Latenz vorausgelesen; der Schalter liegt aber wie bei jedem
  `StopPlay` auf der Ausgangsachse — eine Note genau auf dem Taktstrich geht dann `latency` Frames
  früher hinaus und wird auf dem Taktstrich beendet. Nichts bleibt hängen.
* **Partitur**: unberührt. Ihre Stopps (Sektionswechsel, „Stop all", Ende, Back to start) plant der
  Runner selbst und gehen nicht durch `Scheduler::stop`; die Stopps aus der Partitur tragen keine
  Rücknahme-Markierung, nichts im freien Modus nimmt sie zurück. Der Stopp-Knopf einer Kachel wird
  wie jede Transporthandlung abgewiesen, solange eine Partitur läuft; `⇧S` ist dann „Stop all".
* Der **Transport-Stopp** des freien Loopens (Leertaste, *Stop* der Uhr, `transport.toggle`) bleibt
  sofort: er hält die Uhr an und verwirft den Warteraum (`TransportStop`), er ist keine Spuraktion.

Code: `engine/src/engine/schedule.rs` (`Scheduler::stop`, `supersede_stop`, `StopAt`),
`Command::CancelStop` in `command.rs`/`process.rs`, `Action::StopTrack`/`StopTrackNow` im Host,
`StopSplit` in `ui/src/live/TrackTile.tsx`. Bewiesen in `engine/src/engine/stop_tests.rs`
(sample-genau auf dem Taktstrich, Zurücknehmen, sofort während des Wartens, Play/Overdub nehmen
zurück, Take-Ende und dessen Rücknahme, Uhr steht, zu späte Rücknahme, keine Allokation),
`schedule.rs` und `midi_tests.rs`.

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

Puffer werden im **Steuer-Thread** alloziert, genullt und einmal je Speicherseite beschrieben
(sonst blendet das System die Seiten erst beim ersten Schreiben im Audio-Thread ein), wandern über eine Install-Queue in
den Audio-Thread (der einen Vorrat hält) und über eine Retire-Queue zurück. Entfernte Layer
werden nicht im Callback freigegeben. Dasselbe gilt für jede Queue und jeden Puffer, in den der
Callback schreibt (`rt::prefault_ring`, `rt::touch_spare`, Abschnitt 18). Geprüft wird das nicht
nur durch Lesen: `engine/src/engine/rt_tests.rs` fährt die Partitur des Besitzers mit einem
zählenden Allokator und verlangt null Allokationen und Freigaben im Callback.

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
  Ebenen-Summe + Mithören ─► (mono: auf beide Kanäle) ─► Kette ─► Panorama ─► Bus (L/R)
  ```
  Der Panner sitzt **nach** der Kette, wie in einem DAW-Kanalzug: der Hall entsteht mittig und wird
  dann platziert, statt ein schon einseitiges Signal zu bekommen. Der Klick geht an Kette und
  Panner vorbei und mit gleichem Pegel in beide Kanäle des **Monitor-Busses** — er ist Referenz,
  kein Teil des Arrangements, und er erreicht den Saal auf keinem Weg (Abschnitt 12).
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

### Die Engine startet von selbst (`app/src-tauri/src/supervisor.rs`)

Die Engine ist ein technisches Detail der App, kein Schritt für den Musiker. Das Fenster schickt
seine Startkonfiguration bei jedem Start und jeder Änderung an den Host (`engine_configure`); wann
daraus ein laufendes Gerät wird, entscheidet `supervisor.rs` ohne Gerät, mit der Geräteliste als
Eingabe – deshalb ist alles davon ohne Hardware getestet:

| Auslöser | Startet? |
|---|---|
| **Boot** – erstes `engine_configure` eines Laufs | wenn „Start the engine when the app opens“ an ist (Vorgabe), einmal |
| **Demand** – ein Druck, der die Engine braucht (Load, Start, Record, Monitor, Regler, Plugin-Fenster) | immer ein Versuch; danach wird der Druck ausgeführt, als käme er einen Moment später (`then_act`, erst nach dem ersten Status-Schnappschuss – Einzähler und Takt 1 regulär ab dort) |
| **Passive** – Note-On oder CC über MIDI | wie Demand, aber nie nach einem Fehlschlag oder beim Warten |
| **Plug** – das erwartete Interface ist da (alle 2 s nachgesehen) | ja |

Ein Fehlschlag wird einmal versucht und mit Grund gezeigt („Engine could not start: …“, Retry);
nichts wiederholt ihn von selbst. Wer die Engine im Engine-Panel stoppt, bekommt keinen Neustart
von selbst, der nächste Druck startet sie wieder. Beim Beenden geht `quit` statt `stop` an den
Host: ab da startet nichts mehr, auch keine MIDI-Note, die während des Plugin-Abbaus ankommt.

**Gerätewahl.** Ein im Setup benanntes Gerät wird geöffnet, wenn es da ist, sonst wird gewartet.
„Default device“ bevorzugt das zuletzt benutzte Interface (`engine.json` im Konfigurationsordner,
nur Geräte mit Ein- und Ausgang, die nicht eingebaut sind). Fehlt es und landete der Standard auf
Lautsprechern, wird gewartet statt ausgewichen; „Use … now“ ist die bewusste Wahl des Standards.

**Rückkopplung.** Ist der Ausgang ein eingebauter Lautsprecher (CoreAudio-Transporttyp `bltn`,
Name als Rückfall, Kopfhörer nie), startet die Engine mit `Command::SetMonitorMute`: alle
mitgehörten Live-Eingänge schweigen am Ausgang, Lautstärke und Schalter bleiben, Instrumente
klingen. Das Status-Event meldet `monitor_muted`, solange tatsächlich ein Monitor an ist;
„Unmute anyway“ hebt es für den Rest des Laufs auf. Eine verweigerte Mikrofon-Freigabe
(AVCaptureDevice) wird vor dem Öffnen erkannt und als eigener Fehler mit Weg in die
Systemeinstellungen gemeldet.

**Ohne Engine ruht die Bühne.** Statt eines leeren Status sendet der Host `ready_event`: Spuren,
Takt, Tempo und Busse der Startkonfiguration (oder der Partitur, die gleich wieder geladen wird),
`running: false`, `ready: true`, Meter in Ruhe. Klick und Tempo sind dann Einstellungen des
nächsten Starts. Setup-Änderungen, die nur ein Neustart anwendet (Gerät, Rate, Puffer, Spurliste
ohne geladene Partitur), meldet der Host als `restart_needed`; beim Verlassen des Setups baut die
App neu auf – sofort, oder nach Rückfrage, wenn Loops verloren gingen (`engine_restart` lädt eine
geladene Partitur auf der neuen Engine wieder).

**Partitur wieder laden.** War beim Beenden eine Partitur geladen (`looper.loadedScore` im
Fenster), wird sie nach dem Autostart wieder geladen, sofern dieselbe Datei offen ist, noch
existiert und fehlerfrei übersetzt; die Engine wird gleich mit ihren Spuren gebaut. Sonst eine
Notiz und freier Modus.

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

**Seit den getrennten Bussen gibt es eine Kette je Bus, und das widerspricht dem Satz darüber
nicht.** Wenn Loop und Mithörsignal auf *verschiedene* Busse gelegt werden, führen die beiden Busse
verschiedene Signale, und ein Kompressor oder ein Hall lässt sich nachträglich nicht wieder
auseinandernehmen: `Kette(a + b)` ist nicht `Kette(a)` plus `Kette(b)`. Zwei verschiedene Signale
brauchen zwei Zustände. Die alte Warnung galt zwei Ketten auf *demselben* Signal — die wären ein Weg,
Loop und Livestimme verschieden klingen zu lassen; hier ist es umgekehrt. **Solange beide Busse
dieselben Quellen sehen, rechnet nur die erste Kette** und ihr Ergebnis bedient beide; die zweite
bekommt Stille und kostet den Leerlaufpreis aus der Tabelle unten. Eine gewöhnliche Sitzung zahlt
also genau das, was sie vorher gezahlt hat. Der Speicher verdoppelt sich auf rund 1,8 MB je Track,
was neben den Ebenen-Puffern nicht ins Gewicht fällt.

### Reihenfolge und Presets

Oktaver, Hochpass, drei Bänder parametrischer EQ, Kompressor, Delay, Hall - die Reihenfolge aus
dem Plan, mit dem Oktaver davor (Begründung unten). Die Reihenfolge der eingebauten Geräte ist
fest; Plugins lassen sich seit 2026-11 vor jedes von ihnen setzen (Abschnitt 14, „Plugins in der
Kette“). Das oberste EQ-Band ist als Kuhschwanz
ausgelegt: "Luft" ist das ganze obere Ende, nicht eine Frequenz. Der Kompressor hat ein weiches
Knie (6 bis 8 dB), weil eine Stimme *an* der Schwelle lebt und ein hartes Knie mehrmals pro Sekunde
umschalten würde.

Vier Presets, jeder Wert im Code begründet: `stimme` (80 Hz Hochpass, Präsenz +2,5 dB bei 3 kHz,
3:1 ab -18 dBFS mit +6 dB Makeup, 18 % Hall), `gitarre` (100 Hz Hochpass, -4 dB Quäkband bei
3 kHz, 2,5:1 mit langsamem Attack, 10 % Hall), `bass` (nur der Oktaver: `dry` 0, `sub` 1, Tone
8 kHz, Drive 0; alles andere aus), `trocken` (alles aus, Kette umgangen). In der Partitur heißen
sie `voice`, `piezo_guitar`, `bass_from_guitar`, `dry`.

**`bass` ist am 2026-11-21 nach Gehör festgelegt.** Zuerst war er nach Messung „wie ein Bass"
gestimmt (Drive 0,4, Tone 2,5 kHz, Hochpass 30 Hz, +2 dB bei 70 Hz und 800 Hz, -4 dB Kuhschwanz
ab 3 kHz, 4:1 ab -20 dBFS mit 10 ms Attack und +6 dB Makeup). Im Hörvergleich am selben Stem hat
die nackte Oktave gewonnen; das Preset ist jetzt genau dieser Render,
`{octaver: {on: true, dry: 0, sub: 1, tone: 8000}}` auf einer trockenen Kette - offline nachgeprüft
bytegleich. Die gestimmte Variante bleibt als `mixer:`-Eintrag zum Ausprobieren:

```yaml
bass:
  preset: bass_from_guitar
  octaver: {tone: 2500, drive: 0.4}
  high_pass: {on: true, hz: 30}
  eq: {on: true}
  band1: {hz: 70, q: 0.9, gain: 2}
  band2: {hz: 800, q: 0.8, gain: 2}
  band3: {kind: high_shelf, hz: 3000, gain: -4}
  comp: {on: true, threshold: -20, ratio: 4, attack: 10, release: 120, knee: 6, makeup: 6}
```

### Oktaver (`engine/src/engine/fx/octaver.rs`)

Eine Oktave tiefer, gebaut für einstimmig gespielte (Piezo-)Gitarre, die nach Bass klingen soll.

**Verfahren: halbe Geschwindigkeit, auf die Periode geschnitten.** Eine Oktave tiefer ist das
Eingangssignal mit halber Geschwindigkeit gelesen - jede Frequenz exakt halbiert, ohne einen
Tonhöhendetektor im Signalweg, der sich verschätzen könnte. Der Lesezeiger fällt dabei um einen
halben Sample je Sample zurück und muss immer wieder nach vorn springen. Springt er um eine
beliebige Strecke, landen die beiden übergeblendeten Abgriffe an verschiedenen Stellen der
Schwingung (das „Leiern" einfacher Pitch-Shifter); springt er um ganze Perioden, tragen beide
dieselbe Schwingung und die Überblendung ist unhörbar. Die Sprungweite wird deshalb im Moment des
Sprungs per normierter Korrelation gesucht - grob auf einer 12-kHz-Kopie, fein auf voller Rate -
zwischen einer halben und einer ganzen längsten Periode (70 Hz); unter den annähernd besten
*Spitzen* gewinnt die längste. In diesem Intervall liegt für jeden Ton bis 70 Hz ein Vielfaches
seiner Periode; welches gefunden wird, ist egal, Oktavfehler gibt es also nicht. Anders als PSOLA
verschiebt das die Formanten mit - der Korpus klingt eine Oktave tiefer, wie eine dickere Saite
in einem größeren Instrument, statt wie eine Gitarre mit Sub-Oszillator. Gewählt, weil es für
Einzeltöne am stabilsten ist (kein Tracker, der auf langen Tönen schwankt) und für Akkorde
gutmütig bleibt (verschmiert, aber nie lauter oder instabil).

**Anschläge.** Ein Detektor (schnelle gegen langsame Hüllkurve auf einem 300-Hz-hochpassgefilterten
Abbild, Schwelle +6 dB und -46 dBFS, 60 ms Sperrzeit) setzt bei jedem Anschlag den Lesezeiger
direkt vor den Anschlag und blendet den alten Ton in 3 ms aus. Jeder Ton beginnt daher gleich
spät nach dem Anschlag - das ist die Latenz -, und das Ende des alten Tons wird nicht in den
neuen hineingeblendet (kein „Gurgeln"). Hinter dem Anschlag wandert die Verzögerung des
Tonhaltes zwischen etwa 3 und 15 ms, was auf einem klingenden Ton nicht zu hören ist.

**Regler:** an/aus, `dry` (Original, 0..1), `sub` (Oktave, 0..1), `tone` (Tiefpass 12 dB/Okt. auf
der Oktave, 100 Hz bis 8 kHz), `drive` (0..1). Drive ist eine Sättigung **auf die eigene Hüllkurve
normiert**: das Signal wird durch seine Hüllkurve geteilt, hinter bis zu +24 dB weich begrenzt
und wieder mit ihr multipliziert. Dieselbe Einstellung macht so bei -30 dBFS dieselben Obertöne
wie bei -6 dBFS. Gebaut, weil die Messung es verlangt hat: eine Oktave tiefer hat die Piezo-Gitarre
über ein paar hundert Hertz kaum noch etwas (bei 1 kHz rund 30 dB unter dem 100-Hz-Band), und ein
EQ kann nicht anheben, was nicht da ist; ein gewöhnlicher Clipper vor einem festen Gain hat bei
realistischen Pegeln nichts bewirkt. Drive 0,4 hebt 300 Hz bis 1 kHz gegenüber dem Grundton um 6
bis 10 dB.

**Latenz und Ausgleich.** 144 Frames bei 48 kHz (3,0 ms; allgemein zweimal 1,5 ms Vorlauf, weil
der Vorlauf mit halber Geschwindigkeit gelesen wird). Auch der `dry`-Anteil wird um genau diese
Zeit verzögert, damit Original und Oktave zusammenbleiben; der ganze Oktaver ist also eine Stufe
mit fester Latenz. `Chain::latency()` meldet sie, solange der Oktaver an und die Kette nicht
umgangen ist, und `process::collect_inserts` zählt sie zur Plugin-Latenz des Tracks: **der Loop
wird um genau so viel früher gelesen und landet auf dem Raster** (Test
`the_octavers_latency_is_made_up_for_on_the_loop`: mit Oktaver als reiner Verzögerung ist die
Wiedergabe ab dem zweiten Durchgang sample-gleich mit der ohne). **Nicht ausgeglichen**: das live
Mitgehörte kommt 3 ms später (es lässt sich nicht vorauslesen - wie hinter einem Plugin mit
Latenz); beim Ein- und Ausschalten springt die Lesestelle des Loops um 144 Frames, was in der
20-ms-Überblendung des Schalters liegt; und die wandernde Verzögerung des Tonhaltes (bis etwa 12
ms nach dem Anschlag) ist bewusst nicht ausgeglichen, weil das Raster an den Anschlägen hängt. Die
Aufnahme bleibt trocken wie immer.

**Gemessen** am Bass-Stem der Skizze 124 (eine auf der Gitarre gespielte Basslinie, 40 s,
YIN-Tonhöhe alle 10 ms): in 99,2 % der stimmhaften Frames liegt der Ausgang innerhalb ±3 % bei der
halben Eingangsfrequenz (Median-Verhältnis 0,4999); die Tonhöhe schwankt von Frame zu Frame im
Median um 1,0 Cent (Eingang 0,4), im 95. Perzentil um 11 Cent; Ausreißer über 50 Cent gibt es nur
an Tonwechseln (16 von 2373 Frames). Das ist zugleich die Messung des heutigen Bass-Presets (nur
Oktaver); die frühere gestimmte Variante (oben) brachte den Pegelabstand zwischen leisen und lauten
Tönen (10./90. Perzentil des 0,5-s-RMS) von 6,6 dB auf etwa 3,5 dB, klang aber schlechter. Auf einer
Melodiestimme mit nachklingenden Saiten sind es noch 96,6 % Oktave, aber das 95. Perzentil der
Schwankung steigt auf 15 bis 28 Cent - dort ist es hörbar unruhiger. Nachmessen:
`looper-engine fxrender <stem> <out> --mix "{preset: bass_from_guitar}"` rendert offline durch
genau die Kette, die eine Partitur einstellt.

**Grenzen.** Akkorde und zwei gleichzeitig klingende Saiten kommen verschmiert heraus. Bendings und
Vibrato werden mitgenommen (die Oktave ist exakt die halbe Frequenz), aber die Splice-Suche
findet dann schlechtere Übereinstimmungen. Gebundene Töne (Hammer-on, Slide) lösen keinen
Anschlag aus; der neue Ton erscheint dann erst, wenn der Lesezeiger ihn erreicht - im ungünstigsten Fall rund 30 ms später, und eine Überblendung kann kurz beide Töne mischen. Unter 70 Hz
Eingang (tiefer als Drop-D) gibt es keine periodensynchronen Sprünge mehr.

**Warum vorn in der Kette.** Alles danach formt einen Klang, und zu formen ist der eine Oktave
tiefer: ein Hochpass davor nähme der Gitarre ihr tiefes E (82 Hz), bevor es zu den 41 Hz des
Basses werden kann; ein EQ davor säße eine Oktave zu hoch; ein Kompressor davor glättete die
Gitarre statt des Basses. Und als einziges Glied mit Latenz verzögert er vorn einfach alles
dahinter gleich - genau das setzt der Ausgleich voraus.

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

**Oktaver** (`looper-engine fxbench`, Release, 48 kHz, 2026-11-21 - ein anderer
Lauf als die Tabelle oben, daher eigene Zeilen; die Hochpass-Zeile desselben Laufs zum Vergleich):

| | ns/Frame | % je Track | % bei 8 Tracks |
|---|---:|---:|---:|
| Oktaver (Drive an, Splice-Suche gemittelt) | 16,8 | 0,08 | 0,64 |
| Hochpass (derselbe Lauf) | 6,7 | 0,03 | 0,26 |
| Kette „bass" (seit 2026-11-21 nur Oktaver, ohne Drive) | 18,0 | 0,09 | 0,69 |
| Kette komplett, alle sechs | 78,1 | 0,37 | 3,00 |

Die Suche nach der Sprungweite kostet rund 20 000 Multiplikationen auf einmal, höchstens einmal je
14 ms je Track - im ungünstigsten Callback ein paar Mikrosekunden zusätzlich.

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
| nicht `stack` | `stack` | `LatchOverdub an`, dann der erste Durchlauf wie `record` (leere Spur) bzw. `overdub` — jeder weitere Durchlauf ist Sache der Engine |
| `stack` | `stack` | **keins** — der Stapel läuft einfach weiter |
| `stack` | `play` | nur `LatchOverdub aus` mitten im letzten Durchlauf |
| `stack` | `record` / `overdub` | `LatchOverdub aus` + `StopRecord` am Ende des letzten Durchlaufs, dann wie oben |
| klingt schon | `play` | **keins** (mit `restart: true`: `RestartLoop`, wenn die Sektion nicht auf seinem Raster beginnt) |
| still, mit Inhalt | `play` | `StartPlay`, davor `RestartLoop`, wenn die Sektion nicht auf seinem Raster beginnt (unten) |
| klingt | `stop` / `hear_through` | `StopPlay` |
| still | `stop` | **keins** |

Ein Track, der über fünf Sektionen auf `play` steht, bekommt **null** Kommandos; einer, der
durchgehend `stop` steht, ebenso. Das ist keine Sparsamkeit, sondern Klangschutz: Jedes
überflüssige Kommando ist ein Zustandswechsel im Audio-Thread an einer musikalisch exponierten
Stelle — die Sorte Fehler, die man erst nach zwanzig Minuten Spielen als Knacksen bemerkt.

**Mithören** folgt demselben Prinzip: an bei `record`, `overdub`, `stack` und `hear_through`, aus bei `play`
und `stop`, und nur, wenn die Partitur es dem Track überhaupt erlaubt (`monitor:`). Ein Wechsel
wird nur geschickt, wenn er den Zustand wirklich ändert.

### Ein Loop, der einsetzt, beginnt vorn (`RestartLoop`, `restart: true`)

Ein Loop wird als `(Position − origin) mod loop_len` gelesen und bleibt deshalb vom ersten Take an
phasenstarr zum Song. Das ist im freien Modus richtig (Abschnitt 3: Stopp und Wiedergabe verlieren
nichts) — in einer Partitur aber falsch, sobald eine Sektion kein Vielfaches der Looplänge ist. Der
Fall des Besitzers („The Hardest Part", 92 BPM): `theme` und `bass` sind Zwei-Takt-Loops, die
Sektion `verse_1_b` ist **einen** Takt lang und lässt beide schweigen, und im folgenden Refrain
setzten sie mit ihrem **zweiten** Takt ein und blieben bis zur nächsten Ein-Takt-Sektion versetzt.
Belegt vor der Änderung mit dem Test `a_loop_that_comes_back_after_a_one_bar_section_starts_from_its_top`
(`runner/restart_tests.rs`): genau ein Takt daneben. Ein Musiker erwartet: Setzt ein Loop mit einer
Sektion ein, beginnt er vorn.

**Die Regel (Runner, `Runner::restarts`).** Ein Track wird am Anfang einer Sektion neu verankert —
`origin` = erste Sample der Sektion —, wenn

1. er in der Sektion seinen Loop spielt: `play`, oder `overdub`/`stack` auf einen schon
   vorhandenen Loop (nicht ein `stack`, der aus der Vorsektion einfach weiterläuft),
2. er **einsetzt** — in der Sektion davor klang er nicht (`stop`, `hear_through`, noch nie
   bespielt) — oder der Clip es ausdrücklich verlangt: `theme: {state: play, restart: true}`,
3. und die Sektion **nicht ohnehin** auf seinem Raster beginnt. Gefragt in Takten
   (`(Sektionstakt − Ursprungstakt) mod Looptakte`): Ein Loop ist immer ganze Takte, sein Ursprung
   immer ein Sektionsanfang.

Bedingung 3 ist die Bestandsgarantie: Eine Partitur, deren Sektionen Vielfache ihrer Loops sind,
erzeugt **kein einziges** neues Kommando und klingt bitgleich wie vorher. Das beweist
`runner/golden_tests.rs`: Henrys 3/4-Partitur, der Walzer, die Solo-Partitur und eine Partitur mit
Pause, Wiederkehr auf dem Raster, Overdub und Release laufen durch die Engine, und ein Hash über
jedes Sample beider Busse muss gleich dem Wert sein, den derselbe Test auf `origin/octaver`
(2a135a0, vor der Änderung) ergab.

**Entscheidungen zu den Randfällen:**

| Fall | Verhalten |
|---|---|
| Track spielt über die Grenze weiter | **kein** Neuanfang (unverändert), außer `restart: true` |
| Wechsel per Release, `quantize: bar` oder `loop`, per `goto` | verankert auf der Sample, auf der die Sektion **wirklich** beginnt — wo auch immer der Release sie hingelegt hat |
| Sektion ohne `autorelease` loopt | nur beim Betreten der Sektion, nicht in jedem Durchlauf |
| mehrere Tracks setzen an derselben Grenze ein (theme + bass) | alle auf dieselbe Sample verankert, also untereinander phasengleich. Ein Track, der *weiterspielt*, behält seine Phase — er kann danach gegen die eingesetzten versetzt sein; wer das nicht will, schreibt bei ihm `restart: true` |
| `overdub` auf einen einsetzenden Track | erst neu verankert, dann die Ebene auf dem neuen Raster: sie beginnt am Sektionsanfang |
| Take, der genau an der Grenze endet (`overdub` in einer Sektion kürzer als der Loop, dann `restart: true`) | seine letzten Frames kommen erst `R` später herein und landen noch auf dem **alten** Raster: der Ursprung gilt je Position, nicht ab einem Zeitpunkt (unten) |
| Take, der über die Grenze läuft | kann es nicht geben (ein Wechsel landet nie in einem laufenden Take); die Engine lehnt den Neuanfang unter einem offenen Loop-Take als `Busy` ab |
| `restart` auf `record` | **Fehler** — der neue Take legt den Ursprung ohnehin an den Sektionsanfang |
| `restart` auf `stop` / `hear_through` | **Fehler** — es spielt kein Loop |
| `restart: false` | erlaubt, sagt nichts; der Text schreibt es nicht zurück |
| Live-Ansicht ohne Partitur | unverändert — nur der Runner schickt `RestartLoop` |

**Wie es gebaut ist (Engine).** `Command::RestartLoop { track, at, play }` legt den neuen Ursprung
**neben** den alten (`LoopGrid::restart`) statt ihn zu überschreiben: Wiedergabe und Aufnahme
erreichen `at` zu verschiedenen Zeiten — die Wiedergabe wird für Plugin- und Kettenlatenz (der
Oktaver von `bass_from_guitar`!) um `playback_advance` vorausgelesen, die Aufnahme läuft um `R`
hinterher. Deshalb geht jede Position durch `LoopGrid::origin_at(pos)`: vor `at` der alte, ab `at`
der neue Ursprung — beim Lesen (`Track::read`), beim Schreiben (`write_index`) und beim Abspielen
der MIDI-Ebenen. Das Kommando wirkt **früh**, sobald der Vorauslesezeiger `at` erreicht
(`EngineCore::next_due` zieht die Vorauslese des Tracks ab; bei einem Audio-Loop zusätzlich die
10 ms, über die altes und neues Raster überblenden, Abschnitt 19); `play: true` lässt den Track ab `at`
auf der Leseachse klingen (`Track::sounds_at`), das `StartPlay` für dieselbe Sample folgt wie
immer. So beginnt ein vorausgelesener Loop mit seinem ersten Frame, statt die ersten
`playback_advance` Frames — den Anschlag auf der Eins — zu verlieren. Der nächste Neuanfang faltet
den vorigen in `origin`; Leeren, ein neuer Loop und ein Transport-Start verwerfen ihn. Gebundene
Arbeit: ein paar Speicherungen, nichts alloziert.

**MIDI-Loops.** `play_midi` teilt sein Fenster an `at`: davor das alte Raster (nur wenn der Track
spielt), ab da das neue. Alles, was die alte Phase klingen ließ, bekommt genau auf `at` sein
Note-off — vor den ersten Noten der neuen Phase; das Note-off aus dem übersprungenen Teil des Loops
käme sonst nie, und die Note hinge. Bewiesen in `midi_tests.rs` mit und ohne Instrumentlatenz
(`a_midi_loop_re_anchored_mid_note_leaves_nothing_hanging`, `a_midi_loop_that_comes_back_plays_its_first_note`).

**MIDI-Loops: ein Akkord, über das Loop-Ende gehalten** (Fix 2026-11-22, Bericht des Besitzers:
Pad-Akkord über vier Takte gehalten, beim Wiederholen nichts zu hören). Drei Ursachen, alle drei
behoben:

* *Länge.* Eine Note, die länger als der Loop gehalten wird, endet ein Frame vor dem Ende des
  Durchlaufs, **für den sie gespielt wurde** — ein Anschlag im Vorlauf-Fenster (eine
  Zweiunddreißigstel vor der Eins) gehört zu dem Durchlauf, der dort beginnt — und nie später als
  ein Frame vor ihrem eigenen nächsten Anschlag (`midiloop::note_end`). Vorher rechnete der Vorlauf
  zum Durchlauf davor: der leicht früh gedrückte Akkord endete eine Millisekunde nach seinem Anfang,
  und ein im Take selbst kurz vor dem Ende gelöster wurde gar nicht gekürzt (Off hinter dem
  eigenen On). Gilt im Take, im Nachlauf und beim Schließen des Nachlaufs.
* *Eine Taste, eine Stimme.* Live-Taste und MIDI-Ebenen spielen dasselbe Instrument, und MIDI kennt
  keine Noten-Ids: schlägt der Loop eine Taste an, die der Musiker noch hält, und lässt er sie
  danach los, beendete sein Off die Note des Loops. Die Event-Stufe (`source::shared_key`) schlägt
  eine Taste, die eine andere Quelle hält, sauber neu an (Off, On) und lässt ein Off durch, erst
  wenn keine Quelle die Taste mehr hält. Ohne Raster liegt der Anschlag eines früh gedrückten
  Akkords vor dem Beginn der Wiedergabe; dann übernimmt der Loop die gehaltene Taste
  (`adopt_held`, `SourceRack::adopt`), und sein eigenes Off beendet sie.
* *Neue Abspielliste.* Mit Raster baut der Steuer-Thread die Liste neu, sobald das späte Off
  ankommt; der Tausch beendete alles, was die alte Liste klingen ließ — mitten im Akkord. Jetzt
  bleibt, was die neue Liste an dieser Stelle selbst klingen lässt (`NoteTracker::release_except`).

Offen, bewusst: ohne Raster und im Take gelöst hat der erste Durchlauf nach der Aufnahme nichts
vom früh gedrückten Akkord (sein Anschlag lag vor dem Beginn der Wiedergabe), ab dem zweiten
klingt er durch. Kanaldruck und Controller werden wie gespielt wiederholt, auch ein Druck 0 beim
Loslassen vor dem Ende; im Nachlauf wird nur Note-Off und Pedal-Hoch aufgenommen. Bewiesen in
`midi_tests.rs` (`a_chord_held_over_the_end_sounds_for_the_whole_loop_in_every_pass`: drei
Anschlagzeiten mal vier Loslass-Zeiten, mit und ohne Raster, danach Stopp ohne hängende Note;
Overdub; eine Stimme je Taste).

### Ein Take kürzer als die Sektion (`{state: record, bars: 2}`)

Der zweite Wunsch des Besitzers: im sechstaktigen Refrain nimmt `chorus_guitar` die Takte 1–2
auf und spielt sie als Zwei-Takt-Loop für 3–6, während `mic` alle sechs Takte in einem Take
aufnimmt. Ein Take kann keine Sektionsgrenze überqueren, also musste beides in eine Sektion:

```yaml
  - id: chorus_1
    bars: 6
    tracks:
      chorus_guitar: {state: record, bars: 2}   # zwei Takte, dann der Loop
      mic: record                               # alle sechs Takte
```

Der Runner plant den Take über `bars` Takte ab Sektionsanfang (`StartRecord`, `StopRecord` und
`StartPlay` am Takeende), die Looplänge sind diese `bars`, der Ursprung der Sektionsanfang. Ohne
`bars` ändert sich nichts.

| Frage | Entscheidung |
|---|---|
| Gültige Werte | `1 … bars der Sektion`; gleich der Sektionslänge ist erlaubt (wie ohne). Mehr: **Fehler** „records 8 bars in Section 'x', but the section has only 6" — auch, wenn die Sektion ihre Länge über `repeat:` bekommt (geprüft auf den aufgelösten Sektionen) |
| Teilt die Sektion nicht (3 in 4) | erlaubt; der Loop läuft einfach weiter. Spielt der Track in der Folgesektion weiter, behält er seine Phase (kein Neuanfang); setzt er nach einer Pause wieder ein, beginnt er vorn (Regel oben) |
| `autorelease: false` | der Take endet nach seinen Takten, der Loop spielt bis zum Release. Ein Release während des Takes wird wie immer an sein Ende geschoben |
| `overdub` | **Fehler** — ein Overdub dauert immer genau einen Durchlauf des Loops |
| `stack` | **Fehler** — `stack` nimmt Durchlauf um Durchlauf bis zum Sektionsende auf |
| `play` / `stop` / `hear_through` | **Fehler** — `bars` ist die Länge eines Takes |
| MIDI-Loop, Clip-`grid` | wie Audio: derselbe `record_at` des Schedulers, das Notenraster ist davon unabhängig |
| Einzähler | keine Wechselwirkung — Sektionen beginnen nach ihm |
| Mitschnitt (Stems, Marker) | unverändert: der Take ist ein Take, die Marker hängen an Sektionen |
| `repeat:` | erbt der Clip den Zustand, erbt er seine Optionen mit; nennt die Sektion den Track selbst, gelten nur ihre eigenen |

Bewiesen in `runner/restart_tests.rs`: `a_short_take_and_a_whole_section_take_share_one_section`
(Zwei-Takt- und Sechs-Takt-Take in einer Sektion, mit und ohne `autorelease`, sample-genau und
phasenrichtig in der Folgesektion) und `a_short_take_that_does_not_divide_its_section_runs_on`.

**Format.** Beide Optionen stehen in der Clip-Form neben `grid`, in fester Reihenfolge
(`{state: play, grid: 1/16, restart: true}`, `{state: record, bars: 2}`), im kompilierten Score als
`CompiledSection.clips` (nur Clips, die etwas sagen), im Dokument als `DocSectionTrack.restart` /
`.bars`. `score --check` zeigt sie hinter dem Zustand: `theme=play(restart)`,
`chorus_guitar=record(2 bars)`. Im Arrangement zeigt der Clip „2 of 6 bars" bzw. „from top", und
der Clip-Inspektor hat *Take length* (bei Record) und *Restart the loop here* (bei Play, Overdub,
Stack). Wechselt ein Clip den Zustand, fallen Optionen weg, die der neue nicht nimmt.

### Loop spielt, Eingang ist zu hören (`{state: play, monitor: true}`)

Der Wunsch: im letzten Refrain eine dritte Stimme live über Lead und Backings singen — alle acht
Tracks sind belegt, also gibt es keinen freien Track für `hear_through`. `mic: {state: play,
monitor: true}`: der Loop spielt wie bei `play`, **und** der Live-Eingang des Tracks ist zu hören —
durch seine Effektkette, auf seinem Monitor-Bus, genau wie bei `hear_through`. Aufgenommen wird
nichts. Der Runner schaltet das Mithören an der Sektionsgrenze (wie bei jeder Sektion), der Loop
läuft ohne Lücke weiter. Im Mitschnitt erscheint der Eingang so wie bei einem `hear_through`-Track
(der Mitschnitt nimmt den Live-Eingang jedes Tracks, dessen Mithören an ist). Die Stummschaltung
des Mithörens bei eingebauten Lautsprechern gilt auch hier (sie wirkt in der Engine auf jedes
Mithören). „Stop all" schaltet es mit allem anderen aus.

| Frage | Entscheidung |
|---|---|
| `record`, `overdub`, `stack` | **Fehler** „'monitor' on record does nothing" — ein Take lässt den Eingang ohnehin hören |
| `hear_through` | **Fehler** — das ist der Eingang allein |
| `stop` | **Fehler** — dazu gehört ein Loop, der spielt; der Hinweis nennt `hear_through` |
| Track mit `monitor: false` | **Fehler** an der Clip-Option: beides zugleich kann nicht gelten, eins muss weg |
| Instrument-Track | **Fehler** — die Sektionen schalten sein Mithören nie; das tut sein Monitor-Schalter (Startwert `monitor`) |
| `monitor: false` am Clip | sagt nichts, überall erlaubt |
| mit `restart: true` | geht zusammen |

`score --check`: `mic=play(monitor)`; Dokument `DocSectionTrack.monitor`, Reihenfolge `grid, bars,
restart, monitor`; im Arrangement „+ input" am Clip und *Hear the input over the loop* im
Clip-Inspektor (nur bei Play, nicht bei Instrument-Tracks). Bewiesen in `score/clip_tests.rs` und
`runner/overdub_tests.rs` (Loop lückenlos, Mithören an und wieder aus, keine Ebene dazu, Stop all).

### Mithören von Anfang an (`monitor: always`)

Der Besitzer will sich durch die App hören, sobald die Partitur geladen ist — beim Prüfen des
Klangs vor Start, im Einzähler, in jeder Sektion —, nicht nur wo eine Sektion aufnimmt oder
`hear_through` sagt. `tracks: {theme: {input: 2, monitor: always}}`. `monitor:` heißt sonst
weiter „darf überhaupt mitgehört werden" (`true`, Standard) bzw. nie (`false`).

**Modell: je Track, mit Vorrang je Eingang** (`runner::monitor_plan`). Erst bestimmt die Sektion,
wer mithört (Takes, `hear_through`, `play` mit `monitor: true`; nie ein Track mit `monitor:
false`). Dann bekommt jeder `always`-Track sein Mithören, **wenn kein schon mithörender Track einen
seiner Eingangskanäle teilt**; von zwei `always`-Tracks auf einem Eingang gewinnt der erste. So
hört er im Bass-Take die Kette des Basses, sonst die des `always`-Tracks — nie den Eingang doppelt.
Erwogen und verworfen: ein Mithören je *Eingang* statt je Track. Das Mithören läuft aber durch die
Effektkette und den Monitor-Bus eines Tracks — welcher Klang zu hören ist, *ist* die Wahl des
Tracks; ein Eingangs-Modell müsste genau diese Wahl wieder einführen.

| Phase | `always`-Track |
|---|---|
| geladen / „Back to start" | an (`setup_commands` schaltet ihn ein, das Bild des Runners weiß es) |
| Einzähler, Sektionen | an, außer ein anderer Track auf seinem Eingang hört gerade mit |
| Ende der Partitur | an |
| „Stop all" | **aus** — die Notbremse macht alles still; „Back to start" schaltet ihn wieder ein |
| Instrument-Track | **Fehler** — er ist ohnehin zu hören, solange er spielt |

Der Wechsel zwischen zwei Tracks auf einem Eingang geschieht im selben Steuer-Takt (erst aus,
dann an); mit `monitor_fade` (unten) wird er eine echte Überblendung. Die Stummschaltung bei
eingebauten Lautsprechern wirkt in der Engine auf jedes Mithören, also auch hier. Anzeige:
`score --check` „Monitor always (from the load on)"; Dokument `DocTrack.monitor_always`
(`monitor: always`); im Arrangement wählt der Track-Inspektor *Allowed / Always / Off*. Bewiesen in
`runner/monitor_tests.rs` (vor Start, Einzähler, Take auf demselben Eingang, nie beide zugleich,
Stop all, Back to start, Fehler).

### Das Mithören einblenden (`monitor_fade`)

„Man sollte das Mithören kurz vorher einblenden können, von −∞ dB bis zum Zielpegel." `monitor_fade:
{in: 2, out: 1}` in Schlägen (Vierteln, Dezimalen erlaubt, 0–16) — für die ganze Partitur und je
Track (`tracks: {mic: {input: 1, monitor_fade: {in: 4}}}`, Seite für Seite über der Partitur).
Ohne Angabe wie bisher: an der Grenze geschaltet.

* **Engine:** `Command::MonitorFade { track, at, on, frames }`, zeitgestempelt. Ein schaltet das
  Mithören bei `at` ein und bringt es über `frames` von Stille auf vollen Pegel; aus bringt es ab
  `at` herunter und schaltet am Ende ab. Die Kurve ist quadratisch in der Amplitude (eine Gerade
  klingt, als setze sie schlagartig ein). Nur das **Gehörte** wird geblendet
  (`monitor_frame`); ein Take nimmt den Eingang trocken und voll auf — bewiesen: der Take nach einem
  Einblenden ist ab seinem ersten Frame auf vollem Pegel. Ein `SetMonitor` beendet eine laufende
  Blende.
* **Runner** (`schedule_monitor_fades`, beim Armieren): Wer an der Grenze `at` angeht, beginnt
  `in` Schläge davor — **nie früher als der Augenblick des Armierens plus Kopfstart**: nach einem
  späten Release, oder wenn die Blende länger als der Einzähler ist, wird sie kürzer und ist
  trotzdem genau auf der Grenze voll. Wer ausgeht, blendet ab `at` über `out` Schläge aus. Gibt ein
  Track seinen Eingang an einen ab, der eingeblendet wird (`always` weicht einem Take), blendet er
  über genau dieselbe Strecke aus — eine Überblendung. Diese Tracks schaltet der Runner an der
  Grenze nicht mehr selbst, er führt nur sein Bild nach.
* **„Stop all"** schickt jedem Track ein `SetMonitor { on: false }`, gleich was sein Bild sagt — eine
  schon laufende Einblendung wird damit beendet, die wartenden holt `DropWaiting` zurück.

Anzeige: `score --check` „monitor fades in 2 / out 1 beats" am Track; Dokument `monitor_fade` oben
und am Track. Je Clip gibt es die Blende (noch) nicht, und im Arrangement wird sie nur im Text
bearbeitet. Bewiesen in `runner/monitor_tests.rs` (Verlauf vor der Grenze, voll auf ihr, Ausblenden
danach, Take trocken, nach Release, länger als der Einzähler, Stop all mitten in der Blende).

### Das Ende eines Takes auslassen (`skip_end`)

Am Ende einer aufgenommenen Sektion singt oder spielt der Besitzer oft schon den Auftakt in den
nächsten Teil; der landete im Loop und klang bei der Wiedergabe falsch. `mic: {state: record,
skip_end: 2}`: die letzten zwei Schläge (Viertel, Dezimalen erlaubt: `1.5`) des Takes werden nicht
aufgenommen. Der Loop behält seine volle Länge, sein Ende ist still; davor die Kanten-Blende des
Loops (10 ms, halbe Kosinus-Periode, Abschnitt 19), damit die Ebene nicht in einem Knack endet.
Gebaut war sie zuerst als gerade 2-ms-Rampe; deren Knicke biegen tieffrequentes Material so hart,
dass es bei jeder Wiedergabe knackt (`clean_tests::skip_end_in_the_owners_score_does_not_click_on_playback`,
46 Stellen über der Schranke) — deshalb dieselbe Blende wie an jeder anderen Kante. Der Eingang
bleibt währenddessen zu hören (das Mithören des Takes läuft weiter), und der Mitschnitt hat ihn
ungeschnitten — beschnitten wird nur, was in die Ebene geschrieben wird.

* **Engine:** `Command::CutTake { track, at, from }`, vom Runner direkt hinter den Kommandos des
  Takes geschickt (`at` = Start des Takes, `from` = Ende − skip). Ab der musikalischen Position
  `from` schreibt der Take Stille (`EngineCore::cut_gain` beim Schreiben, in `record_frame`), er
  endet weiter dort, wo sein `StopRecord` sagt. Weil die Position musikalisch ist, sitzt der
  Schnitt latenzkompensiert genau auf dem Sample (bewiesen durch Vergleich mit demselben Take ohne
  Option, `runner/overdub_tests.rs`). Gilt nur für den Take, der bei `at` beginnt; ein späterer
  ignoriert ihn. Diese Blende liegt in der Aufnahme; die Wiedergabe blendet zusätzlich nichts —
  vor der Naht liegt bei einem beschnittenen Loop Stille, die Nahtblende führt aus ihr in den
  Vorlauf des Takes (Abschnitt 19). Keine doppelte Blende, kein Sprung am Schnitt.
* **MIDI-Loop:** keine neue Note ab dem Schnitt; eine gehaltene Note endet auf dem Schnitt, auch
  wenn ihr Note-Off erst nach dem Take kommt (`OpenNotes::cut`, auch im Nachlauf). Bewiesen in
  `midi_tests.rs` (mit und ohne Instrument-Latenz).
* **Wo:** `record` (mit und ohne `bars:`) und `overdub` (bei einem Durchlauf, den seine Sektion
  kürzt, höchstens bis zum Take-Anfang). `stack`, `play`, `stop`, `hear_through`: **Fehler** — ein
  Stapel nimmt Durchlauf um Durchlauf auf, jeder bräuchte sein eigenes Ende.
* **Standard je Track:** `tracks: {mic: {input: 1, skip_end: 1}}` gilt für jedes `record`/`overdub`
  dieses Tracks; der Clip überschreibt ihn, `skip_end: 0` am Clip schaltet ihn aus.
* **Prüfung:** 0 … 64; kürzer als der Take (bei `record` die `bars` des Clips bzw. der Sektion, bei
  `overdub` die Sektion), sonst Fehler mit Zeile — am Clip oder am Track-Standard.
* **Anzeige:** `score --check` zeigt `mic=record(skip_end 2)` und am Track „takes skip the last 1
  beats"; im Arrangement „last 2 out" am Clip und *Leave out the end* im Clip-Inspektor (Record,
  Overdub; leer = Track-Standard). Der Track-Standard wird bisher nur im Text bearbeitet.

### Ein Overdub bleibt in seiner Sektion — und hört den Loop, auf den es schichtet

Zwei Fehler, gefunden im Lauf des Besitzers vom 21.11. 14:57 (`chorus_3` 4 Takte und
`chorus_3_all` 6 Takte, beide `backing_vocals: overdub`, der Loop 10 Takte aus `chorus_2`):

1. **Ein Overdub auf einem Loop, der nach einer Pause wiederkommt, war stumm.** Die Engine nimmt
   eine Ebene auch über einen Loop auf, der nicht spielt; der Runner schickte das `StartPlay` erst
   am Takeende. In `chorus_3` lief also der ganze Overdub über einem stummen Loop („the backing
   vocals do not play from the start"); was in Takt 1–2 im Stem zu hören war, ist sehr
   wahrscheinlich die Live-Stimme über das Mithören des Overdubs — die Simulation zeigt den Loop
   ab Sample 200 der Sektion stumm. Jetzt startet `Runner::comes_back` den Loop auf dem ersten
   Sample der Ebene (das ist der Sektionsanfang, denn ein wiederkommender Loop wird dort neu
   verankert; die Neuverankerung trägt dann `play: true`). Gilt für `overdub` und `stack`.
2. **Ein Overdub ohne Durchlauf in seiner Sektion lief in die nächste.** Ein Overdub beginnt auf
   dem eigenen Raster des Loops. Spielt der Loop über die Grenze weiter (kein `restart`), liegt der
   nächste Rasterpunkt womöglich hinter dem Sektionsende — in `chorus_3_all` lag er auf dessen
   Ende, und der Take wurde trotzdem geplant (Start nach dem Ende, `StopRecord` davor), ebenso
   wurde ein ganzer Durchlauf, der nicht mehr hineinpasste, über das Ende hinaus geplant.

Die Regel (`Runner::place_pass`):

| Fall | Was geschieht |
|---|---|
| Loop wird an der Sektion neu verankert (kommt wieder, oder `restart: true`) | der Durchlauf beginnt am Sektionsanfang |
| sonst | auf dem nächsten Rasterpunkt des Loops **in** der Sektion |
| kein Rasterpunkt in der Sektion | **nichts** wird geplant, der Loop spielt einfach weiter; Meldung zur Laufzeit; `score --check` und Editor: **Fehler** mit Zeile, wenn alle Sektionen bis dahin `autorelease: true` haben (dann steht fest, wo jede beginnt), sonst ein **Hinweis** („with Release pressed as early as possible"). Vorschlag: `restart: true`, oder die Sektion so lang, dass der nächste Durchlauf hineinfällt |
| Durchlauf passt ganz hinein | ein ganzer Durchlauf (wie bisher) |
| passt nicht ganz, Sektion mit `autorelease` | die Ebene endet mit der Sektion — kürzer als der Loop, wie ein Overdub in einer Sektion, die kürzer als der Loop ist, es immer war. Nichts davon reicht in die nächste Sektion |
| passt nicht ganz, Sektion wartet auf Release | ganzer Durchlauf; der Release wird wie immer hinter den laufenden Take geschoben |
| `stack` | gleiche Regel für den ersten Durchlauf (kein Rasterpunkt: kein Stapel). Der letzte Durchlauf eines Stapels wird wie bisher zu Ende aufgenommen (siehe `stack`) |

Die Prüfung vor dem Lauf rechnet nichts nach: `runner::dry_run` spielt die Partitur durch den
Runner ohne Engine (jede Release-Sektion so früh wie möglich gelöst) und meldet, was
`place_pass` nicht platzieren konnte. Der Golden-Wert „whole loops" in `runner/golden_tests.rs`
ändert sich dadurch bewusst (dort überdubbt `back` einen Loop, der in `pause` schwieg); alles vor
`back` ist bitgleich, geprüft mit einem eigenen Hash. Bewiesen in `runner/overdub_tests.rs` und
`score/clip_tests.rs`.

### `stack`: eine neue Ebene in jedem Durchlauf, bis die Sektion endet

```yaml
  - id: drums
    bars: 4
    autorelease: false      # stapeln bis Release
    quantize: loop
    tracks:
      Drums: stack          # Audio- und Instrument-Spuren, auch {state: stack, grid: 1/16}
```

`overdub` nimmt genau **eine** Ebene im ersten Durchlauf der Sektion auf und spielt danach nur
noch. `stack` nimmt in **jedem** Durchlauf eine eigene Ebene auf, solange die Sektion läuft:

- **Start.** Auf einer leeren Spur definiert der erste Durchlauf den Loop wie `record`: Ursprung
  = Sektionsanfang, Länge = `bars`. Auf einer Spur mit Inhalt ist schon der erste Durchlauf eine
  neue Ebene, auf dem Raster der Spur (`origin + n · loop_len`, wie `overdub`).
- **Ende.** Mit `autorelease: false` bis Release; der Wechsel greift bei `quantize: loop` am Ende
  des laufenden Durchlaufs, und auch bei `quantize: bar` wird der laufende Durchlauf zu Ende
  aufgenommen (ein Wechsel landet nie in einem laufenden Take). Mit `autorelease: true` genau
  `bars` Takte, also `bars / Looplänge` Durchläufe — auf einer leeren Spur ist das ein einziger
  Durchlauf, gleichbedeutend mit `record`; der Compiler gibt dafür einen Hinweis („stack with
  autorelease records a single pass – use record, or switch autorelease off"), keinen Fehler.
- **Leere Durchläufe** (Audio: Spitze unter −40 dBFS; MIDI: keine Note) werden verworfen — so
  stapelt sich nichts Stilles, während man nach dem letzten Fill auf den Release wartet. Der
  zuletzt laufende Durchlauf beim Sektionsende zählt normal.
- **Leerer erster Durchlauf auf leerer Spur** (der Besitzer braucht ihn, um von der Gitarre zu den
  Pads zu kommen): Die Geometrie steht trotzdem fest. Die Engine behält die leere erste Ebene als
  **stumme Platzhalter-Ebene**, solange keine andere existiert; der erste bespielte Durchlauf
  ersetzt sie und wird Ebene 1 — auf demselben Raster, taktgenau zu den anderen Spuren. Wird bis
  zum Ende gar nichts gespielt, ist die Spur danach leer, und folgende `play`-Sektionen laufen
  ohne Fehler still weiter. Die Live-Kachel zählt den Platzhalter nicht mit (Status-Feld
  `placeholder` an der Ebene).
- **Frühe Schläge (MIDI).** Ein Durchlauf, dessen einzige Noten im Vorlauf-Fenster vor der
  nächsten Eins liegen (eine Zweiunddreißigstel), gilt als leer; seine Noten gehen in den
  Durchlauf, der dort beginnt — für den waren sie gemeint. Ein Durchlauf mit sonstigem Inhalt
  behält sie an seinem Ende, wie bisher.
- **Ebenengrenze 16:** Das Stapeln endet mit der Meldung „Overdub ended: this track holds 16
  layers…", die Spur spielt weiter.

**Wie es gebaut ist.** Der Runner plant nicht einen Take pro Durchlauf, sondern schaltet den
durchgehenden Overdub der Engine (`Command::LatchOverdub`, `engine/src/engine/latch.rs` — derselbe
wie im freien Modus, seit `stack` mit Zeitstempel `at`) zum ersten Durchlauf ein und plant nur
diesen ersten Durchlauf selbst. Jeden weiteren beginnt die Engine auf genau der Sample, auf der
der vorige endete — also auf `origin + n · loop_len`, nie auf einzeln gerundeten Taktgrenzen; die
Vorhersage des Runners (unten) und das Raster der Engine können nicht auseinanderlaufen. Das Ende
kennt der Runner erst, wenn die Folgesektion geplant wird (Release, bzw. sofort bei
`autorelease`): Dann geht der Latch **mitten im letzten Durchlauf** aus — der, der auf dem letzten
Rasterpunkt vor der Grenze beginnt —, damit dieser zu Ende aufgenommen wird und keiner mehr
folgt. Ein halber Durchlauf ist weit mehr als jede Verzögerung zwischen den Threads. Folgt auf der
Spur ein Take (`record`/`overdub`), bekommt der letzte Durchlauf zusätzlich sein `StopRecord`,
damit die Engine den neuen Take dahinter einreiht. Während ein Stapel läuft, beantwortet der
Runner „läuft hier noch ein Take" aus dem Raster: Ein Release wird ans Ende des Durchlaufs
geschoben, der zum frühesten möglichen Wechselzeitpunkt läuft. „Stop all" und das Partiturende
schalten den Latch ebenfalls ab. Bewiesen in `runner/tests.rs` (drei nahtlose Ebenen mit Impuls
auf der Grenze, leerer Durchlauf, leerer erster Durchlauf, `stack`→`play`/`stack`/`overdub`,
Stop all, 80 Startpositionen wie beim Fix ce2eeb2) und `midi_tests.rs` (der Ablauf des Besitzers:
zwei Aufnahmen, dann sofort `stack` auf leerer Drum-Spur mit leerem erstem Durchlauf und frühem
Schlag; und derselbe Ablauf ganz ohne Spielen).

### Einzähler

Zwei Takte Klick vor der ersten Sektion — so wünscht es der Besitzer nach dem Spielen damit.
Ursprünglich war es ein Takt (der kürzeste Vorlauf, der noch ein *Takt* ist); in der Praxis
reichte das nicht: Im ersten Takt erkennt man das Tempo, im zweiten macht man sich bereit, und
dann trifft man den Einsatz auch bei schnellem Tempo oder ungerader Taktart. Ein halber Takt
würde die Taktart nicht etablieren.

Woher die Zahl kommt, in dieser Reihenfolge:

1. ein Wert **für diesen Lauf**: `--count-in` der Score-CLI, oder der Knopf „Count-in 2 bars"
   im Live-Modus (nur vor dem Start; ist das geöffnete Dokument das geladene, schreibt er auch
   `count_in:` der Partitur, wie die Mixer-Rückschreibung);
2. das Partiturfeld `count_in: <Takte>` auf oberster Ebene (1–8; `0` wird mit Begründung
   abgelehnt — ohne Einzähler begänne der erste Take auf der allernächsten Taktgrenze, ohne etwas
   zum Mitspielen). Das Einzähler-Feld der Arrangement-Leiste bearbeitet genau dieses Feld (ein
   Undo-Schritt wie Titel oder Tempo), „Load" übernimmt es;
3. sonst `DEFAULT_COUNT_IN_BARS` = 2 (auch neue Partituren aus der Vorlage schreiben `count_in: 2`).

**Start heißt jetzt.** Der Einzähler wartet auf keine Taktgrenze: Der Moment des Drucks plus ein
fester Vorlauf ist Schlag 1 des ersten Einzähler-Takts, und Takt 1 der ersten Sektion liegt genau
`count_in` Takte später. Der Vorlauf ist der Kopfstart, den jedes zeitgestempelte Kommando
braucht (`Scheduler::guard`, `Runner::start_lead`): vier Audio-Puffer plus 20 ms — bei 48 kHz
31 ms mit 128er-Puffern, 41 ms mit 256ern. Derselbe Vorlauf wie beim Start des freien Modus; kürzer
geht nicht sicher, denn das Kommando muss im Wartezimmer der Engine liegen, bevor das Sample
gerendert wird, sonst fehlte der erste Schlag. Er hängt nicht davon ab, wo im Takt gedrückt wurde.

Dafür setzt `Runner::start` die Eins neu: Er schickt `Command::TransportStart { at }` für genau
dieses Sample (die Engine verschiebt `Timeline::zero` dorthin — die „verschiebbare Eins" des freien
Modus) und verschiebt sein eigenes Raster ebenso; der Host übernimmt es in seinen Spiegel
(Anzeige, Mitschnitt). Die Uhr hält dabei nie an und nichts Wartendes wird verworfen, anders als
mit `TransportStop` (siehe unten). Loops, die `TransportStart` neu verankern müsste, gibt es nicht:
Eine Partitur startet nur auf leeren Spuren — nach dem Laden (das auf belegte Spuren abgelehnt
wird) oder nach „Back to start" (dessen `ClearAll` alles leert). Ein Start nach „Stop all" oder
nach dem Ende wird wie bisher abgelehnt („Use Back to start …"); die Loops eines gestoppten Laufs
bleiben bis dahin liegen, und der zweite Start nach „Back to start" ist ebenso sofort.

Der Einzähler ist damit Takt 0 … `count_in`−1 des Laufs, Sektion 0 beginnt auf Takt `count_in`,
und alles, was der Runner plant — Sektionsgrenzen, Autorelease, Release mit `quantize: bar|loop`,
die vorhergesagte Loop-Geometrie `origin + n·loop_len`, `stack`/Latch, Klick, Plugin-Position,
Mitschnitt-Anfang und Sektionsmarker —, liegt auf diesem Raster. Ein Lauf ist relativ zu seiner Eins
sample-gleich, wann immer gedrückt wurde (Test `the_count_in_starts_right_away_wherever_the_button_is_pressed`,
40 Druckzeitpunkte; im Simulator `the_first_click_follows_the_press_by_the_fixed_head_start_only`).
Die Rundung der Taktgrenzen verschiebt sich jetzt nur noch mit der Länge des Einzählers, deshalb
spielen die Regressionen von ce2eeb2 (80 Starts, auch die `stack`-Variante) jeden Einzähler von 1
bis 8.

Er läuft genau einmal, vor Sektion 0. Vor dem Start steht die Anzeige (Takt 1, Schlag 1, nichts
zählt; während des Vorlaufs ebenso); nach dem Druck zählt die Release-Fläche genau die
eingestellten Takte herunter („bar 1 in 2 bars" … „in 1 beat"), mit derselben Anzeige wie der
Einzähler des freien Modus. Start-Knopf, Leertaste und MIDI `transport.start` gehen denselben Weg
(`ScoreAction::Start`). Der freie Modus hat seine eigene Einstellung (Off/1/2 im LOOP-Feld,
Vorgabe ebenfalls 2 Takte, eine gespeicherte Einstellung bleibt).

### Nichts zu hören, solange die Partitur nicht läuft

Beim Laden startet der Host die Uhr der Engine und lässt sie laufen; ihre Phase im Leerlauf hat
keine Bedeutung, denn der Start setzt die Eins neu (siehe „Einzähler"). Zu **hören** ist von
dieser Uhr nur etwas, während die Partitur läuft. Dafür hat die
Engine ein zeitgestempeltes Klickfenster (`Command::ClickGate`):

* Der Host schließt es beim Laden (`runner::CLOSED_CLICK_GATE`) — geladen und bereit ist still,
  auch mit eingeschaltetem Klick.
* Der Runner öffnet es beim Start auf dem ersten Schlag des Einzählers (der neuen Eins), sample-genau.
* Er schließt es auf dem Sample, auf dem die Partitur endet, bei „Stop all" (nie vor dem Öffnen,
  falls Stop innerhalb des Vorlaufs gedrückt wird) und noch einmal hinter dem `ClearAll` von
  „Back to start".

Die Loops brauchen kein Fenster: Vor dem Start sind alle Spuren leer, Ende und „Stop all" stoppen
jede. Bei geschlossenem Fenster hören gehostete Plugins „steht, Position 0"; offen zählen sie auf
Takt 1 der ersten Sektion zu (im Einzähler negativ, wie beim Einzähler des freien Modus). Der
Mitschnitt ist nicht betroffen: Er beginnt ohnehin auf Takt 1 der ersten Sektion und enthält
keinen Klick. Der Weg über ein echtes `TransportStop` im Leerlauf wurde verworfen: Ein Stopp
verwirft alle wartenden Kommandos (auch das Schließen des Fensters durch „Stop all" und das
`ClearAll` von „Back to start"), behandelt laufende Takes anders als der Runner, und das Ende der Partitur ließe
sich nur einen Steuerschleifen-Takt zu spät stoppen — der Klick auf der folgenden Eins wäre zu
hören. Das Fenster bleibt deshalb auch mit der neu gesetzten Eins nötig: Es hält die laufende Uhr
im Leerlauf, nach „Stop all" und nach dem Ende stumm.

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
deshalb abgelehnt, solange etwas armiert ist, und sagt das auch.

**„Stop all" ist die Notbremse und holt alles zurück.** `Runner::stop_all` kann nicht ablehnen. Es
schickt als Erstes `Command::DropWaiting { at: soon }`: Auf der Sample, auf der der Stopp wirkt
(Kopfstart `Scheduler::soon`, 31 ms bei 128er-Puffern), verwirft die Engine alles, was vorher
geschickt wurde und noch wartet — den armierten Wechsel, Take-Enden samt ihrem `StartPlay`,
Neuverankerungen, Overdubs auf späteren Rasterpunkten. Was danach kommt, bleibt: `StopPlay` für
**jede** Spur auf `soon`, Latch aus, Mithören aus (sofort), Klickfenster zu, und für einen laufenden
Take ein `StopRecord` auf der nächsten Taktgrenze (oder auf seinem eigenen Ende, wenn das früher
liegt) — er wird sauber geschlossen, ist aber nicht zu hören. Zugesagt ist damit: ab `soon` sind
beide Busse digital still (bis auf die Ausklänge der Effektketten — Hall, Delay, die 144 Samples
Latenz des Octavers —, die nur abklingen), keine Loop-Note eines MIDI-Loops kommt mehr, keine hängt,
und Anzeige und Engine-Status sagen „gestoppt". Die Zahl in „Stopped all (N tracks)" ist die der
Spuren, die beim Druck klangen oder aufnahmen.

Früher beantwortete `stop_all` jedes wartende Kommando mit einem Gegenkommando, wo es landet. Das
übersah Fälle, und einer davon war der Fehler des Besitzers („manchmal läuft nach Stop all alles
weiter"): Mit `autorelease` ist die Folgesektion ab dem ersten Sample einer Sektion geplant, und
das Bild des Runners beschreibt dann schon sie — jede Spur, die *dort* schweigt, galt als „klingt
nicht" und spielte bis zur Grenze weiter (in „The Hardest Part" einen Takt nach Beginn von
`chorus_2`: „Stopped all (1 track)", fünf Loops liefen neun Takte weiter). „Manchmal", weil es nur
die Spuren traf, die in der Folgesektion verstummen. Ebenso übersehen: das `StartPlay` am Ende eines
`{state: record, bars: N}`, wenn dieselbe Spur in der Folgesektion wieder aufnimmt, und ein Overdub,
der erst auf einem späteren Rasterpunkt beginnt; und nach einem Stopp im Einzähler schnitt das noch
wartende `StopRecord` des alten Laufs den ersten Take einer neu geladenen Partitur ab. Bewiesen in
`runner/stop_tests.rs` mit der Partitur des Besitzers (ohne `sheet:`, `engine/tests/data/`):
96 Druckzeitpunkte über alle Sektionen und den Einzähler (früh, ein Takt, Mitte, letzter Takt, kurz
vor der Grenze, im Vorlesefenster des Octavers, auf der Grenze), mit und ohne Mixer, mit einem
MIDI-Loop auf dem Instrument; dazu „Back to start" danach und das Laden nach einem Stopp im
Einzähler. Vor der Korrektur klang es nach 37 der 96 Drücke weiter.

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
transport.toggle                         (Uhr des freien Loopens; mit Partitur abgewiesen)
global.click | global.clear_all | global.tempo | global.quantize.bar|loop
global.bus.<main|monitor>.gain
track.<ref>.record | overdub | play | stop | stop_now | clear | monitor | pan | latency_trim
track.<ref>.bus.<main|monitor>          · monitor_bus.<main|monitor>
track.<ref>.layer.<n>.mute | remove | gain
track.<ref>.fx.bypass
track.<ref>.fx.<octaver|high_pass|eq|comp|delay|reverb>.on
track.<ref>.fx.preset.<trocken|stimme|gitarre|bass>
track.<ref>.fx.octaver.<dry|sub|tone|drive>
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

## 12. Getrennte Ausgangsbusse (Phase 5, „Klick nur auf den Monitorweg")

Der Musiker steht auf der Bühne und braucht den Klick auf dem Kopfhörer. Im Saal darf nur ankommen,
was er gemacht hat. Mit einem einzigen Ausgang ist das keine Einstellung, sondern ein Widerspruch:
alles, was die Engine erzeugt, geht durch dieselbe Buchse. **Die Summierung auf zwei Busse ist,
was den Satz aus Abschnitt 0 überhaupt aussprechbar macht.**

```text
                       ┌──────────────────────► Main    ─► Ausgangspaar A  (Saal)
 Loop je Track      ───┤
 Mithören je Track  ───┤
 Klick ────────────────┴─(nur hierhin)────────► Monitor ─► Ausgangspaar B  (Kopfhörer)
```

| Quelle | Main (Saal) | Monitor (Kopfhörer) |
|---|---|---|
| Loop-Wiedergabe je Track | ja, schaltbar (Vorgabe: an) | ja, schaltbar (Vorgabe: an) |
| Mithörsignal je Track | ja, schaltbar (Vorgabe: **aus**) | ja, schaltbar (Vorgabe: an) |
| **Klick** | **nie, auf keinem Weg** | ja, mit dem gewohnten Klick-Schalter |

Jeder Bus ist stereo und hat eine eigene Lautstärke. Das ist der halbe Sinn der Sache: den
Kopfhörer leiser drehen, ohne den Saal zu verändern.

### Die eine Zuordnung, die keine Einstellung ist

Für den Klick gibt es **keinen** Schalter auf Main. Er ist der Grund, warum es zwei Busse gibt, und
ein Schalter dafür wäre ein Schalter, der an genau dem einen Abend versehentlich gedrückt wird, an
dem er zählt. Was die beiden Busse *physisch* bekommen, ist eine andere Frage — siehe unten.

**Die Vorgabe des Mithörsignals ist „nur Kopfhörer", und das ist kein Versehen.** Der Saal hat den
Sänger in aller Regel über den eigenen Kanal der PA; ihn zusätzlich durch den Looper zu schicken
heißt, ihn doppelt zu hören. Wer den Looper als ganze Kette benutzt, legt das Mithörsignal mit einem
Klick auch auf Main.

### Zuordnung zu physischen Ausgängen

Jeder Bus liegt auf einem **Kanalpaar** oder auf einem **einzelnen Kanal**, frei wählbar im Rahmen
dessen, was das Gerät hat. Bei einem einzelnen Kanal wird der Bus mit halbem Pegel auf mono gefaltet
— einen Kanal wegzulassen würde alles verstummen lassen, was hart dorthin gepannt ist.

| Ausgangskanäle | Vorgabe |
|---|---|
| 4 und mehr | Main auf 1-2, Monitor auf 3-4 — getrennt, wofür das Ganze gebaut ist |
| 2 oder 3 | beide auf 1-2 — es gibt nur den einen Weg |
| 1 | beide auf Kanal 1, mono gefaltet |

`engine/src/engine/bus.rs::place_buses` ist die **einzige** Stelle, an der aus einem Bus eine Buchse
wird; sie hat `process::spread_frame` abgelöst, das L/R auf jedes Kanalpaar des Geräts legte. Jeder
Ausgabe-Callback (App, CLI, `calibrate`, Simulation) ruft sie über `EngineCore::place_output` auf, die
Ausgangsstufe: platzieren, je Gerätekanal messen, Sicherheits-Limiter (siehe unten).

### Was bei zwei Ausgängen passiert, und wie es erklärt wird

Henrys Gerät ist ein Focusrite Scarlett 2i2, dessen Kopfhörerausgang eine Kopie von Ausgang 1/2 ist:
**zwei Ausgangskanäle, getrennte Busse brauchen vier.** Das ist kein Fehler und wird nicht heimlich
repariert.

**Teilen sich die Busse auch nur einen Gerätekanal, mischt die Engine sie zu einem**
(`BusRouting::collapsed`). Jede Quelle klingt dann genau **einmal**, gleichgültig auf wie viele
Busse sie gelegt ist. Das ist keine Kosmetik: Zwei Busse übereinanderzulegen würde einen Track, der
auf beide geht, 6 dB über einen legen, der auf einen geht — und das Einstecken eines zweiten
Ausgangspaares würde die Balance des ersten verändern. Die Regel:

* Ein Weg hinaus ist **eine Mischung**: alle Tracks, das Mithören und der Klick, jede Quelle einmal,
  auf dem Main-Bus gerechnet. Der Monitor-Bus bleibt still.
* **Die Main-Lautstärke regelt diese eine Mischung**, den Klick eingeschlossen. Die
  Monitor-Lautstärke hat nichts Eigenes zu regeln und ist in der App ausgegraut. Für den Klick und
  das Mithören gibt es ohnehin die eigenen Pegel (Klick, Monitoring). Gegenüber einem Gerät mit vier
  Ausgängen hört der Kopfhörer also, was dort der Saal hört, plus Klick — nicht 6 dB mehr.
* Die Regel gilt **kanalweise**: `place_buses` legt die eine Mischung auf jeden Kanal, den einer der
  Busse belegt, und auf keinen zweimal — mit Mains Belegung, wo Main liegt, mit der des Monitors, wo
  nur er liegt. Main auf 1-2 und Monitor auf 3-4 überschneiden sich nicht und bleiben getrennt; beide
  auf 1-2 sind eine Mischung; Main auf 1-2 und Monitor auf 2-3 sind es auch, und Kanal 3 bekommt die
  rechte Seite derselben Mischung.
* Plugin-Inserts, Mitschnitt und Latenzmessung bleiben, wie sie sind: der Track-Insert verarbeitet
  das Main-Signal, `Mix.wav` ist der Main-Bus nach den Inserts, vor der Main-Lautstärke, ohne Klick
  (Stems addiert = Mix), und `calibrate` legt beide Busse absichtlich auf ein Paar.

Der Preis steht in einem Satz, einmal beim Start, einmal im Einrichtungsfeld, einmal im
Engine-Panel und einmal auf der Bus-Leiste (`bus::routing_note`) — nicht bei jeder
Statusaktualisierung, denn eine Meldung, die nervt, erzieht dazu, Meldungen nicht mehr zu lesen:

> The device has 2 output channels. Separate buses need four. Main and Monitor are therefore both
> on output 1-2: there is one way out and therefore one mix. Every source is in it exactly once, so
> levels stay as they would be - but the click goes out to the room too, the Main level sets the
> whole output and the Monitor level has nothing of its own to control.

**Der Notbehelf, den erfahrene Musiker mit zwei Ausgängen benutzen**, braucht dafür keinen eigenen
Modus: weil ein Bus auch auf einem einzelnen Kanal liegen darf, ist „Mix links, Klick rechts" schon
möglich — `--bus-out main:1 --bus-out monitor:2`, mit einem Y-Kabel aufzutrennen. Ein eigener
Schalter dafür wäre eine dritte Sache zu erklären für etwas, das der allgemeine Mechanismus ohnehin
kann.

### Warum der Kopfhörer übersteuern konnte, obwohl der Mitschnitt sauber war

Beobachtet am 2i2: Kratzen im Kopfhörer, `Mix.wav` mit Spitze −1,4 dBFS, nichts geclippt. Eine
Doppelung war es nicht (siehe oben, durch Simulation belegt: ein Loop auf beiden Bussen mit −3 dBFS
kommt mit −3,00 dBFS heraus). Es war, was der Mitschnitt nicht enthält: **der Klick** (Spitze bis
0,5 × Klick-Pegel, also −6 dBFS bei Pegel 1) liegt auf dem Mix, und ein Schlag auf der Zählzeit trifft
ihn genau dort; dazu die **Main-Lautstärke**, die nach dem Abgriff des Mitschnitts kommt. Ein
−1,4-dBFS-Mix plus Klick erreicht in der Simulation +1,8 dBFS. Bis dahin klemmte die Engine jeden
Bus hart auf ±1 — hartes digitales Clipping auf jedem Schlag — und das Bus-Meter maß **nach** dieser
Klemme, konnte also höchstens 0 dBFS zeigen.

Seitdem gilt:

* **Kein Clamp mehr je Bus.** Die Bus-Meter zeigen den wahren Pegel, auch über 0 dBFS.
* **Ausgangsmeter je Gerätekanal** (`Status::out_peak`, die ersten acht Kanäle): die Summe auf der
  Buchse, **vor** dem Limiter — das einzige Meter, das eine Übersteuerung der Summe zeigen kann.
* **Sicherheits-Limiter je Gerätekanal** (`bus::OutputLimiter`): Schwelle −0,3 dBFS, **ohne
  Vorausschau und damit ohne Latenz** (die ganze Zeitrechnung des Loopers ruht auf einer einmal
  gemessenen Roundtrip-Latenz). Ein Sample über der Schwelle verlangt die Verstärkung, die es auf die
  Schwelle bringt; die niedrigste wird 10 ms gehalten und mit 80 ms Zeitkonstante freigegeben, die
  angewandte Verstärkung folgt ihr in etwa 0,5 ms nach unten. Was in dieser halben Millisekunde noch
  darüber liegt, rundet ein Soft-Clipper (`safety_limit`, tanh-Knie, nie über 1,0). Solange nichts
  darüber ist und die Verstärkung wieder bei 1 steht, geht jedes Sample **bitgleich** durch. Die
  Reduktion steht als `out_reduction_db` im Status. Im Callback wird nichts alloziert; der Zustand
  wird beim Bau der Engine angelegt.
* **Der Mitschnitt bleibt unbegrenzt** — er wird lange vor der Ausgangsstufe abgegriffen.
* **Anzeige**: im Engine-Panel je Ausgang ein Meter und „Limiter −x dB", solange er arbeitet; in der
  Kopfleiste eine rote, einrastende Clip-Lampe („Clip 1/2"), sobald ein Ausgang 0 dBFS erreicht hat,
  mit Tooltip, welcher Ausgang wie weit darüber war und was leiser zu stellen ist. Ein Klick löscht sie
  und öffnet das Engine-Panel.

### Warum es eine Effektkette je Bus gibt

Siehe Abschnitt 9. Kurz: Wenn Loop und Mithörsignal auf verschiedenen Bussen liegen, führen die
Busse verschiedene Signale, und eine nichtlineare Kette lässt sich nachträglich nicht auftrennen.
Solange beide Busse dieselben Quellen sehen — der Normalfall —, rechnet nur eine Kette.

### Busse sind Ausgang, und das ist eine harte Zusage

Kein Schalter, kein Regler und keine Zuordnung dieses Abschnitts kann einen Take verändern.
`record_input` sieht weder einen Bus noch eine Bus-Lautstärke, genauso wenig wie es eine Effektkette
sieht (Abschnitt 9). Genau deshalb darf der Musiker diese Schalter *während* einer Aufnahme anfassen
und deshalb bleiben sie auch erreichbar, während eine Partitur läuft (Abschnitt 10: der Transport
gehört dem Runner, der Mix dem Menschen). Ein Test hält es fest.

### Echtzeit

Die Busse sind vorab alloziert: der Ausgabepuffer der Engine ist ein Block mit vier Samples je Frame
(`[main_l, main_r, mon_l, mon_r]`), im Steuer-Thread angelegt wie bisher. Im Callback wird nichts
alloziert, gesperrt oder geloggt. Die drei neuen Kommandos (`SetTrackSend`, `SetBusGain`,
`SetBusOutput`) sind `Copy` und ändern je ein paar Bytes. Die Bus-Lautstärke wird **nicht**
geglättet — wie das Panorama und die Ebenenlautstärke auch nicht: alle drei kommen als Strom kleiner
Schritte von einem Regler, und ein Glätter obendrauf wären drei Glätter, die ehrlich gehalten werden
müssten.

### Wo es konfiguriert wird

| Ort | Bus auf Ausgang | Bus-Lautstärke | je Track |
|---|---|---|---|
| CLI (`live`, `score`) | `--bus-out main:1-2`, `--bus-out monitor:3-4`, `--bus-out monitor:2` | `--bus-gain monitor:0.8` | `--track-bus stimme:monitor`, `--track-monitor-bus stimme:main+monitor` |
| CLI zur Laufzeit | `u <main\|monitor> <kanal[-kanal]>` | `g <main\|monitor> <wert>` | `b <loop\|mon> <busse>` |
| Partitur | — | — | `stimme: {input: 1, bus: monitor, monitor_bus: main+monitor}` |
| App | `StartConfig.bus_out_main` / `bus_out_monitor`, Kommando `bus_output` | `bus_gain_main` / `bus_gain_monitor`, Kommando `bus_gain` | `TrackConfig.bus` / `monitor_bus`, Kommando `track_bus` |

Die Wörter sind überall dieselben: `main`, `monitor`, `main+monitor`, `none`.

### Was im Status mitfährt

Global: `buses` (je Bus Kennung, Beschriftung, Ausgangskanal, Breite, Lautstärke, Pegel je Seite,
und ob der Klick darauf liegt), `output_channels`, `buses_collapsed` und `bus_note` — der deutsche
Satz von oben, oder `null`, wenn die Busse sauber getrennt sind — und `outputs`: je
Gerätekanal (höchstens acht) Kanalnummer, Spitze vor dem Limiter, Limiter-Reduktion in dB und welche
Busse dort hinausgehen. Je Track vier Schalter:
`bus_main`, `bus_monitor`, `monitor_bus_main`, `monitor_bus_monitor`. `output_peak` meint jetzt
ausdrücklich den **Main-Bus**; eine Zahl, die den Klick enthielte, wäre zum Pegeleinstellen wertlos.

### Was das UI zeigt

* **Bus-Leiste** unter dem Kopf, im Betrieb: je Bus das Ausgangspaar (umschaltbar), die Lautstärke
  als Regler, zwei Pegelbalken, und ein Abzeichen „Klick" auf dem Bus, der ihn führt. Darunter,
  falls nötig, der eine Satz.
* **Track-Karte**: eine Reihe mit vier kleinen Schaltern — `Loop: Main | Monitor` und
  `Mithören: Main | Monitor`. Zwei beschriftete Paare statt eines Auswahlfelds mit vier Einträgen:
  „geht dieser Track in den Saal" ist eine Frage mit Ja und Nein, und ein Menü mitten im Stück ist
  eine Leseaufgabe.
* **Einrichtung**: ein Feld „Ausgangsbusse" mit der Zuordnung beider Busse und dem Hinweis, was bei
  zwei Ausgängen passiert; je Track zwei Auswahlfelder für die Voreinstellung.

### MIDI

`global.bus.<main|monitor>.gain` ist ein Regler über 0 bis 4 — dieselbe Spanne, die die Engine selbst
klemmt. `track.<ref>.bus.<main|monitor>` und `track.<ref>.monitor_bus.<main|monitor>` sind Schalter.
Ein Schalter ändert **ein Bit einer Menge**: der Router liest die ganze Zuordnung aus dem Status
zurück, bevor er das Bit schreibt, sonst nähme „Main aus" den Track auch vom Kopfhörer. Keine der
neuen Adressen verschiebt den Transport, alle bleiben also bedienbar, während eine Partitur läuft.

### Wo der Code liegt

| Datei | Inhalt |
|---|---|
| `engine/src/engine/bus.rs` | `Bus`, `BusSend`, `BusOutput`, `BusRouting`, `place_buses`, `routing_note`, `OutputLimiter`, `safety_limit` |
| `engine/src/engine/track.rs` | eine Kette je Bus, die zwei Zuordnungen je Track, `render` |
| `engine/src/engine/process.rs` | Summierung auf zwei Busse, Bus-Lautstärken, Klick auf Monitor, Ausgangsstufe `place_output` |
| `engine/src/engine/output_tests.rs` | geteilte Busse, Limiter und Ausgangsmeter in der Simulation |
| `ui/src/shell/OutputClip.tsx`, `outputLatch.ts` | die Clip-Lampe der Kopfleiste |
| `engine/src/engine/live.rs` | `--bus-out`, `--bus-gain`, `--track-bus`, `--track-monitor-bus`, Tasten `b g u` |
| `engine/src/score/read.rs` | `bus:` und `monitor_bus:` in der Partitur |
| `engine/src/midi/target.rs` | die neuen Adressen |
| `app/src-tauri/src/proto.rs` | `BusOutConfig`, `BusStatusEvent`, die vier Track-Schalter |
| `ui/src/live/BusStrip.tsx` | die Bus-Leiste |
| `ui/src/live/TrackCard.tsx` | `BusRow`, die vier Schalter je Track |

## 14. Plugin-Hosting (VST3, Audio Units)

**Gebaut, macOS.** Code in `engine/src/engine/insert.rs` (Slots im Audio-Thread),
`engine/src/plugin/` (VST3, AU, Scan, Editor-Fenster, CLI `looper-engine plugins scan|probe|edit`),
`app/src-tauri/src/plugins.rs` (Manager, Tauri-Kommandos), `ui/src/plugins/`.

**Formate.** VST3 über die Crate `vst3` 0.3 (aus den Steinberg-Headern generiert, MIT/Apache; das
SDK selbst ist seit 3.8 MIT). Audio Units über die AudioToolbox-C-API — fast geschenkt neben VST3,
und Apples mitgelieferte Effekte sind auf jedem Mac echte Testobjekte. CLAP (`clack-host` 0.2) ist
möglich, aber nicht gebaut: hier ist kein CLAP-Plugin installiert, und die großen Bibliotheken
liefern es nicht (Abschnitt 8).

**Lage im Signalweg.** Acht Insert-Slots je Track **an beliebiger Stelle der eingebauten Kette,
vor Fader und Panorama** (seit 2026-11, siehe „Plugins in der Kette“ unten; ohne Angabe nach der
eingebauten Kette, wie zuvor), acht je Bus über der Summe der Tracks, **vor** dem Klick (der bleibt
außerhalb jedes Plugins). Aufgenommen wird weiter trocken. Eine Instanz je Slot; hören Main und Monitor
verschiedene Quellen (4+ Ausgänge, Loop und Mithören getrennt), rechnen die Inserts auf Main —
Monitor bekommt dann die eingebaute Kette ohne Plugins. Bekannte Grenze.

**Echtzeit.** Der Audio-Thread lädt, erzeugt und verwirft nie ein Plugin. Ein `Insert` (Prozessor +
geteilte Atomics) wandert über eine eigene rtrb-Queue hinein und über eine Retire-Queue zurück;
Bypass ist ein Atomic. Solange kein Slot belegt ist, rendert die Engine bitgleich wie vorher
(Rahmenpfad); sonst blockweise, höchstens 1024 Frames je Plugin-Aufruf. NaN/Inf aus einem Plugin
wird an der Eintrittsstelle zu Stille. Parameter: VST3 über eine lock-freie Queue als
`IParameterChanges` im nächsten Block, AU über `AudioUnitSetParameter` (threadsicher). **Was ein
fremdes Plugin in seinem `process` tut, kann die Engine nicht garantieren.**

**Latenz.** Die gemeldete Latenz der aktiven Inserts eines Tracks wird auf der **Loop-Wiedergabe**
ausgeglichen (der Loop wird um genau so viele Frames vorgelesen). Das Mithörsignal ist um sie
später, Bus-Inserts verschieben den ganzen Bus gegen den Klick — beides wird nur ausgewiesen.

**Threads.** Jedes geladene Plugin lebt auf dem Hauptthread (VST3-Editor, AppKit, JUCE). Jeder
Engine-Start veröffentlicht seine Insert-Queue mit neuer Generation; der Manager deaktiviert,
aktiviert mit der neuen Samplerate und setzt neu ein.

**Abstürze.** Der **Scan** lädt jedes Plugin zuerst in einem Kindprozess (`--plugin-probe`):
Absturz oder Hänger (45 s) werden vermerkt, das Plugin wird nie geladen. Cache `plugins.json`
neben den MIDI-Profilen. Im laufenden Betrieb gibt es keine Prozesstrennung: ein Plugin, das in
`process` abstürzt, reißt die App mit. Die Belegung samt Plugin-Zustand liegt in
`plugin-rack.json` und kommt beim Start wieder; stürzt das Wiederherstellen ab, wird es beim
nächsten Start übersprungen.

**Slots gehören dem Spurnamen, nicht der Position** (seit 2026-11). Partitur und Setup ordnen
Spuren verschieden (Setup `stimme, gitarre`, Partitur `gitarre, voice`); nach Position landete
der De-Esser der Stimme dann auf der Gitarre. `plugin-rack.json` (Version 2) ist nach Spurname
(`tracks`) bzw. `main`/`monitor` (`buses`) geschlüsselt. Jeder Engine-Start übergibt dem Manager
die Spurliste: Slots einer vorhandenen Spur werden an deren Position eingesetzt, Slots einer
fehlenden Spur bleiben **geparkt** (geladen, in der Datei, in Einrichtung > Plugins aufgeführt)
und kommen zurück, sobald es die Spur wieder gibt. Die Kommandos und das UI sagen weiter
`track.<n>` — gemeint ist die Position in der laufenden Engine, also der Kanalzug.

*Umbenennen.* `app/src-tauri/src/layout.rs` merkt sich die zuletzt gelaufene Spurliste in
`layout.json` und vergleicht bei jedem Start: verschwundener und neuer Name auf **denselben
Eingängen** (eindeutig) oder — sonst — genau ein verschwundener und ein neuer Name an
**derselben Position** gelten als Umbenennung; Plugins und Mix (`mixer.json`) ziehen mit. Hat der
neue Name schon eigene Slots, wird nichts überschrieben. Alles Unsichere ist keine Umbenennung.

*Migration.* Eine Datei im alten Format (`slots: [{target: "track.2", …}]`) wird beim Lesen
übernommen: `track.<n>` wird zum Namen, den die `n`-te Spur der letzten bekannten Spurliste hatte;
ist die nicht bekannt (erster Start nach dem Update), bleibt der Slot eine Position und wird beim
ersten Engine-Start an die Spur gebunden, die dort steht. Nichts geht verloren; die alte Datei
bleibt als `plugin-rack.json.v1` liegen. Plugins, die beim Wiederherstellen nicht laden
(deinstalliert, nicht gescannt), bleiben ebenfalls in der Datei.

**MIDI.** `track.<name>.insert.<slot>.bypass` (Schalter), `track.<name>.insert.<slot>.param.<id>`
(0..1, Pickup), dasselbe unter `global.bus.<main|monitor>.insert.…`. Die Id ist die des Plugins.
Die App vergibt die Adressen mit dem **Spurnamen**, damit ein gelernter Regler dem Plugin folgt,
wenn eine Partitur die Spuren anders ordnet; ein Name, der in keiner Adresse stehen kann (nur
Ziffern, Punkt, Leerzeichen), wird als Position geschrieben. Schon gelernte Belegungen der Form
`track.<n>.insert…` bleiben positionsbezogen wie alle `track.<n>.*`-Adressen: nach einem
Umordnen greifen sie auf den Slot der Spur, die jetzt an Position `n` steht (oder ins Leere).
Wer das nicht will, lernt sie einmal neu.

**Plugins in der Kette** (gebaut 2026-11). Ein Impulsantwort-Lader (Gitarrenbox) gehört vor EQ,
Kompressor und Hall, auf das trockene Signal — bis dahin konnte ein Plugin nur hinter dem Hall
sitzen. Jetzt ist die Spalte eines Tracks **eine geordnete Liste**:

```
[IR-Lader] ─► Oktaver ─► Hochpass ─► [De-Esser] ─► EQ ─► Kompressor ─► Delay ─► Hall ─► [Delay-Plugin] ─► Fader/Panorama
```

* *Entscheidung: die eingebauten Geräte behalten ihre feste Reihenfolge*, jedes Plugin bekommt
  einen Platz **vor** einem von ihnen oder **hinter** allen (`InsertPosition`, 0 = vor dem Oktaver
  … 6 = Ende). Frei umsortierbare eingebaute Geräte fielen nicht von selbst mit ab: die Presets
  und die Begründungen der Reihenfolge (Oktaver vorn, Delay vor dem Hall, Abschnitt 9) würden
  bedeutungslos, die Kette hat *einen* Bypass mit *einer* Überblendung und einen Leerlauf-Detektor
  am Eingang, und die Parameter-Kommandos adressieren die Geräte über ihren festen Index. Für den
  Wunsch („IR vor alles“) reichen Plätze zwischen den Geräten vollständig.
* *Slot-Reihenfolge ist Signal-Reihenfolge.* Der Host hält die Slots eines Tracks nach Platz
  sortiert (`in_signal_order`, eine Verschiebung, die das bräche, wird abgelehnt). Der Audio-Thread
  rechnet nach (Platz, Slot), braucht die Sortierung also nicht.
* *Echtzeit.* Liegt ein Plugin innerhalb der Kette, rechnet der Blockpfad die eingebaute Kette in
  **Segmenten** (`Chain::process_segment`: die aufeinanderfolgenden Geräte zwischen zwei
  Plugin-Plätzen, je über den ganzen Block, dazwischen die Plugins dieses Platzes) — nichts wird
  alloziert, die Blöcke gibt es schon. Ohne ein solches Plugin bleibt der Pfad exakt der alte
  (Test `an_identity_plugin_anywhere_changes_nothing`: ein Identitäts-Plugin an jedem der sieben
  Plätze ergibt bitgleich dasselbe wie am Ende, mit Voice-Preset, Mithören und getrennten Bussen).
  Der Ketten-Bypass blendet je Segment über, mit demselben Faktor je Frame — eingeschwungen ist
  das exakt die ganze Kette, während der 25-ms-Überblendung eine andere, ebenso klickfreie
  Überblendung. Der Leerlauf-Detektor schaut auf den Eingang jedes Segments (ein Hall hinter einem
  ausklingenden IR-Plugin darf nicht abgeschnitten werden); die Zähler beginnen neu, sobald sich
  die Aufteilung ändert. Die Ein/Aus-Überblendungen der eingebauten Geräte sind unverändert, ein
  Plugin-Bypass schaltet wie bisher hart.
* *Latenz* ist die Summe von allem, was gehört wird, egal wo es sitzt (Oktaver + Plugins; Test
  `latencies_add_up_wherever_the_plugins_sit`); der Loop wird um genau so viel vorgelesen.
* *Umordnen während der Musik.* Ein Platzwechsel ändert das Signal hinter dem Einfügepunkt von
  einem Sample aufs nächste — ein Knack. Darum geht jedes `Move`/`Place` eines hörbaren Track-Racks
  durch einen **Dip**: der Track blendet in 256 Frames (5 ms) aus, die Slots wechseln in der Stille,
  er blendet wieder ein (`REORDER_FADE`; Test `moving_a_plugin_while_it_plays_dips_instead_of_clicking`:
  kein Sprung größer als eine Rampenstufe). Ein zweites Umordnen wartet in der Queue, bis das erste
  durch ist. Laden, Entfernen und Bus-Umordnen bleiben sofort, wie bisher.
* *Nur ein Signal.* Bei getrennten Bussen (Abschnitt 12) bekommt Monitor weiter die eingebaute
  Kette ohne Plugins, jetzt in ganzer Länge.
* *MIDI.* Eingebaute Geräte heißen in Adressen nach Namen (`fx.reverb.mix`), Plugins nach Slot
  (`insert.<n>`). Ein Plugin nur über eingebaute Geräte hinweg zu ziehen ändert seinen Slot nicht
  (`placeDrop` im UI behält den Slot, wann immer das reicht) — gelernte Belegungen bleiben gültig,
  eine Belegung auf den Hall erst recht. Wer die Plugins **untereinander** umordnet, verschiebt ihre
  Slots, wie schon vorher.
* *Datei.* `plugin-rack.json` bekommt je Slot `before` (fehlt = Ende); eine alte Datei liest sich
  mit allen Plugins am Ende — genau wie vorher —, Version bleibt 2 (ein älteres Programm ignoriert
  das Feld). In der Partitur: `inserts: [{plugin: …, before: eq}]`, Abschnitt 15.
* *UI.* Eine Liste ohne „Built-in“/„Plug-ins“-Trenner; eine Plugin-Zeile lässt sich über und unter
  jede eingebaute Zeile ziehen (über die Kopfzeile oder den Griff; maßgeblich ist die Mitte der
  Kopfzeilen, die Regler einer aufgeklappten Zeile zählen also als „dahinter“; „+ Add plugin“ =
  ans Ende; eine Linie zeigt die Einfügestelle, Esc bricht ab). Ein neues Plugin kommt ans Ende.
  **Zeiger-Ereignisse, kein HTML5-Drag-and-Drop** (Fehler vom 2026-11-21): im App-Fenster
  (WKWebView) übernimmt Tauris eigene Datei-Ablage (`dragDropEnabled`, standardmäßig an) die native
  Drag-Sitzung, die Seite bekommt kein `dragover`/`drop` — das Plugin ließ sich anfassen, aber
  nirgends ablegen (in Chrome ging es, darum fiel es in keinem Test auf). Jedes andere Ziehen der
  App (Raster, Mixer-Kante, Blatt) benutzt ohnehin Zeiger-Ereignisse. Rückfallweg, der nicht
  scheitern kann: „↑ Up“/„↓ Down“ in der aufgeklappten Plugin-Zeile und Alt+↑/↓ auf der
  fokussierten Zeile, Schritt für Schritt durch die sieben Plätze und an anderen Plugins am selben
  Platz vorbei (`stepPlace`). Prüfen im Browser mit echten Eingabe-Ereignissen:
  `node ui/scripts/drag-check.mjs` (eigener Vite-Server auf 5199, Chrome headless mit Wegwerf-Profil
  über das DevTools-Protokoll, Vorschau `ui/dev/devchain.html` mit Stellvertreter-Host).

**Kopieren.** „Copy to…“ in einer aufgeklappten Plugin-Zeile legt eine Kopie — dasselbe Plugin,
sein vollständiger Zustand (derselbe Chunk wie im Rack), Bypass und Platz — auf eine andere Spur
oder einen Bus (dort am Ende); eine zweite, unabhängige Instanz, geladen auf dem Hauptthread wie
jedes Laden, mit dem Zustand **vor** dem Start, hinter den Plugins, die schon an diesem Platz sitzen
(`plugin_copy`). Dasselbe für Instrumente (`instrument_copy`, nur auf Instrument-Spuren, ersetzt
das dortige). „Copy chain to…“ im Kopf einer Spur kopiert die **ganze Kette**: die eingebauten
Geräte mit allen Werten über dieselben Kommandos wie ein `mixer:`-Eintrag (Fader, Mute und
Instrument des Ziels bleiben), dann ersetzt es die Plugins des Ziels durch Kopien (`chain_copy`).
Alles zählt als menschliche Änderung: die offene Partitur schreibt den Eintrag des Ziels zurück.
Eine Tastenkonvention für Kopieren/Einfügen hat die App nicht; es gibt nur die Knöpfe.

**Nicht gebaut.** Instrumente (werden gelistet, nicht geladen), Sidechain, MIDI an Plugins,
Latenzausgleich über Busse hinweg, Windows-Host (kompiliert, sagt „nur macOS“).

## 15. Der Mix bleibt beim Stück

Fader, Mute, die eingebaute Kette je Spur und die Pegel der Busse, des Klicks und des Mithörens
überleben, was sie früher zurückgesetzt hat: Neustart der App, Neustart der Engine, und den
Neuaufbau beim Laden einer Partitur mit anderen Spuren.

**Mit Partitur steht der Mix in der Partitur**, im optionalen Abschnitt `mixer:`
(`engine/src/score/mixer.rs`; Parser `score/read/mixer.rs`, Dokumentmodell in `score/doc.rs`):

```yaml
mixer:
  buses: {main: 0, monitor: -6, click: -12, inputs: 0}   # dB; inputs = Mithören
  tracks:
    voice: {gain: -3.5, preset: voice}
    guitar:
      gain: -1
      mute: true
      preset: piezo_guitar
      comp: {threshold: -12}   # nur was vom Preset abweicht
      reverb: {on: false}
    bass:
      preset: bass_from_guitar
      octaver: {tone: 1800, drive: 0.5}   # on, dry, sub (0..1), tone (Hz), drive (0..1)
```

* Pegel in dB, auf 0,1 dB gerundet; `-100` und weniger ist Stille, Obergrenze +12 dB.
* Eine Kette ist ein Preset (`dry`, `voice`, `piezo_guitar`, `bass_from_guitar`; fehlt = `dry`)
  plus Effektzeilen (`octaver`, `high_pass`, `eq`, `band1`…`band3`, `comp`, `delay`, `reverb`), die
  nur die genannten Felder überschreiben. `octaver: {on: true, dry: 0, sub: 1}` schaltet den
  Oktaver auf jedem Preset dazu. `bypass` fehlt = Kette aus genau dann, wenn kein Effekt an ist — wie die Presets.
* In einem Eintrag heißt ein fehlendes Feld Standardwert; eine Spur ohne Eintrag behält, was sie
  hat. Fehlt der Abschnitt ganz, ändert sich nichts — alte Partituren lesen und rendern sich
  unverändert.
* Kommentare hängen wie überall am Element (Eintrag, Feldzeile, `buses`/`tracks`).
* **Solo wird nicht gespeichert**: ein vergessenes Solo in einer gespeicherten Partitur macht
  beim nächsten Laden alle anderen Spuren stumm — auf der Bühne. **Mute schon**: eine
  stummgeschaltete Spur ist eine Entscheidung über das Arrangement und am Kanalzug sichtbar.
* Pan, Busrouting und Mithören sind keine Mixer-Felder, sondern stehen schon unter `tracks:`.
  **Pan läuft trotzdem denselben Rückweg** (seit 2026-11): eine Pan-Änderung am Kanalzug zählt
  `mixer_rev` hoch, `mixer_take` liefert sie unter `pan` (Spurname → Wert) mit, und das
  Arrangement schreibt sie unter `tracks.<name>.pan` (Mitte = Feld weg); Undo/Tippen schickt die
  Pans mit `mixer_apply(mixer, pan)` zurück. Der `mixer:`-Eintrag der Spur wird dabei nicht
  angefasst, eine Partitur ohne `mixer:` bekommt durch eine Pan-Bewegung keinen.
* **Plugins: nur Ids** (seit 2026-11, siehe unten): `instrument` und `inserts` je Spur.

*Laden.* Der Host wendet den Abschnitt nach Namen an (`load_score`, auch nach dem Neuaufbau),
über gewöhnliche Kommandos, höchstens 48 je Runde (`MIX_BATCH`), damit die Kommando-Queue nie
überläuft. Eine Kette, die schon stimmt, wird nicht neu geladen (das setzte ihre Filter zurück).

*Rückweg.* Jede menschliche Mix-Änderung (Maus, Taste, MIDI — alles läuft durch `Session::apply`)
zählt `mixer_rev` im Status hoch; das Arrangement (`ui/src/mixer/scoreMix.ts`) fragt nach einer
Ruhepause von 350 ms `mixer_take` und schreibt **nur die geänderten Einträge** per `docStore.edit`
in die offene Partitur — die Datei ist dann geändert, ⌘S speichert den Mix mit. Hat die Partitur
noch keinen Abschnitt, wird beim ersten Mal der ganze Mix geschrieben. Das gilt nur, wenn das
offene Dokument das geladene ist (Dokument-Id aus dem Laden, nach einem Fenster-Reload: gleiche
Spurnamen – und nur für das Dokument, mit dem das Fenster zurückkam; eine danach geöffnete Datei
bewegt die Engine nie). Für „geladene Partitur ist veraltet“ (Knopf „Reload“) zählt der
`mixer:`-Block **nach Bedeutung**: verglichen wird mit dem Mix, den die Engine von hier bekommen
hat (Laden, Rückschreibung, Undo/Tippen in der geladenen Partitur). Was die Rückschreibung schreibt,
ist also kein Unterschied; ein Wert, der anders in der Datei steht (auf der Platte geändert und neu
geöffnet), schon – erst „Load“ wendet ihn an. Wird die geladene Datei unverändert noch einmal
geöffnet, gilt sie weiter als die laufende (Fader schreiben weiter hinein).

*Laden ist die Wahrheit.* „Load“ wendet den `mixer:`-Abschnitt immer vollständig an, mit und ohne
Neuaufbau und egal, was die Engine vorher hatte; die Meldung sagt „Mixer applied to N tracks“. Ein
abgelehntes Laden (Spur nicht leer, Partitur läuft) ändert nichts – auch nicht den Text, den der
nächste Mitschnitt als `score.yaml` ablegt (der ist immer der zuletzt erfolgreich geladene).
Offen: Tippen im `mixer:` der geladenen Partitur bewegt die Engine sofort, die `score.yaml` des
Mitschnitts zeigt aber den Text vom Laden.

*Datei auf der Platte geändert.* Bekommt das Fenster den Fokus zurück, und vor ⌘S auf dieselbe
Datei, liest die App die Datei (`score_file_peek`, ohne Recent-Liste) und vergleicht mit dem Text
vom Öffnen/Speichern. Weicht er ab, erscheint im Arrangement die Leiste „… was changed on disk“
mit „Reload from disk“ und „Keep mine“; ⌘S schreibt bis dahin nicht darüber. „Keep mine“ gilt für
genau diesen Stand der Datei, eine weitere Änderung wird wieder gemeldet.

*Reload setzt alles zurück — ohne Rückfrage (Wunsch des Besitzers).* Liegt eine Partitur im
Runner, heißt der Knopf „Reload“ (auch während sie spielt, das Arrangement ist dann sonst
gesperrt) und tut in einem Schritt: Datei so lesen, wie sie auf der Platte steht (ungespeichertes
Tippen hier und „Keep mine“ gehen vor, die werden nie verworfen; `ui/src/arrangement/reload.ts`),
dann `score_load` mit `reset: true`. Der Host übersetzt zuerst; **eine Datei mit Fehlern setzt
nichts zurück**, es kommt nur die Fehlermeldung. Erst danach `Session::reset_for_reload`: ein
laufender Mitschnitt wird beendet und gespeichert (wie bei „Stop all"), `runner::RELOAD_CLEAR`
(`ClearAll { at: 0 }`: jede Spur leer, alles Wartende verworfen, sofort), Runner, Scheduler-Pending
und Einzähler vergessen; die Status-Schnappschüsse von vor dem Clear zählen für „Spur nicht leer"
und den Tempowechsel nicht. Dann das gewöhnliche Laden (Tempo, Setup, Mixer, Blatt) — bei anderer
Spurliste mit Neuaufbau der Engine, ebenfalls ohne Rückfrage. Ergebnis: wie frisch geladen, bereit
zum Spielen. „Reload from disk“ in der Leiste tut dasselbe, wenn eine Partitur geladen ist (sonst
öffnet es nur die Datei). Das erste „Load“ ohne geladene Partitur bleibt wie es war: nur auf leere
Spuren. Bewiesen in `runner/stop_tests.rs` (Reload während des Spiels, nach Stop all mit allen
Loops und neuem Tempo, mitten im Drum-Take) und `reload.test.ts`.

*Undo/Redo — Entscheidung.* Mix-Änderungen sind Schritte in derselben Historie wie Rasteränderungen
(`docStore.ts`), eine Geste (eine Fader-Fahrt) ist **ein** Schritt. Undo/Redo und Tippen im
`mixer:`-Block bewegen die Engine mit (`mixer_apply`): der Abschnitt der geladenen Partitur und
der Klang stimmen immer überein. Alternativen verworfen: Undo nur im Text (Datei und Klang
liefen auseinander, ⌘S speicherte etwas anderes als man hört) und Mix außerhalb der Historie
(ein späteres Undo einer Rasteränderung würde den Mix im Text zurückdrehen). Ein *anderes*
geöffnetes Dokument bewegt die Engine nie; erst „Load“ wendet seinen Mix an.

**Ohne Partitur (freier Modus)** gibt es keine Datei. Der Host schreibt Fader, Mute und Kette je
Spurname nach `mixer.json` im Konfigurationsordner (1 s nach der letzten Änderung und beim
Stoppen) und wendet ihn bei jedem Engine-Start an — auch beim Neuaufbau für eine Partitur, deren
Spuren gleich heißen (deren `mixer:` gilt danach). Einträge fehlender Spuren bleiben stehen. Die
Bus-, Klick- und Mithörpegel gehören im freien Modus zum Setup (Startkonfiguration): das UI trägt
sie dort ein, die Engine startet damit, und der Neuaufbau übernimmt sie aus dem laufenden Zustand.

Der Konfigurationsordner (`~/Library/Application Support/henrys-looper/`) lässt sich mit
`HENRYS_LOOPER_CONFIG_DIR` umlenken; die Tests benutzen eigene temporäre Verzeichnisse.

**Plugins in der Partitur: welches, nicht wie** (gebaut 2026-11). Der Zustand eines Plugins ist
ein binärer, oft mehrere Kilobyte großer Blob — er bliebe unlesbar und schlecht diffbar in der
Partitur. Darum steht dort nur, **welches** Plugin auf welcher Spur sitzt:

```yaml
tracks:
  keys:  {input: instrument, midi: {device: "KeyLab 49", channel: 1}}
  drums: {input: instrument, midi: {device: MPD218, channel: 10, notes: 36-51}}
mixer:
  tracks:
    keys:
      gain: -3
      instrument: "au:aumu:dls :appl"                          # Plugin-Id
      inserts: [{plugin: "au:aufx:dely:appl", bypass: false}]  # Signal-Reihenfolge
    guitar:
      inserts: [{plugin: "vst3:…IRDX…", before: octaver}, {plugin: "au:aufx:dely:appl"}]
```

* `before:` (seit 2026-11) nennt das eingebaute Gerät, **vor** dem das Plugin sitzt (`octaver`,
  `high_pass`, `eq`, `comp`, `delay`, `reverb`); **fehlt es, sitzt das Plugin hinter der ganzen
  eingebauten Kette** — dort, wo jedes Plugin einer älteren Partitur saß, die sich also unverändert
  liest und klingt. Die Liste ist in Signal-Reihenfolge: ein Plugin darf nicht weiter vorn sitzen
  als das davor genannte, sonst Fehler mit Zeile; ein unbekannter Name ebenso (mit Vorschlag). Der
  Rückweg schreibt `before` nur, wenn es etwas sagt. Gleiche Plugins in gleicher Reihenfolge an
  anderen Plätzen werden beim Anwenden nur verschoben, nicht neu geladen.

* `input: instrument` macht die Spur zur Instrument-Spur (immer stereo, `monitor_bus` ohne
  Angabe `main+monitor`, kein gemessenes `latency` — die Kompensation ist automatisch, nur
  `latency_trim` gilt). `midi:` sagt, was sie spielt: `device` (Name oder eindeutiger Teil, fehlt
  = alle Geräte), `channel: 1..16|all`, `notes: a-b` (fehlt = alle Tasten; die übrigen bleiben bei
  den MIDI-Bindungen). `midi:` an einer Audiospur, `instrument:` im `mixer:` einer Audiospur und
  `latency:` an einer Instrument-Spur sind Fehler mit Zeilenangabe.
* `instrument` und `inserts` sind die einzigen Mixer-Felder, bei denen **fehlend „behalten“**
  heißt (nicht Standardwert): eine Partitur von vor dieser Änderung nimmt niemandem das Klavier
  oder den Hall weg. `inserts: []` leert die Slots ausdrücklich; der Rückweg schreibt `[]`, wenn
  ein Mensch den letzten Effekt einer Spur entfernt hat.
* Die Zustände stehen **neben der Partitur** (seit 2026-11, unten „Die Plugin-Einstellungen
  gehören zum Stück“); lokal nach Spurname bleiben sie als Rückfall: `instruments.json` (mit
  einem „Regal“ der zuletzt von einer Spur genommenen Instrumente samt Zustand, höchstens 32) und
  `plugin-rack.json`.

*Laden* (`Session::queue_mix` → `app/src-tauri/src/pluginmix.rs` → Hauptthread): Sitzt das
genannte Plugin schon auf der Spur, bleibt es mit seinem Klang (bei Inserts: gleiche Ids in
gleicher Reihenfolge, nur Bypass folgt). Sonst wird geladen — ein Plugin, das auf dieser Spur
schon einmal lief (Regal bzw. bisheriger Slot), bekommt seinen Zustand zurück, ein neues seinen
Standardzustand. **Fehlt es auf dem Rechner**, läuft die Spur stumm (Instrument) bzw. ohne diesen
Effekt weiter, eine englische Notice sagt es, und `pluginmix` meldet für die Spur weiter, was die
Partitur verlangt hat — so schreibt der nächste Fader-Zug den Eintrag nicht weg. Erst wenn ein
Mensch auf dieser Spur ein Plugin wählt, gilt seine Wahl. Instrument-Routing (Gerät, Kanal,
Tasten) folgt beim Laden ohne Neuaufbau, ebenso ein anderes `monitor` einer Instrument-Spur (die
`setup_commands` der Partitur schalten es in der laufenden Engine).

*Rückweg* wie bei den Fadern: Plugin wählen, laden, verschieben, entfernen, Bypass (Maus oder
MIDI) zählt `mixer_rev` hoch (`pluginmix::touch_*`), `mixer_take` nennt die Spur, ihr Eintrag
wird mit `instrument`/`inserts` neu geschrieben, ein Undo setzt die Plugins über denselben Weg
zurück. Das Wiederherstellen beim Start und das Anwenden einer Partitur sind keine Änderung. Im
freien Modus landen die Plugin-Felder nicht in `mixer.json` (die Plugins haben ihre eigenen
Dateien).

*Monitor bei Instrument-Spuren — Entscheidung (überarbeitet 2026-11).* Der Monitor-Schalter
einer Instrument-Spur tut aus Sicht des Spielers dasselbe wie bei einer Audiospur: **an = ich
höre, was ich spiele; aus = mein Live-Spiel ist nicht zu hören, wird aber weiter aufgenommen.**
Schalten lässt er sich überall, wo eine Audiospur ihn hat: Knopf und Taste M auf der Bühne, die
MIDI-Bindung `track.N.monitor`, das Host-Kommando — im freien Modus und während eine Partitur
läuft.

* **MIDI-Loop** (`loop: midi`): Live-Tasten und Loop spielen *ein* Instrument. „Aus“ schneidet
  deshalb das Live-MIDI **vor** dem Instrument ab (`SourceRack::set_live_cut`, gesetzt aus
  `Track::live_midi_cut` vor jedem Stück eines Blocks), nicht dessen Ton: der Loop klingt weiter,
  der Instrumentenausgang einer MIDI-Loop-Spur hat immer Pegel 1 (`Track::monitor_level`).
  Abgeschnitten wird jedes Live-Ereignis (Noten, CCs samt Pedal, Pitch Bend, Aftertouch), das ab
  dem Umschalten fällig wird — auch eines, das vorher gespielt wurde und noch auf seine feste
  Verzögerung wartete. Beim Ausschalten endet, was der Spieler hält (Note-off je Taste, Pedal
  hoch), **außer** einer Taste oder einem Pedal, das der Loop ebenfalls hält (eine Stimme je
  Taste: dessen eigenes Off beendet sie); kein „all notes off“, das träfe den Loop. Was der
  Spieler im Aus drückt oder beim Ausschalten hielt, merkt sich der Rack (`unheard`): dessen Offs
  und Pedal-Hochs werden auch nach dem Wiedereinschalten verschluckt — kein streunendes Note-off,
  und eine über das Einschalten gehaltene Taste bleibt stumm, bis sie neu angeschlagen wird.
  **Aufnahme läuft weiter**: aufgenommen wird aus den Zeitstempeln (`heard`), nicht aus dem, was
  das Instrument bekam; die Noten eines im Aus gespielten Takes klingen ab der ersten
  Wiederholung. Ein im Aus gedrücktes Pedal zählt für den Take-Beginn als gedrückt
  (`live_sustain`).
* **Audio-Loop** (`loop: audio`): hier teilt sich kein Loop das Instrument, und ein abgeschnittenes
  MIDI hätte nichts zum Aufnehmen. Der Schalter wirkt deshalb wie bei einer Audiospur auf den
  Instrumentenausgang (10-ms-Blende auf der Ausgangsachse, `instrument_frame`), und der Take nimmt
  das Instrument weiter auf (`record_sources` liest es unabhängig vom Schalter). Unterschied zum
  MIDI-Loop: eine über das Einschalten gehaltene Taste wird mit dem Einblenden hörbar.

Die Sektionen schalten das Mithören einer Instrument-Spur weiterhin **nie** (`monitor_plan`,
`Runner::set_monitor`): eine Partitur, die eine Spur nur während ihrer Takes mithört, würde sonst
das Klavier zwischen zwei Takes stummschalten. `monitor` der Spur (Setup `instrument.monitor`,
Partitur `monitor: false`) ist nur der **Startzustand**: die Engine startet so, und
`setup_commands` schickt beim Laden und nach „Back to start“ ein `SetMonitor` mit dem Wert der
Partitur; danach gehört der Schalter dem Spieler, auch während die Partitur läuft. Ein
`hear_through`-Clip einer Instrument-Spur ist erlaubt und ändert nichts; der Inspektor sagt das.
Gespeichert wird der Schalter wie bei Audiospuren nicht: er lebt in der laufenden Engine, ein
Neustart, Neuaufbau oder „Clear all“ setzt ihn auf den Startwert zurück.

*Reserviert: `loop` und `grid`.* Damit das Format nicht zweimal bricht, liest, prüft und
rendert der Compiler schon jetzt `loop: audio|midi` je Spur (`midi` nur an einer Instrument-Spur)
und `grid: none|1/4|1/8|1/16|1/8t|1/16t` je Spur und je Clip (`keys: {state: play, grid: 1/16}`)
— das Notenraster für eine spätere Quantisierung von MIDI-Loops, begrifflich getrennt vom
Sektionsfeld `quantize: bar|loop` (wann eine Sektion wechselt). Gesetzt wird daraus nur ein
**Hinweis** („not supported yet“, `score_notes`, im Text-Editor als Warnung, beim Laden in der
Meldung), nie ein Fehler; die Bedienelemente dafür gibt es noch nicht.

*Kommandozeile.* `looper-engine score` lehnt eine Partitur mit Instrument-Spuren mit einer klaren
Meldung ab (sie hostet keine Instrumente und hört auf kein Keyboard); `--check` zeigt sie samt
MIDI-Routing.

**Die Plugin-Einstellungen gehören zum Stück** (gebaut 2026-11, `app/src-tauri/src/scorestate.rs`).
Vorher lagen die Zustände nur lokal nach Spurname: zwei Stücke mit einer Spur „theme“ teilten
(und überschrieben) sich die Einstellungen, und auf einem anderen Rechner kam ein Stück ohne sie an.

*Format und Ort.* Neben der Partitur, gefunden über den Namen: `The Hardest Part.yaml` →
`The Hardest Part.plugins.json`. Die YAML verweist nicht darauf und bleibt lesbar; ein
Zustand ist ein undurchsichtiger Chunk (bei einem Sampler Megabytes), der in handgeschriebenes YAML
nicht gehört. Eine JSON-Datei statt eines Ordners mit Binärdateien, weil sie sich **atomar**
ersetzen lässt (temporäre Datei, dann umbenennen) — base64 kostet ein Drittel mehr Platz.

```json
{ "version": 1,
  "tracks": {
    "guitar": { "inserts": [ {"plugin_id": "vst3:…", "name": "IRDX Studio", "slot": 0,
                              "before": "octaver", "state": "<base64>"},
                             {"plugin_id": "au:aufx:dely:appl", "name": "AUDelay", "slot": 1,
                              "bypass": true, "state": "<base64>"} ] },
    "keys":   { "instrument": {"plugin_id": "au:aumu:dls :appl", "name": "DLSMusicDevice",
                               "state": "<base64>"} } } }
```

*Lesen.* `score_load` bekommt vom UI den Pfad der Datei (`null` bei einer ungespeicherten
Partitur). Die Datei daneben wird gelesen; beim Anwenden des `mixer:` nimmt jeder Manager die
Zustände seiner Spur **einmal**: nach Reihenfolge, dann nach Id, **nie den Zustand eines anderen
Plugins** (`match_states`). Sitzt dasselbe Plugin schon auf der Spur, bekommt es den Zustand
(`load_state` auf das laufende Plugin, wie das Kommando `plugin_state_set`); sonst wird es mit ihm
geladen, vor dem Start. Was die Datei nicht hat, kommt wie bisher aus den lokalen Dateien (der
Zustand, den das Plugin auf einer Spur dieses Namens zuletzt hatte) — **bestehende Setups behalten
beim ersten Laden nach dem Update ihren Klang**, das nächste Speichern schreibt ihn in die Datei.
Nach dem Anwenden wird der Rest verworfen (`finish_load`, in der Hauptthread-Queue hinter den
Managern), damit ein späteres Undo im `mixer:` keine Zustände aus der Datei zieht.

*Schreiben — wann.* Plugin-Zustände ändern sich im Fenster des Plugins, ohne dass die App es
merkt; geschrieben wird darum zu Zeitpunkten, die „behalten“ bedeuten, nie im Hintergrund:
**⌘S / Save As der geladenen Partitur** (das UI sagt mit `loaded: true`, dass das gespeicherte
Dokument das geladene ist; bei Save As danach neben der neuen Datei), **Laden einer anderen
Partitur** (vorher die Zustände der bisherigen) und **Beenden der App**. Jedes Schreiben wird mit
dem vorhandenen Inhalt zusammengeführt (`merge`): ein Plugin, das seinen Zustand nicht liefert,
behält den der Datei; eine Spur, deren Plugins hier fehlen, behält ihren Eintrag; Spuren, die die
laufende Engine nicht hat, bleiben stehen. Eine unlesbare alte Datei wird als `….plugins.json.bak`
aufgehoben. Ein Fehler beim Schreiben der Zustände lässt das Speichern der Partitur gelingen
(nur im Log).

*Reload* liest die Datei neu: was seit dem letzten Speichern im Plugin gedreht wurde, ist weg —
wie alles andere seit dem Speichern, das ist die Bedeutung von Reload. Es wird vorher **nicht**
geschrieben (sonst wäre der Dreh gerettet und der Reload wirkungslos); halb angewendet wird nichts:
eine Partitur mit Fehlern setzt nichts zurück und liest keine Zustände, ein abgelehntes Laden
lässt die bisherige Partitur die aktuelle.

*Ohne Datei.* Eine nie gespeicherte Partitur und der freie Modus ohne Partitur verhalten sich wie
bisher (nur lokal nach Spurname). Ein Mitschnitt legt die Datei, wie zuletzt gespeichert, als
`score.plugins.json` neben `score.yaml` (Abschnitt 16).

*Kopieren und Plätze* laufen durch: die Datei hat `before` und `bypass` je Slot; eine Kopie (`Copy
to…`) ist eine gewöhnliche Belegung der Zielspur und wird beim nächsten Speichern mitgeschrieben.

*Tests* (Stellvertreter-Plugins, keine installierten): `scorestate::tests` (Ort, atomarer
Rundlauf, kaputte Datei, Zusammenführen ohne Verlust, Zuordnung nach Reihenfolge und Id, einmalige
Übergabe) und `plugins::tests::a_scores_plugin_settings_travel_with_it` (auf einem „Rechner“
erfasst, auf einem anderen angewendet, Reload über einem Dreh, Undo ohne Dateizugriff).

## 16. Mitschnitt der Performance als Stems

Jeder Durchlauf wird auf Platte geschrieben, als Stems für eine DAW. Code: `engine/src/capture/`
(Abgriff im Audio-Thread, Schreib-Thread, WAV, `session.json`, Tests) und
`app/src-tauri/src/recording.rs` (wann, wohin, Einstellungen, Liste). UI: Kopfleiste
(`ui/src/recording/RecordingBadge.tsx`) und Setup → Recording.

**Wann.** Partitur: ab Takt 1 der ersten Sektion (der Einzähler ist nicht in den Dateien) bis
„End of the score" oder „Stop all", danach ein Nachlauf bis 0,5 s Stille (−80 dBFS über alle
Kanäle), höchstens 6 s. Freier Modus: von Start bis Stopp der Uhr (Takt 1 = Uhr-Start). Engine-
Stopp, Neuaufbau und das Laden einer Partitur beenden einen laufenden Mitschnitt sofort. Läufe
unter 5 s oder ganz ohne Take werden verworfen. Ein Stopp-Knopf in der Kopfleiste beendet den
laufenden Mitschnitt von Hand; eine MIDI-Adresse dafür gibt es (noch) nicht.

**Wohin.** `~/Documents/Henrys Looper/Recordings/<YYYY-MM-DD HH.MM.SS> <Titel>/` oder der in Setup
gewählte Ordner. Einstellungen in `recording.json` im Konfigurationsordner
(`HENRYS_LOOPER_CONFIG_DIR` gilt).

| Datei | Inhalt |
|---|---|
| `01 voice.wav` | Stem, stereo: Beitrag der Spur zum Main-Bus – nach eingebauter FX-Kette, Plugin-Inserts, Fader, Mute/Solo, Pan; vor Main-Bus-Inserts und Main-Lautstärke. Plus Live-Ergänzung (s. u.) |
| `01 voice (dry).wav` | Loop-Wiedergabe (hörbare Ebenen mit Ebenen-Gain, **mit denselben Blenden wie hörbar**, Abschnitt 19) + roher Live-Eingang während Record/Overdub/Hear-through (an seinen Kanten ebenso geblendet); vor allem anderen, ohne Monitor-Lautstärke. Kanalzahl der Spur (mono bleibt mono). Abschaltbar |
| `Mix.wav` | Main-Bus nach seinen Inserts, vor der Main-Lautstärke, **ohne Klick** (auch bei zusammengelegten Bussen) |
| `score.yaml` | Kopie der geladenen Partitur (nur Partitur-Lauf) |
| `score.plugins.json` | Kopie der Plugin-Einstellungen der Partitur, wie zuletzt gespeichert (nur Partitur-Lauf aus einer Datei, die eine hat; Abschnitt 15) |
| `session.json` | Titel, Modus, Startzeit, BPM, Taktart, Samplerate, Länge, Spuren (mit `midi: null` als Platz für spätere `.mid`-Dateien), Sektionsmarker (Frame, Sekunden, Takt ab 1, Sektions-Id), Aussetzer-Zähler, Klartext-Erklärung der Dateien |

Alle Dateien: 24 Bit PCM, Samplerate der Engine, gleich lang, Frame 0 = Takt 1. Über 4 GB wird
der Header auf RF64 umgeschrieben (Platz dafür hält ein `JUNK`-Chunk frei); bei 24 Bit stereo
48 kHz sind das gut vier Stunden.

**Live-Signal im Stem.** Ohne es fehlte im Stem jeweils der erste Durchlauf eines Loops (der Musiker
spielt ihn, gehört wird er im Raum akustisch oder nur auf dem Monitor). Deshalb legt der Recorder
während Record/Overdub/Hear-through jedes Eingangs-Frame `k` an die Stelle `m = k − R` der Spur –
dieselbe Rechnung wie die Aufnahme (Abschnitt 4). Live-Durchlauf und die anschließende
Loop-Wiedergabe liegen im Stem damit auf demselben Sample (Test
`a_live_pass_and_its_loop_playback_sit_on_the_same_sample`, Impuls, Toleranz null).
Im Stem kommt diese Live-Ergänzung durch Fader, Mute/Solo und Pan, **aber nicht durch Effekte
und Inserts** – die laufen in Echtzeit einmal je Bus auf dem, was der Raum hört, und können nicht
ein zweites, zeitversetztes Signal verarbeiten. Sie wird nur addiert, wenn der Eingang die Spur
**nicht** schon über Main erreicht (Monitoring aus oder `monitor_bus` ohne Main; bei
zusammengelegten Bussen: Monitoring aus oder `monitor_bus: none`). Erreicht er Main, steht er im
Stem so, wie der Raum ihn hörte: mit Effekten und um die Roundtrip-Latenz später. Der trockene
Stem hat das Live-Signal immer latenzrichtig.

**Stem gegen Mix.** Ohne Bus-Inserts ist die Summe der Stems gleich `Mix.wav`, abgesehen von den
Live-Ergänzungen (die im Raum nicht über Main liefen). Main-Bus-Inserts sind nur im Mix; der Klick
ist in keiner Datei; die Main-Lautstärke ist in keiner Datei.

**Echtzeit.** Der Audio-Thread addiert nur in einen vorab angelegten Sammelring (Größe aus der
größten Kompensation + zwei Treiberblöcke) und kopiert fertige Frames mit `lag` Frames Verzug in
eine `rtrb`-Queue (4 s). Ist sie voll, werden die Frames verworfen und als Lücke gemeldet; der
Schreib-Thread füllt die Lücke mit genau so viel Stille, alle Dateien bleiben synchron, der Vorfall
wird gezählt (Notice nach dem Lauf, `session.json`). Die WAV-Header werden jede Sekunde
nachgeführt: Nach einem Absturz ist jede Datei bis zur letzten Sekunde lesbar (nur `session.json`
fehlt dann).

### Nachträglich neu mischen: `looper-engine render` (gebaut 2026-11)

```text
looper-engine render "<Mitschnitt-Ordner>" --out <Ordner> [--score bearbeitet.yaml]
    [--plugins <Partitur>.plugins.json] [--stems-from-recording Drums] [--only bass,bass_b]
    [--no-plugins] [--plugin-bundle X.vst3] [--compare] [--write-dry]
    [--block 128] [--outputs 2] [--monitor-db 0]
```

**Die Aufführung wird noch einmal gespielt** (`engine/src/render/perform.rs`), nicht jede Spur
einzeln durch ihren Kanalzug geschickt. Zwischen trockenem Stem und Stem liegen Pegel, die der
trockene Stem nicht trägt: `monitor_fade`, die Übergabe zwischen Spuren an einem Eingang
(`monitor: always` gibt nach), die Kantenblenden von Mithören und Takes, die Mithör-Lautstärke -
und das Live-Signal erreicht die Kette, wenn es ankam (einen Roundtrip später), nicht auf dem
Raster. Die erste Fassung (2026-11-21 vormittags) ließ das weg; die live gespielte Gitarre war dann
an Sektionswechseln doppelt und 8-14 dB zu laut. Jetzt entscheidet darüber die Engine selbst:

* Ein `EngineCore` mit **allen** Spuren der Partitur aus dem Mitschnitt, geladen wie die App lädt
  (`setup_commands`, die aufgenommene Kompensation je Spur, `mixer_commands`), Plugins mit ihrem
  gespeicherten Zustand vor `activate`; der **Runner** der Partitur fährt die Sektionen mit dem
  Einzähler aus `session.json`, Takt 1 der ersten Sektion = Frame 0, auf derselben Blockphase wie
  live (`engine_position` mod Block). Stems und Mix nimmt der **Recorder** (`crate::capture`) ab,
  der an dieser Engine hängt - derselbe Abgriff wie live, Live-Ergänzung eingeschlossen.
* **Der Eingang wird aus den trockenen Stems zurückgewonnen.** Der Recorder schrieb
  `dry(m) = Loop(m) + w(k)·Eingang(k)` mit `k = m + R`. `w` hängt nur an den Kommandos: ein erster
  Durchgang mit Stille sammelt es Frame für Frame (`process::Probe`, nur offline angehängt, die App
  hängt nie eine an). Im zweiten Durchgang ist `Eingang(k) = (dry(m) − Loop(m)) / w(k)`, `Loop(m)`
  das, was diese Engine selbst an `m` gespielt hat; genommen wird die Spur am Kanal, deren Loop dort
  schweigt, sonst die mit dem größten `w`. Wo kein `w > 0` ist, hat niemand den Eingang gehört - Stille.
  **Beweis im Lauf:** die trockenen Stems der Wiederholung werden mit den aufgenommenen verglichen
  („input reconstruction“), die Gewichte beider Durchgänge ebenso.
* `--no-plugins`: jedes Insert fällt mit lauter Warnung weg, **seine Latenz bleibt** als reine
  Verzögerung (aus dem Scan-Cache, nur gelesen) - sonst laufen Vorlesen und Loop-Einsätze anders als
  live. Instrument-Spuren und `--stems-from-recording` nehmen den aufgenommenen Stem, auch im Mix.
* **Nicht im Mitschnitt, darum angenommen und ausgegeben:** Zahl der Ausgänge (`--outputs`, 2 =
  ein Mix, Mitgehörtes erreicht Main) und Mithör-Pegel, wenn `mixer.buses.inputs` fehlt
  (`--monitor-db`). **Vorschlag für den Recorder:** beides in `session.json` schreiben, dazu den
  Eingang ab Beginn des Einzählers (die Vorrolle eines Takes ab Takt 1 stammt aus dem Einzähler)
  und die Zeitpunkte, zu denen Kommandos live tatsächlich wirkten.
* **`--compare`**: je Spur und für den Mix gegen die Aufnahme - ganzer Take, je Sektion (`live`
  markiert), und **Pegel je 0,25 s**: jedes Fenster, das um mehr als 3 dB abweicht (eine Seite über
  −80 dBFS), wird aufgeführt. Genau diese Fehlerklasse hatte der erste Nulltest übersehen.
  `--write-dry` schreibt die wiederholten trockenen Stems dazu.

**Nachmessung am Take vom 21.11.2026, 18:43** (`--no-plugins`, Spuren ohne Inserts):

| Spur | Eingang zurückgewonnen | Stem, ganzer Take | Pegelfenster > 3 dB |
|---|---|---|---|
| `mic` (Voice-Kette) | max. 1 LSB | −83,5 dB, auch in allen Live-Sektionen (−83 … −84 dB) | keins |
| `bass` (Oktaver + Kompressor) | max. 1 LSB | −43,2 dB; Live-Durchgang −42,7 dB | keins |
| `bass_b` | max. 1 LSB | −13,6 dB (kurzer Loop, Schwanz) | eins: 82,50 s, −5,9 dB |

Was bleibt, ist erklärt: die −84 dB sind die 24-Bit-Rundung des trockenen Stems (dieselben −84 dB
macht ±1 LSB Rauschen), die −43 dB das Gedächtnis des Oktavers (seine Sprungsuche läuft auf einer
4:1 dezimierten Kopie, deren Phase die Zahl der seit dem Einschalten gerechneten Samples ist -
inklusive früherer Durchläufe seit dem Laden; mit zwei Samples Versatz werden genau diese Abschnitte
bitgleich), das eine Fenster bei 82,5 s ist derselbe Oktaver am Ende des `bass_b`-Loops. `theme`
und `theme_b` (mit IRDX, hier ohne) folgen den Blenden jetzt im Verlauf, liegen ohne die
Box-Impulsantwort aber pegelmäßig daneben; ihre Rückgewinnung weicht an Loop-Nähten um einige
Millisekunden ab (−65/−61 dB): die Vorrolle des `theme`-Takes ab Takt 1 stammt aus dem Einzähler,
der nicht im Mitschnitt ist.

Tests (`render/tests.rs`): synthetische Mitschnitte, wie die App sie macht (Runner + Engine +
Recorder, ein gespielter Eingang), drei Spuren mit `monitor: always`, Übergabe, `hear_through` und
`monitor_fade`: die Wiederholung gibt dieselben Dateien (Eingang bis auf 1 LSB, Stems bitgleich oder
unter −80 dB, kein Pegelfenster); eine falsche Mithör-Lautstärke wird vom Pegelcheck gefunden;
geänderte Partitur ändert nur ihre Spur; kaputte Ordner; das Test-VST3 des Repos mit gespeichertem
Zustand gibt bitgleich den Live-Stem, mit Standardzustand nicht.

## 17. Das Blatt: Text, Akkorde und Regie neben der Bühne

Der Besitzer singt und spielt über seine Loops und muss dabei wissen, was gerade aufgenommen wird und
was als Nächstes kommt. Das **Blatt** (`sheet:` in der Partitur) ist ein Leadsheet wie eine
Karaoke-/Tab-Seite: Teile, Regieanweisungen, Zeilen mit Akkorden. Abgenommen als HTML-Prototyp; Format
und Aussehen folgen ihm. Format im Einzelnen: `docs/contracts-v0.md`, Abschnitt „YAML v0“.

**Speicherung: ein Literal-Block in der Partitur.** Der Musiker schreibt und kopiert das Blatt von
Hand (Akkordzeilen von Tab-Seiten, deren Spalten zählen); jede andere Form (Liste von Zeilen,
Strukturen) würde beim Einfügen zerbrechen. Deshalb ist `sheet` ein einziger Text, und das
Dokumentmodell (`score/doc.rs`) gibt ihn **byte-genau** zurück — anders als den Rest, dessen Layout
kanonisch ist. `render_doc` schreibt ihn als letzten Schlüssel (`sheet: |`, `|-`/`|+` nach den
Zeilenumbrüchen am Ende, `|2`, wenn der Text mit einem Leerzeichen beginnt), prüft die Rückrichtung und
fällt nur für Unschreibbares auf einen Text in Anführungszeichen zurück. `find_comments` überspringt das
Innere jedes Block-Skalars: `# Verse` ist dort Text, kein YAML-Kommentar. Bearbeitungen im Raster,
Mixer-Rückschreibung und Reload gehen alle durch `ScoreDoc` und nehmen das Feld mit. Der Runner liest
es nie; eine Partitur ohne `sheet:` kompiliert und spielt wie vorher, im JSON fehlt das Feld.

**Ein Parser, in Rust** (`engine/src/score/sheet.rs`). Die Oberfläche bräuchte ihn zum Zeichnen,
`score --check` zum Prüfen: unbekannte Anker mit Zeile, und Text, der zwischen zwei Ankern anders lang
dauert als seine Sektionen — das braucht die ganze Zeitrechnung. Zwei Implementierungen würden
auseinanderlaufen, also parst die Engine und liefert das Ergebnis mit der kompilierten Partitur
(`CompiledScore.sheet`: Elemente mit Takt-Start und -Länge, Akkorde mit Spalte, und **eine Spanne je
Sektion**). Die UI rechnet nur noch. Die Vorschau im Editor kommt deshalb aus derselben Quelle: Der Text
geht nach einer Tipp-Pause (400 ms) als normale Bearbeitung in das Dokument, das Backend rendert und
kompiliert, die Vorschau zeigt `score.sheet`.

**Sektionen statt Sekunden.** Eine Sektion ohne `autorelease` loopt, bis Release kommt — „Takte seit
dem Start“ liefe danach am Text vorbei. Deshalb hängt das Blatt an Sektionen: `# Verse 1 @verse_1`
verankert einen Teil am Anfang der Sektion; jede Sektion bekommt eine Spanne `[start, end)` des Blatts
(verankert: am Anker; sonst einen Durchlauf nach der vorigen; Ende = Anfang der nächsten). Ohne Anker
laufen die Sektionen einfach der Reihe nach. Abbildung (`ui/src/sheet/position.ts`), aus dem, was der
Status ohnehin trägt (Sektion, Durchlauf, Takt, Schlag, `beat_offset`, armierter Wechsel samt
Abstand):

| Lage | Stelle auf dem Blatt |
|---|---|
| Ready, Stop all, Ende, Back to start | Ruhe: oben, nichts hervorgehoben, NEXT = erste Anweisung |
| Einzähler | Anfang der ersten Spanne, NEXT zählt „in N bars“ herunter |
| Sektion mit `autorelease` | linear durch die Spanne; ist der Text kürzer, bleibt die letzte Zeile stehen |
| Sektion bis Release | `Anfang + (verstrichen mod max(Spannenlänge, Sektionstakte))` — bei gleicher Länge springt der Text mit jedem Durchlauf nach vorn, ein längerer Text (Strophe über einem Zwei-Takt-Loop) läuft über mehrere Durchläufe |
| Wechsel (Release, Autorelease, Ende des Einzählers) | Sprung an den Anfang der nächsten Spanne, genau auf dem Taktstrich |

**Pünktlich trotz 20 Status je Sekunde.** Zwischen zwei Status rechnet die Seitenleiste je Bild mit
dem Tempo weiter (höchstens 250 ms über den letzten Status hinaus, damit eine stehende Engine nicht
weiterläuft). Den Wechsel sagt der armierte Wechsel voraus: Sein Abstand ist bekannt (`bars`, `beats`
minus der angebrochene Schlag), also springt das Blatt beim Überschreiten selbst in die Folgesektion,
auch wenn der Runner es erst einen Steuertakt später meldet. NEXT nennt Anweisungen der Folgesektion
in einer loopenden Sektion „after Release“, bis Release gedrückt ist; dann „in N bars“.

**Zeichnen ohne Rendern.** `SheetView` zeichnet die Seite einmal je Blatt; je Bild werden nur Klassen
(`cur`, `past`, `active`), der Fortschritt (`--p`) und zwei Texte gesetzt — dieselbe Disziplin wie bei
den Pegeln (`status.ts`). Mitscrollen hält die aktuelle Zeile in der Mitte; Mausrad, Wischen,
Scrollbalken oder Pfeiltasten im Blatt pausieren es für vier Sekunden.

**Welches Blatt.** Die Live-Ansicht zeigt das Blatt der geladenen Partitur — oder das des geöffneten
Dokuments, solange dessen Sektionen (Ids, Takte, Autorelease) mit der geladenen übereinstimmen. So
erscheint eine Textkorrektur sofort auf der Bühne, ohne Reload (der jede Spur zurücksetzt).

**Bedienung.** Seitenleiste rechts in der Live-Ansicht, Taste `L` oder die schmale Leiste am Rand;
Breite (Anteil am Fenster) am linken Rand ziehen, Schrift A−/A+; beides und offen/zu in
`localStorage` (`live.sheet`). Ohne gespeicherte Wahl startet sie nur ab 1280 px Fensterbreite offen —
bei 1024 px braucht die Bühne den Platz. Ohne Blatt zeigt sie, wie man eins anlegt. Im Arrangement ist
*Sheet* der dritte Reiter neben *Grid* und *Text (YAML)*: Editor links, Vorschau rechts, die Probleme
des Blatts darunter (Klick springt zur Zeile, Klick in der Vorschau ebenso).

| Datei | Inhalt |
|---|---|
| `engine/src/score/sheet.rs` | Parser, Spannen je Sektion, Prüfungen (Fehler: Anker; Hinweise: Längen) |
| `engine/src/score/read.rs` | `sheet` als Feld; Prüfung auf den aufgelösten Sektionen, Zeilen der Datei |
| `engine/src/score/doc.rs` | `ScoreDoc.sheet`, Block-Skalar schreiben, `#` im Block ist kein Kommentar |
| `ui/src/sheet/position.ts` | Status → Stelle auf dem Blatt, NOW/NEXT |
| `ui/src/sheet/SheetSidebar.tsx`, `SheetView.tsx`, `SheetEditor.tsx` | Seitenleiste, Seite, Editor |

## 18. Audiolast und Overruns: woher ein Knackser kommt

Frage, die das beantworten soll: **Ist ein gelegentlicher Knackser eine Überlast?** Dafür misst
der Output-Callback, wie viel vom Zeitbudget seines Blocks er braucht. Budget = Spielzeit des
Blocks, `frames / Abtastrate` (128 Frames bei 48 kHz: 2,67 ms). Was länger braucht, ist zu spät.

**Gemessen** (`engine/src/load.rs`, `LoadClock`, im Callback; `LoadShared`, Atomics für den
Steuer-Thread):

- **Last je Callback** = Rechenzeit / Budget. Veröffentlicht als **Spitze** (schlimmster einzelner
  Callback) und **Mittel** (Summe Rechenzeit / Summe Budget) über die letzte Sekunde — vier
  Viertelsekunden-Fenster, in Audio-Frames gezählt, nicht in Wanduhrzeit. Die Spitze ist die
  Zahl, die über Aussetzer entscheidet: 30 % Mittel mit 105 % Spitze knackt.
- **Plugin-Anteil**: Die Racks messen jeden Plugin-Aufruf ohnehin (Instrumente in `source.rs`,
  Inserts in `insert.rs`); sie summieren jetzt die Nanosekunden, `EngineCore::take_plugin_ns`
  holt sie einmal je Callback ab. Zwei Additionen, keine weitere Uhr.
- **Späte Blöcke**: Rechenzeit > Budget.
- **Späte Weckrufe**: Abstand zweier Callback-Starts > 2 × Blockdauer. Entweder lief der Callback
  davor zu lang (dann folgt der Weckruf dem späten Block), oder das System hat ihn zu spät geweckt;
  welches von beiden, sagt sein Profil (unten).
- **Treiber-Overloads**: cpal 0.18 registriert auf macOS selbst einen Listener auf
  `kAudioDeviceProcessorOverload` und meldet ihn als `ErrorKind::Xrun` — der vorhandene
  `xruns`-Zähler des Hosts *ist* also CoreAudios Overload-Meldung; kein eigener Listener nötig.
  Bei Ein- und Ausgang auf demselben Gerät meldet er eine Überlast einmal je Stream.
- **Eingang verloren / zu spät** (`input lost`, `input late`): die beiden FIFO-Zähler des Hosts.
  *Verloren* (FIFO voll, Frames verworfen) ist die eine Art, die einen laufenden Take schneidet:
  Die Frames nach der Lücke landen direkt hinter denen davor, ein Sprung von einem Sample, den der
  Loop bei jedem Durchlauf wiederholt (so `theme_b` im Mitschnitt vom 21.11., Abschnitt 19 — dort
  war es eine Treiber-Überlast, die einen ganzen Zyklus verlor). *Zu spät* (FIFO leer) verliert
  nichts, lässt aber das Mithören für den Rest des Blocks verstummen. Beide waren bisher nur in
  `dropouts.engine_dropouts` gezählt; jetzt sind sie eigene Arten im Overrun (Log, Zähler der
  Anzeige, `overruns` in `session.json`), an der Positionsschätzung des Hosts — ein Knackser in
  einem Take lässt sich so einem Eintrag zuordnen.

**Kosten**: je Callback eine zusätzliche Uhrablesung (`Instant::now` liest auf macOS die
Commpage, kein Syscall), ein paar Ganzzahl-Operationen und Atomics; viermal je Sekunde drei
Stores. Gemessen ~40 ns je Callback samt beider Uhrablesungen — 0,0015 % von 2,67 ms.

**Overruns** (`app/src-tauri/src/loadwatch.rs`): Was innerhalb von 60 ms zusammen steigt (später
Block, Treibermeldung je Stream), ist **ein** Overrun. Abgeschlossen (60 ms ohne neuen Anstieg)
wird er

- ins Log geschrieben — höchstens eine Zeile je 2 s, was dazwischen kommt, wird gezählt und mit
  der schlimmsten Last nachgereicht: Uhrzeit, Art, Last in % und ms, Spitze/Mittel/Plugins der
  letzten Sekunde, Engine-Frame, Puffer, und bei laufender Partitur Sektion und Takt;
- als Marker in `session.json` der laufenden Aufnahme eingetragen: `overruns: [{frame, seconds,
  kind, load_percent}]` auf der Achse der Dateien (höchstens 1000 je Aufnahme). Ein Knackser im
  WAV bei 83,2 s lässt sich so neben einen Overrun bei 83,2 s legen. Der Frame ist bei späten
  Blöcken exakt (der Callback zählt seine Frames, wie `out_pos`), bei reinen Treibermeldungen die
  Positionsschätzung des Hosts (einige ms später).

**Status**: `load` im Status-Event (`proto::LoadStatusEvent`, `ui/src/types.ts` `LoadStatus`).
Der Engine-`Status` (der durch den Callback wandert) wächst **nicht** — die Last läuft über
eigene Atomics, die Größenbremse von 2400 Byte bleibt unberührt.

**Was das UI zeigt**: In der Titelleiste neben der Status-Pille ein kurzer Balken mit der
Spitzenlast (`ui/src/shell/LoadMeter.tsx`, Logik `loadModel.ts`), gedämpft wie die Pegelmeter
(steigt sofort, fällt 25 %-Punkte je Sekunde, Zahl höchstens viermal je Sekunde). Ab 70 %
Warnfarbe, ab 100 % und 4 s nach einem Overrun Alarmfarbe; ab dem ersten Overrun ein roter Zähler,
den ein Klick zurücksetzt (der Klick öffnet auch das Engine-Panel). Im Engine-Panel: Spitze,
Mittel, Plugins, Overruns mit Aufschlüsselung, letzter Overrun, Puffer/Budget und bei Overruns
„Try a larger buffer in Setup › Audio“. Damit das bei 1024 px passt, zeigt die Status-Pille unter
1500 px nur ihre Lampe (Format und Dropouts im Tooltip und im Panel).

Nicht gemacht: Die CLI (`live`, `score`, `soak`) misst die Last nicht; dort bleibt es bei
`Callback max` und den Xruns.

### Was die Zeile vom 21.11., 17:26 wirklich sagte

`Audio overrun #1 … (late block + late wake-up + driver overload): 7 % of the 2.67 ms budget
(0.19 ms); last second peak 5 %, average 3 % … Section "verse_2", bar 1.` Die 7 % gehörten **nicht**
zum späten Block. Ein Overrun fasst alles zusammen, was innerhalb von 60 ms steigt; genannt wurde
die Last des **letzten** späten Callbacks (`last_frame`/`last_permille` werden von jedem überschrieben).
Ein später Block zieht fast immer einen späten Weckruf nach sich: Der nächste Callback beginnt erst,
wenn der lange fertig ist – mehr als zwei Budgets nach dem Start des vorigen. Dieser zweite war
schnell (0,19 ms = 7 %), seine Zahl stand in der Zeile, die Dauer des langsamen ging verloren. Dass
er mehr als 5,33 ms nach seinem Vorgänger begann, heißt: Der späte Block selbst lief mehrere
Millisekunden. „Last second peak 5 %“ widerspricht dem nicht: Spitze und Mittel werden viermal je
Sekunde veröffentlicht, die Zeile wird 60 ms nach dem Overrun geschrieben – meist bevor das
Fenster mit dem späten Block veröffentlicht ist. Die Rechenlast der App war also klein, **ein**
Block war es nicht. Sein Frame (im Mitschnitt 2 939 316, der des Weckrufs, der späte Block 128
davor) liegt rund 44 ms **vor** dem Taktstrich von `verse_2`, auf der Ausgabeachse.

Derselbe Mitschnitt zählt `engine_dropouts: 2` – bei einem einzigen Overrun mit „driver overload“:
cpal meldet CoreAudios Overload einmal je Stream. Der Mitschnitt von 14:57 (Schnitt in `theme_b`
133 ms nach Beginn von `intro_b`) zählt ebenfalls 2, ohne Overrun-Zeile (die gab es noch nicht).
Wahrscheinlich war es auch dort **ein** Overload, also ein im Treiber verlorener Zyklus – kein
übergelaufener FIFO (dafür hätte der Output-Callback 170 ms stehen müssen).

### Jeder späte Callback bringt sein Profil mit

Damit der nächste Aussetzer seine Ursache nennt, führt jeder Callback ein Profil
(`load::BlockProfile`, gefüllt von `EngineCore` und dem Output-Callback des Hosts):

| Feld | woher |
|---|---|
| Zeit je Phase: `prepare` (Puffer, Kommandos, Racks, Aufnahme-Übergabe, Clips, FIFO), `instruments`, `commands` (Anwenden fälliger Kommandos), `mix` (Loops, Mithören, Ketten, Inserts, Fader, Klick, Stem-/Mix-Abgriff), `record` (Takes, Live-Signal, MIDI), `capture` (Übergabe an den Schreib-Thread), `status`, `output` (Platzierung, Limiter) | eine Uhrablesung je Phasengrenze (`EngineCore::lap`) |
| Zeit je Spur | im Blockpfad (sobald ein Insert steckt – beim Besitzer immer): je Spur und Block, Plugins eingeschlossen |
| Plugin-Zeit | die Racks wie bisher |
| angewendete Kommandoarten | Bitmaske über `Command::kind` (`RestartLoop`, `CutTake`, `StartRecord` …) |
| Allokationen / Freigaben im Callback | `rt::CountingAlloc` (s. u.) |
| User- und System-CPU des Threads, daraus „nicht laufend“ = Wanduhr − CPU | `thread_info(THREAD_EXTENDED_INFO)` am Anfang und Ende, ~0,5 µs je Aufruf |
| Frames im Eingangs-FIFO zu Beginn, davon genommen | `fifo::FifoReader` |

War ein Callback spät (eigene Dauer über dem Budget **oder** Start mehr als zwei Budgets nach dem
vorigen), schiebt `LoadClock` einen `LateDetail` in eine lock-freie Queue (32 Plätze): sein Profil,
das des vorigen Callbacks, die eigene Dauer, den Abstand zum vorigen Start, die Wartezeit nach dem
Ende des vorigen. Der Host hängt sie an den Overrun und schreibt **jeden späten Callback einzeln**
ins Log (`loadwatch::describe`), mit einer kurzen Ursache (`loadwatch::cause`):

* eigener Block zu lang → Allokationen im Callback; „nicht laufend X ms“ (verdrängt, oder Warten
  auf Lock oder Page-in); „X ms im Kernel“ (Seitenfehler, Systemaufrufe); „X ms in Plugins“; sonst
  die langsamste Phase;
* nur spät geweckt → „der Callback davor lief X ms“ oder „vom System X ms zu spät geweckt – der
  davor war schnell“ (Treiber, Kernel, anderer Echtzeit-Thread);
* nur der Treiber meldet → „kein Callback war spät: Treiber oder Eingang hat einen Zyklus verloren“.

Dazu die **Seitenfehler des ganzen Prozesses** in den letzten 100 ms (`getrusage`, im Host-Thread
je Runde, nicht im Callback) – davon „von Platte gelesen“. Beispiel (aus dem Test, der den Fall vom
21.11. nachstellt):

```
late block at frame 12800: took 6.00 ms (225 % of 2.67 ms), started 2.67 ms after the previous
callback (which took 0.08 ms); CPU 5.92 ms (system 0.02), not running 0.08 ms; phases (ms): mix 5.80,
commands 0.15; plugins 5.60 ms; slowest tracks (ms): Drums 5.70; commands applied: StartRecord,
RestartLoop; allocations 0 / frees 0; input FIFO 0 frames waiting, 0 taken - 5.60 ms inside plugins
| late wake-up at frame 12928: took 0.19 ms (7 % of 2.67 ms), started 6.10 ms after the previous
callback (which took 6.00 ms); … - the callback before it ran 6.00 ms (over its 2.67 ms budget)
```

In `session.json` bekommt jeder Overrun zusätzlich `block_ms` (Dauer des langsamsten späten
Callbacks), `wake_gap_ms` (größter Abstand zweier Starts) und `cause`. Einmal je Sitzung loggt der
Host Priorität und Policy des Callback-Threads (97 = Echtzeit, wie CoreAudio ihn anlegt), und
sofort, wenn im Callback alloziert wurde. Ändert der Eingang seine Blockgröße, steht auch das im
Log: coreaudio-rs (unter cpal) fragt dann im Echtzeit-Thread des Eingangs das Gerät ab und
vergrößert seinen Puffer – vor unserem Callback, also für den Zähler unsichtbar.

**Kosten** des Profils: etwa 15–25 Uhrablesungen je Block (Commpage, je ~20 ns), zwei
`thread_info` (gemessen 465 ns je Aufruf) – zusammen gut 1 µs je Callback, 0,05 % des Budgets bei
128 Frames. `bench_loop_playback` (Abschnitt 19) zeigt 194 ns je Frame gegen 183 vorher – innerhalb
der dort genannten ±15 %, gemessen während die App lief. (Mit 206 ns, wenn die Simulation
`place_output` wie bisher Frame für Frame ruft: dort liest die Ausgangsphase die Uhr je Frame, in
der App je Block.)

### Der Zähl-Allokator

`rt::CountingAlloc` ist der globale Allokator der App, der CLI und der Engine-Tests: das
System-Malloc, das bei jedem Aufruf ein thread-lokales Flag liest. Ist der Thread „im Callback“
(`rt::arm()`, der Output-Callback setzt es für seine ganze Dauer), wird gezählt – in thread-lokalen
Zellen, ohne Atomics. **Immer an**: außerhalb kostet es eine TLS-Lesung je Allokation, im Callback
eines korrekten Programms passiert nichts. Kein Schalter in Setup, keine Umgebungsvariable – es gibt
nichts abzuwägen.

### Geprüft gegen die Partitur des Besitzers

`engine/src/engine/rt_tests.rs` fährt die Partitur vom Abend des 21.11. (`tests/data/
hardest-part-live.yaml`, ohne `sheet:`) durch den echten `EngineCore`, mit allem, was die App
anhängt: Inserts vor dem Octaver auf `theme`, `theme_b`, `chorus_guitar` (Testdoppel mit 8 Frames
Latenz, wie IRDX), ein Instrument (Testdoppel) auf `Drums` mit MIDI-Loop und Raster 1/8, Pads
während des Drum-Takes in `verse_2` (einer kurz vor dem Taktstrich), die laufende Aufnahme mit
Schreib-Thread (17 Dateien), danach „Stop all“, Ende der Aufnahme, Reload-Clear. Geprüft:

* **null Allokationen und Freigaben im Callback**, über alles – mit und ohne Plugins;
* dass der Lauf das Gemeinte enthält: `StartRecord`, `StopRecord`, `StartPlay`, `StopPlay`,
  `RestartLoop`, `CutTake`, `MonitorFade`, `SetMonitor`, `DropWaiting`, `ClearAll`, `ClickGate`,
  `TransportStart`, `SetLoopKind` wurden angewendet, der Drum-Take hält die Pads;
* ein Gegenbeweis: ein Plugin, das in `process` alloziert, wird mit Block und Profil gefangen;
* **keine Sperre auf dem Audiopfad**: Locks lassen sich nicht zählen, also liest
  `no_lock_on_the_audio_path` den Quelltext aller Module, durch die der Callback läuft. Die einzige
  Sperre dort (`source::FeedHome`) nimmt nur der Steuer-Thread; ein `PortFeed`, der im Callback
  fiele, würde zudem als Freigabe gezählt.

**Seitenfehler** zählt der Allokator nicht. Dafür gibt es einen Diagnose-Test
(`page_faults_of_the_owners_score`, `--ignored`, mit `RT_PROBE_NO_RECORDING=1` ohne Schreib-Thread),
der jeden `process`-Aufruf mit dem Fehlerzähler des Prozesses klammert. Vorher **2284 Seitenfehler
in 1750 Callbacks** eines Laufs, jetzt **0** (zweiter Lauf im selben Prozess; im ersten noch einer:
Code, der zum ersten Mal läuft). Gefunden und behoben:

| Was | wann es faultete | jetzt |
|---|---|---|
| Aufnahme-Queue (`rtrb`, uninitialisiert, ~20 MB bei acht Spuren mit trockenen Stems) | jeder Block in den ersten 4 s **jeder** Aufnahme | `rt::prefault_ring` beim Anlegen |
| Sammelring der Aufnahme | erste Runde | `rt::resident_zeros` |
| Status-Queue (2,4 KB je Schnappschuss) | jeder siebte Schnappschuss in den ersten Sekunden | `prefault_ring` |
| Vorlauf-Slot je Ebene (`Track::pre_rolls`, 128 KB je Spur) | beim **ersten Take jeder Ebene**, im Block des `StartRecord` | `resident_zeros` |
| News-Queue der MIDI-Clips (16 384 Einträge) | erstes Ereignis eines MIDI-Takes, danach alle paar hundert | `prefault_ring` |
| MIDI-Clips (`raw`, `play`), Event-Listen der Instrumente, Rack-Scratch | erstes Ereignis / erster Block | `rt::touch_spare`, `resident_zeros` |
| Rückgabe-Queues (Puffer, Clips, Inserts, Instrumente, Feeds), Kommando-Queue | erste Rückgabe | `prefault_ring` |
| Stack des Callback-Threads | erster tiefe Pfad (erster MIDI-Take, seltener Plugin-Zweig) | erster Callback berührt 128 KB vorab (`rt::prefault_stack`, höchstens den halben Stack) |

Ein Seitenfehler auf frischem Speicher kostet sonst wenige Mikrosekunden – unter Speicherdruck
(großes Sample-Instrument, Browser, komprimierter Speicher) muss der Kernel aber erst eine Seite
freimachen, und das kann Millisekunden dauern, im Audio-Thread. Zwei davon lagen genau auf
Sektionswechseln: der erste Take einer Ebene und der erste Ton eines MIDI-Takes.

### Der Eingangs-FIFO

`engine/src/fifo.rs` (App und CLI). 64 Blöcke Platz (170 ms bei 128 Frames): Damit Eingang
**verloren** geht, müsste der Output-Callback 170 ms stillstehen. Ein einzelner später Callback
kann das nicht; zu knapp bemessen ist der FIFO nicht. Ein verlorener Treiber-Zyklus (CoreAudio-
Overload) fehlt dagegen schon im Gerät – der FIFO bekommt ihn nie zu sehen.

Gefunden wurde etwas anderes: Der Output-Callback nahm je Block **höchstens** einen Block. Kam ein
Eingangs-Callback zu spät (Ausgang fand den FIFO kurz, „input late“), blieb sein Block danach für
den Rest der Sitzung im FIFO liegen – jedes Mal so viel Mithör-Verzögerung mehr, wie der Eingang zu
spät war (die Doku versprach „holt es beim nächsten Mal auf“, der Code tat es nicht). Jetzt nimmt
der Output alles mit, was über **einen Block Polster** hinaus wartet. Die Engine schreibt alle
Frames in die Takes (ein Eingangsblock darf länger sein als sein Ausgangsblock), das Mithören hört
die ersten `n` und überspringt den Rest einmal – an der Stelle, an der der späte Eingang ohnehin
eine Lücke gerissen hat. Der Polster-Block fängt den nächsten späten Eingang ohne Lücke ab. Kosten:
höchstens ein Block (2,67 ms) Mithör-Verzögerung, und nur nach einem solchen Ereignis; vorher waren
es beliebig viele. Die Kompensation `m = k − R` bleibt unberührt (sie zählt Frames, nicht Zeit).
Bewiesen mit `fifo_tests::a_take_through_a_jittery_fifo_is_the_input_shifted_by_the_latency`
(Eingang mal vor, mal nach dem Ausgang, drei Blöcke zurückgehalten und auf einmal geliefert: der
Take ist Sample für Sample der Eingang um `R` verschoben, nichts verloren, Rückstand höchstens zwei
Blöcke, nichts alloziert) und `fifo::tests::a_late_input_no_longer_stays_as_delay` (vorher blieben
nach einem um drei Blöcke späten Eingang vier Blöcke im FIFO, jetzt zwei).

Nicht gebaut: einen vom Treiber verlorenen Zyklus mit Stille aufzufüllen. Ob nur der Eingang oder
beide Streams ihn verloren haben, lässt sich im Callback nicht sicher sagen; falsch aufgefüllt
würde jeder spätere Take um einen Block verrutschen.

### Threads neben dem Callback

* **Callback-Thread**: der HAL-IO-Thread von CoreAudio (cpal registriert AUHAL-Render-Callbacks).
  CoreAudio legt ihn als Time-Constraint-Thread (Priorität 97) an und führt ihn in der Workgroup des
  Geräts; cpal tut dafür nichts und muss nichts tun. Der Host loggt die Priorität beim Start.
  Worker-Threads eines Plugins (Faltung, Sample-Streaming) treten der Workgroup nicht bei – VST3
  kennt dafür keinen Weg. Wartet ein Plugin im Callback auf einen eigenen Thread, sieht man das im
  Profil als „not running“ in der Plugin-Spur.
* **Host-Thread** (Runner, MIDI, Pool, Status, Log): jetzt `QOS_CLASS_USER_INTERACTIVE`, weil er im
  4-ms-Takt Kommandos, Pads und Puffer liefert. Er teilt keine Sperre mit dem Callback (nur
  `rtrb`-Queues und Atomics) und kann ihn nicht verdrängen.
* **Schreib-Thread der Aufnahme**: bleibt mit Absicht in der Standardklasse. `utility` würde seine
  Schreibzugriffe hinter anderem Plattenverkehr (Sample-Streaming) drosseln lassen; die 4-s-Queue
  liefe dann über. Er schreibt 17 Dateien über je 256-KB-Puffer, bringt jede Sekunde die Header
  auf Stand (flush, kein fsync) und ruft `fsync` nur einmal am Ende – kein Ausbruch an Take-Enden.
* **Was der Host beim Sektionswechsel tut**: Runner-Tick, Marker in den Aufnahme-Metadaten (eigene
  Sperre, nur Steuer- und Schreib-Thread), Notice- und Status-Event an die Oberfläche (JSON), eine
  Logzeile; der Pool ersetzt den Puffer, den ein neuer Take genommen hat (bis zu ~10 MB, im
  Steuer-Thread geschrieben). Nichts davon teilt eine Sperre mit dem Callback.

### 128 oder 256 Frames?

128 bleibt die Voreinstellung. Die gemessene Rechenlast (Spitze 5 %, Mittel 3 %) lässt das Zwanzigfache
an Reserve; ein größerer Puffer behebt keinen Fehler, er versteckt Ausreißer bis zu einer bestimmten
Länge. Was er kostet: Bei 256 Frames wächst der Roundtrip um zwei Blöcke (≈ 5,3 ms, `R` ≈ 852 statt
596 Frames, neu kalibrieren), das Mithören wird um dieselbe Zeit später – für Stimme über Kopfhörer
an der Grenze des Spürbaren. Was er bringt: Ein Stillstand bis ~5 ms statt ~2,5 ms bleibt folgenlos.

Die Entscheidung hängt deshalb an dem, was die nächste Zeile sagt: Steht dort „woken … late by the
system“ oder „not running … ms“ (Kernel, Treiber, Page-in) oder ein einmaliger Plugin-Ausreißer unter
~5 ms, ist 256 für Auftritte mit SSD5 eine vertretbare Versicherung. Steht dort eine Allokation, eine
Phase der Engine oder ein Plugin, das regelmäßig über dem Budget liegt, ist es ein Fehler, den 256
nur seltener macht.

## 19. Kein Knacksen: Blenden an jeder Kante eines Loops

Ein Take beginnt und endet mitten in dem, was gespielt wird; sein erstes und sein letztes Sample
treffen sich fast nie. Ein Loop spielt aber bei jedem Durchlauf das eine direkt nach dem anderen.
Roh abgespielt ist das an **jeder Naht** eine Stufe von einem Sample auf das nächste — und mit dem
tieffrequenten Anteil einer Piezo-Gitarre ein deutlich hörbares Knacken, alle vier Takte. Dasselbe
an jedem **Einsatz** (von exakt 0 auf den ersten Wert), jedem **Stopp** (vom laufenden Wert auf 0),
jedem **Neuanfang** (`RestartLoop`: von einer Stelle des Loops auf seinen Anfang) und beim
**Mithören**, das bisher hart ein- und ausgeschaltet wurde. Gefunden im Mitschnitt „The Hardest
Part" vom 2026-11-21 (Theme +0,0027 → −0,0319 an jeder Naht, im Mix +0,0667 → +0,0203); der
Regressionstest unten zählt im Stand davor **78 Knackstellen** in einem Durchlauf der Partitur,
jetzt **0**.

Bis dahin gab es Blenden nur in der Effektkette (Schalter und Bypass, 20–80 ms, Abschnitt 9) und
am Kanalzug (Fader, Mute, Solo, 10 ms). Keine davon lag auf dem Weg des Loops selbst: die Summe der
Ebenen ging ungeblendet in die Kette.

### Die Regel: Blenden liegen vor der Kante, der erste Frame bleibt unberührt

Alle Blenden sind **10 ms** lang (`track::EDGE_MS`, 480 Frames bei 48 kHz), Form: halbe
Kosinus-Periode, amplitudenkomplementär (`fade_in + fade_out = 1`). Warum so:

* **10 ms** decken die tiefen Frequenzen ab, um die es geht: Eine Stufe, die über 480 Frames
  verteilt wird, hat oberhalb von ~100 Hz keine Energie mehr. Kürzer (1–3 ms) macht aus dem Klick
  einen Tupfer; länger verwischt mehr vom Loop, ohne mehr zu nützen.
* **Amplitudenkomplementär statt gleicher Leistung**, weil das, was knackt, tieffrequent ist — und
  das ist auf beiden Seiten einer Naht über wenige Millisekunden fast dasselbe Signal. Zwei
  korrelierte Signale mit gleicher Leistung überblendet wölben sich um bis zu 3 dB auf: ein Wummern
  an jeder Naht. Für unkorrelierte Anteile ergibt die Amplitudenblende eine Delle von höchstens
  3 dB für ein paar Millisekunden, die man nicht hört.
* **Kosinus statt Gerade**, weil er an beiden Enden ohne Knick ansetzt.

**Wo** geblendet wird, ist die eigentliche Entscheidung: **vor** der Kante. Der erste Frame eines
Durchlaufs, eines Einsatzes, eines Neuanfangs ist der Anschlag auf der Eins — der klingt
unverändert, sample-genau dort, wo er immer lag. Was angepasst wird, sind die 10 ms davor: das
Ausklingen am Ende eines Durchlaufs, die Stille vor einem Einsatz.

### Woher das Material vor der Kante kommt: der Vorlauf jeder Ebene

Eine Naht ist nur dann sauber zu überblenden, wenn man weiß, was **vor** dem ersten Sample des
Takes gespielt wurde — das ist das Signal, das physikalisch stetig in ihn hineinläuft. Dafür
merkt sich jeder Track die letzten 10 ms seines Eingangs (`Track::remember`, ein Ring je Track,
im Steuer-Thread alloziert, jedes Eingangs-Frame, ob ein Take läuft oder nicht). Schreibt ein
Take sein erstes Frame, kopiert die Ebene diese 480 Frames als ihren **Vorlauf**
(`Track::write`). Dazu merkt sich jede Ebene ihre **Naht** (der Loop-Index ihres ersten Frames: 0
beim Take, der den Loop festlegt, die Rasterstelle bei einem Overdub) und ihr **Ende** (die Naht,
oder bei einem vorzeitig beendeten Overdub der Index nach dem letzten Frame).

Abgespielt (`Track::play_frame`, `Layer::parts`) wird jede Ebene dann so:

| Stelle | was klingt |
|---|---|
| die letzten 10 ms vor der Naht | eigener Inhalt × `fade_out` + Vorlauf × `fade_in` — beim letzten Frame vor der Naht ist man fast ganz im Vorlauf, und der läuft in das erste Frame hinein |
| ab der Naht | der Take, Sample für Sample |
| die letzten 10 ms vor dem Ende eines kurzen Overdubs | eigener Inhalt × `fade_out` (dahinter ist die Ebene still) |

Jede Ebene hat ihre eigene Naht; Overdubs, `stack`-Durchläufe und Ebenen auf einem neu verankerten
Raster sind damit ohne Sonderfall abgedeckt (der Index einer Ebene ändert sich beim Neuanfang
nicht, nur wann er klingt). Der Vorlauf eines `stack`-Durchlaufs ist das Ende des vorigen — die
Eingabe läuft ja durch. Der Vorlauf eines Takes, der aus der Stille beginnt, ist eben diese Stille
mit dem, was die Gitarre gerade summt; das Ende des Loops blendet dann sanft dorthin aus.

### Einsatz, Stopp, Neuanfang: Befehle, die eine Blende früher wirken

Damit ein Einsatz **vor** seiner Position einblenden und ein Stopp **auf** seiner Position still
sein kann, muss der Track sie 10 ms im Voraus kennen. `EngineCore::due` lässt deshalb vier
Befehle auf Audio-Loops früher wirken:

| Befehl | wirkt | was er dann tut |
|---|---|---|
| `StartPlay` / `StopPlay` | 10 ms früher | merkt einen Wechsel der Hörbarkeit für genau seine Position vor (`Track::arm_playing`) |
| `RestartLoop` | 10 ms plus Vorauslese früher | wie bisher `LoopGrid::restart`, dazu das Blendfenster |
| `StopRecord` | 10 ms früher, nie vor Beginn seines Takes | setzt die Loop-Länge wie bisher — nur früher, damit ein Einsatz genau am Take-Ende schon blenden kann. Die Aufnahme endet weiter auf dem Sample: es ist eine Position |

Was geschieht, steht in einer kleinen Liste je Track (`Switch`, höchstens vier, nichts alloziert)
und wird pro Position ausgewertet — dieselbe Funktion für den Mix (mit Vorauslese) und den
trockenen Stem (ohne):

* **Einsatz an `P`** — die 10 ms vor `P` blenden ein, was in `P` hineinführt. Eine Ebene, deren
  Naht `P` ist (ein Loop, der vorn beginnt — nach einem Take, mit `restart`, oder einfach auf
  seinem Raster), steuert dort **nur ihren Vorlauf** bei: vom vorigen Durchlauf, den niemand hörte,
  sickert nichts durch. Jede andere Ebene blendet mit ihrem eigenen Inhalt ein.
* **Stopp an `P`** — ab `P` still, die 10 ms davor blenden aus. Liegt `P` auf der Naht, läuft der
  Durchlauf durch seine eigene Nahtblende aus, ohne den Vorlauf des nächsten, der nicht kommt.
* **Neuanfang an `P`** — altes Raster aus, neues ein, über dieselben 10 ms vor `P`, jedes nach der
  Regel oben; der neue Anfang klingt unberührt.
* Kommt ein Befehl zu spät für die volle Blende, wird sie verkürzt; liegt seine Position schon
  hinter dem Lesezeiger, blendet er **ab** dort.
* **Ungetimte und späte Wechsel** blenden ab dem Sample, an dem sie wirken: das `Stop` der Engine,
  Transport-Stopp und -Start (die Eins kann nicht vor sich selbst einblenden), das Halten beim
  Einzähler.
* **„Stop all"** ist ein getimtes `StopPlay` auf `Scheduler::soon` — es blendet also davor aus und
  ist **ab genau dem Sample still**, das der Runner meldet (Test
  `stop_all_fades_out_and_is_silent_on_its_sample`, und alle Tests in `runner/stop_tests.rs`
  unverändert grün).

**Der Wartesaal bleibt gültig.** Ein Befehl, der früher wirkt, hat den Wartesaal früher verlassen.
Ein später geschickter Ablass (`DropWaiting` von „Stop all", `ClearAll`, Transport-Stopp) hätte ihn
dort noch gefunden — deshalb trägt jeder Befehl seine Ankunftsnummer, und ein vorgezogener Befehl,
den ein später geschickter Ablass mit erfasst, wird verworfen (`EngineCore::drained_later`). Ohne
das könnte ein Einsatz, der 5 ms nach dem Stopp-Sample geplant war, „Stop all" überleben.

**MIDI-Loops** sind unverändert: keine Samples, keine Blende, keine verschobenen Zeiten.

### Mithören

`SetMonitor` hat keinen Zeitstempel (Abschnitt 10). Statt hart zu schalten, blendet das Mithören
jetzt über 10 ms, gerechnet in Eingangs-Frames ab dem Frame, mit dem der Befehl wirkt
(`Track::monitor_level`). Dieselbe Kurve gewichtet das Live-Signal im Stem. Im Mitschnitt waren das
die Klicks bei 12,77 s und 109,80 s (Mikrofon an).

### Die Stems hören, was der Saal hört

Der trockene Stem las den Loop bisher direkt aus den Ebenen, an der Blende vorbei. Jetzt liest er
ihn über dieselbe Funktion wie der Mix — **Stems enthalten den geblendeten Loop**, so wie er zu
hören war. Das Live-Signal im Stem (Abschnitt 16) bekommt dieselben Kanten: Es blendet mit dem
Mithören, am Ende eines Takes, den der Loop übernimmt, blendet es über die letzten 10 ms aus —
genau dort, wo der Loop seinen Vorlauf einblendet (die Nahtblende, im Stem) —, und ein Take, der
aus der Stille beginnt, bekommt seinen Vorlauf vor dem ersten Frame eingeblendet. Ob das
Live-Signal schon über Main im Stem steht, wird jetzt für das Eingangs-Frame selbst entschieden
(`capture::live_reach`), nicht für den Moment, in dem es `R` Frames später einsortiert wird — vorher
landeten die letzten Frames eines Takes, nach dem das Mithören abgeschaltet wurde, doppelt im Stem.

### Was nicht geblendet wird

* Inhalt, der verschwindet: Löschen, Undo, Entfernen einer Ebene und ein neuer Take auf einem
  spielenden Track (`StartRecord` räumt die alten Ebenen sofort ab). Was nicht mehr da ist, kann
  nicht ausblenden. In keiner Partitur des Besitzers kommt das unter Klang vor.
* Ebene stumm/laut, Ebenen-Lautstärke, globales Mithör-Stumm (`SetMonitorMute`) — wie bisher
  Schritte, bewusst ungeglättet (Abschnitt 6).
* Loops unter 80 ms (8 Blendlängen): keine Blende. Baut kein Musiker, nur Tests, die Loops Sample
  für Sample lesen.

### Mit Plugins in der Kette

Plugins dürfen überall zwischen den eingebauten Geräten sitzen (`InsertPosition`, Abschnitt 14);
die Kette wird dann in Segmenten gerechnet (`render_split_chain`). Für die Blenden ändert das
nichts: Der Loop wird um die Summe aller gehörten Latenzen vorausgelesen, und jede Blende —
Naht, Einsatz, Stopp, Neuanfang — liegt auf genau dieser Leseachse; beide Wege lesen den Loop
über `Track::play_frame` (`render_sources`), und die Stems werden in beiden nach dem Fader
abgegriffen. Das Umstellen eines Plugins im Lauf taucht den Track kurz ab (256 Frames aus, 256
ein); die Kurve ist jetzt dieselbe halbe Kosinus-Periode wie alle Blenden hier — als gerade
Rampe hatte sie an ihren Ecken auf tieffrequentem Material geknackt. Bewiesen mit
`clean_tests::plugins_with_latency_and_a_plugin_move_do_not_click` (Verzögerungs-Plugins mit
300 Frames Latenz vor dem ersten Gerät, 211 hinter dem letzten, eines im Lauf umgestellt; vorher
3 Stellen über der Schranke, jetzt 0). Die Plugin-Zeit der Lastanzeige zählt Plugins an jeder
Stelle.

### Kosten

Echtzeit-sicher: nichts alloziert (Ring und Vorlauf-Speicher je Track im Steuer-Thread, 16 Slots à
1024 Stereo-Frames), keine Sperre, keine Latenz. Gemessen mit `bench_loop_playback`
(`cargo test --release -p looper-engine --lib bench_loop_playback -- --ignored --nocapture`, acht
Tracks mit je vier Ebenen auf einem Ein-Takt-Loop, ganzer Engine-Block samt Simulation): **137 →
183 ns je Frame**, also 0,66 % → 0,88 % des Callback-Budgets; in den Blöcken, in denen alle 32
Ebenen gleichzeitig vor ihrer Naht blenden, rund 250 ns je Frame (1,2 %). Gemessen auf dem
Entwicklungsrechner, während die App lief — die Zahlen schwanken um ±15 %.

### Was der Knackser in `theme_b` war

Im selben Mitschnitt sprang `03 theme_b (dry).wav` bei 10,3454 s um 0,06 in einem Sample, und der
Loop wiederholte den Sprung bei jedem Durchlauf. Das ist **kein** Fehler beim Schreiben der Ebene:
Das gemithörte Live-Signal derselben Spur (`03 theme_b.wav`, `Mix.wav`) springt identisch,
**genau `R` = 596 Frames später** — dort, wo das Mithören dasselbe Eingangs-Frame ungeschoben
ausgibt — und dieses Sample liegt exakt auf einer 128er-Blockgrenze der Engine. Der Eingang
selbst hatte dort einen Schnitt: ein verlorener Treiber-Zyklus (CoreAudio-Overload oder ein
Überlauf des Eingangs-FIFO); `session.json` zählt für den Lauf zwei solche Aussetzer. Der Vergleich
„Stem bei 10,3454 s glatt" sah auf das falsche Sample, weil der Stem das Live-Signal ohne
Kompensation trägt. Bewiesen wird die Engine-Seite durch
`every_take_of_the_owners_score_is_the_input_shifted_by_the_latency`: dieselbe Partitur, ein
Eingang, der seine Frame-Nummern trägt, und alle neun Takes sind Sample für Sample der Eingang,
um `R` verschoben — kein Frame fehlt, keiner doppelt. Nebenbei werden neue Ebenen-Puffer jetzt im
Steuer-Thread einmal je Speicherseite beschrieben (`command::resident_zeros`): `vec![0.0; n]` liefert
Seiten, die erst beim ersten Schreiben eingeblendet werden — im Audio-Thread, alle 16 KB eines
Takes. Ob das den Aussetzer verursacht hat, lässt sich aus der Aufnahme nicht sagen; es ist eine
Ursache weniger.

### Tests

| Test | beweist |
|---|---|
| `clean_tests::every_take_of_the_owners_score_is_the_input_shifted_by_the_latency` | alle Takes der Partitur sample-genau = Eingang um `R` verschoben (auch kurze Overdubs) |
| `clean_tests::theme_b_is_continuous_across_the_spot_of_the_owners_click` | die eine Stelle aus dem Mitschnitt |
| `clean_tests::the_fades_touch_only_the_frames_before_each_edge` | Vorlauf = Eingang vor dem Take; Naht, Einsatz, Neuanfang, Stopp Sample für Sample gegen die Formel; erster Frame unberührt |
| `clean_tests::the_owners_score_plays_without_a_single_click` | ganze Partitur mit 41/73-Hz-Material: Main hat keine Stufe und keine Krümmung über der Schranke, die aus Steigung und Krümmung des Eingangs plus einer Blende folgt (0 statt 78 Stellen) |
| `clean_tests::stacked_and_cut_short_layers_play_without_a_click` | `stack` (Vorlauf jedes Durchlaufs = Ende des vorigen), Wiederkehr nach Pause neu verankert, gekürzter Overdub (vorher 26 Stellen über der Schranke, jetzt 0) |
| `clean_tests::stop_all_fades_out_and_is_silent_on_its_sample` | „Stop all" in drei Sektionen: blendet, ab dem Stopp-Sample digital still |
| `clean_tests::the_stems_of_the_owners_score_have_no_click_either` | jeder Stem und jeder trockene Stem der Partitur ebenso |

Geänderte bestehende Tests, jeweils weil sie den Loop Sample für Sample mit dem Ebeneninhalt
verglichen und die letzten 10 ms vor der Naht jetzt geblendet sind: in `tests.rs` die Summen-,
Stumm-, Entfernen-, Stereo-, Panorama-, Bypass- und Mithör-Tests (sie vergleichen jetzt mit
`Track::steady_layer`, dem Inhalt mit Nahtblende; außerhalb der Blende unverändert exakt),
`loop_seam_loses_and_repeats_nothing` und `start_play_acts_exactly_at_its_sample` (rechnen die
Blende ausdrücklich nach), `clear_stop_and_monitor_do_what_they_say` (Mithören blendet aus),
`output_tests` (dasselbe), `transport_tests::stop_keeps_the_loops_and_start_plays_them_from_the_top`
(Transport-Stopp und -Start blenden ab ihrem Sample), `restart_tests::the_chorus_sounds_with_the_first_bar_of_both_loops`
(der Takt davor ist still bis auf die Einblendung) und die goldenen Hashes in
`runner/golden_tests.rs`: gegen den Stand davor Sample für Sample verglichen, unterscheiden sich
nur Läufe von 480–484 Frames vor Nähten, Einsätzen, Stopps, Neuanfängen und nach
Mithör-Schaltern, 0,2 % aller Frames.

**Nichts wird über Sitzungen hinweg gespeichert**: Ebenen leben nur im Speicher der laufenden
Engine; ein Loop von vor dieser Änderung kann also nicht auftauchen.

## 13. Arbeitsweise

- Henry bedient Hardware selbst; Audio-Tests laufen nur mit seiner Zustimmung.
- **Nie gleichzeitig an derselben Sache arbeiten.** Das hat schon Schaden angerichtet.
- **Bei Audio-Werten nicht raten, messen.** Besonders bei Latenz.
- Code und Kommentare Englisch, Nutzerausgaben Deutsch, Commits Deutsch.
- Offline beweisbare Eigenschaften werden offline bewiesen, bevor sie am Instrument geprüft
  werden. Zwei Millisekunden Versatz hört man beim Spielen nicht, aber sie ruinieren jeden
  Overdub.
