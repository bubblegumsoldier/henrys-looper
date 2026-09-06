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
use looper_engine::engine::calibrate::{CalibrateOpts, cmd_calibrate};
use looper_engine::engine::command::{
    Command, CommandSender, LayerPool, MAX_TRACKS, Refusal, Status, StatusReceiver, TrackStatus,
    buffer_channel, command_channel, status_channel,
};
use looper_engine::engine::frame::{Channels, TrackInput};
use looper_engine::engine::fx::{FxParam, FxPreset, FxSlot};
use looper_engine::engine::live::MAX_LATENCY_FRAMES;
use looper_engine::engine::process::{
    EngineConfig, EngineCore, OUT_CHANNELS, loop_capacity, max_latency, max_memory_bytes,
    spare_channels, spare_slots_for, spread_frame, total_channels,
};
use looper_engine::engine::timeline::Timeline;
use looper_engine::engine::track::{MAX_LAYERS, Track, TrackLatency, TrackState};

use crate::logfile::{self, log};
use crate::proto::{
    AppInfo, CalibrateConfig, CalibrateOutcome, ConfigInfo, DeviceInfo, DeviceReport, EngineInfo,
    HostInfo, QuantizeName, StartConfig, StatusEvent, TrackConfig, dbfs, track_event,
};
use crate::schedule::{Quantize, Scheduled, Scheduler, build_timeline, resolve_tracks};

/// Event the status snapshot is pushed on.
pub const STATUS_EVENT: &str = "looper://status";
/// Event carrying [`AppInfo`], emitted once at startup.
pub const READY_EVENT: &str = "looper://ready";

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

impl Action {
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
    Calibrate(Box<CalibrateConfig>, Sender<Result<CalibrateOutcome, String>>),
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

    pub fn calibrate(&self, config: CalibrateConfig) -> Result<CalibrateOutcome, String> {
        self.call(|reply| Request::Calibrate(Box::new(config), reply))
    }
}

// ---------------------------------------------------------------------------------------------
// The thread
// ---------------------------------------------------------------------------------------------

fn host_thread(rx: Receiver<Request>, app: AppHandle) {
    log!("Audio-Thread bereit (Engine laeuft noch nicht).");
    let mut session: Option<Session> = None;

    loop {
        let tick = if session.is_some() {
            RUNNING_TICK
        } else {
            IDLE_TICK
        };
        match rx.recv_timeout(tick) {
            Ok(request) => handle(request, &mut session, &app),
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
    }

    // The handle was dropped, so the app is going down. Give the device back properly anyway.
    stop_session(&mut session, &app);
    log!("Audio-Thread beendet.");
}

fn handle(request: Request, session: &mut Option<Session>, app: &AppHandle) {
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
                None => Err("Die Engine laeuft nicht. Erst starten.".to_string()),
            };
            if let Err(e) = &result {
                log!("Kommando abgelehnt: {e}");
            }
            let _ = reply.send(result);
        }
        Request::Calibrate(config, reply) => {
            let _ = reply.send(run_calibration(*config, session.is_some()));
        }
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

        let tracks: Vec<Track> = defs
            .iter()
            .map(|d| Track::new(d.input, config.monitor, d.pan, rate).with_latency(d.latency))
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
        let mut out_scratch = vec![0.0f32; scratch_frames * OUT_CHANNELS];
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
                        &mut out_scratch[..n * OUT_CHANNELS],
                    );
                    let base = done * out_channels;
                    for (i, frame) in data[base..base + n * out_channels]
                        .chunks_exact_mut(out_channels)
                        .enumerate()
                    {
                        let bus = &out_scratch[i * OUT_CHANNELS..(i + 1) * OUT_CHANNELS];
                        spread_frame(bus, frame);
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
            output_peaks: status.output_peak.to_vec(),
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
            ignored_commands: status.ignored_commands,
            spares: status.spares_total(),
            message: self.message.clone(),
        }
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
        let timeline = build_timeline(
            self.info.sample_rate,
            bpm,
            beats_per_bar,
            beat_unit,
            self.scheduler.bars,
            max_latency(
                self.info.latency_frames,
                self.tracks.iter().map(|ui| ui.latency),
            ),
        )?;
        let layer_frames = loop_capacity(&timeline, self.scheduler.bars);
        self.cmd.send(Command::SetTempo {
            bpm,
            signature: timeline.signature(),
            layer_frames,
        })?;
        // Every allocation at the new layer length happens on this thread; the old stock is
        // dropped here and the engine hands back what it still holds.
        self.pool.set_layer_frames(layer_frames as usize);
        self.scheduler.timeline = timeline;
        self.update_timeline_info(&timeline, layer_frames);
        self.message = Some(format!("Tempo {bpm} BPM, {beats_per_bar}/{beat_unit}."));
        Ok(())
    }

    fn update_timeline_info(&mut self, timeline: &Timeline, layer_frames: u64) {
        let loop_samples = timeline.span_bars(0, self.scheduler.bars);
        self.info.bpm = timeline.bpm();
        self.info.beats_per_bar = timeline.signature().beats_per_bar;
        self.info.beat_unit = timeline.signature().beat_unit;
        self.info.samples_per_beat = timeline.samples_per_beat();
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

    /// The one fact about the build that decides whether the app is usable at all.
    #[test]
    fn the_device_survey_reports_whether_asio_is_built_in() {
        assert_eq!(app_info().asio_built, cfg!(feature = "asio"));
        assert_eq!(app_info().max_tracks, MAX_TRACKS as u32);
        assert_eq!(app_info().max_layers, MAX_LAYERS as u32);
    }
}
