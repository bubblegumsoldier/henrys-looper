//! When the engine runs, decided without a device: the app starts it by itself.
//!
//! The engine - an open audio device, a real-time thread, preallocated tracks, running plugins -
//! is a technical detail. The musician opens the app and plays; this module decides when the host
//! thread opens the device, and with which one:
//!
//! * **Boot** - once the window has sent its setup ([`Supervisor::configure`]) and "Start the
//!   engine when the app opens" is on.
//! * **Demand** - something was pressed that needs the engine (Load, Start, Record, Monitor, a
//!   knob). The engine starts and the press is carried out afterwards, as if it had come a moment
//!   later. Every explicit press may try again, also after a failed start.
//! * **Passive** - a note or a pad arrived over MIDI. Like Demand, but never after a failure or
//!   while waiting for an interface: a keyboard that sends a note every second must not turn into
//!   a start loop.
//! * **Plug** - the interface the engine waits for has appeared.
//!
//! Two safety rules sit here as well, because they decide *before* anything can make a sound:
//!
//! * A missing interface is not replaced by whatever macOS offers when that would put the music on
//!   the laptop's speakers ([`choose_device`]). The app waits for it, says so, and offers the
//!   default device as an explicit choice.
//! * Built-in speakers and a live microphone feed back. [`output_is_speakers`] recognises them
//!   (CoreAudio's transport type, and the name as a fallback); the host then starts with the
//!   monitored inputs muted until the musician says otherwise.
//!
//! Everything here takes the device list as an argument, so the tests need no hardware.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::proto::{StartConfig, TrackConfig};

/// What asked for the engine.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Trigger {
    /// The app has just opened.
    Boot,
    /// A press that needs the engine: the musician wants it now.
    Demand,
    /// A note or a pad over MIDI.
    Passive,
    /// The awaited interface was plugged in.
    Plug,
}

/// One audio device as the decision needs it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DeviceFacts {
    pub name: String,
    pub input: bool,
    pub output: bool,
    /// Sound from this output is heard in the room - built-in speakers, a display's speakers.
    pub speakers: bool,
}

/// The devices present right now, and what the host would take for "default".
#[derive(Clone, Debug, Default)]
pub struct DeviceList {
    pub devices: Vec<DeviceFacts>,
    pub default_input: Option<String>,
    pub default_output: Option<String>,
}

impl DeviceList {
    fn find(&self, needle: &str) -> Option<&DeviceFacts> {
        let needle = needle.to_lowercase();
        self.devices
            .iter()
            .find(|d| d.name.to_lowercase().contains(&needle))
    }

    /// The output "default device" ends on: the default input itself when it can play (one
    /// device, one clock - `looper_engine::audio::open_duplex`), else the default output.
    pub fn default_target(&self) -> Option<&DeviceFacts> {
        let input = self
            .default_input
            .as_deref()
            .and_then(|n| self.devices.iter().find(|d| d.name == n));
        if let Some(i) = input.filter(|d| d.output) {
            return Some(i);
        }
        self.default_output
            .as_deref()
            .and_then(|n| self.devices.iter().find(|d| d.name == n))
    }
}

/// Which device to open.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DeviceChoice {
    /// Open this one (`None`: the host default).
    Open(Option<String>),
    /// The wanted interface is missing and the default would be the speakers: wait for it.
    Wait {
        missing: String,
        /// What "Use … now" would open.
        alternative: Option<String>,
    },
}

/// The device rule.
///
/// * A device named in the setup is opened when it is there and waited for when it is not - the
///   setup said which one, so no other one is right.
/// * "Default device" prefers the interface the engine last ran on (`remembered`). When that one
///   is gone and the default would end on speakers, the app waits for it instead of playing the
///   loops into the room; when the default is something else (headphones, another interface), it
///   takes the default.
/// * `fallback` is the musician's "Use … now": take the default, whatever it is.
pub fn choose_device(
    wanted: Option<&str>,
    remembered: Option<&str>,
    present: &DeviceList,
    fallback: bool,
) -> DeviceChoice {
    let alternative = present.default_target().map(|d| d.name.clone());
    match wanted {
        Some(needle) => {
            if present.find(needle).is_some() {
                DeviceChoice::Open(Some(needle.to_string()))
            } else if fallback {
                DeviceChoice::Open(None)
            } else {
                DeviceChoice::Wait {
                    missing: needle.to_string(),
                    alternative,
                }
            }
        }
        None => {
            let Some(last) = remembered else {
                return DeviceChoice::Open(None);
            };
            if present
                .devices
                .iter()
                .any(|d| d.name == last && d.input && d.output)
            {
                return DeviceChoice::Open(Some(last.to_string()));
            }
            let into_the_room = present.default_target().is_none_or(|d| d.speakers);
            if fallback || !into_the_room {
                DeviceChoice::Open(None)
            } else {
                DeviceChoice::Wait {
                    missing: last.to_string(),
                    alternative,
                }
            }
        }
    }
}

