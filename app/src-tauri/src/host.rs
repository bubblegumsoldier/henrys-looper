//! The engine host: one OS thread that owns the audio engine for the life of the process.
//!
//! ```text
//!  Tauri-Kommando (async)          Host-Thread "looper-engine"        Audio-Callbacks (Echtzeit)
//!  ┌────────────────────┐  Request ┌───────────────────────────┐  Kommandos ┌──────────────────┐
//!  │ spawn_blocking     │ ───────► │ Streams, LayerPool,       │ ─────────► │ EngineCore       │
//!  │ wartet auf Antwort │ ◄─────── │ Planung, Statusversand    │ ◄───────── │ Zeitachse, Mixer │
//!  └────────────────────┘  Result  └───────────────────────────┘  Status    └──────────────────┘
//!                                            │ app.emit("looper://status")
//!                                            ▼
//!                                        Web-View
//! ```
//!
//! Three rules from the spike, and this file exists to keep them:
//!
//! 1. **cpal's `Stream` is `!Send`.** It is built here, lives here and is dropped here. It never
//!    appears in Tauri's managed state - only [`EngineHandle`], which is nothing but a channel.
//! 2. **The Tauri main thread is never blocked.** It is MAINSTA and drives the window's message
//!    pump; every command hands its work to this thread and waits on a reply channel from a
//!    `spawn_blocking` worker.
//! 3. **Nothing is allocated, locked or logged in an audio callback.** This thread does all of
//!    that on behalf of the engine: it allocates and zeroes layer buffers, drops returned ones,
//!    and turns status snapshots into events.
//!
//! This thread is *not* the real-time thread. cpal's callbacks run on driver threads; what happens
//! here is the control side of the picture in `docs/architektur.md` section 5.

use std::collections::VecDeque;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::time::{Duration, Instant};

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{InputCallbackInfo, OutputCallbackInfo, SupportedBufferSize};
use tauri::{AppHandle, Emitter};

use looper_engine::audio::{self, DeviceOpts};
use looper_engine::capture::capture_channel;
use looper_engine::capture::run::{Mode as RecordMode, SectionWatch};
use looper_engine::engine::bus::{
    Bus, BusOutput, BusSend, MAX_BUS_GAIN, TrackSource, place_buses, routing_note,
};
use looper_engine::engine::calibrate::{CalibrateOpts, cmd_calibrate};
use looper_engine::engine::command::{
    Command, CommandSender, LayerPool, MAX_LEVEL_GAIN, MAX_TRACKS, Refusal, Status, StatusReceiver, TrackStatus,
    buffer_channel, command_channel, status_channel,
};
use looper_engine::engine::frame::{Channels, TrackInput};
use looper_engine::engine::source::{FeedMsg, FeedSender, HostClock, TrackRoute, source_channel};
use looper_engine::engine::fx::{FxParam, FxPreset, FxSlot};
use looper_engine::engine::live::MAX_LATENCY_FRAMES;
use looper_engine::engine::live::TrackDef;
use looper_engine::engine::track::MAX_TRACK_GAIN;
use looper_engine::engine::bus::BUS_SAMPLES;
use looper_engine::engine::process::{
    EngineConfig, EngineCore, loop_capacity, max_latency, max_memory_bytes, spare_channels,
    spare_slots_for, total_channels,
};
use looper_engine::engine::runner::{
    DEFAULT_COUNT_IN_BARS, Phase, Runner, TRANSPORT_BELONGS_TO_SCORE, check_tracks, setup_commands,
};
use looper_engine::engine::timeline::{TimeSignature, Timeline};
use looper_engine::engine::track::{MAX_LAYERS, Track, TrackLatency, TrackState};
use looper_engine::midi::{
    Context, FxKnob, MidiAction, ParamState, Resolution, SessionShape, Target, TrackLayout,
    TrackRef,
};
use looper_engine::score::mixer::{BusLevels, MixerSource, TrackMixSource, mixer_commands};
use looper_engine::score::{CompiledScore, OrderedMap, ScoreError, compile_score};

use crate::logfile::{self, log};
use crate::layout::{self, LayoutTrack};
use crate::mixstore;
use crate::midi::{BindingOptions, MidiBridge, MidiOutcome, MidiPortView, MidiSaved, MidiView, ProfileAction};
use crate::proto::{
    AppInfo, BusOutConfig, CalibrateConfig, NoticeEvent, CalibrateOutcome, CompileOutcome, ConfigInfo, DeviceInfo,
    DeviceReport, EngineInfo, HostInfo, InstrumentInput, OverdubMode, QuantizeName, ScoreLoaded,
    StartConfig, StatusEvent, TrackConfig, bus_events, dbfs, score_event, track_event,
};
use crate::schedule::{Quantize, Scheduled, Scheduler, build_timeline, resolve_tracks};

/// Event the status snapshot is pushed on.
pub const STATUS_EVENT: &str = "looper://status";
/// Event carrying [`AppInfo`], emitted once at startup.
pub const READY_EVENT: &str = "looper://ready";
/// Event carrying incoming MIDI and the state of the mapping. Its own event and not part of the
/// status snapshot - see the module comment of [`crate::midi`] for why.
pub const MIDI_EVENT: &str = "looper://midi";
/// Event carrying a fresh [`EngineInfo`] when the engine was rebuilt without `engine_start` - when
/// loading a score set it up with the score's tracks.
pub const ENGINE_EVENT: &str = "looper://engine";
/// Event carrying a [`NoticeEvent`]: something the program says on its own, e.g. the runner at a
/// section boundary.
pub const NOTICE_EVENT: &str = "looper://notice";

/// Input FIFO size in multiples of the audio buffer. Same value as the CLI: a few dozen kB, and
/// the difference between a hiccup and a permanently misaligned recording.
const INPUT_FIFO_BUFFERS: u32 = 64;
/// Status snapshots per second the audio thread produces.
const STATUS_HZ: u32 = 200;
/// Status events per second sent to the frontend. Twenty is smooth for a position display and a
/// level meter, and two orders of magnitude below what the web view could choke on.
const EMIT_INTERVAL: Duration = Duration::from_millis(50);
/// Prepared layer buffers kept inside the engine, so an overdub never waits for an allocation.
const SPARE_SLOTS: usize = 3;
/// How long the host thread sleeps between rounds while the engine runs. Short enough that the
/// buffer pool is serviced long before the engine could run out.
const RUNNING_TICK: Duration = Duration::from_millis(4);
/// The same while nothing runs - then there is nothing to service and only requests matter.
const IDLE_TICK: Duration = Duration::from_millis(200);
/// Most commands of a mix restore sent per round of the host thread. The command queue holds 256;
/// a whole mix of eight tracks with every knob off its preset is about as many, and the audio
/// thread empties the queue once per callback, so a few rounds are always enough.
const MIX_BATCH: usize = 48;
/// How long the free-mode mix waits after the last change before it is written to `mixer.json`.
const MIX_SAVE_DELAY: Duration = Duration::from_secs(1);
/// The one sentence for "there is nothing to talk to yet", so every command says it the same way.
const NO_ENGINE: &str = "The engine is not running. Start it first.";

// ---------------------------------------------------------------------------------------------
// Tap tempo
// ---------------------------------------------------------------------------------------------

/// Taps further apart than this start a new series instead of counting as a very slow beat.
const TAP_RESET: Duration = Duration::from_millis(2_500);
/// The last this many gaps count, so the estimate follows a player who speeds up.
const TAP_WINDOW: usize = 8;

/// Turns taps into a tempo: the median gap of the last few, so one stumble does not move it.
/// The same rule the setup page's tap button uses (`ui/src/live/TapTempo.tsx`).
#[derive(Default, Debug)]
struct TapTempo {
    taps: VecDeque<Instant>,
}

impl TapTempo {
    /// One tap. Returns a tempo from the third tap on (two gaps), rounded to 0.1 BPM.
    fn tap(&mut self, now: Instant) -> Option<f64> {
        if self.taps.back().is_some_and(|last| now.duration_since(*last) > TAP_RESET) {
            self.taps.clear();
        }
        self.taps.push_back(now);
        while self.taps.len() > TAP_WINDOW + 1 {
            self.taps.pop_front();
        }
        if self.taps.len() < 3 {
            return None;
        }
        let mut gaps: Vec<f64> = self
            .taps
            .iter()
            .zip(self.taps.iter().skip(1))
            .map(|(a, b)| b.duration_since(*a).as_secs_f64())
            .collect();
        gaps.sort_by(f64::total_cmp);
        let mid = gaps.len() / 2;
        let median = if gaps.len() % 2 == 1 {
            gaps[mid]
        } else {
            (gaps[mid - 1] + gaps[mid]) / 2.0
        };
        if median <= 0.0 {
            return None;
        }
        let bpm = (600.0 / median).round() / 10.0;
        (20.0..=300.0).contains(&bpm).then_some(bpm)
    }
}

// ---------------------------------------------------------------------------------------------
// What the frontend can ask for
// ---------------------------------------------------------------------------------------------

/// One user action on a running engine. Everything that needs a musical timestamp gets it here,
/// in [`Scheduler`], not in the caller.
#[derive(Debug, Clone, PartialEq)]
pub enum Action {
    Record { track: usize },
    Overdub { track: usize },
    StopTrack { track: usize },
    Play { track: usize },
    ClearTrack { track: usize },
    SetMonitor { track: usize, on: bool },
    /// Where a track sits between the speakers: -1.0 hard left, 0.0 centre, +1.0 hard right.
    SetPan { track: usize, pan: f32 },
    /// The channel fader, linear `0..=MAX_TRACK_GAIN` (+12 dB). After the chain and the inserts.
    SetTrackGain { track: usize, gain: f32 },
    /// Channel mute and solo. Listening only - what is recorded stays the raw input.
    SetTrackMute { track: usize, on: bool },
    SetTrackSolo { track: usize, on: bool },
    /// Which output buses one source of a track (its loop or its monitored input) is heard on.
    SetTrackSend {
        track: usize,
        source: TrackSource,
        send: BusSend,
    },
    /// One bus of that set on or off, which is how it is operated: a switch per bus. The set it
    /// belongs to is read from the newest engine snapshot before the bit is written back, so
    /// switching a track off Main cannot take it off the headphones as well.
    SetTrackBus {
        track: usize,
        source: TrackSource,
        bus: Bus,
        on: bool,
    },
    /// Volume of one output bus, linear.
    SetBusGain { bus: Bus, gain: f32 },
    /// Which device output channels one bus leaves on.
    SetBusOutput { bus: Bus, out: BusOutput },
    /// What this track subtracts while recording, in frames. Takes effect for input arriving from
    /// now on; nothing already in a layer buffer moves.
    SetTrackLatency {
        track: usize,
        latency: TrackLatency,
    },
    LayerMute { track: usize, layer: usize, muted: bool },
    LayerRemove { track: usize, layer: usize },
    LayerGain { track: usize, layer: usize, gain: f32 },
    ClearAll,
    /// Start or stop the clock of free looping; `None` toggles. While it stands nothing counts,
    /// clicks or plays; the loops are kept and a start plays them from bar 1.
    Transport { run: Option<bool> },
    SetClick { on: bool },
    /// Volume of the click, linear.
    SetClickGain { gain: f32 },
    /// Volume of every monitored live input, linear. Output side only.
    SetMonitorGain { gain: f32 },
    /// Tempo, time signature and - when given - the loop length in bars. Free looping only, and
    /// only while every track is empty.
    SetTempo {
        bpm: f64,
        beats_per_bar: u32,
        beat_unit: u32,
        bars: Option<u32>,
    },
    /// One beat of tap tempo (key T, the Live page's Tap button, a MIDI pad or foot switch).
    TapTempo,
    /// Switch the grid a take snaps to while the engine runs. Takes that are already armed keep
    /// the position they were given - the engine has those commands already.
    SetQuantize { quantize: Quantize },
    /// "All notes off" on one track's instrument, or on every instrument.
    Panic { track: Option<usize> },
    /// What `O` does in free looping: stay on until pressed again, or one pass.
    SetOverdubMode { mode: OverdubMode },

    // --- effects. They act on playback and monitoring; the recording stays dry. ---------------
    /// Whole chain of one track out of the signal path (bit-identical pass-through) or back in.
    FxBypass { track: usize, on: bool },
    /// One effect of one track's chain.
    FxEnable {
        track: usize,
        slot: FxSlot,
        on: bool,
    },
    /// One knob. The wire format resolves the name to this before it gets here.
    FxParam { track: usize, param: FxParam },
    /// Load a ready-made chain.
    FxPreset { track: usize, preset: FxPreset },
}

/// One press of the score transport.
///
/// Separate from [`Action`] because every one of these answers with a German sentence rather than
/// with success or failure: the runner has no failure. `goto` while a change is armed reports how
/// far that change still is and sends nothing, and that sentence is the whole answer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScoreAction {
    /// Count-in, then the first section.
    Start,
    /// The release button: arm the change to the next section.
    Next,
    /// Jump to a section instead of taking the next one. Zero-based.
    Goto(usize),
    /// Stop every track and end the run.
    StopAll,
    /// Clear every track and put the score back before its first section, ready to start again.
    Rewind,
    /// Bars of count-in for the next start. Only before the score starts.
    CountIn(u32),
}

impl Action {
    /// Whether this action moves the transport of a track, which is the runner's business while a
    /// score is playing.
    ///
    /// The split is deliberate and it is the whole rule: **the runner owns the transport, the human
    /// owns the mix.** A manual `record` against a running score would put a take on the timeline
    /// the runner did not schedule, and the runner's predicted loop geometry - the thing that makes
    /// every later overdub land on the right sample - would be wrong from that moment on (see the
    /// module comment of `looper_engine::engine::runner`). Panning, layer gains, mutes, the effect
    /// chains and the click change nothing about where a take sits, so they stay available: that is
    /// exactly the work one does *while* a score runs.
    fn moves_transport(&self) -> bool {
        matches!(
            self,
            Action::Record { .. }
                | Action::Overdub { .. }
                | Action::StopTrack { .. }
                | Action::Play { .. }
                | Action::ClearTrack { .. }
                | Action::ClearAll
                | Action::Transport { .. }
                | Action::SetTempo { .. }
                | Action::TapTempo
                | Action::SetQuantize { .. }
        )
    }

    /// The track this action addresses, if it addresses one.
    fn track(&self) -> Option<usize> {
        match *self {
            Action::Record { track }
            | Action::Overdub { track }
            | Action::StopTrack { track }
            | Action::Play { track }
            | Action::ClearTrack { track }
            | Action::SetMonitor { track, .. }
            | Action::SetPan { track, .. }
            | Action::SetTrackGain { track, .. }
            | Action::SetTrackMute { track, .. }
            | Action::SetTrackSolo { track, .. }
            | Action::SetTrackSend { track, .. }
            | Action::SetTrackBus { track, .. }
            | Action::SetTrackLatency { track, .. }
            | Action::LayerMute { track, .. }
            | Action::LayerRemove { track, .. }
            | Action::LayerGain { track, .. }
            | Action::FxBypass { track, .. }
            | Action::FxEnable { track, .. }
            | Action::FxParam { track, .. }
            | Action::FxPreset { track, .. }
            | Action::Panic {
                track: Some(track),
            } => Some(track),
            Action::Panic { track: None }
            | Action::ClearAll
            | Action::Transport { .. }
            | Action::SetClick { .. }
            | Action::SetClickGain { .. }
            | Action::SetMonitorGain { .. }
            | Action::SetBusGain { .. }
            | Action::SetBusOutput { .. }
            | Action::SetTempo { .. }
            | Action::TapTempo
            | Action::SetQuantize { .. }
            | Action::SetOverdubMode { .. } => None,
        }
    }
}

enum Request {
    ListDevices(Sender<Result<DeviceReport, String>>),
    Start(Box<StartConfig>, Sender<Result<EngineInfo, String>>),
    Stop(Sender<Result<(), String>>),
    /// What the running engine opened, or `None` - for a page that was reloaded while it runs.
    Info(Sender<Result<Option<EngineInfo>, String>>),
    Act(Action, Sender<Result<(), String>>),
    ScoreLoad(String, Option<u32>, Sender<Result<ScoreLoaded, String>>),
    ScoreAct(ScoreAction, Sender<Result<String, String>>),
    Calibrate(Box<CalibrateConfig>, Sender<Result<CalibrateOutcome, String>>),
    /// End the recording to disk that is running now.
    RecordingStop(Sender<Result<(), String>>),

    /// The mix as the score's `mixer:` section would write it: every track and the buses.
    MixerState(Sender<Result<MixerSource, String>>),
    /// The entries changed by a fader, a knob or a pad since the last call, and only those.
    MixerTake(Sender<Result<MixerSource, String>>),
    /// Put the engine into a mix (an undo in the arrangement). Tracks by name.
    MixerApply(Box<MixerSource>, Sender<Result<(), String>>),

    // --- MIDI. Handled on this thread because that is where the session is, and because a MIDI
    // connection belongs to the thread that opened it.
    MidiPorts(Sender<Result<Vec<MidiPortView>, String>>),
    MidiOpen(String, Sender<Result<MidiView, String>>),
    /// `Some(port or device)` disconnects that one, `None` all of them.
    MidiClose(Option<String>, Sender<Result<MidiView, String>>),
    /// `Some(address)` arms the learn mode for that address, `None` cancels it.
    MidiLearn(Option<String>, Sender<Result<MidiView, String>>),
    /// Id and, when several profiles bind it, the device.
    MidiUnbind(String, Option<String>, Sender<Result<MidiView, String>>),
    MidiEdit(String, Option<String>, BindingOptions, Sender<Result<MidiView, String>>),
    MidiSave(Sender<Result<MidiSaved, String>>),
    MidiState(Sender<Result<MidiView, String>>),
    /// Move, rename or delete a device's profile.
    MidiProfile(ProfileAction, Sender<Result<MidiView, String>>),
}

/// Cloneable, `Send + Sync` handle held in Tauri's managed state. Nothing audio-related crosses
/// it, only requests and answers.
#[derive(Clone)]
pub struct EngineHandle {
    tx: Sender<Request>,
}

impl EngineHandle {
    /// Start the host thread. Call from Tauri's `setup`, i.e. after WebView2 has initialised COM
    /// on the main thread - the spike proved the two apartments do not collide.
    pub fn spawn(app: AppHandle) -> Self {
        let (tx, rx) = mpsc::channel::<Request>();
        std::thread::Builder::new()
            .name("looper-engine".into())
            .spawn(move || host_thread(rx, app))
            .expect("the audio thread did not start");
        Self { tx }
    }

    fn call<T>(&self, make: impl FnOnce(Sender<Result<T, String>>) -> Request) -> Result<T, String> {
        let (tx, rx) = mpsc::channel();
        self.tx
            .send(make(tx))
            .map_err(|_| "The audio thread is no longer running.".to_string())?;
        rx.recv()
            .map_err(|_| "The audio thread did not answer.".to_string())?
    }

