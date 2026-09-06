"""AbletonOscEngine - drives Ableton Live via AbletonOSC (UDP 11000 -> replies on 11001).

OSC facts verified in the M1 spike (spike/spike_ableton_osc.py) and the AbletonOSC
sources (abletonosc/song.py, track.py, clip_slot.py, clip.py):
  * replies always carry the request indices in front of the value: (*indices, *values);
  * /live/song/set/clip_trigger_quantization 5  -> Global Quantization = 1 Bar;
  * /live/song/start_listen/beat -> /live/song/get/beat <int song beat>, once per beat;
  * /live/song/set/signature_numerator <int>, /live/song/set/signature_denominator <int>
    (README "Song API"); get/signature_numerator|denominator reply with the plain value;
  * /live/song/get/num_tracks -> <int count>; /live/track/get/name <t> -> (t, <str name>)
    (README "Song API" / "Track API"); a track index that does not exist gets NO reply at all
    (only /live/error), which is exactly the timeout trap the Runner's start() check avoids;
  * /live/track/set/arm <t> 1|0; /live/track/set/current_monitoring_state <t> 0|1|2 (In/Auto/Off);
  * /live/clip_slot/fire <t> <s>; /live/clip_slot/start_listen/has_clip <t> <s>;
  * /live/clip_slot/start_listen/playing_status <t> <s> -> (t, s, 0|1|2) = stopped|playing|recording
    (Live API ClipSlot.playing_status; works for empty slots, so it is our primary clip-state source);
  * /live/clip/start_listen/is_recording <t> <s> only works once a clip exists (fallback source).

Robustness rules (Ableton test 2026-09-05, another OSC client was hammering AbletonOSC):
  * request() matches replies by address AND the leading index arguments, so listener
    updates for other slots / foreign gets never satisfy or confuse a pending request;
  * unexpected or malformed messages are ignored with a debug log, never block anything;
  * prime_slots() batches many has_clip gets into one wait (used by the Runner in start());
    everything the Runner does during playback is send-only (no waiting on replies).
"""

from __future__ import annotations

import logging
import socket
import threading
import time
from typing import Callable, Iterable

from pythonosc.dispatcher import Dispatcher
from pythonosc.osc_server import ThreadingOSCUDPServer
from pythonosc.udp_client import SimpleUDPClient

from .base import MONITORING_MODES, Engine, EngineConnectionError, EngineError

log = logging.getLogger(__name__)

DEFAULT_HOST = "127.0.0.1"
DEFAULT_SEND_PORT = 11000
DEFAULT_REPLY_PORT = 11001
REPLY_TIMEOUT_S = 2.0
QUANT_1_BAR = 5
MONITORING_VALUES = {"in": 0, "auto": 1, "off": 2}
PLAYING_STATUS = {0: "stopped", 1: "playing", 2: "recording"}


def _port_in_use(exc: OSError) -> bool:
    return (
        getattr(exc, "errno", None) in (10048, 98)
        or getattr(exc, "winerror", None) == 10048
        or "in use" in str(exc).lower()
    )


