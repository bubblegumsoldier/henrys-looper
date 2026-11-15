//! The MIDI hub: one place that holds the connection, the mapping and the last few events.
//!
//! Deliberately the same shape as `status.ts`, and deliberately a **second** stream. The status
//! event is a twenty-per-second sampling of a continuous state and older snapshots are thrown away;
//! MIDI is a rare, bursty stream of discrete facts where a lost one is a pad press the monitor
//! never shows. See the module comment of `app/src-tauri/src/midi.rs` for the whole argument.
//!
//! What lives here and not in React state: the learn mode. It is read by a document-level listener
//! (`LearnLayer.tsx`), by the badge decoration and by the panel, and lifting it into a component
//! would mean either a context around the whole app or the same boolean in three places.

import { useSyncExternalStore } from "react";
import { midiApi } from "../api";
import { EMPTY_MIDI_VIEW, type MidiBindingView, type MidiEventReport, type MidiFeed, type MidiView } from "./types";

/** Events kept for the monitor. Enough to see a bank of pads being pressed one after the other. */
const MAX_EVENTS = 60;

type Listener = () => void;

const listeners = new Set<Listener>();

let view: MidiView = EMPTY_MIDI_VIEW;
let events: MidiEventReport[] = [];
let learnMode = false;
/**
 * The address the binding list and the controls point at together: hovering or clicking a row
 * outlines its control, hovering a control in learn mode lights up its row. One value for both
 * directions, because only one thing can be under the pointer.
 */
let focus: string | null = null;
/**
 * `address -> bindings`, rebuilt whenever the view changes. What the badges are drawn from.
 *
 * Keyed by `resolved_address ?? address`, because that is the spelling a `data-midi` attribute
 * uses: a score binding written as `track.vocals.record` belongs to the button `track.1.record`.
 * A list, because one control may be bound several times - a pad *and* a foot switch for the same
 * record button.
 */
let byAddress = new Map<string, MidiBindingView[]>();

function announce(): void {
  for (const l of listeners) l();
}

function subscribe(listener: Listener): () => void {
  listeners.add(listener);
  return () => {
    listeners.delete(listener);
  };
}

// ---------------------------------------------------------------------------------------------
// Writing
// ---------------------------------------------------------------------------------------------

/** A fresh view, from a command's answer or from an event that carried one. */
export function setMidiView(next: MidiView): void {
  view = next;
  byAddress = new Map();
  for (const b of next.bindings) {
    const key = b.resolved_address ?? b.address;
    const list = byAddress.get(key);
    if (list) list.push(b);
    else byAddress.set(key, [b]);
  }
  announce();
}

/** One `looper://midi` payload. */
export function pushMidiFeed(feed: MidiFeed): void {
  if (feed.events.length > 0) {
    // Newest first: the eye starts at the top, and the monitor is read while pressing a pad.
    const at = Date.now();
    events = [...feed.events]
      .reverse()
      .map((e): MidiEventReport => ({ ...e, at }))
      .concat(events)
      .slice(0, MAX_EVENTS);
  }
  if (feed.view) {
    setMidiView(feed.view);
    return;
  }
  if (feed.events.length > 0) announce();
}

/**
 * Say something about the MIDI side without a round trip - a refusal that came back from a command,
 * usually. It lands where the engine's own sentences land, in the sidebar's message line.
 */
export function sayMidi(message: string): void {
  view = { ...view, message };
  announce();
}

export function clearMidiEvents(): void {
  events = [];
  announce();
}

/** Point at one address from the list or from a control (null = nothing). */
export function setMidiFocus(address: string | null): void {
  if (focus === address) return;
  focus = address;
  announce();
}

/**
 * Turn the learn mode on or off.
 *
 * **A switch, not a modifier key.** Ctrl-click works too (see `LearnLayer.tsx`), but the switch is
 * the one that is offered: with Ctrl one clicks *next to* the button often enough, and next to
 * "Record" is "Record" - a missed modifier would start a take instead of learning a pad.
 */
export function setLearnMode(on: boolean): void {
  if (learnMode === on) return;
  learnMode = on;
  if (!on) focus = null;
  announce();
}

/**
 * Leave the learn mode, and take back a pending target with it: the Rust side would otherwise keep
 * waiting, and the next pad pressed for a completely different reason would be bound to whatever
 * was clicked last.
 */
export function leaveLearnMode(): void {
  setLearnMode(false);
  if (view.learning) {
    midiApi
      .learn(null)
      .then(setMidiView)
      .catch((e) => sayMidi((e as Error).message));
  }
}

/** Fetch whatever the Rust side already knows - connections survive a reload of the web view. */
export function loadMidiView(): void {
  midiApi
    .state()
    .then(setMidiView)
    .catch(() => {
      // No engine behind this window (a browser tab on the dev server).
    });
}

// ---------------------------------------------------------------------------------------------
// Reading
// ---------------------------------------------------------------------------------------------

export function currentMidiView(): MidiView {
  return view;
}

export function isLearnMode(): boolean {
  return learnMode;
}

/** Everything bound to an address (empty when nothing is). Used by the badges and by the panel. */
export function bindingsFor(address: string): MidiBindingView[] {
  return byAddress.get(address) ?? [];
}

export function useMidiView(): MidiView {
  return useSyncExternalStore(subscribe, currentMidiView);
}

export function useMidiEvents(): MidiEventReport[] {
  return useSyncExternalStore(subscribe, () => events);
}

export function useLearnMode(): boolean {
  return useSyncExternalStore(subscribe, isLearnMode);
}

export function currentMidiFocus(): string | null {
  return focus;
}

export function useMidiFocus(): string | null {
  return useSyncExternalStore(subscribe, currentMidiFocus);
}

// A way to drive the MIDI side from the console while `npm run dev` runs, so the learn mode, the
// badges and the monitor can be looked at without a controller on the desk. Stripped from the
// build, exactly like `window.pushStatus`.
if (import.meta.env.DEV) {
  const w = window as unknown as {
    pushMidi?: typeof pushMidiFeed;
    setMidiView?: typeof setMidiView;
  };
  w.pushMidi = pushMidiFeed;
  w.setMidiView = setMidiView;
}