    pub fn list_devices(&self) -> Result<DeviceReport, String> {
        self.call(Request::ListDevices)
    }

    pub fn start(&self, config: StartConfig) -> Result<EngineInfo, String> {
        self.call(|reply| Request::Start(Box::new(config), reply))
    }

    pub fn stop(&self) -> Result<(), String> {
        self.call(Request::Stop)
    }

    pub fn info(&self) -> Result<Option<EngineInfo>, String> {
        self.call(Request::Info)
    }

    pub fn act(&self, action: Action) -> Result<(), String> {
        self.call(|reply| Request::Act(action, reply))
    }

    pub fn score_load(&self, yaml: String, count_in: Option<u32>) -> Result<ScoreLoaded, String> {
        self.call(|reply| Request::ScoreLoad(yaml, count_in, reply))
    }

    pub fn score_act(&self, action: ScoreAction) -> Result<String, String> {
        self.call(|reply| Request::ScoreAct(action, reply))
    }

    pub fn recording_stop(&self) -> Result<(), String> {
        self.call(Request::RecordingStop)
    }

    pub fn calibrate(&self, config: CalibrateConfig) -> Result<CalibrateOutcome, String> {
        self.call(|reply| Request::Calibrate(Box::new(config), reply))
    }

    pub fn mixer_state(&self) -> Result<MixerSource, String> {
        self.call(Request::MixerState)
    }

    pub fn mixer_take(&self) -> Result<MixerSource, String> {
        self.call(Request::MixerTake)
    }

    pub fn mixer_apply(&self, mixer: MixerSource) -> Result<(), String> {
        self.call(|reply| Request::MixerApply(Box::new(mixer), reply))
    }

    // ---- MIDI ------------------------------------------------------------------------------

    pub fn midi_ports(&self) -> Result<Vec<MidiPortView>, String> {
        self.call(Request::MidiPorts)
    }

    pub fn midi_open(&self, device: String) -> Result<MidiView, String> {
        self.call(|reply| Request::MidiOpen(device, reply))
    }

    pub fn midi_close(&self, device: Option<String>) -> Result<MidiView, String> {
        self.call(|reply| Request::MidiClose(device, reply))
    }

    pub fn midi_learn(&self, address: Option<String>) -> Result<MidiView, String> {
        self.call(|reply| Request::MidiLearn(address, reply))
    }

    pub fn midi_unbind(&self, id: String, device: Option<String>) -> Result<MidiView, String> {
        self.call(|reply| Request::MidiUnbind(id, device, reply))
    }

    pub fn midi_edit(
        &self,
        id: String,
        device: Option<String>,
        options: BindingOptions,
    ) -> Result<MidiView, String> {
        self.call(|reply| Request::MidiEdit(id, device, options, reply))
    }

    pub fn midi_save(&self) -> Result<MidiSaved, String> {
        self.call(Request::MidiSave)
    }

    pub fn midi_state(&self) -> Result<MidiView, String> {
        self.call(Request::MidiState)
    }

    pub fn midi_profile(&self, action: ProfileAction) -> Result<MidiView, String> {
        self.call(|reply| Request::MidiProfile(action, reply))
    }
}

// ---------------------------------------------------------------------------------------------
// The thread
// ---------------------------------------------------------------------------------------------

fn host_thread(rx: Receiver<Request>, app: AppHandle) {
    log!("Audio thread ready (engine not running yet).");
    let mut session: Option<Session> = None;
    // The MIDI side lives here rather than inside `Session`, because it outlives one: a controller
    // stays connected across a stop and a restart of the engine, and the learn mode has to work
    // before anything is started at all.
    let mut midi = MidiBridge::new();

    loop {
        // An open MIDI port needs the short tick as much as a running engine does: the queue
        // between the driver callback and here is drained on this loop, and a pad that arrives four
        // milliseconds late is inaudible while one that arrives two hundred late is not.
        let tick = if session.is_some() || midi.is_open() {
            RUNNING_TICK
        } else {
            IDLE_TICK
        };
        match rx.recv_timeout(tick) {
            Ok(request) => handle(request, &mut session, &mut midi, &app),
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => break,
        }

        if let Some(active) = session.as_mut() {
            active.service(&app);
            if active.finished() {
                log!("The engine ended by itself.");
                stop_session(&mut session, &app);
            }
        }

        sync_instruments(&mut midi, session.as_mut());
        pump_midi(&mut midi, &mut session, &app);
        // Recordings whose writer has finished - also those of a session that is gone.
        crate::recording::poll(&app);
    }

    // The handle was dropped, so the app is going down. Give the device back properly anyway.
    stop_session(&mut session, &app);
    log!("Audio thread ended.");
}

fn handle(
    request: Request,
    session: &mut Option<Session>,
    midi: &mut MidiBridge,
    app: &AppHandle,
) {
    match request {
        Request::ListDevices(reply) => {
            let report = survey_devices();
            for host in &report.hosts {
                match &host.error {
                    Some(e) => log!("Host {}: {e}", host.name),
                    None => log!(
                        "Host {}: {} devices, default output {}",
                        host.name,
                        host.devices.len(),
                        host.default_output.as_deref().unwrap_or("none")
                    ),
                }
            }
            let _ = reply.send(Ok(report));
        }
        Request::Start(config, reply) => {
            // Restart with a different configuration is a stop plus a start; doing it here rather
            // than refusing keeps the frontend from having to sequence two commands.
            if session.is_some() {
                log!("Engine already running - stopping it for the restart.");
                stop_session(session, app);
            }
            match Session::start(*config) {
                Ok(active) => {
                    // Every parameter is back at its starting value, so every knob has to catch its
                    // parameter again. Without this, pickup would protect the first touch after
                    // startup and nothing afterwards.
                    midi.rearm();
                    let info = active.info.clone();
                    let stereo = info.tracks.iter().filter(|t| t.channels() == 2).count();
                    log!(
                        "Engine started: {} / {} @ {} Hz, {} frames, {} tracks ({} of them stereo), \
                         default latency {} frames, {} tracks with their own value.",
                        info.host,
                        info.output_device,
                        info.sample_rate,
                        info.buffer_frames,
                        info.tracks.len(),
                        stereo,
                        info.latency_frames,
                        info.tracks
                            .iter()
                            .filter(|t| t.latency_frames.is_some() || t.latency_trim != 0)
                            .count()
                    );
                    *session = Some(active);
                    let _ = reply.send(Ok(info));
                }
                Err(e) => {
                    log!("Engine does not start: {e}");
                    let _ = reply.send(Err(e));
                }
            }
        }
        Request::Info(reply) => {
            let _ = reply.send(Ok(session.as_ref().map(|s| s.info.clone())));
        }
        Request::Stop(reply) => {
            if session.is_none() {
                let _ = reply.send(Err("The engine is not running.".to_string()));
                return;
            }
            stop_session(session, app);
            log!("Engine stopped.");
            let _ = reply.send(Ok(()));
        }
        Request::Act(action, reply) => {
            let result = match session.as_mut() {
                Some(active) => active.apply(action),
                None => Err(NO_ENGINE.to_string()),
            };
            if let Err(e) = &result {
                log!("Command refused: {e}");
            }
            let _ = reply.send(result);
        }
        Request::ScoreLoad(yaml, count_in, reply) => {
            let result = load_score_onto(session, midi, app, &yaml, count_in);
            match &result {
                Ok(loaded) => {
                    log!("Score loaded: {}", loaded.message);
                    // The score's `midi:` block supplements the profile for this one piece - the
                    // same overlay the CLI does. Every pad the score takes over is said out loud.
                    match midi.set_score(Some(&loaded.score)) {
                        Ok(notes) => {
                            for note in notes {
                                log!("MIDI: {note}");
                                midi.say(note);
                            }
                        }
                        Err(issues) => {
                            let text = format!(
                                "The score's MIDI bindings were not applied: {}",
                                issues.join(" ")
                            );
                            log!("{text}");
                            midi.say(text);
                        }
                    }
                    // A score sets tempo, grid and every parameter it names; the knobs have to
                    // catch up with that before they act.
                    midi.rearm();
                }
                Err(e) => log!("Score not loaded: {e}"),
            }
            let _ = reply.send(result);
        }
        Request::ScoreAct(action, reply) => {
            let result = match session.as_mut() {
                Some(active) => active.score_act(action),
                None => Err(NO_ENGINE.to_string()),
            };
            if let Ok(message) = &result {
                log!("Score: {message}");
            }
            let _ = reply.send(result);
        }
        Request::RecordingStop(reply) => {
            let result = match session.as_mut() {
                Some(active) => active.stop_recording(),
                None => Err(NO_ENGINE.to_string()),
            };
            let _ = reply.send(result);
        }
        Request::Calibrate(config, reply) => {
            let _ = reply.send(run_calibration(*config, session.is_some()));
        }
        Request::MixerState(reply) => {
            let _ = reply.send(match session.as_ref() {
                Some(active) => Ok(active.mixer_state(None)),
                None => Err(NO_ENGINE.to_string()),
            });
        }
        Request::MixerTake(reply) => {
            let _ = reply.send(match session.as_mut() {
                Some(active) => Ok(active.mixer_take()),
                None => Err(NO_ENGINE.to_string()),
            });
        }
        Request::MixerApply(mixer, reply) => {
            let _ = reply.send(match session.as_mut() {
                Some(active) => {
                    active.queue_mix(&mixer);
                    Ok(())
                }
                None => Err(NO_ENGINE.to_string()),
            });
        }

        // --- MIDI -------------------------------------------------------------------------
        Request::MidiPorts(reply) => {
            let _ = reply.send(midi.ports());
        }
        Request::MidiOpen(device, reply) => {
            let result = midi.open(&device);
            match &result {
                Ok(view) => log!("MIDI: {}", view.message.as_deref().unwrap_or("")),
                Err(e) => log!("MIDI input not opened: {e}"),
            }
            let _ = reply.send(result);
        }
        Request::MidiClose(device, reply) => {
            let view = midi.close(device.as_deref());
            log!("MIDI: {}", view.message.as_deref().unwrap_or(""));
            let _ = reply.send(Ok(view));
        }
        Request::MidiLearn(address, reply) => {
            let result = match address {
                Some(address) => midi.arm(&address),
                None => Ok(midi.cancel_learn()),
            };
            if let Some(text) = result.as_ref().ok().and_then(|v| v.message.clone()) {
                log!("MIDI: {text}");
            }
            let _ = reply.send(result);
        }
        Request::MidiUnbind(id, device, reply) => {
            let _ = reply.send(midi.unbind(&id, device.as_deref()));
        }
        Request::MidiEdit(id, device, options, reply) => {
            let _ = reply.send(midi.edit(&id, device.as_deref(), &options));
        }
        Request::MidiSave(reply) => {
            let result = midi.save();
            match &result {
                Ok(saved) => log!("MIDI: {}", saved.message),
                Err(e) => log!("MIDI profile not saved: {e}"),
            }
            let _ = reply.send(result);
        }
        Request::MidiState(reply) => {
            let _ = reply.send(Ok(midi.view()));
        }
        Request::MidiProfile(action, reply) => {
            let result = midi.manage(&action);
            if let Some(text) = result.as_ref().ok().and_then(|v| v.message.clone()) {
                log!("MIDI: {text}");
            }
            let _ = reply.send(result);
        }
    }
}

// ---------------------------------------------------------------------------------------------
// MIDI: from the queue into the session
// ---------------------------------------------------------------------------------------------

/// One resolved MIDI intent, on the way to the same two doors a mouse click uses.
///
/// There are two of them because the program has two: everything that changes the mix or the
/// transport of a track is an [`Action`] and goes through [`Session::apply`]; the score's own
/// transport is a [`ScoreAction`] and goes to the runner. A pad uses exactly these doors and no
/// private one, which is what makes "a pad can do what a mouse can do, and no more" true by
/// construction rather than by inspection.
#[derive(Debug, Clone, PartialEq)]
enum MidiIntent {
    Session(Action),
    Score(ScoreAction),
    /// A plugin slot: bypass or parameter. Handled by the plugin manager (`crate::plugins`).
    Plugin(MidiAction),
}

/// A resolved MIDI action as the action a click produces.
///
/// Two of them need a number this module does not carry, and both are deliberate:
///
/// * `Tempo` sets a BPM and leaves the time signature alone - a knob has one dimension, and the
///   metre is not something one nudges with a fader.
/// * `LatencyTrim` is the *manual surcharge* only. The measured half belongs to `calibrate` and is
///   passed straight back in, so turning the trim knob can never wipe out a measurement.
fn intent_of(action: MidiAction, signature: TimeSignature, measured: Option<u32>) -> MidiIntent {
    match action {
        MidiAction::ScoreStart => MidiIntent::Score(ScoreAction::Start),
        MidiAction::ScoreNext => MidiIntent::Score(ScoreAction::Next),
        MidiAction::ScoreStopAll => MidiIntent::Score(ScoreAction::StopAll),
        MidiAction::ScoreGoto(section) => MidiIntent::Score(ScoreAction::Goto(section)),

        MidiAction::Click(on) => MidiIntent::Session(Action::SetClick { on }),
        MidiAction::ClearAll => MidiIntent::Session(Action::ClearAll),
        MidiAction::Transport => MidiIntent::Session(Action::Transport { run: None }),
        MidiAction::Panic { track } => MidiIntent::Session(Action::Panic { track }),
        MidiAction::Tempo(bpm) => MidiIntent::Session(Action::SetTempo {
            bpm,
            beats_per_bar: signature.beats_per_bar,
            beat_unit: signature.beat_unit,
            bars: None,
        }),
        MidiAction::TapTempo => MidiIntent::Session(Action::TapTempo),
        MidiAction::SetQuantize(quantize) => {
            MidiIntent::Session(Action::SetQuantize { quantize })
        }

        MidiAction::Record { track } => MidiIntent::Session(Action::Record { track }),
        MidiAction::Overdub { track } => MidiIntent::Session(Action::Overdub { track }),
        MidiAction::Play { track } => MidiIntent::Session(Action::Play { track }),
        MidiAction::StopTrack { track } => MidiIntent::Session(Action::StopTrack { track }),
        MidiAction::ClearTrack { track } => MidiIntent::Session(Action::ClearTrack { track }),
        MidiAction::Monitor { track, on } => {
            MidiIntent::Session(Action::SetMonitor { track, on })
        }
        MidiAction::Pan { track, pan } => MidiIntent::Session(Action::SetPan { track, pan }),
        MidiAction::Gain { track, gain } => {
            MidiIntent::Session(Action::SetTrackGain { track, gain })
        }
        MidiAction::Mute { track, on } => MidiIntent::Session(Action::SetTrackMute { track, on }),
        MidiAction::Solo { track, on } => MidiIntent::Session(Action::SetTrackSolo { track, on }),
        MidiAction::ClickGain(gain) => MidiIntent::Session(Action::SetClickGain { gain }),
        MidiAction::MonitorGain(gain) => MidiIntent::Session(Action::SetMonitorGain { gain }),
        MidiAction::Send {
            track,
            source,
            send,
        } => MidiIntent::Session(Action::SetTrackSend {
            track,
            source,
            send,
        }),
        MidiAction::BusGain { bus, gain } => MidiIntent::Session(Action::SetBusGain { bus, gain }),
        MidiAction::LatencyTrim { track, trim } => {
            MidiIntent::Session(Action::SetTrackLatency {
                track,
                latency: TrackLatency { measured, trim },
            })
        }

        MidiAction::LayerMute {
            track,
            layer,
            muted,
        } => MidiIntent::Session(Action::LayerMute {
            track,
            layer,
            muted,
        }),
        MidiAction::LayerRemove { track, layer } => {
            MidiIntent::Session(Action::LayerRemove { track, layer })
        }
        MidiAction::LayerGain { track, layer, gain } => {
            MidiIntent::Session(Action::LayerGain { track, layer, gain })
        }

        MidiAction::FxBypass { track, on } => MidiIntent::Session(Action::FxBypass { track, on }),
        MidiAction::FxEnable { track, slot, on } => {
            MidiIntent::Session(Action::FxEnable { track, slot, on })
        }
        MidiAction::FxPreset { track, preset } => {
            MidiIntent::Session(Action::FxPreset { track, preset })
        }
        MidiAction::FxParam { track, param } => {
            MidiIntent::Session(Action::FxParam { track, param })
        }
        MidiAction::InsertBypass { .. } | MidiAction::InsertParam { .. } => {
            MidiIntent::Plugin(action)
        }
    }
}

/// The session as the MIDI status sees it: track names, and the sections of a loaded score.
fn midi_shape(session: Option<&Session>) -> SessionShape {
    match session {
        None => SessionShape::default(),
        Some(active) => SessionShape {
            tracks: Some(active.tracks.iter().map(|ui| ui.name.clone()).collect()),
            sections: active
                .runner
                .as_ref()
                .map(|runner| runner.score().sections.len()),
        },
    }
}

/// Keep the instrument routing in line with the open ports and the running engine: which port
/// feeds which instrument track, what each port's driver callback sends the fast way, and whether
/// learn mode currently takes everything. Every turn of the host loop; does nothing unless
/// something changed.
fn sync_instruments(midi: &mut MidiBridge, session: Option<&mut Session>) {
    looper_engine::engine::source::set_learning(midi.is_learning());
    match session {
        Some(active) => active.sync_instruments(midi),
        // No engine, nothing to play: every key goes back to the bindings.
        None => {
            for port in midi.instrument_ports() {
                if port.route.active() {
                    port.route.clear();
                }
            }
        }
    }
}

