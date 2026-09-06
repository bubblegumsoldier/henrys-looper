# Phase 0: Audio-Nachweis unter Windows

Messbericht. Stand 2026-09-06. Gemessen auf Henrys Maschine, nicht geschätzt.

**Frage dieser Phase:** Trägt Windows-Audio in Rust für einen Live-Looper mit sample-genauer
Latenzkompensation? Fällt die Antwort negativ aus, wird das Projekt hier gestoppt.

**Antwort in einem Satz:** Ja, mit ASIO. WASAPI scheidet aus, und zwar strukturell, nicht knapp.

---

## 1. Umgebung

| Posten | Wert |
|---|---|
| Rechner | Windows 11 Home 10.0.26100 |
| Interface | Focusrite Scarlett 2i2 (USB), Treiber Focusrite USB 4.65.5.658 |
| Rust | 1.98.1, Toolchain `stable-x86_64-pc-windows-msvc` |
| Audio-Bibliothek | `cpal` 0.18.2, `asio-sys` 0.4.0 |
| ASIO-SDK | Steinberg ASIO SDK 2.3.4 (2025-10-15), unter GPL3 |
| Build | `cargo build --release --features asio`, `opt-level=3`, LTO |
| Messprogramm | `engine/`, Subcommands `list`, `thru`, `click`, `latency`, `soak` |

Ableton Live war bei allen Messungen geschlossen. ASIO-Geräte sind exklusiv.

### Einrichtungsaufwand, der nötig war

Für Nachahmer und für die eigene Erinnerung:

1. **MSVC Build Tools mit C++-Workload.** Rust war installiert, aber `link.exe` fehlte, damit
   baute nichts. `winget install --id Microsoft.VisualStudio.2022.BuildTools -e --override
   "--quiet --wait --add Microsoft.VisualStudio.Workload.VCTools --includeRecommended"`.
   Ohne das `--override` installiert winget eine leere Hülle ohne Compiler.
2. **LLVM** für `bindgen`, das die C++-Header des ASIO-SDK übersetzt.
3. **ASIO-SDK** von `https://www.steinberg.net/asiosdk` — leitet ohne Registrierung direkt auf
   das ZIP. Entpackt nach `C:\SDKs\ASIOSDK`, `CPAL_ASIO_DIR` auf dieses Verzeichnis gesetzt.

Seit Oktober 2025 ist das ASIO-SDK zusätzlich unter GPL3 lizenziert. Dieses Repo steht unter
GPL3, die freie Variante greift also, und es entsteht kein Lizenzkonflikt.

---

## 2. Geräte und Fähigkeiten

Ausgabe von `looper-engine list`, gekürzt auf das Scarlett:

| | **ASIO** | **WASAPI** |
|---|---|---|
| Erscheint als | ein Duplex-Gerät | zwei getrennte Geräte |
| Puffergröße bei 48 kHz | **16 bis 1024 Frames** | **480, fest** |
| Sampleraten | 44,1 / 48 / 88,2 / 96 kHz | 48 kHz |
| Sampleformat | ausschließlich I32 | U8 bis F64 |

Drei Befunde daraus, die vor jeder Latenzmessung feststehen:

**WASAPI erlaubt keine Wahl der Puffergröße.** 480 Frames sind keine Untergrenze, sondern
Vorgabe — die 10-ms-Geräteperiode des Windows-Audiodienstes. Der Zielwert 128 wird schlicht
abgelehnt.

**cpal kann kein WASAPI-Exclusive.** Im Quellcode von cpal 0.18.2 ist der Share-Mode an allen
drei relevanten Stellen hart auf `AUDCLNT_SHAREMODE_SHARED` verdrahtet
(`src/host/wasapi/device.rs`). Ebenso fehlt `IAudioClient3`, also auch der Low-Latency-Shared-Modus
von Windows 10 und neuer. Der ursprünglich geplante Vergleich „WASAPI Exclusive gegen ASIO" ist
mit cpal deshalb nicht durchführbar. Er wäre auch folgenlos: Nutzt die Engine cpal, steht
Exclusive gar nicht zur Verfügung.

**ASIO liefert nur I32, kein Float.** Ein reiner f32-Pfad hätte genau das Backend blockiert, um
das es geht. Die Engine braucht Formatkonvertierung an der Gerätegrenze; im Messprogramm ist sie
mit vorab allozierten Puffern gelöst, ohne Allokation im Callback.

---

## 3. Durchhören

