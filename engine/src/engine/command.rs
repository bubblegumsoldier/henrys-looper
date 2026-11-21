//! Commands with a musical timestamp, the status snapshot coming back, and the buffer traffic
//! between the two threads.
//!
//! The control thread may send a command arbitrarily early; the audio thread executes it at the
//! sample it is stamped for. That is what makes the engine independent of thread scheduling: no
//! amount of jitter between the threads can move a musical event, as long as the command arrives
//! before its time.
//!
//! Everything crossing the boundary is `Copy` and fixed size, so nothing is allocated, freed or
//! locked on the audio side. The only exception is the layer buffers themselves, which are
//! allocated *and zeroed* in the control thread and handed over as whole `Vec`s through
//! [`BufferChannel`] / [`LayerPool`]:
//!
//! ```text
//!  Steuer-Thread                                        Audio-Thread
//!  LayerPool.reserve ──push──► install-Queue ──pop──► EngineCore.spares
//!        ▲                                                  │ take_spare()
//!        │                                                  ▼
//!        └────────pop──── retire-Queue ◄──push──── Layer eines Tracks
//! ```
//!
//! No `Vec` is ever created or dropped on the right-hand side of that picture.
//!
//! # Two kinds of buffer
//!
//! Since loop buffers are mono *or* stereo (see [`super::frame`]), the pool keeps two stocks: one
//! of `layer_frames` samples and one of `2 * layer_frames`. Everything below that is indexed by
//! kind is indexed by `Channels::index` - mono is 0, stereo is 1 - so the accounting is the same
//! arithmetic twice rather than two code paths. A track only ever asks for its own kind, and a
//! buffer of the wrong length is handed straight back instead of being used.

use rtrb::{Consumer, Producer, PushError, RingBuffer};

use super::bus::{
    BUS_CHANNELS, BUS_COUNT, Bus, BusOutput, BusRouting, BusSend, OUT_METERS, TrackSource,
};
#[cfg(test)]
use super::frame::Channels;
use super::fx::{FxParam, FxPreset, FxSlot, FxStatus};
use super::timeline::TimeSignature;
use super::track::{TrackLatency, TrackState};

/// Upper end of the click and monitor volumes, linear - the same headroom a layer gain has.
pub const MAX_LEVEL_GAIN: f32 = 4.0;

/// Hard ceiling of tracks. Bounds the fixed-size status snapshot, which has to stay `Copy`.
pub const MAX_TRACKS: usize = 8;

/// How many kinds of layer buffer there are: mono and stereo.
pub const BUFFER_KINDS: usize = 2;