/// Whether an output with this name (and, where known, a CoreAudio built-in transport) plays into
/// the room. Headphones never do; built-in outputs and anything called a speaker do. In doubt -
/// a built-in output whose name says nothing - it counts as speakers: a needless warning costs a
/// click, feedback costs ears.
pub fn is_speaker_output(name: &str, built_in: Option<bool>) -> bool {
    let n = name.to_lowercase();
    const HEADPHONES: [&str; 5] = ["headphone", "kopfhörer", "kopfhoerer", "casque", "auricular"];
    if HEADPHONES.iter().any(|w| n.contains(w)) {
        return false;
    }
    if built_in == Some(true) {
        return true;
    }
    const SPEAKERS: [&str; 7] = [
        "speaker",
        "lautsprecher",
        "haut-parleur",
        "altavoces",
        "altoparlant",
        "built-in output",
        "integrierte ausgabe",
    ];
    SPEAKERS.iter().any(|w| n.contains(w))
}

/// [`is_speaker_output`] for a device of this machine, by name.
pub fn output_is_speakers(name: &str) -> bool {
    is_speaker_output(name, platform::built_in(name))
}

/// The device list of a host, for [`choose_device`]. Opens nothing.
pub fn survey(host: &str) -> DeviceList {
    use cpal::traits::{DeviceTrait, HostTrait};
    let Ok(host) = looper_engine::audio::open_host(host) else {
        return DeviceList::default();
    };
    let built = platform::built_in_all();
    let devices = host
        .devices()
        .map(|list| {
            list.map(|d| {
                let name = d.to_string();
                let built_in = built.iter().find(|(n, _)| *n == name).map(|(_, b)| *b);
                DeviceFacts {
                    speakers: is_speaker_output(&name, built_in),
                    input: d.supports_input(),
                    output: d.supports_output(),
                    name,
                }
            })
            .collect()
        })
        .unwrap_or_default();
    DeviceList {
        devices,
        default_input: host.default_input_device().map(|d| d.to_string()),
        default_output: host.default_output_device().map(|d| d.to_string()),
    }
}

/// Whether the configured tracks need a microphone or line input at all.
pub fn has_audio_tracks(tracks: &[TrackConfig]) -> bool {
    tracks.iter().any(|t| t.instrument.is_none())
}

// ---------------------------------------------------------------------------------------------
// Whether a restart is needed
// ---------------------------------------------------------------------------------------------

/// Whether `wanted` differs from what the running engine was started with in something only a new
/// start can change: the device and its clock, and - while no score has put its own tracks in -
/// the track list with its inputs and instruments. Tempo, click, levels, pans and latencies change
/// in the running engine and never need one.
pub fn needs_restart(running: &StartConfig, wanted: &StartConfig, score_loaded: bool) -> bool {
    let device = running.host != wanted.host
        || running.device != wanted.device
        || running.sample_rate != wanted.sample_rate
        || running.buffer_frames != wanted.buffer_frames
        || running.input_channels != wanted.input_channels
        || running.output_channels != wanted.output_channels
        || running.force_buffer != wanted.force_buffer;
    if device || score_loaded {
        return device;
    }
    let shape = |t: &TrackConfig| {
        (
            t.name.trim().to_string(),
            t.instrument.is_none().then_some(t.input_channel),
            t.instrument.is_none().then_some(t.input_channel_right).flatten(),
            t.instrument.as_ref().map(|i| {
                (
                    i.device.clone(),
                    i.channel,
                    i.note_low,
                    i.note_high,
                    i.monitor,
                    i.loop_kind.clone(),
                )
            }),
        )
    };
    running.tracks.len() != wanted.tracks.len()
        || running
            .tracks
            .iter()
            .zip(&wanted.tracks)
            .any(|(a, b)| shape(a) != shape(b))
}

// ---------------------------------------------------------------------------------------------
// The state machine
// ---------------------------------------------------------------------------------------------

/// What [`Supervisor::plan`] decided.
#[derive(Clone, Debug, PartialEq)]
pub enum Plan {
    /// Open the device with this configuration (the setup's, with the device resolved).
    Open(Box<StartConfig>),
    /// Do nothing and say nothing.
    Quiet,
    /// Do nothing; the press that asked gets this sentence.
    Refuse(String),
}

#[derive(Clone, Debug, PartialEq)]
struct Failure {
    reason: String,
    permission: bool,
}

#[derive(Clone, Debug, PartialEq)]
struct Waiting {
    missing: String,
    alternative: Option<String>,
}

/// The engine's situation as the window shows it (`looper://engine-state`).
#[derive(Serialize, Clone, Debug, PartialEq)]
#[serde(rename_all = "snake_case")]
pub struct EngineStateView {
    /// `running`, `starting`, `off` (not started yet, starts when needed), `stopped` (stopped by
    /// hand), `waiting` (for an interface) or `failed`.
    pub phase: &'static str,
    /// The sentence for the phase - the reason of a failure, what is awaited.
    pub message: Option<String>,
    /// The awaited device.
    pub missing: Option<String>,
    /// What "Use … now" would open instead.
    pub alternative: Option<String>,
    /// The failure is the missing microphone permission.
    pub permission: bool,
    /// The setup changed in something only a restart applies.
    pub restart_needed: bool,
    /// "Start the engine when the app opens".
    pub autostart: bool,
}

/// The sentence for a refused microphone.
pub const NO_MIC_PERMISSION: &str = "macOS does not let Henrys Looper use the audio input. Allow it in \
     System Settings › Privacy & Security › Microphone, then press Retry.";

