# Plan: Standalone-Looper in Rust (ohne Ableton)

Arbeitsdokument für den umsetzenden Agenten. Stand 2026-09-06. Sprache mit Henry: Deutsch, Du-Form. Code und Kommentare Englisch, Nutzertexte Deutsch.

## 1. Warum dieser Schnitt

Der bisherige Prototyp steuert Ableton Live über AbletonOSC. Das funktioniert, stößt aber wiederholt an strukturelle Grenzen, die nicht in unserem Code liegen:

- **Kein verlässlicher Zustand.** Clip-Status kommt verzögert (rund 100 ms Antwortzeit) oder gar nicht. Das Looper-Device meldet überhaupt nichts zurück, sondern echot nur gesetzte Werte.
- **Fremde Looping-Semantik.** Eine Spur spielt genau einen Clip. Overdub-Layer brauchen deshalb eigene Spuren, Mithören braucht eine weitere Spur, und Spuren anzulegen verschiebt alle Indizes.
- **Timing nur grob steuerbar.** Wir können Aktionen nicht auf einen Takt terminieren, sondern nur kurz vorher senden und auf Abletons Launch-Quantisierung hoffen.
- **Kein Löschen, kein sauberer Reset** beim Looper-Device; beim Clip-Weg jedes Mal Strukturänderungen am Set.

Eine eigene Engine kennt ihren Zustand exakt, terminiert Aktionen auf Samples statt auf Hoffnung, und kann beliebig viele Layer stapeln. Der Preis: Audio-I/O, Latenzkompensation, Klick, Effekte und Persistenz müssen selbst gebaut werden.

**Nicht verhandelbar für den Erfolg:** Aufnahme und Wiedergabe müssen sample-genau ausgerichtet sein. Ein Looper, dessen Layer um 10 ms driften, ist unbrauchbar, egal wie gut der Rest ist.

## 2. Was übernommen wird

Aus dem bestehenden Repo bleibt konzeptionell erhalten:

- **Partitur-Modell**: deklarative Sektionen, jede beschreibt den Soll-Zustand aller Tracks. Felder `bpm`, `time_signature`, `bars`, `autorelease`, `quantize`, Track-States `record | overdub | play | stop | hear_through`, `repeat`, MIDI-Mapping. Siehe `docs/contracts-v0.md` und `looper/score/schema.json`.
- **UI-Konzept**: Editor oben, kompilierte Block-Vorschau unten, MIDI-Monitor rechts, große Positionsanzeige. Die React-Komponenten in `ui/` sind wiederverwendbar, die Event-Formate ebenfalls.
- **Runner-Semantik**: Einzähler, Pending-Wechsel, Autorelease, Quantisierung auf Takt oder Loop-Ende.
- **Erkenntnisse**: alles in `docs/handover-tag1.md` und diesem Verzeichnis.

Was wegfällt: `looper/engine/ableton_osc.py`, `looper/session.py` (Gruppen-Pool), der gesamte OSC-Pfad, `spike/`.

## 3. Architektur

Ein einziger Rust-Prozess, der alles hält, plus das React-Frontend im Browser.

```
 Browser (React/Vite)                      Rust-Prozess
 ┌────────────────────────┐   HTTP/WS   ┌──────────────────────────────────────┐
 │ Editor │ Blockvorschau │ ◄─────────► │ axum  ──►  Runner  ──►  Scheduler    │
 │        │ MIDI-Monitor  │             │            (Steuer-Thread)           │
 └────────────────────────┘             │                 │ lock-freie Queue   │
                                        │                 ▼                    │
                                        │        Audio-Callback (Echtzeit)     │
                                        │        Mixer · Loops · Klick · FX    │
                                        └───────────┬──────────────────────────┘
                                                    │ cpal
                                              Audio-Interface
```

**Drei Threads, klar getrennt:**

1. **Audio-Thread** (vom Treiber getrieben, Echtzeit). Darf niemals allozieren, sperren, loggen oder Dateien anfassen. Er liest Kommandos aus einer lock-freien Queue, schreibt Eingangs-Samples in Loop-Puffer, mischt Layer, erzeugt den Klick, wendet Effekte an und zählt die Sample-Position mit.
2. **Steuer-Thread**: Runner, Partitur, Zustandsautomat. Schickt terminierte Kommandos in die Queue, liest Statusmeldungen aus einer zweiten Queue zurück.
3. **Web-Thread**: axum mit HTTP und WebSocket, versorgt das UI.

**Der zentrale Kniff: musikalische Zeit statt Wanduhr.** Der Audio-Thread führt eine fortlaufende Sample-Position und rechnet daraus Takt und Schlag. Kommandos tragen einen Zeitstempel in dieser Zeitachse: „Track 2, Aufnahme starten bei Takt 9, Schlag 1". Der Steuer-Thread schickt sie beliebig früh los, der Audio-Thread führt sie sample-genau aus. Damit spielt Latenz zwischen den Threads keine Rolle mehr. Genau diese Fähigkeit fehlt bei Ableton.

