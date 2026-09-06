//! The wire format between the Rust side and the web view.
//!
//! Everything here is `serde` with `rename_all = "snake_case"`, so what the frontend sees is
//! exactly what the field names below say. Two conventions run through the whole file and are the
//! only thing that needs remembering:
//!
//! * **Indices are zero-based.** `track` and `layer` in a command are positions in the `tracks`
//!   and `layers` arrays of [`StatusEvent`]. Nothing has to be counted twice.
//! * **Hardware numbers are one-based**, because that is how they are printed on the interface.
//!   That affects `input_channel` and `input_channels`, and every layer additionally carries a
//!   `number` for display next to its zero-based `index`.
//!
//! Levels are reported twice: `*_peak` as a linear absolute value in `0.0..=1.0` for a meter bar,
//! and `*_dbfs` for a number to print. `*_dbfs` is `null` at digital silence, because minus
//! infinity has no JSON representation.
//!
//! # Stereo on the wire
//!
//! Since the engine became stereo, every level exists in two forms, and both are sent:
//!
//! * `*_peak` / `*_dbfs` - the **louder of the two channels**, so a frontend that draws one bar per
//!   meter needs no arithmetic and no case distinction between a mono and a stereo track;
//! * `*_peaks` - one entry per channel, so a frontend that wants a real stereo meter has the
//!   numbers. A track's `input_peaks` has one entry when it records mono and two when it records
//!   stereo; the output and the master are always two, because the bus is.
//!
//! Sending both rather than only the array is deliberate: the maximum is what a level is *read* as
//! ("am I clipping"), and computing it in three places in the frontend is how the three places
//! start to disagree.

use serde::{Deserialize, Serialize};

use looper_engine::engine::command::TrackStatus;
use looper_engine::engine::fx::{
    BandKind, DelayNote, EQ_BANDS, FxParam, FxPreset, FxSlot, FxStatus,
};
use looper_engine::engine::schedule::{Lead, PendingKind, Quantize};
use looper_engine::engine::track::TrackState;

/// dBFS for display, or `None` at digital silence.
pub fn dbfs(linear: f32) -> Option<f64> {
    if linear > 0.0 {
        Some(20.0 * (linear as f64).log10())
    } else {
        None
    }
}

// ---------------------------------------------------------------------------------------------
// Device listing
// ---------------------------------------------------------------------------------------------

#[derive(Serialize, Clone, Debug)]
#[serde(rename_all = "snake_case")]
pub struct DeviceReport {
    /// Whether this binary was built with `--features asio`. Without it the ASIO host is missing
    /// from `hosts` and the looper cannot be run at a useful latency.
    pub asio_built: bool,
    pub hosts: Vec<HostInfo>,
}

#[derive(Serialize, Clone, Debug)]
#[serde(rename_all = "snake_case")]
pub struct HostInfo {
    /// The value to pass back as `host` when starting the engine, lowercased: `asio`, `wasapi`.
    pub name: String,
    /// Set when the host could not be opened at all; `devices` is then empty.
    pub error: Option<String>,
    pub default_input: Option<String>,
    pub default_output: Option<String>,
    pub devices: Vec<DeviceInfo>,
}

#[derive(Serialize, Clone, Debug)]
#[serde(rename_all = "snake_case")]
pub struct DeviceInfo {
    /// Pass this back as `device` when starting the engine.
    pub name: String,
    pub input: bool,
    pub output: bool,
    pub input_configs: Vec<ConfigInfo>,
    pub output_configs: Vec<ConfigInfo>,
    /// Why a direction could not be queried, if that happened.
    pub note: Option<String>,
}

#[derive(Serialize, Clone, Debug)]
#[serde(rename_all = "snake_case")]
pub struct ConfigInfo {
    pub channels: u16,
    pub min_sample_rate: u32,
    pub max_sample_rate: u32,
    pub sample_format: String,
    /// Buffer range the driver reports, in frames. `null` when it reports nothing usable - WASAPI
    /// in shared mode does that, and then only the device period actually works.
    pub min_buffer_frames: Option<u32>,
    pub max_buffer_frames: Option<u32>,
}

// ---------------------------------------------------------------------------------------------
// Starting the engine
// ---------------------------------------------------------------------------------------------

/// One track as the user set it up: a name, the input or input pair it listens on, and where it
/// sits between the speakers.
///
/// Naming one input makes a **mono** track, naming two makes a **stereo** one - which two inputs
/// belong together is a wiring fact and is never guessed. `pan` and `input_channel_right` both
/// default, so a mono track in the middle is still `{"name": "stimme", "input_channel": 1}`.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(rename_all = "snake_case")]
pub struct TrackConfig {
    pub name: String,
    /// One-based, as printed on the interface. The left one of a stereo pair.
    pub input_channel: u32,
    /// Right-hand input of a stereo track, one-based. `null` or absent means the track is mono.
    #[serde(default)]
    pub input_channel_right: Option<u32>,
    /// -1.0 hard left, 0.0 centre, +1.0 hard right. Absent means centre.
    #[serde(default)]
    pub pan: f32,
}

impl TrackConfig {
    /// A mono track in the centre - what the default setup and most tracks are.
    pub fn mono(name: &str, input_channel: u32) -> Self {
        Self {
            name: name.to_string(),
            input_channel,
            input_channel_right: None,
            pan: 0.0,
        }
    }

    /// 1 for a mono track, 2 for a stereo one.
    pub fn channels(&self) -> u32 {
        if self.input_channel_right.is_some() { 2 } else { 1 }
    }
}

// ---------------------------------------------------------------------------------------------
// Quantisation
// ---------------------------------------------------------------------------------------------

/// Which grid a take snaps to, mirroring [`Quantize`] one to one.
///
/// The engine crate carries no `serde`, so this is its wire twin - the same arrangement
/// [`TrackStateName`] has with [`TrackState`].
#[derive(Serialize, Deserialize, Clone, Copy, Debug, Default, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum QuantizeName {
    /// Next bar boundary.
    Bar,
    /// Next loop boundary. The default: one early press is enough.
    #[default]
    Loop,
}

impl From<Quantize> for QuantizeName {
    fn from(q: Quantize) -> Self {
        match q {
            Quantize::Bar => QuantizeName::Bar,
            Quantize::Loop => QuantizeName::Loop,
        }
    }
}

impl From<QuantizeName> for Quantize {
    fn from(q: QuantizeName) -> Self {
        match q {
            QuantizeName::Bar => Quantize::Bar,
            QuantizeName::Loop => Quantize::Loop,
        }
    }
}

/// What a track is waiting for, mirroring [`PendingKind`] one to one.
#[derive(Serialize, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum PendingKindName {
    Record,
    Overdub,
    Play,
    Stop,
}

impl From<PendingKind> for PendingKindName {
    fn from(kind: PendingKind) -> Self {
        match kind {
            PendingKind::Record => PendingKindName::Record,
            PendingKind::Overdub => PendingKindName::Overdub,
            PendingKind::Play => PendingKindName::Play,
            PendingKind::Stop => PendingKindName::Stop,
        }
    }
}