/// One scheduled engine action.
///
/// `at` is an absolute position on the engine's sample timeline - the same axis the audio callback
/// counts in. Commands without `at` take effect at the next block boundary; they carry no musical
/// meaning that would be worth a sample-accurate timestamp.
///
/// `track` and `layer` are zero-based indices; user-facing numbering adds one.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Command {
    /// Begin a *new* loop on this track: existing layers are returned and the first layer defines
    /// origin and loop length. `at` is the position of the first musical sample of the loop.
    StartRecord { track: usize, at: u64 },
    /// Begin a *further* layer on the existing loop of this track. On an empty track this behaves
    /// exactly like `StartRecord`, so an overdub can never fail just because nothing is there yet.
    StartOverdub { track: usize, at: u64 },
    /// End the running take; `at` is the position just after its last musical sample, so a
    /// loop-defining take produces a loop of exactly `at - start` samples.
    StopRecord { track: usize, at: u64 },
    /// Keep overdubbing: while on, every take of this track that ends at its loop boundary is
    /// followed seamlessly by a further overdub pass, each one its own layer, and a pass that
    /// stayed practically silent is dropped instead of kept. Off lets the running pass finish and
    /// starts no further one. See `super::latch` for the whole behaviour.
    ///
    /// Timed like a take: `at: 0` (long past) acts in the next block, which is what a button press
    /// wants; the score runner stamps it, so the latch goes on with the first pass of a `stack`
    /// section and off inside its last one, both scheduled bars ahead.
    LatchOverdub { track: usize, on: bool, at: u64 },
    StartPlay { track: usize, at: u64 },
    StopPlay { track: usize, at: u64 },
    /// Re-anchor this track's loop at `at`: from there on it plays from its top. Sent by the score
    /// runner when a section brings a loop back in (or asks for it with `restart: true`) and the
    /// section does not begin on the loop's own grid. `play`: a `StartPlay` for the same `at`
    /// follows, and the loop may already be read ahead from `at` on, so a track whose plugins or
    /// chain report latency begins with its very first frame.
    ///
    /// It acts **early**, as soon as the track's read-ahead reaches `at` (see
    /// `EngineCore::next_due`); each axis switches when it reaches `at` itself (`LoopGrid::restart`).
    /// Refused as `Busy` while the take defining the loop is still open across `at`.
    RestartLoop { track: usize, at: u64, play: bool },
    /// Throw this track's layers away; the buffers go back to the control thread.
    ClearTrack { track: usize, at: u64 },
    /// Reset every track to the state of a fresh start and return every buffer. Commands that were
    /// sent before it and are still waiting for their time are dropped with it - a fresh start has
    /// nothing scheduled.
    ClearAll { at: u64 },
    /// Call back everything that was sent before it and is still waiting at `at`: those commands
    /// never act. Nothing else changes - no track is stopped, cleared or reset by this alone. The
    /// score's emergency brake ("Stop all", `Runner::stop_all`): a run schedules its sections,
    /// takes and re-anchorings bars ahead, and a stop has to take all of that back at once rather
    /// than answer every waiting command with a counter-command where it lands. What is sent after
    /// it (the stop itself) stays. Timed, so whatever was due before `at` still happens in order.
    DropWaiting { at: u64 },
    /// Input monitoring (hearing yourself through the engine) for one track, independent of whether
    /// that track is playing.
    SetMonitor { track: usize, on: bool },
    /// Where this track sits in the stereo field: -1.0 hard left, 0.0 centre, +1.0 hard right.
    /// Untimed - a pan is a mix decision, not a musical event, and it is set before a take rather
    /// than during one.
    SetPan { track: usize, pan: f32 },
    /// The channel fader, linear, clamped to `0..=MAX_TRACK_GAIN` (+12 dB). After the chain and
    /// the plugin inserts, before the panner; smoothed, so a jump is a short ramp, not a click.
    /// Untimed, and like every channel-strip command it never reaches a recording.
    SetTrackGain { track: usize, gain: f32 },
    /// Channel mute: the strip is held at silence on both buses. Listening only.
    SetTrackMute { track: usize, on: bool },
    /// Channel solo: while any track is soloed, every track that is not is held at silence.
    /// Listening only - a soloed-away track records exactly what it would have recorded.
    SetTrackSolo { track: usize, on: bool },
    /// Which output buses one source of one track feeds. Untimed and deliberately harmless: it
    /// changes the output stage only, never a layer buffer, so it can be pressed during a take.
    SetTrackSend {
        track: usize,
        source: TrackSource,
        send: BusSend,
    },
    /// Volume of one output bus, linear, clamped to `0..MAX_BUS_GAIN`. Untimed. The whole point of
    /// two buses: turning the headphones down must not change the room.
    SetBusGain { bus: Bus, gain: f32 },
    /// Which device output channels one bus leaves on. Untimed, and clamped by the engine against
    /// what the device really has.
    SetBusOutput { bus: Bus, out: BusOutput },
    /// Latency compensation of one track, in frames. Untimed: it decides where *later* input
    /// frames are stored and has no business moving anything already recorded - a take that is
    /// running keeps writing with the new value from the next frame on, which is the only
    /// behaviour that does not rewrite the past.
    SetTrackLatency {
        track: usize,
        latency: TrackLatency,
    },
    SetLayerMute {
        track: usize,
        layer: usize,
        muted: bool,
    },
    SetLayerGain {
        track: usize,
        layer: usize,
        gain: f32,
    },
    /// Remove one layer; its buffer travels back to the control thread.
    RemoveLayer { track: usize, layer: usize },
    /// The looper's undo: throw away what this track recorded last. A take that is running (or
    /// armed, or queued) is cancelled together with its layer - in a continuous overdub that ends
    /// it; otherwise the newest layer goes. The last layer leaves the track empty. Untimed.
    UndoLayer { track: usize },
    /// What this track's takes store: the notes (`midi: true`, a MIDI loop - instrument tracks
    /// only) or the sound. Refused with `Busy` while the track holds or records anything. Untimed.
    SetLoopKind { track: usize, midi: bool },
    /// Metronome on or off. The click grid keeps running either way.
    SetClick { on: bool },
    /// Volume of the click, linear, clamped to `0..=MAX_LEVEL_GAIN`. Untimed: a level, not an event.
    SetClickGain { gain: f32 },
    /// Volume of every monitored live input, linear, clamped like the click. Untimed, and it
    /// cannot reach a recording - what is recorded is the input before this gain.
    SetMonitorGain { gain: f32 },
    /// Silence every monitored live input on the way out, whatever the switches and the volume
    /// say - the host's guard against feedback while the output is a pair of built-in speakers.
    /// Untimed. The monitor volume and the switches are left as they are, so lifting the guard
    /// brings back exactly what was set; instruments are not live inputs and stay audible.
    SetMonitorMute { on: bool },
    /// New tempo, time signature and layer length. Only accepted while every track is empty and
    /// idle, because it redefines what every sample position means.
    SetTempo {
        bpm: f64,
        signature: TimeSignature,
        /// Length in **frames** the control thread now allocates layer buffers at. A mono buffer
        /// is that many samples long, a stereo one twice as many.
        layer_frames: u64,
    },
    /// Whole effect chain of one track in or out of the signal path. Out means bit-identical
    /// pass-through - the panic switch.
    SetFxBypass { track: usize, on: bool },
    /// One effect of one track's chain on or off.
    SetFxEnabled {
        track: usize,
        slot: FxSlot,
        on: bool,
    },
    /// One knob of one track's chain.
    SetFxParam { track: usize, param: FxParam },
    /// A hand on an effect's switch: like `SetFxEnabled`, but switching on also takes the chain
    /// out of bypass and gives a delay or reverb at mix 0 a start mix. What the window, a MIDI
    /// controller and the terminal send; a score's `mixer:` section uses the exact commands.
    TouchFxEnabled {
        track: usize,
        slot: FxSlot,
        on: bool,
    },
    /// A hand on a knob: like `SetFxParam`, and the knob's effect is switched on as by
    /// `TouchFxEnabled`.
    TouchFxParam { track: usize, param: FxParam },
    /// Load a ready-made chain. The one command the musician uses on stage.
    LoadFxPreset { track: usize, preset: FxPreset },
    /// Start the transport: from `at` on the clock runs, the click sounds and every track that is
    /// on play is heard again. `at` becomes bar 1 (`Timeline::zero`), and every loop is re-anchored
    /// there, so each one plays from its top again. A take queued for the same `at` behind it
    /// starts on that very downbeat - the order of arrival decides among equal timestamps.
    TransportStart { at: u64 },
    /// A count-in before free looping starts: from `at` the clock runs and the click sounds -
    /// whether the click is switched on or not, a count-in nobody hears is no count-in - while
    /// every loop stays silent. A `TransportStart { at: end }` sent behind it ends it: bar 1 is
    /// `end`, and that is where the take begins. `TransportStop` cancels it like any start.
    CountIn { at: u64, end: u64 },
    /// The score's window: open from `at` on, closed from `at` on. While it is closed the click is
    /// silent and a hosted plugin is told that nothing plays (position 0), whatever the clock does
    /// underneath - a loaded score keeps the engine's clock running so the runner can plan on it,
    /// but nothing may be heard of that clock before the start or after the end. Open, the click
    /// sounds as its switch says and a plugin counts from `bar1` (the first bar of the first
    /// section; negative during the count-in). Open by default, so everything that never sends it
    /// - free looping, the CLI, the tests - runs exactly as before. Timed, so the count-in's first
    /// click and the end of the score are exact to the sample.
    ClickGate { at: u64, open: bool, bar1: u64 },
    /// Stop the transport: the clock stands at bar 1, the click and the loops are silent, the
    /// monitored inputs stay audible. Layers and play states are kept. A take that has not reached
    /// its end is dealt with so nothing half-made is left behind - see
    /// `EngineCore::stop_transport`. Untimed: a stop button has to act now. Like `ClearAll`, it
    /// drops every timed command that was sent before it and is still waiting.
    TransportStop,
    /// Stop everything (playback and recording) and mark the engine as finished.
    Stop,
}

