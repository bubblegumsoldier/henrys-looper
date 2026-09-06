# Übergabedokument: Looper-Software — Spike → Prototyp (Tag 1)

Für: Claude Code (lokal, Windows-Maschine von Henry). Ziel heute: Vom AbletonOSC-Skript-Test zu einem ersten lauffähigen Prototypen mit echtem UI. Sprache mit Henry: Deutsch (Du-Form).

## 0. Orchestrierungs-Regeln

* Der Main Agent bleibt kompakt und schreibt niemals selbst Code. Alles, was Code ist, geht an Subagents. Der Main Agent plant, delegiert, prüft Ergebnisse und führt Henry durch die manuellen Schritte.
* Pro Milestone: einen Subagent mit klar umrissenem Auftrag + Definition of Done. Ergebnisse zusammenfassen lassen, nicht den ganzen Code in den Main-Kontext ziehen.
* Manuelle Schritte (Ableton anklicken, Geräte anschließen, Preferences) macht Henry selbst — der Main Agent gibt präzise Schritt-für-Schritt-Anweisungen und wartet auf Bestätigung.
* Nach jedem Milestone: Verifikations-Checkpoint mit Henry, erst dann weiter.
* Bei Architektur-Abweichungen vom Plan: erst Henry fragen.

## 1. Vision

Henry (CTO, schreibt selbst viel Code, ambitionierter Musiker) baut eine Looping-Software für vorgeplante Live-Arrangements: Statt bei jedem Loop-Wechsel zum Pedal zu greifen, schreibt er vorher eine "Partitur" (Sektionen mit Track-Zuständen), und die Software steppt live durch — mit minimalem manuellem Input (ein Release-Button, später günstige MIDI-Pedale). Ziel: Mix aus Automatisierung und manuellen kreativen Eingriffen, ohne teure Looper-Station.

UI-Konzept (gesetzt):
1. Oben: Code-/YAML-Editor für die Partitur.
2. Unten: Kompilierte Block-Vorschau der Sektionen, read-only, mit Live-Anzeige der aktuellen Position ("Sektion 3, Takt 2/4").
3. Rechts: MIDI-Monitor — zeigt live IDs/Daten jedes MIDI-Inputs für leichteres Mapping.

## 2. Architektur-Entscheidung (gesetzt)

Variante C: "Ableton loopt, unsere App dirigiert."

