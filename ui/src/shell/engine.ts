//! The engine, as the window sees it: a technical detail the app takes care of.
//!
//! The window sends its setup to the Rust side at startup and after every change
//! (`api.engineConfigure`); the first of those is when the engine starts by itself (Setup ›
//! General: "Start the engine when the app opens"). Whatever needs the engine later - Load, Start,
//! Record, a MIDI pad - starts it on the Rust side if it is not running yet (`supervisor.rs`).
//! This store holds what the driver opened and where the engine stands (`EngineStateView`), so the
//! status pill, the banners and the Setup page agree. Start and Stop remain as buttons for whoever
//! wants them.

import { useSyncExternalStore } from "react";
import { listen } from "@tauri-apps/api/event";
import { api, insideTauri } from "../api";
import { currentConfig, subscribeConfig } from "../setup/config";
import { fail, note } from "./notes";
import { T } from "./terms";
import type { EngineInfo, EngineStateView, StartConfig } from "../types";

interface EngineState {
  info: EngineInfo | null;
  busy: boolean;
  /** Where the engine stands; null until the Rust side has answered. */
  view: EngineStateView | null;
}

let state: EngineState = { info: null, busy: false, view: null };
const listeners = new Set<() => void>();

function set(next: Partial<EngineState>): void {
  state = { ...state, ...next };
  for (const l of listeners) l();
}

export function currentEngine(): EngineState {
  return state;
}

export function useEngine(): EngineState {
  return useSyncExternalStore(
    (l) => {
      listeners.add(l);
      return () => listeners.delete(l);
    },
    () => state,
  );
}

// ---------------------------------------------------------------------------------------------
// Preferences: kept in the window, sent along with the setup
// ---------------------------------------------------------------------------------------------

export interface EnginePrefs {
  /** Start the engine as soon as the app opens. */
  autostart: boolean;
  /** A score that was loaded when the app was closed is loaded again after the start. */
  reloadScore: boolean;
}

const PREFS_KEY = "looper.engine";
const DEFAULT_PREFS: EnginePrefs = { autostart: true, reloadScore: true };

function loadPrefs(): EnginePrefs {
  try {
    const raw = localStorage.getItem(PREFS_KEY);
    return raw ? { ...DEFAULT_PREFS, ...(JSON.parse(raw) as Partial<EnginePrefs>) } : DEFAULT_PREFS;
  } catch {
    return DEFAULT_PREFS;
  }
}

let prefs: EnginePrefs = loadPrefs();
const prefListeners = new Set<() => void>();

export function enginePrefs(): EnginePrefs {
  return prefs;
}

export function setEnginePrefs(change: Partial<EnginePrefs>): void {
  prefs = { ...prefs, ...change };
  try {
    localStorage.setItem(PREFS_KEY, JSON.stringify(prefs));
  } catch {
    /* a blocked storage only costs the setting */
  }
  for (const l of prefListeners) l();
  scheduleConfigure();
}

export function useEnginePrefs(): EnginePrefs {
  return useSyncExternalStore(
    (l) => {
      prefListeners.add(l);
      return () => prefListeners.delete(l);
    },
    () => prefs,
  );
}

// ---------------------------------------------------------------------------------------------
// The setup goes to the Rust side
// ---------------------------------------------------------------------------------------------

/** The score that will be loaded again after the start; its tracks are what the stage shows. */
let preview: string | null = null;
let timer: number | undefined;
/** Nothing is sent before the frame has booted the engine: the first message is the boot. */
let booted = false;

export function setPreviewScore(yaml: string | null): void {
  preview = yaml;
}

function configureNow(): Promise<void> {
  window.clearTimeout(timer);
  timer = undefined;
  return api
    .engineConfigure(currentConfig(), prefs.autostart, preview)
    .then((view) => set({ view }))
    .catch(() => undefined);
}

/** Send the setup a moment after the last change - a name typed letter by letter is one message. */
function scheduleConfigure(): void {
  if (!booted) return;
  window.clearTimeout(timer);
  timer = window.setTimeout(() => void configureNow(), 250);
}

/**
 * Hand the setup to the Rust side for the first time - from here on the engine may start by
 * itself. Called once by the frame, after anything that shapes the first start (the score to load
 * again) is known.
 */
export function bootEngine(): Promise<void> {
  booted = true;
  return configureNow();
}

if (insideTauri()) {
  // Loading a score with other tracks rebuilds the engine on the Rust side, and a start on demand
  // happens there too; both announce the device facts on `looper://engine`.
  void listen<EngineInfo>("looper://engine", (e) => set({ info: e.payload }));
  void listen<EngineStateView>("looper://engine-state", (e) => {
    const view = e.payload;
    set({ view, busy: view.phase === "starting", ...(view.phase === "running" ? {} : { info: null }) });
  });
  subscribeConfig(scheduleConfigure);
  // A reload of the page (or a dev hot reload) while the engine keeps running would otherwise lose
  // the device facts until the next start.
  api
    .engineInfo()
    .then((info) => {
      if (info && !state.info) set({ info });
    })
    .catch(() => undefined);
  api
    .engineState()
    .then((view) => set({ view }))
    .catch(() => undefined);
}

// ---------------------------------------------------------------------------------------------
// The buttons
// ---------------------------------------------------------------------------------------------

function opened(info: EngineInfo): void {
  set({ info });
  note(
    `Engine running: ${info.host}, ${info.input_device}, ${info.sample_rate} Hz, ` +
      `${info.driver_input_frames ?? info.buffer_frames} frames, latency ${info.latency_ms.toFixed(2)} ms.`,
  );
}

function busyWhile(work: () => Promise<EngineInfo>): Promise<boolean> {
  set({ busy: true });
  return work()
    .then((info) => {
      opened(info);
      return true;
    })
    .catch((e) => {
      fail((e as Error).message);
      return false;
    })
    .finally(() => set({ busy: false }));
}

export function startEngine(config: StartConfig): Promise<boolean> {
  return busyWhile(() => api.engineStart(config));
}

/** Rebuild with the current setup (a loaded score comes along). */
export function restartEngine(): Promise<boolean> {
  return configureNow().then(() => busyWhile(() => api.engineRestart()));
}

/** "Use … now" while waiting for an interface. */
export function useFallbackDevice(): Promise<boolean> {
  return busyWhile(() => api.engineUseFallback());
}

export function stopEngine(): void {
  set({ busy: true });
  api
    .engineStop()
    .then(() => set({ info: null }))
    .catch((e) => fail((e as Error).message))
    .finally(() => set({ busy: false }));
}

export function unmuteMonitoring(): void {
  api.engineUnmute().catch((e) => fail((e as Error).message));
}

/**
 * Leaving Setup: what changed goes to the Rust side now, and a change only a restart applies is
 * applied - at once when nothing is recorded, after asking (the band under the title bar) when
 * loops would go.
 */
export function applySetupChanges(hasLoops: boolean): Promise<void> {
  return configureNow().then(() => {
    if (state.view?.phase === "running" && state.view.restart_needed && !hasLoops) {
      return restartEngine().then(() => undefined);
    }
  });
}

/** The words of the status pill for where the engine stands; null while it runs. */
export function engineWords(view: EngineStateView | null): string | null {
  switch (view?.phase ?? "off") {
    case "running":
      return null;
    case "starting":
      return T.engineStarting;
    case "waiting":
      return `Waiting for ${view?.missing ?? "the interface"}`;
    case "failed":
      return "Engine could not start";
    default:
      return T.engineOff;
  }
}