`looper-engine thru --host asio --buffer 128`

Der Treiber bestätigt tatsächlich 128 Frames für Ein- und Ausgang, ohne stille Korrektur nach
oben. Null Xruns, null Ringpuffer-Unter- oder -Überläufe. Eingangspegel sauber bei −45 dBFS.

Henrys Urteil beim Gitarrespielen: **kein spürbarer Versatz, kein Knacken, flüssig.**

Dabei war die gehörte Latenz sogar schlechter als das Erreichbare: Das Messprogramm koppelt
Ein- und Ausgang über einen Ringpuffer von 512 Samples, der bis zu 10,67 ms zusätzlich kostet.
In der Engine entfällt dieser Umweg unter ASIO, weil dort ein Treiber-Callback beide Richtungen
bedient. Das Urteil „unauffällig" gilt also mit Sicherheitsabstand.

---

## 4. Metronom

`looper-engine click --host asio --buffer 128 --bpm 100 --beats-per-bar 3`

Geprüft in 3/4 und in 7/8. Beide vom Gehör fehlerfrei: Die Eins sitzt, der Takt läuft
gleichmäßig, der Klick selbst knackt nicht.

Die Schlagposition wird aus dem fortlaufenden Sample-Zähler des Callbacks berechnet, nicht aus
der Wanduhr, und die Taktgrenze als `(beat_index * samples_per_beat).round()` bestimmt, damit
sich Rundungsfehler nicht aufaddieren. Ungerade Taktarten sind damit kein Sonderfall.

---

## 5. Roundtrip-Latenz

Aufbau: Klinkenkabel (TS, unsymmetrisch) von Line Out 1 zurück in Eingang 1, INST und
Direct Monitor aus, Gain auf mittlerer Stellung. Impuls von vier Samples auf 0,9 Amplitude,
Detektion über Schwellwert relativ zum Grundrauschen.

### Messreihe über fünf Puffergrößen

Jede Zeile ist eine eigene Messreihe mit 10 bis 20 Läufen.

| Puffer | Roundtrip | in ms | Standardabweichung |
|---:|---:|---:|---:|
| 32 | 443 Samples | **9,23 ms** | **0,000** |
| 64 | 603 Samples | **12,56 ms** | **0,000** |
| 128 | 827 Samples | **17,23 ms** | **0,000** |
| 256 | 1371 Samples | **28,56 ms** | **0,000** |
| 512 | 2363 Samples | **49,23 ms** | **0,000** |

Insgesamt 60 Läufe, verteilt über mehrere Programmstarts. Keine Fehldetektion, keine Xruns.

### Die Streuung ist der eigentliche Befund

**Die Standardabweichung ist bei jeder Puffergröße exakt null.** Nicht klein, sondern null —
sechzig Messungen, kein einziges Sample Abweichung.

Das entscheidet über die Machbarkeit des Projekts. Die Latenzkompensation zieht beim Aufnehmen
einen festen Sample-Versatz ab. Das funktioniert nur, wenn der Versatz jedes Mal derselbe ist.
Schwankte er, würden Overdub-Layer gegeneinander driften, und der Looper wäre unbrauchbar,
egal wie niedrig die absolute Latenz ausfiele. Das größte Risiko aus Abschnitt 7 des Plans ist
damit ausgeräumt.

### Der Absolutwert ist validiert, nicht geglaubt

Die Messmethode zählt Frames in beiden Callbacks. Beide Zähler starten beim jeweils ersten
Callback bei null, ein konstanter unbekannter Versatz war deshalb nicht auszuschließen. Zwei
unabhängige Prüfungen entkräften das:

**Linearität.** Eine Regression über alle fünf Messpunkte ergibt

> **Roundtrip ≈ 3,98 × Puffergröße + 331 Samples**

Alle fünf Punkte liegen innerhalb von ±20 Samples (±0,4 ms) auf dieser Geraden, über einen
Faktor 16 in der Puffergröße hinweg. Die Steigung von praktisch genau vier Puffern entspricht
Ein- und Ausgang bei jeweils doppelter Pufferung, wie ASIO es vorsieht. Der konstante Anteil
von 331 Samples (6,9 ms) deckt AD- und DA-Wandler sowie den USB-Transport ab. Wer einen
Startversatz misst statt Latenz, bekommt keine Gerade mit physikalisch sinnvollen Koeffizienten.