#[derive(Default)]
pub struct Supervisor {
    /// The setup the window holds, sent with every change.
    desired: Option<StartConfig>,
    autostart: bool,
    /// The boot start has been decided (it happens once per run of the app).
    booted: bool,
    /// "Stop engine" was pressed: no start of its own until something asks for one.
    user_stopped: bool,
    failure: Option<Failure>,
    waiting: Option<Waiting>,
    /// "Use … now": the next start takes the default device.
    fallback: bool,
    /// The app is going down; nothing starts any more.
    quitting: bool,
    /// The interface the engine last ran on, by its full name.
    remembered: Option<String>,
    /// The setup the running engine was started from (device not yet resolved).
    started_from: Option<StartConfig>,
    restart_needed: bool,
    /// "Unmute anyway" was pressed: built-in speakers no longer mute the inputs (this run only).
    pub monitor_consent: bool,
    /// Where [`Supervisor::remembered`] is kept; `None` in tests.
    store: Option<PathBuf>,
    /// What the window was last told (the host emits only changes).
    pub shown: Option<EngineStateView>,
    /// The setup with the tracks of the score that is about to be loaded, for the resting stage.
    preview: Option<StartConfig>,
}

impl Supervisor {
    /// With the remembered device read from `store`.
    pub fn with_store(store: PathBuf) -> Self {
        Self {
            remembered: load_remembered(&store),
            store: Some(store),
            ..Self::default()
        }
    }

    pub fn desired(&self) -> Option<&StartConfig> {
        self.desired.as_ref()
    }

    pub fn preview(&self) -> Option<&StartConfig> {
        self.preview.as_ref()
    }

    pub fn set_preview(&mut self, preview: Option<StartConfig>) {
        self.preview = preview;
    }

    pub fn is_waiting(&self) -> bool {
        self.waiting.is_some()
    }

    /// The window's setup arrived (it sends it on every change).
    /// `score_loaded` is `None` while no engine runs.
    pub fn configure(&mut self, config: StartConfig, autostart: bool, score_loaded: Option<bool>) {
        self.autostart = autostart;
        self.desired = Some(config);
        self.refresh_restart(score_loaded);
    }

    /// Recompute [`EngineStateView::restart_needed`].
    pub fn refresh_restart(&mut self, score_loaded: Option<bool>) {
        // "Default device" means the remembered interface where there is one: switching between
        // the two spellings of the same device is no reason to reopen it.
        let resolved = |c: &StartConfig| {
            let mut c = c.clone();
            c.device = c.device.or_else(|| self.remembered.clone());
            c
        };
        self.restart_needed = match (&self.started_from, &self.desired, score_loaded) {
            (Some(from), Some(want), Some(score)) => needs_restart(&resolved(from), &resolved(want), score),
            _ => false,
        };
    }

    /// Decide whether `trigger` starts the engine, and with what. `present` is only looked at
    /// when the answer could be "start".
    pub fn plan(&mut self, trigger: Trigger, present: impl FnOnce() -> DeviceList) -> Plan {
        if self.quitting {
            return Plan::Quiet;
        }
        let Some(desired) = self.desired.clone() else {
            return match trigger {
                Trigger::Demand => Plan::Refuse(
                    "The window has not sent its setup yet - try again in a moment.".to_string(),
                ),
                _ => Plan::Quiet,
            };
        };
        match trigger {
            Trigger::Boot => {
                if self.booted || !self.autostart || self.user_stopped {
                    self.booted = true;
                    return Plan::Quiet;
                }
                self.booted = true;
            }
            Trigger::Plug if self.waiting.is_none() => return Plan::Quiet,
            Trigger::Passive if self.failure.is_some() || self.waiting.is_some() => {
                return Plan::Quiet;
            }
            _ => {}
        }
        if desired.tracks.is_empty() {
            return match trigger {
                Trigger::Demand => Plan::Refuse(
                    "No track is set up - add one in Setup › Tracks & Inputs.".to_string(),
                ),
                _ => Plan::Quiet,
            };
        }
        let list = present();
        match choose_device(
            desired.device.as_deref(),
            self.remembered.as_deref(),
            &list,
            self.fallback,
        ) {
            DeviceChoice::Open(device) => {
                let mut config = desired;
                config.device = device;
                Plan::Open(Box::new(config))
            }
            DeviceChoice::Wait {
                missing,
                alternative,
            } => {
                let text = waiting_text(&missing, alternative.as_deref());
                self.waiting = Some(Waiting {
                    missing,
                    alternative,
                });
                match trigger {
                    Trigger::Demand => Plan::Refuse(text),
                    _ => Plan::Quiet,
                }
            }
        }
    }

    /// The engine is running on `output` (the full device name), started from the window's setup.
    /// `interface`: input and output are the same device, and it is not a built-in one - the
    /// thing worth preferring next time.
    pub fn started(&mut self, device: &str, interface: bool) {
        self.failure = None;
        self.waiting = None;
        self.user_stopped = false;
        self.fallback = false;
        self.started_from = self.desired.clone();
        self.restart_needed = false;
        if interface && self.remembered.as_deref() != Some(device) {
            self.remembered = Some(device.to_string());
            if let Some(store) = &self.store {
                save_remembered(store, device);
            }
        }
    }

    /// The start did not work. One attempt; nothing retries by itself afterwards.
    pub fn failed(&mut self, reason: String, permission: bool) {
        self.failure = Some(Failure { reason, permission });
        self.started_from = None;
    }

    /// "Stop engine" was pressed.
    pub fn stopped_by_user(&mut self) {
        self.user_stopped = true;
        self.started_from = None;
        self.restart_needed = false;
    }

