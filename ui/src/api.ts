//! Every call into the Rust side.
//!
//! There is no HTTP any more: the engine lives in the same process and is reached through Tauri's
//! `invoke`. A rejected command carries a ready-made German sentence as its payload (see
//! app/src-tauri/src/main.rs), so the only job here is to unwrap that payload into an `Error` the
//! existing banner can print.

import { invoke } from "@tauri-apps/api/core";
import type {
  AppInfo,
  BandKindName,
  BusId,
  CalibrateOutcome,
  CompileOutcome,
  DelayNoteName,
  DeviceReport,
  EngineInfo,
  FxParamName,
  FxPresetName,
  FxSlotName,
  Quantize,
  ScoreLoaded,
  StartConfig,
  TrackSourceId,
} from "./types";
import type { MidiPortView, MidiSaved, MidiView } from "./midi/types";

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
  /**
   * Which output buses one source of a track is heard on.
   *
   * `source` is `loop` (the recorded layers) or `monitor` (the live input while monitoring is on).
   * A bus assignment is an output decision - it cannot reach a layer buffer - so it stays available
   * while a take is running and while a score is playing.
   */
  trackBus: (track: number, source: TrackSourceId, bus: BusId, on: boolean) =>
    call<void>("track_bus", { track, source, bus, on }),

  /**
   * What this track subtracts while recording, in frames.
   *
   * `measured` is the loopback value for this input, or null to follow the engine's default;
   * `trim` is the manual surcharge on top, for the part of the way no measurement can see (a
   * plugin host's own buffer). The two stay apart because a calibration overwrites the first and
   * must never lose the second. Takes effect for what is recorded from now on.
   */
  trackLatency: (track: number, measured: number | null, trim: number) =>
    call<void>("track_latency", { track, measured, trim }),

  // --- effects (same zero-based track index) --------------------------------
  // Values are clamped by the engine to a range that makes sense, so a slider cannot produce
  // something unusable; only a missing or out-of-range band is refused.
  /** Whole chain in or out of the signal path. Out is a bit-identical pass-through. */
  fxBypass: (track: number, on: boolean) => call<void>("fx_bypass", { track, on }),
  /** One effect on or off. */
  fxEnable: (track: number, effect: FxSlotName, on: boolean) =>
    call<void>("fx_enable", { track, effect, on }),
  /** Load a ready-made chain. The one command that matters on stage. */
  fxPreset: (track: number, preset: FxPresetName) => call<void>("fx_preset", { track, preset }),
  /** One numeric knob by name; the three band-scoped names additionally need `band`, zero-based. */
  fxSet: (track: number, param: FxParamName, value: number, band?: number) =>
    call<void>("fx_set", { track, param, value, band: band ?? null }),
  /** What one EQ band does. `band` is zero-based. */
  fxBandKind: (track: number, band: number, kind: BandKindName) =>
    call<void>("fx_band_kind", { track, band, kind }),
  /** The delay is tempo-synchronous, so what it takes is a note value, not a millisecond count. */
  fxDelayNote: (track: number, note: DelayNoteName) => call<void>("fx_delay_note", { track, note }),

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
  /**
   * Volume of one output bus, 0 to 4. Half the reason there are two: the headphones can be turned
   * down without the room changing.
   */
  busGain: (bus: BusId, gain: number) => call<void>("bus_gain", { bus, gain }),
  /**
   * Which device output channels one bus leaves on. `channel` is one-based; `width` is 2 for a
   * stereo pair and 1 for a single channel - the stopgap on a two-output interface (main on 1,
   * monitor on 2, split with a Y-cable).
   */
  busOutput: (bus: BusId, channel: number, width: number) =>
    call<void>("bus_output", { bus, channel, width }),
  /** Makes sound and takes over the device. Only from a stopped engine. */
  calibrate: (config: Record<string, unknown>) => call<CalibrateOutcome>("calibrate", { config }),
};

// ---------------------------------------------------------------------------------------------
// The score - phase 3
// ---------------------------------------------------------------------------------------------
//
// Two things here are unlike the rest of this file.
//
// * `compile` reaches nothing: no device, no engine, not even the audio thread. That is what lets
//   the editor call it while somebody types, with the engine stopped or with a score playing.
// * The four transport calls resolve with a **German sentence**, and that sentence is the answer,
//   not a nicety. The runner does not fail: `goto` while a change is armed reports how far the
//   armed change still is and sends not one command. A caller that ignores the string has silently
//   thrown away the only feedback there is.

// ---------------------------------------------------------------------------------------------
// MIDI - phase 5
// ---------------------------------------------------------------------------------------------
//
// Incoming events are **not** here. They arrive on the `looper://midi` event and go into
// `midi/store.ts`; what these commands do is change something and hand back the new view, which is
// the same object that event carries. So a caller may either use the answer or wait for the event -
// both are the same picture, and the store takes whichever comes first.

export const midiApi = {
  /** Every MIDI input, with the profile that belongs to it. Opens nothing. */
  ports: () => call<MidiPortView[]>("midi_ports"),
  /** Open a port by number or by part of its name, and load its profile. */
  open: (device: string) => call<MidiView>("midi_open", { device }),
  close: () => call<MidiView>("midi_close"),
  /**
   * Arm the learn mode for one address - the dotted path of the clicked control. `null` cancels.
   * The address is parsed on the Rust side, so a wrong one comes back as a German sentence.
   */
  learn: (address: string | null) => call<MidiView>("midi_learn", { address }),
  /** Take a control's job away. `id` is what the overview shows: `ch1.note36`. */
  unbind: (id: string) => call<MidiView>("midi_unbind", { id }),
  /** Write the profile. The answer names the file. */
  save: () => call<MidiSaved>("midi_save"),
  /** For a frontend that just started and missed the events so far. */
  state: () => call<MidiView>("midi_state"),
};

export const scoreApi = {
  /**
   * Translate a score without loading it. `ok` decides; `errors` carries **every** problem at
   * once, each with line, column, message and often a suggestion.
   */
  compile: (yaml: string) => call<CompileOutcome>("score_compile", { yaml }),
  /**
   * Compile and hand to the runner. Needs a running engine whose tracks match the score and every
   * track empty; tempo, time signature and the layer length then come from the score.
   */
  load: (yaml: string, countIn?: number) =>
    call<ScoreLoaded>("score_load", { yaml, count_in: countIn ?? null }),
  /** Count-in, then the first section. */
  start: () => call<string>("score_start"),
  /** The release button. Pressing it twice skips nothing. */
  next: () => call<string>("score_next"),
  /** Jump to a section, zero-based. Refused while a change is armed - the answer says so. */
  goto: (section: number) => call<string>("score_goto", { section }),
  /** Stop every track and end the run. */
  stopAll: () => call<string>("score_stop_all"),
};
