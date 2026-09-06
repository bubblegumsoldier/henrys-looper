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

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::time::{Duration, Instant};

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{InputCallbackInfo, OutputCallbackInfo, SupportedBufferSize};
use tauri::{AppHandle, Emitter};

use looper_engine::audio::{self, DeviceOpts};
use looper_engine::engine::bus::{
    Bus, BusOutput, BusSend, MAX_BUS_GAIN, TrackSource, place_buses, routing_note,
};
use looper_engine::engine::calibrate::{CalibrateOpts, cmd_calibrate};
use looper_engine::engine::command::{
    Command, CommandSender, LayerPool, MAX_TRACKS, Refusal, Status, StatusReceiver, TrackStatus,
    buffer_channel, command_channel, status_channel,
};
use looper_engine::engine::frame::{Channels, TrackInput};
use looper_engine::engine::fx::{FxParam, FxPreset, FxSlot};
use looper_engine::engine::live::MAX_LATENCY_FRAMES;
use looper_engine::engine::live::TrackDef;
use looper_engine::engine::bus::BUS_SAMPLES;
use looper_engine::engine::process::{
    EngineConfig, EngineCore, loop_capacity, max_latency, max_memory_bytes, spare_channels,
    spare_slots_for, total_channels,
};
use looper_engine::engine::runner::{
    DEFAULT_COUNT_IN_BARS, Phase, Runner, TRANSPORT_BELONGS_TO_SCORE, check_tracks,
};
use looper_engine::engine::timeline::{TimeSignature, Timeline};
use looper_engine::engine::track::{MAX_LAYERS, Track, TrackLatency, TrackState};
use looper_engine::midi::{
    Context, FxKnob, MidiAction, ParamState, Resolution, Target, TrackLayout, TrackRef,
};
use looper_engine::score::{ScoreError, compile_score};

use crate::logfile::{self, log};
use crate::midi::{MidiBridge, MidiOutcome, MidiPortView, MidiSaved, MidiView};
use crate::proto::{
    AppInfo, CalibrateConfig, CalibrateOutcome, CompileOutcome, ConfigInfo, DeviceInfo,
    DeviceReport, EngineInfo, HostInfo, QuantizeName, ScoreLoaded, StartConfig, StatusEvent,
    TrackConfig, bus_events, dbfs, score_event, track_event,
};
use crate::schedule::{Quantize, Scheduled, Scheduler, build_timeline, resolve_tracks};

/// Event the status snapshot is pushed on.
pub const STATUS_EVENT: &str = "looper://status";
/// Event carrying [`AppInfo`], emitted once at startup.
pub const READY_EVENT: &str = "looper://ready";
/// Event carrying incoming MIDI and the state of the mapping. Its own event and not part of the
/// status snapshot - see the module comment of [`crate::midi`] for why.
pub const MIDI_EVENT: &str = "looper://midi";

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
/// The one sentence for "there is nothing to talk to yet", so every command says it the same way.
const NO_ENGINE: &str = "Die Engine laeuft nicht. Erst starten.";

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
    SetClick { on: bool },
    SetTempo { bpm: f64, beats_per_bar: u32, beat_unit: u32 },
    /// Switch the grid a take snaps to while the engine runs. Takes that are already armed keep
    /// the position they were given - the engine has those commands already.
    SetQuantize { quantize: Quantize },

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
                | Action::SetTempo { .. }
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
            | Action::SetTrackSend { track, .. }
            | Action::SetTrackBus { track, .. }
            | Action::SetTrackLatency { track, .. }
            | Action::LayerMute { track, .. }
            | Action::LayerRemove { track, .. }
            | Action::LayerGain { track, .. }
            | Action::FxBypass { track, .. }
            | Action::FxEnable { track, .. }
            | Action::FxParam { track, .. }
            | Action::FxPreset { track, .. } => Some(track),
            Action::ClearAll
            | Action::SetClick { .. }
            | Action::SetBusGain { .. }
            | Action::SetBusOutput { .. }
            | Action::SetTempo { .. }
            | Action::SetQuantize { .. } => None,
        }
    }
}