/// Drain the MIDI queue, apply what it means, and tell the web view.
///
/// Called once per turn of the host loop, which is every four milliseconds while anything is open.
/// Nothing here allocates unless an event actually arrived.
fn pump_midi(midi: &mut MidiBridge, session: &mut Option<Session>, app: &AppHandle) {
    // Known controllers connect by themselves, unplugged ones are dropped, profiles edited by hand
    // are read again - see the module comment of `midi.rs`.
    for line in midi.tick() {
        log!("{line}");
    }
    // What the status of every binding is judged against: which tracks exist, whether a score is
    // loaded. Sampled, because a session's shape changes a few times an evening.
    if midi.shape_due() {
        midi.set_shape(midi_shape(session.as_ref()));
    }

    let mut events = Vec::new();
    midi.drain(&mut events);

    for (link, event) in events {
        // The learn mode swallows the stream: while it waits for a control, that control must not
        // also do its old job on the way in.
        if midi.is_learning() {
            // A key an instrument plays never reaches a binding; learning it would make a binding
            // that cannot fire.
            if midi.played_by_instrument(link, &event) {
                let reason = format!(
                    "Not learned: {} is played by an instrument track. Use a pad, another \
                     channel or a key outside the instrument's range.",
                    event.describe().split_whitespace().collect::<Vec<_>>().join(" ")
                );
                midi.report(link, &event, MidiOutcome::Refused, None, Some(reason));
                continue;
            }
            if let Some(report) = midi.feed_learn(link, &event) {
                log!("MIDI learned: {}", report.note.as_deref().unwrap_or(""));
            }
            continue;
        }

        let resolution = match session.as_ref() {
            Some(active) => {
                let layout = active.layout();
                let state = SessionState { session: active };
                let ctx = Context {
                    layout: &layout,
                    state: &state,
                    score_is_playing: active.score_is_playing(),
                };
                midi.resolve(link, &event, &ctx)
            }
            // Without an engine there are no tracks to resolve a binding against. The router says
            // so per binding, which is a better sentence than one blanket refusal.
            None => {
                let layout = TrackLayout::default();
                let ctx = Context::bare(&layout);
                midi.resolve(link, &event, &ctx)
            }
        };

        let target = midi.target_of(link, &event);
        match resolution {
            Resolution::Unbound => midi.report(link, &event, MidiOutcome::Unbound, None, None),
            Resolution::Absorbed(reason) => {
                midi.report(link, &event, MidiOutcome::Absorbed, target.as_ref(), Some(reason));
            }
            Resolution::Refused(reason) => {
                log!("MIDI refused: {reason}");
                midi.report(link, &event, MidiOutcome::Refused, target.as_ref(), Some(reason));
            }
            Resolution::Action(action) => {
                let Some(active) = session.as_mut() else {
                    midi.report(
                        link,
                        &event,
                        MidiOutcome::Refused,
                        target.as_ref(),
                        Some(NO_ENGINE.to_string()),
                    );
                    continue;
                };
                let signature = active.scheduler.timeline.signature();
                // The measured half of the compensation is read back out and handed straight in
                // again: a knob on the surcharge must not wipe out a measurement.
                let measured = match action {
                    MidiAction::LatencyTrim { track, .. } => active
                        .tracks
                        .get(track)
                        .and_then(|ui| ui.latency.measured),
                    _ => None,
                };
                let outcome = match intent_of(action, signature, measured) {
                    MidiIntent::Session(what) => active.apply(what).map(|()| None),
                    MidiIntent::Score(what) => active.score_act(what).map(Some),
                    MidiIntent::Plugin(what) => crate::plugins::apply_midi(&what).map(|()| None),
                };
                match outcome {
                    Ok(message) => {
                        // A preset moves every knob at once; the physical ones have to catch up
                        // before they act again.
                        if matches!(action, MidiAction::FxPreset { .. }) {
                            midi.rearm();
                        }
                        midi.report(link, &event, MidiOutcome::Action, target.as_ref(), message);
                    }
                    Err(e) => {
                        log!("MIDI refused: {e}");
                        midi.report(link, &event, MidiOutcome::Refused, target.as_ref(), Some(e));
                    }
                }
            }
        }
    }

    if let Some(feed) = midi.take_feed() {
        let _ = app.emit(MIDI_EVENT, feed);
    }
}

/// Where the router reads the current value of a parameter from: the newest status snapshot.
///
/// Without this, pickup has nothing to pick up against and a toggle flips its own private copy of a
/// state that the mouse and the score also change. Everything below is read out of the same
/// snapshot the screen is drawn from, so the knob and the number agree.
struct SessionState<'a> {
    session: &'a Session,
}

impl SessionState<'_> {
    /// A binding's track reference as an index. A profile says `track.1`, a score `track.stimme`.
    fn index(&self, reference: &TrackRef) -> Option<usize> {
        match reference {
            TrackRef::Index(index) => Some(*index).filter(|i| *i < self.session.tracks.len()),
            TrackRef::Name(name) => self.session.tracks.iter().position(|ui| ui.name == *name),
        }
    }

    fn fx(&self, reference: &TrackRef) -> Option<looper_engine::engine::fx::FxStatus> {
        let index = self.index(reference)?;
        let (status, _) = self.session.last?;
        status.tracks().get(index).map(|track| track.fx)
    }

    /// The rack a plugin address names, with the track resolved.
    fn insert_target(
        &self,
        owner: &looper_engine::midi::InsertOwner,
    ) -> Option<looper_engine::engine::insert::InsertTarget> {
        use looper_engine::engine::insert::InsertTarget;
        use looper_engine::midi::InsertOwner;
        match owner {
            InsertOwner::Track(track) => self.index(track).map(InsertTarget::Track),
            InsertOwner::Bus(bus) => Some(InsertTarget::Bus(*bus)),
        }
    }

    /// The whole bus assignment of one source of a track, as the engine last reported it.
    fn send_of(&self, index: usize, source: TrackSource) -> BusSend {
        let status = self.session.status_of(index);
        match source {
            TrackSource::Loop => status.loop_send,
            TrackSource::Monitor => status.monitor_send,
        }
    }
}

impl ParamState for SessionState<'_> {
    fn value(&self, target: &Target) -> Option<f32> {
        match target {
            Target::Tempo => Some(self.session.scheduler.timeline.bpm() as f32),
            Target::Pan(track) => {
                let index = self.index(track)?;
                Some(self.session.status_of(index).pan)
            }
            Target::LatencyTrim(track) => {
                let index = self.index(track)?;
                Some(self.session.tracks[index].latency.trim as f32)
            }
            Target::Gain(track) => {
                let index = self.index(track)?;
                Some(self.session.status_of(index).gain)
            }
            Target::ClickGain => self.session.last.map(|(status, _)| status.click_gain),
            Target::MonitorGain => self.session.last.map(|(status, _)| status.monitor_gain),
            Target::LayerGain(track, layer) => {
                let index = self.index(track)?;
                self.session.tracks[index].gains.get(*layer).copied()
            }
            Target::FxKnob(track, knob) => {
                let fx = self.fx(track)?;
                Some(knob_value(&fx.settings, *knob))
            }
            Target::BusGain(bus) => {
                let (status, _) = self.session.last?;
                Some(status.bus_gain(*bus))
            }
            Target::InsertParam(owner, slot, id) => {
                crate::plugins::snapshot_param(self.insert_target(owner)?, *slot, *id)
            }
            _ => None,
        }
    }

    fn switch(&self, target: &Target) -> Option<bool> {
        match target {
            Target::Click => self.session.last.map(|(status, _)| status.click),
            Target::Monitor(track) => {
                let index = self.index(track)?;
                Some(self.session.status_of(index).monitor)
            }
            Target::LayerMute(track, layer) => {
                let index = self.index(track)?;
                let status = self.session.status_of(index);
                if *layer >= status.layers as usize {
                    return None;
                }
                Some(status.muted_mask & (1 << layer) != 0)
            }
            Target::Mute(track) => {
                let index = self.index(track)?;
                Some(self.session.status_of(index).muted)
            }
            Target::Solo(track) => {
                let index = self.index(track)?;
                Some(self.session.status_of(index).soloed)
            }
            Target::FxBypass(track) => Some(self.fx(track)?.bypass),
            Target::FxEnabled(track, slot) => {
                let fx = self.fx(track)?;
                Some(fx.settings.enabled[slot.index()])
            }
            Target::Send(track, source, bus) => {
                let index = self.index(track)?;
                Some(self.send_of(index, *source).on(*bus))
            }
            Target::InsertBypass(owner, slot) => {
                crate::plugins::snapshot_bypass(self.insert_target(owner)?, *slot)
            }
            _ => None,
        }
    }

    fn send(&self, track: usize, source: TrackSource) -> Option<BusSend> {
        self.session.last?;
        Some(self.send_of(track, source))
    }
}

/// Where one effect knob currently stands. The mirror image of [`FxKnob::to_param`].
fn knob_value(settings: &looper_engine::engine::fx::ChainSettings, knob: FxKnob) -> f32 {
    match knob {
        FxKnob::HighPassHz => settings.high_pass_hz,
        FxKnob::BandHz(band) => settings.bands[band].hz,
        FxKnob::BandQ(band) => settings.bands[band].q,
        FxKnob::BandGainDb(band) => settings.bands[band].gain_db,
        FxKnob::CompThresholdDb => settings.comp.threshold_db,
        FxKnob::CompRatio => settings.comp.ratio,
        FxKnob::CompAttackMs => settings.comp.attack_ms,
        FxKnob::CompReleaseMs => settings.comp.release_ms,
        FxKnob::CompKneeDb => settings.comp.knee_db,
        FxKnob::CompMakeupDb => settings.comp.makeup_db,
        FxKnob::DelayFeedback => settings.delay_feedback,
        FxKnob::DelayMix => settings.delay_mix,
        FxKnob::ReverbSize => settings.reverb_size,
        FxKnob::ReverbDamping => settings.reverb_damping,
        FxKnob::ReverbMix => settings.reverb_mix,
    }
}

/// Load a score, rebuilding the engine around the score's tracks when they differ from the running
/// ones.
///
/// # Why a rebuild, and why only on an empty session
///
/// The track layout is part of what the audio callback owns: the track array, the loop buffers of
/// each kind, the size of the buffer pool and of the return queue are all fixed when the streams
/// are built. Changing them under a running callback would need allocation or a hand-over the
/// real-time rules forbid. So the layout is not *changed*, the session is **rebuilt**: the same
/// stop and start a changed setup goes through - streams dropped on this thread, every buffer of
/// the new session allocated before its streams exist. Nothing new happens on the audio side.
///
/// That costs a short gap in the audio (the device is closed and reopened) and it throws away
/// whatever the tracks hold, which is why it is only done while every track is empty and no score
/// plays; otherwise the old refusal stands, with the reason. Device, buffer size, sample rate,
/// latency default, bus outputs, bus volumes and the click carry over from the running session.
fn load_score_onto(
    session: &mut Option<Session>,
    midi: &mut MidiBridge,
    app: &AppHandle,
    yaml: &str,
    count_in: Option<u32>,
) -> Result<ScoreLoaded, String> {
    let Some(active) = session.as_mut() else {
        return Err(NO_ENGINE.to_string());
    };
    let score = compile_score(yaml).map_err(|error| render_issues(&error))?;
    let Err(mismatch) = check_tracks(&score, &active.defs) else {
        return active.load_score(yaml, count_in).map(|loaded| ScoreLoaded {
            engine: None,
            ..loaded
        });
    };
    if active.score_is_playing() {
        return Err(format!(
            "{mismatch} A score is playing - press \"Stop all\" first; the engine can only be \
             rebuilt with the score's tracks while nothing plays."
        ));
    }
    if let Some(busy) = active.first_non_empty_track() {
        return Err(format!(
            "The score's tracks differ from the running engine's, and rebuilding the engine would \
             throw away what track \"{busy}\" holds. Clear all tracks first, then load again. \
             ({mismatch})"
        ));
    }

    let previous = active.config.clone();
    let config = rebuilt_config(
        &previous,
        &score,
        active.last.map(|(status, _)| status).as_ref(),
        active.scheduler.quantize,
    );
    log!("Rebuilding the engine for the score: {mismatch}");
    // Not `stop_session`: that would tell the frontend "stopped" and the live view would flip to
    // the setup screen for the moment the rebuild takes.
    if let Some(mut old) = session.take() {
        old.save_free_mix(true);
        old.shutdown();
    }
    match Session::start(config) {
        Ok(fresh) => {
            *session = Some(fresh);
            midi.rearm();
        }
        Err(e) => {
            log!("Rebuild failed: {e}");
            let restored = Session::start(previous);
            let note = match &restored {
                Ok(_) => "The previous setup is running again.",
                Err(_) => "The engine is stopped; start it again from the setup screen.",
            };
            *session = restored.ok();
            return Err(format!(
                "Could not rebuild the engine with the score's tracks: {e} {note}"
            ));
        }
    }
    let active = session.as_mut().expect("just started");
    let info = active.info.clone();
    let _ = app.emit(ENGINE_EVENT, &info);
    let loaded = active.load_score(yaml, count_in)?;
    let names: Vec<&str> = info.tracks.iter().map(|t| t.name.as_str()).collect();
    let message = format!(
        "Engine rebuilt with the score's tracks ({}). {}",
        names.join(", "),
        loaded.message
    );
    active.message = Some(message.clone());
    Ok(ScoreLoaded {
        score: loaded.score,
        message,
        engine: Some(info),
    })
}

/// The start configuration a rebuild uses: the running one, with the score's tracks and grid, and
/// with what was changed at runtime (bus outputs and volumes, click, grid) carried over from the
/// newest snapshot - a rebuild must not reset the monitor volume the musician just turned down.
fn rebuilt_config(
    previous: &StartConfig,
    score: &CompiledScore,
    status: Option<&Status>,
    quantize: Quantize,
) -> StartConfig {
    let mut config = previous.clone();
    config.tracks = score
        .tracks
        .iter()
        .map(|t| TrackConfig {
            name: t.name.clone(),
            input_channel: t.input,
            input_channel_right: t.input_right,
            pan: t.pan,
            latency_frames: t.latency,
            latency_trim: t.latency_trim,
            bus: Some(t.bus.clone()),
            monitor_bus: Some(t.monitor_bus.clone()),
            // Scores do not name instruments yet.
            instrument: None,
        })
        .collect();
    config.bpm = score.bpm;
    config.beats_per_bar = score.beats_per_bar;
    config.beat_unit = score.beat_unit;
    config.bars = score.sections.iter().map(|s| s.bars).max().unwrap_or(1).max(1);
    config.quantize = quantize.into();
    if let Some(status) = status {
        config.click = status.click;
        config.click_gain = status.click_gain;
        config.monitor_gain = status.monitor_gain;
        config.bus_gain_main = Some(status.bus_gain(Bus::Main));
        config.bus_gain_monitor = Some(status.bus_gain(Bus::Monitor));
        for (bus, slot) in [
            (Bus::Main, &mut config.bus_out_main),
            (Bus::Monitor, &mut config.bus_out_monitor),
        ] {
            let out = status.bus_out.get(bus);
            *slot = Some(BusOutConfig {
                channel: out.first as u32 + 1,
                width: out.width as u32,
            });
        }
    }
    config
}

/// A click or monitor volume, refused in a sentence when it is out of range rather than clamped
/// silently.
fn check_level(what: &str, gain: f32) -> Result<(), String> {
    if (0.0..=MAX_LEVEL_GAIN).contains(&gain) {
        Ok(())
    } else {
        Err(format!("{what} level must be between 0.0 and {MAX_LEVEL_GAIN}."))
    }
}

/// Shut the engine down and tell the frontend, in that order.
fn stop_session(session: &mut Option<Session>, app: &AppHandle) {
    if let Some(mut active) = session.take() {
        active.save_free_mix(true);
        active.shutdown();
        let _ = app.emit(STATUS_EVENT, StatusEvent::idle());
    }
}

// ---------------------------------------------------------------------------------------------
// Device survey
// ---------------------------------------------------------------------------------------------

fn buffer_range(size: &SupportedBufferSize) -> (Option<u32>, Option<u32>) {
    match size {
        SupportedBufferSize::Range { min, max } => (Some(*min), Some(*max)),
        SupportedBufferSize::Unknown => (None, None),
    }
}

/// Turn one direction's supported configurations into the wire format. A direction that cannot be
/// queried is not an error - the device simply has nothing on that side, and the reason is
/// appended to `note`.
fn configs_of<E, I>(result: Result<I, E>, label: &str, note: &mut Option<String>) -> Vec<ConfigInfo>
where
    E: std::fmt::Display,
    I: Iterator<Item = cpal::SupportedStreamConfigRange>,
{
    match result {
        Ok(list) => list
            .map(|c| {
                let (min_buffer_frames, max_buffer_frames) = buffer_range(c.buffer_size());
                ConfigInfo {
                    channels: c.channels(),
                    min_sample_rate: c.min_sample_rate(),
                    max_sample_rate: c.max_sample_rate(),
                    sample_format: format!("{:?}", c.sample_format()),
                    min_buffer_frames,
                    max_buffer_frames,
                }
            })
            .collect(),
        Err(e) => {
            let message = format!("{label}: {e}");
            *note = Some(match note.take() {
                Some(previous) => format!("{previous}; {message}"),
                None => message,
            });
            Vec::new()
        }
    }
}

fn survey_devices() -> DeviceReport {
    let mut hosts = Vec::new();
    for host_id in cpal::available_hosts() {
        let name = host_id.name().to_lowercase();
        let host = match cpal::host_from_id(host_id) {
            Ok(h) => h,
            Err(e) => {
                hosts.push(HostInfo {
                    name,
                    error: Some(format!("Could not open the host: {e}")),
                    default_input: None,
                    default_output: None,
                    devices: Vec::new(),
                });
                continue;
            }
        };
        let default_input = host.default_input_device().map(|d| d.to_string());
        let default_output = host.default_output_device().map(|d| d.to_string());
        let devices = match host.devices() {
            Ok(list) => list,
            Err(e) => {
                hosts.push(HostInfo {
                    name,
                    error: Some(format!("Could not read the device list: {e}")),
                    default_input,
                    default_output,
                    devices: Vec::new(),
                });
                continue;
            }
        };

        let mut infos = Vec::new();
        for device in devices {
            let mut note: Option<String> = None;
            let input_configs = configs_of(
                device.supported_input_configs(),
                "Input configurations",
                &mut note,
            );
            let output_configs = configs_of(
                device.supported_output_configs(),
                "Output configurations",
                &mut note,
            );
            infos.push(DeviceInfo {
                name: device.to_string(),
                input: device.supports_input(),
                output: device.supports_output(),
                input_configs,
                output_configs,
                note,
            });
        }

        hosts.push(HostInfo {
            name,
            error: None,
            default_input,
            default_output,
            devices: infos,
        });
    }
    DeviceReport {
        asio_built: asio_built(),
        hosts,
    }
}

pub fn asio_built() -> bool {
    cfg!(feature = "asio")
}

/// Everything the frontend can know without asking the audio thread.
pub fn app_info() -> AppInfo {
    AppInfo {
        version: env!("CARGO_PKG_VERSION").to_string(),
        log_path: logfile::path().display().to_string(),
        logging: logfile::enabled(),
        asio_built: asio_built(),
        max_tracks: MAX_TRACKS as u32,
        max_layers: MAX_LAYERS as u32,
    }
}

// ---------------------------------------------------------------------------------------------
// The score compiler
// ---------------------------------------------------------------------------------------------

/// Translate a score without loading it - what the editor calls while somebody types.
///
/// It opens nothing, needs no engine and does not touch the host thread, so it is safe at any time
/// and while a score is playing. The compiler reports **every** problem at once, each with line,
/// column and often a suggestion (see `looper_engine::score::error`), and that list travels
/// unflattened: an editor needs the positions to put markers on, not a paragraph.
pub fn compile(yaml: &str) -> CompileOutcome {
    match compile_score(yaml) {
        Ok(score) => CompileOutcome::ok(score),
        Err(error) => CompileOutcome::failed(error.issues),
    }
}

/// The same complaints as one block of text, for a caller with nowhere to put markers - the load
/// command, whose answer is a single sentence.
fn render_issues(error: &ScoreError) -> String {
    let mut out = format!(
        "The score does not compile ({} error{}):",
        error.issues.len(),
        if error.issues.len() == 1 { "" } else { "s" }
    );
    for issue in &error.issues {
        out.push_str("\n  ");
        out.push_str(&issue.to_string());
    }
    out
}

