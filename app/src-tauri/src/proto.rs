//! The wire format between the Rust side and the web view.
//!
//! Everything here is `serde` with `rename_all = "snake_case"`, so what the frontend sees is
//! exactly what the field names below say. Two conventions run through the whole file and are the
//! only thing that needs remembering:
//!
//! * **Indices are zero-based.** `track` and `layer` in a command are positions in the `tracks`
//!   and `layers` arrays of [`StatusEvent`]. Nothing has to be counted twice.
//! * **Hardware numbers are one-based**, because that is how they are printed on the interface.
//!   That affects exactly one field, `input_channel`, and every layer additionally carries a
//!   `number` for display next to its zero-based `index`.
//!
//! Levels are reported twice: `*_peak` as a linear absolute value in `0.0..=1.0` for a meter bar,
//! and `*_dbfs` for a number to print. `*_dbfs` is `null` at digital silence, because minus
//! infinity has no JSON representation.

use serde::{Deserialize, Serialize};

use looper_engine::engine::command::TrackStatus;
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

/// One track as the user set it up: a name and the input it listens on.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub struct TrackConfig {
    pub name: String,
    /// One-based, as printed on the interface.
    pub input_channel: u32,
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
    /// One-based, as printed on the interface.
    pub input_channel: u32,
    pub state: TrackStateName,
    /// German word for the state, ready to print.
    pub state_label: String,
    pub monitor: bool,
    pub playing: bool,
    /// Loop length of this track in samples; 0 while nothing is recorded.
    pub loop_samples: u64,
    pub loop_seconds: f64,
    /// Samples already written during a running loop-defining take.
    pub filled_samples: u64,
    pub input_peak: f32,
    pub input_dbfs: Option<f64>,
    /// Peak this track's audible layers contributed to the output. Monitoring is not part of it.
    pub output_peak: f32,
    pub output_dbfs: Option<f64>,
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
    pub output_peak: f32,
    pub output_dbfs: Option<f64>,
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
    TrackStatusEvent {
        index,
        name: name.to_string(),
        input_channel: ts.input_channel as u32 + 1,
        state: ts.state.into(),
        state_label: ts.state.label().to_string(),
        monitor: ts.monitor,
        playing: ts.playing,
        loop_samples: ts.loop_len,
        loop_seconds: ts.loop_len as f64 / rate,
        filled_samples: ts.filled,
        input_peak: ts.input_peak,
        input_dbfs: dbfs(ts.input_peak),
        output_peak: ts.output_peak,
        output_dbfs: dbfs(ts.output_peak),
        layers,
        pending_kind: lead.kind.map(PendingKindName::from),
        pending_label: lead.kind.map(|k| k.label().to_string()),
        pending_bars: lead.bars,
        pending_beats: lead.beats,
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
            input_peak: 0.5,
            output_peak: 1.0,
            monitor: true,
            playing: true,
            // Zero-based inside the engine; input 2 as printed on the interface.
            input_channel: 1,
        }
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
                "filled_samples",
                "index",
                "input_channel",
                "input_dbfs",
                "input_peak",
                "layers",
                "loop_samples",
                "loop_seconds",
                "monitor",
                "name",
                "output_dbfs",
                "output_peak",
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
            vec![TrackConfig {
                name: "stimme".to_string(),
                input_channel: 1
            }]
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
                {"name": "gitarre", "input_channel": 2}
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