enum Request {
    ListDevices(Sender<Result<DeviceReport, String>>),
    Start(Box<StartConfig>, Sender<Result<EngineInfo, String>>),
    Stop(Sender<Result<(), String>>),
    Act(Action, Sender<Result<(), String>>),
    ScoreLoad(String, Option<u32>, Sender<Result<ScoreLoaded, String>>),
    ScoreAct(ScoreAction, Sender<Result<String, String>>),
    Calibrate(Box<CalibrateConfig>, Sender<Result<CalibrateOutcome, String>>),

    // --- MIDI. Handled on this thread because that is where the session is, and because a MIDI
    // connection belongs to the thread that opened it.
    MidiPorts(Sender<Result<Vec<MidiPortView>, String>>),
    MidiOpen(String, Sender<Result<MidiView, String>>),
    MidiClose(Sender<Result<MidiView, String>>),
    /// `Some(address)` arms the learn mode for that address, `None` cancels it.
    MidiLearn(Option<String>, Sender<Result<MidiView, String>>),
    MidiUnbind(String, Sender<Result<MidiView, String>>),
    MidiSave(Sender<Result<MidiSaved, String>>),
    MidiState(Sender<Result<MidiView, String>>),
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
            .expect("Audio-Thread liess sich nicht starten");
        Self { tx }
    }

    fn call<T>(&self, make: impl FnOnce(Sender<Result<T, String>>) -> Request) -> Result<T, String> {
        let (tx, rx) = mpsc::channel();
        self.tx
            .send(make(tx))
            .map_err(|_| "Der Audio-Thread laeuft nicht mehr.".to_string())?;
        rx.recv()
            .map_err(|_| "Der Audio-Thread hat nicht geantwortet.".to_string())?
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

    pub fn act(&self, action: Action) -> Result<(), String> {
        self.call(|reply| Request::Act(action, reply))
    }

    pub fn score_load(&self, yaml: String, count_in: Option<u32>) -> Result<ScoreLoaded, String> {
        self.call(|reply| Request::ScoreLoad(yaml, count_in, reply))
    }

    pub fn score_act(&self, action: ScoreAction) -> Result<String, String> {
        self.call(|reply| Request::ScoreAct(action, reply))
    }

    pub fn calibrate(&self, config: CalibrateConfig) -> Result<CalibrateOutcome, String> {
        self.call(|reply| Request::Calibrate(Box::new(config), reply))
    }

    // ---- MIDI ------------------------------------------------------------------------------

    pub fn midi_ports(&self) -> Result<Vec<MidiPortView>, String> {
        self.call(Request::MidiPorts)
    }

    pub fn midi_open(&self, device: String) -> Result<MidiView, String> {
        self.call(|reply| Request::MidiOpen(device, reply))
    }

    pub fn midi_close(&self) -> Result<MidiView, String> {
        self.call(Request::MidiClose)
    }

    pub fn midi_learn(&self, address: Option<String>) -> Result<MidiView, String> {
        self.call(|reply| Request::MidiLearn(address, reply))
    }

    pub fn midi_unbind(&self, id: String) -> Result<MidiView, String> {
        self.call(|reply| Request::MidiUnbind(id, reply))
    }

    pub fn midi_save(&self) -> Result<MidiSaved, String> {
        self.call(Request::MidiSave)
    }

    pub fn midi_state(&self) -> Result<MidiView, String> {
        self.call(Request::MidiState)
    }
}

// ---------------------------------------------------------------------------------------------
// The thread
// ---------------------------------------------------------------------------------------------

