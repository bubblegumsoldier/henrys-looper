//! MIDI in the app: every connected controller, its profile, and the learn mode the UI drives.
//!
//! ```text
//!   Controller A ──► Port ──┐
//!   Controller B ──► Port ──┼─► drain() ──► resolve(Link) ──► MidiAction ──► Session::apply
//!   (Treiber-Thread)        │   Host-       Router je Geraet   (dieselbe       (derselbe Weg
//!                           │   Thread      = Profil+Partitur   Absicht wie     wie ein
//!                           │                                   ein Klick)      Mausklick)
//!   ProfileStore ◄──────────┘  (ein YAML je Geraet, nach jedem Lernen sofort gespeichert)
//! ```
//!
//! # What is here and what is in the engine library
//!
//! Everything *about* MIDI - the byte decoding, the address tree, the mapping, the resolution, the
//! learn mechanic, the profile files and the status of a binding - lives in `looper_engine::midi`
//! and is provable without a device. This file is the part that only makes sense inside the app:
//! which ports are open, which profile belongs to which, and turning all of it into something the
//! web view can show.
//!
//! # Several controllers at once
//!
//! A foot pedal and a pad controller on the same evening is the normal case for a looper, so every
//! port whose profile is known is opened, each with **its own router**. Its own, because two
//! devices send the same ids - both the pedal and the pads say `ch1.note36` - and one shared map
//! would make the pedal fire the pads' bindings. A learned binding lands in the profile of the
//! device that sent the learning press. The score's `midi:` block names no device, so it is laid
//! over every connected one.
//!
//! # Connecting by itself
//!
//! Every [`SCAN_INTERVAL`] the bridge looks at the port list and the profile folder ([`MidiBridge::tick`]):
//! a port whose profile is known is opened, a port that went away is dropped, a profile edited by
//! hand is read again. A device that is connected by hand once gets an (empty) profile file at
//! once, and from then on it is "known" and comes back by itself after every start and every
//! replug. "Disconnect" by hand is respected until the device is unplugged or connected again.
//!
//! On Windows this means the app takes a known controller whenever it is free - that is what was
//! asked for, and the device list says so when another program holds it.
//!
//! # Why MIDI events do not ride in the status snapshot
//!
//! The status event is a **twenty-per-second sampling of a continuous state**, and `Status::latest`
//! deliberately throws older snapshots away - a position that is one snapshot old is worthless.
//! MIDI is the opposite: a rare, bursty stream of *discrete* facts where every single one matters.
//! So MIDI travels on its own event ([`crate::host::MIDI_EVENT`]), queued rather than sampled. A
//! knob sweep is coalesced and emitted at most every [`EMIT_INTERVAL`]; anything discrete goes out
//! at once.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

use looper_engine::midi::input::{self, MidiIn};
use looper_engine::midi::{
    Binding, BindingState, ButtonMode, Context, Control, Learn, MidiEvent, MidiId, MidiIdKind,
    MidiMap, ProfileStore, Resolution, Router, SessionShape, Takeover, Target, check_target,
    profile_dir, score_map,
};
use looper_engine::score::CompiledScore;

/// How often the web view is told about a knob that is being swept.
const EMIT_INTERVAL: Duration = Duration::from_millis(40);
/// Reports kept while waiting for the next emit window.
const PENDING_MAX: usize = 64;
/// How often the port list and the profile folder are looked at. Neither Windows nor (without a
/// run loop) macOS tells a thread that a device came or went, so the list is asked - at a rate where
/// asking costs nothing and a replugged pedal is back before the foot finds it.
pub const SCAN_INTERVAL: Duration = Duration::from_secs(1);
/// How often the session's shape (tracks, sections) is sampled for the status of the bindings.
const SHAPE_INTERVAL: Duration = Duration::from_millis(250);

// ---------------------------------------------------------------------------------------------
// The MIDI system, behind a seam so the bridge can be tested without CoreMIDI or winmm
// ---------------------------------------------------------------------------------------------

/// What the bridge needs from the operating system.
pub trait MidiSystem {
    /// Names of every input port right now.
    fn ports(&mut self) -> Result<Vec<String>, String>;
    /// Open the port with exactly this name.
    fn open(&mut self, name: &str) -> Result<Box<dyn MidiPort>, String>;
}

/// One open input.
pub trait MidiPort {
    fn name(&self) -> &str;
    fn try_recv(&mut self) -> Option<MidiEvent>;
    /// Events lost between the driver callback and the host thread.
    fn dropped(&self) -> u64;
}

/// The real thing: midir.
struct SystemMidi;

impl MidiSystem for SystemMidi {
    fn ports(&mut self) -> Result<Vec<String>, String> {
        Ok(input::list_ports()?.into_iter().map(|p| p.name).collect())
    }

    fn open(&mut self, name: &str) -> Result<Box<dyn MidiPort>, String> {
        Ok(Box::new(input::open_named(name)?))
    }
}

impl MidiPort for MidiIn {
    fn name(&self) -> &str {
        MidiIn::name(self)
    }

    fn try_recv(&mut self) -> Option<MidiEvent> {
        MidiIn::try_recv(self).map(|timed| timed.event)
    }

    fn dropped(&self) -> u64 {
        MidiIn::dropped(self)
    }
}

// ---------------------------------------------------------------------------------------------
// What the web view gets
// ---------------------------------------------------------------------------------------------

/// What one incoming event did.
#[derive(Serialize, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum MidiOutcome {
    Unbound,
    Absorbed,
    Action,
    Refused,
    Learned,
}