    /// The engine went down for another reason (it ended by itself, a rebuild failed).
    pub fn lost(&mut self) {
        self.started_from = None;
        self.restart_needed = false;
    }

    /// "Use … now".
    pub fn use_fallback(&mut self) {
        self.fallback = true;
        self.waiting = None;
    }

    /// The way out: from here on nothing starts.
    pub fn quit(&mut self) {
        self.quitting = true;
    }

    pub fn view(&self, running: bool, starting: bool) -> EngineStateView {
        let mut view = EngineStateView {
            phase: "off",
            message: None,
            missing: None,
            alternative: None,
            permission: false,
            restart_needed: running && self.restart_needed,
            autostart: self.autostart,
        };
        if running {
            view.phase = "running";
        } else if starting {
            view.phase = "starting";
        } else if let Some(w) = &self.waiting {
            view.phase = "waiting";
            view.message = Some(waiting_text(&w.missing, w.alternative.as_deref()));
            view.missing = Some(w.missing.clone());
            view.alternative = w.alternative.clone();
        } else if let Some(f) = &self.failure {
            view.phase = "failed";
            view.message = Some(f.reason.clone());
            view.permission = f.permission;
        } else if self.user_stopped {
            view.phase = "stopped";
        }
        view
    }
}

fn waiting_text(missing: &str, alternative: Option<&str>) -> String {
    match alternative {
        Some(alt) => format!(
            "{missing} is not connected. The engine starts as soon as it is plugged in - or use \
             {alt} now."
        ),
        None => format!("{missing} is not connected. The engine starts as soon as it is plugged in."),
    }
}

/// What a start opened, for [`Supervisor::started`].
pub struct Opened {
    /// The output device's full name.
    pub device: String,
    /// Input and output are one device and it is not built in: an interface worth preferring.
    pub interface: bool,
}

/// The sentence for a press that found no engine and may not start one (on the way out).
pub const NOT_RUNNING: &str = "The engine is not running.";

/// Make sure `engine` holds a running engine: when it does not, ask the supervisor whether
/// `trigger` may start one, and let `open` start it with the planned configuration. One attempt;
/// a failure is recorded with its reason and nothing retries it by itself.
///
/// Generic over the engine so the order of things - plan, open, record, then the press - is
/// provable without a device. `open` gets the supervisor to read, e.g. for the monitor consent.
pub fn bring_up<S>(
    engine: &mut Option<S>,
    sup: &mut Supervisor,
    trigger: Trigger,
    present: impl FnOnce() -> DeviceList,
    open: impl FnOnce(StartConfig, &Supervisor) -> Result<(S, Opened), String>,
) -> Result<(), String> {
    if engine.is_some() {
        return Ok(());
    }
    let config = match sup.plan(trigger, present) {
        Plan::Open(config) => *config,
        Plan::Quiet => return Err(NOT_RUNNING.to_string()),
        Plan::Refuse(text) => return Err(text),
    };
    match open(config, sup) {
        Ok((started, opened)) => {
            sup.started(&opened.device, opened.interface);
            *engine = Some(started);
            Ok(())
        }
        Err(e) if e == NO_MIC_PERMISSION => {
            sup.failed(e.clone(), true);
            Err(e)
        }
        Err(e) => {
            let text = format!("Engine could not start: {e}");
            sup.failed(text.clone(), false);
            Err(text)
        }
    }
}

/// Bring the engine up (`bring`: [`bring_up`] with what the host needs around it), then carry
/// out the press on it - the way every command reaches an engine that may not run yet. The press
/// runs only on an engine that is there; a start that did not happen is the answer instead.
pub fn then_act<S, T>(
    engine: &mut Option<S>,
    bring: impl FnOnce(&mut Option<S>) -> Result<(), String>,
    act: impl FnOnce(&mut S) -> Result<T, String>,
) -> Result<T, String> {
    bring(engine)?;
    match engine.as_mut() {
        Some(running) => act(running),
        None => Err(NOT_RUNNING.to_string()),
    }
}

// ---------------------------------------------------------------------------------------------
// The remembered interface: `<config>/engine.json`
// ---------------------------------------------------------------------------------------------

#[derive(Serialize, Deserialize, Default)]
struct Stored {
    #[serde(default)]
    last_device: Option<String>,
}

/// `<config>/engine.json`, next to `mixer.json`. Follows `HENRYS_LOOPER_CONFIG_DIR`.
pub fn store_path() -> PathBuf {
    let midi = looper_engine::midi::profile::profile_dir();
    midi.parent().map(PathBuf::from).unwrap_or(midi).join("engine.json")
}

fn load_remembered(path: &Path) -> Option<String> {
    std::fs::read_to_string(path)
        .ok()
        .and_then(|t| serde_json::from_str::<Stored>(&t).ok())
        .and_then(|s| s.last_device)
}

fn save_remembered(path: &Path, device: &str) {
    let stored = Stored {
        last_device: Some(device.to_string()),
    };
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    if let Ok(text) = serde_json::to_string_pretty(&stored) {
        let _ = std::fs::write(path, text);
    }
}

// ---------------------------------------------------------------------------------------------
// The platform: CoreAudio's transport type, the microphone permission
// ---------------------------------------------------------------------------------------------

/// What macOS says about the use of the audio input.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MicPermission {
    Granted,
    /// Not asked yet: opening the input makes macOS ask.
    NotDetermined,
    Denied,
    Unknown,
}