**Datenmodell im Audio-Thread:**

- `Track`: eine logische Spur, hält N `Layer`, einen Eingangskanal, Monitoring-Zustand und eine Effektkette.
- `Layer`: ein vorab allozierter `Vec<f32>` fester Maximallänge, plus Länge in Samples und Schreib-/Lesezeiger. Overdub heißt: neuer Layer, alle Layer werden beim Abspielen summiert. Damit ist Stapeln unbegrenzt bis zum Speicher.
- Speicherbedarf: 5 Minuten Stereo bei 48 kHz sind rund 115 MB pro Layer. Deshalb Layer in Loop-Länge allozieren, nicht pauschal, und Allokation im Steuer-Thread erledigen, dann den fertigen Puffer per Queue übergeben.

## 4. Latenzkompensation, der kritische Teil

Beim Aufnehmen kommt das Signal um die Eingangslatenz verzögert an, die Wiedergabe geht um die Ausgangslatenz verzögert raus. Wird das nicht kompensiert, sitzt jeder aufgenommene Layer um den Roundtrip zu spät und der Loop eiert.

Vorgehen:

1. **Messen statt schätzen.** Loopback-Test: Ausgang 1 mit Eingang 1 verkabeln, einen Impuls ausgeben, die Sample-Position des zurückkommenden Impulses messen. Differenz ist die Roundtrip-Latenz in Samples. Das ist die einzige verlässliche Methode.
2. Wert als Kalibrierung speichern, im UI anzeigen und manuell nachjustierbar machen.
3. Beim Schreiben in den Loop-Puffer den Schreibzeiger um diesen Betrag nach vorn verschieben.
4. Nach jeder Änderung von Gerät, Samplerate oder Puffergröße neu messen.

Ein- und Ausgang laufen auf derselben Interface-Clock, deshalb gibt es keinen Drift über die Zeit. Das ist der große Vorteil gegenüber getrennten Geräten.

## 5. Technologie

Der Agent verifiziert Verfügbarkeit und aktuelle Versionen, bevor er sich festlegt.

| Zweck | Kandidat | Anmerkung |
|---|---|---|
| Audio-I/O | `cpal` | WASAPI und ASIO. ASIO braucht ein Feature-Flag und das Steinberg-SDK lokal. |
| Lock-freie Queues | `rtrb` oder `crossbeam` | Zwischen Steuer- und Audio-Thread, in beide Richtungen. |
| MIDI-Eingang | `midir` | Direkt vom Gerät, nicht über Umwege. |
| Web-Server | `axum` | HTTP plus WebSocket, `tokio` als Runtime. |
| YAML und Schema | `serde_yaml` bzw. `serde_yml`, `jsonschema` | Zeilennummern für Fehlermeldungen sicherstellen. |
| Tippfehler-Vorschläge | `strsim` | Für „meintest du 'bass'?" |
| WAV-Export | `hound` | Stems und Sicherung. |
| DSP-Bausteine | `fundsp` oder eigene Filter | Siehe Effekte. |

**Audio-Backend:** ASIO liefert am Scarlett die niedrigste Latenz, kostet aber SDK-Einrichtung. WASAPI im Exclusive-Modus ist ohne Zusatz-SDK nutzbar und für einen ersten Nachweis oft ausreichend. Phase 0 misst beide und entscheidet mit Zahlen.

## 6. Phasen

Jede Phase endet mit einem Zustand, den Henry hören oder bedienen kann. Keine Phase beginnt, bevor die vorige abgenommen ist.

### Phase 0: Audio-Nachweis (blockierend für alles Weitere)

Kleines Rust-Programm, kein UI.

- Geräte auflisten, Scarlett 2i2 öffnen, Samplerate und Puffergröße wählen.
- Eingang direkt auf Ausgang durchreichen, Henry hört sich selbst.
- Metronom-Klick erzeugen, tempo- und taktartgesteuert.
- Roundtrip-Latenz per Loopback messen und ausgeben.
- Dauerlauf zehn Minuten, Aussetzer zählen.

**Definition of Done:** Durchhören ohne hörbare Verzögerung, Klick stabil, Roundtrip gemessen und dokumentiert, keine Aussetzer im Dauerlauf. Zahlen für ASIO und WASAPI im Vergleich.

**Wenn diese Phase nicht überzeugt, wird das Projekt hier gestoppt und neu bewertet.** Das ist der Sinn der Phase.

### Phase 1: Loop-Kern, ein Track

- Sample-Position, Takt- und Schlagzählung, variable Taktart.
- Kommando-Queue mit musikalischen Zeitstempeln.
- Aufnahme starten und beenden auf Taktgrenze, Loop-Wiedergabe, Latenzkompensation angewandt.
- Bedienung über die Tastatur, Ausgabe im Terminal.