/// One incoming event as the monitor prints it.
#[derive(Serialize, Clone, Debug)]
#[serde(rename_all = "snake_case")]
pub struct MidiEventReport {
    pub seq: u64,
    /// `ch1.note36` - the string a profile file is keyed by.
    pub id: String,
    /// `Pad 36`, `CC 3` - the badge on a bound control.
    pub short: String,
    pub channel: u8,
    pub what: String,
    pub number: Option<u8>,
    pub value: i32,
    pub target: Option<String>,
    pub target_label: Option<String>,
    pub outcome: MidiOutcome,
    pub note: Option<String>,
    pub continuous: bool,
    /// The port the event came from.
    pub port: Option<String>,
    /// `device:` of that port's profile.
    pub device: Option<String>,
}

/// One MIDI input port as the system lists it.
#[derive(Serialize, Clone, Debug, PartialEq)]
#[serde(rename_all = "snake_case")]
pub struct MidiPortView {
    pub index: usize,
    pub name: String,
    /// `device:` of the profile that matches this port, or `null`.
    pub profile: Option<String>,
    pub bindings: usize,
    pub connected: bool,
}

/// How a known device is doing.
#[derive(Serialize, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum DeviceState {
    /// Open; its bindings work.
    Connected,
    /// Not plugged in (no such port).
    Missing,
    /// Plugged in, but disconnected by hand.
    Disconnected,
    /// Plugged in, but it could not be opened (on Windows: another program holds it).
    Unavailable,
    /// The profile file does not parse; it is left untouched until it is fixed.
    Error,
}

/// One known controller: a profile file, and whether its device is here.
#[derive(Serialize, Clone, Debug)]
#[serde(rename_all = "snake_case")]
pub struct MidiDeviceView {
    /// `device:` of the profile - also the key the other views refer to.
    pub device: String,
    pub path: String,
    pub state: DeviceState,
    pub connected: bool,
    /// The port it is open on.
    pub port: Option<String>,
    pub bindings: usize,
    /// Bindings that would work right now ([`BindingState::works`]).
    pub active: usize,
    pub inactive: usize,
    /// Parse error of the file, or why the port could not be opened.
    pub error: Option<String>,
    /// True while a change could not be written (see `save_error`).
    pub unsaved: bool,
    /// When the device was last connected, in milliseconds since 1970 - "last seen 20:41". Only
    /// known since the app started; null before that.
    pub last_seen: Option<u64>,
    /// One English line, ready to show: "connected - 12 of 12 bindings active".
    pub summary: String,
}

/// One binding with everything needed to show it, edit it and say whether it works.
#[derive(Serialize, Clone, Debug)]
#[serde(rename_all = "snake_case")]
pub struct MidiBindingView {
    pub id: String,
    pub short: String,
    pub channel: u8,
    /// `note`, `cc` or `bend`.
    pub kind: String,
    pub number: u8,
    /// The target as written in the file.
    pub address: String,
    /// The same with the track as a position (`track.1...`) - the spelling `data-midi` uses. Null
    /// while a track name cannot be resolved (no engine running, or no such track).
    pub resolved_address: Option<String>,
    pub label: String,
    pub describe: String,
    /// From the loaded score rather than from a controller profile.
    pub from_score: bool,
    /// `device:` of the profile it belongs to; null for a score binding.
    pub device: Option<String>,
    /// `trigger`, `switch` or `range` - which of the options below apply.
    pub control: String,
    /// Button behaviour (`press`, `release`, `toggle`, `momentary`); null for a range.
    pub mode: Option<String>,
    /// The modes this target allows.
    pub modes: Vec<String>,
    /// `pickup` or `jump`; null for a trigger or switch.
    pub takeover: Option<String>,
    /// The binding's own bounds, null where the target's range applies.
    pub min: Option<f32>,
    pub max: Option<f32>,
    /// The target's full range, for a range.
    pub range_min: Option<f32>,
    pub range_max: Option<f32>,
    pub state: BindingState,
    /// English sentence for anything but `active`.
    pub reason: Option<String>,
}

/// What the statuses were checked against.
#[derive(Serialize, Clone, Debug, PartialEq)]
#[serde(rename_all = "snake_case")]
pub struct MidiSessionView {
    pub running: bool,
    pub tracks: Vec<String>,
    /// Sections of the loaded score, null without one.
    pub sections: Option<usize>,
}

/// Everything about the MIDI side that is not a single event. Sent whole whenever any of it
/// changes, on `looper://midi` and as the answer of every MIDI command.
#[derive(Serialize, Clone, Debug)]
#[serde(rename_all = "snake_case")]
pub struct MidiView {
    /// True when at least one device is connected.
    pub connected: bool,
    /// First connected port (kept for older views; see `devices`).
    pub port: Option<String>,
    /// Its profile's device.
    pub device: Option<String>,
    /// Its profile's file.
    pub path: Option<String>,
    /// True when a change could not be written. Normally false: every change is saved at once.
    pub dirty: bool,
    pub learning: Option<String>,
    pub learning_label: Option<String>,
    /// Every binding of every known profile plus the score's, with its status.
    pub bindings: Vec<MidiBindingView>,
    /// Every known profile, connected or not.
    pub devices: Vec<MidiDeviceView>,
    /// Every input port the system has right now.
    pub ports: Vec<MidiPortView>,
    /// The folder holding the profiles.
    pub dir: String,
    pub session: MidiSessionView,
    pub notes: Vec<String>,
    pub message: Option<String>,
    /// Why the last save failed, if it did.
    pub save_error: Option<String>,
    pub dropped: u64,
}

/// What travels on `looper://midi`.
#[derive(Serialize, Clone, Debug)]
#[serde(rename_all = "snake_case")]
pub struct MidiFeed {
    pub events: Vec<MidiEventReport>,
    pub view: Option<MidiView>,
}

/// Answer of "save now".
#[derive(Serialize, Clone, Debug)]
#[serde(rename_all = "snake_case")]
pub struct MidiSaved {
    pub path: String,
    pub message: String,
    pub view: MidiView,
}