**Reproduzierbarkeit über Programmstarts.** Der Wert bei 128 Samples betrug in zwei getrennten
Programmläufen identisch 827,0 Samples. Ein zufälliger Versatz durch versetzten Stream-Start
müsste zwischen Starts variieren.

### Zeitstempel-Methode: unbrauchbar, und das ist ein Ergebnis

Als Gegenprobe war eine zweite Methode über `StreamInstant` vorgesehen. Sie lieferte über alle
Messreihen hinweg Unsinn: teils null, teils zufällige Kleinwerte, teils exakt 2³².

Ursache: **cpal speist `StreamInstant` unter ASIO aus `timeGetTime()`**, einem
32-Bit-Millisekundenzähler von Windows. Daher der Überlauf. Und selbst ohne Überlauf wäre die
Auflösung untauglich — eine Millisekunde sind 48 Samples.

Der ursprüngliche Code verschlimmerte das durch `as_nanos() as u64` (Truncation von `u128`) und
`saturating_sub`, das eine negative Differenz stillschweigend als 0,00 ms auswies. Beides ist
korrigiert: Die Differenz läuft jetzt über `checked_duration_since`, und ungültige Läufe werden
als solche ausgewiesen statt als Messwert getarnt.

**Konsequenz für Phase 1:** Die musikalische Zeitachse der Engine muss aus selbst gezählten
Frames im Audio-Callback kommen. cpal-Zeitstempel sind unter ASIO dafür ungeeignet. Der Plan
sieht das ohnehin so vor; diese Messung belegt, dass es keine Stilfrage ist.

### WASAPI zum Vergleich

`latency --host wasapi --device Focusrite --buffer 480`, 10 Läufe, gleicher Aufbau:

| Methode | Wert | Streuung |
|---|---:|---:|
| Frame-Zähler | 6776 Samples, **141,17 ms** | 0,000 |
| Zeitstempel | 5321 Samples, **110,86 ms** | 22,2 Samples (0,46 ms) |

**Gegenüber 17,23 ms bei ASIO ist das Faktor sechs bis acht.** Bei 100 BPM ist eine
Sechzehntelnote 150 ms lang — man hörte also fast eine Sechzehntel zu spät, was man spielt.
Für einen Looper unbrauchbar, und zwar nicht knapp.

Zwei Nebenbefunde aus dieser Messung sind wichtiger als die Zahl selbst:

**Die Zeitstempel-Methode funktioniert hier einwandfrei** (10 von 10 Läufen gültig, plausible
Werte). Der Ausfall unter ASIO ist also kein allgemeines cpal-Problem, sondern spezifisch für
das ASIO-Backend und seine `timeGetTime()`-Quelle.

**Die Differenz von 1455 Samples zwischen beiden Methoden ist der Startversatz getrennter
Streams** — unter WASAPI sind Ein- und Ausgang zwei unabhängige Geräte. Genau dieser Versatz
war der offene Zweifel an der ASIO-Messung. Dort tritt er nicht auf, weil beide Richtungen am
selben Duplex-Treiber hängen. Damit ist unabhängig bestätigt: Die 827 Samples sind Latenz,
kein Artefakt.

---

## 6. Dauerlauf

`soak` schaltet Durchhören und Klick gleichzeitig, unter normaler Rechnernutzung nebenher.

| Puffer | Dauer | Callbacks | Xruns | Ring-Under/Overruns | Callback max | Auslastung |
|---:|---:|---:|---:|---:|---:|---:|
| 128 | 10 min | 450.014 | **0** | 0 / 0 | 0,159 ms | 6 % |
| 64 | 5 min | 450.194 | **0** | 0 / 0 | 0,310 ms | 23 % |

Bei 128 Samples liefen 750,0 Callbacks pro Sekunde, exakt der Sollwert. **Die Spitze von
0,159 ms blieb über alle zehn Minuten unverändert** — nicht ein einziger Ausreißer nach oben
über 450.000 Callbacks hinweg. Genau dort würden sich Windows-Hintergrunddienste, USB-Energie-
verwaltung oder ein Virenscanner zeigen. Da ist nichts.

Bei 64 Samples sprang die Spitze in Minute 3 auf 0,310 ms. Kein Xrun, aber die Reserve
schrumpft spürbar: 23 Prozent des Budgets statt 6 — und das mit einem Callback, der kaum mehr
tut als Samples durchreichen. Die spätere Engine wird dort Layer summieren, mischen und
Effekte rechnen.

