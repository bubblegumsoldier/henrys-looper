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

/**
 * What `O` does in free looping. `latch` - the default - keeps overdubbing until O is pressed
 * again, every pass of the loop a layer of its own (a pass with nothing played is dropped);
 * `once` records one pass. A score is not affected.
 */
export type OverdubMode = "latch" | "once";

/** What a track is waiting for while its count-in runs. */
export type PendingKind = "record" | "overdub" | "play" | "stop";

export interface LayerStatus {
  /** Zero-based; this is what a layer command expects. */
  index: number;
  /** One-based, for display. */
  number: number;
  muted: boolean;
  /** 0.0 to 4.0. On a MIDI layer it scales the velocity. */
  gain: number;
  /** On a MIDI loop: how many notes the layer holds. Absent on an audio layer. */
  notes?: number;
  /**
   * The muted stand-in of a `stack` whose first pass stayed empty: it only holds the loop's grid
   * and is replaced by the first pass that is played. Not counted as a layer. Absent when false.
   */
  placeholder?: boolean;
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
  /** `HEC-R`: one letter per effect that is on, a dash for one that is off. Always five characters. */
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

/** Which output bus something goes to. `main` is the room, `monitor` the headphones. */
export type BusId = "main" | "monitor";

/** Which of a track's two sources a bus assignment is about. */
export type TrackSourceId = "loop" | "monitor";

/** One output bus: where it goes, how loud it is, and what leaves on it. */
export interface BusStatus {
  id: BusId;
  /** German label, ready to print. */
  label: string;
  /** First device output channel, one-based. */
  channel: number;
  /** 1 (folded to mono onto `channel`) or 2 (a stereo pair). */
  width: number;
  /** `1-2` or `3`, ready to print. */
  channel_label: string;
  /** Linear volume, 0 to 4. */
  gain: number;
  /** Louder side, measured after the volume - what actually leaves. */
  peak: number;
  dbfs: number | null;
  peaks: number[];
  /** True for the bus the click is on. Hard-wired, and the reason the buses exist. */
  has_click: boolean;
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

  // --- output buses ---------------------------------------------------------
  // Two switches per source rather than one word: "geht dieser Track in den Saal" is a question
  // with a yes and a no, and a select with four entries would make it a reading exercise.
  /** Loop playback of this track is heard on the main bus (to the PA). */
  bus_main: boolean;
  /** Loop playback of this track is heard on the monitor bus (to the headphones). */
  bus_monitor: boolean;
  /** The monitored live input of this track is heard on the main bus. */
  monitor_bus_main: boolean;
  /** The monitored live input of this track is heard on the monitor bus. */
  monitor_bus_monitor: boolean;

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
  /** 1-based bar inside this track's own loop (or its first take), 0 when there is none. */
  take_bar: number;
  /** Loop length in whole bars, 0 while nothing is recorded. */
  loop_bars: number;
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
  /**
   * The overdub keeps going into the next pass. During the first recording this means "overdub
   * next": the take runs straight on into a continuous overdub.
   */
  latched: boolean;
  /** This track's effect chain. It sits in the playback path; what is recorded is always dry. */
  fx: FxStatus;