/// The options of one binding as the binding list edits them. A full replacement: `null` means
/// "the target's default" (toggle for a switch, press for a trigger, pickup, the full range).
#[derive(Deserialize, Clone, Debug, Default)]
#[serde(rename_all = "snake_case")]
pub struct BindingOptions {
    pub mode: Option<String>,
    pub takeover: Option<String>,
    pub min: Option<f32>,
    pub max: Option<f32>,
}

// ---------------------------------------------------------------------------------------------
// The bridge
// ---------------------------------------------------------------------------------------------

/// One open port and what it resolves against.
struct Link {
    port: Box<dyn MidiPort>,
    /// The profile file this port belongs to.
    profile: PathBuf,
    router: Router,
    /// Controls of this device the score took over.
    notes: Vec<String>,
}

/// Every connected controller, every known profile, and the learn mode.
///
/// Lives on the host thread next to the session and is never touched from anywhere else.
pub struct MidiBridge {
    system: Box<dyn MidiSystem>,
    store: ProfileStore,
    links: Vec<Link>,
    /// The port list as of the last scan.
    available: Vec<String>,
    /// Ports disconnected by hand: not opened again until replugged or connected by hand.
    suppressed: Vec<String>,
    /// Ports that could not be opened, with the reason - shown, and logged once.
    failures: Vec<(String, String)>,
    score: MidiMap,
    shape: SessionShape,
    learn: Learn,
    save_error: Option<String>,
    message: Option<String>,
    seq: u64,
    pending: Vec<MidiEventReport>,
    view_changed: bool,
    last_emit: Instant,
    last_scan: Option<Instant>,
    last_shape: Option<Instant>,
    /// Profile file -> when its device was last seen connected.
    seen: Vec<(PathBuf, u64)>,
}

impl Default for MidiBridge {
    fn default() -> Self {
        Self::new()
    }
}

impl MidiBridge {
    /// The bridge on the real MIDI system and the real profile folder.
    pub fn new() -> Self {
        Self::with(Box::new(SystemMidi), profile_dir())
    }

    /// The bridge on any MIDI system and any folder - what the tests use.
    pub fn with(system: Box<dyn MidiSystem>, dir: impl Into<PathBuf>) -> Self {
        Self {
            system,
            store: ProfileStore::new(dir),
            links: Vec::new(),
            available: Vec::new(),
            suppressed: Vec::new(),
            failures: Vec::new(),
            score: MidiMap::new(),
            shape: SessionShape::default(),
            learn: Learn::new(),
            save_error: None,
            message: None,
            seq: 0,
            pending: Vec::new(),
            view_changed: true,
            last_emit: Instant::now(),
            last_scan: None,
            last_shape: None,
            seen: Vec::new(),
        }
    }

    /// True while at least one port is open - the host loop then runs on its short tick.
    pub fn is_open(&self) -> bool {
        !self.links.is_empty()
    }

    pub fn is_learning(&self) -> bool {
        self.learn.is_armed()
    }

    // ---- connecting by itself ----------------------------------------------------------------

    /// Scan if it is time. Returns log lines for what happened.
    pub fn tick(&mut self) -> Vec<String> {
        if self
            .last_scan
            .is_some_and(|at| at.elapsed() < SCAN_INTERVAL)
        {
            return Vec::new();
        }
        self.scan()
    }

    /// Look at the profile folder and the port list now, and connect, drop and reload accordingly.
    pub fn scan(&mut self) -> Vec<String> {
        self.last_scan = Some(Instant::now());
        let mut log = Vec::new();

        // Profiles first, so a port that appears in the same breath finds its profile.
        let changes = self.store.scan();
        if !changes.is_empty() {
            self.view_changed = true;
            // A file that went away takes its connection with it; one that changed is applied.
            self.links
                .retain(|link| self.store.get(&link.profile).is_some());
            let paths: Vec<PathBuf> = self.links.iter().map(|l| l.profile.clone()).collect();
            for path in paths {
                self.rebuild(&path);
            }
        }

        let Ok(ports) = self.system.ports() else {
            // A port list that cannot be read is not evidence that the devices are gone.
            return log;
        };
        if ports != self.available {
            self.view_changed = true;
        }
        self.available = ports;

        // Still here: remember when, for "last seen" once it is gone.
        let now = now_ms();
        for link in &self.links {
            match self.seen.iter_mut().find(|(p, _)| *p == link.profile) {
                Some(slot) => slot.1 = now,
                None => self.seen.push((link.profile.clone(), now)),
            }
        }

        // Gone: drop the link, say how many bindings went with it.
        let mut index = 0;
        while index < self.links.len() {
            let name = self.links[index].port.name().to_string();
            if self.available.contains(&name) {
                index += 1;
                continue;
            }
            let link = self.links.remove(index);
            let entry = self.store.get(&link.profile);
            let device = entry.map(|e| e.device.clone()).unwrap_or(name.clone());
            let count = entry.map_or(0, |e| e.len());
            let line = format!(
                "MIDI device \"{device}\" disconnected - {count} {} inactive until it is plugged \
                 back in.",
                plural(count, "binding", "bindings")
            );
            log.push(line.clone());
            self.message = Some(line);
            self.view_changed = true;
        }
        if self.links.is_empty() && self.learn.is_armed() {
            // Nothing could answer the learn mode any more.
            self.learn.cancel();
        }
        // Unplugging ends a manual "disconnect" and forgets an old failure.
        let available = self.available.clone();
        self.suppressed.retain(|name| available.contains(name));
        self.failures.retain(|(name, _)| available.contains(name));

        // New: open every port whose profile is known and not yet connected.
        for name in available {
            if self.is_linked(&name) || self.suppressed.contains(&name) {
                continue;
            }
            let Some(path) = self.store.best_for_port(&name).map(Path::to_path_buf) else {
                continue;
            };
            if self.links.iter().any(|link| link.profile == path) {
                continue; // that profile is already in use by another port
            }
            match self.connect(&name, &path) {
                Ok(line) => {
                    self.failures.retain(|(n, _)| *n != name);
                    log.push(line.clone());
                    self.message = Some(line);
                }
                Err(error) => {
                    let known = self.failures.iter().any(|(n, e)| *n == name && *e == error);
                    if !known {
                        log.push(format!("MIDI device \"{name}\" could not be opened: {error}"));
                        self.failures.retain(|(n, _)| *n != name);
                        self.failures.push((name.clone(), error));
                        self.view_changed = true;
                    }
                }
            }
        }
        log
    }

