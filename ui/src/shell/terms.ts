//! The words of the user interface, in one place.
//!
//! The app speaks English, in the vocabulary a DAW user already knows. Every view - Live,
//! Arrangement, Mixer, Setup, MIDI - takes its words for states, transport and positions from here,
//! so "Overdub" is never "Dub" in one corner and "Layer" in another. A state always comes with the
//! same word, the same icon (`STATE_ICON`) and the same colour token (`--st-*` in tokens.css):
//! colour alone never carries a meaning.
//!
//! Sentences that come from the Rust side (engine and runner messages, refusals) are printed as they
//! arrive; translating those is a separate pass on the Rust side.

import type { LooperTrackState, Phase, ScoreState } from "../types";
import { IS_MAC } from "../platform";

/** Visual state of a track or a score target. The class suffix of `.st`, `.is`, `.tb`, `.clip`. */
export type StateKind = "rec" | "dub" | "play" | "stop" | "hear" | "armed" | "empty" | "ready";

export const T = {
  // --- the five actions of a track (buttons) --------------------------------
  record: "Record",
  overdub: "Overdub",
  play: "Play",
  stop: "Stop",
  monitor: "Monitor",
  clear: "Clear",
  clearTrack: "Clear track",
  undo: "Undo",
  undoLast: "Undo last take",
  tap: "Tap",
  tempoLocked: "Clear all loops to change tempo, time signature or loop length.",
  tempoLockedShort: "Clear all loops to change",
  mute: "Mute",
  muted: "Muted",
  /** Overdub mode, setup and tile. */
  overdubLatch: "Stays on until pressed again",
  overdubOnce: "One pass",
  overdubNext: "Overdub next",
  lastPass: "last pass · ends at loop end",

  // --- positions -------------------------------------------------------------
  section: "Section",
  bar: "Bar",
  bars: "bars",
  barShort: "b",
  beat: "Beat",
  tempo: "Tempo",
  timeSig: "Time sig.",
  pass: "Pass",
  loop: "Loop",

  // --- transport -------------------------------------------------------------
  start: "Start",
  /** The free-looping clock: the big button and its MIDI label. */
  transportStart: "Start",
  transportStop: "Stop",
  transportToggle: "Start/stop transport",
  transportStopped: "Stopped",
  release: "Release",
  stopAll: "Stop all",
  running: "Running",
  since: "since",
  click: "Click",
  clickOn: "Click on",
  clickOff: "Click off",
  countIn: "Count-in",
  switchArmed: "Switch armed",
  loopsUntilRelease: "Loops until Release",
  autoNext: "Moves on by itself",
  next: "next",
  now: "now",
  upNext: "up next",
  end: "End",
  quantizeLoop: "Start at loop",
  quantizeBar: "Start at bar",
  atLoopEnd: "at loop end",
  atNextBar: "at next bar",

  // --- modes and places --------------------------------------------------------
  arrangement: "Arrangement",
  live: "Live",
  setup: "Setup",
  midiLearn: "MIDI Learn",
  mixer: "Mixer",
  inspector: "Inspector",

  // --- engine ----------------------------------------------------------------
  engine: "Engine",
  engineRunning: "Engine running",
  engineStopped: "Engine stopped",
  startEngine: "Start engine",
  stopEngine: "Stop engine",
  dropouts: "dropouts",
  clearAll: "Clear all",

  // --- track facts -----------------------------------------------------------
  input: "In",
  output: "Out",
  layer: "Layer",
  layers: "Layers",
  noLoop: "No loop",
  pan: "Pan",
  latency: "Latency",
  trim: "Trim",
  mono: "mono",
  stereo: "stereo",
  // --- instrument tracks ---------------------------------------------------------
  instrument: "Instrument",
  instrumentAdd: "Instrument",
  instrumentNone: "No instrument",
  instrumentPick: "Choose an instrument",
  instrumentLoading: "Loading instrument…",
  allDevices: "All MIDI devices",
  allChannels: "All channels",
  channelShort: "Ch",
  midiIn: "MIDI",
  panic: "All notes off",
  panicShort: "Panic",
  main: "Main",
  monitorBus: "Monitor",
  saved: "saved",
  unsaved: "unsaved",

  // --- recording to disk -------------------------------------------------------
  recording: "Recording",
  recordingToDisk: "Recording to disk",
  recordingTail: "Writing the tail",
  stopRecording: "Stop recording to disk",
  recordEveryRun: "Record every run to disk",
  dryStems: "Also write dry stems",
  recordingsFolder: "Folder",
  freeSpace: "Free space",
  lowSpace: "Little space left",
  showInFinder: IS_MAC ? "Show in Finder" : "Show in folder",
  recentRecordings: "Recent recordings",
  deleteRecording: "Delete",
  recordingSaved: "Recording saved",
  recordingDiscarded: "Recording discarded",

  // --- the effect chain ----------------------------------------------------------
  fxOn: "On",
  fxOff: "Off",
  /** Banner over a bypassed chain - the whole bar is the button that switches it in. */
  chainBypassed: "Chain bypassed – click to switch in",
  chainBypassedHint:
    "Nothing of the chain is heard: the track passes bit for bit. Click to switch the chain in - turning any knob or switching an effect on does that too.",
  /** Tile caption when the chain is bypassed although effects are on. */
  fxBypassedShort: "FX bypassed",
  devices: "Devices",
  builtIn: "Built-in",
  plugIns: "Plug-ins",
  addPlugin: "Add plugin",
  chainOn: "Chain on",
  chainBypassedShort: "Bypassed",
  openEditor: "Open editor",
  closeEditor: "Close editor",
  removeDevice: "Remove",
  changeDevice: "Change…",
  plugUnsupported: "Plugins are available on macOS only for now.",
} as const;

