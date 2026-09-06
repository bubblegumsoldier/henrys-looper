// ---------------------------------------------------------------------------------------------
// Looper engine wire format. Mirrors app/src-tauri/src/proto.rs one to one - that file is the
// authority, this is only its TypeScript shadow.
//
// Two conventions run through all of it: indices are zero-based and are what a command expects,
// while `input_channel` and every layer's `number` are one-based, because that is what is printed
// on the interface. `*_dbfs` is null at digital silence, minus infinity has no JSON form.
// ---------------------------------------------------------------------------------------------

/** Track state as the engine reports it; `state_label` carries the German word to print. */
export type LooperTrackState = "empty" | "armed" | "recording" | "overdub" | "ready" | "playing";

/**
 * Grid a recording and an overdub snap to.
 *
 * `loop` - the default - starts the take at the next loop boundary, so one early press is enough
 * and there is time to get to the instrument. `bar` is the old next-bar behaviour, which means
 * waiting for the last bar of the loop and then hurrying.
 */
export type Quantize = "bar" | "loop";

/** What a track is waiting for while its count-in runs. */
export type PendingKind = "record" | "overdub" | "play" | "stop";

export interface LayerStatus {
  /** Zero-based; this is what a layer command expects. */
  index: number;
  /** One-based, for display. */
  number: number;
  muted: boolean;
  /** 0.0 to 4.0. */
  gain: number;
}

export interface LooperTrackStatus {
  /** Zero-based; this is what a track command expects. */
  index: number;
  name: string;
  /** One-based, as printed on the interface. */
  input_channel: number;
  state: LooperTrackState;
  /** German word for the state, ready to print. */
  state_label: string;
  monitor: boolean;
  playing: boolean;
  loop_samples: number;
  loop_seconds: number;
  /** Samples already written during a running loop-defining take. */
  filled_samples: number;
  input_peak: number;
  input_dbfs: number | null;
  output_peak: number;
  output_dbfs: number | null;
  layers: LayerStatus[];
  /** What this track is waiting for, or null when nothing is scheduled. */
  pending_kind: PendingKind | null;
  /** German word for it, ready to print - same idea as `state_label`. */
  pending_label: string | null;
  /** Whole bars still to go. */
  pending_bars: number;
  /** Beats on top of that; inside the last bar `pending_bars` is 0 and this counts down. */
  pending_beats: number;
}

/** Event `looper://status`, pushed about twenty times a second while the engine runs. */
export interface LooperStatus {
  running: boolean;
  pos: number;
  seconds: number;
  /** One-based bar, as counted in music. */
  bar: number;
  /** One-based beat inside the bar. */
  beat: number;
  beat_offset: number;
  samples_per_beat: number;
  bpm: number;
  beats_per_bar: number;
  beat_unit: number;
  bars: number;
  loop_samples: number;
  loop_seconds: number;
  /** Grid a recording and an overdub currently snap to. Changeable while the engine runs. */
  quantize: Quantize;
  sample_rate: number;
  latency_samples: number;
  click: boolean;
  output_peak: number;
  output_dbfs: number | null;
  tracks: LooperTrackStatus[];
  xruns: number;
  fifo_underruns: number;
  /** Input samples were lost; every later recording is permanently shifted. The bad one. */
  fifo_overruns: number;
  other_errors: number;
  max_callback_ms: number;
  ignored_commands: number;
  spares: number;
  /** Newest German sentence about what happened - a confirmation or a refusal. */
  message: string | null;
}

export const IDLE_STATUS: LooperStatus = {
  running: false,
  pos: 0,
  seconds: 0,
  bar: 1,
  beat: 1,
  beat_offset: 0,
  samples_per_beat: 0,
  bpm: 0,
  beats_per_bar: 0,
  beat_unit: 0,
  bars: 0,
  loop_samples: 0,
  loop_seconds: 0,
  quantize: "loop",
  sample_rate: 0,
  latency_samples: 0,
  click: false,
  output_peak: 0,
  output_dbfs: null,
  tracks: [],
  xruns: 0,
  fifo_underruns: 0,
  fifo_overruns: 0,
  other_errors: 0,
  max_callback_ms: 0,
  ignored_commands: 0,
  spares: 0,
  message: null,
};

// --- device survey ---------------------------------------------------------

export interface ConfigInfo {
  channels: number;
  min_sample_rate: number;
  max_sample_rate: number;
  sample_format: string;
  /** Buffer range the driver reports, in frames; null when it reports nothing usable. */
  min_buffer_frames: number | null;
  max_buffer_frames: number | null;
}

export interface DeviceInfo {
  name: string;
  input: boolean;
  output: boolean;
  input_configs: ConfigInfo[];
  output_configs: ConfigInfo[];
  note: string | null;
}

export interface HostInfo {
  /** Pass this back as `host` when starting: `asio`, `wasapi`. */
  name: string;
  error: string | null;
  default_input: string | null;
  default_output: string | null;
  devices: DeviceInfo[];
}

export interface DeviceReport {
  asio_built: boolean;
  hosts: HostInfo[];
}

// --- starting the engine ---------------------------------------------------

export interface StartTrack {
  name: string;
  /** One-based, as printed on the interface. */
  input_channel: number;
}