    fn is_linked(&self, port: &str) -> bool {
        self.links.iter().any(|link| link.port.name() == port)
    }

    /// Open a port for a profile and put it into service.
    fn connect(&mut self, port: &str, path: &Path) -> Result<String, String> {
        let opened = self.system.open(port)?;
        self.links.push(Link {
            port: opened,
            profile: path.to_path_buf(),
            router: Router::new(MidiMap::new()),
            notes: Vec::new(),
        });
        self.seen.retain(|(p, _)| p != path);
        self.seen.push((path.to_path_buf(), now_ms()));
        self.rebuild(path);
        self.view_changed = true;
        let entry = self.store.get(path);
        let device = entry.map(|e| e.device.clone()).unwrap_or_default();
        Ok(match entry {
            Some(e) if e.error.is_some() => format!(
                "MIDI device \"{port}\" connected, but its profile {} has errors and is not used \
                 until it is fixed.",
                path.display()
            ),
            Some(e) if e.is_empty() => format!(
                "MIDI device \"{port}\" connected. No bindings yet - switch on MIDI Learn and \
                 click a control."
            ),
            Some(e) => format!(
                "MIDI device \"{port}\" connected - profile \"{device}\", {} {}.",
                e.len(),
                plural(e.len(), "binding", "bindings")
            ),
            None => format!("MIDI device \"{port}\" connected."),
        })
    }

    /// Every MIDI input, with the profile that belongs to it. Asks the system afresh.
    pub fn ports(&mut self) -> Result<Vec<MidiPortView>, String> {
        self.available = self.system.ports()?;
        Ok(self.port_views())
    }

    fn port_views(&self) -> Vec<MidiPortView> {
        self.available
            .iter()
            .enumerate()
            .map(|(index, name)| {
                let entry = self
                    .store
                    .best_for_port(name)
                    .and_then(|path| self.store.get(path));
                MidiPortView {
                    index,
                    name: name.clone(),
                    profile: entry.map(|e| e.device.clone()),
                    bindings: entry.map_or(0, |e| e.len()),
                    connected: self.is_linked(name),
                }
            })
            .collect()
    }

    /// Connect a port by hand: by its number in the port list, its exact name or a unique part of
    /// it. A device without a profile gets one at once - from now on it connects by itself.
    pub fn open(&mut self, selector: &str) -> Result<MidiView, String> {
        self.available = self.system.ports()?;
        let name = pick(selector, &self.available)?;
        self.suppressed.retain(|n| *n != name);
        if self.is_linked(&name) {
            self.message = Some(format!("MIDI device \"{name}\" is already connected."));
            return Ok(self.view());
        }
        let path = match self.store.best_for_port(&name) {
            Some(path) => path.to_path_buf(),
            None => self.store.create(&name)?,
        };
        if let Some(other) = self.links.iter().position(|l| l.profile == path) {
            // One profile, one port: the profile moves to the port that was asked for.
            self.links.remove(other);
        }
        let line = self.connect(&name, &path)?;
        self.failures.retain(|(n, _)| *n != name);
        self.message = Some(line);
        Ok(self.view())
    }

    /// Disconnect by hand - one device (by port name or `device:`), or all of them. The device is
    /// not reconnected automatically until it is unplugged or connected by hand again.
    pub fn close(&mut self, which: Option<&str>) -> MidiView {
        let mut closed = Vec::new();
        let mut index = 0;
        while index < self.links.len() {
            let port = self.links[index].port.name().to_string();
            let device = self
                .store
                .get(&self.links[index].profile)
                .map(|e| e.device.clone());
            let hit = match which {
                None => true,
                Some(w) => port == w || device.as_deref() == Some(w),
            };
            if hit {
                self.links.remove(index);
                self.suppressed.push(port.clone());
                closed.push(port);
            } else {
                index += 1;
            }
        }
        if self.links.is_empty() {
            self.learn.cancel();
        }
        self.message = Some(if closed.is_empty() {
            "No such MIDI device is connected.".to_string()
        } else {
            format!(
                "Disconnected: {}. Connect again by hand, or unplug and replug.",
                closed.join(", ")
            )
        });
        self.view_changed = true;
        self.view()
    }

    // ---- the mapping -----------------------------------------------------------------------

    /// The score's `midi:` block, laid over every connected profile.
    pub fn set_score(&mut self, score: Option<&CompiledScore>) -> Result<Vec<String>, Vec<String>> {
        self.score = match score {
            Some(score) => score_map(score)?,
            None => MidiMap::new(),
        };
        let paths: Vec<PathBuf> = self.links.iter().map(|l| l.profile.clone()).collect();
        for path in paths {
            self.rebuild(&path);
        }
        self.view_changed = true;
        Ok(self.notes())
    }

    /// The session the statuses are checked against. Cheap when nothing changed.
    pub fn set_shape(&mut self, shape: SessionShape) {
        self.last_shape = Some(Instant::now());
        if shape != self.shape {
            self.shape = shape;
            self.view_changed = true;
        }
    }