// ---------------------------------------------------------------------------------------------
// Calibration
// ---------------------------------------------------------------------------------------------

/// Run the existing `calibrate` measurement. It reports by printing, and printing goes into the
/// log file (see `logfile`), so the answer here is a pointer to that file.
fn run_calibration(config: CalibrateConfig, engine_running: bool) -> Result<CalibrateOutcome, String> {
    if engine_running {
        return Err(
            "The engine is running. Stop it before calibrating - the measurement needs the device \
             to itself."
                .to_string(),
        );
    }
    if !logfile::enabled() {
        return Err(format!(
            "No log file can be written ({}). The calibration reports its result there, so it \
             is not started.",
            logfile::path().display()
        ));
    }
    let dev = DeviceOpts {
        host: config.host.clone(),
        device: config.device.clone(),
        rate: config.sample_rate,
        buffer: config.buffer_frames,
        in_channels: config.input_channels,
        out_channels: config.output_channels,
        force_buffer: config.force_buffer,
    };
    let opts = CalibrateOpts {
        bpm: config.bpm,
        bars: config.bars,
        latency_frames: config.latency_frames,
        runs: config.runs,
        click_gain: config.click_gain,
        // The measurement is per input, so it needs the same track list the session uses; the
        // CLI's own `--track` spelling is what `CalibrateOpts` takes.
        tracks: config
            .tracks
            .iter()
            .map(|t| match t.input_channel_right {
                Some(right) => format!("{}:{}-{}", t.name, t.input_channel, right),
                None => format!("{}:{}", t.name, t.input_channel),
            })
            .collect(),
        for_track: config.for_track,
    };
    let path = logfile::path().display().to_string();
    log!(
        "Calibration starts: {} runs, {} bars, test value {} frames, track {}.",
        opts.runs,
        opts.bars,
        opts.latency_frames,
        opts.for_track
    );
    cmd_calibrate(&dev, &opts)?;
    log!("Calibration done.");
    Ok(CalibrateOutcome {
        message: format!(
            "Calibration finished. The full report, with the recommended latency compensation \
             for this track, is in the log file: {path}"
        ),
        log_path: path,
    })
}

// ---------------------------------------------------------------------------------------------
// A running engine
// ---------------------------------------------------------------------------------------------

/// Counters the audio callbacks keep. Atomics only - a callback must not lock.
#[derive(Default)]
struct HostStats {
    xruns: AtomicU64,
    other_errors: AtomicU64,
    /// Output callbacks that found the input FIFO short.
    underruns: AtomicU64,
    /// Input callbacks that could not push - frames lost, alignment gone.
    overruns: AtomicU64,
    cb_nanos_max: AtomicU64,
}

impl HostStats {
    #[inline]
    fn record_callback(&self, started: Instant) {
        self.cb_nanos_max
            .fetch_max(started.elapsed().as_nanos() as u64, Ordering::Relaxed);
    }

    fn count_error(&self, err: cpal::Error) {
        if err.kind() == cpal::ErrorKind::Xrun {
            self.xruns.fetch_add(1, Ordering::Relaxed);
        } else {
            self.other_errors.fetch_add(1, Ordering::Relaxed);
        }
    }
}

/// The control side's view of one track: what the user calls it, and the layer gains it set.
///
/// Everything else - state, input channel, layer count, mute mask - comes from the engine's own
/// status snapshot. The gains are mirrored here because they are the one piece of layer state the
/// snapshot does not carry, and they only ever change by command, so the mirror cannot go stale.
struct TrackUi {
    name: String,
    /// How many channels this track's loop buffers have. Fixed when the engine is started, and
    /// needed here to work out the memory ceiling after a tempo change.
    channels: Channels,
    gains: Vec<f32>,
    /// This track's compensation as this thread last set it. Mirrored here - like the gains -
    /// because a tempo change is checked against the largest one before the engine sees the
    /// command, and the newest status snapshot may be a few milliseconds old.
    latency: TrackLatency,
}

struct Session {
    // Declaration order is drop order, and the streams have to go first: their callbacks own the
    // engine core, both ends of the input FIFO and every layer buffer currently in use.
    input: cpal::Stream,
    output: cpal::Stream,
    cmd: CommandSender,
    status_rx: StatusReceiver,
    pool: LayerPool,
    stats: Arc<HostStats>,
    scheduler: Scheduler,
    tracks: Vec<TrackUi>,
    /// The engine's track layout as it was resolved at start. Kept because a score is checked
    /// against it (`check_tracks`) and that check compares names *and* inputs, which the display
    /// mirror above does not carry.
    defs: Vec<TrackDef>,
    /// Output channels the device really has, so a routing that names more can be answered with a
    /// sentence instead of with silence.
    output_channels: usize,
    /// The loaded score, or `None`. It owns the whole transport while it is there.
    runner: Option<Runner>,
    info: EngineInfo,
    /// Newest snapshot and when it arrived, for the position estimate.
    last: Option<(Status, Instant)>,
    /// Whether the meters in `last` went out with an event; until then newer snapshots hold them.
    meters_shown: bool,
    last_emit: Instant,
    last_refusal: (u64, Refusal),
    message: Option<String>,
    stopped: bool,
    /// Sample position of a pending `ClearAll`, while a score is loaded. A clear puts every track
    /// back to the pan, buses and latency it was *started* with, so the score's own setup is sent
    /// again once the engine has passed this position - not before, or the clear would undo it.
    reapply_at: Option<u64>,
    /// What this session was started with - the base for rebuilding it around a score's tracks.
    config: StartConfig,
    /// Whether the transport runs, as this thread last told the engine. Mirrored rather than read
    /// from the snapshot because a Record pressed right after Stop must already know it stands.
    transport: bool,
    /// Commands restoring a mix (the stored free-mode one, a score's `mixer:` section, an undo),
    /// sent [`MIX_BATCH`] at a time so a whole mix never overruns the command queue.
    mix_backlog: VecDeque<Command>,
    /// Tracks (by name) and buses a human changed since the arrangement last asked
    /// ([`Session::mixer_take`]). Restores do not count - they are not news to anybody.
    mix_changed: Vec<String>,
    mix_buses_changed: bool,
    /// Goes up with every human change of the mix; the status event carries it, so the arrangement
    /// knows when to ask.
    mix_rev: u64,
    /// Tracks of the free mode whose mix changed and is not on disk yet, and since when.
    free_mix_dirty: Vec<String>,
    free_mix_since: Option<Instant>,
    /// Recording every run to disk (`crate::recording`).
    recorder: crate::recording::Recorder,
    /// Notices when the runner's armed change takes effect - the recording's section markers.
    section_watch: SectionWatch,
    /// The text of the loaded score, for the copy in a recording's folder.
    score_yaml: Option<String>,
    /// Tap tempo, whichever way the taps come in.
    taps: TapTempo,
    /// The engine's end of the MIDI routing queue (`looper_engine::engine::source`).
    feeds: FeedSender,
    /// Per track, in engine order: what plays it, if it is an instrument track.
    instrument_inputs: Vec<Option<InstrumentInput>>,
    /// Port feeds the engine holds, as (slot, serial).
    attached: Vec<(usize, u64)>,
    /// The routing last sent, per track.
    routes: Vec<TrackRoute>,
    /// What each port's mask was last set to, by the port's serial.
    masks: Vec<(u64, Vec<TrackRoute>)>,
    /// When the devices instrument tracks name were last connected.
    last_connect: Option<Instant>,
    /// `C` and the late events as last logged.
    logged_delay: Option<u32>,
    logged_late: u32,
}

impl Session {
    fn start(config: StartConfig) -> Result<Session, String> {
        let started_with = config.clone();
        let dev = DeviceOpts {
            host: config.host.clone(),
            device: config.device.clone(),
            rate: config.sample_rate,
            buffer: config.buffer_frames,
            in_channels: config.input_channels,
            out_channels: config.output_channels,
            force_buffer: config.force_buffer,
        };
        let setup = audio::open_duplex(&dev)?;
        let rate = setup.sample_rate();
        if setup.out_plan.config.sample_rate != rate {
            return Err(format!(
                "Input and output run at different sample rates ({} and {}). \
                 The engine's timeline needs one shared clock.",
                rate, setup.out_plan.config.sample_rate
            ));
        }
        let in_channels = setup.in_plan.config.channels as usize;
        let out_channels = setup.out_plan.config.channels as usize;
        // The tracks come first now: the loop length has to be checked against the *largest*
        // compensation in the session, and that is not known before they are resolved.
        let defs = resolve_tracks(&config.tracks, in_channels)?;
        let timeline = build_timeline(
            rate,
            config.bpm,
            config.beats_per_bar,
            config.beat_unit,
            config.bars,
            max_latency(config.latency_frames, defs.iter().map(|d| d.latency)),
        )?;
        let capacity = loop_capacity(&timeline, config.bars);

        let buffer_frames = setup.buffer_frames().max(1);
        // Room for a driver block far larger than requested, so no callback ever allocates.
        let scratch_frames = (buffer_frames * 16) as usize;

        let stats = Arc::new(HostStats::default());
        let (mut producer, mut consumer) = rtrb::RingBuffer::<f32>::new(
            (buffer_frames * INPUT_FIFO_BUFFERS) as usize * in_channels,
        );
        let (cmd_tx, cmd_rx) = command_channel(256);
        let (status_tx, status_rx) = status_channel(1024);
        // The return queue has room for every buffer that can be inside the engine at once, so the
        // audio thread can always hand one back instead of leaking it.
        let kinds: Vec<Channels> = defs.iter().map(|d| d.input.channels()).collect();
        let slots = spare_slots_for(&kinds, SPARE_SLOTS);
        let (channel, buffer_endpoint) = buffer_channel(
            slots[0] + slots[1] + 8,
            defs.len() * MAX_LAYERS + slots[0] + slots[1] + 8,
        );
        let mut pool = LayerPool::new(channel, capacity as usize, slots);
        // Every layer buffer of this session is born here, before any stream exists; from now on
        // the pool only recycles.
        pool.service(None);

        let routing = config.bus_routing(out_channels)?;
        let bus_gain = config.bus_gain_pair()?;
        // What plays each instrument track, in engine order. `resolve_tracks` keeps the order of
        // the configuration, so the two line up.
        let instrument_inputs: Vec<Option<InstrumentInput>> =
            config.tracks.iter().map(|t| t.instrument.clone()).collect();
        let tracks: Vec<Track> = defs
            .iter()
            .zip(instrument_inputs.iter())
            .map(|(d, instrument)| {
                // An instrument is heard unless its track says otherwise; the global switch is
                // about microphones.
                let monitor = instrument.as_ref().map_or(config.monitor, |i| i.monitor);
                Track::new(d.input, monitor, d.pan, rate)
                    .with_latency(d.latency)
                    .with_sends(d.loop_send, d.monitor_send)
            })
            .collect();
        let mut core = EngineCore::new(EngineConfig {
            timeline,
            latency_frames: config.latency_frames,
            input_channels: in_channels,
            tracks,
            spares: [Vec::with_capacity(slots[0]), Vec::with_capacity(slots[1])],
            layer_frames: capacity,
            commands: cmd_rx,
            status: status_tx,
            buffers: buffer_endpoint,
            monitor_gain: config.monitor_gain,
            click: config.click,
            click_gain: config.click_gain,
            bus_gain,
            routing,
            output_channels: out_channels,
            status_interval: (rate / STATUS_HZ).max(1) as u64,
        });
        // Plugin insert slots: the engine end of the rack's queue goes into the core, the other end
        // to the plugin manager once the streams run (`crate::plugins`).
        let (inserts, insert_endpoint) = crate::plugins::engine_channel();
        core.attach_inserts(insert_endpoint);
        // The recorder's slot: recordings are handed in per run (`crate::recording`).
        let (capture_port, capture_endpoint) = capture_channel();
        core.attach_capture(capture_endpoint);
        // Instrument slots and the MIDI event stage: the instruments come from the main thread
        // (`crate::instruments`), the routing and the port feeds from this one.
        let (instrument_tx, feeds, source_endpoint) = source_channel(64);
        core.attach_sources(source_endpoint, scratch_frames);
        // The app opens with the clock standing: nothing blinks or clicks until the musician
        // presses Start, records something, or loads a score.
        core.hold_transport();

        // ---- input stream: whole frames into the FIFO, nothing else ------------------------
        let stats_in = Arc::clone(&stats);
        let stats_in_err = Arc::clone(&stats);
        let input = audio::build_input(
            &setup.input,
            &setup.in_plan,
            move |data: &[f32], _: &InputCallbackInfo, _offset: usize| {
                let t0 = Instant::now();
                let mut overrun = false;
                for frame in data.chunks_exact(in_channels) {
                    // Whole frames only: half a frame in the FIFO would shift every channel of
                    // every later recording against each other.
                    if producer.slots() < in_channels {
                        overrun = true;
                        break;
                    }
                    for &sample in frame {
                        let _ = producer.push(sample);
                    }
                }
                if overrun {
                    stats_in.overruns.fetch_add(1, Ordering::Relaxed);
                }
                stats_in.record_callback(t0);
            },
            move |err| stats_in_err.count_error(err),
        )?;

        // ---- output stream: drives the engine ----------------------------------------------
        let stats_out = Arc::clone(&stats);
        let stats_out_err = Arc::clone(&stats);
        let mut in_scratch = vec![0.0f32; scratch_frames * in_channels];
        let mut out_scratch = vec![0.0f32; scratch_frames * BUS_SAMPLES];
        let output = audio::build_output(
            &setup.output,
            &setup.out_plan,
            move |data: &mut [f32], info: &OutputCallbackInfo, _offset: usize| {
                let t0 = Instant::now();
                let frames = data.len() / out_channels;
                // When the first frame of this callback is heard, on the clock MIDI is stamped
                // with - what places a played note (`looper_engine::engine::source`).
                let stamp = info.timestamp();
                let playback_ns = stamp.playback.as_nanos() as u64;
                let callback_ns = stamp.callback.as_nanos() as u64;
                let mut done = 0usize;
                while done < frames {
                    let n = (frames - done).min(scratch_frames);
                    let ahead = done as u64 * 1_000_000_000 / u64::from(rate);
                    core.set_clock(HostClock {
                        playback_ns: playback_ns + ahead,
                        callback_ns: callback_ns + ahead,
                        block_frames: frames as u32,
                    });
                    let mut got = 0usize;
                    while got < n && consumer.slots() >= in_channels {
                        for c in 0..in_channels {
                            if let Ok(v) = consumer.pop() {
                                in_scratch[got * in_channels + c] = v;
                            }
                        }
                        got += 1;
                    }
                    if got < n {
                        stats_out.underruns.fetch_add(1, Ordering::Relaxed);
                    }
                    core.process(
                        &in_scratch[..got * in_channels],
                        &mut out_scratch[..n * BUS_SAMPLES],
                    );
                    // The one place a bus becomes a socket. The routing comes from the engine, so
                    // a `SetBusOutput` takes effect on the very next block.
                    let routing = core.routing();
                    let base = done * out_channels;
                    for (i, frame) in data[base..base + n * out_channels]
                        .chunks_exact_mut(out_channels)
                        .enumerate()
                    {
                        let buses = &out_scratch[i * BUS_SAMPLES..(i + 1) * BUS_SAMPLES];
                        place_buses(buses, frame, &routing);
                    }
                    done += n;
                }
                stats_out.record_callback(t0);
            },
            move |err| stats_out_err.count_error(err),
        )?;

        let driver_input_frames = input.buffer_size().ok();
        let driver_output_frames = output.buffer_size().ok();

        // Order matters: input first, then output, exactly as in the phase 0 latency measurement.
        // Both frame counters start on their stream's first callback, and only the same start
        // order gives them the origin the 827 samples were measured against.
        input
            .play()
            .map_err(|e| format!("The input stream does not start: {e}"))?;
        output
            .play()
            .map_err(|e| format!("The output stream does not start: {e}"))?;
        // Plugins and the free-mode mix are kept by track name. A track renamed since the last
        // start takes both along (`crate::layout` says when a change counts as a rename).
        let layout: Vec<LayoutTrack> = defs
            .iter()
            .map(|d| {
                // An instrument track has no input; 0 is what no 1-based input can be.
                let (left, right) = match d.input {
                    TrackInput::Stereo { left, right } => (left as u32 + 1, Some(right as u32 + 1)),
                    TrackInput::Mono(c) => (c as u32 + 1, None),
                    TrackInput::Instrument => (0, None),
                };
                LayoutTrack::new(d.name.clone(), left, right)
            })
            .collect();
        let renamed = layout::replace(&layout);
        for (from, to) in &renamed {
            log!("Track \"{from}\" is now \"{to}\" - its plugins and mix go along.");
        }
        if let Err(e) = mixstore::rename(&renamed) {
            log!("Mix of renamed tracks not carried over: {e}");
        }
        crate::instruments::engine_started(
            instrument_tx,
            rate,
            defs.iter()
                .map(|d| crate::instruments::TrackSlot {
                    name: d.name.clone(),
                    instrument: d.input.is_instrument(),
                })
                .collect(),
            renamed.clone(),
        );
        crate::plugins::engine_started(
            inserts,
            rate,
            defs.iter().map(|d| d.name.clone()).collect(),
            renamed,
        );

        let loop_samples = timeline.span_bars(0, config.bars);
        let info = EngineInfo {
            host: setup.host_name.clone(),
            input_device: setup.in_name.clone(),
            output_device: setup.out_name.clone(),
            input_channels: setup.in_plan.config.channels,
            output_channels: setup.out_plan.config.channels,
            input_format: format!("{:?}", setup.in_plan.format),
            output_format: format!("{:?}", setup.out_plan.format),
            sample_rate: rate,
            buffer_frames,
            driver_input_frames,
            driver_output_frames,
            bpm: config.bpm,
            beats_per_bar: config.beats_per_bar,
            beat_unit: config.beat_unit,
            samples_per_beat: timeline.samples_per_beat(),
            bars: config.bars,
            loop_samples,
            loop_seconds: timeline.samples_to_secs(loop_samples),
            latency_frames: config.latency_frames,
            latency_ms: config.latency_frames as f64 * 1000.0 / rate as f64,
            layer_capacity: capacity,
            max_layers: MAX_LAYERS as u32,
            max_tracks: MAX_TRACKS as u32,
            memory_mb: max_memory_bytes(
                capacity,
                total_channels(&kinds),
                spare_channels(slots),
            ) as f64
                / 1_048_576.0,
            tracks: defs
                .iter()
                .zip(instrument_inputs.iter())
                .map(|(d, instrument)| TrackConfig {
                    input_channel_right: match d.input {
                        TrackInput::Stereo { right, .. } => Some(right as u32 + 1),
                        TrackInput::Mono(_) | TrackInput::Instrument => None,
                    },
                    pan: d.pan,
                    latency_frames: d.latency.measured,
                    latency_trim: d.latency.trim,
                    instrument: instrument.clone(),
                    ..TrackConfig::mono(&d.name, d.input.first() as u32 + 1)
                })
                .collect(),
        };

        let mut session = Session {
            input,
            output,
            cmd: cmd_tx,
            status_rx,
            pool,
            stats,
            scheduler: Scheduler::new(
                timeline,
                config.bars,
                buffer_frames,
                config.quantize.into(),
            ),
            tracks: defs
                .iter()
                .map(|d| TrackUi {
                    name: d.name.clone(),
                    channels: d.input.channels(),
                    gains: Vec::new(),
                    latency: d.latency,
                })
                .collect(),
            defs,
            output_channels: out_channels,
            runner: None,
            info,
            last: None,
            meters_shown: false,
            last_emit: Instant::now() - EMIT_INTERVAL,
            last_refusal: (0, Refusal::None),
            message: None,
            stopped: false,
            reapply_at: None,
            config: started_with,
            transport: false,
            mix_backlog: VecDeque::new(),
            mix_changed: Vec::new(),
            mix_buses_changed: false,
            mix_rev: 0,
            free_mix_dirty: Vec::new(),
            free_mix_since: None,
            recorder: crate::recording::Recorder::new(capture_port, scratch_frames),
            section_watch: SectionWatch::default(),
            score_yaml: None,
            taps: TapTempo::default(),
            feeds,
            routes: vec![TrackRoute::NONE; instrument_inputs.len()],
            instrument_inputs,
            attached: Vec::new(),
            masks: Vec::new(),
            last_connect: None,
            logged_delay: None,
            logged_late: 0,
        };
        // The free mode's faders and chains, by track name - before anything is played, so the
        // first note already sounds the way the last session ended.
        let stored = mixstore::load();
        if !stored.tracks.is_empty() {
            session.queue_mix(&MixerSource {
                buses: None,
                tracks: stored.tracks,
            });
        }
        Ok(session)
    }