pub fn mic_permission() -> MicPermission {
    platform::mic_permission()
}

#[cfg(target_os = "macos")]
mod platform {
    //! Two questions cpal does not answer: is an output built into the machine, and may this
    //! process use the microphone. Plain CoreAudio and one Objective-C message.

    use std::ffi::c_void;

    use super::MicPermission;

    #[repr(C)]
    struct Address {
        selector: u32,
        scope: u32,
        element: u32,
    }

    const fn code(c: &[u8; 4]) -> u32 {
        u32::from_be_bytes(*c)
    }
    const SYSTEM_OBJECT: u32 = 1;
    const DEVICES: u32 = code(b"dev#");
    const NAME: u32 = code(b"lnam");
    const TRANSPORT: u32 = code(b"tran");
    const GLOBAL: u32 = code(b"glob");
    const BUILT_IN: u32 = code(b"bltn");
    const UTF8: u32 = 0x0800_0100;

    #[link(name = "CoreAudio", kind = "framework")]
    unsafe extern "C" {
        fn AudioObjectGetPropertyDataSize(
            id: u32,
            address: *const Address,
            qualifier_size: u32,
            qualifier: *const c_void,
            size: *mut u32,
        ) -> i32;
        fn AudioObjectGetPropertyData(
            id: u32,
            address: *const Address,
            qualifier_size: u32,
            qualifier: *const c_void,
            size: *mut u32,
            data: *mut c_void,
        ) -> i32;
    }

    #[link(name = "CoreFoundation", kind = "framework")]
    unsafe extern "C" {
        fn CFStringGetLength(s: *const c_void) -> isize;
        fn CFStringGetCString(s: *const c_void, buffer: *mut u8, size: isize, encoding: u32) -> u8;
        fn CFRelease(p: *const c_void);
    }

    #[link(name = "AVFoundation", kind = "framework")]
    unsafe extern "C" {}

    fn address(selector: u32) -> Address {
        Address {
            selector,
            scope: GLOBAL,
            element: 0,
        }
    }

    fn device_ids() -> Vec<u32> {
        let addr = address(DEVICES);
        let mut size = 0u32;
        // SAFETY: plain CoreAudio property calls with buffers of the size CoreAudio reported.
        unsafe {
            if AudioObjectGetPropertyDataSize(SYSTEM_OBJECT, &addr, 0, std::ptr::null(), &mut size) != 0 {
                return Vec::new();
            }
            let mut ids = vec![0u32; size as usize / 4];
            if AudioObjectGetPropertyData(
                SYSTEM_OBJECT,
                &addr,
                0,
                std::ptr::null(),
                &mut size,
                ids.as_mut_ptr().cast(),
            ) != 0
            {
                return Vec::new();
            }
            ids.truncate(size as usize / 4);
            ids
        }
    }

    fn name_of(id: u32) -> Option<String> {
        let addr = address(NAME);
        let mut text: *const c_void = std::ptr::null();
        let mut size = std::mem::size_of::<*const c_void>() as u32;
        // SAFETY: the property is a CFStringRef we own afterwards and release below.
        unsafe {
            if AudioObjectGetPropertyData(id, &addr, 0, std::ptr::null(), &mut size, (&mut text as *mut *const c_void).cast()) != 0
                || text.is_null()
            {
                return None;
            }
            let room = CFStringGetLength(text) * 4 + 1;
            let mut buffer = vec![0u8; room as usize];
            let ok = CFStringGetCString(text, buffer.as_mut_ptr(), room, UTF8) != 0;
            CFRelease(text);
            if !ok {
                return None;
            }
            let end = buffer.iter().position(|&b| b == 0).unwrap_or(buffer.len());
            String::from_utf8(buffer[..end].to_vec()).ok()
        }
    }

    fn transport_of(id: u32) -> Option<u32> {
        let addr = address(TRANSPORT);
        let mut value = 0u32;
        let mut size = 4u32;
        // SAFETY: a u32 property into a u32.
        let status = unsafe {
            AudioObjectGetPropertyData(id, &addr, 0, std::ptr::null(), &mut size, (&mut value as *mut u32).cast())
        };
        (status == 0).then_some(value)
    }

    /// Every device by name, and whether its transport is "built-in".
    pub fn built_in_all() -> Vec<(String, bool)> {
        device_ids()
            .into_iter()
            .filter_map(|id| Some((name_of(id)?, transport_of(id)? == BUILT_IN)))
            .collect()
    }

    pub fn built_in(name: &str) -> Option<bool> {
        built_in_all().into_iter().find(|(n, _)| n == name).map(|(_, b)| b)
    }

    pub fn mic_permission() -> MicPermission {
        use objc2::runtime::AnyClass;
        use objc2_foundation::NSString;
        let Some(class) = AnyClass::get(c"AVCaptureDevice") else {
            return MicPermission::Unknown;
        };
        // AVMediaTypeAudio is the string "soun".
        let media = NSString::from_str("soun");
        // SAFETY: a class method taking an NSString and returning an NSInteger.
        let status: isize = unsafe { objc2::msg_send![class, authorizationStatusForMediaType: &*media] };
        match status {
            0 => MicPermission::NotDetermined,
            1 | 2 => MicPermission::Denied,
            3 => MicPermission::Granted,
            _ => MicPermission::Unknown,
        }
    }
}

#[cfg(not(target_os = "macos"))]
mod platform {
    use super::MicPermission;