/// Everything the engine needs to open a device and start counting.
///
/// Every field except `tracks` has a default, so a frontend can send only what it wants to change
/// from the values phase 0 measured on this machine.
#[derive(Deserialize, Clone, Debug)]
#[serde(rename_all = "snake_case")]
pub struct StartConfig {
    /// `asio` or `wasapi`. Only ASIO reaches a usable latency, see docs/architektur.md section 2.
    #[serde(default = "default_host")]
    pub host: String,
    /// Case-insensitive substring of the device name; `null` takes the host default.
    #[serde(default)]
    pub device: Option<String>,
    #[serde(default = "default_sample_rate")]
    pub sample_rate: u32,
    #[serde(default = "default_buffer_frames")]
    pub buffer_frames: u32,
    /// `null` takes what the device reports as its default.
    #[serde(default)]
    pub input_channels: Option<u16>,
    #[serde(default)]
    pub output_channels: Option<u16>,
    /// Request the buffer size even though the device reports a different range. Only useful for
    /// measuring what a driver makes of it.
    #[serde(default)]
    pub force_buffer: bool,

    pub tracks: Vec<TrackConfig>,

    #[serde(default = "default_bpm")]
    pub bpm: f64,
    #[serde(default = "default_beats_per_bar")]
    pub beats_per_bar: u32,
    /// 4 = quarter note gets the beat, 8 = eighth note.
    #[serde(default = "default_beat_unit")]
    pub beat_unit: u32,
    /// Loop length in bars.
    #[serde(default = "default_bars")]
    pub bars: u32,
    /// Grid a recording and an overdub snap to. `loop` (the default) starts the take at the next
    /// loop boundary, so one early press is enough; `bar` is the old next-bar behaviour.
    #[serde(default)]
    pub quantize: QuantizeName,
    /// Roundtrip latency removed while recording. Measure it with `calibrate` after every change
    /// of device, sample rate or buffer size.
    #[serde(default = "default_latency_samples")]
    pub latency_samples: u64,
    #[serde(default = "default_gain")]
    pub monitor_gain: f32,
    #[serde(default = "default_gain")]
    pub click_gain: f32,
    /// Start with the metronome switched on.
    #[serde(default = "default_true")]
    pub click: bool,
    /// Start with input monitoring switched on, for every track.
    #[serde(default)]
    pub monitor: bool,
}

fn default_host() -> String {
    "asio".to_string()
}
fn default_sample_rate() -> u32 {
    48_000
}
fn default_buffer_frames() -> u32 {
    128
}
fn default_bpm() -> f64 {
    100.0
}
fn default_beats_per_bar() -> u32 {
    4
}
fn default_beat_unit() -> u32 {
    4
}
fn default_bars() -> u32 {
    8
}
fn default_latency_samples() -> u64 {
    827
}
fn default_gain() -> f32 {
    1.0
}
fn default_true() -> bool {
    true
}

/// What the engine actually opened. Returned by `engine_start`, so the UI can show the numbers the
/// driver agreed to rather than the ones that were asked for.
#[derive(Serialize, Clone, Debug)]
#[serde(rename_all = "snake_case")]
pub struct EngineInfo {
    pub host: String,
    pub input_device: String,
    pub output_device: String,
    pub input_channels: u16,
    pub output_channels: u16,
    pub input_format: String,
    pub output_format: String,
    pub sample_rate: u32,
    /// Requested buffer size in frames.
    pub buffer_frames: u32,
    /// What the driver reported after the stream was built; `null` if it says nothing.
    pub driver_input_frames: Option<u32>,
    pub driver_output_frames: Option<u32>,

    pub bpm: f64,
    pub beats_per_bar: u32,
    pub beat_unit: u32,
    pub samples_per_beat: f64,
    pub bars: u32,
    pub loop_samples: u64,
    pub loop_seconds: f64,

    pub latency_samples: u64,
    pub latency_ms: f64,

    /// Length one layer buffer is allocated at, in samples.
    pub layer_capacity: u64,
    pub max_layers: u32,
    pub max_tracks: u32,
    /// Upper bound of the memory a full session can occupy.
    pub memory_mb: f64,

    pub tracks: Vec<TrackConfig>,
}

// ---------------------------------------------------------------------------------------------
// Status push
// ---------------------------------------------------------------------------------------------

/// Track state, mirroring [`TrackState`] one to one.
#[derive(Serialize, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum TrackStateName {
    /// No layer.
    Empty,
    /// A recording is scheduled but has not started yet.
    Armed,
    /// The first take is running; it defines loop length and origin.
    Recording,
    /// A further layer is being recorded into the existing loop.
    Overdub,
    /// Content available, silent.
    Ready,
    /// Content available, playing.
    Playing,
}

impl From<TrackState> for TrackStateName {
    fn from(state: TrackState) -> Self {
        match state {
            TrackState::Empty => TrackStateName::Empty,
            TrackState::Armed => TrackStateName::Armed,
            TrackState::Recording => TrackStateName::Recording,
            TrackState::Overdub => TrackStateName::Overdub,
            TrackState::Ready => TrackStateName::Ready,
            TrackState::Playing => TrackStateName::Playing,
        }
    }
}

#[derive(Serialize, Clone, Copy, Debug)]
#[serde(rename_all = "snake_case")]
pub struct LayerStatus {
    /// Zero-based; this is what a `layer_*` command expects.
    pub index: usize,
    /// One-based, for display.
    pub number: u32,
    pub muted: bool,
    /// 0.0 to 4.0. Mirrored on the control side, because the engine's status snapshot does not
    /// carry it - it only ever changes by command, so the mirror cannot go stale.
    pub gain: f32,
}

#[derive(Serialize, Clone, Debug)]
#[serde(rename_all = "snake_case")]
pub struct TrackStatusEvent {
    /// Zero-based; this is what a `track_*` command expects.
    pub index: usize,
    pub name: String,
    /// One-based, as printed on the interface. The left one of a stereo pair.
    pub input_channel: u32,
    /// Every input this track records, one-based: one entry when it is mono, two when it is
    /// stereo. `input_channel` is always the first of them.
    pub input_channels: Vec<u32>,
    /// 1 when this track's loop buffers are mono, 2 when they are stereo.
    pub channels: u32,
    /// German word for that, ready to print: `mono` or `stereo`.
    pub channels_label: String,
    /// Position in the stereo field: -1.0 hard left, 0.0 centre, +1.0 hard right.
    pub pan: f32,
    pub state: TrackStateName,
    /// German word for the state, ready to print.
    pub state_label: String,
    pub monitor: bool,
    pub playing: bool,
    /// Loop length of this track in frames; 0 while nothing is recorded. A frame is one instant of
    /// audio - one sample on a mono track, two on a stereo one.
    pub loop_samples: u64,
    pub loop_seconds: f64,
    /// Frames already written during a running loop-defining take.
    pub filled_samples: u64,
    /// Loudest recorded input channel.
    pub input_peak: f32,
    pub input_dbfs: Option<f64>,
    /// One entry per recorded input channel, so a stereo source can be metered per side.
    pub input_peaks: Vec<f32>,
    /// Louder side of what this track's audible layers contributed to the bus. Monitoring is not
    /// part of it.
    pub output_peak: f32,
    pub output_dbfs: Option<f64>,
    /// Left and right, always two entries - the bus is stereo whatever the track records.
    pub output_peaks: Vec<f32>,
    pub layers: Vec<LayerStatus>,