    /// Whether it is time to hand in a fresh [`SessionShape`].
    pub fn shape_due(&self) -> bool {
        self.last_shape
            .is_none_or(|at| at.elapsed() >= SHAPE_INTERVAL)
    }

    /// Profile plus score into the router of every port that uses this profile.
    fn rebuild(&mut self, path: &Path) {
        let profile_map = self
            .store
            .get(path)
            .and_then(|e| e.profile.as_ref())
            .map(|p| p.map.clone())
            .unwrap_or_default();
        for link in self.links.iter_mut().filter(|l| l.profile == path) {
            let mut map = profile_map.clone();
            link.notes = map.overlay(&self.score);
            link.router.set_map(map);
        }
        self.view_changed = true;
    }

    fn notes(&self) -> Vec<String> {
        let mut notes: Vec<String> = Vec::new();
        for note in self.links.iter().flat_map(|l| l.notes.iter()) {
            if !notes.contains(note) {
                notes.push(note.clone());
            }
        }
        notes
    }

    /// Make every knob catch its parameter again.
    pub fn rearm(&mut self) {
        for link in &mut self.links {
            link.router.rearm();
        }
    }

    /// Write one profile now; on failure keep the change in memory and say why.
    fn persist(&mut self, path: &Path) -> Result<(), String> {
        match self.store.save(path) {
            Ok(saved) => {
                self.save_error = None;
                if let Some(backup) = saved.backup {
                    self.message = Some(format!(
                        "The profile had to be rewritten; the original is kept as {}.",
                        backup.display()
                    ));
                }
                Ok(())
            }
            Err(error) => {
                self.save_error = Some(error.clone());
                Err(error)
            }
        }
    }

    // ---- learn -----------------------------------------------------------------------------

    /// The next control that arrives becomes this address.
    pub fn arm(&mut self, address: &str) -> Result<MidiView, String> {
        let target = Target::parse(address)?;
        let mut message = self.learn.arm(target);
        if self.links.is_empty() {
            message.push_str(" No MIDI device is connected yet - connect one, then press a control.");
        }
        self.message = Some(message);
        self.view_changed = true;
        Ok(self.view())
    }

    pub fn cancel_learn(&mut self) -> MidiView {
        if let Some(message) = self.learn.cancel() {
            self.message = Some(message);
        }
        self.view_changed = true;
        self.view()
    }

    /// One event from one port while the learn mode is armed. The binding goes into that port's
    /// profile and the file is written at once.
    pub fn feed_learn(&mut self, link: usize, event: &MidiEvent) -> Option<MidiEventReport> {
        let path = self.links.get(link)?.profile.clone();
        let broken = self
            .store
            .get(&path)
            .is_none_or(|entry| entry.profile.is_none());
        if broken {
            if matches!(event, MidiEvent::NoteOff { .. }) {
                return None;
            }
            let note = format!(
                "Not learned: the profile {} has errors. Fix the file first - it is left untouched.",
                path.display()
            );
            self.learn.cancel();
            self.message = Some(note.clone());
            self.view_changed = true;
            let report = self.make_report(link, event, MidiOutcome::Refused, None, Some(note));
            self.push(report.clone());
            return Some(report);
        }

        let previous = self.links[link].router.map().get(event.id()).cloned();
        let learned = self.learn.feed(event, previous.as_ref())?;
        if let Some(profile) = self.store.get_mut(&path).and_then(|e| e.profile.as_mut()) {
            profile.map.insert(learned.id, learned.binding.clone());
        }
        let mut message = learned.message.clone();
        if let Err(error) = self.persist(&path) {
            message.push_str(&format!(" NOT SAVED: {error}"));
        }
        self.rebuild(&path);
        // A freshly bound knob must not act on the position it happens to sit at.
        self.links[link].router.rearm();
        self.message = Some(message.clone());
        let report = self.make_report(
            link,
            event,
            MidiOutcome::Learned,
            Some(&learned.binding.target),
            Some(message),
        );
        self.push(report.clone());
        Some(report)
    }

