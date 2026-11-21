//! The addresses of the parameter tree, as this user interface spells them.
//!
//! # Why this file is three lines of string building and a page of reasoning
//!
//! Every operable thing in this program has a dotted address (`engine/src/midi/target.rs`), and a
//! learn mode has to know **which address the element the user just clicked stands for**. There
//! were three ways to get that knowledge into the components:
//!
//! 1. Pass an address down as a prop through `LiveView` → `TrackCard` → `FxRow` → `FxSlider`. That
//!    is four signatures changed for a string that every one of those components can already work
//!    out from what it has.
//! 2. Ask the Rust side for the catalogue and match a control to an entry by name. That means a
//!    second, fuzzy mapping between two lists that already agree by construction.
//! 3. **Let each element carry its own address in a `data-midi` attribute**, and have one
//!    delegated listener read it off the DOM (`LearnLayer.tsx`).
//!
//! The third is what this is. A component that renders a button for track `i` writes
//! `data-midi={trackAddress(i, "record")}` next to the `onClick` it already has - one attribute, no
//! new prop, no new component. The badge for a bound control is drawn from the same attribute by
//! CSS, so nothing has to render when a binding changes either.
//!
//! **The addresses are not checked here.** They are parsed by `Target::parse` on the Rust side when
//! the learn mode is armed, and a typo comes back as the sentence that names the list. This
//! file exists so the strings are built in one place rather than typed out at forty call sites, not
//! as a second source of truth.
//!
//! # The contract for a new control (read this before adding one)
//!
//! A control becomes MIDI-learnable by **one attribute and nothing else**:
//!
//! ```tsx
//! <button data-midi={trackAddress(i, "record")} onClick={...}>Record</button>
//! ```
//!
//! * `data-midi` holds the address of the parameter tree (`engine/src/midi/target.rs`;
//!   `looper-engine midi targets` lists them all). Build it with a helper from this file.
//! * The element may be anything clickable or not - button, label around a slider, a span showing
//!   a value. In learn mode a click on it (or ⌘/Ctrl+click anywhere) arms it; the click does not
//!   also operate the control. Disabled controls work too.
//! * It gets, without any code of its own: the badge (`data-midi-key`, e.g. `Pad 36`), whether
//!   that binding works right now (`data-midi-state="active" | "inactive"`), the learn-mode outline
//!   (`midi-learnable`) and the pulse while waiting (`midi-waiting`). Style those, render nothing.
//! * Addresses are 1-based and use **track positions** (`track.1`), never track names: a score
//!   binding written as `track.vocals.record` is matched to the element through its
//!   `resolved_address`, which the backend computes from the running session.
//! * The kinds: a *trigger* fires (record, preset, band shape), a *switch* has on/off (monitor,
//!   effect on, channel mute and solo), a *range* has a value (pan, channel fader
//!   `track.N.gain`, layer gain, fx knobs, click and monitor level, tempo). The address decides
//!   the kind; the element does not have to say.
//! * A drawn control that is not a native input (the mixer's knobs and faders, `mixer/controls`)
//!   carries the attribute on the element that is dragged, so it is the thing that gets the badge.
//! * An address the backend does not know is refused when it is armed, with a sentence. A new kind
//!   of thing to control therefore needs a new `Target` in `target.rs` first.
//!
//! Everything about the bindings themselves (which devices, which controls, works or not, why)
//! comes from the backend as data - `MidiView` in `types.ts` - and is not computed here.
//!
//! Everything is **1-based on the outside**, on every level: `track.1` is `tracks[0]`, `layer.2` is
//! `layers[1]`, `eq.3` is `bands[2]`. The indices this file is called with are the 0-based ones the
//! status event uses, and the conversion happens here - once.

import type { BusId, DelayNoteName, FxSlotName, TrackSourceId } from "../types";
import type { LoadablePreset } from "../mixer/fxModel";

/** The score's transport. Available while a score runs - it *is* the score's transport. */
export const TRANSPORT = {
  start: "transport.start",
  next: "transport.next",
  stopAll: "transport.stop_all",
} as const;

/**
 * Start/stop of free looping. Not part of the score's transport: it needs no score (and is refused
 * while one is loaded). The engine side is `Target::Transport` in `engine/src/midi/target.rs`.
 */
export const FREE_TRANSPORT = "transport.toggle";

/** Jump to a section. `section` is the 0-based index the block preview uses. */
export function gotoAddress(section: number): string {
  return `transport.goto.${section + 1}`;
}