export interface StartConfig {
  host: string;
  /** Case-insensitive substring of the device name; null takes the host default. */
  device: string | null;
  sample_rate: number;
  buffer_frames: number;
  input_channels: number | null;
  output_channels: number | null;
  force_buffer: boolean;
  tracks: StartTrack[];
  bpm: number;
  beats_per_bar: number;
  beat_unit: number;
  /** Loop length in bars. */
  bars: number;
  /** Grid a recording and an overdub snap to. */
  quantize: Quantize;
  latency_samples: number;
  monitor_gain: number;
  click_gain: number;
  click: boolean;
  monitor: boolean;
}

/** What the driver actually agreed to. Returned by `engine_start`. */
export interface EngineInfo {
  host: string;
  input_device: string;
  output_device: string;
  input_channels: number;
  output_channels: number;
  input_format: string;
  output_format: string;
  sample_rate: number;
  buffer_frames: number;
  driver_input_frames: number | null;
  driver_output_frames: number | null;
  bpm: number;
  beats_per_bar: number;
  beat_unit: number;
  samples_per_beat: number;
  bars: number;
  loop_samples: number;
  loop_seconds: number;
  latency_samples: number;
  latency_ms: number;
  layer_capacity: number;
  max_layers: number;
  max_tracks: number;
  memory_mb: number;
  tracks: StartTrack[];
}

/** Event `looper://ready`, also readable through the `app_info` command. */
export interface AppInfo {
  version: string;
  log_path: string;
  logging: boolean;
  asio_built: boolean;
  max_tracks: number;
  max_layers: number;
}

export interface CalibrateOutcome {
  log_path: string;
  message: string;
}

// ---------------------------------------------------------------------------------------------
// Score types, mirroring docs/contracts-v0.md. Phase 3 - the Rust side has no compiler yet, so
// nothing below is reachable at the moment; the editor keeps them so it can be switched on again.
// ---------------------------------------------------------------------------------------------

export type TrackState = "record" | "overdub" | "play" | "stop" | "hear_through";

export interface ScoreTrack {
  name: string;
  type: "single" | "group";
  /** single tracks only: 0-based index in the Ableton set. */
  ableton_track?: number | null;
  /** group tracks only: name of the Ableton group track plus the size of its layer pool. */
  group?: string | null;
  layers?: number | null;
  reserve?: number | null;
  monitor?: boolean | null;
}

/** Live occupancy of a group track's layer child tracks (from the runner's state event). */
export interface GroupLayers {
  layers_used: number;
  layers_free: number;
}

/** What ensure_pool() prepared in Ableton when a score with group tracks was loaded. */
export interface PoolGroupResult {
  track: string;
  group: string;
  group_index: number;
  existing_layers: string[];
  created_layers: string[];
  monitor: string | null;
  monitor_created: boolean;
  layer_indices: number[];
  monitor_index: number | null;
  strays: string[];
  warnings: string[];
  message: string;
}

export interface PoolReport {
  changed: boolean;
  groups: PoolGroupResult[];
  messages: string[];
}

export interface ScoreSection {
  id: string;
  index: number;
  bars: number;
  autorelease: boolean;
  quantize: "bar" | "loop";
  tracks: Record<string, TrackState>;
  source_line: number | null;
}

export interface Score {
  title: string;
  bpm: number;
  beats_per_bar: number;
  tracks: ScoreTrack[];
  midi: Record<string, { id: string }>;
  sections: ScoreSection[];
}

export interface ScoreErr {
  line: number | null;
  column: number | null;
  message: string;
  suggestion: string | null;
}

export interface StateEvent {
  type: "state";
  running: boolean;
  section_index: number | null;
  section_id: string | null;
  bars_total: number | null;
  bar: number;
  beat: number;
  pending: "next" | null;
  tracks: Record<string, TrackState>;
  connected: boolean;
  engine: "sim" | "ableton" | string;
  /** Set by the real runner while waiting for the first bar boundary (not in the v0 contract). */
  countin?: boolean;
  /** Group tracks report how many of their layer child tracks are taken (M4). */
  groups?: Record<string, GroupLayers>;
}

/** True while the transport runs but the first section has not started yet. */
export function isCountIn(s: StateEvent): boolean {
  return s.running && (s.countin === true || s.section_index === null || s.section_index < 0);
}

export interface BeatEvent {
  type: "beat";
  song_beat: number;
  bar: number;
  beat: number;
}

export interface MidiEvent {
  type: "midi";
  device: string;
  channel: number;
  kind: "note_on" | "note_off" | "cc" | string;
  number: number;
  value: number;
  id: string;
}

export interface LogEvent {
  type: "log";
  level: "info" | "warn" | "error" | string;
  message: string;
}

export type LooperEvent = StateEvent | BeatEvent | MidiEvent | LogEvent;

export interface MidiRow extends MidiEvent {
  seq: number;
  ts: number;
}

export interface LogRow extends LogEvent {
  seq: number;
  ts: number;
}

export type CompileResult = { ok: true; score: Score } | { ok: false; errors: ScoreErr[] };

/** POST /api/load additionally reports what was prepared in Ableton (group tracks only). */
export type LoadResult = CompileResult & { state?: StateEvent; pool?: PoolReport };

export const IDLE_STATE: StateEvent = {
  type: "state",
  running: false,
  section_index: null,
  section_id: null,
  bars_total: null,
  bar: 0,
  beat: 0,
  pending: null,
  tracks: {},
  connected: false,
  engine: "sim",
};