impl Command {
    /// The position this command is scheduled for, if it has one.
    pub fn at(&self) -> Option<u64> {
        match self {
            Command::StartRecord { at, .. }
            | Command::StartOverdub { at, .. }
            | Command::StopRecord { at, .. }
            | Command::StartPlay { at, .. }
            | Command::StopPlay { at, .. }
            | Command::RestartLoop { at, .. }
            | Command::ClearTrack { at, .. }
            | Command::ClearAll { at }
            | Command::DropWaiting { at }
            | Command::LatchOverdub { at, .. }
            | Command::TransportStart { at }
            | Command::CountIn { at, .. }
            | Command::ClickGate { at, .. } => Some(*at),
            Command::SetMonitor { .. }
            | Command::SetPan { .. }
            | Command::SetTrackGain { .. }
            | Command::SetTrackMute { .. }
            | Command::SetTrackSolo { .. }
            | Command::SetTrackSend { .. }
            | Command::SetBusGain { .. }
            | Command::SetBusOutput { .. }
            | Command::SetTrackLatency { .. }
            | Command::SetLayerMute { .. }
            | Command::SetLayerGain { .. }
            | Command::RemoveLayer { .. }
            | Command::UndoLayer { .. }
            | Command::SetLoopKind { .. }
            | Command::SetClick { .. }
            | Command::SetClickGain { .. }
            | Command::SetMonitorGain { .. }
            | Command::SetMonitorMute { .. }
            | Command::SetTempo { .. }
            | Command::SetFxBypass { .. }
            | Command::SetFxEnabled { .. }
            | Command::SetFxParam { .. }
            | Command::TouchFxEnabled { .. }
            | Command::TouchFxParam { .. }
            | Command::LoadFxPreset { .. }
            | Command::TransportStop
            | Command::Stop => None,
        }
    }

    /// The track this command addresses, if it addresses one.
    pub fn track(&self) -> Option<usize> {
        match self {
            Command::StartRecord { track, .. }
            | Command::StartOverdub { track, .. }
            | Command::StopRecord { track, .. }
            | Command::StartPlay { track, .. }
            | Command::StopPlay { track, .. }
            | Command::RestartLoop { track, .. }
            | Command::ClearTrack { track, .. }
            | Command::LatchOverdub { track, .. }
            | Command::SetMonitor { track, .. }
            | Command::SetPan { track, .. }
            | Command::SetTrackGain { track, .. }
            | Command::SetTrackMute { track, .. }
            | Command::SetTrackSolo { track, .. }
            | Command::SetTrackSend { track, .. }
            | Command::SetTrackLatency { track, .. }
            | Command::SetLayerMute { track, .. }
            | Command::SetLayerGain { track, .. }
            | Command::RemoveLayer { track, .. }
            | Command::UndoLayer { track }
            | Command::SetLoopKind { track, .. }
            | Command::SetFxBypass { track, .. }
            | Command::SetFxEnabled { track, .. }
            | Command::SetFxParam { track, .. }
            | Command::TouchFxEnabled { track, .. }
            | Command::TouchFxParam { track, .. }
            | Command::LoadFxPreset { track, .. } => Some(*track),
            Command::ClearAll { .. }
            | Command::DropWaiting { .. }
            | Command::SetClick { .. }
            | Command::SetClickGain { .. }
            | Command::SetMonitorGain { .. }
            | Command::SetMonitorMute { .. }
            | Command::SetBusGain { .. }
            | Command::SetBusOutput { .. }
            | Command::SetTempo { .. }
            | Command::TransportStart { .. }
            | Command::CountIn { .. }
            | Command::ClickGate { .. }
            | Command::TransportStop
            | Command::Stop => None,
        }
    }
}

/// Why the engine refused the last command it refused. Travels in the status snapshot so the
/// control thread can print a German sentence instead of just a counter.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Refusal {
    #[default]
    None,
    /// Tempo change while something is recorded, recording or playing.
    Tempo,
    /// The layer ceiling of a track was reached.
    LayerLimit,
    /// No prepared buffer was available for a new layer.
    NoBuffer,
    /// The command named a track or layer that does not exist.
    NoSuchTarget,
    /// A layer operation while that track is recording.
    Busy,
    /// A continuous overdub reached the layer ceiling and stopped on its own.
    OverdubFull,
}

impl Refusal {
    /// Explanation for any front end, or `None` when nothing was refused. Free of key hints - the
    /// app has no `a` key; the terminal adds its own with [`Refusal::cli_hint`].
    pub fn message(self) -> Option<&'static str> {
        match self {
            Refusal::None => None,
            Refusal::Tempo => Some(
                "Tempo change refused: something is recorded or playing. Clear all tracks first.",
            ),
            Refusal::LayerLimit => {
                Some("This track has reached its layer limit. Remove a layer first.")
            }
            Refusal::NoBuffer => Some(
                "No prepared buffer free - the control thread cannot keep up refilling them.",
            ),
            Refusal::NoSuchTarget => Some("The command referred to a track or layer that does not exist."),
            Refusal::Busy => Some("Layers cannot be edited while this track is recording."),
            Refusal::OverdubFull => Some(
                "Overdub ended: this track holds 16 layers, the most it can. Undo or remove a layer \
                 to add more.",
            ),
        }
    }

    /// The `live` terminal's key for the way out, appended there to [`Refusal::message`].
    pub fn cli_hint(self) -> Option<&'static str> {
        match self {
            Refusal::Tempo => Some("(a)"),
            Refusal::LayerLimit => Some("(w <nr>)"),
            _ => None,
        }
    }
}

/// Per-track part of the status snapshot.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TrackStatus {
    pub state: TrackState,
    pub layers: u8,
    /// Bit `i` set means layer `i` is muted.
    pub muted_mask: u32,
    /// Loop length in **frames**, 0 while nothing is recorded. A frame is one instant of audio -
    /// one sample on a mono track, two on a stereo one.
    pub loop_len: u64,
    /// Musical position loop index 0 sits at, 0 while nothing is recorded. Together with `loop_len`
    /// this is the track's own grid, which is what a further layer has to be quantised to.
    pub origin: u64,
    /// Frames already written during a running loop-defining take.
    pub filled: u64,
    /// Peak of each recorded input channel, absolute value. On a mono track only index 0 moves.
    pub input_peak: [f32; 2],
    /// Peak this track contributed to the bus, left and right, absolute values. Its audible layers
    /// only, panned but before the effect chain - see `Track::output_peak` for why exactly there.
    pub output_peak: [f32; 2],
    pub monitor: bool,
    pub playing: bool,
    /// Zero-based device input channels this track records. Only the first `channels` of them mean
    /// anything; on a mono track the second entry repeats the first.
    pub input_channels: [u8; 2],
    /// 1 for a mono loop buffer, 2 for a stereo one.
    pub channels: u8,
    /// Position in the stereo field: -1.0 hard left, 0.0 centre, +1.0 hard right.
    pub pan: f32,
    /// Which output buses this track's loop playback feeds.
    pub loop_send: BusSend,
    /// Which output buses this track's monitored live input feeds. Independent of `loop_send`.
    pub monitor_send: BusSend,
    /// How this track's latency compensation is configured: the measured part (or "follow the
    /// engine's default") and the manual surcharge.
    pub latency: TrackLatency,
    /// The number that configuration works out to, in **frames** - the `R` of `m = k - R` for this
    /// track. Sent alongside `latency` so no reader has to know the engine's default to print it.
    pub latency_frames: u32,
    /// State of this track's effect chain: what is on, which preset, and the parameters behind
    /// it. Fixed size and `Copy`, like everything else that crosses the thread boundary.
    pub fx: FxStatus,
    /// The channel fader, linear (1.0 is 0 dB).
    pub gain: f32,
    pub muted: bool,
    pub soloed: bool,
    /// Peak of what this strip sends, left and right: post-fader, post-panner, the louder of the
    /// two buses per side, monitored input included. What a mixer's channel meter shows - unlike
    /// `output_peak`, which measures the take.
    pub strip_peak: [f32; 2],
    /// Whether the track is played by an instrument rather than an input channel.
    pub instrument: bool,
    /// Whether an instrument sits in the track's source slot right now.
    pub instrument_loaded: bool,
    /// Note ons that reached the track since the engine started, wrapping - the activity light
    /// blinks when it moves.
    pub notes: u32,
    /// Notes the musician is holding on this track right now.
    pub held_notes: u8,
    /// Whether the overdub of this track keeps going into the next pass (`Command::LatchOverdub`).
    /// During a first recording it means "overdub next".
    pub latched: bool,
    /// Layer 0 is the muted stand-in of a `stack` whose first pass stayed empty: it only holds the
    /// loop's grid, and a display does not count it (see `super::latch`).
    pub placeholder: bool,
    /// Whether this track's takes store notes - a MIDI loop.
    pub midi_loop: bool,
    /// On a MIDI loop: note ons recorded in each layer.
    pub layer_notes: [u16; super::track::MAX_LAYERS],
}