export const GLOBAL = {
  click: "global.click",
  clearAll: "global.clear_all",
  /** Undo the newest take over all tracks - one foot switch, no track to pick. */
  undo: "global.undo",
  tempo: "global.tempo",
  /** Tap tempo - a button, so a foot switch can set the tempo. */
  tapTempo: "global.tap_tempo",
  /** Click volume - on the Monitor bus strip of the mixer. */
  clickGain: "global.click_gain",
  /** Volume of every monitored live input. */
  monitorGain: "global.monitor_gain",
  /** "All notes off" on every instrument. */
  panic: "global.panic",
} as const;

/** Volume of one output bus - the fader that moves the headphones without moving the room. */
export function busGainAddress(bus: BusId): string {
  return `global.bus.${bus}.gain`;
}

/**
 * Whether one source of a track is heard on one bus. One address per source *and* per bus, because
 * that is how it is operated: a switch has a yes and a no.
 */
export function sendAddress(track: number, source: TrackSourceId, bus: BusId): string {
  const field = source === "loop" ? "bus" : "monitor_bus";
  return `track.${track + 1}.${field}.${bus}`;
}

/** The grid a take snaps to. Two separate triggers, because each one is a state to arrive at. */
export function quantizeAddress(quantize: "bar" | "loop"): string {
  return `global.quantize.${quantize}`;
}

/** What a track can do. `track` is the 0-based index of the status event. */
export type TrackPart =
  | "record"
  | "overdub"
  | "play"
  | "stop"
  | "clear"
  | "undo"
  | "monitor"
  | "pan"
  | "gain"
  | "mute"
  | "solo"
  | "latency_trim"
  | "instrument.panic";

export function trackAddress(track: number, part: TrackPart): string {
  return `track.${track + 1}.${part}`;
}

/** One layer of one track. Both indices are 0-based here and 1-based in the address. */
export function layerAddress(track: number, layer: number, part: "mute" | "remove" | "gain"): string {
  return `track.${track + 1}.layer.${layer + 1}.${part}`;
}

/** Anything inside a track's effect chain. `tail` is what follows `fx.`. */
export function fxAddress(track: number, tail: string): string {
  return `track.${track + 1}.fx.${tail}`;
}

/** One effect on or off. The slot names are the ones the engine and the commands already use. */
export function fxSlotAddress(track: number, slot: FxSlotName): string {
  return fxAddress(track, `${slot}.on`);
}

/**
 * The delay's note value. Machine names, because an address cannot contain a slash - `1/8.` is
 * `dotted_eighth` here and in the engine.
 */
export function fxDelayNoteAddress(track: number, note: DelayNoteName): string {
  return fxAddress(track, `delay.note.${note}`);
}

/**
 * A preset is addressed by its **German name**, because that is what `FxPreset::label` prints and
 * what `Target::to_string` therefore writes into a profile. The wire format's `voice` and this
 * `stimme` are the same preset seen from two sides; the mapping lives here so it lives once.
 */
const PRESET_ADDRESS: Record<LoadablePreset, string> = {
  dry: "trocken",
  voice: "stimme",
  piezo_guitar: "gitarre",
  bass_from_guitar: "bass",
};

export function fxPresetAddress(track: number, preset: LoadablePreset): string {
  return fxAddress(track, `preset.${PRESET_ADDRESS[preset]}`);
}

/** One EQ band's shape - a trigger per shape. `band` is 0-based. */
export function fxBandKindAddress(track: number, band: number, kind: "peak" | "low_shelf" | "high_shelf"): string {
  return fxAddress(track, `eq.${band + 1}.${kind}`);
}

/** One EQ band's knob. `band` is 0-based, as `BandEvent.index` is. */
export function fxBandAddress(track: number, band: number, part: "hz" | "q" | "gain"): string {
  return fxAddress(track, `eq.${band + 1}.${part}`);
}

/**
 * Plugin insert slots (`engine::insert`). `owner` is a slot owner as the plugin host spells it -
 * `track.1` … `track.8`, `bus.main`, `bus.monitor` - and `slot` is 0-based. Addresses:
 *
 * * `track.<n>.insert.<slot>.bypass`, `global.bus.<main|monitor>.insert.<slot>.bypass` - switch
 * * `….insert.<slot>.param.<id>` - value 0..1, `id` being the plugin's own parameter id
 *
 * The backend also hands both out ready-made (`SlotView.bypass_address`, `ParamView.address`).
 */
export function insertOwnerAddress(owner: string): string {
  return owner.startsWith("bus.") ? `global.${owner}` : owner;
}

export function insertBypassAddress(owner: string, slot: number): string {
  return `${insertOwnerAddress(owner)}.insert.${slot + 1}.bypass`;
}

export function insertParamAddress(owner: string, slot: number, id: number): string {
  return `${insertOwnerAddress(owner)}.insert.${slot + 1}.param.${id}`;
}
