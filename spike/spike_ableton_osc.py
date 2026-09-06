#!/usr/bin/env python
"""Spike M1: drive Ableton Live as a looper via AbletonOSC.

Flow:
  1. Read song tempo (connection sanity check, 2 s timeout -> exit 1).
  2. Set global clip trigger quantization to 1 bar (Ableton quantizes for us).
  3. Arm track A, fire its clip slot (scene 0) -> recording starts on the next bar.
  4. Follow /live/song/get/beat events; inside the last bar of the loop, fire
     the slot again -> Ableton ends the recording exactly on the bar line and
     switches the clip to loop playback (variant A, standard looper behaviour).
  5. Wait for a key (Enter = continue, q/Esc = quit), repeat for track B,
     wait for a key again -> stop everything, clean up.

  --cleanup-only: only send the cleanup (arm 0 on both tracks, stop playing,
  quantization 1 bar) and exit -- for the case a previous run was hard-killed.

Keyboard handling (Windows): NO blocking input(). input() relies on the console
being in cooked/line mode; if the calling shell (PSReadLine) left the console in
raw/VT mode, input() never sees the '\\n' it waits for and Ctrl+C arrives as a
plain '\\x03' character instead of a signal -> the script looks frozen. We poll
msvcrt.kbhit()/getwch() from the main thread instead, which reads raw key events
independent of the console mode. SIGINT/SIGBREAK only set an abort flag, all
waits are sliced (100 ms) and check that flag, every background thread is a
daemon thread and the cleanup is idempotent and bounded (~2 s).

OSC address reference: https://github.com/ideoforms/AbletonOSC (README + abletonosc/*.py)
Replies from AbletonOSC always arrive on UDP port 11001 and carry the request's
indices in front of the value, e.g. /live/clip/get/is_recording -> (track, clip, value).

No time.sleep()-based musical timing: all bar alignment is done by Ableton's
quantization; this script only decides *within which bar* to send the stop-fire.
"""

import argparse
import os
import queue
import signal
import sys
import threading
import time

try:
    from pythonosc.dispatcher import Dispatcher
    from pythonosc.osc_server import ThreadingOSCUDPServer
    from pythonosc.udp_client import SimpleUDPClient
except ImportError:  # pragma: no cover
    print("python-osc fehlt. Installieren mit: pip install python-osc")
    sys.exit(3)

try:
    import msvcrt  # Windows console: non-blocking key polling
except ImportError:  # pragma: no cover - non-Windows
    msvcrt = None

# --------------------------------------------------------------------------- #
# Configuration (overridable via CLI flags)
# --------------------------------------------------------------------------- #
DEFAULT_HOST = "127.0.0.1"
DEFAULT_SEND_PORT = 11000       # AbletonOSC listens here
DEFAULT_REPLY_PORT = 11001      # AbletonOSC replies here (fixed by AbletonOSC)
DEFAULT_TRACK_A = 0             # first loop track (Live shows it as track 1)
DEFAULT_TRACK_B = 1             # second loop track (Live shows it as track 2)
DEFAULT_SCENE = 0               # clip slot / scene index
DEFAULT_BARS = 4                # loop length in bars
REPLY_TIMEOUT_S = 2.0           # timeout for simple get requests
QUANT_1_BAR = 5                 # clip_trigger_quantization enum: 5 = 1 Bar (README)
POLL_S = 0.1                    # slice length for interruptible waits (not musical timing)
KEY_POLL_S = 0.05               # keyboard polling interval
CLEANUP_BUDGET_S = 2.0          # hard upper bound for cleanup()

# How many beats before the loop end we send the stop-fire. Ableton quantizes
# the fire to the next bar line, so anything inside the last bar works; we aim
# for the middle of the last bar to be robust against +/-1 beat jitter between
# the has_clip event and the beat counter.
STOP_FIRE_BEATS_BEFORE_END = 2

EXIT_OK = 0
EXIT_NO_REPLY = 1
EXIT_PORT_BUSY = 2
EXIT_ABORTED = 130              # Ctrl+C / q pressed (conventional 128+SIGINT)