impl Default for TrackStatus {
    /// The state of a track that does not exist, and the filler for the unused tail of the
    /// fixed-size array. Written out rather than derived because of `channels`: a derived zero
    /// would say "a buffer with no channels", which is not a state a track can be in.
    fn default() -> Self {
        Self {
            state: TrackState::default(),
            layers: 0,
            muted_mask: 0,
            loop_len: 0,
            origin: 0,
            filled: 0,
            input_peak: [0.0; 2],
            output_peak: [0.0; 2],
            monitor: false,
            playing: false,
            input_channels: [0; 2],
            channels: 1,
            pan: 0.0,
            loop_send: BusSend::BOTH,
            monitor_send: BusSend::MONITOR,
            latency: TrackLatency::INHERITED,
            latency_frames: 0,
            fx: FxStatus::default(),
            gain: 1.0,
            muted: false,
            soloed: false,
            strip_peak: [0.0; 2],
            instrument: false,
            instrument_loaded: false,
            notes: 0,
            held_notes: 0,
            latched: false,
            placeholder: false,
            midi_loop: false,
            layer_notes: [0; super::track::MAX_LAYERS],
        }
    }
}

impl TrackStatus {
    /// Loudest recorded input channel - what a single-bar meter shows.
    pub fn input_peak_max(&self) -> f32 {
        self.input_peak[0].max(self.input_peak[1])
    }

    /// Louder side of this track's contribution to the bus.
    pub fn output_peak_max(&self) -> f32 {
        self.output_peak[0].max(self.output_peak[1])
    }

    /// The input channels that actually mean something: one entry on a mono track, two on a
    /// stereo one.
    pub fn used_input_channels(&self) -> &[u8] {
        &self.input_channels[..(self.channels as usize).clamp(1, 2)]
    }
}

/// Snapshot the audio thread pushes back to the control thread.
///
/// A single `Copy` struct instead of many message kinds: pushing it costs one memcpy of a few
/// hundred bytes, and the control thread simply keeps the newest one it finds.
#[derive(Clone, Copy, Debug, Default)]
pub struct Status {
    /// Absolute sample position of the engine (the output timeline).
    pub pos: u64,
    /// Zero-based bar at `pos`.
    pub bar: u64,
    /// Zero-based beat inside that bar.
    pub beat: u32,
    /// Samples elapsed inside the current beat.
    pub beat_offset: u64,
    /// Length of one beat, so the control thread can render a progress bar without a timeline.
    pub samples_per_beat: f64,
    pub tracks: [TrackStatus; MAX_TRACKS],
    pub track_count: u8,
    /// Peak of each output bus, left and right, absolute values - measured after the bus volume,
    /// because a meter has to show what actually leaves the machine.
    pub bus_peak: [[f32; BUS_CHANNELS]; BUS_COUNT],
    /// Peak of each **device** output channel - the first [`OUT_METERS`] of them - after both buses
    /// were placed on it and before the safety limiter. This is the truth at the socket: what a
    /// bus meter cannot show is the sum on a channel two buses share, and an over above 0 dBFS
    /// here is what the limiter then had to bend.
    pub out_peak: [f32; OUT_METERS],
    /// Deepest gain reduction of the safety limiter per device channel since the last snapshot,
    /// in dB: 0 while it did nothing, negative while it worked.
    pub out_reduction_db: [f32; OUT_METERS],
    /// Volume of each output bus, linear.
    pub bus_gain: [f32; BUS_COUNT],
    /// Which device output channels each bus leaves on, already clamped to what the device has.
    pub bus_out: BusRouting,
    /// Channels the device's output really has, so a display can offer the pairs that exist and
    /// explain the two-output case without asking anybody else.
    pub output_channels: u8,
    pub click: bool,
    /// Volume of the click and of the monitored live inputs, linear.
    pub click_gain: f32,
    pub monitor_gain: f32,
    pub bpm: f64,
    /// Commands the engine refused, in total.
    pub ignored_commands: u64,
    /// Why the last refusal happened.
    pub refusal: Refusal,
    /// Prepared buffers the engine currently holds ready for the next layer, per kind - see
    /// `Channels::index`.
    pub spares: [u32; BUFFER_KINDS],
    /// Buffers the engine has taken out of the install queue since it started, per kind. Together
    /// with what the control thread pushed, this says exactly how many are still in flight.
    pub buffers_taken: [u64; BUFFER_KINDS],
    /// Set once a `Stop` command has been executed.
    pub stopped: bool,
    /// Whether the transport runs. While it stands, `bar`, `beat` and `beat_offset` are all zero
    /// and nothing but the monitored inputs is heard; `pos` keeps counting, because it is the
    /// sample axis every command is stamped on, not the musical clock.
    pub transport: bool,
    /// Whether the click window is open (`Command::ClickGate`): true unless a loaded score holds it
    /// closed because it is not playing.
    pub click_gate: bool,
    /// `C`, the fixed delay between a key and its instrument's sound, in frames; 0 until known
    /// (`super::source`).
    pub instrument_delay: u32,
    /// MIDI events that arrived too late for their fixed position and sounded late, in total.
    pub late_events: u32,
}

