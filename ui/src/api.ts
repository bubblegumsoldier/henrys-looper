//! Every call into the Rust side.
//!
//! There is no HTTP any more: the engine lives in the same process and is reached through Tauri's
//! `invoke`. A rejected command carries a ready-made German sentence as its payload (see
//! app/src-tauri/src/main.rs), so the only job here is to unwrap that payload into an `Error` the
//! existing banner can print.

import { invoke } from "@tauri-apps/api/core";
import type {
  AppInfo,
  CalibrateOutcome,
  CompileResult,
  DeviceReport,
  EngineInfo,
  LoadResult,
  Quantize,
  StartConfig,
  StateEvent,
} from "./types";

/**
 * `status` is kept for the components written against the old HTTP layer. It is 0 for everything
 * that comes out of `invoke`; there are no status codes behind a command.
 */
export class ApiError extends Error {
  status: number;
  constructor(message: string, status = 0) {
    super(message);
    this.name = "ApiError";
    this.status = status;
  }
}

/** True inside the app window, false in a plain browser tab pointed at the dev server. */
export function insideTauri(): boolean {
  return typeof window !== "undefined" && "__TAURI_INTERNALS__" in window;
}

/** Tauri rejects with whatever the command returned as `Err` - here always a German sentence. */
function sentence(e: unknown): string {
  if (!insideTauri()) {
    return "Diese Oberfläche läuft nicht im Anwendungsfenster, sondern im Browser - dort gibt es keine Engine. Starte sie mit `cargo tauri dev`.";
  }
  if (typeof e === "string") return e;
  if (e instanceof Error) return e.message;
  if (e && typeof e === "object" && "message" in e) return String((e as { message: unknown }).message);
  return "Unbekannter Fehler auf der Rust-Seite.";
}

async function call<T>(command: string, args?: Record<string, unknown>): Promise<T> {
  try {
    return await invoke<T>(command, args);
  } catch (e) {
    throw new ApiError(sentence(e));
  }
}

export const api = {
  // --- setup ---------------------------------------------------------------
  appInfo: () => call<AppInfo>("app_info"),
  listDevices: () => call<DeviceReport>("list_devices"),
  engineStart: (config: StartConfig) => call<EngineInfo>("engine_start", { config }),
  engineStop: () => call<void>("engine_stop"),

  // --- per track (zero-based index into the status event's `tracks`) --------
  trackRecord: (track: number) => call<void>("track_record", { track }),
  trackOverdub: (track: number) => call<void>("track_overdub", { track }),
  trackStop: (track: number) => call<void>("track_stop", { track }),
  trackPlay: (track: number) => call<void>("track_play", { track }),
  trackClear: (track: number) => call<void>("track_clear", { track }),
  trackMonitor: (track: number, on: boolean) => call<void>("track_monitor", { track, on }),
  /** Where the track sits between the speakers: -1 hard left, 0 centre, +1 hard right. */
  trackPan: (track: number, pan: number) => call<void>("track_pan", { track, pan }),

  // --- per layer -----------------------------------------------------------
  layerMute: (track: number, layer: number, muted: boolean) =>
    call<void>("layer_mute", { track, layer, muted }),
  layerRemove: (track: number, layer: number) => call<void>("layer_remove", { track, layer }),
  layerGain: (track: number, layer: number, gain: number) =>
    call<void>("layer_gain", { track, layer, gain }),

  // --- global --------------------------------------------------------------
  clearAll: () => call<void>("clear_all"),
  setClick: (on: boolean) => call<void>("set_click", { on }),
  setTempo: (bpm: number, beatsPerBar: number, beatUnit: number) =>
    call<void>("set_tempo", { bpm, beats_per_bar: beatsPerBar, beat_unit: beatUnit }),
  /** Grid a recording and an overdub snap to. Armed takes keep the position they already have. */
  setQuantize: (quantize: Quantize) => call<void>("set_quantize", { quantize }),
  /** Makes sound and takes over the device. Only from a stopped engine. */
  calibrate: (config: Record<string, unknown>) => call<CalibrateOutcome>("calibrate", { config }),
};

// ---------------------------------------------------------------------------------------------
// Score commands - phase 3
// ---------------------------------------------------------------------------------------------

/** Whether the Rust side understands `compile`, `load` and the transport. It does not, yet. */
export const SCORE_AVAILABLE = false;

export const SCORE_HINT = "Kommt mit der Partitur-Phase (Phase 3) - die Engine kennt diese Kommandos noch nicht.";

function notYet<T>(): Promise<T> {
  return Promise.reject(new ApiError(SCORE_HINT, 501));
}

/**
 * The score editor is written against these. They reject without touching the Rust side, so a
 * component that still calls one gets a single German sentence instead of a retry loop.
 */
export const scoreApi = {
  compile: (_yaml: string) => notYet<CompileResult>(),
  load: (_yaml: string) => notYet<LoadResult>(),
  transport: (_action: "start" | "stop_all" | "next") => notYet<{ ok: boolean; state: StateEvent }>(),
  state: () => notYet<StateEvent>(),
  score: () => notYet<{ yaml: string; score: unknown; stub: boolean }>(),
  engines: () => notYet<{ current: string; connected: boolean; available: string[] }>(),
  setEngine: (_engine: string) => notYet<{ current: string; connected: boolean }>(),
};