  // --- channel fader ----------------------------------------------------------
  // After the chain and the plugin inserts, before the panner: what a DAW calls the channel fader.
  // Listening only - a take is always recorded dry from the input.
  /** Linear, 0 to `MAX_TRACK_GAIN` (+12 dB). 1.0 is 0 dB. */
  gain: number;
  /** The same in decibels; null at -inf. */
  gain_db: number | null;
  /** Channel mute: silent on both buses. Layers keep their own mutes. */
  muted: boolean;
  /** Channel solo: while any track is soloed, the others fall silent on both buses. */
  soloed: boolean;
  /** What this strip sends, post-fader and post-pan, left and right (monitored input included). */
  strip_peaks: number[];
  /** Louder side of that. */
  strip_peak: number;
  /** Played by an instrument over MIDI instead of an input channel. */
  instrument?: boolean;
  /** An instrument sits in the track's slot and runs. */
  instrument_loaded?: boolean;
  /** Note ons that reached the track (wrapping counter) - the activity light blinks when it moves. */
  notes?: number;
  /** Keys held down right now. */
  held_notes?: number;
  /** The takes store notes (a MIDI loop), played back through the instrument. */
  midi_loop?: boolean;
  /** Note grid of the MIDI layers: "none", "1/4", "1/8", "1/16", "1/8t", "1/16t". */
  grid?: string;
  /** Semitones the live notes are shifted by before they reach the instrument. */
  transpose?: number;
}

/** Event `looper://status`, pushed about twenty times a second while the engine runs. */
export interface LooperStatus {
  running: boolean;
  /**
   * Whether the clock runs. False: bar 1, beat 1, nothing counts or blinks. In free looping it is
   * the Start/Stop button (`api.transport`); with a score it is true from count-in to end of run.
   */
  transport: boolean;
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
  /** What `O` does in free looping. */
  overdub_mode: OverdubMode;
  /** Bars of count-in before a take that starts the standing clock of free looping; 0 = off. */
  count_in_bars: number;
  /** A free-looping count-in is running; the take begins on bar 1 after it. */
  counting_in: boolean;
  sample_rate: number;
  /** The engine's **default** compensation in frames; a track with its own reports it itself. */
  latency_frames: number;
  /** The metronome. It is on the monitor bus and reaches the main bus by no path at all. */
  click: boolean;
  /** Click volume, linear 0..4 (`api.clickGain`). */
  click_gain: number;
  /** The click can be heard at all: false while a loaded score is not playing (its click window is closed). */
  click_open: boolean;
  /** Volume of all monitored live inputs, linear 0..4 (`api.monitorGain`). Never reaches a recording. */
  monitor_gain: number;
  /** Louder side of the main bus, i.e. of what goes to the room. */
  output_peak: number;
  output_dbfs: number | null;
  /** Left and right of the main bus, always two entries. */
  output_peaks: number[];