fn host_thread(rx: Receiver<Request>, app: AppHandle) {
    log!("Audio-Thread bereit (Engine laeuft noch nicht).");
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
                log!("Engine hat sich selbst beendet.");
                stop_session(&mut session, &app);
            }
        }

        pump_midi(&mut midi, &mut session, &app);
    }

    // The handle was dropped, so the app is going down. Give the device back properly anyway.
    stop_session(&mut session, &app);
    log!("Audio-Thread beendet.");
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
                        "Host {}: {} Geraete, Standard-Ausgang {}",
                        host.name,
                        host.devices.len(),
                        host.default_output.as_deref().unwrap_or("keiner")
                    ),
                }
            }
            let _ = reply.send(Ok(report));
        }
        Request::Start(config, reply) => {
            // Restart with a different configuration is a stop plus a start; doing it here rather
            // than refusing keeps the frontend from having to sequence two commands.
            if session.is_some() {
                log!("Engine laeuft bereits - wird fuer den Neustart gestoppt.");
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
                        "Engine gestartet: {} / {} @ {} Hz, {} Frames, {} Tracks ({} davon stereo), \
                         Latenz-Vorgabe {} Frames, {} Tracks mit eigenem Wert.",
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
                    log!("Engine startet nicht: {e}");
                    let _ = reply.send(Err(e));
                }
            }
        }
        Request::Stop(reply) => {
            if session.is_none() {
                let _ = reply.send(Err("Die Engine laeuft nicht.".to_string()));
                return;
            }
            stop_session(session, app);
            log!("Engine gestoppt.");
            let _ = reply.send(Ok(()));
        }
        Request::Act(action, reply) => {
            let result = match session.as_mut() {
                Some(active) => active.apply(action),
                None => Err(NO_ENGINE.to_string()),
            };
            if let Err(e) = &result {
                log!("Kommando abgelehnt: {e}");
            }
            let _ = reply.send(result);
        }
        Request::ScoreLoad(yaml, count_in, reply) => {
            let result = match session.as_mut() {
                Some(active) => active.load_score(&yaml, count_in),
                None => Err(NO_ENGINE.to_string()),
            };
            match &result {
                Ok(loaded) => {
                    log!("Partitur geladen: {}", loaded.message);
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
                                "Die MIDI-Bindungen der Partitur wurden nicht uebernommen: {}",
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
                Err(e) => log!("Partitur nicht geladen: {e}"),
            }
            let _ = reply.send(result);
        }
        Request::ScoreAct(action, reply) => {
            let result = match session.as_mut() {
                Some(active) => active.score_act(action),
                None => Err(NO_ENGINE.to_string()),
            };
            if let Ok(message) = &result {
                log!("Partitur: {message}");
            }
            let _ = reply.send(result);
        }
        Request::Calibrate(config, reply) => {
            let _ = reply.send(run_calibration(*config, session.is_some()));
        }

        // --- MIDI -------------------------------------------------------------------------
        Request::MidiPorts(reply) => {
            let _ = reply.send(MidiBridge::ports());
        }
        Request::MidiOpen(device, reply) => {
            let result = midi.open(&device);
            match &result {
                Ok(view) => log!(
                    "MIDI-Eingang offen: {} ({} Bindungen).",
                    view.port.as_deref().unwrap_or("?"),
                    view.bindings.len()
                ),
                Err(e) => log!("MIDI-Eingang nicht geoeffnet: {e}"),
            }
            let _ = reply.send(result);
        }
        Request::MidiClose(reply) => {
            let view = midi.close();
            log!("MIDI-Eingang geschlossen.");
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
        Request::MidiUnbind(id, reply) => {
            let _ = reply.send(midi.unbind(&id));
        }
        Request::MidiSave(reply) => {
            let result = midi.save();
            match &result {
                Ok(saved) => log!("MIDI: {}", saved.message),
                Err(e) => log!("MIDI-Profil nicht gespeichert: {e}"),
            }
            let _ = reply.send(result);
        }
        Request::MidiState(reply) => {
            let _ = reply.send(Ok(midi.view()));
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
        MidiAction::Tempo(bpm) => MidiIntent::Session(Action::SetTempo {
            bpm,
            beats_per_bar: signature.beats_per_bar,
            beat_unit: signature.beat_unit,
        }),
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
    }
}

/// Drain the MIDI queue, apply what it means, and tell the web view.
///
/// Called once per turn of the host loop, which is every four milliseconds while anything is open.
/// Nothing here allocates unless an event actually arrived.
fn pump_midi(midi: &mut MidiBridge, session: &mut Option<Session>, app: &AppHandle) {
    if let Some(gone) = midi.check_alive() {
        log!("{gone}");
    }

    let mut events = Vec::new();
    midi.drain(&mut events);

    for event in events {
        // The learn mode swallows the stream: while it waits for a control, that control must not
        // also do its old job on the way in.
        if midi.is_learning() {
            if let Some(report) = midi.feed_learn(&event) {
                log!("MIDI gelernt: {}", report.note.as_deref().unwrap_or(""));
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
                midi.resolve(&event, &ctx)
            }
            // Without an engine there are no tracks to resolve a binding against. The router says
            // so per binding, which is a better sentence than one blanket refusal.
            None => {
                let layout = TrackLayout::default();
                let ctx = Context::bare(&layout);
                midi.resolve(&event, &ctx)
            }
        };

        let target = midi.target_of(&event);
        match resolution {
            Resolution::Unbound => midi.report(&event, MidiOutcome::Unbound, None, None),
            Resolution::Absorbed(reason) => {
                midi.report(&event, MidiOutcome::Absorbed, target.as_ref(), Some(reason));
            }
            Resolution::Refused(reason) => {
                log!("MIDI abgelehnt: {reason}");
                midi.report(&event, MidiOutcome::Refused, target.as_ref(), Some(reason));
            }
            Resolution::Action(action) => {
                let Some(active) = session.as_mut() else {
                    midi.report(
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
                };
                match outcome {
                    Ok(message) => {
                        // A preset moves every knob at once; the physical ones have to catch up
                        // before they act again.
                        if matches!(action, MidiAction::FxPreset { .. }) {
                            midi.rearm();
                        }
                        midi.report(&event, MidiOutcome::Action, target.as_ref(), message);
                    }
                    Err(e) => {
                        log!("MIDI abgelehnt: {e}");
                        midi.report(&event, MidiOutcome::Refused, target.as_ref(), Some(e));
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
            Target::FxBypass(track) => Some(self.fx(track)?.bypass),
            Target::FxEnabled(track, slot) => {
                let fx = self.fx(track)?;
                Some(fx.settings.enabled[slot.index()])
            }
            Target::Send(track, source, bus) => {
                let index = self.index(track)?;
                Some(self.send_of(index, *source).on(*bus))
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

/// Shut the engine down and tell the frontend, in that order.
fn stop_session(session: &mut Option<Session>, app: &AppHandle) {
    if let Some(active) = session.take() {
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
                    error: Some(format!("Host liess sich nicht oeffnen: {e}")),
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
                    error: Some(format!("Geraeteliste nicht lesbar: {e}")),
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
                "Eingangskonfigurationen",
                &mut note,
            );
            let output_configs = configs_of(
                device.supported_output_configs(),
                "Ausgangskonfigurationen",
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

/// The same complaints as one German block, for a caller with nowhere to put markers - the load
/// command, whose answer is a single sentence.
fn render_issues(error: &ScoreError) -> String {
    let mut out = format!(
        "Die Partitur laesst sich nicht uebersetzen ({} Fehler):",
        error.issues.len()
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
            "Die Engine laeuft. Zum Kalibrieren erst stoppen - die Messung braucht das Geraet \
             fuer sich."
                .to_string(),
        );
    }
    if !logfile::enabled() {
        return Err(format!(
            "Es laesst sich keine Logdatei schreiben ({}). Die Kalibrierung gibt ihr Ergebnis \
             dort aus und wird deshalb nicht gestartet.",
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
        "Kalibrierung startet: {} Laeufe, {} Takte, Pruefwert {} Frames, Track {}.",
        opts.runs,
        opts.bars,
        opts.latency_frames,
        opts.for_track
    );
    cmd_calibrate(&dev, &opts)?;
    log!("Kalibrierung fertig.");
    Ok(CalibrateOutcome {
        message: format!(
            "Kalibrierung abgeschlossen. Der vollstaendige Bericht mit der Empfehlung fuer die \
             Latenzkompensation dieses Tracks steht in der Logdatei: {path}"
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
    last_emit: Instant,
    last_refusal: (u64, Refusal),
    message: Option<String>,
    stopped: bool,
}

impl Session {
    fn start(config: StartConfig) -> Result<Session, String> {
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
                "Ein- und Ausgang laufen auf verschiedenen Sampleraten ({} und {}). \
                 Die Zeitachse der Engine setzt eine gemeinsame Clock voraus.",
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
        let tracks: Vec<Track> = defs
            .iter()
            .map(|d| {
                Track::new(d.input, config.monitor, d.pan, rate)
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
            move |data: &mut [f32], _: &OutputCallbackInfo, _offset: usize| {
                let t0 = Instant::now();
                let frames = data.len() / out_channels;
                let mut done = 0usize;
                while done < frames {
                    let n = (frames - done).min(scratch_frames);
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
            .map_err(|e| format!("Eingangsstream startet nicht: {e}"))?;
        output
            .play()
            .map_err(|e| format!("Ausgabestream startet nicht: {e}"))?;

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
                .map(|d| TrackConfig {
                    input_channel_right: match d.input {
                        TrackInput::Stereo { right, .. } => Some(right as u32 + 1),
                        TrackInput::Mono(_) => None,
                    },
                    pan: d.pan,
                    latency_frames: d.latency.measured,
                    latency_trim: d.latency.trim,
                    ..TrackConfig::mono(&d.name, d.input.first() as u32 + 1)
                })
                .collect(),
        };

        Ok(Session {
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
            last_emit: Instant::now() - EMIT_INTERVAL,
            last_refusal: (0, Refusal::None),
            message: None,
            stopped: false,
        })
    }

    /// Stop the streams and give every buffer back.
    ///
    /// Dropping the streams stops the driver callbacks and, with them, drops the engine core: its
    /// layers, its stock of prepared buffers and both ends of the input FIFO. All of that happens
    /// on this thread, which is allowed to free memory. `pool.drain()` then empties the return
    /// queue and the reserve, so nothing survives into the next session.
    fn shutdown(mut self) {
        drop(self.input);
        drop(self.output);
        self.pool.drain();
    }

    /// The engine acknowledged a `Stop` command.
    fn finished(&self) -> bool {
        self.stopped
    }

    /// One round of control-thread duty: take the newest status, keep the buffer pool filled, and
    /// push an event if the rate allows.
    fn service(&mut self, app: &AppHandle) {
        if let Some(status) = self.status_rx.latest() {
            if status.stopped {
                self.stopped = true;
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
            if let Some(runner) = self.runner.as_mut() {
                runner.tick(status.pos);
            }
            self.pump_runner();
        }

        // The only place layer buffers are allocated, zeroed and dropped.
        self.pool.service(self.last.as_ref().map(|(s, _)| s));

        if self.last_emit.elapsed() >= EMIT_INTERVAL
            && let Some((status, _)) = self.last
        {
            self.last_emit = Instant::now();
            let _ = app.emit(STATUS_EVENT, self.event(&status));
        }
    }

    fn event(&self, status: &Status) -> StatusEvent {
        let rate = self.info.sample_rate.max(1);
        let stats = &self.stats;
        StatusEvent {
            running: true,
            pos: status.pos,
            seconds: status.pos as f64 / rate as f64,
            bar: status.bar + 1,
            beat: status.beat + 1,
            beat_offset: status.beat_offset,
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
            sample_rate: rate,
            latency_frames: self.info.latency_frames,
            click: status.click,
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
                    track_event(i, &ui.name, &ts, &ui.gains, rate, lead)
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
        if self.score_is_playing() {
            return Err(
                "Es laeuft schon eine Partitur. Erst \"Alles stoppen\" - ein Wechsel mitten im Lauf \
                 wuerde die schon armierten Kommandos im Audio-Thread stehen lassen, die keiner \
                 mehr zurueckholen kann."
                    .to_string(),
            );
        }
        let score = compile_score(yaml).map_err(|error| render_issues(&error))?;

        // The layout check comes from the engine library and carries its own German instructions.
        check_tracks(&score, &self.defs)?;

        if let Some(busy) = self.first_non_empty_track() {
            return Err(format!(
                "Track \"{busy}\" ist nicht leer. Eine Partitur wird nur auf eine leere Sitzung \
                 geladen - der Runner plant jeden Overdub gegen die Loop-Geometrie der Takes, die \
                 er selbst geschickt hat, und einen von Hand aufgenommenen Loop sieht er nicht. \
                 Erst \"Alles leeren\", dann laden."
            ));
        }

        let bars = score.sections.iter().map(|s| s.bars).max().unwrap_or(1).max(1);
        let signature = score.signature();
        self.reconfigure(score.bpm, signature.beats_per_bar, signature.beat_unit, bars)?;

        // Monitoring belongs to the runner from now on (`hear_through` and the takes switch it,
        // `monitor:` in the score says which tracks may have it at all). The runner's picture starts
        // at "everything off", so the engine is put into that state rather than left wherever the
        // setup screen happened to leave it - otherwise a track started with monitoring on would
        // stay on for the whole score, because the runner would never see a difference to send.
        for track in 0..self.tracks.len() {
            self.cmd.send(Command::SetMonitor { track, on: false })?;
        }

        let count_in = count_in.unwrap_or(DEFAULT_COUNT_IN_BARS);
        let title = score.title.clone();
        let sections = score.sections.len();
        let runner = Runner::new(score.clone(), self.scheduler.timeline, self.info.buffer_frames)?
            .with_count_in(count_in);
        self.runner = Some(runner);

        let message = format!(
            "Partitur \"{title}\" geladen: {sections} Sektionen, {:.1} BPM, {}, laengste Sektion \
             {bars} Takte, {count_in} Takt{} Einzaehler.",
            score.bpm,
            score.time_signature,
            if count_in == 1 { "" } else { "e" },
        );
        self.message = Some(message.clone());
        Ok(ScoreLoaded { score, message })
    }

    /// One press of the score transport. The answer is always a German sentence - see
    /// [`ScoreAction`].
    fn score_act(&mut self, action: ScoreAction) -> Result<String, String> {
        let est = self.estimated_pos();
        let Some(runner) = self.runner.as_mut() else {
            return Err(
                "Es ist keine Partitur geladen. Erst im Reiter \"Partitur\" eine laden.".to_string(),
            );
        };
        let message = match action {
            ScoreAction::Start => runner.start(est),
            ScoreAction::Next => runner.next(est),
            ScoreAction::Goto(index) => runner.goto(index, est),
            ScoreAction::StopAll => runner.stop_all(est),
        };
        self.pump_runner();
        self.message = Some(message.clone());
        Ok(message)
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
                "{} Gemeint war Track {}, vorhanden sind 1 bis {}.",
                Refusal::NoSuchTarget
                    .message()
                    .unwrap_or("Unbekanntes Ziel."),
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
                    "{} \"{name}\" hat keine Ebenen.",
                    Refusal::NoSuchTarget
                        .message()
                        .unwrap_or("Unbekanntes Ziel.")
                )
            } else {
                format!(
                    "{} \"{name}\" hat die Ebenen 1 bis {}.",
                    Refusal::NoSuchTarget
                        .message()
                        .unwrap_or("Unbekanntes Ziel."),
                    ts.layers
                )
            });
        }
        // Layers cannot be edited while that track records - the engine refuses it too, this only
        // says so immediately instead of one status snapshot later.
        if matches!(ts.state, TrackState::Recording | TrackState::Overdub) {
            return Err(Refusal::Busy
                .message()
                .unwrap_or("Track ist beschaeftigt.")
                .to_string());
        }
        Ok(())
    }

    fn send_all(&mut self, plan: Scheduled) -> Result<(), String> {
        for command in plan.commands {
            self.cmd.send(command)?;
        }
        self.message = Some(plan.message);
        Ok(())
    }

    fn apply(&mut self, action: Action) -> Result<(), String> {
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
            Action::Record { track } => {
                let name = self.tracks[track].name.clone();
                let plan = self.scheduler.record(track, &name, est);
                self.tracks[track].gains.clear();
                self.send_all(plan)
            }
            Action::Overdub { track } => {
                let ts = self.status_of(track);
                let name = self.tracks[track].name.clone();
                // `origin` and `loop_len` are this track's own grid; a further layer has to start
                // on it, not on the global one. See looper_engine::engine::schedule.
                let plan =
                    self.scheduler
                        .overdub(track, &name, est, ts.origin, ts.loop_len, ts.layers);
                self.send_all(plan)
            }
            Action::StopTrack { track } => {
                let ts = self.status_of(track);
                let name = self.tracks[track].name.clone();
                let plan = self.scheduler.stop(track, &name, est, ts.state);
                self.send_all(plan)
            }
            Action::Play { track } => {
                let name = self.tracks[track].name.clone();
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
                let plan = self.scheduler.clear_all(est);
                for ui in self.tracks.iter_mut() {
                    ui.gains.clear();
                }
                self.send_all(plan)
            }
            Action::SetMonitor { track, on } => {
                self.cmd.send(Command::SetMonitor { track, on })?;
                self.message = Some(format!(
                    "\"{}\": Mithoeren {}.",
                    self.tracks[track].name,
                    if on { "an" } else { "aus" }
                ));
                Ok(())
            }
            Action::SetPan { track, pan } => {
                if !(-1.0..=1.0).contains(&pan) {
                    return Err(
                        "Das Panorama muss zwischen -1 (ganz links) und 1 (ganz rechts) liegen."
                            .to_string(),
                    );
                }
                self.cmd.send(Command::SetPan { track, pan })?;
                // No message: a knob being turned is not news, and a sentence per mouse move would
                // make the status line flicker. The new value comes back in the status.
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
                        "Die Bus-Lautstaerke muss zwischen 0.0 und {MAX_BUS_GAIN} liegen."
                    ));
                }
                self.cmd.send(Command::SetBusGain { bus, gain })?;
                // No message, for the same reason a pan produces none: a fader being ridden is not
                // news, and the new value comes back in the status anyway.
                Ok(())
            }
            Action::SetBusOutput { bus, out } => {
                if out.width == 0 || out.width > 2 {
                    return Err("Ein Bus liegt auf einem Kanal oder auf einem Paar.".to_string());
                }
                self.cmd.send(Command::SetBusOutput { bus, out })?;
                self.message = Some(format!(
                    "{} liegt auf Ausgang {}.{}",
                    bus.label(),
                    out.label(),
                    if out.first + out.width > self.output_channels {
                        " Das Geraet hat so viele Kanaele nicht - der Bus landet auf dem naechsten                          Paar, das es gibt."
                    } else {
                        ""
                    }
                ));
                Ok(())
            }
            Action::SetTrackLatency { track, latency } => self.set_track_latency(track, latency),
            Action::SetClick { on } => {
                self.cmd.send(Command::SetClick { on })?;
                self.message = Some(format!("Klick {}.", if on { "an" } else { "aus" }));
                Ok(())
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
                    "\"{}\": Ebene {} {}.",
                    self.tracks[track].name,
                    layer + 1,
                    if muted { "stumm" } else { "wieder hoerbar" }
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
                    "\"{}\": Ebene {} entfernt.",
                    self.tracks[track].name,
                    layer + 1
                ));
                Ok(())
            }
            Action::LayerGain { track, layer, gain } => {
                self.check_layer(track, layer)?;
                if !(0.0..=4.0).contains(&gain) {
                    return Err("Die Lautstaerke muss zwischen 0.0 und 4.0 liegen.".to_string());
                }
                self.cmd.send(Command::SetLayerGain { track, layer, gain })?;
                let gains = &mut self.tracks[track].gains;
                while gains.len() <= layer {
                    gains.push(1.0);
                }
                gains[layer] = gain;
                self.message = Some(format!(
                    "\"{}\": Ebene {} auf {gain:.2}.",
                    self.tracks[track].name,
                    layer + 1
                ));
                Ok(())
            }
            Action::SetTempo {
                bpm,
                beats_per_bar,
                beat_unit,
            } => self.set_tempo(bpm, beats_per_bar, beat_unit),
            Action::SetQuantize { quantize } => {
                self.message = Some(self.scheduler.set_quantize(quantize));
                Ok(())
            }
            Action::FxBypass { track, on } => {
                self.cmd.send(Command::SetFxBypass { track, on })?;
                self.message = Some(format!(
                    "\"{}\": Effektkette {}.",
                    self.tracks[track].name,
                    if on { "umgangen" } else { "aktiv" }
                ));
                Ok(())
            }
            Action::FxEnable { track, slot, on } => {
                self.cmd.send(Command::SetFxEnabled { track, slot, on })?;
                self.message = Some(format!(
                    "\"{}\": {} {}.",
                    self.tracks[track].name,
                    slot.label(),
                    if on { "an" } else { "aus" }
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
                        "\"eigen\" ist kein Preset zum Laden - so heisst eine Kette, an der von                          Hand gedreht wurde."
                            .to_string(),
                    );
                }
                self.cmd.send(Command::LoadFxPreset { track, preset })?;
                self.message = Some(format!(
                    "\"{}\": Preset \"{}\" geladen.",
                    self.tracks[track].name,
                    preset.label()
                ));
                Ok(())
            }
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
                "Die Latenzkompensation muss innerhalb von {MAX_LATENCY_FRAMES} Frames liegen \
                 (zwei Sekunden bei 48 kHz)."
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
            "\"{}\": Latenzkompensation {effective} Frames ({:.2} ms){}. Wirkt auf kommende \
             Aufnahmen, nicht auf schon Aufgenommenes.",
            self.tracks[track].name,
            effective as f64 * 1000.0 / rate,
            if latency.inherits() {
                format!(" - globale Vorgabe {}", self.info.latency_frames)
            } else {
                String::new()
            },
        ));
        Ok(())
    }

    /// A new tempo redefines what every sample position means, so it is only accepted while every
    /// track is empty. The German sentence for the refusal is the engine's own.
    fn set_tempo(&mut self, bpm: f64, beats_per_bar: u32, beat_unit: u32) -> Result<(), String> {
        self.reconfigure(bpm, beats_per_bar, beat_unit, self.scheduler.bars)?;
        self.message = Some(format!("Tempo {bpm} BPM, {beats_per_bar}/{beat_unit}."));
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
                .unwrap_or("Tempo abgelehnt.")
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
                beat_unit: 4
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
    fn an_unknown_host_is_refused_in_german_before_any_device_is_opened() {
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
        assert!(err.contains("Verfuegbar"), "die Meldung zaehlt die Hosts auf: {err}");
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
            (
                Target::Tempo,
                Action::SetTempo {
                    bpm: 120.0,
                    beats_per_bar: 4,
                    beat_unit: 4,
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

    /// The load command has nowhere to put markers, so it says the same thing as one German block -
    /// with the positions in it, so it can be pasted back into the editor.
    #[test]
    fn the_load_command_renders_every_complaint_into_one_german_block() {
        let error = compile_score("bpm: 100\n").expect_err("ohne tracks und sections");
        let text = render_issues(&error);
        assert!(text.starts_with("Die Partitur laesst sich nicht uebersetzen"), "{text}");
        assert_eq!(
            text.lines().count(),
            1 + error.issues.len(),
            "Kopfzeile plus eine Zeile je Fehler: {text}"
        );
    }

    /// The one fact about the build that decides whether the app is usable at all.
    #[test]
    fn the_device_survey_reports_whether_asio_is_built_in() {
        assert_eq!(app_info().asio_built, cfg!(feature = "asio"));
        assert_eq!(app_info().max_tracks, MAX_TRACKS as u32);
        assert_eq!(app_info().max_layers, MAX_LAYERS as u32);
    }
}