    /// Stop the streams and give every buffer back.
    ///
    /// Dropping the streams stops the driver callbacks and, with them, drops the engine core: its
    /// layers, its stock of prepared buffers and both ends of the input FIFO. All of that happens
    /// on this thread, which is allowed to free memory. `pool.drain()` then empties the return
    /// queue and the reserve, so nothing survives into the next session.
    fn shutdown(mut self) {
        // A recording ends with the engine; its writer finishes with what reached it.
        self.recorder.abandon();
        drop(self.input);
        drop(self.output);
        // Every installed plugin processor went down with the engine core just now.
        crate::plugins::engine_stopped();
        crate::instruments::engine_stopped();
        self.pool.drain();
    }

    /// See [`sync_instruments`].
    fn sync_instruments(&mut self, midi: &mut MidiBridge) {
        self.feeds.drain_retired();
        // The devices instrument tracks name are connected by themselves, at the pace the port
        // list is scanned.
        if self
            .last_connect
            .is_none_or(|t| t.elapsed() >= crate::midi::SCAN_INTERVAL)
        {
            self.last_connect = Some(Instant::now());
            let devices: Vec<String> = self
                .instrument_inputs
                .iter()
                .flatten()
                .filter_map(|i| i.device.clone())
                .collect();
            for device in devices {
                if let Some(line) = midi.connect_for_instrument(&device) {
                    log!("{line}");
                }
            }
        }
        let ports = midi.instrument_ports();
        let named: Vec<(usize, &str)> = ports.iter().map(|p| (p.slot, p.name.as_str())).collect();
        let routes = crate::instruments::track_routes(&self.instrument_inputs, &named);
        // Feeds: the engine listens to every port some instrument track takes.
        let wanted: Vec<(usize, u64)> = ports
            .iter()
            .filter(|p| routes.iter().any(|r| r.ports & (1 << p.slot) != 0))
            .map(|p| (p.slot, p.serial))
            .collect();
        for &(slot, serial) in &self.attached.clone() {
            if !wanted.contains(&(slot, serial)) && self.feeds.send(FeedMsg::Detach { port: slot }).is_ok() {
                self.attached.retain(|a| *a != (slot, serial));
            }
        }
        for port in &ports {
            let key = (port.slot, port.serial);
            if !wanted.contains(&key) || self.attached.contains(&key) {
                continue;
            }
            match port.feed.take_feed(port.slot) {
                Some(feed) => {
                    if self.feeds.send(FeedMsg::Attach(feed)).is_ok() {
                        self.attached.push(key);
                        log!("Instruments listen to MIDI device \"{}\".", port.name);
                    }
                }
                // Still held by an engine that is on its way out; the next turn gets it.
                None => {}
            }
        }
        for (track, route) in routes.iter().enumerate() {
            if self.routes.get(track) != Some(route)
                && self.feeds.send(FeedMsg::Route { track, route: *route }).is_ok()
            {
                self.routes[track] = *route;
            }
        }
        // Masks: what each port's driver callback sends the fast way.
        for port in &ports {
            let mine: Vec<TrackRoute> = routes
                .iter()
                .filter(|r| r.ports & (1 << port.slot) != 0)
                .copied()
                .collect();
            let known = self.masks.iter().find(|(s, _)| *s == port.serial).map(|(_, r)| r);
            if known == Some(&mine) || (known.is_none() && mine.is_empty() && !port.route.active()) {
                continue;
            }
            port.route.set(&mine);
            self.masks.retain(|(s, _)| *s != port.serial);
            self.masks.push((port.serial, mine));
        }
        self.masks.retain(|(s, _)| ports.iter().any(|p| p.serial == *s));
    }

    /// The engine acknowledged a `Stop` command.
    fn finished(&self) -> bool {
        self.stopped
    }

    /// One round of control-thread duty: take the newest status, keep the buffer pool filled, and
    /// push an event if the rate allows.
    fn service(&mut self, app: &AppHandle) {
        if let Some(mut status) = self.status_rx.latest() {
            // Meters hold their peaks until the next event shows them (`Status::hold_meters`).
            if let Some((prev, _)) = self.last.filter(|_| !self.meters_shown) {
                status.hold_meters(&prev);
            }
            self.meters_shown = false;
            if status.stopped {
                self.stopped = true;
            }
            // The instrument timing, for the log: `C` once it is known, and late notes when they
            // happen - a late note sounds (and is recorded) up to a block off its beat.
            if status.instrument_delay != 0 && self.logged_delay != Some(status.instrument_delay) {
                self.logged_delay = Some(status.instrument_delay);
                let rate = f64::from(self.info.sample_rate.max(1));
                log!(
                    "Instrument delay C = {} frames ({:.1} ms): output latency plus one block.",
                    status.instrument_delay,
                    f64::from(status.instrument_delay) * 1000.0 / rate
                );
            }
            if status.late_events > self.logged_late {
                log!(
                    "{} MIDI event(s) reached the engine too late for their fixed position ({} \
                     in total).",
                    status.late_events - self.logged_late,
                    status.late_events
                );
                self.logged_late = status.late_events;
            }
            // A refusal is only news when the counter moved or the reason changed.
            if status.ignored_commands > self.last_refusal.0 || status.refusal != self.last_refusal.1
            {
                if let Some(text) = status.refusal.message() {
                    self.message = Some(text.to_string());
                }
                self.last_refusal = (status.ignored_commands, status.refusal);
            }
            // Keep the gain mirror the same length the engine reports.
            for (i, ui) in self.tracks.iter_mut().enumerate() {
                let layers = status
                    .tracks()
                    .get(i)
                    .map(|t| t.layers as usize)
                    .unwrap_or(0);
                while ui.gains.len() < layers {
                    ui.gains.push(1.0);
                }
                ui.gains.truncate(layers);
            }
            self.last = Some((status, Instant::now()));
        }

        // The runner's turn of the control loop. It is driven by the position of the newest
        // snapshot, not by an estimate: this is where an armed change *becomes* the sounding
        // section and where the one untimed command - monitoring - is sent, so it must not run
        // ahead of the audio thread.
        if let Some((status, _)) = self.last
            && self.runner.is_some()
        {
            let mut section = None;
            let notices = match self.runner.as_mut() {
                Some(runner) => {
                    self.section_watch.before(runner);
                    runner.tick(status.pos);
                    section = self.section_watch.after(runner, status.pos);
                    runner.take_notices()
                }
                None => Vec::new(),
            };
            if let Some(event) = section {
                self.recorder.section(&event);
            }
            self.pump_runner();
            // What the runner said on its own - a section began, the score ended. No reply carries
            // it, so it goes out as the status message and as an event of its own (the status
            // message only shows the newest sentence, and two equal sentences in a row would look
            // like one).
            for text in notices {
                log!("Score: {text}");
                let _ = app.emit(NOTICE_EVENT, NoticeEvent::score(&text));
                self.message = Some(text);
            }
        }

        // A clear has happened: the score's track setup goes back on.
        if let (Some(at), Some((status, _))) = (self.reapply_at, self.last)
            && status.pos > at
        {
            self.reapply_at = None;
            if let Some(score) = self.runner.as_ref().map(|r| r.score().clone())
                && let Err(e) = self.apply_score_setup(&score)
            {
                self.message = Some(e);
            }
        }

        self.drain_mix();
        if self
            .free_mix_since
            .is_some_and(|since| since.elapsed() >= MIX_SAVE_DELAY)
        {
            self.save_free_mix(false);
        }

        // The only place layer buffers are allocated, zeroed and dropped.
        self.pool.service(self.last.as_ref().map(|(s, _)| s));

        let takes = self.last.is_some_and(|(s, _)| {
            s.tracks()
                .iter()
                .any(|t| matches!(t.state, TrackState::Recording | TrackState::Overdub))
        });
        let dropouts = self.dropouts();
        self.recorder.service(takes, dropouts, app);

        if self.last_emit.elapsed() >= EMIT_INTERVAL
            && let Some((status, _)) = self.last
        {
            self.last_emit = Instant::now();
            self.meters_shown = true;
            let _ = app.emit(STATUS_EVENT, self.event(&status));
        }
    }

    fn event(&self, status: &Status) -> StatusEvent {
        let rate = self.info.sample_rate.max(1);
        let stats = &self.stats;
        // Whether anything on screen may count. With a score loaded the engine's clock runs the
        // whole time (the runner plans on it), but before the start and after "Stop all" nothing
        // is playing, and a display that counts bars of nothing is only a distraction. Display
        // only: the runner and the engine never see this.
        let transport = status.transport && (self.runner.is_none() || self.score_is_playing());
        let (bar, beat, beat_offset) = if transport {
            (status.bar + 1, status.beat + 1, status.beat_offset)
        } else {
            (1, 1, 0)
        };
        StatusEvent {
            running: true,
            transport,
            pos: status.pos,
            seconds: status.pos as f64 / rate as f64,
            bar,
            beat,
            beat_offset,
            samples_per_beat: status.samples_per_beat,
            bpm: status.bpm,
            beats_per_bar: self.scheduler.timeline.signature().beats_per_bar,
            beat_unit: self.scheduler.timeline.signature().beat_unit,
            bars: self.scheduler.bars,
            loop_samples: self.scheduler.timeline.span_bars(0, self.scheduler.bars),
            loop_seconds: self
                .scheduler
                .timeline
                .samples_to_secs(self.scheduler.timeline.span_bars(0, self.scheduler.bars)),
            quantize: QuantizeName::from(self.scheduler.quantize),
            overdub_mode: self.config.overdub_mode,
            sample_rate: rate,
            latency_frames: self.info.latency_frames,
            click: status.click,
            click_gain: status.click_gain,
            monitor_gain: status.monitor_gain,
            output_peak: status.output_peak_max(),
            output_dbfs: dbfs(status.output_peak_max()),
            output_peaks: status.output_peak().to_vec(),
            buses: bus_events(status),
            output_channels: u32::from(status.output_channels),
            buses_collapsed: status.bus_out.collapsed(),
            bus_note: routing_note(&status.bus_out, status.output_channels as usize),
            tracks: self
                .tracks
                .iter()
                .enumerate()
                .map(|(i, ui)| {
                    let ts = status.tracks().get(i).copied().unwrap_or_default();
                    // The count-in is measured against the position of this very snapshot, so the
                    // number on screen and the beat that is sounding belong together.
                    let lead = self.scheduler.lead(i, status.pos);
                    let mut event = track_event(i, &ui.name, &ts, &ui.gains, rate, lead);
                    let timeline = &self.scheduler.timeline;
                    let bar = timeline.samples_per_beat() * timeline.beats_per_bar() as f64;
                    if bar > 0.0 {
                        let into = if ts.loop_len > 0 && !status.transport {
                            // A standing loop stands at its top - which is where a start puts it.
                            Some(0)
                        } else if ts.loop_len > 0 {
                            Some(status.pos.saturating_sub(ts.origin) % ts.loop_len)
                        } else if ts.filled > 0 {
                            Some(ts.filled)
                        } else {
                            None
                        };
                        // A hair of slack: the loop grid and the bar grid differ by a sample or two.
                        event.take_bar = into.map_or(0, |f| ((f as f64 + 2.0) / bar) as u32 + 1);
                        event.loop_bars = (ts.loop_len as f64 / bar).round() as u32;
                    }
                    event
                })
                .collect(),
            xruns: stats.xruns.load(Ordering::Relaxed),
            fifo_underruns: stats.underruns.load(Ordering::Relaxed),
            fifo_overruns: stats.overruns.load(Ordering::Relaxed),
            other_errors: stats.other_errors.load(Ordering::Relaxed),
            max_callback_ms: stats.cb_nanos_max.load(Ordering::Relaxed) as f64 / 1e6,
            score: self.runner.as_ref().map(|runner| {
                let names: Vec<String> = self.tracks.iter().map(|ui| ui.name.clone()).collect();
                score_event(&runner.score().title, &runner.view(status.pos), &names)
            }),
            ignored_commands: status.ignored_commands,
            spares: status.spares_total(),
            message: self.message.clone(),
            mixer_rev: self.mix_rev,
        }
    }

    // ---- the score -----------------------------------------------------------------------
    //
    // The runner is the whole of phase 3 and it lives in the engine library; everything below is
    // plumbing. The one rule that is decided *here* rather than there is what a score may be loaded
    // onto, and it is strict on purpose - see `load_score`.

    /// Hand the runner's freshly produced commands to the engine, oldest first.
    fn pump_runner(&mut self) {
        let Some(runner) = self.runner.as_mut() else {
            return;
        };
        for command in runner.take_commands() {
            if let Err(e) = self.cmd.send(command) {
                self.message = Some(e);
                return;
            }
        }
    }

    /// True while the score owns the transport: it has been started and is not over.
    fn score_is_playing(&self) -> bool {
        self.runner
            .as_ref()
            .is_some_and(|r| matches!(r.phase(), Phase::CountIn | Phase::Running))
    }

    /// Compile a score and load it onto this session.
    ///
    /// **Only onto an empty session, and that is not tidiness.** The runner starts from the belief
    /// that every track is silent and holds nothing (`TrackRun` in
    /// `looper_engine::engine::runner`), and it plans overdubs against a loop geometry it predicts
    /// from the takes it issued itself. A loop somebody recorded by hand beforehand is invisible to
    /// that prediction, so the first overdub of the score would be quantised to a grid that does
    /// not exist. Refusing is one sentence; the alternative is a layer that sits a few frames off
    /// and is only noticed on the recording.
    ///
    /// The musical grid comes from the score, not from the setup screen: tempo, time signature and
    /// the buffer length every layer is allocated at (the longest section). That is the same thing
    /// the `score` subcommand does by building the engine out of the score - here the device stays
    /// open and only the grid moves, which the engine allows precisely because nothing is recorded.
    fn load_score(&mut self, yaml: &str, count_in: Option<u32>) -> Result<ScoreLoaded, String> {
        self.score_yaml = Some(yaml.to_string());
        if self.score_is_playing() {
            return Err(
                "A score is already running. Press \"Stop all\" first - switching in the middle of a \
                 run would leave commands armed in the audio thread that nobody can call back."
                    .to_string(),
            );
        }
        let score = compile_score(yaml).map_err(|error| render_issues(&error))?;

        // The layout check comes from the engine library and carries its own instructions.
        check_tracks(&score, &self.defs)?;

        if let Some(busy) = self.first_non_empty_track() {
            return Err(format!(
                "Track \"{busy}\" is not empty. A score only loads onto an empty session - the \
                 runner plans every overdub against the loops of the takes it sent itself, and it \
                 cannot see a loop recorded by hand. Press \"Clear all\" first, then load."
            ));
        }

        let bars = score.sections.iter().map(|s| s.bars).max().unwrap_or(1).max(1);
        let signature = score.signature();
        // The score's own compensation per track counts for the loop-length check, so it goes into
        // the mirror before the grid moves - and back out if the grid refuses.
        let previous: Vec<TrackLatency> = self.tracks.iter().map(|ui| ui.latency).collect();
        for (ui, spec) in self.tracks.iter_mut().zip(&score.tracks) {
            ui.latency = spec.track_latency();
        }
        if let Err(e) =
            self.reconfigure(score.bpm, signature.beats_per_bar, signature.beat_unit, bars)
        {
            for (ui, latency) in self.tracks.iter_mut().zip(previous) {
                ui.latency = latency;
            }
            return Err(e);
        }

        // Everything the score says about its tracks: pan, both bus assignments, latency - and
        // monitoring off. Monitoring belongs to the runner from now on (`hear_through` and the
        // takes switch it, `monitor:` in the score says which tracks may have it at all), and its
        // picture starts at "everything off"; a track started with monitoring on would otherwise
        // stay on for the whole score, because the runner would never see a difference to send.
        self.apply_score_setup(&score)?;
        // The score's mix, for the tracks it names. What it leaves out keeps what it has.
        if let Some(mixer) = &score.mixer {
            self.queue_mix(mixer);
        }

        let title = score.title.clone();
        let sections = score.sections.len();
        // The runner owns the clock from here on and plans every bar on the grid it is handed, so
        // that grid has to be running - and has to be the one handed over. A free-looping stop
        // must never hold a score's count-in, so the transport starts now and stays running for
        // as long as the score is loaded; the display stands still on its own while nothing plays
        // (`Session::event`).
        if !self.transport {
            let at = self.scheduler.soon(self.estimated_pos());
            self.start_transport_at(at)?;
        }
        let runner = Runner::new(score.clone(), self.scheduler.timeline, self.info.buffer_frames)?
            .with_count_in(count_in.unwrap_or(DEFAULT_COUNT_IN_BARS));
        // What the runner really uses: a requested 0 is one bar, and the sentence says so.
        let count_in = runner.count_in_bars();
        // A free-play recording ends here: from now on the score's runs are recorded.
        if self.recorder.mode() == Some(RecordMode::Free) {
            self.recorder.end(self.estimated_pos());
        }
        self.runner = Some(runner);

        let message = format!(
            "Score \"{title}\" loaded: {sections} sections, {:.1} BPM, {}, longest section \
             {bars} bars, {count_in}-bar count-in.",
            score.bpm,
            score.time_signature,
        );
        self.message = Some(message.clone());
        Ok(ScoreLoaded {
            score,
            message,
            engine: None,
        })
    }