  // --- the two output buses -------------------------------------------------
  /** One entry per bus, main first. */
  buses: BusStatus[];
  /** Output channels the device really has - what a routing selector may offer. */
  output_channels: number;
  /**
   * True while both buses leave on exactly the same channels, which is what a two-output
   * interface forces. There is then one mix: every source is heard once, the click included, and
   * the monitor volume has nothing to regulate on its own.
   */
  buses_collapsed: boolean;
  /** German sentence explaining an overlapping routing, or null when the buses are apart. */
  bus_note: string | null;

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
  /**
   * Goes up whenever a human changes a fader, mute, effect or bus level (mouse, key or MIDI);
   * `mixerApi.take` then says what changed. See `mixer/scoreMix.ts`.
   */
  mixer_rev: number;
}

export const IDLE_STATUS: LooperStatus = {
  running: false,
  transport: false,
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
  overdub_mode: "latch",
  count_in_bars: 2,
  counting_in: false,
  sample_rate: 0,
  latency_frames: 0,
  click: false,
  click_gain: 0,
  click_open: false,
  monitor_gain: 0,
  output_peak: 0,
  output_dbfs: null,
  output_peaks: [0, 0],
  buses: [],
  output_channels: 0,
  buses_collapsed: false,
  bus_note: null,
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
  mixer_rev: 0,
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
  /**
   * Which output buses the **loop playback** of this track is heard on: `main`, `monitor`,
   * `main+monitor` (the default) or `none`.
   */
  bus: string;
  /**
   * Which output buses the **monitored live input** is heard on, independent of the loop. Default
   * `monitor`: the room usually has the musician through the PA's own channel.
   */
  monitor_bus: string;
  /**
   * Set: the track is played by an instrument plugin over MIDI (the input channels are then
   * ignored; the track is stereo). Which plugin lives with the plugins, by track name.
   */
  instrument?: StartInstrument | null;
}

/** What plays an instrument track. */
export interface StartInstrument {
  /** MIDI port name or a unique part of it; null: every connected device. */
  device: string | null;
  /** 1..16; null: every channel. */
  channel: number | null;
  /** Key range (MIDI note numbers); null: all keys. Keys outside stay with the MIDI bindings. */
  note_low?: number | null;
  note_high?: number | null;
  /** Heard whenever it is played (default true). */
  monitor: boolean;
  /** What a take stores: the sound ("audio", default) or the notes ("midi"). */
  loop?: "audio" | "midi" | null;
  /** Note grid of a MIDI loop (default "none"). */
  grid?: string | null;
  /** Semitones the live notes are shifted by, -48..48 (default 0). The key range applies before. */
  transpose?: number | null;
}

/** Where one bus leaves the machine, as the setup writes it. */
export interface StartBusOut {
  /** First device output channel, one-based. */
  channel: number;
  /** 1 folds the bus to mono onto that channel, 2 is a stereo pair. */
  width: number;
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
  /** What `O` does in free looping. Missing in an older stored setup: `latch`. */
  overdub_mode?: OverdubMode;
  /** Bars of count-in before the first take of free looping (0, 1, 2). Missing: 2. */
  count_in_bars?: number;
  /** Default compensation in frames, for every track that brings none of its own. */
  latency_frames: number;
  monitor_gain: number;
  click_gain: number;
  click: boolean;
  monitor: boolean;
  /** Where the main bus (to the PA) leaves. null means outputs 1-2. */
  bus_out_main: StartBusOut | null;
  /**
   * Where the monitor bus (to the headphones, and the only bus the click is on) leaves. null means
   * outputs 3-4 on a device that has four, and 1-2 on one that does not - where both buses are
   * then folded into one mix, which the engine says out loud rather than quietly dropping the
   * click.
   */
  bus_out_monitor: StartBusOut | null;
  /** Volume of the main bus, linear. null means 1.0. */
  bus_gain_main: number | null;
  /** Volume of the monitor bus, linear. null means 1.0. */
  bus_gain_monitor: number | null;
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
/** `stack`: a new layer every pass until the section ends (overdub that keeps going). */
export type ScoreState = "record" | "overdub" | "stack" | "play" | "stop" | "hear_through";

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
  /** Present on an instrument track. */
  instrument?: TrackMidi;
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
  /** The score's `count_in:`, absent when it does not say. */
  count_in?: number;
  tracks: CompiledTrack[];
  midi: Record<string, { id: string }>;
  sections: CompiledSection[];
  /** The `mixer:` section as written; absent when the score has none. */
  mixer?: MixerSource;
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
  /** Set when loading rebuilt the engine with the score's tracks (also sent as `looper://engine`). */
  engine: EngineInfo | null;
}

// ---------------------------------------------------------------------------------------------
// The score as an editable document. Mirrors `looper_engine::score::doc`.
//
// Text and graphical editor are two views of one document: `score_parse` turns text into a
// `ScoreDoc`, `score_render` turns a `ScoreDoc` back into canonical text and validates it with the
// compiler. Optional fields are optional on purpose: absent = "not written here" (the default
// applies, or the section inherits it through `repeat`). What a value resolves to is in
// `DocSection.resolved`, with its origin. `line` and `resolved` are output only - the backend
// ignores them when a document comes back.
// ---------------------------------------------------------------------------------------------

/** Comments attached to one element: whole lines above it, and the comment at the end of its line.
 *  Stored without the leading `# `. They move with the element and vanish with it. */
export interface DocComments {
  before?: string[];
  after?: string;
}

export interface DocTrack {
  name: string;
  /** One-based input; the left one of a stereo pair. 0 on an instrument track. */
  input: number;
  /** `input: instrument`: played by an instrument plugin over MIDI. */
  instrument?: boolean;
  /** `midi:` of an instrument track: which device, channel and keys play it. */
  midi?: TrackMidi;
  /** One-based right input of a stereo track; absent = mono. */
  input_right?: number;
  /** -1..1; absent = centre. */
  pan?: number;
  /** Absent = true. */
  monitor?: boolean;
  /** Frames, 0..96000; absent = the engine's default. */
  latency?: number;
  /** Frames, -96000..96000; absent = 0. */
  latency_trim?: number;
  /** `main` | `monitor` | `main+monitor` | `none`; absent = `main+monitor`. */
  bus?: string;
  /** Same words; absent = `monitor` (`main+monitor` on an instrument track). */
  monitor_bus?: string;
  /** `audio` | `midi`: what a take stores; absent = `audio`. `midi` needs an instrument track. */
  loop?: string;
  /** Note grid of the track's MIDI layers (`none`, `1/4`, `1/8`, `1/16`, `1/8t`, `1/16t`). */
  grid?: string;
  comments?: DocComments;
  line?: number;
}

/** `midi:` of an instrument track. Absent fields: every device, every channel, all keys. */
export interface TrackMidi {
  device?: string;
  /** 1..16. */
  channel?: number;
  note_low?: number;
  note_high?: number;
  /** Semitones, -48..48; absent = 0. */
  transpose?: number;
}

/** One entry of `midi:`. Exactly one of `note` / `cc`. */
export interface DocMidiBinding {
  /** The address as written: `next_section`, `track.voice.record`, ... */
  key: string;
  note?: number;
  cc?: number;
  /** 1..16; absent = 1. */
  channel?: number;
  /** `press` | `release` | `toggle` | `momentary`. */
  mode?: string;
  /** `pickup` | `jump`. */
  takeover?: string;
  min?: number;
  max?: number;
  comments?: DocComments;
  line?: number;
}

/** One `track: state` line a section states itself. */
export interface DocSectionTrack {
  track: string;
  state: ScoreState;
  /** The clip's own note grid, overriding the track's while the section runs. */
  grid?: string;
  comments?: DocComments;
  line?: number;
}

/** Where a resolved value comes from. */
export type ValueSource =
  | { kind: "own" }
  /** Inherited through `repeat`, from the section that actually states it (end of the chain). */
  | { kind: "repeat"; index: number; section: string }
  /** Nobody states it: 4 bars, no autorelease, `loop`, track `stop`. */
  | { kind: "default" };

export interface Resolved<T> {
  value: T;
  from: ValueSource;
}

/** A section as the runner will play it. One `tracks` entry per score track, in track order. */
export interface ResolvedSection {
  bars: Resolved<number>;
  autorelease: Resolved<boolean>;
  quantize: Resolved<Quantize>;
  tracks: { track: string; state: ScoreState; from: ValueSource }[];
}

export interface DocSection {
  id: string;
  repeat?: string;
  bars?: number;
  autorelease?: boolean;
  quantize?: Quantize;
  /** Only the states this section states itself, in written order. */
  tracks: DocSectionTrack[];
  /** Comments above the `- id:` line and at its end. */
  comments?: DocComments;
  /** Comments of the other field lines, keyed by field name. */
  field_comments?: Record<string, DocComments>;
  line?: number;
  /** Output only. */
  resolved?: ResolvedSection;
}

// --- the `mixer:` section. Mirrors `looper_engine::score::mixer`. Levels in dB (-100 and below is
// silence), every field optional: inside an entry an omitted field is the default (0 dB, not muted,
// dry); an effect line overrides only the fields it names, on top of `preset`.

export interface MixerBusLevels {
  main?: number;
  monitor?: number;
  click?: number;
  /** The monitored live inputs (`monitor_gain`). */
  inputs?: number;
}

export interface TrackMix {
  /** Channel fader, dB. */
  gain?: number;
  mute?: boolean;
  /** `dry` | `voice` | `piezo_guitar`; absent = dry. */
  preset?: string;
  /** Whole chain out; absent = out exactly when no effect is on. */
  bypass?: boolean;
  high_pass?: { on?: boolean; hz?: number };
  eq?: { on?: boolean };
  band1?: MixBand;
  band2?: MixBand;
  band3?: MixBand;
  comp?: { on?: boolean; threshold?: number; ratio?: number; attack?: number; release?: number; knee?: number; makeup?: number };
  delay?: { on?: boolean; note?: string; feedback?: number; mix?: number };
  reverb?: { on?: boolean; size?: number; damping?: number; mix?: number };
  /** Instrument plugin id of an instrument track; absent = keep what the track has. */
  instrument?: string;
  /** Effect plugins in slot order, by id; absent = keep, `[]` = empty the slots. */
  inserts?: MixInsert[];
}

export interface MixInsert {
  plugin: string;
  bypass?: boolean;
}

export interface MixBand {
  kind?: string;
  hz?: number;
  q?: number;
  gain?: number;
}

/** The section without comments - what `mixer_state` / `mixer_take` answer and `mixer_apply` takes. */
export interface MixerSource {
  buses?: MixerBusLevels;
  /** By track name, in order. */
  tracks?: Record<string, TrackMix>;
}

/** What `mixer_take` answers: the changed entries, plus changed pans (kept under `tracks:`). */
export interface MixerTake extends MixerSource {
  pan?: Record<string, number>;
}

/** One entry of `mixer.tracks` in the document: the mix fields flat, plus comments. */
export interface DocMixerTrack extends TrackMix {
  track: string;
  comments?: DocComments;
  /** Comments of the field lines of a block entry. */
  field_comments?: Record<string, DocComments>;
  line?: number;
}

export interface DocMixer {
  buses?: MixerBusLevels;
  tracks: DocMixerTrack[];
  /** Comments of the `buses:` and `tracks:` lines. */
  field_comments?: Record<string, DocComments>;
}

export interface ScoreDoc {
  title?: string;
  bpm: number;
  /** `"3/4"`. */
  time_signature?: string;
  /** Old alias for the numerator; kept when a file used it. New documents use `time_signature`. */
  beats_per_bar?: number;
  /** Bars of click before the first section, 1 to 8; absent means the default of two. */
  count_in?: number;
  tracks: DocTrack[];
  midi: DocMidiBinding[];
  sections: DocSection[];
  /** Fader, mute, effects and bus levels kept with the song; absent when the score has none. */
  mixer?: DocMixer;
  /** Comment lines above the first key / after the last element. */
  header?: string[];
  footer?: string[];
  /** Comments of the top-level keys. */
  field_comments?: Record<string, DocComments>;
}

/** Which element something belongs to. `field` names one field of it; absent = the whole element. */
export type ScoreRef =
  | { kind: "score"; field?: string }
  | { kind: "track"; index: number; name: string; field?: string }
  | { kind: "midi"; index: number; key: string; field?: string }
  | { kind: "section"; index: number; id: string; field?: string }
  /** `index` is the position in `DocSection.tracks`. */
  | { kind: "section_track"; section: number; section_id: string; index: number; track: string }
  /** The `mixer:` section; `field` is `buses` or `tracks`. */
  | { kind: "mixer"; field?: string }
  /** `index` is the position in `DocMixer.tracks`. */
  | { kind: "mixer_track"; index: number; track: string; field?: string };

/** Where an element's key stands in the text. */
export interface ScoreLocation {
  at: ScoreRef;
  line: number;
  column: number;
}

/** A compiler issue plus the element it belongs to. */
export interface ModelIssue extends ScoreIssue {
  at: ScoreRef | null;
}

/** Answer of `score_parse`. `doc` only when the text compiles. */
export interface ParseOutcome {
  ok: boolean;
  doc: ScoreDoc | null;
  score: CompiledScore | null;
  locations: ScoreLocation[];
  errors: ModelIssue[];
  /** Not errors: written correctly, not supported yet (`loop: midi`, `grid`). */
  notes?: ModelIssue[];
}

/** Answer of `score_render`. `yaml` always; positions in `errors` refer to it. */
export interface RenderOutcome {
  ok: boolean;
  yaml: string;
  /** When ok: parsed back from `yaml`. Otherwise the document as sent, `resolved` recomputed. */
  doc: ScoreDoc;
  score: CompiledScore | null;
  locations: ScoreLocation[];
  errors: ModelIssue[];
  notes?: ModelIssue[];
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
  /** Bars of count-in the next start plays (1..8). Set with `scoreApi.countIn` before the start. */
  count_in: number;
}

/**
 * The word for each target state, and for each phase - the same words as `shell/terms.ts`.
 *
 * The wire format carries a `label` beside every one of these, and it is deliberately **not** what
 * gets printed here: the words the UI can derive itself - the five states and the five phases - are
 * taken from the UI's own vocabulary, and only the runner's whole *sentences* are printed as they
 * arrive.
 *
 * The table has to exist anyway: a compiled section carries states, not labels, so the block
 * preview could not print a word without it.
 */
export const SCORE_STATE_LABEL: Record<ScoreState, string> = {
  record: "Record",
  overdub: "Overdub",
  stack: "Stack",
  play: "Play",
  stop: "Stop",
  hear_through: "Monitor",
};

export const PHASE_LABEL: Record<Phase, string> = {
  idle: "Ready",
  count_in: "Count-in",
  running: "Running",
  finished: "Finished",
  stopped: "Stopped",
};

// ---------------------------------------------------------------------------------------------
// Score files (app/src-tauri/src/files.rs). Plain YAML, extension `.yaml`.
// ---------------------------------------------------------------------------------------------

export interface RecentFile {
  path: string;
  /** File name without folder. */
  name: string;
  /** False when the file has gone since it was last used. */
  exists: boolean;
}

export interface FilesInfo {
  /** `~/Documents/Henrys Looper`, already created; null without a documents folder. */
  default_dir: string | null;
  /** Newest first, at most ten. */
  recent: RecentFile[];
}

export interface OpenedFile {
  path: string;
  name: string;
  yaml: string;
  recent: RecentFile[];
}

export interface SavedFile {
  /** The path really written (`.yaml` appended when the name had no extension). */
  path: string;
  name: string;
  recent: RecentFile[];
}

/** One track for the "New" template. Inputs one-based. */
export interface TemplateTrack {
  name: string;
  input: number;
  input_right?: number;
  /** An instrument track, as the setup has it. */
  instrument?: StartInstrument | null;
}

/** Pushed as `looper://notice`: something the program says on its own - the runner at a section
 *  boundary and at the end of the score. `seq` increases per notice. */
export interface NoticeEvent {
  seq: number;
  /** `score` for the runner. */
  source: string;
  text: string;
}