impl Status {
    /// The tracks that actually exist, without the unused tail of the fixed-size array.
    pub fn tracks(&self) -> &[TrackStatus] {
        &self.tracks[..(self.track_count as usize).min(MAX_TRACKS)]
    }

    /// Both channels of one bus.
    pub fn bus_peak(&self, bus: Bus) -> [f32; BUS_CHANNELS] {
        self.bus_peak[bus.index()]
    }

    /// Louder side of one bus.
    pub fn bus_peak_max(&self, bus: Bus) -> f32 {
        let peak = self.bus_peak(bus);
        peak[0].max(peak[1])
    }

    /// The main bus, i.e. what goes to the room. What "the output level" means when only one
    /// number is wanted; the monitor bus carries the click and would make that number useless.
    pub fn output_peak(&self) -> [f32; BUS_CHANNELS] {
        self.bus_peak(Bus::Main)
    }

    /// Louder side of the main bus.
    pub fn output_peak_max(&self) -> f32 {
        self.bus_peak_max(Bus::Main)
    }

    /// Volume of one bus, linear.
    pub fn bus_gain(&self, bus: Bus) -> f32 {
        self.bus_gain[bus.index()]
    }

    /// Prepared buffers of both kinds together, for a display that only wants one number.
    pub fn spares_total(&self) -> u32 {
        self.spares.iter().sum()
    }

    /// Take over the meters of an `earlier` snapshot that will not be shown on its own: every
    /// peak becomes the larger of both, every gain reduction the deeper one. A snapshot covers
    /// 5 ms, a display shows one in ten - without this the meters would show a random 5 ms slice
    /// of every 50 ms and miss most transients, which reads as nervous flicker.
    pub fn hold_meters(&mut self, earlier: &Status) {
        let max2 = |a: &mut [f32; 2], b: [f32; 2]| {
            a[0] = a[0].max(b[0]);
            a[1] = a[1].max(b[1]);
        };
        for (t, e) in self.tracks.iter_mut().zip(earlier.tracks.iter()) {
            max2(&mut t.input_peak, e.input_peak);
            max2(&mut t.output_peak, e.output_peak);
            max2(&mut t.strip_peak, e.strip_peak);
            t.fx.reduction_db = t.fx.reduction_db.min(e.fx.reduction_db);
        }
        for (b, e) in self.bus_peak.iter_mut().zip(earlier.bus_peak.iter()) {
            for (c, v) in b.iter_mut().zip(e.iter()) {
                *c = c.max(*v);
            }
        }
        for (c, e) in self.out_peak.iter_mut().zip(earlier.out_peak) {
            *c = c.max(e);
        }
        for (c, e) in self.out_reduction_db.iter_mut().zip(earlier.out_reduction_db) {
            *c = c.min(e);
        }
    }

    /// The device output channels that are metered: as many as the device has, at most
    /// [`OUT_METERS`].
    pub fn out_meter_count(&self) -> usize {
        (self.output_channels as usize).min(OUT_METERS)
    }
}

/// Control thread -> audio thread.
pub struct CommandSender {
    tx: Producer<Command>,
}

impl CommandSender {
    /// Fails only if the queue is full, which means the audio thread stopped consuming.
    pub fn send(&mut self, cmd: Command) -> Result<(), String> {
        self.tx
            .push(cmd)
            .map_err(|_| "Command queue is full - the audio thread is not picking anything up.".to_string())
    }
}

/// Audio thread side of the command queue.
pub struct CommandReceiver {
    rx: Consumer<Command>,
}

impl CommandReceiver {
    #[inline]
    pub fn pop(&mut self) -> Option<Command> {
        self.rx.pop().ok()
    }
}

pub fn command_channel(capacity: usize) -> (CommandSender, CommandReceiver) {
    let (tx, rx) = RingBuffer::<Command>::new(capacity);
    (CommandSender { tx }, CommandReceiver { rx })
}

/// Audio thread -> control thread.
pub struct StatusSender {
    tx: Producer<Status>,
}

impl StatusSender {
    /// Dropping the update when the queue is full is the right behaviour here: status is a
    /// snapshot, and the audio thread must never wait for the display.
    #[inline]
    pub fn push(&mut self, status: Status) {
        let _ = self.tx.push(status);
    }
}

pub struct StatusReceiver {
    rx: Consumer<Status>,
}

impl StatusReceiver {
    /// Newest status in the queue, discarding everything older - except the meters, which hold
    /// the peaks of the discarded snapshots too (`Status::hold_meters`).
    pub fn latest(&mut self) -> Option<Status> {
        let mut last: Option<Status> = None;
        while let Ok(mut s) = self.rx.pop() {
            if let Some(prev) = &last {
                s.hold_meters(prev);
            }
            last = Some(s);
        }
        last
    }
}

pub fn status_channel(capacity: usize) -> (StatusSender, StatusReceiver) {
    let (tx, rx) = RingBuffer::<Status>::new(capacity);
    (StatusSender { tx }, StatusReceiver { rx })
}

/// Hand-over of layer buffers between the threads.
///
/// `install_tx` carries freshly allocated, zeroed buffers to the audio thread, `retire_rx` carries
/// used ones back so they are dropped or recycled where that is allowed.
pub struct BufferChannel {
    pub install_tx: Producer<Vec<f32>>,
    pub retire_rx: Consumer<Vec<f32>>,
}

pub struct BufferEndpoint {
    pub install_rx: Consumer<Vec<f32>>,
    pub retire_tx: Producer<Vec<f32>>,
}

impl BufferChannel {
    /// Control-thread side: hand one buffer over.
    pub fn install(&mut self, buffer: Vec<f32>) -> Result<(), String> {
        self.install_tx
            .push(buffer)
            .map_err(|_| "Buffer queue is full.".to_string())
    }

    pub fn drain_retired(&mut self) {
        // Dropping happens here, in the control thread, never in the callback.
        while self.retire_rx.pop().is_ok() {}
    }
}

impl BufferEndpoint {
    /// Audio-thread side: take a prepared buffer if one is waiting.
    #[inline]
    pub fn take_new(&mut self) -> Option<Vec<f32>> {
        self.install_rx.pop().ok()
    }

    /// Send a used buffer back to the control thread.
    ///
    /// If the return queue is unexpectedly full, the buffer is leaked rather than dropped: a
    /// deallocation inside the audio callback can block, a leaked buffer cannot. The return queue
    /// has room for every buffer that can ever be in the engine at once, so this cannot happen
    /// unless the control thread has stopped draining.
    #[inline]
    pub fn retire(&mut self, buffer: Vec<f32>) {
        if let Err(PushError::Full(buffer)) = self.retire_tx.push(buffer) {
            std::mem::forget(buffer);
        }
    }
}