    /// One press of the score transport. The answer is always a sentence - see
    /// [`ScoreAction`].
    fn score_act(&mut self, action: ScoreAction) -> Result<String, String> {
        let est = self.estimated_pos();
        let Some(runner) = self.runner.as_mut() else {
            return Err(
                "No score is loaded. Load one in the Arrangement first.".to_string(),
            );
        };
        let message = match action {
            ScoreAction::Start => {
                let message = runner.start(est);
                if runner.phase() == Phase::CountIn
                    && let Some((at, at_bar)) = runner.armed_at()
                {
                    let title = runner.score().title.clone();
                    let count_in = runner.count_in_bars();
                    self.begin_recording(RecordMode::Score, &title, at, at_bar, count_in);
                }
                message
            }
            ScoreAction::Next => runner.next(est),
            ScoreAction::Goto(index) => runner.goto(index, est),
            ScoreAction::StopAll => {
                let message = runner.stop_all(est);
                self.recorder.end(est);
                message
            }
            ScoreAction::CountIn(bars) => runner.set_count_in(bars),
            ScoreAction::Rewind => {
                let (message, commands) = runner.rewind(est)?;
                for command in commands {
                    if let Command::ClearAll { at } = command {
                        self.reapply_at = Some(at);
                    }
                    self.cmd.send(command)?;
                }
                for ui in self.tracks.iter_mut() {
                    ui.gains.clear();
                }
                message
            }
        };
        self.pump_runner();
        self.message = Some(message.clone());
        Ok(message)
    }

    /// Put every track into the setup the score describes. Untimed commands, only ever sent while
    /// the tracks are empty and no score plays; the mirrors on this side follow along so a tempo
    /// check and `EngineInfo` see the same numbers the engine has.
    fn apply_score_setup(&mut self, score: &CompiledScore) -> Result<(), String> {
        for command in setup_commands(score) {
            self.cmd.send(command)?;
        }
        for (i, spec) in score.tracks.iter().enumerate() {
            if let Some(ui) = self.tracks.get_mut(i) {
                ui.latency = spec.track_latency();
            }
            if let Some(info) = self.info.tracks.get_mut(i) {
                info.pan = spec.pan;
                info.latency_frames = spec.latency;
                info.latency_trim = spec.latency_trim;
                info.bus = Some(spec.bus.clone());
                info.monitor_bus = Some(spec.monitor_bus.clone());
            }
        }
        Ok(())
    }

    /// The name of the first track that holds something, for the refusal above.
    fn first_non_empty_track(&self) -> Option<String> {
        let (status, _) = self.last?;
        status
            .tracks()
            .iter()
            .take(self.tracks.len())
            .position(|t| t.state != TrackState::Empty)
            .map(|i| self.tracks[i].name.clone())
    }

    /// The track list a MIDI binding's address is resolved against. `track.1` is the first entry,
    /// `track.stimme` the one with that name - the same two spellings the profile and the score use.
    fn layout(&self) -> TrackLayout {
        TrackLayout::new(self.tracks.iter().map(|ui| ui.name.clone()))
    }

    fn status_of(&self, track: usize) -> TrackStatus {
        self.last
            .and_then(|(s, _)| s.tracks().get(track).copied())
            .unwrap_or_default()
    }

    fn estimated_pos(&self) -> u64 {
        self.scheduler
            .estimated_pos(self.last.map(|(s, seen)| (s.pos, seen.elapsed())))
    }

    /// Check a track index against the engine's track list.
    fn check_track(&self, track: usize) -> Result<(), String> {
        if track >= self.tracks.len() {
            return Err(format!(
                "{} You meant track {}; there are tracks 1 to {}.",
                Refusal::NoSuchTarget
                    .message()
                    .unwrap_or("Unknown target."),
                track + 1,
                self.tracks.len()
            ));
        }
        Ok(())
    }

    /// Check a layer index against what the engine says exists on that track.
    fn check_layer(&self, track: usize, layer: usize) -> Result<(), String> {
        let ts = self.status_of(track);
        if layer >= ts.layers as usize {
            let name = &self.tracks[track].name;
            return Err(if ts.layers == 0 {
                format!(
                    "{} \"{name}\" has no layers.",
                    Refusal::NoSuchTarget
                        .message()
                        .unwrap_or("Unknown target.")
                )
            } else {
                format!(
                    "{} \"{name}\" has layers 1 to {}.",
                    Refusal::NoSuchTarget
                        .message()
                        .unwrap_or("Unknown target."),
                    ts.layers
                )
            });
        }
        // Layers cannot be edited while that track records - the engine refuses it too, this only
        // says so immediately instead of one status snapshot later.
        if matches!(ts.state, TrackState::Recording | TrackState::Overdub) {
            return Err(Refusal::Busy
                .message()
                .unwrap_or("The track is busy.")
                .to_string());
        }
        Ok(())
    }

    /// Start the transport at `at`: bar 1 is there, on both sides of the queue. The engine moves
    /// its grid when it reaches `at`; this side moves its mirror now, so every take scheduled from
    /// here on is planned on the grid the engine will have by then. Both use the very same number,
    /// so they agree even when the command arrives late and acts after `at`.
    fn start_transport_at(&mut self, at: u64) -> Result<(), String> {
        self.cmd.send(Command::TransportStart { at })?;
        self.scheduler.timeline = self.scheduler.timeline.with_zero(at);
        self.transport = true;
        Ok(())
    }

    /// Start/Stop of free looping. With a score loaded the score's own Start / Stop all are the
    /// transport, and this one is refused rather than allowed to hold a running count-in.
    fn set_transport(&mut self, run: Option<bool>) -> Result<(), String> {
        if self.runner.is_some() {
            return Err(
                "A score is loaded - its transport is Start and Stop all in the Arrangement."
                    .to_string(),
            );
        }
        let run = run.unwrap_or(!self.transport);
        if run == self.transport {
            return Ok(());
        }
        if run {
            let at = self.scheduler.soon(self.estimated_pos());
            self.start_transport_at(at)?;
            self.begin_recording(RecordMode::Free, "Free play", at, 0, 0);
            self.message = Some("Transport started at bar 1.".to_string());
        } else {
            self.recorder.end(self.estimated_pos());
            self.cmd.send(Command::TransportStop)?;
            self.transport = false;
            self.scheduler.disarm_all();
            self.message = Some(
                "Transport stopped. The loops are kept; Start plays them from bar 1.".to_string(),
            );
        }
        Ok(())
    }

    /// Record or overdub pressed while the clock stands: start it right away and let the take
    /// begin on that very bar 1 rather than a loop later. The start is sent first, so among the
    /// two commands stamped for the same sample it acts first.
    fn start_for_take(&mut self) -> Result<(), String> {
        if self.transport || self.runner.is_some() {
            return Ok(());
        }
        let at = self.scheduler.soon(self.estimated_pos());
        self.start_transport_at(at)?;
        self.begin_recording(RecordMode::Free, "Free play", at, 0, 0);
        Ok(())
    }

    /// Hand the recorder a new run with frame 0 at `at` (bar index `at_bar` on the current grid).
    /// A recording that cannot start is said in the status line; the music goes on regardless.
    fn begin_recording(&mut self, mode: RecordMode, title: &str, at: u64, at_bar: u64, count_in: u32) {
        let default = self.info.latency_frames;
        let tracks = crate::recording::run_tracks(
            self.tracks
                .iter()
                .map(|ui| (ui.name.clone(), ui.channels, ui.latency.resolve(default))),
        );
        let ctx = crate::recording::Context {
            timeline: &self.scheduler.timeline,
            tracks,
            now: self.estimated_pos(),
            sample_rate: self.info.sample_rate,
        };
        let yaml = match mode {
            RecordMode::Score => self.score_yaml.clone(),
            RecordMode::Free => None,
        };
        let dropouts = self.dropouts();
        if let Some(text) = self
            .recorder
            .begin(ctx, mode, title, at, at_bar, count_in, yaml, dropouts)
        {
            self.message = Some(text);
        }
    }

    /// End the recording now, whatever started it - the recorder's own stop button.
    fn stop_recording(&mut self) -> Result<(), String> {
        if !self.recorder.active() {
            return Err("Nothing is being recorded to disk.".to_string());
        }
        self.recorder.end(self.estimated_pos());
        self.message = Some("Recording to disk stopped.".to_string());
        Ok(())
    }

    /// Everything the audio callbacks counted as lost or late, added up.
    fn dropouts(&self) -> u64 {
        let s = &self.stats;
        s.xruns.load(Ordering::Relaxed)
            + s.other_errors.load(Ordering::Relaxed)
            + s.underruns.load(Ordering::Relaxed)
            + s.overruns.load(Ordering::Relaxed)
    }

    /// One overdub pass, planned on the track's own grid (and the clock started if it stands).
    fn overdub_plan(&mut self, track: usize, est: u64) -> Result<Scheduled, String> {
        let mut ts = self.status_of(track);
        let name = self.tracks[track].name.clone();
        if !self.transport && self.runner.is_none() {
            self.start_for_take()?;
            // The engine re-anchors every loop on the new bar 1; plan on that grid.
            if ts.loop_len > 0 {
                ts.origin = self.scheduler.timeline.zero();
            }
        }
        // `origin` and `loop_len` are this track's own grid; a further layer has to start on it,
        // not on the global one. See looper_engine::engine::schedule.
        Ok(self
            .scheduler
            .overdub(track, &name, est, ts.origin, ts.loop_len, ts.layers))
    }

    /// `O` in free looping with the continuous overdub (`OverdubMode::Latch`). One button, read
    /// against what the track is doing - see `looper_engine::engine::latch` for the engine side:
    ///
    /// * **idle** (empty, stopped, playing): arm an overdub as before and latch it - every pass from
    ///   then on is a layer of its own. On an empty track the first pass defines the loop.
    /// * **recording or armed**: toggle "overdub next" - the take runs straight on into a
    ///   continuous overdub instead of into playback.
    /// * **overdubbing, latched**: end it. The running pass is finished to the loop end, so a press
    ///   anywhere in the last pass keeps a complete layer and the phrase is never cut. A press in
    ///   the first beat of a pass counts as a late press for the boundary just gone: that pass ends
    ///   right away (and is dropped if nothing was played in it).
    /// * **overdubbing, not latched** (a last pass running out): latch it again.
    fn latched_overdub(&mut self, track: usize, est: u64) -> Result<(), String> {
        let ts = self.status_of(track);
        let name = self.tracks[track].name.clone();
        let layer = ts.layers.max(1);
        match ts.state {
            TrackState::Recording | TrackState::Armed => {
                let on = !ts.latched;
                self.cmd.send(Command::LatchOverdub { track, on })?;
                self.message = Some(if on {
                    format!("\"{name}\": overdub next - the take runs straight on into overdub.")
                } else {
                    format!("\"{name}\": overdub next cancelled.")
                });
                Ok(())
            }
            TrackState::Overdub if ts.latched => {
                self.cmd.send(Command::LatchOverdub { track, on: false })?;
                let beat = self.scheduler.timeline.samples_per_beat().round() as u64;
                let into = match ts.loop_len {
                    0 => u64::MAX,
                    len => est.saturating_sub(ts.origin) % len,
                };
                if into < beat {
                    let at = self.scheduler.soon(est);
                    self.cmd.send(Command::StopRecord { track, at })?;
                    self.cmd.send(Command::StartPlay { track, at })?;
                    self.message = Some(format!("\"{name}\": overdub off."));
                } else {
                    self.message = Some(format!(
                        "\"{name}\": overdub ends at the end of this pass (layer {layer})."
                    ));
                }
                Ok(())
            }
            TrackState::Overdub => {
                self.cmd.send(Command::LatchOverdub { track, on: true })?;
                self.message = Some(format!("\"{name}\": overdub stays on."));
                Ok(())
            }
            TrackState::Empty | TrackState::Ready | TrackState::Playing => {
                let plan = self.overdub_plan(track, est)?;
                self.cmd.send(Command::LatchOverdub { track, on: true })?;
                self.send_all(Scheduled {
                    message: format!(
                        "{} Stays on until O is pressed again.",
                        plan.message
                    ),
                    ..plan
                })
            }
        }
    }

    fn send_all(&mut self, plan: Scheduled) -> Result<(), String> {
        for command in plan.commands {
            self.cmd.send(command)?;
        }
        self.message = Some(plan.message);
        Ok(())
    }

    fn apply(&mut self, action: Action) -> Result<(), String> {
        let mix_track = match action {
            Action::SetTrackGain { track, .. }
            | Action::SetTrackMute { track, .. }
            | Action::FxBypass { track, .. }
            | Action::FxEnable { track, .. }
            | Action::FxParam { track, .. }
            | Action::FxPreset { track, .. } => Some(Some(track)),
            Action::SetBusGain { .. } | Action::SetClickGain { .. } | Action::SetMonitorGain { .. } => {
                Some(None)
            }
            _ => None,
        };
        let result = self.apply_action(action);
        if result.is_ok()
            && let Some(track) = mix_track
        {
            self.mix_touched(track);
        }
        result
    }

