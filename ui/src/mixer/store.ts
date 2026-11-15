//! What the mixer looks like right now: open or closed, how tall, docked or full, which strips,
//! and which channel the detail panel shows. Remembered across restarts (per machine, in
//! `localStorage`) - it is a workspace preference, not part of a song.

import { useSyncExternalStore } from "react";
import { currentStatus } from "../status";
import type { BusId } from "../types";

export type MixerShow = "all" | "buses";

export interface MixerState {
  open: boolean;
  /** Fills the whole editor area instead of sharing it with the arrangement. */
  full: boolean;
  /** Height of the docked mixer in pixels. */
  height: number;
  show: MixerShow;
  /** The channel the detail panel shows, 0-based. */
  selected: number;
  /** A bus the panel shows instead of a channel (its inserts), or null. */
  bus: BusId | null;
}

const KEY = "looper.mixer";
export const MIN_HEIGHT = 300;
/** Room for the device column of a strip, its buses and a usable fader. */
export const DEFAULT_HEIGHT = 480;

function load(): MixerState {
  const base: MixerState = { open: false, full: false, height: DEFAULT_HEIGHT, show: "all", selected: 0, bus: null };
  try {
    const saved = JSON.parse(localStorage.getItem(KEY) ?? "null") as Partial<MixerState> | null;
    return saved ? { ...base, ...saved } : base;
  } catch {
    return base;
  }
}

let state = load();
const listeners = new Set<() => void>();

export function setMixer(patch: Partial<MixerState>): void {
  state = { ...state, ...patch };
  try {
    localStorage.setItem(KEY, JSON.stringify(state));
  } catch {
    /* a preference, not worth an error */
  }
  for (const l of listeners) l();
}

export function mixerState(): MixerState {
  return state;
}

export function useMixer(): MixerState {
  return useSyncExternalStore(
    (l) => {
      listeners.add(l);
      return () => listeners.delete(l);
    },
    () => state,
  );
}

/** Select a channel - from a strip, from the arrangement, from anywhere. */
export function selectChannel(track: number): void {
  if (state.selected !== track || state.bus !== null) setMixer({ selected: track, bus: null });
}

/** Show a bus in the panel - from its strip. */
export function selectBus(bus: BusId): void {
  if (state.bus !== bus) setMixer({ bus });
}

/**
 * Follow a track chosen elsewhere (the arrangement grid) by its name - the score's track list and
 * the engine's agree on names, not necessarily on positions. Unknown names are ignored.
 */
export function selectChannelByName(name: string | null | undefined): void {
  if (!name) return;
  const i = currentStatus().tracks.findIndex((t) => t.name === name);
  if (i >= 0) selectChannel(i);
}