    // --- the count-in ------------------------------------------------------------------------
    // "scharf" alone does not say whether to pick the instrument up now or in five bars. These
    // four fields say it. They are all quiet (`null` / 0) when nothing is scheduled.
    /// What this track is waiting for, or `null` when nothing is scheduled.
    pub pending_kind: Option<PendingKindName>,
    /// German word for it, ready to print - same idea as `state_label`.
    pub pending_label: Option<String>,
    /// Whole bars still to go before it happens.
    pub pending_bars: u32,
    /// Beats on top of `pending_bars`. Inside the last bar `pending_bars` is 0 and this counts down.
    pub pending_beats: u32,

    /// This track's effect chain. Effects act on playback and on monitoring; what is recorded is
    /// always dry, so nothing in here can be baked into a layer.
    pub fx: FxEvent,
}

/// Pushed to the frontend as event `looper://status`, about twenty times a second while the engine
/// runs, plus once with `running: false` after it stops.
#[derive(Serialize, Clone, Debug)]
#[serde(rename_all = "snake_case")]
pub struct StatusEvent {
    pub running: bool,
    /// Absolute sample position on the engine's output timeline.
    pub pos: u64,
    /// The same, in seconds since the engine started.
    pub seconds: f64,
    /// One-based bar, as counted in music.
    pub bar: u64,
    /// One-based beat inside the bar.
    pub beat: u32,
    /// Samples elapsed inside the current beat; with `samples_per_beat` this drives a beat bar.
    pub beat_offset: u64,
    pub samples_per_beat: f64,
    pub bpm: f64,
    pub beats_per_bar: u32,
    pub beat_unit: u32,
    /// Configured loop length in bars.
    pub bars: u32,
    /// The same in samples and seconds, at the current tempo.
    pub loop_samples: u64,
    pub loop_seconds: f64,
    /// Grid a recording and an overdub currently snap to. Changeable while the engine runs.
    pub quantize: QuantizeName,
    pub sample_rate: u32,
    pub latency_samples: u64,
    pub click: bool,
    /// Louder side of the produced output block.
    pub output_peak: f32,
    pub output_dbfs: Option<f64>,
    /// Left and right of the master bus, always two entries.
    pub output_peaks: Vec<f32>,
    pub tracks: Vec<TrackStatusEvent>,

    /// Buffer underruns reported by cpal. Anything but 0 means the audio was interrupted.
    pub xruns: u64,
    /// Output callbacks that found the input FIFO short. Costs a little delay, no alignment.
    pub fifo_underruns: u64,
    /// Input callbacks that could not push. **Input samples were lost and every later recording is
    /// permanently shifted** - the session has to be restarted.
    pub fifo_overruns: u64,
    /// Stream errors that were not underruns.
    pub other_errors: u64,
    /// Longest audio callback so far. At 128 frames / 48 kHz the budget is 2.67 ms.
    pub max_callback_ms: f64,
    /// Commands the engine refused, in total.
    pub ignored_commands: u64,
    /// Prepared layer buffers the engine is currently holding ready.
    pub spares: u32,
    /// The newest German sentence about what happened - a confirmation or a refusal. `null` when
    /// there is nothing to say.
    pub message: Option<String>,
}

impl StatusEvent {
    /// The event that says "nothing is running". Sent once after the engine stops, so a frontend
    /// that missed the reply of `engine_stop` still ends up in the right state.
    pub fn idle() -> Self {
        Self {
            running: false,
            pos: 0,
            seconds: 0.0,
            bar: 1,
            beat: 1,
            beat_offset: 0,
            samples_per_beat: 0.0,
            bpm: 0.0,
            beats_per_bar: 0,
            beat_unit: 0,
            bars: 0,
            loop_samples: 0,
            loop_seconds: 0.0,
            quantize: QuantizeName::default(),
            sample_rate: 0,
            latency_samples: 0,
            click: false,
            output_peak: 0.0,
            output_dbfs: None,
            output_peaks: vec![0.0, 0.0],
            tracks: Vec::new(),
            xruns: 0,
            fifo_underruns: 0,
            fifo_overruns: 0,
            other_errors: 0,
            max_callback_ms: 0.0,
            ignored_commands: 0,
            spares: 0,
            message: None,
        }
    }
}

/// Build the per-track part of a status event from the engine snapshot plus the control-side
/// mirror (name, input channel, layer gains).
pub fn track_event(
    index: usize,
    name: &str,
    ts: &TrackStatus,
    gains: &[f32],
    sample_rate: u32,
    lead: Lead,
) -> TrackStatusEvent {
    let layers = (0..ts.layers as usize)
        .map(|i| LayerStatus {
            index: i,
            number: i as u32 + 1,
            muted: ts.muted_mask & (1 << i) != 0,
            gain: gains.get(i).copied().unwrap_or(1.0),
        })
        .collect();
    let rate = sample_rate.max(1) as f64;
    let input_peak = ts.input_peak_max();
    let output_peak = ts.output_peak_max();
    TrackStatusEvent {
        index,
        name: name.to_string(),
        input_channel: ts.input_channels[0] as u32 + 1,
        input_channels: ts
            .used_input_channels()
            .iter()
            .map(|&c| c as u32 + 1)
            .collect(),
        channels: ts.channels as u32,
        channels_label: if ts.channels >= 2 { "stereo" } else { "mono" }.to_string(),
        pan: ts.pan,
        state: ts.state.into(),
        state_label: ts.state.label().to_string(),
        monitor: ts.monitor,
        playing: ts.playing,
        loop_samples: ts.loop_len,
        loop_seconds: ts.loop_len as f64 / rate,
        filled_samples: ts.filled,
        input_peak,
        input_dbfs: dbfs(input_peak),
        input_peaks: ts.input_peak[..(ts.channels as usize).clamp(1, 2)].to_vec(),
        output_peak,
        output_dbfs: dbfs(output_peak),
        output_peaks: ts.output_peak.to_vec(),
        layers,
        pending_kind: lead.kind.map(PendingKindName::from),
        pending_label: lead.kind.map(|k| k.label().to_string()),
        pending_bars: lead.bars,
        pending_beats: lead.beats,
        fx: fx_event(&ts.fx, sample_rate),
    }
}

// ---------------------------------------------------------------------------------------------
// Effects
// ---------------------------------------------------------------------------------------------
//
// The same arrangement as everywhere else in this file: the engine crate carries no `serde`, so
// each of its enums gets a wire twin here and a `From` in both directions. What the frontend sees
// are snake_case strings - `high_pass`, `piezo_guitar`, `dotted_eighth` - and never a number whose
// meaning it would have to know.

/// One switchable position in a track's chain, mirroring [`FxSlot`].
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum FxSlotName {
    HighPass,
    Eq,
    Comp,
    Delay,
    Reverb,
}

impl From<FxSlot> for FxSlotName {
    fn from(slot: FxSlot) -> Self {
        match slot {
            FxSlot::HighPass => FxSlotName::HighPass,
            FxSlot::Eq => FxSlotName::Eq,
            FxSlot::Comp => FxSlotName::Comp,
            FxSlot::Delay => FxSlotName::Delay,
            FxSlot::Reverb => FxSlotName::Reverb,
        }
    }
}