    pub fn built_in_all() -> Vec<(String, bool)> {
        Vec::new()
    }

    pub fn built_in(_name: &str) -> Option<bool> {
        None
    }

    pub fn mic_permission() -> MicPermission {
        MicPermission::Unknown
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dev(name: &str, input: bool, output: bool, speakers: bool) -> DeviceFacts {
        DeviceFacts {
            name: name.to_string(),
            input,
            output,
            speakers,
        }
    }

    /// The situation at the owner's desk without the interface: the laptop's microphone and its
    /// speakers, nothing else.
    fn laptop_only() -> DeviceList {
        DeviceList {
            devices: vec![
                dev("MacBook Pro-Mikrofon", true, false, false),
                dev("MacBook Pro-Lautsprecher", false, true, true),
            ],
            default_input: Some("MacBook Pro-Mikrofon".into()),
            default_output: Some("MacBook Pro-Lautsprecher".into()),
        }
    }

    fn with_scarlett() -> DeviceList {
        let mut list = laptop_only();
        list.devices.push(dev("Scarlett 2i2 USB", true, true, false));
        list
    }

    fn config(device: Option<&str>) -> StartConfig {
        serde_json::from_value(serde_json::json!({
            "device": device,
            "tracks": [{ "name": "voice", "input_channel": 1 }]
        }))
        .expect("config")
    }

    fn configured(device: Option<&str>, autostart: bool) -> Supervisor {
        let mut s = Supervisor::default();
        s.configure(config(device), autostart, None);
        s
    }

    fn opened(plan: Plan) -> Option<String> {
        match plan {
            Plan::Open(c) => c.device,
            other => panic!("expected a start, got {other:?}"),
        }
    }

    #[test]
    fn built_in_speakers_are_recognised_by_transport_and_by_name() {
        assert!(is_speaker_output("MacBook Pro-Lautsprecher", Some(true)));
        assert!(is_speaker_output("MacBook Pro Speakers", None));
        assert!(is_speaker_output("Mac mini Speakers", Some(true)));
        assert!(is_speaker_output("LG UltraFine Display Audio Speakers", Some(false)));
        // Built in, name says nothing: in doubt it counts as speakers.
        assert!(is_speaker_output("Integrated Output", Some(true)));
        // Headphones never feed back, built in or not.
        assert!(!is_speaker_output("Externe Kopfhörer", Some(true)));
        assert!(!is_speaker_output("External Headphones", Some(true)));
        assert!(!is_speaker_output("Scarlett 2i2 USB", Some(false)));
        assert!(!is_speaker_output("Scarlett 2i2 USB", None));
    }

    #[test]
    fn a_named_interface_that_is_missing_is_waited_for_not_replaced() {
        let choice = choose_device(Some("Scarlett"), None, &laptop_only(), false);
        assert_eq!(
            choice,
            DeviceChoice::Wait {
                missing: "Scarlett".into(),
                alternative: Some("MacBook Pro-Lautsprecher".into()),
            }
        );
        assert_eq!(
            choose_device(Some("Scarlett"), None, &with_scarlett(), false),
            DeviceChoice::Open(Some("Scarlett".into()))
        );
        // "Use … now" is the musician's choice.
        assert_eq!(choose_device(Some("Scarlett"), None, &laptop_only(), true), DeviceChoice::Open(None));
    }

    #[test]
    fn the_default_device_prefers_the_interface_it_last_ran_on() {
        let last = Some("Scarlett 2i2 USB");
        assert_eq!(
            choose_device(None, last, &with_scarlett(), false),
            DeviceChoice::Open(Some("Scarlett 2i2 USB".into())),
            "present: taken by name, whatever macOS's default is"
        );
        assert!(
            matches!(choose_device(None, last, &laptop_only(), false), DeviceChoice::Wait { .. }),
            "gone, and the default would be the laptop speakers: wait"
        );
        // Gone, but the default plays into headphones: no reason to wait.
        let mut phones = laptop_only();
        phones.devices.push(dev("Externe Kopfhörer", false, true, false));
        phones.default_output = Some("Externe Kopfhörer".into());
        assert_eq!(choose_device(None, last, &phones, false), DeviceChoice::Open(None));
        // Nothing remembered yet (first run): the default, with the monitor guard behind it.
        assert_eq!(choose_device(None, None, &laptop_only(), false), DeviceChoice::Open(None));
    }

    #[test]
    fn the_app_opens_and_starts_by_itself_once() {
        let mut s = configured(None, true);
        assert_eq!(opened(s.plan(Trigger::Boot, laptop_only)), None);
        s.started("MacBook Pro-Lautsprecher", false);
        assert_eq!(s.view(true, false).phase, "running");
        assert_eq!(s.plan(Trigger::Boot, laptop_only), Plan::Quiet, "only once per run");
        assert!(s.remembered.is_none(), "the laptop itself is never remembered as the interface");
    }

    #[test]
    fn without_autostart_the_engine_waits_for_the_first_press() {
        let mut s = configured(None, false);
        assert_eq!(s.plan(Trigger::Boot, laptop_only), Plan::Quiet);
        assert_eq!(s.view(false, false).phase, "off");
        assert!(matches!(s.plan(Trigger::Demand, laptop_only), Plan::Open(_)));
        assert!(matches!(s.plan(Trigger::Passive, laptop_only), Plan::Open(_)));
    }

    #[test]
    fn stopped_by_hand_means_no_start_of_its_own_but_a_press_starts_it() {
        let mut s = configured(None, true);
        assert!(matches!(s.plan(Trigger::Boot, laptop_only), Plan::Open(_)));
        s.started("Scarlett 2i2 USB", true);
        s.stopped_by_user();
        assert_eq!(s.view(false, false).phase, "stopped");
        assert_eq!(s.plan(Trigger::Boot, laptop_only), Plan::Quiet);
        assert_eq!(s.plan(Trigger::Plug, with_scarlett), Plan::Quiet);
        assert!(matches!(s.plan(Trigger::Demand, with_scarlett), Plan::Open(_)));
    }

    #[test]
    fn a_failed_start_is_tried_once_and_then_only_on_a_press() {
        let mut s = configured(None, true);
        assert!(matches!(s.plan(Trigger::Boot, laptop_only), Plan::Open(_)));
        s.failed("The device refused 48 kHz.".into(), false);
        let view = s.view(false, false);
        assert_eq!((view.phase, view.message.as_deref()), ("failed", Some("The device refused 48 kHz.")));
        // Notes from a keyboard do not hammer the device.
        for _ in 0..5 {
            assert_eq!(s.plan(Trigger::Passive, laptop_only), Plan::Quiet);
        }
        assert_eq!(s.plan(Trigger::Boot, laptop_only), Plan::Quiet);
        // Retry, Load, Record: one more attempt each.
        assert!(matches!(s.plan(Trigger::Demand, laptop_only), Plan::Open(_)));
    }

    #[test]
    fn a_missing_interface_is_waited_for_and_starts_when_plugged_in() {
        let mut s = Supervisor {
            remembered: Some("Scarlett 2i2 USB".into()),
            ..Supervisor::default()
        };
        s.configure(config(None), true, None);
        assert_eq!(s.plan(Trigger::Boot, laptop_only), Plan::Quiet);
        let view = s.view(false, false);
        assert_eq!(view.phase, "waiting");
        assert_eq!(view.missing.as_deref(), Some("Scarlett 2i2 USB"));
        assert_eq!(view.alternative.as_deref(), Some("MacBook Pro-Lautsprecher"));
        // A press while waiting says why nothing happens instead of opening the speakers.
        assert!(matches!(s.plan(Trigger::Demand, laptop_only), Plan::Refuse(t) if t.contains("not connected")));
        assert_eq!(s.plan(Trigger::Passive, laptop_only), Plan::Quiet);
        // Still not there.
        assert_eq!(s.plan(Trigger::Plug, laptop_only), Plan::Quiet);
        // Plugged in.
        assert_eq!(opened(s.plan(Trigger::Plug, with_scarlett)), Some("Scarlett 2i2 USB".into()));
        s.started("Scarlett 2i2 USB", true);
        assert_eq!(s.view(true, false).phase, "running");
    }

    #[test]
    fn use_the_default_now_opens_it_once() {
        let mut s = configured(Some("Scarlett"), true);
        assert_eq!(s.plan(Trigger::Boot, laptop_only), Plan::Quiet);
        s.use_fallback();
        assert_eq!(opened(s.plan(Trigger::Demand, laptop_only)), None);
        s.started("MacBook Pro-Lautsprecher", false);
        // The next time the setup's device counts again.
        s.stopped_by_user();
        assert!(matches!(s.plan(Trigger::Demand, laptop_only), Plan::Refuse(_)));
    }

    #[test]
    fn nothing_starts_on_the_way_out() {
        let mut s = configured(None, true);
        s.quit();
        assert_eq!(s.plan(Trigger::Boot, laptop_only), Plan::Quiet);
        assert_eq!(s.plan(Trigger::Demand, laptop_only), Plan::Quiet);
        assert_eq!(s.plan(Trigger::Passive, laptop_only), Plan::Quiet);
    }

    #[test]
    fn only_device_and_track_shape_need_a_restart() {
        let running = config(None);
        let mut wanted = running.clone();
        wanted.bpm = 140.0;
        wanted.click = false;
        wanted.tracks[0].pan = -0.5;
        assert!(!needs_restart(&running, &wanted, false), "runtime settings");
        wanted.buffer_frames = 256;
        assert!(needs_restart(&running, &wanted, false), "buffer");
        let mut wanted = running.clone();
        wanted.tracks[0].input_channel = 2;
        assert!(needs_restart(&running, &wanted, false), "input of a track");
        assert!(!needs_restart(&running, &wanted, true), "a loaded score has its own tracks");
        let mut wanted = running.clone();
        wanted.device = Some("Scarlett".into());
        assert!(needs_restart(&running, &wanted, true), "device, score or not");
    }

    /// A stand-in engine: what was done to it, in order.
    #[derive(Default)]
    struct FakeEngine {
        device: Option<String>,
        tracks: usize,
        done: Vec<String>,
    }

    fn opens(device: &str) -> impl FnOnce(StartConfig, &Supervisor) -> Result<(FakeEngine, Opened), String> {
        let device = device.to_string();
        move |config, _| {
            let engine = FakeEngine {
                device: config.device.clone(),
                tracks: config.tracks.len(),
                done: vec!["started".into()],
            };
            Ok((engine, Opened { interface: !device.contains("MacBook"), device }))
        }
    }

    fn refuses(why: &'static str) -> impl FnOnce(StartConfig, &Supervisor) -> Result<(FakeEngine, Opened), String> {
        move |_, _| Err(why.to_string())
    }

    #[test]
    fn record_with_the_engine_off_starts_it_and_then_records() {
        let mut s = configured(None, false);
        let mut engine: Option<FakeEngine> = None;
        let result = then_act(&mut engine, |e| bring_up(e, &mut s, Trigger::Demand, with_scarlett, opens("Scarlett 2i2 USB")), |e| {
            e.done.push("record track 1".into());
            Ok(())
        });
        assert_eq!(result, Ok(()));
        let e = engine.as_ref().expect("the engine runs");
        assert_eq!(e.done, ["started", "record track 1"], "first the start, then the press");
        // A second press finds it running and starts nothing.
        then_act(&mut engine, |e| bring_up(e, &mut s, Trigger::Demand, || panic!("no survey"), refuses("must not open")), |e| {
            e.done.push("overdub".into());
            Ok(())
        })
        .expect("runs");
        assert_eq!(engine.unwrap().done, ["started", "record track 1", "overdub"]);
    }

    #[test]
    fn load_and_start_are_carried_out_on_the_engine_they_started() {
        for press in ["load the score", "start the score"] {
            let mut s = configured(None, true);
            let mut engine: Option<FakeEngine> = None;
            let answer = then_act(&mut engine, |e| bring_up(e, &mut s, Trigger::Demand, with_scarlett, opens("Scarlett 2i2 USB")), |e| {
                e.done.push(press.into());
                Ok(format!("{press}: done"))
            });
            assert_eq!(answer.as_deref(), Ok(format!("{press}: done").as_str()));
            assert_eq!(engine.expect("runs").done, ["started", press]);
            assert_eq!(s.view(true, false).phase, "running");
        }
    }

    #[test]
    fn a_press_while_the_interface_is_missing_does_not_open_the_speakers() {
        let mut s = configured(Some("Scarlett"), true);
        let mut engine: Option<FakeEngine> = None;
        let answer = then_act(&mut engine, |e| bring_up(e, &mut s, Trigger::Demand, laptop_only, refuses("must not open")), |_| Ok(()));
        assert!(answer.unwrap_err().contains("Scarlett is not connected"));
        assert!(engine.is_none());
        // Plugged in: the next look starts it, on the interface.
        bring_up(&mut engine, &mut s, Trigger::Plug, with_scarlett, opens("Scarlett 2i2 USB")).expect("starts");
        assert_eq!(engine.unwrap().device.as_deref(), Some("Scarlett"));
    }

    #[test]
    fn a_failed_start_carries_out_nothing_and_says_why() {
        let mut s = configured(None, true);
        let mut engine: Option<FakeEngine> = None;
        let answer = then_act(&mut engine, |e| bring_up(e, &mut s, Trigger::Demand, laptop_only, refuses("The device refused 48 kHz.")), |_| {
            panic!("the press must not run without an engine")
        });
        assert_eq!(answer, Err::<(), _>("Engine could not start: The device refused 48 kHz.".into()));
        // The keyboard keeps sending notes: not one more attempt.
        for _ in 0..3 {
            let again = bring_up(&mut engine, &mut s, Trigger::Passive, || panic!("no survey"), refuses("no"));
            assert!(again.is_err());
        }
        assert_eq!(s.view(false, false).phase, "failed");
    }

    #[test]
    fn a_refused_microphone_is_its_own_failure() {
        let mut s = configured(None, true);
        let mut engine: Option<FakeEngine> = None;
        let answer = bring_up(&mut engine, &mut s, Trigger::Boot, laptop_only, refuses(NO_MIC_PERMISSION));
        assert_eq!(answer, Err(NO_MIC_PERMISSION.to_string()));
        let view = s.view(false, false);
        assert!(view.permission);
        assert_eq!(view.message.as_deref(), Some(NO_MIC_PERMISSION));
    }

    #[test]
    fn a_note_starts_the_engine_with_the_setup_tracks() {
        let mut s = configured(None, false);
        let mut engine: Option<FakeEngine> = None;
        bring_up(&mut engine, &mut s, Trigger::Passive, laptop_only, opens("MacBook Pro-Lautsprecher")).expect("starts");
        assert_eq!(engine.unwrap().tracks, 1);
        assert!(s.remembered.is_none(), "the laptop is not an interface to wait for");
    }

    #[test]
    fn naming_the_remembered_interface_or_saying_default_needs_no_restart() {
        let mut s = configured(Some("Scarlett 2i2 USB"), true);
        assert!(matches!(s.plan(Trigger::Boot, with_scarlett), Plan::Open(_)));
        s.started("Scarlett 2i2 USB", true);
        s.configure(config(None), true, Some(false));
        assert!(!s.view(true, false).restart_needed);
        let mut buffer = config(None);
        buffer.buffer_frames = 64;
        s.configure(buffer, true, Some(false));
        assert!(s.view(true, false).restart_needed);
    }

    #[test]
    fn the_remembered_interface_survives_a_restart_of_the_app() {
        let dir = std::env::temp_dir().join(format!("looper-supervisor-{}", std::process::id()));
        let path = dir.join("engine.json");
        let mut s = Supervisor::with_store(path.clone());
        s.configure(config(None), true, None);
        s.started("Scarlett 2i2 USB", true);
        assert_eq!(Supervisor::with_store(path).remembered.as_deref(), Some("Scarlett 2i2 USB"));
        let _ = std::fs::remove_dir_all(dir);
    }
}