class AbletonOSCError(RuntimeError):
    pass


class Aborted(Exception):
    """User asked to stop (Ctrl+C, Ctrl+Break, q or Esc)."""


# --------------------------------------------------------------------------- #
# Keyboard input without blocking input()
# --------------------------------------------------------------------------- #
class KeyInput:
    """Non-blocking key source.

    Windows console: msvcrt.kbhit()/getwch() polled from the calling thread.
    Anything else (pipe, non-Windows): a daemon thread reads stdin lines into a queue,
    so the script stays scriptable ('' = Enter, 'q' = quit).
    Returns "next", "quit" or None from poll().
    """

    QUIT_CHARS = {"q", "Q", "\x1b", "\x03"}   # q, Esc, Ctrl+C-as-character (raw console mode)
    NEXT_CHARS = {"\r", "\n"}

    def __init__(self):
        self.console = msvcrt is not None and sys.stdin is not None and sys.stdin.isatty()
        self._lines = queue.Queue()
        if not self.console:
            threading.Thread(target=self._line_reader, name="stdin-reader", daemon=True).start()

    def _line_reader(self):
        try:
            for line in sys.stdin:
                self._lines.put(line)
        except Exception:
            pass
        self._lines.put(None)  # EOF

    def poll(self):
        if self.console:
            while msvcrt.kbhit():
                ch = msvcrt.getwch()
                if ch in ("\x00", "\xe0"):          # function/arrow key prefix: swallow the second code
                    if msvcrt.kbhit():
                        msvcrt.getwch()
                    continue
                if ch == "\x1b":                     # Esc alone = quit; Esc-sequence (VT arrow keys) = ignore
                    time.sleep(KEY_POLL_S)
                    if msvcrt.kbhit():
                        while msvcrt.kbhit():
                            msvcrt.getwch()
                        continue
                    return "quit"
                if ch in self.QUIT_CHARS:
                    return "quit"
                if ch in self.NEXT_CHARS:
                    return "next"
            return None
        try:
            line = self._lines.get_nowait()
        except queue.Empty:
            return None
        if line is None:
            return "quit"                            # stdin closed -> treat as quit
        return "quit" if line.strip().lower() in ("q", "quit", "exit") else "next"