impl From<FxSlotName> for FxSlot {
    fn from(name: FxSlotName) -> Self {
        match name {
            FxSlotName::HighPass => FxSlot::HighPass,
            FxSlotName::Eq => FxSlot::Eq,
            FxSlotName::Comp => FxSlot::Comp,
            FxSlotName::Delay => FxSlot::Delay,
            FxSlotName::Reverb => FxSlot::Reverb,
        }
    }
}

/// A ready-made chain, mirroring [`FxPreset`]. `custom` is reported, never sent - it is what the
/// engine calls a chain whose knobs have been moved since a preset was loaded.
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum FxPresetName {
    #[default]
    Dry,
    Voice,
    PiezoGuitar,
    Custom,
}

impl From<FxPreset> for FxPresetName {
    fn from(preset: FxPreset) -> Self {
        match preset {
            FxPreset::Dry => FxPresetName::Dry,
            FxPreset::Voice => FxPresetName::Voice,
            FxPreset::PiezoGuitar => FxPresetName::PiezoGuitar,
            FxPreset::Custom => FxPresetName::Custom,
        }
    }
}

impl From<FxPresetName> for FxPreset {
    fn from(name: FxPresetName) -> Self {
        match name {
            FxPresetName::Dry => FxPreset::Dry,
            FxPresetName::Voice => FxPreset::Voice,
            FxPresetName::PiezoGuitar => FxPreset::PiezoGuitar,
            FxPresetName::Custom => FxPreset::Custom,
        }
    }
}

/// What one EQ band does, mirroring [`BandKind`].
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum BandKindName {
    #[default]
    Peak,
    LowShelf,
    HighShelf,
}

impl From<BandKind> for BandKindName {
    fn from(kind: BandKind) -> Self {
        match kind {
            BandKind::Peak => BandKindName::Peak,
            BandKind::LowShelf => BandKindName::LowShelf,
            BandKind::HighShelf => BandKindName::HighShelf,
        }
    }
}

impl From<BandKindName> for BandKind {
    fn from(name: BandKindName) -> Self {
        match name {
            BandKindName::Peak => BandKind::Peak,
            BandKindName::LowShelf => BandKind::LowShelf,
            BandKindName::HighShelf => BandKind::HighShelf,
        }
    }
}

/// Note value of the tempo-synchronous delay, mirroring [`DelayNote`]. There is deliberately no
/// milliseconds field anywhere: the delay time comes from the engine's timeline.
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum DelayNoteName {
    Quarter,
    #[default]
    DottedEighth,
    Eighth,
    TripletEighth,
}

impl From<DelayNote> for DelayNoteName {
    fn from(note: DelayNote) -> Self {
        match note {
            DelayNote::Quarter => DelayNoteName::Quarter,
            DelayNote::DottedEighth => DelayNoteName::DottedEighth,
            DelayNote::Eighth => DelayNoteName::Eighth,
            DelayNote::TripletEighth => DelayNoteName::TripletEighth,
        }
    }
}

impl From<DelayNoteName> for DelayNote {
    fn from(name: DelayNoteName) -> Self {
        match name {
            DelayNoteName::Quarter => DelayNote::Quarter,
            DelayNoteName::DottedEighth => DelayNote::DottedEighth,
            DelayNoteName::Eighth => DelayNote::Eighth,
            DelayNoteName::TripletEighth => DelayNote::TripletEighth,
        }
    }
}

/// Every numeric knob of a chain, by name.
///
/// One command with a name and a value rather than seventeen commands: the frontend sends
/// `fx_set(track, "comp_ratio", 3.0)`, and the band-scoped names additionally take `band`. The
/// two knobs that are not numbers - the band kind and the delay note - have their own commands,
/// because squeezing an enum into an `f64` is how wire formats rot.
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum FxParamName {
    HighPassHz,
    BandHz,
    BandQ,
    BandGainDb,
    CompThresholdDb,
    CompRatio,
    CompAttackMs,
    CompReleaseMs,
    CompKneeDb,
    CompMakeupDb,
    DelayFeedback,
    DelayMix,
    ReverbSize,
    ReverbDamping,
    ReverbMix,
}

impl FxParamName {
    /// Whether this knob belongs to one EQ band, i.e. whether `band` has to be supplied.
    pub fn needs_band(self) -> bool {
        matches!(
            self,
            FxParamName::BandHz | FxParamName::BandQ | FxParamName::BandGainDb
        )
    }

    /// Turn a name plus a value into an engine parameter, or say in German what is missing.
    ///
    /// The engine clamps every value to a usable range anyway; what is checked here is only the
    /// shape - a band-scoped knob without a band, or a band that does not exist.
    pub fn to_param(self, value: f64, band: Option<u32>) -> Result<FxParam, String> {
        let v = value as f32;
        if self.needs_band() {
            let Some(band) = band else {
                return Err(
                    "Dieser Parameter gehoert zu einem EQ-Band; es fehlt die Angabe \"band\" \
                     (0, 1 oder 2)."
                        .to_string(),
                );
            };
            let band = band as usize;
            if band >= EQ_BANDS {
                return Err(format!(
                    "Es gibt die EQ-Baender 0 bis {}, nicht {band}.",
                    EQ_BANDS - 1
                ));
            }
            return Ok(match self {
                FxParamName::BandHz => FxParam::BandHz { band, hz: v },
                FxParamName::BandQ => FxParam::BandQ { band, q: v },
                _ => FxParam::BandGainDb { band, db: v },
            });
        }
        Ok(match self {
            FxParamName::HighPassHz => FxParam::HighPassHz(v),
            FxParamName::CompThresholdDb => FxParam::CompThresholdDb(v),
            FxParamName::CompRatio => FxParam::CompRatio(v),
            FxParamName::CompAttackMs => FxParam::CompAttackMs(v),
            FxParamName::CompReleaseMs => FxParam::CompReleaseMs(v),
            FxParamName::CompKneeDb => FxParam::CompKneeDb(v),
            FxParamName::CompMakeupDb => FxParam::CompMakeupDb(v),
            FxParamName::DelayFeedback => FxParam::DelayFeedback(v),
            FxParamName::DelayMix => FxParam::DelayMix(v),
            FxParamName::ReverbSize => FxParam::ReverbSize(v),
            FxParamName::ReverbDamping => FxParam::ReverbDamping(v),
            FxParamName::ReverbMix => FxParam::ReverbMix(v),
            // The band-scoped names are all handled above.
            _ => unreachable!("Band-Parameter werden oben behandelt"),
        })
    }
}

#[derive(Serialize, Clone, Copy, Debug)]
#[serde(rename_all = "snake_case")]
pub struct BandEvent {
    /// Zero-based; this is what a `fx_set` command expects as `band`.
    pub index: usize,
    /// One-based, for display.
    pub number: u32,
    pub kind: BandKindName,
    pub hz: f32,
    pub q: f32,
    pub gain_db: f32,
}