* Ableton Live 11 Suite ist die Audio-Engine: Audio-I/O, Klick, Tempo, Takt-Quantisierung, Monitoring, VSTs, Latenz-Kompensation, Stems (= Clips in der Session). Kein virtuelles Audiogerät, kein Kernel-Treiber, kein VST-Bridge-Bau.
* Unsere App ist Dirigent + Partitur-Editor: liest YAML-Partitur, übersetzt in OSC-Kommandos an Ableton ("arm Track 2", "Clip-Record Slot 1", "nach 4 Takten Loop-Play", "Szene launchen"), zeigt das UI.
* Brücke: AbletonOSC (https://github.com/ideoforms/AbletonOSC, Remote Script, Live 11+). Lauscht UDP 11000, antwortet auf 11001.
* MIDI-Steuerung (Buttons/Pedale) liest unsere App direkt vom Gerät — nicht durch Ableton geroutet. Windows: MIDI-Devices oft exklusiv; Gerät nicht gleichzeitig in Ableton aktiv.
* Lock-in-Begrenzung: Engine hinter Interface abstrahieren (`Engine.record(track, bars)`, `Engine.play(track)`, `Engine.stop_all()`), damit später eine Standalone-Audio-Engine (Rust/CPAL) das OSC-Backend ersetzen kann, ohne YAML-Schema und UI anzufassen.

Akzeptierte Trade-offs: Bindung an Abletons Looping-Semantik; Smart-Start per Audio-Threshold geht über OSC nicht (Fallback: quantisierter Record-Start); Gruppentrack-Konzept muss auf Session-Clips/Spuren abgebildet werden.

## 3. Fachliche Entscheidungen (gesetzt)

1. Deklarative Sektionen: Jede Sektion beschreibt den kompletten Soll-Zustand aller Tracks (nicht Deltas).
2. Tempo/Klick: BPM in der Partitur; immer mit Klick (Abletons Metronom).
3. Quantisierung: Release-Button = "diese Sektion zu Ende, dann weiter" — Wechsel immer an Sektions-/Taktgrenzen, mit sichtbarem Pending-State im UI ("Wechsel armiert").
4. Partitur-Format: YAML, keine DSL. Keine Variablen, keine Laufzeitlogik. Wiederverwendung über YAML-Anchors/Aliases oder `repeat: <section-id>`. Pipeline: YAML → Validierung gegen JSON Schema → Auflösung aller Referenzen (Tracks, MIDI-IDs, Sektionen) → statisches JSON als "Partitur" für die Engine. Gute Fehlermeldungen sind Kern-Feature ("Sektion 'bridge' referenziert Track 'bss' — meintest du 'bass'?").
5. Track-Typen: Gruppentrack (jede Aufnahme = neuer Layer/Clip in der Gruppe) vs. Einzeltrack (Overdub). In Ableton: Gruppe = Ableton-Gruppenspur, neuer Layer = neuer Clip in nächster freier Slot-Zeile (oder dynamisch neue Spur — prüfen, was per OSC robuster ist).
6. Sektions-Attribute (mindestens): `bars` (auch ungerade), `autorelease: true/false`, `quantize: bar|loop`, pro Track `state: record|overdub|play|stop|hear_through` (hear_through = Monitoring ohne Aufnahme).
7. Preamble/Mapping: Am Anfang der Partitur Mapping von MIDI-IDs auf Aktionen (`next_section`, `stop_all`, `play`, `goto: <section>`, Szenario-Aktivierung) und von Input-Namen auf Ableton-Tracks.
8. Stems: Clips in der Ableton-Session — kein eigenes Export-Feature in v1.
9. UI: Code ist Single Source of Truth; Block-Vorschau strikt read-only.

Explizit später: Smart-Start per Threshold, Szenarien/Sprünge zur Laufzeit, eigene Audio-Engine, VST-Hosting, Track-als-Input-von-Track.

## 4. Tech-Stack

* Engine/Backend: Python 3.11+ — `python-osc`, `mido` + `python-rtmidi`, `pydantic` oder `jsonschema`, `PyYAML`, `FastAPI` + WebSocket.
* UI: React + Vite als lokale Web-App. Editor: CodeMirror 6 mit YAML-Mode. WebSocket zum Backend für Live-State.

## 5. Milestones

### M0 — Umgebung (Henry manuell)
1. AbletonOSC nach `C:\Users\Admin\Documents\Ableton\User Library\Remote Scripts\AbletonOSC\`.
2. Live neu starten → Preferences → Link/Tempo/MIDI → Control Surface: AbletonOSC. Erwartet: "AbletonOSC: Listening for OSC on port 11000".
3. Test-Set: 2 Audio-Spuren ("gitarre", "voice"), Interface-Inputs zugewiesen, Monitoring sinnvoll, Metronom an.
4. Python-venv: `.venv` (Python 3.11) mit python-osc mido python-rtmidi fastapi uvicorn pyyaml jsonschema websockets. ✅ erledigt (2026-09-05)

### M1 — Spike-Skript (Subagent)
Ein Python-Skript, das nacheinander:
1. Tempo via OSC ausliest und printet (Reply auf Port 11001 empfangen!).
2. Track 1 armed.
3. Clip-Record in Slot (Track 1, Scene 1) startet.
4. Taktposition/Beat-Events abonniert und nach genau 4 Takten den Clip auf Loop-Play schaltet.
5. Auf Tastendruck (Terminal-Enter) einen zweiten Record auf Track 2 startet.

Checkpoint: Henry spielt Gitarre ein, hört nach 4 Takten den Loop, nimmt Voice als zweite Ebene auf. Fühlt es sich schlecht an: STOPP, Befund dokumentieren.

### M2 — Engine-Abstraktion + YAML-Transpiler (Subagent)
1. `Engine`-Interface (`record`, `play`, `stop`, `arm`, `on_beat`, `stop_all`, …) mit `AbletonOscEngine`.
2. YAML-Schema v0 + Transpiler (YAML → validiertes, aufgelöstes JSON) + Fehlerausgabe mit Zeilennummern und Tippfehler-Vorschlägen.
3. Runner: steppt durch Sektionen, reagiert auf `next_section` (Terminal ODER MIDI via mido), respektiert `autorelease` und `quantize`.
4. `example-song.yaml`: 3 Sektionen — (1) Gitarre record bis Release, (2) Voice record 4 Takte autorelease + Gitarre play, (3) alles play + Overdub auf Gitarre.

### M3 — Prototyp-UI (Subagent)
1. FastAPI-Backend: Runner + Engine, WebSocket-Events (Sektion, Takt/Beat, Pending-Wechsel, MIDI roh), REST "Partitur kompilieren".
2. React-UI, Drei-Bereiche-Layout: Editor (CodeMirror, YAML, Kompilieren-Button, Fehler), Block-Vorschau (Karte pro Sektion, Badges, aktuelle Sektion, Takt-Fortschritt, Pending blinkt, read-only), MIDI-Monitor (Live-Liste + Copy-ID).
3. Transport-Buttons: Start, Stop All, Next Section.

DoD heute: Henry schreibt Partitur im UI, kompiliert, sieht Blöcke, startet, spielt Arrangement mit ≥2 Ebenen, steppt per Button/MIDI durch — Ableton loopt, UI zeigt live Position.

## 6. Bekannte Fallstricke

* OSC-Replies: AbletonOSC antwortet auf Port 11001 — Client muss Listener binden, sonst wirken Gets "tot".
* Timing: Takt-genaue Aktionen nicht per `sleep()`, sondern über Beat-/Song-Position-Events bzw. Abletons Launch-Quantisierung (Global Quantization) — serverseitig, robuster als eigenes UDP-Timing.
* Windows-MIDI: exklusive Device-Öffnung — Controller entweder in unserer App ODER in Ableton.
* Firewall: Erste UDP-Kommunikation kann Windows-Firewall-Prompt auslösen.
* Fehlerpfad zuerst testen: Ableton nicht gestartet / AbletonOSC nicht geladen → klare Fehlermeldung statt Hänger.

## 7. Offene Fragen

* Gruppen-Layer: neue Clips untereinander vs. dynamisch neue Spuren — nach OSC-Robustheit entscheiden, Henry vorlegen.
* "8 Takte über 4-Takte-Loop": Ableton-Standard erstmal akzeptieren.
* MIDI-Controller heute: keiner erkannt (Stand 2026-09-05, nur Scarlett 2i2) → Terminal-Keys/UI-Buttons als Fallback.

## Lokale Umgebung (Stand 2026-09-05)

* Projekt: `C:\Users\Admin\repos\henrys-looper` (kein Git-Repo)
* Python: `.venv\Scripts\python.exe` (3.11.9); Node 21.6.2 / npm 10.2.4
* Ableton Live 11 Suite 11.3.30; Log: `C:\Users\Admin\AppData\Roaming\Ableton\Live 11.3.30\Preferences\Log.txt`
* Audio-Interface: Focusrite Scarlett 2i2 (kein MIDI-Port)
