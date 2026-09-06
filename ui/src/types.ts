// ---------------------------------------------------------------------------------------------
// Looper engine wire format. Mirrors app/src-tauri/src/proto.rs one to one - that file is the
// authority, this is only its TypeScript shadow.
//
// Two conventions run through all of it: indices are zero-based and are what a command expects,
// while `input_channel`, `input_channels` and every layer's `number` are one-based, because that is
// what is printed on the interface. `*_dbfs` is null at digital silence, minus infinity has no JSON
// form.
//
// Levels come in two forms since the engine became stereo: `*_peak` is the louder of the two
// channels (draw one bar and be done), `*_peaks` is one entry per channel (draw a stereo meter).
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

// --- effects ---------------------------------------------------------------
// Mirrors the effect half of proto.rs. Every enum travels as a snake_case string and goes straight
// back into the command that changes it, so nothing here has to know a number's meaning.

/** One switchable position in the chain, in signal order. */
export type FxSlotName = "high_pass" | "eq" | "comp" | "delay" | "reverb";

/** A ready-made chain. `custom` is reported, never sent - it means a knob has been moved. */
export type FxPresetName = "dry" | "voice" | "piezo_guitar" | "custom";

/** What one EQ band does. */
export type BandKindName = "peak" | "low_shelf" | "high_shelf";

/** Note value of the tempo-synchronous delay. There is deliberately no millisecond setting. */
export type DelayNoteName = "quarter" | "dotted_eighth" | "eighth" | "triplet_eighth";

/** Every numeric knob of a chain, by the name `fx_set` expects. */
export type FxParamName =
  | "high_pass_hz"
  | "band_hz"
  | "band_q"
  | "band_gain_db"
  | "comp_threshold_db"
  | "comp_ratio"
  | "comp_attack_ms"
  | "comp_release_ms"
  | "comp_knee_db"
  | "comp_makeup_db"
  | "delay_feedback"
  | "delay_mix"
  | "reverb_size"
  | "reverb_damping"
  | "reverb_mix";

export interface FxBandStatus {
  /** Zero-based; this is what `fx_set` expects as `band`. */
  index: number;
  /** One-based, for display. */
  number: number;
  kind: BandKindName;
  hz: number;
  q: number;
  gain_db: number;
}

export interface FxEffectStatus {
  /** Pass this back as `effect` in `fx_enable`. */
  name: FxSlotName;
  /** German word, ready to print. */
  label: string;
  on: boolean;
}

/** State of one track's effect chain. Effects act on playback and monitoring, never on a take. */
export interface FxStatus {
  /** Whole chain out of the signal path - a bit-identical pass-through, i.e. the panic switch. */
  bypass: boolean;
  preset: FxPresetName;
  /** German word for the preset, ready to print. */
  preset_label: string;
  /** `HEK-R`: one letter per effect that is on, a dash for one that is off. Always five characters. */
  letters: string;
  /** On/off per effect, in signal order. */
  effects: FxEffectStatus[];

  high_pass_hz: number;
  bands: FxBandStatus[];

  comp_threshold_db: number;
  comp_ratio: number;
  comp_attack_ms: number;
  comp_release_ms: number;
  comp_knee_db: number;
  comp_makeup_db: number;
  /** Deepest gain reduction since the last status event, in dB and never positive. A meter. */
  comp_reduction_db: number;

  delay_note: DelayNoteName;
  /** The note value as it is written on paper: `1/4`, `1/8.`, `1/8`, `1/8T`. */
  delay_note_label: string;
  /** What the note value works out to at the current tempo. Dictated by the engine's timeline. */
  delay_samples: number;
  delay_ms: number;
  delay_feedback: number;
  delay_mix: number;

  reverb_size: number;
  reverb_damping: number;
  reverb_mix: number;
}

export interface LooperTrackStatus {
  /** Zero-based; this is what a track command expects. */
  index: number;
  name: string;
  /** One-based, as printed on the interface. The left one of a stereo pair. */
  input_channel: number;
  /** Every input this track records, one-based: one entry for mono, two for stereo. */
  input_channels: number[];
  /** 1 when the loop buffers are mono, 2 when they are stereo. */
  channels: number;
  /** German word for that: `mono` or `stereo`. */
  channels_label: string;
  /** Position between the speakers: -1 hard left, 0 centre, +1 hard right. */
  pan: number;