/// State of one track's effect chain.
#[derive(Serialize, Clone, Debug)]
#[serde(rename_all = "snake_case")]
pub struct FxEvent {
    /// Whole chain out of the signal path. While this is true the track is passed through
    /// bit-identically, whatever the rest of these fields say.
    pub bypass: bool,
    pub preset: FxPresetName,
    /// German word for the preset, ready to print.
    pub preset_label: String,
    /// `HEK-R`: one letter per effect that is on, a dash for one that is off. Always five
    /// characters, in signal order.
    pub letters: String,
    /// On/off per effect, in signal order, next to the name the commands use.
    pub effects: Vec<FxEffectEvent>,

    pub high_pass_hz: f32,
    pub bands: Vec<BandEvent>,

    pub comp_threshold_db: f32,
    pub comp_ratio: f32,
    pub comp_attack_ms: f32,
    pub comp_release_ms: f32,
    pub comp_knee_db: f32,
    pub comp_makeup_db: f32,
    /// Deepest gain reduction since the last status event, in dB and never positive. A meter.
    pub comp_reduction_db: f32,

    pub delay_note: DelayNoteName,
    /// The note value as it is written on paper: `1/4`, `1/8.`, `1/8`, `1/8T`.
    pub delay_note_label: String,
    /// The delay time the note value works out to at the current tempo, in samples. This is the
    /// number the engine's timeline dictates - there is no millisecond setting to get wrong.
    pub delay_samples: u64,
    pub delay_ms: f64,
    pub delay_feedback: f32,
    pub delay_mix: f32,

    pub reverb_size: f32,
    pub reverb_damping: f32,
    pub reverb_mix: f32,
}

#[derive(Serialize, Clone, Debug)]
#[serde(rename_all = "snake_case")]
pub struct FxEffectEvent {
    /// Pass this back as `effect` in `fx_enable`.
    pub name: FxSlotName,
    /// German word, ready to print.
    pub label: String,
    pub on: bool,
}

/// Build the effect part of a track's status event.
pub fn fx_event(fx: &FxStatus, sample_rate: u32) -> FxEvent {
    let s = &fx.settings;
    let rate = sample_rate.max(1) as f64;
    FxEvent {
        bypass: fx.bypass,
        preset: fx.preset.into(),
        preset_label: fx.preset.label().to_string(),
        letters: fx.letters(),
        effects: FxSlot::all()
            .iter()
            .map(|&slot| FxEffectEvent {
                name: slot.into(),
                label: slot.label().to_string(),
                on: s.enabled[slot.index()],
            })
            .collect(),
        high_pass_hz: s.high_pass_hz,
        bands: s
            .bands
            .iter()
            .enumerate()
            .map(|(i, b)| BandEvent {
                index: i,
                number: i as u32 + 1,
                kind: b.kind.into(),
                hz: b.hz,
                q: b.q,
                gain_db: b.gain_db,
            })
            .collect(),
        comp_threshold_db: s.comp.threshold_db,
        comp_ratio: s.comp.ratio,
        comp_attack_ms: s.comp.attack_ms,
        comp_release_ms: s.comp.release_ms,
        comp_knee_db: s.comp.knee_db,
        comp_makeup_db: s.comp.makeup_db,
        comp_reduction_db: fx.reduction_db,
        delay_note: s.delay_note.into(),
        delay_note_label: s.delay_note.label().to_string(),
        delay_samples: fx.delay_samples,
        delay_ms: fx.delay_samples as f64 * 1000.0 / rate,
        delay_feedback: s.delay_feedback,
        delay_mix: s.delay_mix,
        reverb_size: s.reverb_size,
        reverb_damping: s.reverb_damping,
        reverb_mix: s.reverb_mix,
    }
}

// ---------------------------------------------------------------------------------------------
// Calibration and app info
// ---------------------------------------------------------------------------------------------

/// Parameters of a latency check. The device part is the same as in [`StartConfig`].
#[derive(Deserialize, Clone, Debug)]
#[serde(rename_all = "snake_case")]
pub struct CalibrateConfig {
    #[serde(default = "default_host")]
    pub host: String,
    #[serde(default)]
    pub device: Option<String>,
    #[serde(default = "default_sample_rate")]
    pub sample_rate: u32,
    #[serde(default = "default_buffer_frames")]
    pub buffer_frames: u32,
    #[serde(default)]
    pub input_channels: Option<u16>,
    #[serde(default)]
    pub output_channels: Option<u16>,
    #[serde(default)]
    pub force_buffer: bool,

    #[serde(default = "default_bpm")]
    pub bpm: f64,
    /// Length of one measurement recording in bars.
    #[serde(default = "default_calibrate_bars")]
    pub bars: u32,
    /// The value to check.
    #[serde(default = "default_latency_samples")]
    pub latency_samples: u64,
    #[serde(default = "default_runs")]
    pub runs: u32,
    #[serde(default = "default_gain")]
    pub click_gain: f32,
}

fn default_calibrate_bars() -> u32 {
    4
}
fn default_runs() -> u32 {
    5
}

/// Result of a calibration run. The measurement itself writes its full report - every run, every
/// deviation, the recommendation - into the log file, because that is where the existing
/// `calibrate` code prints it.
#[derive(Serialize, Clone, Debug)]
#[serde(rename_all = "snake_case")]
pub struct CalibrateOutcome {
    pub log_path: String,
    /// German summary, pointing at the log file for the numbers.
    pub message: String,
}

/// Static facts about this build. Emitted once as `looper://ready` and always readable via the
/// `app_info` command - a frontend that starts listening late uses the command.
#[derive(Serialize, Clone, Debug)]
#[serde(rename_all = "snake_case")]
pub struct AppInfo {
    pub version: String,
    /// Full path of the log file this run writes to.
    pub log_path: String,
    /// Whether diagnostics are actually being written. False only if the log file could not be
    /// opened; the calibration refuses to run in that case, because it reports through the log.
    pub logging: bool,
    pub asio_built: bool,
    pub max_tracks: u32,
    pub max_layers: u32,
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{Value, json};

    const RATE: u32 = 48_000;

    fn sample_status() -> TrackStatus {
        TrackStatus {
            state: TrackState::Playing,
            layers: 3,
            // Layer 2 (one-based) is muted.
            muted_mask: 0b010,
            loop_len: 96_000,
            origin: 0,
            filled: 0,
            input_peak: [0.5, 0.0],
            output_peak: [1.0, 1.0],
            monitor: true,
            playing: true,
            // Zero-based inside the engine; input 2 as printed on the interface.
            input_channels: [1, 0],
            channels: 1,
            pan: 0.0,
            // A track nobody has touched: chain bypassed, everything off, "trocken".
            fx: FxStatus::default(),
        }
    }

    /// A stereo track on inputs 3 and 4, placed a little to the left.
    fn stereo_status() -> TrackStatus {
        TrackStatus {
            channels: 2,
            input_channels: [2, 3],
            input_peak: [0.5, 0.25],
            output_peak: [0.8, 0.2],
            pan: -0.25,
            ..sample_status()
        }
    }

    /// A chain with a preset in it, for the effect part of the wire format.
    fn voice_status() -> FxStatus {
        let mut chain = looper_engine::engine::fx::Chain::new(RATE);
        chain.load_preset(FxPreset::Voice);
        chain.set_quarter_samples(RATE as f64 * 60.0 / 120.0);
        chain.status()
    }

