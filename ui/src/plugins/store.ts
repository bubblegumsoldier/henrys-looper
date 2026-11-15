//! The plugin hub: catalogue, rack, scan progress and the last notice, fed by `looper://plugins`
//! and by command answers. Same shape as `midi/store.ts`; listens by itself on first use, so no
//! other file has to know about it.

import { useSyncExternalStore } from "react";
import { listen } from "@tauri-apps/api/event";
import { pluginApi } from "./api";
import { EMPTY_RACK, type CatalogView, type ParamValue, type PluginEvent, type RackView } from "./types";

type Listener = () => void;
type ParamListener = (target: string, slot: number, values: ParamValue[]) => void;

interface State {
  rack: RackView;
  catalog: CatalogView | null;
  /** Label of the plugin the scan is probing now, `null` when no scan runs. */
  scanning: string | null;
  notice: string | null;
}

let state: State = { rack: EMPTY_RACK, catalog: null, scanning: null, notice: null };
const listeners = new Set<Listener>();
const paramListeners = new Set<ParamListener>();
let started = false;

function set(patch: Partial<State>): void {
  state = { ...state, ...patch };
  for (const l of listeners) l();
}

export function setRack(rack: RackView): void {
  set({ rack });
}

export function setCatalog(catalog: CatalogView): void {
  set({ catalog, scanning: null });
}

export function setNotice(notice: string | null): void {
  set({ notice });
}

function onEvent(event: PluginEvent): void {
  switch (event.kind) {
    case "rack":
      setRack(event.rack);
      break;
    case "catalog":
      setCatalog(event.catalog);
      break;
    case "scan":
      set({ scanning: event.label });
      break;
    case "notice":
      setNotice(event.message);
      break;
    case "params":
      for (const l of paramListeners) l(event.target, event.slot, event.values);
      break;
  }
}

function start(): void {
  if (started || typeof window === "undefined" || !("__TAURI_INTERNALS__" in window)) return;
  started = true;
  void listen<PluginEvent>("looper://plugins", (e) => onEvent(e.payload));
  pluginApi.rack().then(setRack, () => {});
  pluginApi.catalog().then(setCatalog, () => {});
}

function subscribe(listener: Listener): () => void {
  start();
  listeners.add(listener);
  return () => {
    listeners.delete(listener);
  };
}

/** The whole plugin state; re-renders on every change. */
export function usePlugins(): State {
  return useSyncExternalStore(subscribe, () => state);
}

/** Hear parameter changes made in a plugin's own window (or by MIDI). */
export function onParamChange(listener: ParamListener): () => void {
  start();
  paramListeners.add(listener);
  return () => {
    paramListeners.delete(listener);
  };
}

/** Mark a scan as running before the first progress event arrives. */
export function markScanning(): void {
  set({ scanning: "" });
}