    fn apply_action(&mut self, action: Action) -> Result<(), String> {
        if let Some(track) = action.track() {
            self.check_track(track)?;
        }
        if action.moves_transport() && self.score_is_playing() {
            // The sentence is the engine's, not this file's: a MIDI pad runs into the same wall
            // and has to say the same thing. See `runner::TRANSPORT_BELONGS_TO_SCORE`.
            return Err(TRANSPORT_BELONGS_TO_SCORE.to_string());
        }
        let est = self.estimated_pos();

        match action {
            Action::Transport { run } => self.set_transport(run),
            Action::Panic { track } => {
                let msg = match track {
                    Some(track) => FeedMsg::Panic { track },
                    None => FeedMsg::PanicAll,
                };
                self.feeds.send(msg)
            }
            Action::Record { track } => {
                let name = self.tracks[track].name.clone();
                // A new loop is "another action of the track": a continuous overdub ends with it.
                if self.status_of(track).latched {
                    self.cmd.send(Command::LatchOverdub { track, on: false })?;
                }
                // With the clock standing, `soon(est)` is the new bar 1 and therefore the first
                // point of every grid - `record` lands exactly on it.
                self.start_for_take()?;
                let plan = self.scheduler.record(track, &name, est);
                self.tracks[track].gains.clear();
                self.send_all(plan)
            }
            Action::Overdub { track }
                if self.config.overdub_mode == OverdubMode::Latch && self.runner.is_none() =>
            {
                self.latched_overdub(track, est)
            }
            Action::Overdub { track } => {
                let plan = self.overdub_plan(track, est)?;
                self.send_all(plan)
            }
            Action::StopTrack { track } => {
                let ts = self.status_of(track);
                if ts.latched {
                    self.cmd.send(Command::LatchOverdub { track, on: false })?;
                }
                let name = self.tracks[track].name.clone();
                let plan = self.scheduler.stop(track, &name, est, ts.state);
                self.send_all(plan)
            }
            Action::Play { track } => {
                let name = self.tracks[track].name.clone();
                let ts = self.status_of(track);
                if ts.latched {
                    self.cmd.send(Command::LatchOverdub { track, on: false })?;
                }
                // Play during a continuous overdub means "enough layers, just play": the take ends
                // on the next bar line, exactly like Stop ends it.
                if ts.state == TrackState::Overdub
                    && self.config.overdub_mode == OverdubMode::Latch
                    && self.runner.is_none()
                {
                    let plan = self.scheduler.stop(track, &name, est, ts.state);
                    return self.send_all(Scheduled {
                        message: format!("\"{name}\": overdub ends at the next bar line, then plays."),
                        ..plan
                    });
                }
                if !self.transport {
                    // No bar to wait for: the track is on play at once and is heard from bar 1
                    // when the transport starts.
                    let plan = self.scheduler.play_at(track, &name, self.scheduler.soon(est));
                    return self.send_all(Scheduled {
                        message: format!("\"{name}\": Play - heard when the transport starts."),
                        ..plan
                    });
                }
                let plan = self.scheduler.play(track, &name, est);
                self.send_all(plan)
            }
            Action::ClearTrack { track } => {
                let name = self.tracks[track].name.clone();
                let plan = self.scheduler.clear_track(track, &name, est);
                self.tracks[track].gains.clear();
                self.send_all(plan)
            }
            Action::ClearAll => {
                // Free looping: "clear all" is a fresh start, so the clock goes back to bar 1 and
                // stands. Stopped first, because a transport stop drops every timed command sent
                // before it - the clear has to come after.
                let restart = self.runner.is_none() && self.transport;
                if restart {
                    self.set_transport(Some(false))?;
                }
                let est = self.estimated_pos();
                let plan = self.scheduler.clear_all(est);
                for ui in self.tracks.iter_mut() {
                    ui.gains.clear();
                }
                // With a score loaded, "like freshly started" means: as the score sets it up.
                if self.runner.is_some()
                    && let Some(Command::ClearAll { at }) = plan.commands.first()
                {
                    self.reapply_at = Some(*at);
                }
                self.send_all(plan)?;
                if restart {
                    self.message = Some(
                        "All cleared - the clock stands at bar 1; tempo and loop length can be changed."
                            .to_string(),
                    );
                }
                Ok(())
            }
            Action::SetMonitor { track, on } => {
                self.cmd.send(Command::SetMonitor { track, on })?;
                self.message = Some(format!(
                    "\"{}\": Monitor {}.",
                    self.tracks[track].name,
                    if on { "on" } else { "off" }
                ));
                Ok(())
            }
            Action::SetPan { track, pan } => {
                if !(-1.0..=1.0).contains(&pan) {
                    return Err(
                        "Pan must be between -1 (hard left) and 1 (hard right)."
                            .to_string(),
                    );
                }
                self.cmd.send(Command::SetPan { track, pan })?;
                // No message: a knob being turned is not news, and a sentence per mouse move would
                // make the status line flicker. The new value comes back in the status.
                Ok(())
            }
            // The channel strip. Like the pan, no message per fader move; mute and solo are presses
            // and say what they did. None of the three is reset by a score load or "clear all":
            // they are the live balance, not part of the setup (see `Track::restore_defaults`).
            Action::SetTrackGain { track, gain } => {
                if !gain.is_finite() || !(0.0..=MAX_TRACK_GAIN).contains(&gain) {
                    return Err(format!(
                        "The channel fader goes from 0.0 (silence) to {MAX_TRACK_GAIN:.2} (+12 dB)."
                    ));
                }
                self.cmd.send(Command::SetTrackGain { track, gain })
            }
            Action::SetTrackMute { track, on } => {
                self.cmd.send(Command::SetTrackMute { track, on })?;
                self.message = Some(format!(
                    "\"{}\": {}.",
                    self.tracks[track].name,
                    if on { "muted" } else { "unmuted" }
                ));
                Ok(())
            }
            Action::SetTrackSolo { track, on } => {
                self.cmd.send(Command::SetTrackSolo { track, on })?;
                self.message = Some(format!(
                    "\"{}\": solo {}.",
                    self.tracks[track].name,
                    if on { "on" } else { "off" }
                ));
                Ok(())
            }
            // The three bus actions stay available while a score runs: they are mix decisions, and
            // nothing they touch can move a take (see `Action::moves_transport`).
            Action::SetTrackSend {
                track,
                source,
                send,
            } => {
                self.cmd.send(Command::SetTrackSend {
                    track,
                    source,
                    send,
                })?;
                self.message = Some(format!(
                    "\"{}\": {} -> {}.",
                    self.tracks[track].name,
                    source.label(),
                    send.label()
                ));
                Ok(())
            }
            Action::SetTrackBus {
                track,
                source,
                bus,
                on,
            } => {
                let current = match source {
                    TrackSource::Loop => self.status_of(track).loop_send,
                    TrackSource::Monitor => self.status_of(track).monitor_send,
                };
                self.apply(Action::SetTrackSend {
                    track,
                    source,
                    send: current.with(bus, on),
                })
            }
            Action::SetBusGain { bus, gain } => {
                if !(0.0..=MAX_BUS_GAIN).contains(&gain) {
                    return Err(format!(
                        "The bus level must be between 0.0 and {MAX_BUS_GAIN}."
                    ));
                }
                self.cmd.send(Command::SetBusGain { bus, gain })?;
                // No message, for the same reason a pan produces none: a fader being ridden is not
                // news, and the new value comes back in the status anyway.
                Ok(())
            }
            Action::SetBusOutput { bus, out } => {
                if out.width == 0 || out.width > 2 {
                    return Err("A bus sits on one channel or on a pair.".to_string());
                }
                self.cmd.send(Command::SetBusOutput { bus, out })?;
                self.message = Some(format!(
                    "{} is on output {}.{}",
                    bus.label(),
                    out.label(),
                    if out.first + out.width > self.output_channels {
                        " The device does not have that many outputs - the bus lands on the nearest pair \
                         that exists."
                    } else {
                        ""
                    }
                ));
                Ok(())
            }
            Action::SetTrackLatency { track, latency } => self.set_track_latency(track, latency),
            Action::SetClick { on } => {
                self.cmd.send(Command::SetClick { on })?;
                self.message = Some(format!("Click {}.", if on { "on" } else { "off" }));
                Ok(())
            }
            // Levels, like a bus fader: no message per mouse move, the value comes back in the
            // status.
            Action::SetClickGain { gain } => {
                check_level("Click", gain)?;
                self.cmd.send(Command::SetClickGain { gain })
            }
            Action::SetMonitorGain { gain } => {
                check_level("Monitoring", gain)?;
                self.cmd.send(Command::SetMonitorGain { gain })
            }
            Action::LayerMute {
                track,
                layer,
                muted,
            } => {
                self.check_layer(track, layer)?;
                self.cmd.send(Command::SetLayerMute {
                    track,
                    layer,
                    muted,
                })?;
                self.message = Some(format!(
                    "\"{}\": layer {} {}.",
                    self.tracks[track].name,
                    layer + 1,
                    if muted { "muted" } else { "audible again" }
                ));
                Ok(())
            }
            Action::LayerRemove { track, layer } => {
                self.check_layer(track, layer)?;
                self.cmd.send(Command::RemoveLayer { track, layer })?;
                if layer < self.tracks[track].gains.len() {
                    self.tracks[track].gains.remove(layer);
                }
                self.message = Some(format!(
                    "\"{}\": layer {} removed.",
                    self.tracks[track].name,
                    layer + 1
                ));
                Ok(())
            }
            Action::LayerGain { track, layer, gain } => {
                self.check_layer(track, layer)?;
                if !(0.0..=4.0).contains(&gain) {
                    return Err("The level must be between 0.0 and 4.0.".to_string());
                }
                self.cmd.send(Command::SetLayerGain { track, layer, gain })?;
                let gains = &mut self.tracks[track].gains;
                while gains.len() <= layer {
                    gains.push(1.0);
                }
                gains[layer] = gain;
                self.message = Some(format!(
                    "\"{}\": layer {} at {gain:.2}.",
                    self.tracks[track].name,
                    layer + 1
                ));
                Ok(())
            }
            Action::SetTempo {
                bpm,
                beats_per_bar,
                beat_unit,
                bars,
            } => {
                // The runner schedules on the timeline it was loaded with; moving the engine's grid
                // under a loaded score would put every later section on the wrong samples.
                if self.runner.is_some() {
                    return Err(
                        "A score is loaded, and its tempo and time signature come from the score. \
                         Change bpm / time_signature there and load it again."
                            .to_string(),
                    );
                }
                self.set_tempo(bpm, beats_per_bar, beat_unit, bars)
            }
            Action::TapTempo => {
                if self.runner.is_some() {
                    return Err("A score is loaded, and its tempo comes from the score.".to_string());
                }
                match self.taps.tap(Instant::now()) {
                    Some(bpm) => {
                        let sig = self.scheduler.timeline.signature();
                        self.set_tempo(bpm, sig.beats_per_bar, sig.beat_unit, None)
                    }
                    None => {
                        self.message = Some("Tap tempo: keep tapping…".to_string());
                        Ok(())
                    }
                }
            }
            Action::SetQuantize { quantize } => {
                self.message = Some(self.scheduler.set_quantize(quantize));
                Ok(())
            }
            Action::SetOverdubMode { mode } => {
                self.config.overdub_mode = mode;
                self.message = Some(
                    match mode {
                        OverdubMode::Latch => "Overdub stays on until pressed again.",
                        OverdubMode::Once => "Overdub records one pass.",
                    }
                    .to_string(),
                );
                Ok(())
            }
            Action::FxBypass { track, on } => {
                self.cmd.send(Command::SetFxBypass { track, on })?;
                self.message = Some(format!(
                    "\"{}\": effect chain {}.",
                    self.tracks[track].name,
                    if on { "bypassed" } else { "active" }
                ));
                Ok(())
            }
            Action::FxEnable { track, slot, on } => {
                self.cmd.send(Command::SetFxEnabled { track, slot, on })?;
                self.message = Some(format!(
                    "\"{}\": {} {}.",
                    self.tracks[track].name,
                    slot.label(),
                    if on { "on" } else { "off" }
                ));
                Ok(())
            }
            Action::FxParam { track, param } => {
                self.cmd.send(Command::SetFxParam { track, param })?;
                // No message: a knob being turned is not news, and a sentence per mouse move
                // would make the status line flicker. The new value comes back in the status.
                Ok(())
            }
            Action::FxPreset { track, preset } => {
                if preset == FxPreset::Custom {
                    return Err(
                        "\"custom\" is not a preset to load - it is what a chain is called once a knob \
                         was turned by hand."
                            .to_string(),
                    );
                }
                self.cmd.send(Command::LoadFxPreset { track, preset })?;
                self.message = Some(format!(
                    "\"{}\": preset \"{}\" loaded.",
                    self.tracks[track].name,
                    preset.label()
                ));
                Ok(())
            }
        }
    }

    // ---- the mix ---------------------------------------------------------------------------
    //
    // Fader, mute and effect chain of every track and the bus levels survive what used to reset
    // them: a score keeps them in its `mixer:` section, the free mode in `mixer.json`
    // (`crate::mixstore`). See `looper_engine::score::mixer` for the model.

    /// A human changed the mix of `track` (or the bus levels, `None`).
    fn mix_touched(&mut self, track: Option<usize>) {
        self.mix_rev += 1;
        match track.and_then(|t| self.tracks.get(t)).map(|ui| ui.name.clone()) {
            Some(name) => {
                if !self.mix_changed.contains(&name) {
                    self.mix_changed.push(name.clone());
                }
                // The free mode keeps tracks on disk; with a score loaded, the score does.
                if self.runner.is_none() {
                    if !self.free_mix_dirty.contains(&name) {
                        self.free_mix_dirty.push(name);
                    }
                    self.free_mix_since = Some(Instant::now());
                }
            }
            None => self.mix_buses_changed = true,
        }
    }

    /// The mix as a `mixer:` section: every track the engine has (or only `only`), and the buses.
    fn mixer_state(&self, only: Option<&[String]>) -> MixerSource {
        let mut tracks = OrderedMap::new();
        let Some((status, _)) = self.last else {
            return MixerSource::default();
        };
        for (i, ui) in self.tracks.iter().enumerate() {
            if only.is_some_and(|names| !names.contains(&ui.name)) {
                continue;
            }
            if let Some(ts) = status.tracks().get(i) {
                tracks.insert(ui.name.clone(), TrackMixSource::from_status(ts));
            }
        }
        let buses = (only.is_none() || self.mix_buses_changed).then(|| BusLevels::from_status(&status));
        MixerSource { buses, tracks }
    }

    /// What changed since the last call, and forget it.
    ///
    /// Answered from the newest snapshot, which the arrangement asks for a moment after the
    /// revision moved - by then the engine has reported the change.
    fn mixer_take(&mut self) -> MixerSource {
        let names = std::mem::take(&mut self.mix_changed);
        let mut out = self.mixer_state(Some(&names));
        if !self.mix_buses_changed {
            out.buses = None;
        }
        self.mix_buses_changed = false;
        out
    }

    /// Queue the commands that put the engine into `mixer`, tracks matched by name.
    fn queue_mix(&mut self, mixer: &MixerSource) {
        let names: Vec<String> = self.tracks.iter().map(|ui| ui.name.clone()).collect();
        let current: Vec<TrackStatus> = self
            .last
            .map(|(s, _)| s.tracks().to_vec())
            .unwrap_or_default();
        self.mix_backlog
            .extend(mixer_commands(mixer, &names, &current));
        self.drain_mix();
    }

    fn drain_mix(&mut self) {
        for _ in 0..MIX_BATCH {
            let Some(command) = self.mix_backlog.pop_front() else {
                return;
            };
            if let Err(e) = self.cmd.send(command) {
                log!("Mix not fully restored: {e}");
                self.mix_backlog.clear();
                return;
            }
        }
    }

    /// Write the free-mode mix of the tracks changed since the last write. `force` writes at once
    /// (the engine is going down) instead of after [`MIX_SAVE_DELAY`] of quiet.
    fn save_free_mix(&mut self, force: bool) {
        if self.free_mix_dirty.is_empty() {
            self.free_mix_since = None;
            return;
        }
        if !force && self.free_mix_since.is_some_and(|t| t.elapsed() < MIX_SAVE_DELAY) {
            return;
        }
        let names = std::mem::take(&mut self.free_mix_dirty);
        self.free_mix_since = None;
        let state = self.mixer_state(Some(&names));
        let entries: Vec<(String, TrackMixSource)> = state
            .tracks
            .iter()
            .map(|(name, entry)| (name.to_string(), entry.clone()))
            .collect();
        if let Err(e) = mixstore::merge_and_save(entries) {
            log!("Mix not saved to {}: {e}", mixstore::path().display());
        }
    }

    /// New latency compensation for one track.
    ///
    /// Refused when it would make the loop shorter than the compensation, because that is the one
    /// state the recording arithmetic cannot be in: the write pointer trails the play pointer by
    /// this much, so a loop below it would be overwritten while it is still being written.
    fn set_track_latency(&mut self, track: usize, latency: TrackLatency) -> Result<(), String> {
        if latency.trim.unsigned_abs() as i64 > MAX_LATENCY_FRAMES
            || latency.measured.map(i64::from).unwrap_or(0) > MAX_LATENCY_FRAMES
        {
            return Err(format!(
                "The latency compensation must stay within {MAX_LATENCY_FRAMES} frames \
                 (two seconds at 48 kHz)."
            ));
        }
        let worst = max_latency(
            self.info.latency_frames,
            self.tracks
                .iter()
                .enumerate()
                .map(|(i, ui)| if i == track { latency } else { ui.latency }),
        );
        looper_engine::engine::process::check_loop(
            &self.scheduler.timeline,
            self.scheduler.bars,
            worst,
        )?;

        self.cmd.send(Command::SetTrackLatency { track, latency })?;
        self.tracks[track].latency = latency;
        let rate = self.info.sample_rate.max(1) as f64;
        let effective = latency.resolve(self.info.latency_frames);
        self.message = Some(format!(
            "\"{}\": latency compensation {effective} frames ({:.2} ms){}. Applies to recordings \
             from now on, not to what is already recorded.",
            self.tracks[track].name,
            effective as f64 * 1000.0 / rate,
            if latency.inherits() {
                format!(" - global default {}", self.info.latency_frames)
            } else {
                String::new()
            },
        ));
        Ok(())
    }

    /// A new tempo redefines what every sample position means, so it is only accepted while every
    /// track is empty. The sentence for the refusal is the engine's own.
    fn set_tempo(
        &mut self,
        bpm: f64,
        beats_per_bar: u32,
        beat_unit: u32,
        bars: Option<u32>,
    ) -> Result<(), String> {
        let before = self.scheduler.timeline;
        let bars = bars.unwrap_or(self.scheduler.bars);
        self.reconfigure(bpm, beats_per_bar, beat_unit, bars)?;
        if self.transport && self.runner.is_none() {
            self.keep_phase(before)?;
        }
        self.message = Some(format!(
            "Tempo {bpm} BPM, {beats_per_bar}/{beat_unit}, loop {bars} bars."
        ));
        Ok(())
    }

    /// A tempo change while the clock runs: the engine keeps bar 1 where it was, so at the new
    /// tempo "now" would suddenly sit on some other beat and the click would jump. Moving bar 1 so
    /// that the beat count up to now stays what it was lets the click simply go on faster or slower
    /// - which is also what makes a tempo knob or tap tempo usable while the click runs. Only ever
    /// done while every track is empty (that is when a tempo change is accepted at all), so no loop
    /// is moved by it.
    fn keep_phase(&mut self, before: Timeline) -> Result<(), String> {
        let now = self.estimated_pos();
        let after = self.scheduler.timeline;
        let old_spb = before.samples_per_beat();
        let new_spb = after.samples_per_beat();
        if old_spb <= 0.0 || new_spb <= 0.0 {
            return Ok(());
        }
        let beats = now.saturating_sub(before.zero()) as f64 / old_spb;
        let per_loop = (after.signature().beats_per_bar.max(1) * self.scheduler.bars.max(1)) as f64;
        // The place inside the loop is kept; whole loops before it do not matter.
        let mut kept = beats % per_loop;
        if kept * new_spb > now as f64 {
            kept = beats.fract();
        }
        let zero = now.saturating_sub((kept * new_spb).round() as u64);
        // Stamped in the past on purpose: the engine applies it at once and takes `at` as bar 1.
        self.cmd.send(Command::TransportStart { at: zero })?;
        self.scheduler.timeline = after.with_zero(zero);
        Ok(())
    }

    /// The musical grid: tempo, time signature and the number of bars every layer buffer is
    /// allocated at.
    ///
    /// All four move together because they are one decision. A tempo change alone already reallocs
    /// every layer buffer (a bar is a different number of frames), and loading a score changes the
    /// bar count for the same reason - the longest section of a score is the longest loop any take
    /// in it can define. Only accepted while every track is empty, and the refusal is the engine's
    /// own sentence.
    fn reconfigure(
        &mut self,
        bpm: f64,
        beats_per_bar: u32,
        beat_unit: u32,
        bars: u32,
    ) -> Result<(), String> {
        let busy = self
            .last
            .map(|(s, _)| s.tracks().iter().any(|t| t.state != TrackState::Empty))
            .unwrap_or(false);
        if busy {
            return Err(Refusal::Tempo
                .message()
                .unwrap_or("Tempo change refused.")
                .to_string());
        }
        let bars = bars.max(1);
        let timeline = build_timeline(
            self.info.sample_rate,
            bpm,
            beats_per_bar,
            beat_unit,
            bars,
            max_latency(
                self.info.latency_frames,
                self.tracks.iter().map(|ui| ui.latency),
            ),
        )?;
        let layer_frames = loop_capacity(&timeline, bars);
        self.cmd.send(Command::SetTempo {
            bpm,
            signature: timeline.signature(),
            layer_frames,
        })?;
        // Every allocation at the new layer length happens on this thread; the old stock is
        // dropped here and the engine hands back what it still holds.
        self.pool.set_layer_frames(layer_frames as usize);
        // Bar 1 stays where the transport put it, exactly as the engine keeps it on `SetTempo`.
        let timeline = timeline.with_zero(self.scheduler.timeline.zero());
        self.scheduler.timeline = timeline;
        self.scheduler.bars = bars;
        self.update_timeline_info(&timeline, layer_frames);
        Ok(())
    }