    /// The one conversion the whole frontend depends on: engine snapshot in, display numbers out.
    #[test]
    fn a_track_snapshot_becomes_the_wire_format_with_the_right_numbering() {
        let event = track_event(
            0,
            "gitarre",
            &sample_status(),
            &[1.0, 1.0, 0.25],
            RATE,
            Lead::default(),
        );
        assert_eq!(event.index, 0);
        assert_eq!(event.input_channel, 2, "Kanal wird eins-basiert gemeldet");
        assert_eq!(event.input_channels, vec![2], "mono: genau ein Eingang");
        assert_eq!(event.channels, 1);
        assert_eq!(event.channels_label, "mono");
        assert_eq!(event.pan, 0.0);
        assert_eq!(event.input_peaks, vec![0.5], "mono: ein Pegel");
        assert_eq!(event.output_peaks, vec![1.0, 1.0], "der Bus ist immer stereo");
        assert_eq!(event.state, TrackStateName::Playing);
        assert_eq!(event.state_label, "Wiedergabe");
        assert_eq!(event.loop_samples, 96_000);
        assert!((event.loop_seconds - 2.0).abs() < 1e-9);
        assert_eq!(event.layers.len(), 3);
        assert_eq!(event.layers[0].number, 1);
        assert_eq!(event.layers[0].index, 0);
        assert!(!event.layers[0].muted);
        assert!(event.layers[1].muted, "Bit 1 der Maske ist Ebene 2");
        assert_eq!(event.layers[2].gain, 0.25);
        // Full scale is 0 dBFS, half of it is about -6.
        assert_eq!(event.output_dbfs, Some(0.0));
        assert!((event.input_dbfs.unwrap() + 6.02).abs() < 0.01);
        assert_eq!(event.pending_kind, None, "ohne Vorlauf ist nichts geplant");
        assert_eq!(event.pending_label, None);
        assert_eq!(event.pending_bars, 0);
        assert_eq!(event.pending_beats, 0);
    }

    /// A stereo track reports both of its inputs, both of its input meters and a channel count the
    /// frontend can switch on - and the single-number levels stay the louder of the two, so a
    /// frontend that draws one bar per meter needs no case distinction.
    #[test]
    fn a_stereo_track_reports_both_inputs_and_both_meters() {
        let event = track_event(0, "klavier", &stereo_status(), &[], RATE, Lead::default());
        assert_eq!(event.channels, 2);
        assert_eq!(event.channels_label, "stereo");
        assert_eq!(event.input_channel, 3, "der linke Eingang, eins-basiert");
        assert_eq!(event.input_channels, vec![3, 4]);
        assert_eq!(event.input_peaks, vec![0.5, 0.25]);
        assert_eq!(event.input_peak, 0.5, "die Einzahl ist der lautere Kanal");
        assert_eq!(event.output_peaks, vec![0.8, 0.2]);
        assert_eq!(event.output_peak, 0.8);
        assert_eq!(event.pan, -0.25);

        let value = serde_json::to_value(&event).unwrap();
        assert_eq!(value["channels"], json!(2));
        assert_eq!(value["input_channels"], json!([3, 4]));
        assert_eq!(value["channels_label"], json!("stereo"));
    }

    /// A stereo track is set up by naming a second input; a pan travels with it. Both default, so
    /// the minimal configuration above stays what it was.
    #[test]
    fn a_start_configuration_can_name_a_stereo_pair_and_a_pan() {
        let config: StartConfig = serde_json::from_value(json!({
            "tracks": [
                {"name": "stimme", "input_channel": 1, "pan": -0.3},
                {"name": "klavier", "input_channel": 3, "input_channel_right": 4}
            ]
        }))
        .expect("Stereo-Konfiguration");
        assert_eq!(config.tracks[0].channels(), 1);
        assert_eq!(config.tracks[0].pan, -0.3);
        assert_eq!(config.tracks[1].channels(), 2);
        assert_eq!(config.tracks[1].input_channel_right, Some(4));
        assert_eq!(config.tracks[1].pan, 0.0);

        // And back out again as the engine reports it.
        let value = serde_json::to_value(&config.tracks[1]).unwrap();
        assert_eq!(value["input_channel"], json!(3));
        assert_eq!(value["input_channel_right"], json!(4));
    }

    /// The count-in is what the stage screen prints in big letters, so both halves of it - the
    /// German word and the two numbers - have to survive the conversion.
    #[test]
    fn a_scheduled_action_reaches_the_frontend_as_a_countdown() {
        let lead = Lead {
            kind: Some(PendingKind::Record),
            bars: 5,
            beats: 2,
        };
        let event = track_event(0, "gitarre", &sample_status(), &[], RATE, lead);
        assert_eq!(event.pending_kind, Some(PendingKindName::Record));
        assert_eq!(event.pending_label.as_deref(), Some("Aufnahme"));
        assert_eq!(event.pending_bars, 5);
        assert_eq!(event.pending_beats, 2);

        let value = serde_json::to_value(&event).unwrap();
        assert_eq!(value["pending_kind"], json!("record"));
        assert_eq!(value["pending_label"], json!("Aufnahme"));

        // Every kind the scheduler can announce has a snake_case name and a German word.
        for (kind, wire, label) in [
            (PendingKind::Record, "record", "Aufnahme"),
            (PendingKind::Overdub, "overdub", "Overdub"),
            (PendingKind::Play, "play", "Wiedergabe"),
            (PendingKind::Stop, "stop", "Stopp"),
        ] {
            let name: PendingKindName = kind.into();
            assert_eq!(serde_json::to_value(name).unwrap(), json!(wire));
            assert_eq!(kind.label(), label);
        }
    }

    /// The grid is part of the wire format in both directions: it comes in with the start
    /// configuration and goes back out with every status, so the window can show what is set.
    #[test]
    fn the_quantisation_mode_travels_as_bar_or_loop() {
        assert_eq!(
            serde_json::to_value(QuantizeName::Loop).unwrap(),
            json!("loop")
        );
        assert_eq!(serde_json::to_value(QuantizeName::Bar).unwrap(), json!("bar"));
        assert_eq!(QuantizeName::default(), QuantizeName::Loop);
        // Round trip through the engine's own enum, in both directions.
        for name in [QuantizeName::Bar, QuantizeName::Loop] {
            let engine: Quantize = name.into();
            assert_eq!(QuantizeName::from(engine), name);
        }
        assert_eq!(Quantize::from(QuantizeName::Loop), Quantize::Loop);
    }

    /// A gain mirror shorter than the engine's layer count must not lose layers - the missing ones
    /// are simply unity, which is what a layer starts at.
    #[test]
    fn missing_mirrored_gains_default_to_one() {
        let event = track_event(1, "stimme", &sample_status(), &[], RATE, Lead::default());
        assert_eq!(event.layers.len(), 3);
        assert!(event.layers.iter().all(|l| l.gain == 1.0));
    }

    #[test]
    fn silence_has_no_decibel_value() {
        assert_eq!(dbfs(0.0), None, "minus unendlich gibt es in JSON nicht");
        assert_eq!(dbfs(1.0), Some(0.0));
        assert_eq!(
            track_event(0, "t", &TrackStatus::default(), &[], RATE, Lead::default()).input_dbfs,
            None
        );
    }

