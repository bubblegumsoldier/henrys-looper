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
//! the learn mode is armed, and a typo comes back as the German sentence that names the list. This
//! file exists so the strings are built in one place rather than typed out at forty call sites, not
//! as a second source of truth.
//!
//! Everything is **1-based on the outside**, on every level: `track.1` is `tracks[0]`, `layer.2` is
//! `layers[1]`, `eq.3` is `bands[2]`. The indices this file is called with are the 0-based ones the
//! status event uses, and the conversion happens here - once.

import type { BusId, DelayNoteName, FxSlotName, TrackSourceId } from "../types";
import type { LoadablePreset } from "../live/FxRow";

/** The score's transport. Available while a score runs - it *is* the score's transport. */
export const TRANSPORT = {
  start: "transport.start",
  next: "transport.next",
  stopAll: "transport.stop_all",
} as const;

/** Jump to a section. `section` is the 0-based index the block preview uses. */
export function gotoAddress(section: number): string {
  return `transport.goto.${section + 1}`;
}

export const GLOBAL = {
  click: "global.click",
  clearAll: "global.clear_all",
  tempo: "global.tempo",
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
  | "monitor"
  | "pan"
  | "latency_trim";

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
};

export function fxPresetAddress(track: number, preset: LoadablePreset): string {
  return fxAddress(track, `preset.${PRESET_ADDRESS[preset]}`);
}

/** One EQ band's knob. `band` is 0-based, as `BandEvent.index` is. */
export function fxBandAddress(track: number, band: number, part: "hz" | "q" | "gain"): string {
  return fxAddress(track, `eq.${band + 1}.${part}`);
}
