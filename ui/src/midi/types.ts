//! The wire format of the MIDI side, mirroring `app/src-tauri/src/midi.rs`.
//!
//! Two things travel: single **events** (a pad was pressed, a knob moved, and what the mapping made
//! of it) and the **view** (what is connected, what is bound, what the learn mode waits for). Both
//! arrive on the `looper://midi` event; the view is only attached when it changed.

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
  /** German word for the kind: `Note an`, `Note aus`, `Regler`, `Pitchbend`. */
  what: string;
  /** Note or controller number; null for a pitch bend, which has none. */
  number: number | null;
  /** Velocity, controller value, or the bend around zero. */
  value: number;
  /** The address this control is bound to, if any. */
  target: string | null;
  /** Its German name. */
  target_label: string | null;
  outcome: MidiOutcome;
  /** The German sentence belonging to the outcome, where there is one. */
  note: string | null;
  /** True for the steady stream a knob produces. */
  continuous: boolean;
}

/** One MIDI input, with the profile that would be loaded for it. */
export interface MidiPortView {
  index: number;
  name: string;
  profile: string | null;
  bindings: number;
}

/** One entry of the profile overview. */
export interface MidiBindingView {
  id: string;
  short: string;
  channel: number;
  kind: "note" | "cc" | "bend";
  number: number;
  address: string;
  /** German name of the target. */
  label: string;
  /** The same plus the button behaviour or the range. */
  describe: string;
  /** True when the binding comes from the loaded score rather than from the controller profile. */
  from_score: boolean;
}

/** Everything about the MIDI side that is not a single event. */
export interface MidiView {
  connected: boolean;
  port: string | null;
  device: string | null;
  path: string | null;
  dirty: boolean;
  /** The address the learn mode is waiting for, or null. */
  learning: string | null;
  learning_label: string | null;
  bindings: MidiBindingView[];
  /** One German line per control the score took over from the profile. */
  notes: string[];
  message: string | null;
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
  notes: [],
  message: null,
  dropped: 0,
};