    /// The profile that holds `id`, chosen by `device` when several do.
    fn owner_of(&self, id: MidiId, device: Option<&str>) -> Result<PathBuf, String> {
        let owners: Vec<&looper_engine::midi::StoredProfile> = self
            .store
            .entries()
            .iter()
            .filter(|e| device.is_none_or(|d| e.device == d))
            .filter(|e| e.profile.as_ref().is_some_and(|p| p.map.get(id).is_some()))
            .collect();
        match owners.as_slice() {
            [one] => Ok(one.path.clone()),
            [] => Err(if self.score.get(id).is_some() {
                format!(
                    "{id} kommt aus der Partitur, nicht aus dem Profil. Diese Bindung steht im \
                     'midi:'-Block der Partitur und geht mit ihr wieder weg."
                )
            } else {
                format!("{id} ist gar nicht belegt.")
            }),
            many => Err(format!(
                "{id} is bound on several devices ({}) - say which one.",
                many.iter()
                    .map(|e| e.device.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            )),
        }
    }

    /// Take a control's job away, and save.
    pub fn unbind(&mut self, id: &str, device: Option<&str>) -> Result<MidiView, String> {
        let key = MidiId::parse(id)?;
        let path = self.owner_of(key, device)?;
        let removed = self
            .store
            .get_mut(&path)
            .and_then(|e| e.profile.as_mut())
            .and_then(|p| p.map.remove(key))
            .ok_or_else(|| format!("{id} ist gar nicht belegt."))?;
        let mut message = format!("{id} ist wieder frei (war \"{}\").", removed.target.label());
        if let Err(error) = self.persist(&path) {
            message.push_str(&format!(" NOT SAVED: {error}"));
        }
        self.rebuild(&path);
        self.message = Some(message);
        Ok(self.view())
    }

    /// Change a binding's options - button mode, takeover, range - and save.
    pub fn edit(
        &mut self,
        id: &str,
        device: Option<&str>,
        options: &BindingOptions,
    ) -> Result<MidiView, String> {
        let key = MidiId::parse(id)?;
        let path = self.owner_of(key, device)?;
        let current = self
            .store
            .get(&path)
            .and_then(|e| e.profile.as_ref())
            .and_then(|p| p.map.get(key))
            .cloned()
            .ok_or_else(|| format!("{id} is not bound."))?;
        let mut binding = Binding::new(current.target.clone());
        if let Some(mode) = options.mode.as_deref() {
            binding.button = ButtonMode::parse(mode).ok_or_else(|| {
                format!(
                    "'{mode}' is not a button mode. Allowed: {}.",
                    ButtonMode::names().join(", ")
                )
            })?;
        }
        if let Some(takeover) = options.takeover.as_deref() {
            binding.takeover = Takeover::parse(takeover).ok_or_else(|| {
                format!(
                    "'{takeover}' is not a takeover mode. Allowed: {}.",
                    Takeover::names().join(", ")
                )
            })?;
        }
        if matches!(current.target.control(), Control::Range(_)) {
            binding = binding.with_bounds(options.min, options.max);
        }
        binding.check()?;
        if let Some(profile) = self.store.get_mut(&path).and_then(|e| e.profile.as_mut()) {
            profile.map.insert(key, binding.clone());
        }
        let mut message = format!("{id}: {}", binding.describe());
        if let Err(error) = self.persist(&path) {
            message.push_str(&format!(" NOT SAVED: {error}"));
        }
        self.rebuild(&path);
        self.message = Some(message);
        Ok(self.view())
    }

    /// Write every profile with changes that are not on disk yet - the retry after a failed save.
    pub fn save(&mut self) -> Result<MidiSaved, String> {
        let pending: Vec<PathBuf> = self
            .store
            .entries()
            .iter()
            .filter(|e| e.unsaved)
            .map(|e| e.path.clone())
            .collect();
        for path in &pending {
            self.persist(path)?;
        }
        let dir = self.store.dir().display().to_string();
        let message = if pending.is_empty() {
            format!("Everything is saved already ({dir}).")
        } else {
            format!("{} profile(s) saved in {dir}.", pending.len())
        };
        self.message = Some(message.clone());
        self.view_changed = true;
        Ok(MidiSaved {
            path: pending
                .first()
                .map(|p| p.display().to_string())
                .unwrap_or(dir),
            message,
            view: self.view(),
        })
    }

    // ---- the stream ------------------------------------------------------------------------

    /// Everything the drivers have queued, with the index of the port it came from.
    pub fn drain(&mut self, out: &mut Vec<(usize, MidiEvent)>) {
        for (index, link) in self.links.iter_mut().enumerate() {
            while let Some(event) = link.port.try_recv() {
                out.push((index, event));
            }
        }
    }

    pub fn resolve(&mut self, link: usize, event: &MidiEvent, ctx: &Context<'_>) -> Resolution {
        match self.links.get_mut(link) {
            Some(link) => link.router.resolve(event, ctx),
            None => Resolution::Unbound,
        }
    }

    pub fn target_of(&self, link: usize, event: &MidiEvent) -> Option<Target> {
        self.links
            .get(link)?
            .router
            .map()
            .get(event.id())
            .map(|binding| binding.target.clone())
    }

    /// Put one report in the queue to the web view. A knob that is being swept coalesces.
    pub fn report(
        &mut self,
        link: usize,
        event: &MidiEvent,
        outcome: MidiOutcome,
        target: Option<&Target>,
        note: Option<String>,
    ) {
        let report = self.make_report(link, event, outcome, target, note);
        self.push(report);
    }

    fn push(&mut self, report: MidiEventReport) {
        if report.continuous
            && let Some(slot) = self
                .pending
                .iter_mut()
                .rev()
                .find(|old| old.id == report.id && old.port == report.port && old.continuous)
        {
            *slot = report;
            return;
        }
        if self.pending.len() >= PENDING_MAX {
            self.pending.remove(0);
        }
        self.pending.push(report);
    }

    fn make_report(
        &mut self,
        link: usize,
        event: &MidiEvent,
        outcome: MidiOutcome,
        target: Option<&Target>,
        note: Option<String>,
    ) -> MidiEventReport {
        self.seq += 1;
        let id = event.id();
        let (port, device) = match self.links.get(link) {
            Some(l) => (
                Some(l.port.name().to_string()),
                self.store.get(&l.profile).map(|e| e.device.clone()),
            ),
            None => (None, None),
        };
        MidiEventReport {
            seq: self.seq,
            id: id.to_string(),
            short: id.short(),
            channel: id.channel,
            what: what(event).to_string(),
            number: match id.kind {
                MidiIdKind::Bend => None,
                _ => Some(id.number),
            },
            value: value_of(event),
            target: target.map(|t| t.to_string()),
            target_label: target.map(|t| t.label()),
            outcome,
            note,
            continuous: matches!(
                event,
                MidiEvent::ControlChange { .. } | MidiEvent::PitchBend { .. }
            ),
            port,
            device,
        }
    }

    /// The batch to emit, if it is time.
    pub fn take_feed(&mut self) -> Option<MidiFeed> {
        if self.pending.is_empty() && !self.view_changed {
            return None;
        }
        let discrete = self.pending.iter().any(|report| !report.continuous);
        if !discrete && !self.view_changed && self.last_emit.elapsed() < EMIT_INTERVAL {
            return None;
        }
        self.last_emit = Instant::now();
        let view = if self.view_changed {
            self.view_changed = false;
            Some(self.view())
        } else {
            None
        };
        Some(MidiFeed {
            events: std::mem::take(&mut self.pending),
            view,
        })
    }

    pub fn say(&mut self, message: impl Into<String>) {
        self.message = Some(message.into());
        self.view_changed = true;
    }

    // ---- the overview ----------------------------------------------------------------------

    pub fn view(&self) -> MidiView {
        let mut bindings = Vec::new();
        let mut devices = Vec::new();
        for entry in self.store.entries() {
            let link = self.links.iter().find(|l| l.profile == entry.path);
            let port_here = self
                .available
                .iter()
                .find(|port| device_matches(&entry.device, port));
            let failure = port_here.and_then(|port| {
                self.failures
                    .iter()
                    .find(|(n, _)| n == port)
                    .map(|(_, e)| e.clone())
            });
            let state = if entry.error.is_some() {
                DeviceState::Error
            } else if link.is_some() {
                DeviceState::Connected
            } else if port_here.is_some_and(|p| self.suppressed.contains(p)) {
                DeviceState::Disconnected
            } else if failure.is_some() {
                DeviceState::Unavailable
            } else {
                DeviceState::Missing
            };

            let mut active = 0;
            if let Some(profile) = &entry.profile {
                for (id, binding) in profile.map.sorted() {
                    let (state, reason, resolved) = if link.is_none() {
                        (
                            BindingState::DeviceMissing,
                            Some(match state {
                                DeviceState::Disconnected => {
                                    format!("\"{}\" was disconnected by hand.", entry.device)
                                }
                                DeviceState::Unavailable => format!(
                                    "\"{}\" is plugged in but could not be opened.",
                                    entry.device
                                ),
                                _ => format!("\"{}\" is not plugged in.", entry.device),
                            }),
                            check_target(&binding.target, &self.shape).resolved,
                        )
                    } else if let Some(over) = self.score.get(id) {
                        (
                            BindingState::Overridden,
                            Some(format!(
                                "The loaded score uses this control for \"{}\".",
                                over.target.label()
                            )),
                            check_target(&binding.target, &self.shape).resolved,
                        )
                    } else {
                        let check = check_target(&binding.target, &self.shape);
                        (check.state, check.reason, check.resolved)
                    };
                    if state.works() {
                        active += 1;
                    }
                    bindings.push(binding_view(
                        id,
                        binding,
                        Some(entry.device.clone()),
                        state,
                        reason,
                        resolved,
                    ));
                }
            }
            let total = entry.len();
            let inactive = total - active;
            let summary = match state {
                DeviceState::Error => format!(
                    "profile file has errors - left untouched: {}",
                    entry.error.as_deref().unwrap_or("")
                ),
                DeviceState::Connected if inactive == 0 => format!(
                    "connected - {total} {} active",
                    plural(total, "binding", "bindings")
                ),
                DeviceState::Connected => format!(
                    "connected - {active} of {total} bindings active, {inactive} without a target \
                     in this session"
                ),
                DeviceState::Missing => format!(
                    "not plugged in - {total} {} inactive",
                    plural(total, "binding", "bindings")
                ),
                DeviceState::Disconnected => format!(
                    "disconnected by hand - {total} {} inactive",
                    plural(total, "binding", "bindings")
                ),
                DeviceState::Unavailable => format!(
                    "plugged in but busy - {total} {} inactive",
                    plural(total, "binding", "bindings")
                ),
            };
            devices.push(MidiDeviceView {
                device: entry.device.clone(),
                path: entry.path.display().to_string(),
                state,
                connected: link.is_some(),
                port: link.map(|l| l.port.name().to_string()),
                bindings: total,
                active,
                inactive,
                error: entry.error.clone().or(failure),
                unsaved: entry.unsaved,
                last_seen: self
                    .seen
                    .iter()
                    .find(|(p, _)| *p == entry.path)
                    .map(|(_, at)| *at),
                summary,
            });
        }

        // The score's own bindings, once, whichever device sends them.
        for (id, binding) in self.score.sorted() {
            let (state, reason, resolved) = if self.links.is_empty() {
                (
                    BindingState::DeviceMissing,
                    Some("No MIDI device is connected.".to_string()),
                    check_target(&binding.target, &self.shape).resolved,
                )
            } else {
                let check = check_target(&binding.target, &self.shape);
                (check.state, check.reason, check.resolved)
            };
            let mut view = binding_view(id, binding, None, state, reason, resolved);
            view.from_score = true;
            bindings.push(view);
        }

        let first = self.links.first();
        MidiView {
            connected: !self.links.is_empty(),
            port: first.map(|l| l.port.name().to_string()),
            device: first.and_then(|l| self.store.get(&l.profile).map(|e| e.device.clone())),
            path: first.map(|l| l.profile.display().to_string()),
            dirty: self.store.entries().iter().any(|e| e.unsaved),
            learning: self.learn.pending().map(|t| t.to_string()),
            learning_label: self.learn.pending().map(|t| t.label()),
            bindings,
            devices,
            ports: self.port_views(),
            dir: self.store.dir().display().to_string(),
            session: MidiSessionView {
                running: self.shape.running(),
                tracks: self.shape.tracks.clone().unwrap_or_default(),
                sections: self.shape.sections,
            },
            notes: self.notes(),
            message: self.message.clone(),
            save_error: self.save_error.clone(),
            dropped: self.links.iter().map(|l| l.port.dropped()).sum(),
        }
    }
}

/// The same loose comparison a profile uses for its port.
fn device_matches(device: &str, port: &str) -> bool {
    looper_engine::midi::Profile::new(device).matches(port)
}

/// Which port a selector means: its number in the list, its exact name, or a unique part of it.
fn pick(selector: &str, names: &[String]) -> Result<String, String> {
    let selector = selector.trim();
    if names.is_empty() {
        return Err("There is no MIDI input. Is the controller plugged in?".to_string());
    }
    if selector.is_empty() && names.len() == 1 {
        return Ok(names[0].clone());
    }
    if let Ok(index) = selector.parse::<usize>()
        && let Some(name) = names.get(index)
    {
        return Ok(name.clone());
    }
    if let Some(name) = names.iter().find(|n| *n == selector) {
        return Ok(name.clone());
    }
    let needle = selector.to_lowercase();
    let hits: Vec<&String> = names
        .iter()
        .filter(|n| !needle.is_empty() && n.to_lowercase().contains(&needle))
        .collect();
    match hits.as_slice() {
        [one] => Ok((*one).clone()),
        _ => Err(format!(
            "\"{selector}\" does not name exactly one MIDI input. Available: {}.",
            names.join(", ")
        )),
    }
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as u64)
}