pub fn buffer_channel(install_capacity: usize, retire_capacity: usize) -> (BufferChannel, BufferEndpoint) {
    let (install_tx, install_rx) = RingBuffer::<Vec<f32>>::new(install_capacity);
    let (retire_tx, retire_rx) = RingBuffer::<Vec<f32>>::new(retire_capacity);
    (
        BufferChannel {
            install_tx,
            retire_rx,
        },
        BufferEndpoint {
            install_rx,
            retire_tx,
        },
    )
}

/// The control thread's stock of empty layer buffers, one stock per channel count.
///
/// Its whole job is that an overdub never has to wait for `malloc`: the engine is kept supplied
/// with `slots[kind]` prepared buffers of each kind, and everything that comes back is zeroed here
/// and reused. All allocating, zeroing and dropping happens in [`LayerPool::service`], which only
/// ever runs in the control thread.
///
/// A session with only mono tracks asks for `slots[1] == 0` and never allocates a stereo buffer,
/// which is the whole point of keeping the two kinds apart rather than making every buffer stereo.
pub struct LayerPool {
    channel: BufferChannel,
    /// Frames one layer holds. A buffer of kind `k` is `layer_frames * (k + 1)` samples long.
    layer_frames: usize,
    slots: [usize; BUFFER_KINDS],
    reserve: [Vec<Vec<f32>>; BUFFER_KINDS],
    /// Buffers pushed into the install queue since the start, per kind.
    pushed: [u64; BUFFER_KINDS],
    /// Buffers taken back out of the retire queue since the start, per kind.
    reclaimed: [u64; BUFFER_KINDS],
    /// Buffers that came back at a length belonging to no current kind - left over from a tempo
    /// change. Dropped rather than kept; counted so the accounting stays honest.
    discarded: u64,
}

impl LayerPool {
    pub fn new(channel: BufferChannel, layer_frames: usize, slots: [usize; BUFFER_KINDS]) -> Self {
        Self {
            channel,
            layer_frames,
            slots,
            reserve: [
                Vec::with_capacity(slots[0]),
                Vec::with_capacity(slots[1]),
            ],
            pushed: [0; BUFFER_KINDS],
            reclaimed: [0; BUFFER_KINDS],
            discarded: 0,
        }
    }

    /// One mono stock only - the shorthand for a mono-only session and for tests.
    pub fn mono(channel: BufferChannel, layer_frames: usize, slots: usize) -> Self {
        Self::new(channel, layer_frames, [slots, 0])
    }

    /// Length in samples of a buffer of kind `kind`.
    fn buffer_len(&self, kind: usize) -> usize {
        self.layer_frames * (kind + 1)
    }

    /// Which kind a returned buffer belongs to, or `None` if it fits neither - which happens after
    /// a tempo change, when the engine sends its old stock back.
    fn kind_of(&self, buffer: &[f32]) -> Option<usize> {
        (0..BUFFER_KINDS).find(|&k| self.slots[k] > 0 && buffer.len() == self.buffer_len(k))
    }

    /// Reclaim what came back, then top the engine up to `slots[kind]` prepared buffers of each
    /// kind.
    ///
    /// `status` is the newest snapshot, or `None` before the first one arrives. `spares` and
    /// `buffers_taken` in it are what makes the accounting exact: buffers of kind `k` still sitting
    /// in the install queue are `pushed[k] - buffers_taken[k]`, and the engine holds `spares[k]`
    /// more. Layers in use are deliberately *not* counted, so using a layer immediately triggers a
    /// refill.
    pub fn service(&mut self, status: Option<&Status>) {
        while let Ok(mut buffer) = self.channel.retire_rx.pop() {
            match self.kind_of(&buffer) {
                Some(kind) => {
                    self.reclaimed[kind] += 1;
                    if self.reserve[kind].len() < self.slots[kind] {
                        // Zeroing belongs here: a layer buffer must arrive empty, and a memset of
                        // several megabytes has no business in an audio callback.
                        buffer.fill(0.0);
                        self.reserve[kind].push(buffer);
                    }
                    // Everything else is simply dropped - in the control thread, where that is
                    // allowed.
                }
                None => self.discarded += 1,
            }
        }

        for kind in 0..BUFFER_KINDS {
            let taken = status.map(|s| s.buffers_taken[kind]).unwrap_or(0);
            let queued = self.pushed[kind].saturating_sub(taken) as usize;
            let mut available = status.map(|s| s.spares[kind] as usize).unwrap_or(0) + queued;
            let len = self.buffer_len(kind);
            while available < self.slots[kind] {
                let buffer = self.reserve[kind].pop().unwrap_or_else(|| vec![0.0f32; len]);
                match self.channel.install_tx.push(buffer) {
                    Ok(()) => {
                        self.pushed[kind] += 1;
                        available += 1;
                    }
                    Err(PushError::Full(buffer)) => {
                        self.reserve[kind].push(buffer);
                        break;
                    }
                }
            }
        }
    }

    /// New layer length after a tempo change: the stock is worthless at the wrong length.
    ///
    /// The in-flight accounting starts over here, and `EngineCore` resets its half of it in the
    /// same breath (see the `SetTempo` arm of `process::apply`). Anything still in the install
    /// queue is of the old length and will be handed straight back; counting it against the new
    /// length would be arithmetic about buffers that no longer exist.
    pub fn set_layer_frames(&mut self, layer_frames: usize) {
        // Only a changed length invalidates anything, and `EngineCore` makes exactly the same
        // comparison before it throws its stocks away - the two counters have to be reset in the
        // same breath or the accounting drifts apart.
        if layer_frames == self.layer_frames {
            return;
        }
        self.layer_frames = layer_frames;
        for reserve in self.reserve.iter_mut() {
            reserve.clear();
        }
        self.pushed = [0; BUFFER_KINDS];
    }

    /// Buffers that came back from the audio thread and were recognised. Used by the tests to
    /// prove nothing leaks.
    #[cfg(test)]
    pub fn reclaimed(&self) -> u64 {
        self.reclaimed.iter().sum()
    }

    #[cfg(test)]
    pub fn reclaimed_of(&self, channels: Channels) -> u64 {
        self.reclaimed[channels.index()]
    }

    #[cfg(test)]
    pub fn pushed(&self) -> u64 {
        self.pushed.iter().sum()
    }

    #[cfg(test)]
    pub fn pushed_of(&self, channels: Channels) -> u64 {
        self.pushed[channels.index()]
    }