class AbletonOscEngine(Engine):
    name = "ableton"

    def __init__(
        self,
        host: str = DEFAULT_HOST,
        send_port: int = DEFAULT_SEND_PORT,
        reply_port: int = DEFAULT_REPLY_PORT,
        reply_timeout: float = REPLY_TIMEOUT_S,
    ) -> None:
        super().__init__()
        self.host = host
        self.send_port = int(send_port)
        self.reply_port = int(reply_port)
        self.reply_timeout = float(reply_timeout)

        self._lock = threading.RLock()
        self._client: SimpleUDPClient | None = None
        self._server: ThreadingOSCUDPServer | None = None
        self._server_thread: threading.Thread | None = None
        self._connected = False
        self._closed = False

        self._pending: dict[str, list[tuple[tuple, threading.Event, dict]]] = {}
        self._beat_cbs: list[Callable[[int], None]] = []
        self._clip_cbs: list[Callable[[int, int, str], None]] = []
        self._listened_slots: set[tuple[int, int]] = set()      # playing_status + has_clip
        self._hasclip_listened: set[tuple[int, int]] = set()    # has_clip only (primed slots)
        self._clip_listened: set[tuple[int, int]] = set()
        self._clip_state: dict[tuple[int, int], str] = {}
        self._clip_flags: dict[tuple[int, int], dict[str, bool]] = {}
        self._beats_per_bar = 4
        self._original_quantization: int | None = None
        self.errors: list[str] = []  # /live/error messages received (for logging by the caller)

    # ---------------------------------------------------------- transport
    @property
    def connected(self) -> bool:
        return self._connected

    def _start_server(self) -> None:
        dispatcher = Dispatcher()
        dispatcher.map("/live/song/get/beat", self._on_beat_msg)
        dispatcher.map("/live/clip_slot/get/playing_status", self._on_playing_status)
        dispatcher.map("/live/clip_slot/get/has_clip", self._on_has_clip)
        dispatcher.map("/live/clip/get/is_recording", self._on_clip_flag, "is_recording")
        dispatcher.map("/live/clip/get/is_playing", self._on_clip_flag, "is_playing")
        dispatcher.map("/live/error", self._on_error)
        dispatcher.set_default_handler(self._on_any_reply)
        try:
            server = ThreadingOSCUDPServer((self.host, self.reply_port), dispatcher)
        except OSError as exc:
            if _port_in_use(exc):
                raise EngineConnectionError(
                    f"Port {self.reply_port} ist bereits belegt. Läuft noch eine andere Instanz "
                    f"(Spike-Skript, Backend oder zweiter Runner)? Bitte beenden und erneut starten."
                ) from exc
            raise EngineConnectionError(
                f"Reply-Port {self.reply_port} konnte nicht geöffnet werden: {exc}"
            ) from exc
        self._server = server
        self._server_thread = threading.Thread(
            target=server.serve_forever, name="abletonosc-reply", daemon=True
        )
        self._server_thread.start()

    def _stop_server(self) -> None:
        server, self._server = self._server, None
        if server is not None:
            server.shutdown()
            server.server_close()
        thread, self._server_thread = self._server_thread, None
        if thread is not None:
            thread.join(timeout=1.0)

    def connect(self) -> None:
        with self._lock:
            if self._closed:
                raise EngineError("Engine wurde bereits geschlossen.")
            if self._connected:
                return
            self._client = SimpleUDPClient(self.host, self.send_port)
            self._start_server()
        try:
            (tempo,) = self.request("/live/song/get/tempo")
        except EngineError as exc:
            self._stop_server()
            raise EngineConnectionError(
                f"Keine Antwort von AbletonOSC ({self.host}:{self.send_port}, Reply-Port "
                f"{self.reply_port}). Läuft Ableton? Ist AbletonOSC unter Preferences → "
                f"Link/Tempo/MIDI → Control Surface ausgewählt?"
            ) from exc
        self._tempo = float(tempo)
        try:
            (num,) = self.request("/live/song/get/signature_numerator")
            self._beats_per_bar = int(num)
        except EngineError:
            self._beats_per_bar = 4
        self._connected = True
        self.send("/live/song/start_listen/beat")

    def close(self) -> None:
        with self._lock:
            if self._closed:
                return
            self._closed = True
            was_connected = self._connected
            self._connected = False
            slots = list(self._listened_slots)
            hasclip_only = [k for k in self._hasclip_listened if k not in self._listened_slots]
            clips = list(self._clip_listened)
            self._listened_slots.clear()
            self._hasclip_listened.clear()
            self._clip_listened.clear()
        deadline = time.monotonic() + 2.0
        try:
            if was_connected and self._client is not None:
                self.send("/live/song/stop_listen/beat")
                for t, s in slots:
                    self.send("/live/clip_slot/stop_listen/playing_status", t, s)
                    self.send("/live/clip_slot/stop_listen/has_clip", t, s)
                for t, s in hasclip_only:
                    self.send("/live/clip_slot/stop_listen/has_clip", t, s)
                for t, s in clips:
                    self.send("/live/clip/stop_listen/is_recording", t, s)
                    self.send("/live/clip/stop_listen/is_playing", t, s)
                if self._original_quantization is not None:
                    self.send("/live/song/set/clip_trigger_quantization", self._original_quantization)
                # let the last datagrams leave (not musical timing)
                time.sleep(min(0.1, max(0.0, deadline - time.monotonic())))
        finally:
            self._stop_server()
            self._client = None

    # ----------------------------------------------------------- plumbing
    def send(self, address: str, *args) -> None:
        client = self._client
        if client is None:
            raise EngineError("Engine ist nicht verbunden (connect() aufrufen).")
        try:
            client.send_message(address, list(args))
        except (OSError, socket.error) as exc:
            raise EngineError(f"OSC senden fehlgeschlagen ({address}): {exc}") from exc

    def request(self, address: str, *args, timeout: float | None = None) -> tuple:
        """Send a /get request and wait for the reply on the same address.

        Returns the value part of the reply (indices stripped). Raises EngineError on timeout.
        """
        timeout = self.reply_timeout if timeout is None else timeout
        indices = tuple(args)
        event = threading.Event()
        holder: dict = {}
        entry = (indices, event, holder)
        with self._lock:
            self._pending.setdefault(address, []).append(entry)
        try:
            self.send(address, *args)
        except EngineError:
            with self._lock:
                self._pending.get(address, []).remove(entry)
            raise
        if not event.wait(timeout):
            with self._lock:
                lst = self._pending.get(address, [])
                if entry in lst:
                    lst.remove(entry)
            raise EngineError(
                f"Zeitüberschreitung: keine Antwort von AbletonOSC auf {address} {list(args)}"
            )
        reply = holder["args"]
        return reply[len(indices):] if len(reply) > len(indices) else reply

    def prime_slots(self, slots: Iterable[tuple[int, int]], timeout: float = 1.5
                    ) -> dict[tuple[int, int], bool | None]:
        """Batch has_clip query: all gets go out at once, one shared deadline, then subscribe.

        Unanswered slots map to None. Late replies are still consumed by _on_has_clip as
        listener updates (-> on_slot_has_clip), so a slow AbletonOSC only delays, never blocks.
        """
        address = "/live/clip_slot/get/has_clip"
        keys = [(int(t), int(s)) for t, s in slots]
        entries: dict[tuple[int, int], tuple] = {}
        with self._lock:
            for key in keys:
                if key in entries:
                    continue
                entry = (key, threading.Event(), {})
                self._pending.setdefault(address, []).append(entry)
                entries[key] = entry
        send_error: EngineError | None = None
        for key in entries:
            try:
                self.send(address, *key)
            except EngineError as exc:
                send_error = exc
                break
        deadline = time.monotonic() + max(0.0, float(timeout))
        result: dict[tuple[int, int], bool | None] = {}
        for key, (_, event, holder) in entries.items():
            remaining = deadline - time.monotonic()
            if (remaining > 0 and event.wait(remaining)) or event.is_set():
                args = holder.get("args", ())
                result[key] = bool(_num(args[2])) if len(args) >= 3 else None
            else:
                result[key] = None
        with self._lock:
            lst = self._pending.get(address, [])
            for entry in entries.values():
                if entry in lst:
                    lst.remove(entry)
        if send_error is not None:
            raise send_error
        for key in entries:
            try:
                self._ensure_has_clip_listener(*key)
            except EngineError:
                break
        return result

    def _resolve_pending(self, address: str, args: tuple) -> bool:
        with self._lock:
            lst = self._pending.get(address)
            if not lst:
                return False
            for entry in lst:
                indices, event, holder = entry
                n = len(indices)
                if n == 0 or tuple(_num(a) for a in args[:n]) == tuple(_num(i) for i in indices):
                    lst.remove(entry)
                    holder["args"] = tuple(args)
                    event.set()
                    return True
        return False

    # ----------------------------------------------------------- handlers
    def _on_any_reply(self, address: str, *args) -> None:
        if not self._resolve_pending(address, tuple(args)):
            # foreign traffic (other OSC clients, unknown listeners): ignore quietly
            log.debug("Unerwartete OSC-Nachricht ignoriert: %s %s", address, list(args))

    def _on_error(self, address: str, *args) -> None:
        msg = " ".join(str(a) for a in args)
        with self._lock:
            self.errors.append(msg)
            if len(self.errors) > 100:
                del self.errors[:-100]
        # an error can also be the answer to a pending request -> release the waiter early
        # (we do not know which one; waiters time out otherwise)

    def _on_beat_msg(self, address: str, *args) -> None:
        if not args:
            return
        try:
            beat = int(args[0])
        except (TypeError, ValueError):
            return
        with self._lock:
            cbs = list(self._beat_cbs)
        for cb in cbs:
            try:
                cb(beat)
            except Exception:  # pragma: no cover
                pass

    def _emit_clip_state(self, track: int, scene: int, state: str) -> None:
        key = (track, scene)
        with self._lock:
            if self._clip_state.get(key) == state:
                return
            self._clip_state[key] = state
            cbs = list(self._clip_cbs)
        for cb in cbs:
            try:
                cb(track, scene, state)
            except Exception:  # pragma: no cover
                pass

    @staticmethod
    def _slot_args(address: str, args: tuple) -> tuple[int, int, object] | None:
        """(track, scene, value) from a slot reply, or None for malformed/foreign messages."""
        if len(args) < 3:
            log.debug("OSC-Nachricht ohne Slot-Indizes ignoriert: %s %s", address, list(args))
            return None
        try:
            return int(args[0]), int(args[1]), args[2]
        except (TypeError, ValueError):
            log.debug("OSC-Nachricht mit ungültigen Indizes ignoriert: %s %s", address, list(args))
            return None

    def _on_playing_status(self, address: str, *args) -> None:
        # a reply to a pending request is also valid listener information -> process both
        self._resolve_pending(address, tuple(args))
        parsed = self._slot_args(address, tuple(args))
        if parsed is None:
            return
        track, scene, raw = parsed
        state = _playing_status_to_state(raw)
        if state is not None:
            self._emit_clip_state(track, scene, state)

    def _on_has_clip(self, address: str, *args) -> None:
        self._resolve_pending(address, tuple(args))
        parsed = self._slot_args(address, tuple(args))
        if parsed is None:
            return
        track, scene, raw = parsed
        has_clip = bool(_num(raw))
        self._emit_slot_has_clip(track, scene, has_clip)
        if has_clip:
            # fallback source: once a clip exists we can listen to clip flags
            with self._lock:
                new = (track, scene) not in self._clip_listened
                if new:
                    self._clip_listened.add((track, scene))
            if new and self._connected:
                try:
                    self.send("/live/clip/start_listen/is_recording", track, scene)
                    self.send("/live/clip/start_listen/is_playing", track, scene)
                except EngineError:
                    pass
        else:
            with self._lock:
                known = (track, scene) in self._clip_state
            if known:
                # a clip we have seen before disappeared (deleted by hand) -> it is stopped now
                self._emit_clip_state(track, scene, "stopped")

    def _on_clip_flag(self, address: str, fixed_args, *args) -> None:
        flag = fixed_args[0]
        self._resolve_pending(address, tuple(args))
        parsed = self._slot_args(address, tuple(args))
        if parsed is None:
            return
        track, scene, raw = parsed
        value = bool(_num(raw))
        key = (track, scene)
        with self._lock:
            flags = self._clip_flags.setdefault(key, {"is_recording": False, "is_playing": False})
            flags[flag] = value
            if flags["is_recording"]:
                state = "recording"
            elif flags["is_playing"]:
                state = "playing"
            else:
                state = "stopped"
        self._emit_clip_state(track, scene, state)

    # ------------------------------------------------------------- engine
    def get_tempo(self) -> float:
        (tempo,) = self.request("/live/song/get/tempo")
        self._tempo = float(tempo)
        return self._tempo

    def set_tempo(self, bpm: float) -> None:
        self.send("/live/song/set/tempo", float(bpm))
        self._tempo = float(bpm)

    def get_beats_per_bar(self) -> int:
        return self._beats_per_bar

    def set_time_signature(self, numerator: int, denominator: int) -> None:
        numerator, denominator = int(numerator), int(denominator)
        if numerator < 1 or denominator < 1:
            raise EngineError(f"Ungültige Taktart: {numerator}/{denominator}")
        self.send("/live/song/set/signature_numerator", numerator)
        self.send("/live/song/set/signature_denominator", denominator)
        self._beats_per_bar = numerator

    def get_track_count(self) -> int:
        """Number of tracks in the set (/live/song/get/num_tracks, AbletonOSC "Song API")."""
        (count,) = self.request("/live/song/get/num_tracks")
        return int(_num(count))

    def get_track_name(self, index: int) -> str | None:
        """Track name via /live/track/get/name <index> -> (index, name); None if unanswered.

        AbletonOSC stays silent for a track index that does not exist (it only logs
        /live/error), so a timeout here means "unknown", never a hard failure.
        """
        try:
            reply = self.request("/live/track/get/name", int(index))
        except EngineError:
            return None
        return str(reply[0]) if reply else None

    def set_quantization_bar(self) -> None:
        if self._original_quantization is None:
            try:
                (q,) = self.request("/live/song/get/clip_trigger_quantization")
                self._original_quantization = int(q)
            except EngineError:
                self._original_quantization = None
        self.send("/live/song/set/clip_trigger_quantization", QUANT_1_BAR)

    def start_transport(self) -> None:
        self.send("/live/song/start_playing")

    def stop_transport(self) -> None:
        self.send("/live/song/stop_playing")

    def arm(self, track: int, on: bool) -> None:
        self.send("/live/track/set/arm", int(track), 1 if on else 0)

    def set_monitoring(self, track: int, mode: str) -> None:
        if mode not in MONITORING_MODES:
            raise EngineError(f"Ungültiger Monitoring-Modus: {mode!r}")
        self.send("/live/track/set/current_monitoring_state", int(track), MONITORING_VALUES[mode])

    def _ensure_has_clip_listener(self, track: int, scene: int) -> None:
        key = (int(track), int(scene))
        with self._lock:
            if key in self._hasclip_listened or key in self._listened_slots:
                return
            self._hasclip_listened.add(key)
        self.send("/live/clip_slot/start_listen/has_clip", *key)

    def _ensure_slot_listeners(self, track: int, scene: int) -> None:
        key = (int(track), int(scene))
        with self._lock:
            if key in self._listened_slots:
                return
            self._listened_slots.add(key)
            has_clip_done = key in self._hasclip_listened
        self.send("/live/clip_slot/start_listen/playing_status", *key)
        if not has_clip_done:
            self.send("/live/clip_slot/start_listen/has_clip", *key)

    def fire_slot(self, track: int, scene: int) -> None:
        self._ensure_slot_listeners(track, scene)
        self.send("/live/clip_slot/fire", int(track), int(scene))

    def slot_has_clip(self, track: int, scene: int) -> bool:
        (value,) = self.request("/live/clip_slot/get/has_clip", int(track), int(scene))
        return bool(_num(value))

    def stop_track(self, track: int) -> None:
        self.send("/live/track/stop_all_clips", int(track))

    def stop_all(self) -> None:
        self.send("/live/song/stop_all_clips")

    def on_beat(self, cb: Callable[[int], None]) -> None:
        with self._lock:
            self._beat_cbs.append(cb)

    def on_clip_state(self, cb: Callable[[int, int, str], None]) -> None:
        with self._lock:
            self._clip_cbs.append(cb)


def _num(value):
    """Coerce OSC scalars (int/float/bool/str) to a comparable number where possible."""
    if isinstance(value, bool):
        return int(value)
    if isinstance(value, (int, float)):
        return value
    try:
        return float(value)
    except (TypeError, ValueError):
        return value


def _playing_status_to_state(raw) -> str | None:
    if isinstance(raw, str):
        text = raw.lower()
        if "record" in text:
            return "recording"
        if "play" in text:
            return "playing"
        if "stop" in text:
            return "stopped"
        return None
    try:
        return PLAYING_STATUS.get(int(raw))
    except (TypeError, ValueError):
        return None