/** The word for a score target state. */
export const SCORE_STATE: Record<ScoreState, string> = {
  record: "Record",
  overdub: "Overdub",
  play: "Play",
  stop: "Stop",
  hear_through: "Monitor",
};

export const SCORE_STATE_KIND: Record<ScoreState, StateKind> = {
  record: "rec",
  overdub: "dub",
  play: "play",
  stop: "stop",
  hear_through: "hear",
};

/** The word for what a track is doing right now - the big word on a Live tile. */
export const STATE_WORD: Record<StateKind, string> = {
  rec: "Record",
  dub: "Overdub",
  play: "Play",
  stop: "Stopped",
  hear: "Monitor",
  armed: "Armed",
  empty: "Empty",
  ready: "Ready",
};

/** The icon class (`ic-*` in icons.css) that belongs to each state. */
export const STATE_ICON: Record<StateKind, string> = {
  rec: "rec",
  dub: "dub",
  play: "play",
  stop: "stop",
  hear: "hear",
  armed: "armed",
  empty: "empty",
  ready: "play-o",
};

/** The runner's phases. */
export const PHASE: Record<Phase, string> = {
  idle: "Ready",
  count_in: "Count-in",
  running: "Running",
  finished: "Finished",
  stopped: "Stopped",
};

/** What a pending track action is called on its countdown chip. */
export const PENDING: Record<string, StateKind> = {
  record: "rec",
  overdub: "dub",
  play: "play",
  stop: "stop",
};

/** Engine track state -> visual state, ignoring monitoring (see `trackKind`). */
export const ENGINE_STATE_KIND: Record<LooperTrackState, StateKind> = {
  empty: "empty",
  armed: "armed",
  recording: "rec",
  overdub: "dub",
  ready: "ready",
  playing: "play",
};

/** "1 bar", "2 bars"; "1 beat", "3 beats". */
export function bars(n: number): string {
  return `${n} ${n === 1 ? "bar" : "bars"}`;
}
export function beats(n: number): string {
  return `${n} ${n === 1 ? "beat" : "beats"}`;
}

/**
 * A countdown of whole bars plus beats, as one phrase: rounded up to bars while there is more than
 * a bar to go ("in 3 bars" for 2 bars + 1 beat), counted in beats inside the last bar.
 */
export function countdown(wholeBars: number, beatsLeft: number): string {
  if (wholeBars <= 0) return beats(Math.max(beatsLeft, 0));
  return bars(wholeBars + (beatsLeft > 0 ? 1 : 0));
}

/** A linear gain as decibels: "0.0 dB", "−6.0 dB", "−∞ dB". */
export function gainDb(gain: number): string {
  if (gain <= 0) return "−∞ dB";
  const db = 20 * Math.log10(gain);
  const v = Math.abs(db) < 0.05 ? 0 : db;
  return `${v > 0 ? "+" : v < 0 ? "−" : ""}${Math.abs(v).toFixed(1)} dB`;
}

/** Numbers as a musician reads them: one decimal at most, no trailing zero. */
export function num(value: number, digits = 1): string {
  const fixed = value.toFixed(digits);
  return fixed.replace(/\.0+$/, "");
}