**Statistische Ehrlichkeit:** Null Aussetzer in zehn Minuten heißt, dass die Rate mit 95 %
Konfidenz unter etwa 18 pro Stunde liegt. Das ist kein Beweis für Bühnentauglichkeit — dafür
bräuchte es Messungen über Stunden und unter echter Last. Es ist ein starkes Indiz, mehr nicht.

---

## 7. Bewertung und Empfehlung

### Definition of Done

| Kriterium | Ergebnis |
|---|---|
| Durchhören ohne hörbare Verzögerung | **erfüllt** — Urteil am Instrument, mit Sicherheitsabstand |
| Klick stabil, auch ungerade Taktarten | **erfüllt** — 3/4 und 7/8 fehlerfrei |
| Roundtrip gemessen und dokumentiert | **erfüllt** — 17,23 ms bei 128 Samples, über fünf Puffergrößen validiert |
| Keine Aussetzer im Dauerlauf | **erfüllt** — 0 Xruns in 10 Minuten |
| Vergleichszahlen WASAPI und ASIO | **erfüllt, mit Abweichung** — WASAPI nur im Shared-Modus messbar, siehe unten |

Eine Abweichung vom ursprünglichen Auftrag: Der Vergleich sollte WASAPI im **Exclusive**-Modus
umfassen. Das ist mit cpal nicht möglich (Abschnitt 2) und wäre folgenlos, weil die Engine auf
cpal aufbaut und Exclusive dort schlicht nicht zur Verfügung steht. Gemessen wurde daher
WASAPI Shared. Angesichts von Faktor sechs bis acht Rückstand und einer fest vorgegebenen
Puffergröße von 480 Frames würde auch ein besserer WASAPI-Wert die Backend-Wahl nicht drehen.

### Empfehlung

**Phase 1 starten.** Die Messungen tragen, und zwar mit Reserve.

Der entscheidende Wert ist nicht die absolute Latenz, sondern **die Standardabweichung von
null über sechzig Messungen**. Der Plan nennt sample-genaue Ausrichtung als nicht verhandelbar
für den Erfolg. Genau die ist damit erreichbar: Ein Versatz, der sich auf das Sample genau
reproduziert, lässt sich exakt herausrechnen.

Vier Festlegungen ergeben sich unmittelbar aus den Messungen:

1. **ASIO als Backend, WASAPI nicht als Rückfallebene.** Der Abstand ist zu groß, und ohne
   Exclusive-Modus in cpal gibt es keinen Hebel. Wer kein ASIO-Gerät hat, kann die Software
   sinnvoll nicht betreiben.
2. **128 Samples als Zielwert.** 64 bleibt Reserve und wird in Phase 1 unter echter Last neu
   bewertet, nicht heute schon verbraucht.
3. **Zeitachse aus selbst gezählten Frames.** cpal-Zeitstempel sind unter ASIO unbrauchbar.
   Das war ohnehin geplant und ist jetzt belegt.
4. **Latenzkompensation startet bei 827 Samples** für 128er Puffer an diesem Gerät. Der Wert
   muss nach jeder Änderung von Gerät, Samplerate oder Puffergröße neu gemessen werden, und
   im UI justierbar bleiben.

### Was diese Phase nicht beantwortet

- **Last.** Alle Zahlen stammen aus einem nahezu leeren Callback. Mixer, Layer-Summierung und
  Effekte kommen erst noch. Die 6 % Auslastung bei 128 Samples geben viel Spielraum, aber
  geprüft ist er nicht.
- **Dauer.** Zehn Minuten sind kein Auftritt.
- **Mehrkanal.** Gemessen wurde mono auf Eingang 1. cpal liest die Kanalzahl dynamisch vom
  Treiber, größere Interfaces sind also nicht ausgeschlossen — nur ungetestet.
- **Andere Hardware.** Alle Zahlen gelten für ein Scarlett 2i2 an dieser Maschine.

### Portabilität

cpal bringt CoreAudio, ALSA, JACK und PipeWire gleichberechtigt neben ASIO mit. Eine spätere
Mac-Portierung wäre kein Neubau: Host-Auswahl und Sampleformat ändern sich, der Rest der
Engine ist plattformneutrales Rust. Voraussetzung ist, dass nichts ASIO-Spezifisches über die
Geräteschicht hinaus in die Engine sickert. Der Zeitstempel-Befund ist genau so ein Fall — die
Entscheidung für selbst gezählte Frames macht uns davon unabhängig.

