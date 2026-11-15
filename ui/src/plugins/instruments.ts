//! Instrument plugins: which instrument plays which track (by track name), its window, and
//! "all notes off". Commands of `app/src-tauri/src/instruments.rs`, plus a small store fed by
//! `looper://instruments` - the same shape as `store.ts` for the insert slots.

import { useSyncExternalStore } from "react";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import type { PluginFormat } from "./types";

/** One track's instrument. */
export interface InstrumentView {
  /** Track name - instruments belong to names, not positions. */
  track: string;
  /** 0-based engine index while the track runs as an instrument track. */
  index: number | null;
  plugin_id: string;
  name: string;
  vendor: string;
  format: PluginFormat;
  /** Frames the instrument reports; the recording is taken back by it. */
  latency: number;
  has_editor: boolean;
  editor_open: boolean;
  /** Installed in the running engine. */
  active: boolean;
  blocks: number;
  peak: number;
  cpu_us: number;
  error: string | null;
}

export interface InstrumentRack {
  supported: boolean;
  engine_running: boolean;
  instruments: InstrumentView[];
}

type InstrumentEvent = { kind: "rack"; rack: InstrumentRack } | { kind: "notice"; message: string };

const EMPTY: InstrumentRack = { supported: true, engine_running: false, instruments: [] };

async function call<T>(command: string, args?: Record<string, unknown>): Promise<T> {
  try {
    return await invoke<T>(command, args);
  } catch (e) {
    throw new Error(typeof e === "string" ? e : e instanceof Error ? e.message : String(e));
  }
}

export const instrumentApi = {
  rack: () => call<InstrumentRack>("instrument_rack"),
  /** Put an instrument on a track, or take it off with `null`. Applies at once, engine or not. */
  set: (track: string, pluginId: string | null) =>
    call<InstrumentRack>("instrument_set", { track, plugin_id: pluginId }),
  editor: (track: string, open: boolean) => call<InstrumentRack>("instrument_editor", { track, open }),
  /** "All notes off" on one track's instrument, or on all of them. */
  panic: (track: string | null) => call<void>("instrument_panic", { track }),
};

interface State {
  rack: InstrumentRack;
  notice: string | null;
}

let state: State = { rack: EMPTY, notice: null };
const listeners = new Set<() => void>();
let started = false;

function set(patch: Partial<State>): void {
  state = { ...state, ...patch };
  for (const l of listeners) l();
}

export function setInstruments(rack: InstrumentRack): void {
  set({ rack });
}

function start(): void {
  if (started || typeof window === "undefined" || !("__TAURI_INTERNALS__" in window)) return;
  started = true;
  void listen<InstrumentEvent>("looper://instruments", (e) => {
    if (e.payload.kind === "rack") setInstruments(e.payload.rack);
    else set({ notice: e.payload.message });
  });
  instrumentApi.rack().then(setInstruments, () => {});
}

function subscribe(listener: () => void): () => void {
  start();
  listeners.add(listener);
  return () => {
    listeners.delete(listener);
  };
}

export function useInstruments(): State {
  return useSyncExternalStore(subscribe, () => state);
}

/** The instrument of a track, by name. */
export function instrumentOf(rack: InstrumentRack, track: string): InstrumentView | undefined {
  return rack.instruments.find((i) => i.track === track);
}

/** "All channels" or "Ch 3", "all keys" or "C2–C4": how a route reads. */
export function noteName(n: number): string {
  const names = ["C", "C#", "D", "D#", "E", "F", "F#", "G", "G#", "A", "A#", "B"];
  return `${names[n % 12]}${Math.floor(n / 12) - 1}`;
}
