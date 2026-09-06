# Spike M1: Ableton als Looper via AbletonOSC

Start (Ableton 11 läuft, AbletonOSC als Control Surface aktiv, Track 1+2 = Audio-/MIDI-Tracks mit Input, Slots in Scene 1 leer):

    .venv/Scripts/python.exe spike/spike_ableton_osc.py                       # Defaults: Track 0 + 1, Scene 0, 4 Takte
    .venv/Scripts/python.exe spike/spike_ableton_osc.py --bars 2 --track-a 2 --track-b 3
    .venv/Scripts/python.exe spike/spike_ableton_osc.py --cleanup-only        # nur aufraeumen (siehe unten)

Ablauf: Tempo lesen (Verbindungstest, 2 s Timeout -> Exit 1; Port 11001 belegt -> Exit 2) -> Global Quantization = 1 Bar ->
Track A armen, Clip-Slot feuern (Record startet an der Taktgrenze) -> Beat-Events mitzaehlen, im letzten Takt den Slot erneut
feuern -> Ableton beendet die Aufnahme quantisiert und spielt den Loop -> Taste -> dasselbe fuer Track B -> Taste -> alles
stoppen, Listener abmelden, Quantisierung zuruecksetzen. Die Taktgenauigkeit kommt von Ableton, nicht von Python-Timern.

## Tasten (waehrend das Skript laeuft)

| Taste            | Wirkung                                                         |
|------------------|-----------------------------------------------------------------|
| Enter            | weiter (Loop 2 starten bzw. am Ende stoppen und beenden)        |
| q oder Esc       | sofort abbrechen: Cleanup, Exit 130                             |
| Ctrl+C / Ctrl+Break | wie q (zweites Ctrl+C waehrend des Cleanups = harter Exit)   |

Die Tasten werden im Hauptthread per `msvcrt.kbhit()/getwch()` gepollt, nicht per `input()`. Grund: `input()` braucht die
Konsole im Line-Input-Modus; hinterlaesst die aufrufende Shell (PSReadLine) die Konsole im Raw-/VT-Modus, kommt bei `input()`
nie ein Zeilenende an und Ctrl+C wird als Zeichen statt als Signal geliefert -> das Skript wirkte eingefroren (Tag-1-Bug).
Alle Waits sind in 100-ms-Scheiben unterbrechbar, alle Hintergrund-Threads sind Daemon-Threads, das Cleanup ist idempotent
und auf ~2 s begrenzt. Wird das Skript ohne Konsole gestartet (stdin = Pipe), gelten Zeilen: leere Zeile = Enter, `q` = beenden.

## --cleanup-only

Wenn ein Lauf hart abgebrochen wurde (taskkill, Absturz) und Tracks noch armed sind / die Wiedergabe laeuft:

    .venv/Scripts/python.exe spike/spike_ableton_osc.py --cleanup-only [--track-a 0 --track-b 1 --scene 0]

sendet nur `/live/track/set/arm 0` fuer beide Tracks, `/live/song/stop_playing` und Global Quantization = 1 Bar, meldet alle
Listener ab und beendet sich. Exit 0 = AbletonOSC hat geantwortet, Exit 1 = keine Antwort (Cleanup wurde trotzdem blind
gesendet). Ist Port 11001 noch von einer haengenden Instanz belegt, wird der Verbindungstest uebersprungen und blind gesendet.

## Exit-Codes

0 = normal beendet, 1 = keine Antwort von AbletonOSC / Fehler im Ablauf, 2 = Reply-Port belegt, 3 = python-osc fehlt,
130 = Abbruch durch Benutzer (q, Esc, Ctrl+C).