fn plural(n: usize, one: &'static str, many: &'static str) -> &'static str {
    if n == 1 { one } else { many }
}

fn binding_view(
    id: MidiId,
    binding: &Binding,
    device: Option<String>,
    state: BindingState,
    reason: Option<String>,
    resolved: Option<Target>,
) -> MidiBindingView {
    let control = binding.target.control();
    let range = match control {
        Control::Range(range) => Some(range),
        _ => None,
    };
    MidiBindingView {
        id: id.to_string(),
        short: id.short(),
        channel: id.channel,
        kind: match id.kind {
            MidiIdKind::Note => "note",
            MidiIdKind::Cc => "cc",
            MidiIdKind::Bend => "bend",
        }
        .to_string(),
        number: id.number,
        address: binding.target.to_string(),
        resolved_address: resolved.map(|t| t.to_string()),
        label: binding.target.label(),
        describe: binding.describe(),
        from_score: false,
        device,
        control: match control {
            Control::Trigger => "trigger",
            Control::Switch => "switch",
            Control::Range(_) => "range",
        }
        .to_string(),
        mode: range.is_none().then(|| binding.button.name().to_string()),
        modes: match control {
            Control::Trigger => vec!["press".into(), "release".into()],
            Control::Switch => ButtonMode::names().iter().map(|s| s.to_string()).collect(),
            Control::Range(_) => Vec::new(),
        },
        takeover: range.map(|_| binding.takeover.name().to_string()),
        min: binding.min,
        max: binding.max,
        range_min: range.map(|r| r.min),
        range_max: range.map(|r| r.max),
        state,
        reason,
    }
}

