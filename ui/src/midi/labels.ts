//! Words of the MIDI side, and how an address reads in the binding list.
//!
//! The backend sends a label with every binding (`Track 3: Record`), which is right for a log line
//! but not for the sidebar of the design, which says **`chor · Record`** under a group **Tracks**:
//! the track by the name the musician gave it, the action in the same words the buttons use
//! (`shell/terms.ts`). So the address is read here once more - only to *name* it. Whether it exists
//! and whether it works is still the backend's answer (`MidiBindingView.state`), never decided here.

import { T } from "../shell/terms";
import type { MidiBindingState, MidiDeviceState, MidiOutcome } from "./types";

export const M = {
  learnOn: "MIDI Learn on.",
  learnHintLive: "Click a marked control, then press a key or move a knob on the controller. Bound controls carry a badge.",
  learnHintArrangement: "Click a marked control, then press a key or move a knob. Clips are not MIDI targets.",
  keepsPlaying: "Playback keeps running",
  done: "Done",
  title: "MIDI Mapping",
  byArea: "By area",
  byDevice: "By device",
  targetChosen: "Target selected",
  waitingForController: "Waiting for controller …",
  waitingHint: "Press a key or pad, or move a knob.",
  waiting: "waiting …",
  cancel: "Cancel",
  removeBinding: "Remove binding",
  pickTarget: "Click a marked control to map it.",
  noDeviceYet: "No controller connected yet - plug one in, or connect an input below.",
  scanAgain: "Scan again",
  moveTo: "Move to another device",
  move: "Move",
  connect: "Connect",
  disconnect: "Disconnect",
  rename: "Rename",
  delete: "Delete",
  active: "active",
  inputMonitor: "Input monitor",
  channels: "Ch 1–16",
  monitorEmpty: "Press a pad or turn a knob - what arrives shows up here.",
  nothingMapped: "Nothing mapped yet - click a marked control, then press a key on the controller.",
  transport: "Transport",
  global: "Global",
  tracks: "Tracks",
  score: "Score",
  fromScore: "from the score",
  notSaved: "Not saved",
  retry: "Retry",
  newInput: "New input",
  newInputHint: "connect once - it is remembered from then on",
} as const;

export const DEVICE_STATE_WORD: Record<MidiDeviceState, string> = {
  connected: "connected",
  missing: "missing",
  disconnected: "disconnected",
  unavailable: "busy",
  error: "profile error",
};

/** Short word for a binding that cannot do its job (null = it can). */
export const BINDING_STATE_WORD: Record<MidiBindingState, string | null> = {
  active: null,
  unchecked: null,
  device_missing: "device missing",
  target_missing: "target missing",
  overridden: "score overrides",
};

export const OUTCOME_WORD: Record<MidiOutcome, string> = {
  unbound: "unmapped",
  absorbed: "absorbed",
  action: "done",
  refused: "refused",
  learned: "learned",
};

export function plural(n: number, one: string, many = `${one}s`): string {
  return `${n} ${n === 1 ? one : many}`;
}

/** `20:41` - the clock time a "last seen" or a monitor line shows. */
export function clock(ms: number, seconds = false): string {
  return new Date(ms).toLocaleTimeString([], {
    hour: "2-digit",
    minute: "2-digit",
    ...(seconds ? { second: "2-digit" } : {}),
    hour12: false,
  });
}

/** "last seen 20:41", or "last seen 3 Nov, 20:41" when it was not today. */
export function lastSeen(ms: number | null): string | null {
  if (ms === null) return null;
  const at = new Date(ms);
  const today = new Date();
  const sameDay = at.toDateString() === today.toDateString();
  const day = sameDay ? "" : `${at.toLocaleDateString([], { day: "numeric", month: "short" })}, `;
  return `last seen ${day}${clock(ms)}`;
}

const TRACK_PART: Record<string, string> = {
  record: T.record,
  overdub: T.overdub,
  play: T.play,
  stop: T.stop,
  clear: T.clear,
  monitor: T.monitor,
  pan: T.pan,
  latency_trim: "Latency trim",
};

const GLOBAL_PART: Record<string, string> = {
  "global.click": "Click on/off",
  "global.clear_all": T.clearAll,
  "global.tempo": T.tempo,
  "global.quantize.bar": T.quantizeBar,
  "global.quantize.loop": T.quantizeLoop,
  "global.bus.main.gain": "Main volume",
  "global.bus.monitor.gain": "Headphone volume",
};

const BUS: Record<string, string> = { main: T.main, monitor: T.monitorBus };

/** Where an address belongs in the list "by area". */
export interface Area {
  /** Sort and group key: `0transport`, `1global`, `2track.003`, `9score`. */
  key: string;
  title: string;
}

/** 0-based track index of a `track.N...` address, or null. */
export function trackOf(address: string): number | null {
  const m = /^track\.(\d+)\./.exec(address);
  return m ? Number(m[1]) - 1 : null;
}

export function areaOf(address: string, tracks: string[]): Area {
  if (address.startsWith("transport.")) return { key: "0transport", title: M.transport };
  if (address.startsWith("global.")) return { key: "1global", title: M.global };
  const track = trackOf(address);
  if (track !== null) {
    const name = tracks[track];
    return {
      key: `2track.${String(track).padStart(3, "0")}`,
      title: name ? `${M.tracks} · ${track + 1} ${name}` : `${M.tracks} · ${track + 1}`,
    };
  }
  return { key: "8other", title: "Other" };
}

/**
 * The action an address stands for, without its track: `Record`, `Release`, `Main volume`.
 * `fallback` is the backend's label, used for whatever this table does not know.
 */
export function actionLabel(address: string, fallback: string): string {
  if (address === "transport.start") return T.start;
  if (address === "transport.next") return T.release;
  if (address === "transport.stop_all") return T.stopAll;
  const go = /^transport\.goto\.(\d+)$/.exec(address);
  if (go) return `Go to section ${go[1]}`;
  if (GLOBAL_PART[address]) return GLOBAL_PART[address];
  const m = /^track\.[^.]+\.(.+)$/.exec(address);
  if (m) {
    const rest = m[1];
    if (TRACK_PART[rest]) return TRACK_PART[rest];
    const send = /^(bus|monitor_bus)\.(\w+)$/.exec(rest);
    if (send) return `${send[1] === "bus" ? T.loop : T.monitor} → ${BUS[send[2]] ?? send[2]}`;
    const layer = /^layer\.(\d+)\.(mute|remove|gain)$/.exec(rest);
    if (layer) return `${T.layer} ${layer[1]} ${layer[2] === "gain" ? "volume" : layer[2]}`;
  }
  // `Track 3: Reverb mix` -> `Reverb mix`
  const colon = fallback.indexOf(": ");
  return colon >= 0 && fallback.startsWith("Track ") ? fallback.slice(colon + 2) : fallback;
}

/** `chor · Record`, `Release`, `Main volume` - an address as a sentence fragment. */
export function targetLabel(address: string, tracks: string[], fallback = address): string {
  const action = actionLabel(address, fallback);
  const track = trackOf(address);
  if (track === null) return action;
  return `${tracks[track] ?? `Track ${track + 1}`} · ${action}`;
}
