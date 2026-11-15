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
  FilesInfo,
  FxSlotName,
  MixerSource,
  OpenedFile,
  OverdubMode,
  RecentFile,
  SavedFile,
  TemplateTrack,
  ParseOutcome,
  Quantize,
  RenderOutcome,
  ScoreDoc,
  ScoreLoaded,
  StartConfig,
  TrackSourceId,
} from "./types";
import type { MidiBindingOptions, MidiPortView, MidiProfileAction, MidiSaved, MidiView } from "./midi/types";

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

/** Tauri rejects with whatever the command returned as `Err` - here always a whole sentence. */
function sentence(e: unknown): string {
  if (!insideTauri()) {
    return "This window is running in a browser, not in the app window - there is no engine here. Start the app with `cargo tauri dev`.";
  }
  if (typeof e === "string") return e;
  if (e instanceof Error) return e.message;
  if (e && typeof e === "object" && "message" in e) return String((e as { message: unknown }).message);
  return "Unknown error on the Rust side.";
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
  /** What the running engine opened, or null - asked once when the page (re)loads. */
  engineInfo: () => call<EngineInfo | null>("engine_info"),
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
   * The channel fader, linear 0 to `MAX_TRACK_GAIN` (+12 dB). After the effect chain and the
   * plugin inserts, before the panner; smoothed by the engine, so a jump does not click.
   */
  trackGain: (track: number, gain: number) => call<void>("track_gain", { track, gain }),
  /** Channel mute, both buses. Listening only - recording stays dry. */
  trackMute: (track: number, on: boolean) => call<void>("track_mute", { track, on }),
  /** Channel solo. While any track is soloed, every other one is silent on both buses. */
  trackSolo: (track: number, on: boolean) => call<void>("track_solo", { track, on }),

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
  /**
   * Start (`true`), stop (`false`) or toggle (`null`) the clock of free looping. Stopping keeps the
   * loops; starting plays them from bar 1. Refused while a score is loaded.
   */
  transport: (run: boolean | null) => call<void>("transport", { run }),
  setClick: (on: boolean) => call<void>("set_click", { on }),
  /** Click volume, linear 0..4. On the monitor bus only. */
  clickGain: (gain: number) => call<void>("click_gain", { gain }),
  /** Volume of every monitored live input, linear 0..4. Output side only. */
  monitorGain: (gain: number) => call<void>("monitor_gain", { gain }),
  /** Tempo and time signature. Refused unless every track is empty (and no score plays). */
  setTempo: (bpm: number, beatsPerBar: number, beatUnit: number) =>
    call<void>("set_tempo", { bpm, beats_per_bar: beatsPerBar, beat_unit: beatUnit }),
  /** Grid a recording and an overdub snap to. Armed takes keep the position they already have. */
  setQuantize: (quantize: Quantize) => call<void>("set_quantize", { quantize }),
  /** What `O` does in free looping: stay on until pressed again, or one pass. */
  setOverdubMode: (mode: OverdubMode) => call<void>("set_overdub_mode", { mode }),
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

//
// Known devices connect by themselves (at start and when plugged in); `open` is only needed once
// per new device, which is remembered from then on. Every change to a binding is saved at once.
export const midiApi = {
  /** Every MIDI input, with the profile that belongs to it. Opens nothing. */
  ports: () => call<MidiPortView[]>("midi_ports"),
  /** Connect a port by its number, its name or a unique part of it. Remembers the device. */
  open: (device: string) => call<MidiView>("midi_open", { device }),
  /**
   * Disconnect one device (port name or `device:`), or all with no argument. Not reconnected
   * automatically until it is unplugged or connected by hand again.
   */
  close: (device?: string) => call<MidiView>("midi_close", { device: device ?? null }),
  /**
   * Arm the learn mode for one address - the dotted path of the clicked control. `null` cancels.
   * The address is parsed on the Rust side, so a wrong one comes back as a sentence.
   */
  learn: (address: string | null) => call<MidiView>("midi_learn", { address }),
  /**
   * Take a control's job away (saved at once). `id` is what the overview shows (`ch1.note36`);
   * `device` is the binding's `device`, needed when several profiles bind the same id.
   */
  unbind: (id: string, device?: string | null) => call<MidiView>("midi_unbind", { id, device: device ?? null }),
  /** Replace a binding's options - mode, takeover, min, max (`null` = default). Saved at once. */
  edit: (id: string, device: string | null, options: MidiBindingOptions) =>
    call<MidiView>("midi_edit", { id, device, options }),
  /** Retry writing profiles whose automatic save failed. Normally nothing to do. */
  save: () => call<MidiSaved>("midi_save"),
  /** For a frontend that just started and missed the events so far. */
  state: () => call<MidiView>("midi_state"),
  /**
   * Manage a device's profile: move all its bindings to another device, give it a name of its own
   * in the app, or delete it (the file is kept as `.yaml.deleted`).
   */
  profile: (action: MidiProfileAction) => call<MidiView>("midi_profile", { action }),
};

export const scoreApi = {
  /**
   * Translate a score without loading it. `ok` decides; `errors` carries **every** problem at
   * once, each with line, column, message and often a suggestion.
   */
  compile: (yaml: string) => call<CompileOutcome>("score_compile", { yaml }),
  /**
   * Text -> structured document (for the arrangement view). `doc` only when the text compiles;
   * otherwise `errors`, each also naming its element (`at`). Pure, safe at any time.
   */
  parse: (yaml: string) => call<ParseOutcome>("score_parse", { yaml }),
  /**
   * Structured document -> canonical text, validated by the compiler. `yaml` always comes back;
   * `errors` carry the compiler's messages plus the element each belongs to. Comments stay with
   * their element, layout becomes canonical.
   */
  render: (doc: ScoreDoc) => call<RenderOutcome>("score_render", { doc }),
  /**
   * Compile and hand to the runner. Needs a running engine and every track empty. When the score's
   * tracks differ from the engine's, the engine is rebuilt with the score's tracks (short audio
   * gap; `engine` in the answer is then the new `EngineInfo`). Tempo, time signature, layer length
   * and every track's pan, buses and latency then come from the score.
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
  /**
   * Back to the start: **clears every track** and puts the score before its first section, so
   * `start` plays it again. Refused while the score plays (stop it first). Ask before calling.
   */
  rewind: () => call<string>("score_rewind"),
  /** Count-in bars for the next start (1..8; 0 becomes 1). Only before the start. Answers a sentence. */
  countIn: (bars: number) => call<string>("score_count_in", { bars }),
};

/**
 * The mix as a score's `mixer:` section holds it (levels in dB, chains as preset plus what differs).
 * The open score keeps it in sync - see `mixer/scoreMix.ts`.
 */
export const mixerApi = {
  /** Every track of the engine and the buses. */
  state: () => call<MixerSource>("mixer_state"),
  /** Only what a human changed since the last call; `LooperStatus.mixer_rev` says when to ask. */
  take: () => call<MixerSource>("mixer_take"),
  /** Put the engine into this mix, tracks by name (undo/redo in the arrangement). */
  apply: (mixer: MixerSource) => call<void>("mixer_apply", { mixer }),
};

// ---------------------------------------------------------------------------------------------
// Score files. The pickers are the dialog plugin (`dialogs.ts`); contents go through these.
// ---------------------------------------------------------------------------------------------

export const filesApi = {
  /** Proposed folder for "Save as" and the recent files, newest first. */
  info: () => call<FilesInfo>("score_files_info"),
  /** Read a `.yaml`/`.yml` score (at most 1 MB) and put it on top of the recent list. */
  open: (path: string) => call<OpenedFile>("score_file_open", { path }),
  /** Write atomically. A name without extension gets `.yaml`; the answer has the real path. */
  save: (path: string, yaml: string) => call<SavedFile>("score_file_save", { path, yaml }),
  /** Drop a recent entry (a file that has gone). */
  forget: (path: string) => call<RecentFile[]>("score_file_forget", { path }),
  /** Text for "New": the given tracks (normally the engine's) or a default pair. */
  template: (tracks?: TemplateTrack[]) => call<string>("score_template", { tracks: tracks ?? null }),
};