**DoD:** Henry spielt acht Takte Gitarre ein, der Loop läuft nahtlos und bleibt über fünf Minuten synchron zum Klick. Nahtstelle nicht hörbar.

### Phase 2: Mehrere Tracks und unbegrenzte Layer

- Mehrere Tracks mit eigenen Eingangskanälen.
- Overdub legt einen weiteren Layer an, alle Layer klingen gleichzeitig.
- Layer einzeln stumm, entfernen, in der Lautstärke ändern.
- Mithören pro Track unabhängig vom Abspielen, also gleichzeitig Loop hören und live darüber spielen.
- Alles löschen und sauber neu beginnen.

**DoD:** Drei Voice-Layer plus Gitarre gleichzeitig, live darüber singen, einzelnen Layer entfernen, alles zurücksetzen. Genau das, woran der Ableton-Weg gescheitert ist.

### Phase 3: Partitur und Runner

- YAML-Compiler nach dem bestehenden Schema, mit Zeilennummern und Tippfehler-Vorschlägen in den Fehlermeldungen.
- Runner mit Einzähler, Autorelease, Pending-Wechsel, Quantisierung auf Takt oder Loop-Ende.
- Kommandos werden aus der Partitur in terminierte Engine-Kommandos übersetzt.

**DoD:** Henrys Partitur mit vier Sektionen läuft von Anfang bis Ende durch, ohne dass er etwas außer dem Release-Button anfasst.

### Phase 4: UI anschließen

- axum mit HTTP und WebSocket, Event-Formate wie in `docs/contracts-v0.md`.
- Bestehende React-Oberfläche anschließen, Anpassung an die neuen Engine-Zustände wie Layer pro Track und Pegel.
- Pegelanzeige pro Track, sichtbarer Einzähler, große Positionsanzeige.

**DoD:** Henry bedient alles aus dem Browser, wie bisher gewohnt.

### Phase 5: Bühnentauglichkeit

- MIDI-Eingang mit Mapping aus der Partitur, MPD218 als Pedal-Ersatz.
- Sitzung speichern und laden, Layer als WAV exportieren.
- Klick auf separaten Ausgang legen, Kopfhörer statt Saal.
- Sicherung: laufende Aufnahme regelmäßig auf Platte schreiben, damit ein Absturz nicht alles kostet.

### Phase 6: Effekte

Hier liegt der eigentliche Verlust gegenüber Ableton, deshalb bewusst spät und klein anfangen.

- Reihenfolge nach Nutzen für Piezo-Akustik: Hochpass, drei Bänder parametrischer EQ, Kompressor, dann Delay, zuletzt Hall.
- Erst als Kette pro Track, Parameter über das UI.
- Hall als Letztes, ein einfaches Modell nach Freeverb-Art reicht für den Anfang.
- VST-Hosting ist ausdrücklich kein Ziel dieses Plans.

## 7. Risiken

| Risiko | Wirkung | Umgang |
|---|---|---|
| Latenz oder Aussetzer unter Windows zu schlecht | Projekt trägt nicht | Phase 0 misst zuerst, mit Abbruchoption |
| Latenzkompensation falsch | Layer driften, unbrauchbar | Loopback-Messung, im UI justierbar, in Phase 1 hörbar geprüft |
| Allokation oder Sperre im Audio-Thread | Knackser unter Last | Puffer vorab im Steuer-Thread, nur lock-freie Queues, Debug-Assertion gegen Allokation |
| Speicher bei vielen langen Layern | Absturz | Layer in Loop-Länge, Obergrenze mit klarer Meldung |
| Effekte fehlen anfangs | Klang schlechter als gewohnt | Bewusst akzeptiert, Phase 6, trocken aufnehmen |
| Rust-Einarbeitung | Tempo | Phasen sind klein und einzeln abnehmbar |

## 8. Entscheidungen, die Henry treffen muss

1. **Effekte:** trocken starten und ab Phase 6 selbst bauen, oder vorerst ohne bleiben?
2. **Audio-Backend:** ASIO trotz SDK-Einrichtung, oder erst WASAPI und ASIO später?
3. **Samplerate und Puffergröße** als Zielwerte, etwa 48 kHz und 128 Samples.
4. **Sprachgrenze:** Dieser Plan setzt alles in Rust um, inklusive Compiler und Server. Alternative wäre, den bestehenden Python-Compiler und -Runner zu behalten und nur die Audio-Engine in Rust anzusprechen. Das spart Portierungsaufwand, bringt aber wieder eine Prozessgrenze mit Zustandssynchronisation. Empfehlung: alles in Rust, weil genau diese Grenze das Problem des bisherigen Ansatzes war.
5. **Mono oder Stereo** je Track. Gitarre und Stimme sind mono, das halbiert Speicher und Rechenzeit.