# --------------------------------------------------------------------------- #
# The spike
# --------------------------------------------------------------------------- #
class LooperSpike:
    def __init__(self, host, send_port, reply_port, scene, bars):
        self.host = host
        self.send_port = send_port
        self.reply_port = reply_port
        self.scene = scene
        self.bars = bars

        self.print_lock = threading.Lock()
        self.state_lock = threading.Lock()
        self.abort = threading.Event()   # set by SIGINT/SIGBREAK or q/Esc
        self._cleaned = False

        # transport / musical state
        self.beat = None                 # last int beat from /live/song/get/beat
        self.beats_per_bar = 4
        self.tempo = None
        self.original_quantization = None

        # per-recording state
        self.active_track = None         # track currently being recorded
        self.record_start_beat = None
        self.stop_fire_sent = False
        self.loop_done = threading.Event()
        self.record_started = threading.Event()

        # generic request/reply plumbing: address -> (event, value holder)
        self.pending = {}
        self.pending_lock = threading.Lock()

        self.client = SimpleUDPClient(host, send_port)
        self.server = None
        self.server_thread = None
        self.keys = KeyInput()

    # ------------------------------------------------------------------ utils
    def log(self, msg):
        with self.print_lock:
            print(msg, flush=True)

    def install_signal_handlers(self):
        def on_signal(signum, frame):
            if self.abort.is_set():
                # second Ctrl+C while cleanup is running: hard exit
                os._exit(EXIT_ABORTED)
            self.abort.set()

        signal.signal(signal.SIGINT, on_signal)
        if hasattr(signal, "SIGBREAK"):
            signal.signal(signal.SIGBREAK, on_signal)

    def check_abort(self):
        if self.abort.is_set():
            raise Aborted()

    def sleep_poll(self, seconds):
        """Interruptible sleep slice: raises Aborted if the abort flag is set."""
        self.check_abort()
        time.sleep(seconds)
        self.check_abort()

    def wait_event(self, event, timeout):
        """Event.wait() in short slices so Ctrl+C / q are honoured (Event.wait(timeout)
        is not interruptible by SIGINT on Windows). Returns True if set, False on timeout."""
        deadline = time.monotonic() + timeout
        while True:
            self.check_abort()
            if self.keys.poll() == "quit":
                self.abort.set()
                raise Aborted()
            remaining = deadline - time.monotonic()
            if remaining <= 0:
                return False
            if event.wait(min(POLL_S, remaining)):
                return True

    def wait_for_key(self, prompt):
        """Poll the keyboard until Enter ("next") or q/Esc/Ctrl+C ("quit"). Raises Aborted on quit."""
        self.log(f"\n{prompt}  [Enter = weiter, q/Esc = beenden]")
        while True:
            self.check_abort()
            k = self.keys.poll()
            if k == "next":
                return
            if k == "quit":
                self.abort.set()
                raise Aborted()
            time.sleep(KEY_POLL_S)

    def start_server(self):
        dispatcher = Dispatcher()
        dispatcher.map("/live/song/get/beat", self.on_beat)
        dispatcher.map("/live/clip_slot/get/has_clip", self.on_slot_has_clip)
        dispatcher.map("/live/clip/get/is_recording", self.on_clip_is_recording)
        dispatcher.map("/live/error", self.on_error)
        dispatcher.set_default_handler(self.on_any_reply)
        try:
            self.server = ThreadingOSCUDPServer((self.host, self.reply_port), dispatcher)
        except OSError as exc:
            # WinError 10048 / errno 98 (EADDRINUSE): another instance holds the reply port.
            if getattr(exc, "errno", None) in (10048, 98) or getattr(exc, "winerror", None) == 10048 \
                    or "in use" in str(exc).lower():
                print(f"Port {self.reply_port} ist bereits belegt. Läuft schon eine zweite Instanz "
                      f"dieses Skripts (oder ein anderer OSC-Client)? Bitte beenden und erneut starten.")
                sys.exit(EXIT_PORT_BUSY)
            raise
        # Handler threads must never keep the process alive or block server_close().
        self.server.daemon_threads = True
        self.server.block_on_close = False
        self.server_thread = threading.Thread(target=self.server.serve_forever, kwargs={"poll_interval": 0.2},
                                              name="osc-reply-server", daemon=True)
        self.server_thread.start()

    def stop_server(self, timeout=1.0):
        server, self.server = self.server, None
        if server is None:
            return
        # shutdown() blocks until serve_forever() noticed the flag; bound it with a helper thread.
        t = threading.Thread(target=server.shutdown, name="osc-shutdown", daemon=True)
        t.start()
        t.join(timeout)
        try:
            server.server_close()
        except OSError:
            pass

    def send(self, address, *args):
        self.client.send_message(address, list(args))

    def request(self, address, *args, timeout=REPLY_TIMEOUT_S):
        """Send a /get request and wait for the reply on the same address.

        Returns the reply args *without* the echoed indices (i.e. the value part).
        Raises AbletonOSCError on timeout, Aborted on Ctrl+C / q.
        """
        event = threading.Event()
        holder = {}
        with self.pending_lock:
            self.pending[address] = (event, holder)
        self.send(address, *args)
        try:
            if not self.wait_event(event, timeout):
                raise AbletonOSCError(f"timeout waiting for {address}")
        finally:
            with self.pending_lock:
                self.pending.pop(address, None)
        reply = holder["args"]
        return reply[len(args):] if len(reply) > len(args) else reply

    # --------------------------------------------------------------- handlers
    def _resolve_pending(self, address, args):
        with self.pending_lock:
            entry = self.pending.pop(address, None)
        if entry is not None:
            event, holder = entry
            holder["args"] = tuple(args)
            event.set()
            return True
        return False

    def on_any_reply(self, address, *args):
        self._resolve_pending(address, args)

    def on_error(self, address, *args):
        self.log(f"[AbletonOSC /live/error] {' '.join(str(a) for a in args)}")

    def on_beat(self, address, *args):
        if not args:
            return
        beat = int(args[0])
        with self.state_lock:
            self.beat = beat
            bpb = self.beats_per_bar
            song_bar = beat // bpb + 1
            song_beat_in_bar = beat % bpb + 1
            recording = (self.active_track is not None and self.record_start_beat is not None
                         and not self.loop_done.is_set())
            if recording:
                elapsed = beat - self.record_start_beat
                rec_bar = min(elapsed // bpb + 1, self.bars)
                total_beats = self.bars * bpb
                fire_now = (not self.stop_fire_sent
                            and elapsed >= total_beats - STOP_FIRE_BEATS_BEFORE_END)
                if fire_now:
                    self.stop_fire_sent = True
                track = self.active_track
            else:
                elapsed = rec_bar = None
                fire_now = False
                track = None

        if recording:
            self.log(f"[Song Takt {song_bar} Beat {song_beat_in_bar}]  Record läuft "
                     f"(Takt {rec_bar}/{self.bars}, Beat {min(elapsed + 1, self.bars * bpb)}/{self.bars * bpb})")
            if fire_now:
                # Variant A: fire the recording slot again. Ableton quantizes this to
                # the next bar line, ends the recording there and starts loop playback.
                self.send("/live/clip_slot/fire", track, self.scene)
                self.log(f"    Stop-Fire an Slot (Track {track}, Scene {self.scene}) gesendet -> "
                         f"Ableton beendet die Aufnahme an der nächsten Taktgrenze")
        elif song_beat_in_bar == 1:
            self.log(f"[Song Takt {song_bar} Beat 1]")

    def on_slot_has_clip(self, address, *args):
        # reply format: (track_index, clip_index, has_clip)
        if self._resolve_pending(address, args):
            return  # answered an explicit get request
        if len(args) < 3:
            return
        track, clip, has_clip = int(args[0]), int(args[1]), int(args[2])
        with self.state_lock:
            if track != self.active_track or clip != self.scene:
                return
            if has_clip and self.record_start_beat is None:
                # Recording clip has been created -> recording started on the bar line.
                self.record_start_beat = self.beat if self.beat is not None else 0
                started = True
            else:
                started = False
        if started:
            self.log(f"    Aufnahme gestartet auf Track {track} (Song-Beat {self.record_start_beat})")
            self.send("/live/clip/start_listen/is_recording", track, self.scene)
            self.record_started.set()

    def on_clip_is_recording(self, address, *args):
        # reply format: (track_index, clip_index, is_recording)
        if self._resolve_pending(address, args):
            return
        if len(args) < 3:
            return
        track, clip, is_rec = int(args[0]), int(args[1]), int(args[2])
        with self.state_lock:
            relevant = (track == self.active_track and clip == self.scene
                        and self.record_start_beat is not None and not self.loop_done.is_set())
        if relevant and not is_rec:
            self.loop_done.set()
            self.log(f"    -> Loop läuft auf Track {track}")

    # ------------------------------------------------------------- sequence
    def connect_and_sanity_check(self):
        try:
            (tempo,) = self.request("/live/song/get/tempo")
        except AbletonOSCError:
            print(f"Keine Antwort von AbletonOSC auf Port {self.reply_port}. Läuft Ableton? "
                  f"Ist AbletonOSC unter Preferences → Link/Tempo/MIDI → Control Surface ausgewählt?")
            sys.exit(EXIT_NO_REPLY)
        self.tempo = float(tempo)
        self.log(f"Verbunden mit AbletonOSC ({self.host}:{self.send_port}) - Tempo: {self.tempo:.2f} BPM")

        try:
            (num,) = self.request("/live/song/get/signature_numerator")
            self.beats_per_bar = int(num)
        except AbletonOSCError:
            self.log("Warnung: signature_numerator nicht lesbar, nehme 4/4 an.")
        self.log(f"Taktart-Zähler: {self.beats_per_bar}  ->  {self.bars} Takte = "
                 f"{self.bars * self.beats_per_bar} Beats")

        try:
            (q,) = self.request("/live/song/get/clip_trigger_quantization")
            self.original_quantization = int(q)
        except AbletonOSCError:
            self.original_quantization = None
        self.send("/live/song/set/clip_trigger_quantization", QUANT_1_BAR)
        self.log(f"Global Quantization auf 1 Bar gesetzt (vorher: {self.original_quantization})")

        self.send("/live/song/start_listen/beat")

    def record_loop(self, track):
        """Record `self.bars` bars into (track, scene) and switch to loop playback."""
        # Preconditions
        (can_arm,) = self.request("/live/track/get/can_be_armed", track)
        if not int(can_arm):
            raise AbletonOSCError(f"Track {track} kann nicht armed werden (Group/Return/Master?).")
        (has_clip,) = self.request("/live/clip_slot/get/has_clip", track, self.scene)
        if int(has_clip):
            raise AbletonOSCError(f"Clip-Slot (Track {track}, Scene {self.scene}) ist nicht leer. "
                                  f"Bitte Clip in Ableton löschen und neu starten.")

        with self.state_lock:
            self.active_track = track
            self.record_start_beat = None
            self.stop_fire_sent = False
            self.loop_done.clear()
            self.record_started.clear()

        self.send("/live/track/set/arm", track, 1)
        self.log(f"Track {track} armed")
        self.send("/live/clip_slot/start_listen/has_clip", track, self.scene)
        self.send("/live/clip_slot/fire", track, self.scene)
        self.log(f"Record auf Slot (Track {track}, Scene {self.scene}) ausgelöst - startet an der nächsten Taktgrenze")

        bar_s = self.beats_per_bar * 60.0 / max(self.tempo, 1.0)
        if not self.wait_event(self.record_started, timeout=2 * bar_s + 5.0):
            raise AbletonOSCError("Aufnahme wurde nicht gestartet (kein has_clip-Event). "
                                  "Ist der Track ein Audio-/MIDI-Track mit Input?")
        if not self.wait_event(self.loop_done, timeout=(self.bars + 2) * bar_s + 5.0):
            raise AbletonOSCError("Aufnahme wurde nicht beendet (is_recording blieb 1).")

        # Make sure the clip loops and report its length.
        self.send("/live/clip/set/looping", track, self.scene, 1)
        try:
            (length,) = self.request("/live/clip/get/length", track, self.scene)
            self.log(f"    Clip-Länge: {float(length):.2f} Beats (Soll: {self.bars * self.beats_per_bar})")
        except AbletonOSCError:
            pass

        self.send("/live/clip/stop_listen/is_recording", track, self.scene)
        self.send("/live/clip_slot/stop_listen/has_clip", track, self.scene)
        with self.state_lock:
            self.active_track = None

    def send_cleanup_messages(self, tracks, quantization):
        """Fire-and-forget OSC cleanup: unsubscribe listeners, disarm, stop transport, reset quantization."""
        self.send("/live/song/stop_listen/beat")
        for t in tracks:
            self.send("/live/clip/stop_listen/is_recording", t, self.scene)
            self.send("/live/clip_slot/stop_listen/has_clip", t, self.scene)
            self.send("/live/track/set/arm", t, 0)
        self.send("/live/song/stop_playing")
        if quantization is not None:
            self.send("/live/song/set/clip_trigger_quantization", quantization)

    def cleanup(self, tracks):
        """Idempotent, bounded (~CLEANUP_BUDGET_S) and never raises."""
        if self._cleaned:
            return
        self._cleaned = True
        t0 = time.monotonic()
        try:
            if self.tempo is not None:  # never connected -> nothing to undo in Ableton
                self.log("Beende: Listener abmelden, Tracks disarmen, Wiedergabe stoppen ...")
                self.send_cleanup_messages(tracks, self.original_quantization)
                time.sleep(0.2)  # let the last UDP datagrams leave (not musical timing)
        except Exception as exc:  # pragma: no cover - cleanup must not raise
            self.log(f"Warnung: Fehler im Cleanup ignoriert: {exc}")
        finally:
            self.stop_server(timeout=max(0.1, CLEANUP_BUDGET_S - (time.monotonic() - t0) - 0.1))

    def cleanup_only(self, tracks):
        """--cleanup-only: send the cleanup blind (plus a connection check if the reply port is free)."""
        self.log(f"Cleanup-Modus: arm 0 auf Tracks {tracks}, stop_playing, Quantization = 1 Bar")
        connected = None
        if self.server is not None:
            try:
                (tempo,) = self.request("/live/song/get/tempo")
                connected = True
                self.log(f"Verbunden mit AbletonOSC ({self.host}:{self.send_port}) - Tempo: {float(tempo):.2f} BPM")
            except AbletonOSCError:
                connected = False
                self.log(f"Keine Antwort von AbletonOSC auf Port {self.reply_port} - Cleanup wird trotzdem gesendet.")
        self.send_cleanup_messages(tracks, QUANT_1_BAR)
        time.sleep(0.2)
        self.log("Cleanup gesendet.")
        self._cleaned = True
        self.stop_server()
        return EXIT_OK if connected in (True, None) else EXIT_NO_REPLY


def parse_args():
    p = argparse.ArgumentParser(description="AbletonOSC looper spike (M1)")
    p.add_argument("--host", default=DEFAULT_HOST)
    p.add_argument("--send-port", type=int, default=DEFAULT_SEND_PORT)
    p.add_argument("--reply-port", type=int, default=DEFAULT_REPLY_PORT)
    p.add_argument("--track-a", type=int, default=DEFAULT_TRACK_A, help="first track index (0-based)")
    p.add_argument("--track-b", type=int, default=DEFAULT_TRACK_B, help="second track index (0-based)")
    p.add_argument("--scene", type=int, default=DEFAULT_SCENE, help="clip slot / scene index (0-based)")
    p.add_argument("--bars", type=int, default=DEFAULT_BARS, help="loop length in bars")
    p.add_argument("--cleanup-only", action="store_true",
                   help="only send the cleanup (arm 0 on both tracks, stop playing, quantization 1 bar) and exit")
    return p.parse_args()


def main():
    # German messages contain non-cp1252 characters; make piped output safe on Windows.
    for stream in (sys.stdout, sys.stderr):
        if hasattr(stream, "reconfigure"):
            stream.reconfigure(encoding="utf-8", errors="replace")
    args = parse_args()
    spike = LooperSpike(args.host, args.send_port, args.reply_port, args.scene, args.bars)
    spike.install_signal_handlers()
    tracks = [args.track_a, args.track_b]

    if args.cleanup_only:
        try:
            spike.start_server()
        except SystemExit:
            # reply port held by a hung instance: no connection check, just send the cleanup blind
            spike.log(f"Port {args.reply_port} belegt - sende Cleanup ohne Verbindungstest.")
        sys.exit(spike.cleanup_only(tracks))

    spike.start_server()
    exit_code = EXIT_OK
    try:
        spike.connect_and_sanity_check()

        spike.log(f"\n=== Loop 1: Track {args.track_a} ===")
        spike.record_loop(args.track_a)

        spike.wait_for_key(f"Loop 1 läuft. Enter drücken für Loop 2 auf Track {args.track_b} ...")
        spike.log(f"\n=== Loop 2: Track {args.track_b} ===")
        spike.record_loop(args.track_b)

        spike.wait_for_key("Loop 2 läuft. Enter drücken zum Stoppen und Beenden ...")
    except AbletonOSCError as exc:
        spike.log(f"Fehler: {exc}")
        exit_code = EXIT_NO_REPLY
    except (Aborted, KeyboardInterrupt):
        spike.log("\nAbbruch durch Benutzer.")
        exit_code = EXIT_ABORTED
    finally:
        spike.cleanup(tracks)
    sys.exit(exit_code)


if __name__ == "__main__":
    main()