    /// Every state the engine can report must survive the round trip as a snake_case string, and
    /// keep its German label - the frontend switches on the one and prints the other.
    #[test]
    fn track_states_serialise_as_snake_case() {
        let pairs = [
            (TrackState::Empty, "empty", "leer"),
            (TrackState::Armed, "armed", "scharf"),
            (TrackState::Recording, "recording", "Aufnahme"),
            (TrackState::Overdub, "overdub", "Overdub"),
            (TrackState::Ready, "ready", "bereit"),
            (TrackState::Playing, "playing", "Wiedergabe"),
        ];
        for (state, wire, label) in pairs {
            let name: TrackStateName = state.into();
            assert_eq!(serde_json::to_value(name).unwrap(), json!(wire));
            assert_eq!(state.label(), label);
        }
    }

    /// The contract the frontend is written against: these keys, spelled this way.
    #[test]
    fn the_status_event_has_exactly_the_documented_keys() {
        let value = serde_json::to_value(StatusEvent::idle()).unwrap();
        let object = value.as_object().expect("Objekt");
        let mut keys: Vec<&str> = object.keys().map(String::as_str).collect();
        keys.sort_unstable();
        assert_eq!(
            keys,
            [
                "bar",
                "bars",
                "beat",
                "beat_offset",
                "beat_unit",
                "beats_per_bar",
                "bpm",
                "click",
                "fifo_overruns",
                "fifo_underruns",
                "ignored_commands",
                "latency_samples",
                "loop_samples",
                "loop_seconds",
                "max_callback_ms",
                "message",
                "other_errors",
                "output_dbfs",
                "output_peak",
                "output_peaks",
                "pos",
                "quantize",
                "running",
                "sample_rate",
                "samples_per_beat",
                "seconds",
                "spares",
                "tracks",
                "xruns",
            ]
        );
        assert_eq!(object["running"], json!(false));
        assert_eq!(object["tracks"], json!([]));
        assert_eq!(object["message"], Value::Null);
    }

    #[test]
    fn a_track_entry_has_exactly_the_documented_keys() {
        let event = track_event(
            0,
            "gitarre",
            &sample_status(),
            &[1.0, 1.0, 1.0],
            RATE,
            Lead::default(),
        );
        let value = serde_json::to_value(&event).unwrap();
        let mut keys: Vec<&str> = value
            .as_object()
            .expect("Objekt")
            .keys()
            .map(String::as_str)
            .collect();
        keys.sort_unstable();
        assert_eq!(
            keys,
            [
                "channels",
                "channels_label",
                "filled_samples",
                "fx",
                "index",
                "input_channel",
                "input_channels",
                "input_dbfs",
                "input_peak",
                "input_peaks",
                "layers",
                "loop_samples",
                "loop_seconds",
                "monitor",
                "name",
                "output_dbfs",
                "output_peak",
                "output_peaks",
                "pan",
                "pending_bars",
                "pending_beats",
                "pending_kind",
                "pending_label",
                "playing",
                "state",
                "state_label",
            ]
        );
        assert_eq!(
            serde_json::to_value(event.layers[0]).unwrap(),
            json!({"index": 0, "number": 1, "muted": false, "gain": 1.0})
        );
    }

    // ------------------------------------------------------------------------------------------
    // Effects
    // ------------------------------------------------------------------------------------------

    /// Every effect enum has to survive the round trip through its wire name, in both directions -
    /// the frontend switches on these strings and sends them straight back.
    #[test]
    fn every_effect_enum_travels_as_snake_case_in_both_directions() {
        for (slot, wire, label) in [
            (FxSlot::HighPass, "high_pass", "Hochpass"),
            (FxSlot::Eq, "eq", "EQ"),
            (FxSlot::Comp, "comp", "Kompressor"),
            (FxSlot::Delay, "delay", "Delay"),
            (FxSlot::Reverb, "reverb", "Hall"),
        ] {
            let name: FxSlotName = slot.into();
            assert_eq!(serde_json::to_value(name).unwrap(), json!(wire));
            assert_eq!(FxSlot::from(name), slot);
            assert_eq!(slot.label(), label);
        }

        for (preset, wire, label) in [
            (FxPreset::Dry, "dry", "trocken"),
            (FxPreset::Voice, "voice", "stimme"),
            (FxPreset::PiezoGuitar, "piezo_guitar", "gitarre"),
            (FxPreset::Custom, "custom", "eigen"),
        ] {
            let name: FxPresetName = preset.into();
            assert_eq!(serde_json::to_value(name).unwrap(), json!(wire));
            assert_eq!(FxPreset::from(name), preset);
            assert_eq!(preset.label(), label);
        }

        for (note, wire, label) in [
            (DelayNote::Quarter, "quarter", "1/4"),
            (DelayNote::DottedEighth, "dotted_eighth", "1/8."),
            (DelayNote::Eighth, "eighth", "1/8"),
            (DelayNote::TripletEighth, "triplet_eighth", "1/8T"),
        ] {
            let name: DelayNoteName = note.into();
            assert_eq!(serde_json::to_value(name).unwrap(), json!(wire));
            assert_eq!(DelayNote::from(name), note);
            assert_eq!(note.label(), label);
        }

        for (kind, wire) in [
            (BandKind::Peak, "peak"),
            (BandKind::LowShelf, "low_shelf"),
            (BandKind::HighShelf, "high_shelf"),
        ] {
            let name: BandKindName = kind.into();
            assert_eq!(serde_json::to_value(name).unwrap(), json!(wire));
            assert_eq!(BandKind::from(name), kind);
        }
    }

    /// The chain state as the window sees it: which preset, what is on, and the tempo-synchronous
    /// delay time already converted into samples and milliseconds.
    #[test]
    fn a_chain_becomes_the_wire_format_with_its_preset_and_its_delay_time() {
        let event = fx_event(&voice_status(), RATE);
        assert!(!event.bypass, "das Stimme-Preset schaltet die Kette ein");
        assert_eq!(event.preset, FxPresetName::Voice);
        assert_eq!(event.preset_label, "stimme");
        assert_eq!(event.letters, "HEK-R", "Delay ist in diesem Preset aus");
        assert_eq!(event.effects.len(), 5);
        assert_eq!(event.effects[0].name, FxSlotName::HighPass);
        assert_eq!(event.effects[0].label, "Hochpass");
        assert!(event.effects[0].on);
        assert!(!event.effects[3].on, "Delay aus");

        assert_eq!(event.high_pass_hz, 80.0);
        assert_eq!(event.bands.len(), 3);
        assert_eq!(event.bands[0].index, 0);
        assert_eq!(event.bands[0].number, 1);
        assert_eq!(event.bands[1].hz, 3_000.0, "das Praesenzband");
        assert_eq!(
            event.bands[2].kind,
            BandKindName::HighShelf,
            "das oberste Band ist ein Kuhschwanz, kein Glockenfilter"
        );
        assert_eq!(event.comp_ratio, 3.0);
        assert_eq!(event.comp_makeup_db, 6.0);

        // Dotted eighth at 120 BPM and 48 kHz: 24000 * 0.75.
        assert_eq!(event.delay_note, DelayNoteName::DottedEighth);
        assert_eq!(event.delay_note_label, "1/8.");
        assert_eq!(event.delay_samples, 18_000);
        assert!((event.delay_ms - 375.0).abs() < 1e-9);

        // A fresh track reports the dry, bypassed state.
        let idle = fx_event(&FxStatus::default(), RATE);
        assert!(idle.bypass);
        assert_eq!(idle.preset, FxPresetName::Dry);
        assert_eq!(idle.letters, "-----");
    }