    /// Give everything back at the end of a session.
    pub fn drain(&mut self) {
        self.channel.drain_retired();
        for reserve in self.reserve.iter_mut() {
            reserve.clear();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn timestamps_are_reported_per_variant() {
        assert_eq!(Command::StartRecord { track: 0, at: 42 }.at(), Some(42));
        assert_eq!(Command::StopPlay { track: 1, at: 7 }.at(), Some(7));
        assert_eq!(Command::ClearAll { at: 9 }.at(), Some(9));
        assert_eq!(
            Command::SetMonitor {
                track: 0,
                on: true
            }
            .at(),
            None
        );
        assert_eq!(Command::SetPan { track: 2, pan: -0.5 }.at(), None);
        assert_eq!(Command::SetPan { track: 2, pan: -0.5 }.track(), Some(2));
        // A latency is a property of the wiring, not a musical event: it addresses a track and
        // carries no timestamp.
        let latency = Command::SetTrackLatency {
            track: 1,
            latency: TrackLatency::measured(512).with_trim(64),
        };
        assert_eq!(latency.at(), None);
        assert_eq!(latency.track(), Some(1));
        assert_eq!(Command::Stop.at(), None);
        assert_eq!(Command::StartOverdub { track: 3, at: 1 }.track(), Some(3));
        assert_eq!(Command::ClearAll { at: 1 }.track(), None);
    }

    #[test]
    fn commands_arrive_in_the_order_they_were_sent() {
        let (mut tx, mut rx) = command_channel(8);
        tx.send(Command::StartRecord { track: 0, at: 10 }).unwrap();
        tx.send(Command::StopRecord { track: 0, at: 20 }).unwrap();
        assert_eq!(rx.pop(), Some(Command::StartRecord { track: 0, at: 10 }));
        assert_eq!(rx.pop(), Some(Command::StopRecord { track: 0, at: 20 }));
        assert_eq!(rx.pop(), None);
    }

    /// The snapshot is memcpy'd into the queue **inside the audio callback**, so its size is a
    /// real-time concern and not just a memory one. Phase 6 put the effect state into it; the
    /// stereo rebuild added a second peak per meter, the channel count, the second input channel
    /// and the pan; the per-track latency added another sixteen bytes, bringing it to 1624.
    ///
    /// The two output buses add, in this order of size: the routing (two channel/width pairs, 32
    /// bytes), a peak per bus per channel where there used to be one stereo pair (+8), a volume per
    /// bus (+8), the device's output channel count, and **two bytes per track** for the loop and
    /// monitor sends - which is exactly why a send is a bitmask and not four bools.
    ///
    /// 1.7 kB at 200 snapshots per second is a copy of well under a microsecond per callback,
    /// against a budget of 2667 - and the queue of 1024 slots costs 1.7 MB, allocated once in the
    /// control thread. The limit is pinned so the next addition has to be a decision rather than
    /// an accident.
    ///
    /// The channel strip was such a decision: fader, mute, solo and a stereo strip meter are 16
    /// bytes per track, 128 for eight - 1816 in all. The limit moved from 1792 to 1920; a mixer
    /// whose meter does not move with its fader is worth a hundred bytes per snapshot.
    ///
    /// MIDI loops were the next: the note count of every layer (sixteen `u16` per track) is what
    /// lets a MIDI layer show "42 notes" instead of a waveform - 2144 bytes, limit 2176.
    ///
    /// Then `stack`: one flag per track saying that layer 0 is only the placeholder of an empty
    /// first pass, so the display does not count a layer that is not there. It costs a whole
    /// alignment step per track - 2208 bytes, limit 2208.
    ///
    /// Then the output meters: peak and limiter reduction of eight device channels, because a bus
    /// meter cannot show the sum on a socket two buses share - 64 bytes, 2272, limit 2272.
    ///
    /// Then the octaver: its four knobs in every track's chain settings, 16 bytes per track -
    /// 2400 bytes, limit 2400. Its latency is *not* carried; `FxStatus::latency_frames` computes
    /// it from the switches.
    #[test]
    fn the_status_snapshot_stays_small_enough_to_memcpy_in_a_callback() {
        let size = std::mem::size_of::<Status>();
        assert!(
            size <= 2_400,
            "Der Status ist auf {size} Bytes gewachsen - das wandert bei jedem Snapshot durch \
             den Audio-Callback."
        );
        assert!(std::mem::size_of::<Command>() <= 32, "Kommandos bleiben klein");
    }

    /// The bus commands are output-stage changes and carry no musical timestamp: a routing switch
    /// that waited for the next bar would be a routing switch nobody trusts.
    #[test]
    fn the_bus_commands_are_untimed_and_only_the_track_send_names_a_track() {
        let send = Command::SetTrackSend {
            track: 2,
            source: TrackSource::Monitor,
            send: BusSend::MONITOR,
        };
        assert_eq!(send.at(), None);
        assert_eq!(send.track(), Some(2));

        let gain = Command::SetBusGain {
            bus: Bus::Monitor,
            gain: 0.5,
        };
        assert_eq!(gain.at(), None);
        assert_eq!(gain.track(), None, "eine Bus-Lautstaerke gehoert keinem Track");

        let out = Command::SetBusOutput {
            bus: Bus::Monitor,
            out: BusOutput::pair(2),
        };
        assert_eq!(out.at(), None);
        assert_eq!(out.track(), None);
    }

    #[test]
    fn status_receiver_keeps_only_the_newest() {
        let (mut tx, mut rx) = status_channel(8);
        for pos in 0..5u64 {
            tx.push(Status {
                pos,
                ..Default::default()
            });
        }
        assert_eq!(rx.latest().map(|s| s.pos), Some(4));
        assert!(rx.latest().is_none());
    }

    /// The newest snapshot wins - but its meters hold the peaks of every snapshot it replaced, so
    /// a transient between two display events is not lost.
    #[test]
    fn status_receiver_holds_the_peaks_of_discarded_snapshots() {
        let (mut tx, mut rx) = status_channel(8);
        let mut loud = Status::default();
        loud.tracks[0].input_peak = [0.9, 0.1];
        loud.tracks[0].output_peak = [0.2, 0.8];
        loud.tracks[0].strip_peak = [0.7, 0.7];
        loud.tracks[0].fx.reduction_db = -9.0;
        loud.bus_peak[0] = [0.6, 0.5];
        tx.push(loud);
        let mut quiet = Status {
            pos: 7,
            ..Default::default()
        };
        quiet.tracks[0].input_peak = [0.05, 0.3];
        quiet.tracks[0].fx.reduction_db = -1.0;
        tx.push(quiet);
        let s = rx.latest().expect("Status");
        assert_eq!(s.pos, 7);
        assert_eq!(s.tracks[0].input_peak, [0.9, 0.3]);
        assert_eq!(s.tracks[0].output_peak, [0.2, 0.8]);
        assert_eq!(s.tracks[0].strip_peak, [0.7, 0.7]);
        assert_eq!(s.tracks[0].fx.reduction_db, -9.0);
        assert_eq!(s.bus_peak[0], [0.6, 0.5]);
    }

    /// A track that does not exist reports one channel, not zero - a buffer with no channels is
    /// not a state anything downstream could render.
    #[test]
    fn an_empty_track_status_is_a_mono_track_in_the_middle() {
        let ts = TrackStatus::default();
        assert_eq!(ts.channels, 1);
        assert_eq!(ts.pan, 0.0);
        // A track is heard in the room and in the headphones; what it monitors is a cue and stays
        // in the headphones unless someone says otherwise.
        assert_eq!(ts.loop_send, BusSend::BOTH);
        assert_eq!(ts.monitor_send, BusSend::MONITOR);
        assert_eq!(ts.used_input_channels(), &[0]);
        assert_eq!(ts.input_peak_max(), 0.0);
        assert_eq!(ts.output_peak_max(), 0.0);

        let stereo = TrackStatus {
            channels: 2,
            input_channels: [2, 3],
            input_peak: [0.1, 0.4],
            output_peak: [0.8, 0.2],
            ..Default::default()
        };
        assert_eq!(stereo.used_input_channels(), &[2, 3]);
        assert_eq!(stereo.input_peak_max(), 0.4);
        assert_eq!(stereo.output_peak_max(), 0.8);
    }

    #[test]
    fn buffers_travel_both_ways() {
        let (mut control, mut audio) = buffer_channel(2, 2);
        control.install(vec![0.0; 16]).unwrap();
        let taken = audio.take_new().expect("Puffer kommt an");
        assert_eq!(taken.len(), 16);
        audio.retire(taken);
        control.drain_retired();
        assert!(audio.take_new().is_none());
    }

    /// The pool keeps exactly `slots` buffers in flight, recycles what comes back, and zeroes it
    /// on the way - the three properties an overdub depends on.
    #[test]
    fn the_pool_keeps_the_engine_supplied_and_recycles_what_comes_back() {
        let (control, mut audio) = buffer_channel(8, 8);
        let mut pool = LayerPool::mono(control, 32, 3);
        pool.service(None);
        assert_eq!(pool.pushed(), 3, "Vorrat wird sofort angelegt");

        // The engine takes all three and turns them into layers.
        let mut taken = Vec::new();
        while let Some(b) = audio.take_new() {
            taken.push(b);
        }
        assert_eq!(taken.len(), 3);
        let status = Status {
            spares: [0, 0],
            buffers_taken: [3, 0],
            ..Default::default()
        };

        // Two of them come back dirty, as a removed layer does.
        for _ in 0..2 {
            let mut used = taken.pop().expect("Puffer");
            used.fill(0.7);
            audio.retire(used);
        }
        pool.service(Some(&status));
        assert_eq!(pool.reclaimed(), 2);
        assert_eq!(pool.pushed(), 6, "der Vorrat wird wieder auf drei gebracht");

        // The reserve is used before anything new is allocated, so these two are the recycled ones.
        let a = audio.take_new().expect("Puffer");
        let b = audio.take_new().expect("Puffer");
        assert!(
            a.iter().chain(b.iter()).all(|&s| s == 0.0),
            "wiederverwendete Puffer muessen leer beim Audio-Thread ankommen"
        );
    }

    /// The stereo half of the pool: two stocks of different lengths, each topped up on its own,
    /// each recognising its own buffers on the way back. A mono buffer handed to a stereo track
    /// would be half a loop long, so getting this wrong is not subtle.
    #[test]
    fn the_pool_keeps_mono_and_stereo_buffers_apart() {
        let (control, mut audio) = buffer_channel(16, 16);
        // 64 frames: mono buffers are 64 samples, stereo ones 128.
        let mut pool = LayerPool::new(control, 64, [2, 3]);
        pool.service(None);
        assert_eq!(pool.pushed_of(Channels::Mono), 2);
        assert_eq!(pool.pushed_of(Channels::Stereo), 3);

        let mut mono = Vec::new();
        let mut stereo = Vec::new();
        while let Some(b) = audio.take_new() {
            match b.len() {
                64 => mono.push(b),
                128 => stereo.push(b),
                other => panic!("Puffer mit unerwarteter Laenge {other}"),
            }
        }
        assert_eq!(mono.len(), 2, "zwei Mono-Puffer");
        assert_eq!(stereo.len(), 3, "drei Stereo-Puffer");

        // One of each comes back used; both stocks have to be refilled, each with its own length.
        let mut used = stereo.pop().expect("Puffer");
        used.fill(0.9);
        audio.retire(used);
        let mut used = mono.pop().expect("Puffer");
        used.fill(0.9);
        audio.retire(used);
        pool.service(Some(&Status {
            spares: [0, 0],
            buffers_taken: [2, 3],
            ..Default::default()
        }));
        assert_eq!(pool.reclaimed_of(Channels::Mono), 1);
        assert_eq!(pool.reclaimed_of(Channels::Stereo), 1);
        assert_eq!(pool.pushed_of(Channels::Mono), 4);
        assert_eq!(pool.pushed_of(Channels::Stereo), 6);

        let mut refilled_mono = 0usize;
        let mut refilled_stereo = 0usize;
        while let Some(b) = audio.take_new() {
            assert!(b.iter().all(|&s| s == 0.0), "recycelte Puffer kommen leer an");
            match b.len() {
                64 => refilled_mono += 1,
                128 => refilled_stereo += 1,
                other => panic!("Puffer mit unerwarteter Laenge {other}"),
            }
        }
        assert_eq!(refilled_mono, 2);
        assert_eq!(refilled_stereo, 3);
    }

    /// After a tempo change every buffer of the old length comes back. It belongs to no kind any
    /// more, so it is dropped rather than handed out again at the wrong length.
    #[test]
    fn buffers_of_a_stale_length_are_dropped_instead_of_reused() {
        let (control, mut audio) = buffer_channel(8, 8);
        let mut pool = LayerPool::new(control, 32, [1, 1]);
        pool.service(None);
        while audio.take_new().is_some() {}

        pool.set_layer_frames(50);
        // The engine hands its old stock back: 32 and 64 samples, neither of which fits now.
        audio.retire(vec![0.0; 32]);
        audio.retire(vec![0.0; 64]);
        pool.service(Some(&Status {
            spares: [0, 0],
            buffers_taken: [1, 1],
            ..Default::default()
        }));
        assert_eq!(pool.reclaimed(), 0, "nichts davon zaehlt als wiedergewonnen");
        let mut lengths: Vec<usize> = Vec::new();
        while let Some(b) = audio.take_new() {
            lengths.push(b.len());
        }
        lengths.sort_unstable();
        assert_eq!(lengths, vec![50, 100], "die neuen Laengen, nicht die alten");
    }
}
