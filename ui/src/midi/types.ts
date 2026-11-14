//! The wire format of the MIDI side, mirroring `app/src-tauri/src/midi.rs`.
//!
//! Two things travel: single **events** (a pad was pressed, a knob moved, and what the mapping made
//! of it) and the **view** (which devices are known and connected, what is bound, whether each
//! binding works right now, what the learn mode waits for). Both arrive on the `looper://midi`
//! event; the view is only attached when it changed. Every MIDI command answers with the view too.
//!
//! This file is the contract a user interface builds on. Nothing in it is specific to the current
//! layout: a new design reads `devices` for the connection state, `bindings[].state` for "works /
//! does not work", and `resolved_address ?? address` to find the element a binding belongs to.

/** What one incoming event did. The four answers the router gives, plus the one learn adds. */
export type MidiOutcome = "unbound" | "absorbed" | "action" | "refused" | "learned";

/** One line of the monitor. */
export interface MidiEventReport {
  /** Counts up for the life of the app - the key of a list row. */
  seq: number;
  /** `ch1.note36` - the key a profile file uses. */
  id: string;
  /** `Pad 36`, `CC 3`, `Bend` - the badge on a bound control. */
  short: string;
  channel: number;
  /** `Note on`, `Note off`, `CC`, `Pitch bend`. */
  what: string;
  /** Note or controller number; null for a pitch bend, which has none. */
  number: number | null;
  /** Velocity, controller value, or the bend around zero. */
  value: number;
  /** The address this control is bound to, if any. */
  target: string | null;
  target_label: string | null;
  outcome: MidiOutcome;
  /** The sentence belonging to the outcome, where there is one. */
  note: string | null;
  /** True for the steady stream a knob produces. */
  continuous: boolean;
  /** The port the event came from. */
  port: string | null;
  /** `device:` of that port's profile. */
  device: string | null;
}

/** One MIDI input the system has right now. */
export interface MidiPortView {
  index: number;
  name: string;
  /** `device:` of the profile that matches this port, or null. */
  profile: string | null;
  bindings: number;
  connected: boolean;
}

/**
 * How a known device is doing.
 * - `connected`: open, its bindings work.
 * - `missing`: not plugged in.
 * - `disconnected`: plugged in, disconnected by hand (not reconnected until replug or "Connect").
 * - `unavailable`: plugged in, but could not be opened (Windows: another program holds it).
 * - `error`: the profile file does not parse; it is left untouched until fixed.
 */
export type MidiDeviceState = "connected" | "missing" | "disconnected" | "unavailable" | "error";

/** One known controller - one profile file. */
export interface MidiDeviceView {
  /** `device:` of the profile. Bindings refer to their device by this. */
  device: string;
  path: string;
  state: MidiDeviceState;
  connected: boolean;
  port: string | null;
  bindings: number;
  /** Bindings that work right now. */
  active: number;
  inactive: number;
  /** Parse error of the file, or why the port could not be opened. */
  error: string | null;
  /** A change could not be written (see `save_error`). */
  unsaved: boolean;
  /** Ready-made line: "connected - 12 bindings active", "not plugged in - 3 bindings inactive". */
  summary: string;
  /** Milliseconds since 1970 when the device was last seen connected (since app start). */
  last_seen: number | null;
}

/**
 * Whether one binding works right now.
 * - `active`: device connected, target exists.
 * - `unchecked`: device connected, engine not running - nothing to check the target against yet.
 * - `device_missing`: its controller is not connected.
 * - `target_missing`: the target does not exist in this session (track 5 with 2 tracks, no score).
 * - `overridden`: the loaded score binds the same control to something else.
 */
export type MidiBindingState = "active" | "unchecked" | "device_missing" | "target_missing" | "overridden";

/** `active` and `unchecked` can do their job; everything else cannot. */
export function bindingWorks(state: MidiBindingState): boolean {
  return state === "active" || state === "unchecked";
}

/** One binding, with everything needed to show it, edit it and say whether it works. */
export interface MidiBindingView {
  id: string;
  short: string;
  channel: number;
  kind: "note" | "cc" | "bend";
  number: number;
  /** The target as written in the file (`track.vocals.record` or `track.1.record`). */
  address: string;
  /**
   * The same with the track as a position - the spelling `data-midi` uses. Null while a track name
   * cannot be resolved. Match elements by `resolved_address ?? address`.
   */
  resolved_address: string | null;
  label: string;
  describe: string;
  /** From the loaded score rather than from a controller profile. */
  from_score: boolean;
  /** `device:` of the profile it belongs to; null for a score binding. */
  device: string | null;
  /** Which options apply: `mode` for trigger/switch, `takeover`/`min`/`max` for range. */
  control: "trigger" | "switch" | "range";
  mode: string | null;
  /** The modes this target allows. */
  modes: string[];
  takeover: "pickup" | "jump" | null;
  min: number | null;
  max: number | null;
  /** The target's full range (for a range). */
  range_min: number | null;
  range_max: number | null;
  state: MidiBindingState;
  /** Why it does not work, for anything but `active`. */
  reason: string | null;
}

/** The options `midi_edit` replaces at once. `null` = the target's default. */
export interface MidiBindingOptions {
  mode: string | null;
  takeover: string | null;
  min: number | null;
  max: number | null;
}

/** Everything about the MIDI side that is not a single event. */
export interface MidiView {
  /** At least one device connected. */
  connected: boolean;
  /** First connected port / its device / its file (older single-device fields). */
  port: string | null;
  device: string | null;
  path: string | null;
  /** A change could not be written. Normally false: every change is saved at once. */
  dirty: boolean;
  /** The address the learn mode is waiting for, or null. */
  learning: string | null;
  learning_label: string | null;
  /** Every binding of every known profile, then the score's. */
  bindings: MidiBindingView[];
  /** Every known profile, connected or not. */
  devices: MidiDeviceView[];
  /** Every MIDI input the system has right now. */
  ports: MidiPortView[];
  /** The folder holding the profiles. */
  dir: string;
  /** What the binding states were checked against. */
  session: { running: boolean; tracks: string[]; sections: number | null };
  /** One line per control the score took over from a profile. */
  notes: string[];
  message: string | null;
  /** Why the last save failed, if it did. */
  save_error: string | null;
  dropped: number;
}

/** What arrives on `looper://midi`. */
export interface MidiFeed {
  events: MidiEventReport[];
  view: MidiView | null;
}

export interface MidiSaved {
  path: string;
  message: string;
  view: MidiView;
}

export const EMPTY_MIDI_VIEW: MidiView = {
  connected: false,
  port: null,
  device: null,
  path: null,
  dirty: false,
  learning: null,
  learning_label: null,
  bindings: [],
  devices: [],
  ports: [],
  dir: "",
  session: { running: false, tracks: [], sections: null },
  notes: [],
  message: null,
  save_error: null,
  dropped: 0,
};