    /// The generic knob command: a name plus a value, and the three band-scoped names additionally
    /// need a band. Both mistakes a frontend can make get a German sentence, not a panic.
    #[test]
    fn a_named_parameter_becomes_an_engine_parameter_or_a_german_complaint() {
        assert_eq!(
            FxParamName::CompRatio.to_param(3.5, None).unwrap(),
            FxParam::CompRatio(3.5)
        );
        assert_eq!(
            FxParamName::HighPassHz.to_param(90.0, None).unwrap(),
            FxParam::HighPassHz(90.0)
        );
        assert_eq!(
            FxParamName::BandGainDb.to_param(-4.0, Some(1)).unwrap(),
            FxParam::BandGainDb {
                band: 1,
                db: -4.0
            }
        );
        assert_eq!(
            FxParamName::BandQ.to_param(2.0, Some(2)).unwrap(),
            FxParam::BandQ { band: 2, q: 2.0 }
        );

        let err = FxParamName::BandHz.to_param(1_000.0, None).expect_err("ohne Band");
        assert!(err.contains("band"), "{err}");
        let err = FxParamName::BandHz
            .to_param(1_000.0, Some(7))
            .expect_err("Band 7 gibt es nicht");
        assert!(err.contains("0 bis 2"), "{err}");
        // A knob that is not band-scoped ignores a band rather than refusing it.
        assert!(FxParamName::ReverbMix.to_param(0.2, Some(9)).is_ok());
        assert_eq!(
            serde_json::to_value(FxParamName::CompThresholdDb).unwrap(),
            json!("comp_threshold_db")
        );
    }

    /// A frontend should be able to send only the tracks and get the values phase 0 measured on
    /// this machine for everything else.
    #[test]
    fn a_start_configuration_needs_nothing_but_its_tracks() {
        let config: StartConfig = serde_json::from_value(json!({
            "tracks": [{"name": "stimme", "input_channel": 1}]
        }))
        .expect("Minimalkonfiguration");
        assert_eq!(config.host, "asio");
        assert_eq!(config.device, None);
        assert_eq!(config.sample_rate, 48_000);
        assert_eq!(config.buffer_frames, 128);
        assert_eq!(config.bpm, 100.0);
        assert_eq!(config.beats_per_bar, 4);
        assert_eq!(config.beat_unit, 4);
        assert_eq!(config.bars, 8);
        assert_eq!(
            config.quantize,
            QuantizeName::Loop,
            "ohne Angabe rastet eine Aufnahme auf den Loop-Anfang ein"
        );
        assert_eq!(config.latency_samples, 827);
        assert_eq!(config.monitor_gain, 1.0);
        assert_eq!(config.click_gain, 1.0);
        assert!(config.click, "der Klick ist standardmaessig an");
        assert!(!config.monitor, "Mithoeren ist standardmaessig aus");
        assert!(!config.force_buffer);
        assert_eq!(
            config.tracks,
            vec![TrackConfig::mono("stimme", 1)],
            "ohne zweiten Kanal und ohne Panorama: ein Mono-Track in der Mitte"
        );
    }

    #[test]
    fn a_start_configuration_takes_every_field_the_ui_can_set() {
        let config: StartConfig = serde_json::from_value(json!({
            "host": "wasapi",
            "device": "Scarlett",
            "sample_rate": 44_100,
            "buffer_frames": 480,
            "input_channels": 2,
            "output_channels": 2,
            "force_buffer": true,
            "tracks": [
                {"name": "stimme", "input_channel": 1},
                {"name": "gitarre", "input_channel": 2, "pan": 0.4}
            ],
            "bpm": 137.0,
            "beats_per_bar": 7,
            "beat_unit": 8,
            "bars": 4,
            "quantize": "bar",
            "latency_samples": 900,
            "monitor_gain": 0.5,
            "click_gain": 0.25,
            "click": false,
            "monitor": true
        }))
        .expect("Vollkonfiguration");
        assert_eq!(config.host, "wasapi");
        assert_eq!(config.device.as_deref(), Some("Scarlett"));
        assert_eq!(config.input_channels, Some(2));
        assert_eq!(config.tracks.len(), 2);
        assert_eq!(config.beats_per_bar, 7);
        assert_eq!(config.beat_unit, 8);
        assert!(config.force_buffer);
        assert!(!config.click);
        assert!(config.monitor);
        assert_eq!(config.quantize, QuantizeName::Bar);
    }

    #[test]
    fn a_calibration_configuration_defaults_to_the_measurement_from_phase_zero() {
        let config: CalibrateConfig = serde_json::from_value(json!({})).expect("leer geht");
        assert_eq!(config.host, "asio");
        assert_eq!(config.sample_rate, 48_000);
        assert_eq!(config.buffer_frames, 128);
        assert_eq!(config.bpm, 100.0);
        assert_eq!(config.bars, 4);
        assert_eq!(config.runs, 5);
        assert_eq!(config.latency_samples, 827);
        assert_eq!(config.click_gain, 1.0);
    }

    #[test]
    fn the_device_report_keeps_the_names_the_start_configuration_expects() {
        let report = DeviceReport {
            asio_built: true,
            hosts: vec![HostInfo {
                name: "asio".to_string(),
                error: None,
                default_input: Some("Focusrite USB ASIO".to_string()),
                default_output: Some("Focusrite USB ASIO".to_string()),
                devices: vec![DeviceInfo {
                    name: "Focusrite USB ASIO".to_string(),
                    input: true,
                    output: true,
                    input_configs: vec![ConfigInfo {
                        channels: 2,
                        min_sample_rate: 44_100,
                        max_sample_rate: 48_000,
                        sample_format: "I32".to_string(),
                        min_buffer_frames: Some(16),
                        max_buffer_frames: Some(1024),
                    }],
                    output_configs: Vec::new(),
                    note: None,
                }],
            }],
        };
        let value = serde_json::to_value(&report).unwrap();
        assert_eq!(value["hosts"][0]["name"], json!("asio"));
        assert_eq!(value["hosts"][0]["devices"][0]["input_configs"][0]["min_buffer_frames"], json!(16));
        assert_eq!(value["hosts"][0]["devices"][0]["output_configs"], json!([]));
        assert_eq!(value["hosts"][0]["error"], Value::Null);

        // An unknown buffer range must survive as null, not as a made-up number.
        let unknown = ConfigInfo {
            channels: 2,
            min_sample_rate: 48_000,
            max_sample_rate: 48_000,
            sample_format: "F32".to_string(),
            min_buffer_frames: None,
            max_buffer_frames: None,
        };
        assert_eq!(
            serde_json::to_value(unknown).unwrap()["min_buffer_frames"],
            Value::Null
        );
    }
}