    fn update_timeline_info(&mut self, timeline: &Timeline, layer_frames: u64) {
        let loop_samples = timeline.span_bars(0, self.scheduler.bars);
        self.info.bpm = timeline.bpm();
        self.info.beats_per_bar = timeline.signature().beats_per_bar;
        self.info.beat_unit = timeline.signature().beat_unit;
        self.info.samples_per_beat = timeline.samples_per_beat();
        self.info.bars = self.scheduler.bars;
        self.info.loop_samples = loop_samples;
        self.info.loop_seconds = timeline.samples_to_secs(loop_samples);
        self.info.layer_capacity = layer_frames;
        // A stereo track costs twice a mono one, so the ceiling counts channels, not tracks.
        let kinds: Vec<Channels> = self.tracks.iter().map(|t| t.channels).collect();
        self.info.memory_mb = max_memory_bytes(
            layer_frames,
            total_channels(&kinds),
            spare_channels(spare_slots_for(&kinds, SPARE_SLOTS)),
        ) as f64
            / 1_048_576.0;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tap_tempo_takes_the_median_gap_and_starts_over_after_a_pause() {
        let t0 = Instant::now();
        let mut taps = TapTempo::default();
        let at = |ms: u64| t0 + Duration::from_millis(ms);
        assert_eq!(taps.tap(at(0)), None);
        assert_eq!(taps.tap(at(500)), None, "zwei Taps sind noch kein Tempo");
        assert_eq!(taps.tap(at(1_000)), Some(120.0));
        // One stumble does not move the median.
        assert_eq!(taps.tap(at(1_700)), Some(120.0));
        assert_eq!(taps.tap(at(2_200)), Some(120.0));
        // A pause starts a new series.
        assert_eq!(taps.tap(at(10_000)), None);
        assert_eq!(taps.tap(at(10_600)), None);
        assert_eq!(taps.tap(at(11_200)), Some(100.0));
    }

    #[test]
    fn every_action_knows_whether_it_addresses_a_track() {
        assert_eq!(Action::Record { track: 2 }.track(), Some(2));
        assert_eq!(
            Action::LayerGain {
                track: 1,
                layer: 0,
                gain: 0.5
            }
            .track(),
            Some(1)
        );
        assert_eq!(Action::ClearAll.track(), None);
        assert_eq!(Action::SetClick { on: true }.track(), None);
        // The effect actions are all track-scoped: a chain belongs to one channel.
        assert_eq!(Action::FxBypass { track: 3, on: true }.track(), Some(3));
        assert_eq!(
            Action::FxEnable {
                track: 0,
                slot: FxSlot::Reverb,
                on: true
            }
            .track(),
            Some(0)
        );
        assert_eq!(
            Action::FxPreset {
                track: 1,
                preset: FxPreset::Voice
            }
            .track(),
            Some(1)
        );
        assert_eq!(
            Action::FxParam {
                track: 2,
                param: FxParam::ReverbMix(0.2)
            }
            .track(),
            Some(2)
        );
        assert_eq!(
            Action::SetTempo {
                bpm: 120.0,
                beats_per_bar: 4,
                beat_unit: 4,
                bars: None,
            }
            .track(),
            None
        );
    }

    /// The start path has to refuse a bad configuration *before* it opens anything - a wrong host
    /// name must not cost a device, and the sentence has to name what was wrong.
    ///
    /// Deliberately the one failure that needs no hardware: everything past it opens the
    /// interface, which is not something a test may do.
    #[test]
    fn an_unknown_host_is_refused_in_words_before_any_device_is_opened() {
        let config: StartConfig = serde_json::from_str(
            r#"{"host": "gibtsnicht", "tracks": [{"name": "stimme", "input_channel": 1}]}"#,
        )
        .expect("Konfiguration");
        // `Session` owns two cpal streams and has no `Debug`, so the error is unwrapped by hand.
        let err = match Session::start(config) {
            Err(e) => e,
            Ok(_) => panic!("den Host gibt es nicht, das haette scheitern muessen"),
        };
        assert!(err.contains("gibtsnicht"), "{err}");
        // Wording-robust: the sentence lists the hosts this system has, whatever it says around them.
        for host in cpal::available_hosts() {
            assert!(err.contains(&host.to_string()), "die Meldung zaehlt die Hosts auf: {err}");
        }
    }

    /// The transport belongs to the runner while a score plays, the mix belongs to the human. This
    /// is the line, and it is the reason the runner's predicted loop geometry stays exact.
    #[test]
    fn the_runner_owns_the_transport_and_the_human_owns_the_mix() {
        for action in [
            Action::Record { track: 0 },
            Action::Overdub { track: 0 },
            Action::StopTrack { track: 0 },
            Action::Play { track: 0 },
            Action::ClearTrack { track: 0 },
            Action::ClearAll,
            Action::SetTempo {
                bpm: 120.0,
                beats_per_bar: 4,
                beat_unit: 4,
                bars: None,
            },
            Action::SetQuantize {
                quantize: Quantize::Bar,
            },
        ] {
            assert!(action.moves_transport(), "{action:?} verschiebt Takes");
        }
        for action in [
            Action::SetPan {
                track: 0,
                pan: 0.5,
            },
            Action::SetMonitor {
                track: 0,
                on: true,
            },
            Action::SetClick { on: false },
            Action::LayerGain {
                track: 0,
                layer: 0,
                gain: 0.5,
            },
            Action::LayerMute {
                track: 0,
                layer: 0,
                muted: true,
            },
            Action::FxBypass { track: 0, on: true },
            Action::FxPreset {
                track: 0,
                preset: FxPreset::Voice,
            },
            Action::SetTrackLatency {
                track: 0,
                latency: TrackLatency::INHERITED,
            },
        ] {
            assert!(
                !action.moves_transport(),
                "{action:?} gehoert weiter dem Menschen, auch waehrend die Partitur laeuft"
            );
        }
    }

    /// **The line has to be in the same place for a pad as for the mouse.**
    ///
    /// While a score plays, the runner owns the transport (`docs/architektur.md` section 10). The
    /// mouse runs into that through [`Action::moves_transport`] here; a MIDI pad runs into it
    /// through `midi::Target::moves_transport` in the engine library, and the two are separate
    /// lists because they are separate vocabularies. Separate lists drift, so this test pairs them
    /// up and insists they agree - and then insists that every address which moves the transport is
    /// actually in the pairing, so a new one cannot be added on one side only.
    #[test]
    fn a_midi_pad_hits_the_same_transport_boundary_as_a_mouse_click() {
        use looper_engine::midi::{Target, TrackRef, catalogue};

        let track = || TrackRef::Index(0);
        let pairs: Vec<(Target, Action)> = vec![
            (Target::Record(track()), Action::Record { track: 0 }),
            (Target::Overdub(track()), Action::Overdub { track: 0 }),
            (Target::Play(track()), Action::Play { track: 0 }),
            (Target::StopTrack(track()), Action::StopTrack { track: 0 }),
            (Target::ClearTrack(track()), Action::ClearTrack { track: 0 }),
            (Target::ClearAll, Action::ClearAll),
            (Target::Transport, Action::Transport { run: None }),
            (Target::TapTempo, Action::TapTempo),
            (
                Target::Tempo,
                Action::SetTempo {
                    bpm: 120.0,
                    beats_per_bar: 4,
                    beat_unit: 4,
                    bars: None,
                },
            ),
            (
                Target::Quantize(Quantize::Bar),
                Action::SetQuantize {
                    quantize: Quantize::Bar,
                },
            ),
            (
                Target::Quantize(Quantize::Loop),
                Action::SetQuantize {
                    quantize: Quantize::Loop,
                },
            ),
            // The other half of the rule: the mix stays the human's, from either input.
            (
                Target::Monitor(track()),
                Action::SetMonitor {
                    track: 0,
                    on: true,
                },
            ),
            (Target::Pan(track()), Action::SetPan { track: 0, pan: 0.5 }),
            (Target::Click, Action::SetClick { on: true }),
            (
                Target::LayerGain(track(), 0),
                Action::LayerGain {
                    track: 0,
                    layer: 0,
                    gain: 0.5,
                },
            ),
            (
                Target::LayerMute(track(), 0),
                Action::LayerMute {
                    track: 0,
                    layer: 0,
                    muted: true,
                },
            ),
            (
                Target::FxBypass(track()),
                Action::FxBypass { track: 0, on: true },
            ),
            (
                Target::FxPreset(track(), FxPreset::Voice),
                Action::FxPreset {
                    track: 0,
                    preset: FxPreset::Voice,
                },
            ),
        ];

        for (target, action) in &pairs {
            assert_eq!(
                target.moves_transport(),
                action.moves_transport(),
                "\"{target}\" und {action:?} sind sich uneinig darueber, ob das den Transport \
                 verschiebt"
            );
        }

        for target in catalogue(1, 1, 2) {
            if target.moves_transport() {
                assert!(
                    pairs.iter().any(|(paired, _)| *paired == target),
                    "\"{target}\" verschiebt den Transport, steht aber in keiner Paarung - die \
                     MIDI-Seite und die Maus-Seite koennen auseinanderlaufen, ohne dass es \
                     auffaellt"
                );
            }
        }
    }

    /// **An incoming MIDI event ends up on the session, with the right target and the right value.**
    ///
    /// The whole chain minus the device: a mapping, an event, the router - and then the piece this
    /// file adds, which is turning the resolved intent into the very action a mouse click produces.
    /// If this drifts, a pad and a button on the same target start doing different things.
    #[test]
    fn a_resolved_midi_event_becomes_the_action_a_mouse_click_produces() {
        use looper_engine::midi::{
            Binding, Context, MidiEvent, MidiId, MidiMap, Router, TrackLayout,
        };

        let signature = TimeSignature::new(4, 4);
        let mut map = MidiMap::new();
        map.insert(
            MidiId::note(1, 36),
            Binding::new(Target::parse("track.2.record").expect("Adresse")),
        );
        map.insert(
            MidiId::cc(1, 3),
            Binding::new(Target::parse("track.1.fx.reverb.mix").expect("Adresse")),
        );
        map.insert(
            MidiId::note(1, 45),
            Binding::new(Target::parse("transport.next").expect("Adresse")),
        );
        let mut router = Router::new(map);
        let layout = TrackLayout::new(["stimme", "gitarre"]);

        // A pad on the second track's record button.
        let ctx = Context::bare(&layout);
        let action = router
            .resolve(
                &MidiEvent::NoteOn {
                    channel: 1,
                    note: 36,
                    velocity: 100,
                },
                &ctx,
            )
            .action()
            .expect("das Pad loest aus");
        assert_eq!(
            intent_of(action, signature, None),
            MidiIntent::Session(Action::Record { track: 1 }),
            "track.2 ist der zweite Track, also Index 1"
        );

        // A knob at its stop lands exactly on the end of the target's range, not a hair below.
        let action = router
            .resolve(
                &MidiEvent::ControlChange {
                    channel: 1,
                    controller: 3,
                    value: 127,
                },
                &ctx,
            )
            .action()
            .expect("der Regler faehrt den Wert");
        match intent_of(action, signature, None) {
            MidiIntent::Session(Action::FxParam { track, param }) => {
                assert_eq!(track, 0);
                assert_eq!(param, FxParam::ReverbMix(1.0));
            }
            other => panic!("ein Regler auf dem Hall-Anteil, nicht {other:?}"),
        }

        // The score's transport goes to the runner, not through `Session::apply`.
        let action = router
            .resolve(
                &MidiEvent::NoteOn {
                    channel: 1,
                    note: 45,
                    velocity: 127,
                },
                &ctx,
            )
            .action()
            .expect("der Release-Knopf");
        assert_eq!(
            intent_of(action, signature, None),
            MidiIntent::Score(ScoreAction::Next)
        );
    }

    /// The transport boundary has to survive the translation into an [`Action`].
    ///
    /// The pairing test above proves `Target` and `Action` agree about what moves the transport.
    /// This one proves the step in between does not lose it: every MIDI action that moves the
    /// transport has to become an action that also does, or the refusal would be worked out on one
    /// vocabulary and the take put on the timeline by the other.
    #[test]
    fn the_transport_boundary_survives_the_translation_from_midi_to_action() {
        let signature = TimeSignature::new(4, 4);
        let actions = [
            MidiAction::Record { track: 0 },
            MidiAction::Overdub { track: 0 },
            MidiAction::Play { track: 0 },
            MidiAction::StopTrack { track: 0 },
            MidiAction::ClearTrack { track: 0 },
            MidiAction::ClearAll,
            MidiAction::Transport,
            MidiAction::Tempo(120.0),
            MidiAction::SetQuantize(Quantize::Bar),
            MidiAction::Click(true),
            MidiAction::Monitor {
                track: 0,
                on: true,
            },
            MidiAction::Pan { track: 0, pan: 0.5 },
            MidiAction::LatencyTrim {
                track: 0,
                trim: 120,
            },
            MidiAction::LayerMute {
                track: 0,
                layer: 0,
                muted: true,
            },
            MidiAction::LayerRemove { track: 0, layer: 0 },
            MidiAction::LayerGain {
                track: 0,
                layer: 0,
                gain: 0.5,
            },
            MidiAction::FxBypass { track: 0, on: true },
            MidiAction::FxEnable {
                track: 0,
                slot: FxSlot::Reverb,
                on: true,
            },
            MidiAction::FxPreset {
                track: 0,
                preset: FxPreset::Voice,
            },
            MidiAction::FxParam {
                track: 0,
                param: FxParam::ReverbMix(0.2),
            },
        ];
        for action in actions {
            match intent_of(action, signature, None) {
                MidiIntent::Session(what) => assert_eq!(
                    action.moves_transport(),
                    what.moves_transport(),
                    "{action:?} und {what:?} sind sich uneinig darueber, ob das den Transport \
                     verschiebt"
                ),
                // The score's own transport is deliberately outside the rule: it *is* the runner's
                // release button, and locking it would lock the one thing the pad is held for.
                MidiIntent::Score(_) => {
                    panic!("keine dieser Absichten gehoert dem Runner: {action:?}")
                }
                // Plugin slots are mix, never transport.
                MidiIntent::Plugin(what) => assert!(!what.moves_transport()),
            }
        }
    }

    /// The manual surcharge is the only half a knob may touch. A measurement that a turn of the
    /// trim knob wiped out would have to be made again with a cable and a click track.
    #[test]
    fn the_latency_knob_moves_the_surcharge_and_keeps_the_measurement() {
        let intent = intent_of(
            MidiAction::LatencyTrim {
                track: 1,
                trim: -240,
            },
            TimeSignature::new(4, 4),
            Some(827),
        );
        assert_eq!(
            intent,
            MidiIntent::Session(Action::SetTrackLatency {
                track: 1,
                latency: TrackLatency {
                    measured: Some(827),
                    trim: -240
                }
            })
        );
    }

    /// The editor's own command: a score in, either the compiled form or every complaint at once.
    /// Nothing here opens a device, which is what lets it run while somebody types.
    #[test]
    fn compiling_a_score_needs_no_device_and_reports_everything_at_once() {
        let good = "title: T\nbpm: 100\ntracks:\n  a: {input: 1}\nsections:\n  - id: eins\n    bars: 4\n    tracks:\n      a: record\n";
        let outcome = compile(good);
        assert!(outcome.ok);
        assert!(outcome.errors.is_empty());
        assert_eq!(outcome.score.expect("Partitur").sections.len(), 1);

        // Two mistakes, two complaints - a compiler that stopped at the first would turn one pass
        // through the file into two.
        let bad = "title: T\nbpm: 100\ntracks:\n  a: {input: 1}\nsections:\n  - id: eins\n    bars: 4\n    tracks:\n      b: record\n      a: recrd\n";
        let outcome = compile(bad);
        assert!(!outcome.ok);
        assert!(outcome.score.is_none());
        assert_eq!(outcome.errors.len(), 2, "{:?}", outcome.errors);
        assert!(outcome.errors.iter().all(|i| i.line.is_some()));
    }

    /// The load command has nowhere to put markers, so it says the same thing as one block of text -
    /// with the positions in it, so it can be pasted back into the editor.
    #[test]
    fn the_load_command_renders_every_complaint_into_one_block() {
        let error = compile_score("bpm: 100\n").expect_err("ohne tracks und sections");
        let text = render_issues(&error);
        assert!(text.starts_with("The score does not compile"), "{text}");
        assert_eq!(
            text.lines().count(),
            1 + error.issues.len(),
            "Kopfzeile plus eine Zeile je Fehler: {text}"
        );
    }

    fn start_config(tracks: &str) -> StartConfig {
        serde_json::from_str(&format!(r#"{{"host": "coreaudio", "tracks": {tracks}}}"#))
            .expect("Konfiguration")
    }

    /// A rebuild takes the score's tracks with everything the score says about them, and the
    /// score's grid - and the result is exactly what `check_tracks` accepts, so the load that
    /// follows cannot be refused for the layout.
    #[test]
    fn a_rebuild_takes_the_tracks_and_grid_of_the_score() {
        let score = compile_score(
            "bpm: 90\ntime_signature: 6/8\ntracks:\n  keys: {input: [3, 4], pan: 0.3, latency: 400, \
             latency_trim: 12, bus: main, monitor_bus: none}\n  stimme: {input: 1}\nsections:\n  \
             - id: a\n    bars: 3\n  - id: b\n    bars: 5\n",
        )
        .expect("Partitur");
        let previous = start_config(r#"[{"name": "gitarre", "input_channel": 2}]"#);
        let config = rebuilt_config(&previous, &score, None, Quantize::Bar);

        assert_eq!(config.tracks.len(), 2);
        let keys = &config.tracks[0];
        assert_eq!(
            (keys.name.as_str(), keys.input_channel, keys.input_channel_right),
            ("keys", 3, Some(4))
        );
        assert_eq!(keys.pan, 0.3);
        assert_eq!(keys.track_latency(), TrackLatency { measured: Some(400), trim: 12 });
        assert_eq!(keys.loop_send("keys").unwrap(), BusSend::parse("main").unwrap());
        assert_eq!(keys.monitor_send("keys").unwrap(), BusSend::parse("none").unwrap());
        assert_eq!(
            (config.bpm, config.beats_per_bar, config.beat_unit, config.bars),
            (90.0, 6, 8, 5)
        );
        assert_eq!(config.quantize, QuantizeName::Bar);
        // The device side is untouched.
        assert_eq!(config.host, previous.host);
        assert_eq!(config.buffer_frames, previous.buffer_frames);
        assert_eq!(config.latency_frames, previous.latency_frames);

        let defs = resolve_tracks(&config.tracks, 8).expect("Tracks passen aufs Geraet");
        assert_eq!(check_tracks(&score, &defs), Ok(()));
        assert!(
            check_tracks(&score, &resolve_tracks(&previous.tracks, 8).unwrap()).is_err(),
            "vorher passte die Engine nicht"
        );
    }

    /// What was changed at runtime survives a rebuild: bus volumes and outputs, and the click.
    #[test]
    fn a_rebuild_keeps_the_bus_setup_and_the_click_of_the_running_session() {
        use looper_engine::engine::bus::{BusOutput, BusRouting};
        let score =
            compile_score("bpm: 100\ntracks:\n  a: {input: 1}\nsections:\n  - id: x\n    bars: 2\n")
                .expect("Partitur");
        let previous = start_config(r#"[{"name": "b", "input_channel": 1}]"#);
        let mut status = Status::default();
        status.click = false;
        status.bus_gain = [0.8, 0.25];
        status.bus_out = BusRouting::new(BusOutput::pair(0), BusOutput::pair(2));
        status.click_gain = 0.3;
        status.monitor_gain = 1.5;
        let config = rebuilt_config(&previous, &score, Some(&status), Quantize::Loop);
        assert!(!config.click);
        assert_eq!(config.click_gain, 0.3, "the click level turned down live stays down");
        assert_eq!(config.monitor_gain, 1.5);
        assert_eq!(config.bus_gain_main, Some(0.8));
        assert_eq!(config.bus_gain_monitor, Some(0.25));
        assert_eq!(config.bus_out_monitor, Some(BusOutConfig { channel: 3, width: 2 }));
        assert_eq!(config.bus_out_main, Some(BusOutConfig { channel: 1, width: 2 }));
    }

    /// Click and monitor levels are mix decisions: available while a score plays, on no track,
    /// and refused in a sentence outside 0..4.
    #[test]
    fn the_levels_belong_to_the_human_and_are_range_checked() {
        for action in [Action::SetClickGain { gain: 0.5 }, Action::SetMonitorGain { gain: 2.0 }] {
            assert!(!action.moves_transport());
            assert_eq!(action.track(), None);
        }
        assert!(check_level("Click", 0.0).is_ok());
        assert!(check_level("Click", MAX_LEVEL_GAIN).is_ok());
        let refusal = check_level("Monitoring", 4.5).unwrap_err();
        assert!(refusal.starts_with("Monitoring level"), "{refusal}");
        assert!(check_level("Click", -0.1).is_err());
    }

    /// The one fact about the build that decides whether the app is usable at all.
    #[test]
    fn the_device_survey_reports_whether_asio_is_built_in() {
        assert_eq!(app_info().asio_built, cfg!(feature = "asio"));
        assert_eq!(app_info().max_tracks, MAX_TRACKS as u32);
        assert_eq!(app_info().max_layers, MAX_LAYERS as u32);
    }
}