fn what(event: &MidiEvent) -> &'static str {
    match event {
        MidiEvent::NoteOn { .. } => "Note on",
        MidiEvent::NoteOff { .. } => "Note off",
        MidiEvent::ControlChange { .. } => "CC",
        MidiEvent::PitchBend { .. } => "Pitch bend",
    }
}

fn value_of(event: &MidiEvent) -> i32 {
    match *event {
        MidiEvent::NoteOn { velocity, .. } | MidiEvent::NoteOff { velocity, .. } => {
            i32::from(velocity)
        }
        MidiEvent::ControlChange { value, .. } => i32::from(value),
        MidiEvent::PitchBend { value, .. } => i32::from(value),
    }
}

#[cfg(test)]
pub(crate) mod fake {
    //! A MIDI system in memory: ports appear and disappear, events are queued by hand.

    use std::cell::RefCell;
    use std::collections::{HashMap, VecDeque};
    use std::rc::Rc;

    use super::*;

    #[derive(Default)]
    struct State {
        ports: Vec<String>,
        busy: Vec<String>,
        queues: HashMap<String, VecDeque<MidiEvent>>,
        opened: Vec<String>,
    }

    #[derive(Clone, Default)]
    pub struct FakeMidi(Rc<RefCell<State>>);

    impl FakeMidi {
        pub fn plug(&self, name: &str) {
            let mut s = self.0.borrow_mut();
            if !s.ports.iter().any(|p| p == name) {
                s.ports.push(name.to_string());
            }
        }
        pub fn unplug(&self, name: &str) {
            let mut s = self.0.borrow_mut();
            s.ports.retain(|p| p != name);
            s.queues.remove(name);
        }
        /// Make opening fail, like Windows when Ableton holds the device.
        pub fn hold(&self, name: &str) {
            self.0.borrow_mut().busy.push(name.to_string());
        }
        pub fn send(&self, name: &str, event: MidiEvent) {
            self.0
                .borrow_mut()
                .queues
                .entry(name.to_string())
                .or_default()
                .push_back(event);
        }
        pub fn opened(&self) -> Vec<String> {
            self.0.borrow().opened.clone()
        }
    }

    struct FakePort {
        name: String,
        state: Rc<RefCell<State>>,
    }

    impl MidiPort for FakePort {
        fn name(&self) -> &str {
            &self.name
        }
        fn try_recv(&mut self) -> Option<MidiEvent> {
            self.state
                .borrow_mut()
                .queues
                .get_mut(&self.name)?
                .pop_front()
        }
        fn dropped(&self) -> u64 {
            0
        }
    }

    impl MidiSystem for FakeMidi {
        fn ports(&mut self) -> Result<Vec<String>, String> {
            Ok(self.0.borrow().ports.clone())
        }
        fn open(&mut self, name: &str) -> Result<Box<dyn MidiPort>, String> {
            let mut s = self.0.borrow_mut();
            if !s.ports.iter().any(|p| p == name) {
                return Err(format!("no port {name}"));
            }
            if s.busy.iter().any(|p| p == name) {
                return Err("MMSYSERR_ALLOCATED".to_string());
            }
            s.opened.push(name.to_string());
            s.queues.entry(name.to_string()).or_default();
            Ok(Box::new(FakePort {
                name: name.to_string(),
                state: Rc::clone(&self.0),
            }))
        }
    }
}

#[cfg(test)]
mod tests;