  // --- latency compensation, in frames -------------------------------------
  // Four fields rather than one, because a display that shows only the effective value cannot say
  // whether it is a setting or something this track inherited - and that difference is the reason
  // the value became per track in the first place.
  /** What this track really subtracts while recording: base plus surcharge. */
  latency_frames: number;
  /** The same in milliseconds. */
  latency_ms: number;
  /** The measured part as configured, or null while the track follows the engine's default. */
  latency_measured: number | null;
  /** Manual surcharge in frames. A calibration never changes it. */
  latency_trim: number;
  /** True while `latency_measured` is null, i.e. while the base is inherited. */
  latency_inherited: boolean;

  state: LooperTrackState;
  /** German word for the state, ready to print. */
  state_label: string;
  monitor: boolean;
  playing: boolean;
  /** Loop length in frames - one sample per frame on a mono track, two on a stereo one. */
  loop_samples: number;
  loop_seconds: number;
  /** Frames already written during a running loop-defining take. */
  filled_samples: number;
  /** Loudest recorded input channel. */
  input_peak: number;
  input_dbfs: number | null;
  /** One entry per recorded input channel. */
  input_peaks: number[];
  /** Louder side of this track's contribution to the bus. */
  output_peak: number;
  output_dbfs: number | null;
  /** Left and right, always two entries - the bus is stereo whatever the track records. */
  output_peaks: number[];
  layers: LayerStatus[];
  /** What this track is waiting for, or null when nothing is scheduled. */
  pending_kind: PendingKind | null;
  /** German word for it, ready to print - same idea as `state_label`. */
  pending_label: string | null;
  /** Whole bars still to go. */
  pending_bars: number;
  /** Beats on top of that; inside the last bar `pending_bars` is 0 and this counts down. */
  pending_beats: number;
  /** This track's effect chain. It sits in the playback path; what is recorded is always dry. */
  fx: FxStatus;
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
  /** The engine's **default** compensation in frames; a track with its own reports it itself. */
  latency_frames: number;
  click: boolean;
  /** Louder side of the master bus. */
  output_peak: number;
  output_dbfs: number | null;
  /** Left and right of the master bus, always two entries. */
  output_peaks: number[];
  tracks: LooperTrackStatus[];
  /** What the runner is doing, or null while no score is loaded. */
  score: ScoreStatus | null;
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
  latency_frames: 0,
  click: false,
  output_peak: 0,
  output_dbfs: null,
  output_peaks: [0, 0],
  tracks: [],
  xruns: 0,
  fifo_underruns: 0,
  fifo_overruns: 0,
  other_errors: 0,
  max_callback_ms: 0,
  ignored_commands: 0,
  spares: 0,
  message: null,
  score: null,
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
  /** One-based, as printed on the interface. The left one of a stereo pair. */
  input_channel: number;
  /**
   * Second input, one-based. Set it and the track records **stereo**; leave it null and the track
   * records mono. Which two inputs belong together is a wiring fact, so it is never guessed.
   */
  input_channel_right: number | null;
  /** Position between the speakers: -1 hard left, 0 centre, +1 hard right. */
  pan: number;
  /**
   * Measured latency compensation of this input, in frames; null takes the global default. This
   * is the half `calibrate` writes.
   */
  latency_frames: number | null;
  /**
   * Manual surcharge on top, in frames - what a measurement cannot see because it happens inside
   * an external plugin host. Survives every calibration.
   */
  latency_trim: number;
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
  /** Default compensation in frames, for every track that brings none of its own. */
  latency_frames: number;
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
  latency_frames: number;
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
// The score. Mirrors `looper_engine::score` (the compiled form and the compiler's complaints) and
// the score half of proto.rs.
//
// The compiled score is sent **as the engine produces it**: it has carried serde derives since the
// day it was written, precisely so a UI could read it without a translation layer. That is why the
// names below are the ones from `engine/src/score/model.rs` rather than app-side inventions.
// ---------------------------------------------------------------------------------------------

/** What a score asks of a track during a section. The complete state, never a delta. */
export type ScoreState = "record" | "overdub" | "play" | "stop" | "hear_through";

/** A track after compilation. Carries both numberings so nothing has to convert twice. */
export interface CompiledTrack {
  name: string;
  /** Position in `tracks`, which is the index the engine addresses this track by. */
  index: number;
  /** One-based input channel as written in the score; the left one of a stereo pair. */
  input: number;
  /** The same channel zero-based, ready for the engine. */
  input_channel: number;
  /** Right-hand input of a stereo track, one-based. Absent on a mono track. */
  input_right?: number;
  input_channel_right?: number;
  /** 1 for a mono loop buffer, 2 for a stereo one. */
  channels: number;
  pan: number;
  /** Whether this track may be monitored at all. `hear_through` and the takes need it. */
  monitor: boolean;
  /** Measured latency compensation in frames; absent means the engine's default. */
  latency?: number;
  latency_trim: number;
}

/** A fully resolved section: `repeat:` unrolled, defaults filled in, **every** track named. */
export interface CompiledSection {
  id: string;
  index: number;
  bars: number;
  /** True: the follow-up is scheduled the moment this section starts. False: it loops until the
   *  release button, and the change is then quantised to `quantize`. */
  autorelease: boolean;
  quantize: Quantize;
  /** One entry per track of the score, in track order. */
  tracks: Record<string, ScoreState>;
  /** One-based line this section starts on, so the preview can point back at the source. */
  source_line?: number;
}

/** The compiled score - static, fully resolved. What the runner walks and the preview draws. */
export interface CompiledScore {
  title: string;
  bpm: number;
  /** `"3/4"`, kept as written. */
  time_signature: string;
  beats_per_bar: number;
  /** Denominator: the note value that gets one beat. */
  beat_unit: number;
  tracks: CompiledTrack[];
  midi: Record<string, { id: string }>;
  sections: CompiledSection[];
}

/**
 * One complaint of the compiler.
 *
 * Four fields rather than one sentence, and that is the point: `line` and `column` are what the
 * editor puts a marker on, and `suggestion` ("meintest du 'bass'?") is kept apart so it can be
 * rendered differently from the message.
 */
export interface ScoreIssue {
  line: number | null;
  column: number | null;
  message: string;
  suggestion: string | null;
}

/** Answer of `score_compile`: the compiled score, or **every** problem at once. */
export interface CompileOutcome {
  ok: boolean;
  score: CompiledScore | null;
  errors: ScoreIssue[];
}

/** Answer of `score_load`. */
export interface ScoreLoaded {
  score: CompiledScore;
  message: string;
}

/** What the runner is doing. */
export type Phase = "idle" | "count_in" | "running" | "finished" | "stopped";

/** A change that is scheduled but has not taken effect yet. */
export interface ArmedChange {
  /** `voice_2`, or `Ende` when the change leads past the last section. */
  label: string;
  /** Zero-based index of the section it leads to; null for the end of the score. */
  section: number | null;
  /** Whole bars still to go. */
  bars: number;
  /** Beats on top of that; inside the last bar `bars` is 0 and this counts down. */
  beats: number;
  /** True while this is the count-in rather than a section change. */
  count_in: boolean;
  /** The whole thing as one German sentence, ready to print. */
  text: string;
}

/** What the score asks of one track in the section that is sounding. */
export interface ScoreTarget {
  /** Zero-based, the same index the status event's `tracks` array uses. */
  index: number;
  name: string;
  state: ScoreState;
  /** German word for it, ready to print. */
  label: string;
}

/** The runner as one snapshot, carried inside every status event while a score is loaded. */
export interface ScoreStatus {
  title: string;
  phase: Phase;
  /** German word for the phase, ready to print. */
  phase_label: string;
  /** Zero-based index of the sounding section, or null before the first and after the last. */
  section: number | null;
  section_id: string;
  section_count: number;
  bars_total: number;
  /** One-based bar inside the current pass; 0 when nothing is sounding. */
  bar: number;
  beat: number;
  /** One-based pass number - a section without `autorelease` repeats until the release button. */
  pass: number;
  tracks: ScoreTarget[];
  armed: ArmedChange | null;
}

/**
 * The German word for each target state, and for each phase.
 *
 * The wire format carries a `label` beside every one of these, and it is deliberately **not** what
 * gets printed here: the Rust side spells German without umlauts throughout, because the same
 * sentences go to a Windows console (`Einzaehler`, `mithoeren`). This window is not a console, and
 * the rest of it already writes `Mithören`. So the words the UI can derive itself - the five states
 * and the five phases - are spelled properly here, and only the runner's whole *sentences* are
 * printed as they arrive.
 *
 * The table has to exist anyway: a compiled section carries states, not labels, so the block
 * preview could not print a word without it.
 */
export const SCORE_STATE_LABEL: Record<ScoreState, string> = {
  record: "Aufnahme",
  overdub: "Overdub",
  play: "Wiedergabe",
  stop: "still",
  hear_through: "mithören",
};

export const PHASE_LABEL: Record<Phase, string> = {
  idle: "bereit",
  count_in: "Einzähler",
  running: "läuft",
  finished: "Ende",
  stopped: "gestoppt",
};
