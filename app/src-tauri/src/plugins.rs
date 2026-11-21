//! Plugin slots in the app: which plugin sits in which insert slot, its parameters, its window,
//! and getting it into the running engine.
//!
//! ```text
//!  Tauri-Kommando ─► spawn_blocking ─► run_on_main_thread ─► Manager (Hauptthread)
//!                                                             │  laden, Parameter, Zustand, Fenster
//!                                                             │  activate() ─► Prozessor
//!                                                             ▼
//!  Host-Thread: Session::start ─► engine_started(Sender) ──► LINK ─► InsertMsg ─► Audio-Thread (Rack)
//!               Session::shutdown ─► engine_stopped()       ◄── Retire-Queue ◄──┘
//! ```
//!
//! # Why the main thread
//!
//! Every loaded plugin lives on the main thread, for the whole of its life. VST3 requires it for
//! the editor and most plugins (everything built on JUCE) assume it for everything else; AppKit
//! requires it for windows. Tauri commands are not on the main thread, so they hand their work over
//! with `run_on_main_thread` and wait for the answer on a channel - the same shape as the engine's
//! host thread, one thread over.
//!
//! # Surviving an engine restart
//!
//! The slots outlive a session the way the MIDI bridge does: stopping the engine, changing the
//! device or loading a score rebuilds the engine core, and with it the rack inside it. Every start
//! publishes a new [`Link`] with a new generation; [`Manager::reconcile`] notices, deactivates the
//! plugins (their processors went down with the old core), activates them at the new sample rate
//! and installs them again.
//!
//! # Surviving a restart of the program
//!
//! The rack is written to `plugin-rack.json` next to the MIDI profiles - which plugin in which slot,
//! bypass, and the plugin's own state chunk - and loaded again at the next start. A marker file is
//! written while that happens: if a plugin takes the program down while it is being restored, the
//! next start finds the marker and does **not** restore again, so a broken plugin cannot lock the
//! user out of his own program.
//!
//! # Owned by track *name*
//!
//! A slot belongs to a track by its name (buses by `main` / `monitor`), not by its position. A
//! score that lists its tracks in another order than the setup - or has fewer of them - would
//! otherwise put the singer's de-esser on the guitar. Every engine start hands over the new track
//! list ([`engine_started`]); a slot whose track is in it is installed at that track's position,
//! one whose track is not is **parked**: it stays loaded and in the file, and goes back in as soon
//! as a track of that name exists again. A rename the host recognises (`crate::layout`) moves the
//! slots along.
//!
//! The file of the version before this was keyed by position (`track.1`). It is read as well: a
//! position is turned into the name the track had then where the last layout is known, and is
//! otherwise kept as a position and bound once, to the track at that position in the first engine
//! that starts. Nothing is dropped on the way.
//!
//! The commands and the UI still say `track.1` - the position in the running engine, which is
//! what a strip is. The MIDI addresses of a slot are spelled with the name
//! (`track.stimme.insert.1.bypass`), so a learned knob follows the plugin when the tracks move.

use std::cell::RefCell;
use std::collections::{BTreeMap, HashMap};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock, mpsc};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Emitter};

use looper_engine::engine::bus::Bus;
use looper_engine::engine::command::MAX_TRACKS;
use looper_engine::engine::insert::{
    Insert, InsertEndpoint, InsertMsg, InsertPosition, InsertSender, InsertShared, InsertTarget,
    MAX_BLOCK, MAX_INSERTS, insert_channel,
};
use looper_engine::midi::MidiAction;
use looper_engine::score::mixer::InsertLine;
use looper_engine::plugin::scan::{self, Scanned};
use looper_engine::plugin::{self, PluginInstance};

use crate::layout;
use crate::logfile::log;
use crate::scorestate;

/// Event for everything this module reports: the rack, parameter changes made in a plugin's own
/// window, scan progress. See [`PluginEvent`].
pub const PLUGINS_EVENT: &str = "looper://plugins";

/// How often the main thread looks after the plugins: editor changes, latency, autosave.
const TICK: Duration = Duration::from_millis(250);
/// How long removing a plugin waits for the audio thread to hand its processor back.
const RETIRE_WAIT: Duration = Duration::from_millis(750);

// ---------------------------------------------------------------------------------------------
// The link to the running engine
// ---------------------------------------------------------------------------------------------

struct Link {
    sender: InsertSender,
    sample_rate: f64,
    generation: u64,
}

static LINK: Mutex<Option<Link>> = Mutex::new(None);
static GENERATION: AtomicU64 = AtomicU64::new(0);
static APP: OnceLock<AppHandle> = OnceLock::new();

/// A fresh queue pair for an engine about to be built. The endpoint goes into the core.
pub fn engine_channel() -> (InsertSender, InsertEndpoint) {
    insert_channel(64)
}

/// Called by the host thread once a new engine runs, with its track names in order and the
/// renames since the last start. Its plugins are (re)installed on the main thread right after.
pub fn engine_started(
    sender: InsertSender,
    sample_rate: u32,
    tracks: Vec<String>,
    renamed: Vec<(String, String)>,
) {
    let generation = GENERATION.fetch_add(1, Ordering::Relaxed) + 1;
    *LINK.lock().unwrap() = Some(Link {
        sender,
        sample_rate: f64::from(sample_rate),
        generation,
    });
    schedule(move |m| {
        m.set_layout(tracks, &renamed);
        m.reconcile();
    });
}

/// Called by the host thread after the engine's streams are gone - and with them every processor
/// that was installed. The plugins are deactivated on the main thread right after.
pub fn engine_stopped() {
    LINK.lock().unwrap().take();
    schedule(|m| m.reconcile());
}

// ---------------------------------------------------------------------------------------------
// What MIDI reads: a snapshot, kept by the manager
// ---------------------------------------------------------------------------------------------

#[derive(Clone, Default)]
struct SlotSnapshot {
    bypass: bool,
    params: HashMap<u32, f64>,
}

fn snapshot() -> &'static Mutex<HashMap<(InsertTarget, usize), SlotSnapshot>> {
    static SNAP: OnceLock<Mutex<HashMap<(InsertTarget, usize), SlotSnapshot>>> = OnceLock::new();
    SNAP.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Current bypass of a slot, for a MIDI toggle. `None` when the slot is empty.
pub fn snapshot_bypass(target: InsertTarget, slot: usize) -> Option<bool> {
    snapshot().lock().unwrap().get(&(target, slot)).map(|s| s.bypass)
}

/// Current normalised value of a plugin parameter, for MIDI pickup.
pub fn snapshot_param(target: InsertTarget, slot: usize, id: u32) -> Option<f32> {
    snapshot()
        .lock()
        .unwrap()
        .get(&(target, slot))
        .and_then(|s| s.params.get(&id))
        .map(|v| *v as f32)
}

/// A resolved MIDI action on a plugin slot. Called on the host thread; the work is handed to the
/// main thread without waiting, so a knob never stalls the MIDI queue.
pub fn apply_midi(action: &MidiAction) -> Result<(), String> {
    match *action {
        MidiAction::InsertBypass { target, slot, on } => {
            {
                let mut snap = snapshot().lock().unwrap();
                let entry = snap
                    .get_mut(&(target, slot))
                    .ok_or_else(|| empty_slot(target, slot))?;
                entry.bypass = on;
            }
            schedule(move |m| {
                if let Ok(owner) = m.owner(target)
                    && m.bypass(&owner, slot, on).is_ok()
                {
                    m.touched(&owner);
                }
            });
            Ok(())
        }
        MidiAction::InsertParam {
            target,
            slot,
            param,
            value,
        } => {
            {
                let mut snap = snapshot().lock().unwrap();
                let entry = snap
                    .get_mut(&(target, slot))
                    .ok_or_else(|| empty_slot(target, slot))?;
                entry.params.insert(param, f64::from(value));
            }
            schedule(move |m| {
                if let Ok(owner) = m.owner(target) {
                    let _ = m.set_param(&owner, slot, param, f64::from(value), true);
                }
            });
            Ok(())
        }
        _ => Err("Not a plugin action.".to_string()),
    }
}

fn empty_slot(target: InsertTarget, slot: usize) -> String {
    format!(
        "Plugin slot {} of {} is empty.",
        slot + 1,
        target_label(target)
    )
}

fn empty_owned_slot(owner: &Owner, slot: usize) -> String {
    format!("Plugin slot {} of {} is empty.", slot + 1, owner.label())
}

fn target_label(target: InsertTarget) -> String {
    match target {
        InsertTarget::Track(t) => format!("track {}", t + 1),
        InsertTarget::Bus(b) => format!("the {} bus", b.name()),
    }
}

// ---------------------------------------------------------------------------------------------
// Running things on the main thread
// ---------------------------------------------------------------------------------------------

thread_local! {
    static MANAGER: RefCell<Manager> = RefCell::new(Manager::default());
}

/// Run `work` on the main thread and wait for its answer. Never call this *from* the main thread.
fn on_main<T, F>(work: F) -> Result<T, String>
where
    T: Send + 'static,
    F: FnOnce(&mut Manager) -> Result<T, String> + Send + 'static,
{
    let app = APP
        .get()
        .ok_or_else(|| "The plugin host is not ready yet.".to_string())?;
    let (tx, rx) = mpsc::channel();
    app.run_on_main_thread(move || {
        let result = MANAGER.with(|m| match m.try_borrow_mut() {
            Ok(mut m) => work(&mut m),
            Err(_) => Err("The plugin host is busy. Try again.".to_string()),
        });
        let _ = tx.send(result);
    })
    .map_err(|e| format!("The plugin host is not reachable: {e}"))?;
    rx.recv()
        .map_err(|_| "The plugin host did not answer.".to_string())?
}

/// Run `work` on the main thread later, without waiting.
fn schedule(work: impl FnOnce(&mut Manager) + Send + 'static) {
    if let Some(app) = APP.get() {
        let _ = app.run_on_main_thread(move || {
            MANAGER.with(|m| {
                if let Ok(mut m) = m.try_borrow_mut() {
                    work(&mut m);
                }
            });
        });
    }
}

/// Start the plugin host. Call from Tauri's `setup` (on the main thread).
pub fn init(app: &AppHandle) {
    let _ = APP.set(app.clone());
    MANAGER.with(|m| m.borrow_mut().catalog = scan::cached());
    // Restoring loads plugin code; it happens after the window is up, not before.
    schedule(|m| m.restore());
    std::thread::Builder::new()
        .name("plugin-tick".into())
        .spawn(|| {
            loop {
                std::thread::sleep(TICK);
                schedule(|m| m.tick());
            }
        })
        .ok();
}

/// On the way out of the program, on the main thread, **after** the engine has stopped: save the
/// rack, then close every window and unload every plugin in order (window, deactivate, the
/// instance itself). A plugin the engine still holds is left alone - destroying it while it may
/// be processing would be worse than leaking it. Returns how many were left alone.
pub fn shutdown() -> usize {
    MANAGER.with(|m| match m.try_borrow_mut() {
        Ok(mut m) => m.shutdown(),
        Err(_) => {
            log!("Plugin host busy on the way out; plugins are left loaded.");
            usize::MAX
        }
    })
}

// ---------------------------------------------------------------------------------------------
// Wire format
// ---------------------------------------------------------------------------------------------

/// One occupied slot as the UI sees it.
#[derive(Clone, Debug, Serialize)]
pub struct SlotView {
    /// `track.1` … `track.8`, `bus.main`, `bus.monitor`.
    pub target: String,
    /// 0-based slot index; the UI shows it 1-based.
    pub slot: usize,
    pub plugin_id: String,
    pub name: String,
    pub vendor: String,
    /// `vst3` or `au`.
    pub format: plugin::Format,
    pub bypass: bool,
    /// Latency the plugin reports, in frames. Made up for on the track's loop playback.
    pub latency: u32,
    pub has_editor: bool,
    pub editor_open: bool,
    /// Installed in the running engine.
    pub active: bool,
    /// Blocks processed so far - grows while audio goes through the slot.
    pub blocks: u64,
    /// Output peak since the last report, linear.
    pub peak: f32,
    /// Longest process call since the last report, in microseconds.
    pub cpu_us: f32,
    /// What went wrong when the plugin was started, if anything.
    pub error: Option<String>,
    /// MIDI address of the slot's bypass, e.g. `track.stimme.insert.2.bypass`.
    pub bypass_address: String,
    /// Name of the track the slot belongs to; `None` on a bus.
    pub track: Option<String>,
    /// The built-in effect the plugin sits in front of (`octaver` … `reverb`); `None` = after the
    /// built-in chain (always on a bus).
    pub before: Option<String>,
}

/// A slot whose track is not in the running engine: kept, and back when the track is.
#[derive(Clone, Debug, Serialize)]
pub struct ParkedView {
    /// The track's name - or `track 3` for a slot from an old rack file that has not met an engine
    /// yet.
    pub track: String,
    pub slot: usize,
    pub plugin_id: String,
    pub name: String,
}

/// Every occupied slot, plus whether plugins can run at all.
#[derive(Clone, Debug, Serialize)]
pub struct RackView {
    pub supported: bool,
    pub engine_running: bool,
    pub sample_rate: Option<f64>,
    pub max_slots: usize,
    pub slots: Vec<SlotView>,
    pub parked: Vec<ParkedView>,
}

/// The plugins this machine has, from the scan.
#[derive(Clone, Debug, Serialize)]
pub struct CatalogView {
    pub supported: bool,
    pub plugins: Vec<Scanned>,
    /// Where the scan result is kept.
    pub cache_path: String,
}

/// One parameter, with its current value and the plugin's own text for it.
#[derive(Clone, Debug, Serialize)]
pub struct ParamView {
    pub id: u32,
    pub name: String,
    pub unit: String,
    /// Normalised 0..1.
    pub value: f64,
    pub text: String,
    pub default: f64,
    pub steps: u32,
    pub automatable: bool,
    pub read_only: bool,
    pub is_bypass: bool,
    /// MIDI address, e.g. `track.stimme.insert.1.param.42`.
    pub address: String,
}

/// A parameter value that changed, e.g. in the plugin's own window.
#[derive(Clone, Debug, Serialize)]
pub struct ParamValue {
    pub id: u32,
    pub value: f64,
    pub text: String,
}

/// Payload of [`PLUGINS_EVENT`].
#[derive(Clone, Debug, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum PluginEvent {
    /// The rack changed (or its meters did - sent about once a second while anything is loaded).
    Rack { rack: RackView },
    /// Parameters of one slot changed outside the UI's own controls.
    Params {
        target: String,
        slot: usize,
        values: Vec<ParamValue>,
    },
    /// The scan is probing this plugin now.
    Scan { label: String },
    /// The scan finished.
    Catalog { catalog: CatalogView },
    /// Something the user should read.
    Notice { message: String },
}

fn emit(event: PluginEvent) {
    if let Some(app) = APP.get() {
        let _ = app.emit(PLUGINS_EVENT, event);
    }
}

/// `track.1` (1-based) or `bus.main` / `bus.monitor`.
pub fn parse_target(text: &str) -> Result<InsertTarget, String> {
    let bad = || {
        format!(
            "'{text}' is not a slot owner. Use track.1 … track.{MAX_TRACKS}, bus.main or bus.monitor."
        )
    };
    match text.split_once('.') {
        Some(("track", n)) => {
            let n: usize = n.parse().map_err(|_| bad())?;
            if n == 0 || n > MAX_TRACKS {
                return Err(bad());
            }
            Ok(InsertTarget::Track(n - 1))
        }
        Some(("bus", name)) => Bus::parse(name).map(InsertTarget::Bus).ok_or_else(bad),
        _ => Err(bad()),
    }
}

/// The MIDI owner prefix: `track.stimme` or `global.bus.main`. A name that cannot stand in an
/// address (digits only, a dot, a space) is spelled by position, `track.1`.
fn midi_owner(owner: &Owner, target: Option<InsertTarget>) -> String {
    match (owner, target) {
        (Owner::Track(name), _) if addressable(name) => format!("track.{name}"),
        (Owner::Bus(b), _) => format!("global.bus.{}", b.name()),
        (_, Some(InsertTarget::Track(t))) => format!("track.{}", t + 1),
        (Owner::Track(name), _) => format!("track.{name}"),
        (Owner::Position(p), _) => format!("track.{}", p + 1),
    }
}

fn addressable(name: &str) -> bool {
    !name.is_empty()
        && !name.bytes().all(|b| b.is_ascii_digit())
        && name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
}

/// Whom a slot belongs to.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
enum Owner {
    Track(String),
    Bus(Bus),
    /// From a rack file of the old, position-keyed format, until the first engine says which track
    /// stands there. 0-based.
    Position(usize),
}

impl Owner {
    fn label(&self) -> String {
        match self {
            Owner::Track(name) => format!("track \"{name}\""),
            Owner::Bus(b) => format!("the {} bus", b.name()),
            Owner::Position(p) => format!("track {}", p + 1),
        }
    }
}

// ---------------------------------------------------------------------------------------------
// The manager
// ---------------------------------------------------------------------------------------------

struct Slot {
    owner: Owner,
    index: usize,
    /// Where in the track's chain it runs (`InsertPosition::END` on a bus). The slots of an owner
    /// are kept sorted by it, so slot order is signal order.
    position: InsertPosition,
    info: Scanned,
    plugin: Box<dyn PluginInstance>,
    shared: Arc<InsertShared>,
    /// Engine generation the processor was installed into, `None` while not running.
    active_gen: Option<u64>,
    error: Option<String>,
    editor_was_open: bool,
    /// Whether the first audio through this slot has been logged for the current activation.
    heard: bool,
}

/// How [`Manager::load_as`] starts a plugin: where in the chain, with which state, bypassed or not.
#[derive(Default, Clone, Copy)]
struct LoadAs<'a> {
    position: InsertPosition,
    state: Option<&'a [u8]>,
    bypass: bool,
}

#[derive(Default)]
struct Manager {
    slots: Vec<Slot>,
    /// Slots of the file that could not be loaded this time - written back unchanged.
    lost: Vec<(Owner, SavedSlot)>,
    /// Track names of the running engine, in order - or of the last one, while none runs.
    layout: Vec<String>,
    catalog: Vec<Scanned>,
    /// The rack changed since it was last written to disk.
    dirty: bool,
    last_change: Option<Instant>,
    last_meter: Option<Instant>,
    /// Tests: plain-Rust stand-ins instead of real plugins (`tests::fake_load`) - no installed
    /// plugin is ever loaded by a test.
    #[cfg(test)]
    fake_plugins: bool,
}

/// `plugin-rack.json` as written since slots belong to names.
#[derive(Debug, Default, Serialize, Deserialize, PartialEq)]
struct SavedRack {
    version: u32,
    /// Slots of tracks, by track name - including tracks the running engine does not have.
    #[serde(default)]
    tracks: BTreeMap<String, Vec<SavedSlot>>,
    /// Slots of the two buses, by `main` / `monitor`.
    #[serde(default)]
    buses: BTreeMap<String, Vec<SavedSlot>>,
    /// Slots of an old file whose track name could not be told yet, by 1-based position.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    positions: BTreeMap<String, Vec<SavedSlot>>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
struct SavedSlot {
    slot: usize,
    plugin_id: String,
    name: String,
    bypass: bool,
    /// The plugin's state chunk, base64.
    state: Option<String>,
    /// The built-in effect the plugin sits in front of (`eq`, ...); absent = after the built-in
    /// chain, which is what every file written before positions existed means.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    before: Option<String>,
}

impl SavedSlot {
    fn position(&self) -> InsertPosition {
        InsertPosition::parse(self.before.as_deref()).unwrap_or_default()
    }
}

/// The file of the version before: one list, keyed by `track.<n>` / `bus.<name>`.
#[derive(Deserialize)]
struct OldRack {
    slots: Vec<OldSlot>,
}

#[derive(Deserialize)]
struct OldSlot {
    target: String,
    slot: usize,
    plugin_id: String,
    name: String,
    bypass: bool,
    state: Option<String>,
}

const RACK_VERSION: u32 = 2;

/// Read a rack file of either format. An old one is converted: `track.<n>` becomes the name the
/// `n`th track has in `layout` (the last one the engine ran with), or stays a position when that is
/// not known. Nothing is dropped.
fn read_rack(text: &str, layout: &[String]) -> Option<SavedRack> {
    if let Ok(rack) = serde_json::from_str::<SavedRack>(text)
        && rack.version >= RACK_VERSION
    {
        return Some(rack);
    }
    let old = serde_json::from_str::<OldRack>(text).ok()?;
    let mut rack = SavedRack {
        version: RACK_VERSION,
        ..SavedRack::default()
    };
    for o in old.slots {
        let saved = SavedSlot {
            slot: o.slot,
            plugin_id: o.plugin_id,
            name: o.name,
            bypass: o.bypass,
            state: o.state,
            before: None,
        };
        let list = match parse_target(&o.target) {
            Ok(InsertTarget::Track(t)) => match layout.get(t) {
                Some(name) => rack.tracks.entry(name.clone()).or_default(),
                None => rack.positions.entry((t + 1).to_string()).or_default(),
            },
            Ok(InsertTarget::Bus(b)) => rack.buses.entry(b.name().to_string()).or_default(),
            // Not an owner either format knows: kept under its own spelling rather than lost.
            Err(_) => rack.tracks.entry(o.target.clone()).or_default(),
        };
        list.push(saved);
    }
    Some(rack)
}

impl SavedRack {
    /// Every slot with its owner, in a fixed order.
    fn entries(self) -> Vec<(Owner, SavedSlot)> {
        let mut out = Vec::new();
        for (bus, slots) in self.buses {
            if let Some(b) = Bus::parse(&bus) {
                out.extend(slots.into_iter().map(|s| (Owner::Bus(b), s)));
            }
        }
        for (name, slots) in self.tracks {
            out.extend(slots.into_iter().map(|s| (Owner::Track(name.clone()), s)));
        }
        for (position, slots) in self.positions {
            if let Some(p) = position.parse::<usize>().ok().and_then(|p| p.checked_sub(1)) {
                out.extend(slots.into_iter().map(|s| (Owner::Position(p), s)));
            }
        }
        out
    }

    fn add(&mut self, owner: &Owner, slot: SavedSlot) {
        let list = match owner {
            Owner::Track(name) => self.tracks.entry(name.clone()).or_default(),
            Owner::Bus(b) => self.buses.entry(b.name().to_string()).or_default(),
            Owner::Position(p) => self.positions.entry((p + 1).to_string()).or_default(),
        };
        list.push(slot);
        list.sort_by_key(|s| s.slot);
    }
}

/// Where the slots of `owners` go when the track list changes from `old` to `new`, with the
/// renames found by the host. Pure, so it can be tested without a plugin in sight.
///
/// * a renamed track takes its slots along - unless the new name already has slots of its own;
/// * a position from an old file is bound to the track standing there now;
/// * everything else keeps its owner (and is parked while its track is missing).
fn rehome(owners: &[Owner], new: &[String], renamed: &[(String, String)]) -> Vec<Owner> {
    let occupied = |name: &str| owners.iter().any(|o| *o == Owner::Track(name.to_string()));
    owners
        .iter()
        .map(|owner| match owner {
            Owner::Track(name) => match renamed.iter().find(|(from, _)| from == name) {
                Some((_, to)) if !occupied(to) => Owner::Track(to.clone()),
                _ => owner.clone(),
            },
            Owner::Position(p) => match new.get(*p) {
                Some(name) if !occupied(name) => Owner::Track(name.clone()),
                _ => owner.clone(),
            },
            Owner::Bus(_) => owner.clone(),
        })
        .collect()
}

// ---- the order of a chain, as plain data ------------------------------------------------------
//
// An owner's slots as `(slot index, position)`. Slot indices may have gaps (a removed plugin
// leaves its slot empty); the order is the order of the indices, and it is the signal order only
// while the positions never fall along it - which `in_signal_order` checks and every change keeps.

/// Where slot `index` ends up when the plugin in `from` is moved to `to` - the rotation the
/// audio thread does (`InsertRack`), which shifts the ones in between.
fn moved_index(index: usize, from: usize, to: usize) -> usize {
    if index == from {
        to
    } else if from < to && index > from && index <= to {
        index - 1
    } else if to < from && index >= to && index < from {
        index + 1
    } else {
        index
    }
}

/// The layout after moving `from` to `to` with the moved plugin at `position`, sorted by index.
fn after_place(
    layout: &[(usize, InsertPosition)],
    from: usize,
    to: usize,
    position: InsertPosition,
) -> Vec<(usize, InsertPosition)> {
    let mut out: Vec<(usize, InsertPosition)> = layout
        .iter()
        .map(|&(i, p)| (moved_index(i, from, to), if i == from { position } else { p }))
        .collect();
    out.sort_by_key(|(i, _)| *i);
    out
}

/// Whether slot order is signal order: the positions never fall along the indices.
fn in_signal_order(layout: &[(usize, InsertPosition)]) -> bool {
    let mut sorted = layout.to_vec();
    sorted.sort_by_key(|(i, _)| *i);
    sorted.windows(2).all(|w| w[0].1 <= w[1].1)
}

/// The place a plugin moved among the plugins (no place given) takes: its own, if that still fits
/// between its new neighbours, otherwise the place of the neighbour it now follows (or precedes,
/// at the front).
fn adopted_position(
    layout: &[(usize, InsertPosition)],
    from: usize,
    to: usize,
    current: InsertPosition,
) -> InsertPosition {
    let moved = after_place(layout, from, to, current);
    let prev = moved.iter().rev().find(|(i, _)| *i < to).map(|(_, p)| *p);
    let next = moved.iter().find(|(i, _)| *i > to).map(|(_, p)| *p);
    match (prev, next) {
        (Some(p), _) if current < p => p,
        (_, Some(n)) if current > n => n,
        _ => current,
    }
}

/// The slot a plugin just loaded into the free slot `free` has to move to so that it sits after
/// every plugin at its position or further forward, and before every one further back. `layout`
/// is the owner's other plugins.
fn placement_index(layout: &[(usize, InsertPosition)], free: usize, position: InsertPosition) -> usize {
    let last_before = layout.iter().filter(|(_, p)| *p <= position).map(|(i, _)| *i).max();
    let first_after = layout.iter().filter(|(_, p)| *p > position).map(|(i, _)| *i).min();
    match (last_before, first_after) {
        (Some(b), _) if free < b => b,
        (_, Some(a)) if free > a => a,
        _ => free,
    }
}

fn rack_path() -> PathBuf {
    scan::cache_path().with_file_name("plugin-rack.json")
}

fn restore_marker() -> PathBuf {
    scan::cache_path().with_file_name("plugin-rack.restoring")
}

impl Manager {
    /// The owner a command's `track.<n>` / `bus.<name>` means in the current layout.
    fn owner(&self, target: InsertTarget) -> Result<Owner, String> {
        match target {
            InsertTarget::Bus(b) => Ok(Owner::Bus(b)),
            InsertTarget::Track(t) => self.layout.get(t).cloned().map(Owner::Track).ok_or_else(|| {
                format!(
                    "There is no track {} - the engine has {} track(s).",
                    t + 1,
                    self.layout.len()
                )
            }),
        }
    }

    /// Where an owner sits in the running engine, if it does.
    fn target_of(&self, owner: &Owner) -> Option<InsertTarget> {
        match owner {
            Owner::Bus(b) => Some(InsertTarget::Bus(*b)),
            Owner::Track(name) => self
                .layout
                .iter()
                .position(|n| n == name)
                .map(InsertTarget::Track),
            Owner::Position(_) => None,
        }
    }

    /// A new track list. Renamed tracks take their slots along, old positions are bound.
    fn set_layout(&mut self, tracks: Vec<String>, renamed: &[(String, String)]) {
        let owners: Vec<Owner> = self.slots.iter().map(|s| s.owner.clone()).collect();
        let homes = rehome(&owners, &tracks, renamed);
        let mut moved = false;
        for (slot, home) in self.slots.iter_mut().zip(homes) {
            if slot.owner != home {
                log!(
                    "Plugin {} slot {}: {} -> {}.",
                    slot.info.info.name,
                    slot.index + 1,
                    slot.owner.label(),
                    home.label()
                );
                slot.owner = home;
                moved = true;
            }
        }
        self.layout = tracks;
        if moved {
            self.dirty = true;
            self.last_change = Some(Instant::now());
        }
        self.refresh_snapshot();
    }

    fn position(&self, owner: &Owner, index: usize) -> Option<usize> {
        self.slots
            .iter()
            .position(|s| s.owner == *owner && s.index == index)
    }

    fn slot(&self, owner: &Owner, index: usize) -> Result<&Slot, String> {
        self.position(owner, index)
            .map(|p| &self.slots[p])
            .ok_or_else(|| empty_owned_slot(owner, index))
    }

    fn slot_mut(&mut self, owner: &Owner, index: usize) -> Result<&mut Slot, String> {
        let p = self
            .position(owner, index)
            .ok_or_else(|| empty_owned_slot(owner, index))?;
        Ok(&mut self.slots[p])
    }

    fn view(&self) -> RackView {
        let link = LINK.lock().unwrap();
        let mut slots = Vec::new();
        let mut parked = Vec::new();
        for s in &self.slots {
            let Some(target) = self.target_of(&s.owner) else {
                parked.push(ParkedView {
                    track: match &s.owner {
                        Owner::Track(name) => name.clone(),
                        other => other.label(),
                    },
                    slot: s.index,
                    plugin_id: s.info.info.id.clone(),
                    name: s.info.info.name.clone(),
                });
                continue;
            };
            slots.push(SlotView {
                target: target.address(),
                slot: s.index,
                plugin_id: s.info.info.id.clone(),
                name: s.info.info.name.clone(),
                vendor: s.info.info.vendor.clone(),
                format: s.info.info.format,
                bypass: s.shared.bypass(),
                latency: s.shared.latency(),
                has_editor: s.plugin.has_editor(),
                editor_open: s.plugin.editor_open(),
                active: s.active_gen.is_some()
                    && s.active_gen == link.as_ref().map(|l| l.generation),
                blocks: s.shared.blocks(),
                peak: s.shared.take_peak(),
                cpu_us: s.shared.take_peak_us(),
                error: s.error.clone(),
                bypass_address: format!(
                    "{}.insert.{}.bypass",
                    midi_owner(&s.owner, Some(target)),
                    s.index + 1
                ),
                track: match &s.owner {
                    Owner::Track(name) => Some(name.clone()),
                    _ => None,
                },
                before: s.position.name().map(str::to_string),
            });
        }
        slots.sort_by(|a, b| (a.target.clone(), a.slot).cmp(&(b.target.clone(), b.slot)));
        parked.sort_by(|a, b| (a.track.clone(), a.slot).cmp(&(b.track.clone(), b.slot)));
        RackView {
            supported: plugin::SUPPORTED,
            engine_running: link.is_some(),
            sample_rate: link.as_ref().map(|l| l.sample_rate),
            max_slots: MAX_INSERTS,
            slots,
            parked,
        }
    }

    fn changed(&mut self) {
        self.dirty = true;
        self.last_change = Some(Instant::now());
        self.refresh_snapshot();
        emit(PluginEvent::Rack { rack: self.view() });
    }

    fn refresh_snapshot(&self) {
        // Which plugins sit on which track, for the score's `mixer:` (`crate::pluginmix`).
        let mut by_track: HashMap<String, Vec<(usize, InsertLine)>> = HashMap::new();
        for s in &self.slots {
            if let Owner::Track(name) = &s.owner {
                by_track.entry(name.clone()).or_default().push((
                    s.index,
                    InsertLine {
                        plugin: s.info.info.id.clone(),
                        bypass: s.shared.bypass().then_some(true),
                        before: s.position.name().map(str::to_string),
                    },
                ));
            }
        }
        crate::pluginmix::publish_inserts(
            by_track
                .into_iter()
                .map(|(track, mut lines)| {
                    lines.sort_by_key(|(i, _)| *i);
                    (track, lines.into_iter().map(|(_, l)| l).collect())
                })
                .collect(),
        );
        let mut snap = snapshot().lock().unwrap();
        snap.clear();
        for s in &self.slots {
            let Some(target) = self.target_of(&s.owner) else {
                continue;
            };
            let params = s
                .plugin
                .params()
                .into_iter()
                .map(|p| (p.id, s.plugin.param(p.id)))
                .collect();
            snap.insert(
                (target, s.index),
                SlotSnapshot {
                    bypass: s.shared.bypass(),
                    params,
                },
            );
        }
    }

    fn catalog_entry(&mut self, plugin_id: &str) -> Result<Scanned, String> {
        if self.catalog.is_empty() {
            self.catalog = scan::cached();
        }
        let entry = self
            .catalog
            .iter()
            .find(|p| p.info.id == plugin_id)
            .cloned()
            .ok_or_else(|| {
                format!("No plugin with the id '{plugin_id}' is known. Scan for plugins first.")
            })?;
        if !entry.loadable() {
            return Err(format!(
                "\"{}\" cannot be loaded: {}",
                entry.info.name,
                entry
                    .message
                    .clone()
                    .unwrap_or_else(|| "it did not pass the scan.".to_string())
            ));
        }
        if entry.info.kind == plugin::Kind::Instrument {
            return Err(format!(
                "\"{}\" is an instrument. Choose it as a track's input (Setup, Input: Instrument).",
                entry.info.name
            ));
        }
        Ok(entry)
    }

    /// Start one slot's plugin in the running engine, if there is one.
    fn install(&mut self, p: usize) {
        let link_guard = LINK.lock().unwrap();
        let Some(link) = link_guard.as_ref() else {
            return;
        };
        let (sample_rate, generation) = (link.sample_rate, link.generation);
        drop(link_guard);
        let target = self.target_of(&self.slots[p].owner);
        let slot = &mut self.slots[p];
        if slot.active_gen == Some(generation) {
            return;
        }
        if slot.active_gen.take().is_some() {
            slot.plugin.deactivate();
        }
        // Parked: its track is not in this engine. Loaded, not running.
        let Some(target) = target else {
            return;
        };
        match slot.plugin.activate(sample_rate, MAX_BLOCK) {
            Ok(processor) => {
                slot.shared.set_latency(slot.plugin.latency());
                let msg = InsertMsg::Install {
                    target,
                    slot: slot.index,
                    insert: Insert::new(processor, slot.shared.clone()).at(slot.position),
                };
                let mut link_guard = LINK.lock().unwrap();
                match link_guard.as_mut() {
                    Some(link) if link.generation == generation => {
                        match link.sender.send(msg) {
                            Ok(()) => {
                                slot.active_gen = Some(generation);
                                slot.error = None;
                                slot.heard = false;
                                log!(
                                    "Plugin {} in {} slot {} started: {} Hz, latency {} frames.",
                                    slot.info.info.name,
                                    slot.owner.label(),
                                    slot.index + 1,
                                    sample_rate,
                                    slot.shared.latency()
                                );
                            }
                            Err(e) => slot.error = Some(e),
                        }
                    }
                    // The engine went away while the plugin was starting.
                    _ => {
                        drop(msg);
                        slot.plugin.deactivate();
                    }
                }
            }
            Err(e) => {
                log!("Plugin {} does not start: {e}", slot.info.info.name);
                slot.error = Some(e);
            }
        }
    }

    /// Bring every slot in line with the engine that runs now (or with none running).
    fn reconcile(&mut self) {
        let running = LINK.lock().unwrap().as_ref().map(|l| l.generation);
        for p in 0..self.slots.len() {
            match running {
                Some(_) => self.install(p),
                None => {
                    let slot = &mut self.slots[p];
                    if slot.active_gen.take().is_some() {
                        slot.plugin.deactivate();
                    }
                }
            }
        }
        if !self.slots.is_empty() {
            emit(PluginEvent::Rack { rack: self.view() });
        }
    }

    fn load(
        &mut self,
        owner: &Owner,
        index: Option<usize>,
        plugin_id: &str,
    ) -> Result<RackView, String> {
        self.load_as(owner, index, plugin_id, LoadAs::default())
    }

    /// [`Self::load`] with a place in the chain, a state and a bypass - all given to the plugin
    /// before it is started, so it never plays a block in its default sound.
    fn load_as(
        &mut self,
        owner: &Owner,
        index: Option<usize>,
        plugin_id: &str,
        how: LoadAs<'_>,
    ) -> Result<RackView, String> {
        if !plugin::SUPPORTED {
            return Err(plugin::UNSUPPORTED.to_string());
        }
        let index = match index {
            Some(i) if i < MAX_INSERTS => i,
            Some(i) => {
                return Err(format!(
                    "There is no slot {} - there are {MAX_INSERTS}.",
                    i + 1
                ));
            }
            None => (0..MAX_INSERTS)
                .find(|i| self.position(owner, *i).is_none())
                .ok_or_else(|| {
                    format!(
                        "All {MAX_INSERTS} plugin slots of {} are taken.",
                        owner.label()
                    )
                })?,
        };
        let entry = self.catalog_entry(plugin_id)?;
        if self.position(owner, index).is_some() {
            self.remove_slot(owner, index)?;
        }
        let mut instance = self.instantiate(&entry.info)?;
        log!(
            "Plugin loaded: {} ({}) in {} slot {}",
            entry.info.name,
            entry.info.format.label(),
            owner.label(),
            index + 1
        );
        if let Some(state) = how.state
            && let Err(e) = instance.load_state(state)
        {
            log!("State of {} not loaded: {e}", entry.info.name);
        }
        let shared = InsertShared::new(0);
        shared.set_bypass(how.bypass);
        self.slots.push(Slot {
            owner: owner.clone(),
            index,
            position: match owner {
                Owner::Bus(_) => InsertPosition::END,
                _ => how.position,
            },
            info: entry,
            plugin: instance,
            shared,
            active_gen: None,
            error: None,
            editor_was_open: false,
            heard: false,
        });
        let p = self.slots.len() - 1;
        self.install(p);
        self.changed();
        Ok(self.view())
    }

    /// Take a slot out of the engine and unload its plugin.
    fn remove_slot(&mut self, owner: &Owner, index: usize) -> Result<(), String> {
        let p = self
            .position(owner, index)
            .ok_or_else(|| empty_owned_slot(owner, index))?;
        let target = self.target_of(owner);
        let mut slot = self.slots.remove(p);
        if slot.active_gen.is_some()
            && let Some(target) = target
        {
            let sent = LINK
                .lock()
                .unwrap()
                .as_mut()
                .filter(|l| Some(l.generation) == slot.active_gen)
                .map(|l| l.sender.send(InsertMsg::Remove { target, slot: index }));
            if let Some(Err(e)) = sent {
                self.slots.insert(p, slot);
                return Err(e);
            }
            // Wait for the audio thread to hand the processor back; only then may the plugin go.
            let started = Instant::now();
            while Arc::strong_count(&slot.shared) > 1 && started.elapsed() < RETIRE_WAIT {
                if let Some(link) = LINK.lock().unwrap().as_mut() {
                    link.sender.drain_retired();
                }
                std::thread::sleep(Duration::from_millis(2));
            }
            if Arc::strong_count(&slot.shared) > 1 {
                // The engine still holds it (no audio callbacks?). Destroying a plugin that may be
                // processing would crash; keeping it in memory costs only memory.
                log!(
                    "Plugin {} did not come back from the engine and stays in memory.",
                    slot.info.info.name
                );
                std::mem::forget(slot);
                self.changed();
                return Ok(());
            }
            slot.active_gen = None;
            slot.plugin.deactivate();
        }
        if slot.active_gen.take().is_some() {
            slot.plugin.deactivate();
        }
        log!(
            "Plugin removed: {} from {} slot {}",
            slot.info.info.name,
            owner.label(),
            index + 1
        );
        drop(slot);
        self.changed();
        Ok(())
    }

    fn move_slot(&mut self, owner: &Owner, from: usize, to: usize) -> Result<RackView, String> {
        self.place_slot(owner, from, to, None)
    }

    /// Move the plugin in slot `from` to slot `to` and, with `position`, to that place in the
    /// built-in chain. The slots stay in signal order: a place that would put a plugin in front of
    /// one it follows in slot order is refused. Without `position` (a plain reorder among the
    /// plugins) the moved plugin takes the place of its new neighbour, so the order holds.
    fn place_slot(
        &mut self,
        owner: &Owner,
        from: usize,
        to: usize,
        position: Option<InsertPosition>,
    ) -> Result<RackView, String> {
        if from >= MAX_INSERTS || to >= MAX_INSERTS {
            return Err(format!("There are slots 1 to {MAX_INSERTS}."));
        }
        let current = self.slot(owner, from)?.position;
        let position = match owner {
            Owner::Bus(_) => Some(InsertPosition::END),
            _ => position,
        };
        let layout: Vec<(usize, InsertPosition)> = self
            .slots
            .iter()
            .filter(|s| s.owner == *owner)
            .map(|s| (s.index, s.position))
            .collect();
        let position = position.unwrap_or_else(|| adopted_position(&layout, from, to, current));
        if from == to && position == current {
            return Ok(self.view());
        }
        if !in_signal_order(&after_place(&layout, from, to, position)) {
            return Err(
                "A plugin cannot sit in front of a built-in effect that a plugin before it comes \
                 after. Move the plugins in the order the signal passes them."
                    .to_string(),
            );
        }
        if let Some(target) = self.target_of(owner)
            && let Some(link) = LINK.lock().unwrap().as_mut()
        {
            link.sender.send(InsertMsg::Place {
                target,
                from,
                to,
                position,
            })?;
        }
        // The same rotation the rack does.
        for s in self.slots.iter_mut().filter(|s| s.owner == *owner) {
            if s.index == from {
                s.position = position;
            }
            s.index = moved_index(s.index, from, to);
        }
        self.changed();
        Ok(self.view())
    }

    /// Put a copy of the plugin in `from`'s slot `index` - same plugin, its full state, bypass and
    /// place in the chain - onto `to`, after the plugins already at that place. A second,
    /// independent instance, loaded here on the main thread like any other.
    fn copy_slot(&mut self, from: &Owner, index: usize, to: &Owner) -> Result<RackView, String> {
        let (plugin_id, state, bypass, position) = {
            let s = self.slot(from, index)?;
            let state = s.plugin.save_state().map_err(|e| {
                format!("\"{}\" did not hand over its settings: {e}", s.info.info.name)
            })?;
            (s.info.info.id.clone(), state, s.shared.bypass(), s.position)
        };
        let position = match to {
            Owner::Bus(_) => InsertPosition::END,
            _ => position,
        };
        let free = (0..MAX_INSERTS)
            .find(|i| self.position(to, *i).is_none())
            .ok_or_else(|| format!("All {MAX_INSERTS} plugin slots of {} are taken.", to.label()))?;
        self.load_as(
            to,
            Some(free),
            &plugin_id,
            LoadAs {
                position,
                state: Some(&state),
                bypass,
            },
        )?;
        let layout: Vec<(usize, InsertPosition)> = self
            .slots
            .iter()
            .filter(|s| s.owner == *to && s.index != free)
            .map(|s| (s.index, s.position))
            .collect();
        let target = placement_index(&layout, free, position);
        if target != free {
            self.place_slot(to, free, target, Some(position))?;
        }
        Ok(self.view())
    }

    /// Replace the plugins of `to` with copies of those of `from`, in the same places. What `to`
    /// had is removed first.
    fn copy_all(&mut self, from: &Owner, to: &Owner) -> Result<RackView, String> {
        if from == to {
            return Ok(self.view());
        }
        let mut sources: Vec<usize> = self
            .slots
            .iter()
            .filter(|s| s.owner == *from)
            .map(|s| s.index)
            .collect();
        sources.sort_unstable();
        let mut old: Vec<usize> = self
            .slots
            .iter()
            .filter(|s| s.owner == *to)
            .map(|s| s.index)
            .collect();
        old.sort_unstable();
        for index in old {
            self.remove_slot(to, index)?;
        }
        for index in sources {
            self.copy_slot(from, index, to)?;
        }
        Ok(self.view())
    }

    fn bypass(&mut self, owner: &Owner, index: usize, on: bool) -> Result<RackView, String> {
        self.slot(owner, index)?.shared.set_bypass(on);
        self.changed();
        Ok(self.view())
    }

    fn instantiate(&self, info: &plugin::PluginInfo) -> Result<Box<dyn PluginInstance>, String> {
        #[cfg(test)]
        if self.fake_plugins {
            return tests::fake_load(info);
        }
        plugin::load(info)
    }

    /// Every track's inserts with their states, by track name, for the tracks of the running
    /// engine (or the last one) - what the score's plugin settings file keeps.
    fn capture(&self) -> (Vec<String>, BTreeMap<String, Vec<scorestate::SidePlugin>>) {
        let mut out: BTreeMap<String, Vec<scorestate::SidePlugin>> = BTreeMap::new();
        for s in &self.slots {
            let Owner::Track(name) = &s.owner else {
                continue;
            };
            if !self.layout.contains(name) {
                continue;
            }
            let state = match s.plugin.save_state() {
                Ok(bytes) => Some(base64_encode(&bytes)),
                Err(e) => {
                    log!("{} on track \"{name}\" did not hand over its state: {e}", s.info.info.name);
                    None
                }
            };
            out.entry(name.clone()).or_default().push(scorestate::SidePlugin {
                plugin_id: s.info.info.id.clone(),
                name: s.info.info.name.clone(),
                slot: Some(s.index),
                bypass: s.shared.bypass(),
                before: s.position.name().map(str::to_string),
                state,
            });
        }
        for list in out.values_mut() {
            list.sort_by_key(|p| p.slot);
        }
        (self.layout.clone(), out)
    }

    /// A human changed the slots of `owner`: the open score writes it into its `mixer:`.
    fn touched(&self, owner: &Owner) {
        if let Owner::Track(name) = owner {
            crate::pluginmix::touch_inserts(name);
        }
    }

    /// What a loaded score (or an undo in its `mixer:`) says sits in `track`'s insert slots, in
    /// slot order. The same plugins in the same order stay as they are - only their bypass and
    /// place follow - so their sound is kept. Anything else is rebuilt from the front: a plugin
    /// that was already on this track comes back with the state it had, a new one with its
    /// default, one that is not installed here is left out with a notice, and the score keeps its
    /// entry. Right after a load, the score's own settings file (`crate::scorestate`) comes first:
    /// its states replace both.
    fn apply_score(&mut self, track: &str, wanted: &[InsertLine]) {
        let owner = Owner::Track(track.to_string());
        let mut have: Vec<usize> = (0..self.slots.len())
            .filter(|p| self.slots[*p].owner == owner)
            .collect();
        have.sort_by_key(|p| self.slots[*p].index);
        // The settings the score's own file has for these plugins - only right after the score
        // was loaded (`crate::scorestate`); `None` per plugin where it has none.
        let ids: Vec<&str> = wanted.iter().map(|w| w.plugin.as_str()).collect();
        let fresh = scorestate::match_states(&ids, &scorestate::take_inserts(track));
        let same = have.len() == wanted.len()
            && have
                .iter()
                .zip(wanted)
                .all(|(p, w)| self.slots[*p].info.info.id == w.plugin);
        if same {
            let mut changed = false;
            // The score's settings replace what the plugins have now (a load, a Reload). The
            // plugin is running; a state is set the way the "state" command sets it.
            for (k, p) in have.iter().enumerate() {
                if let Some(Some(state)) = fresh.get(k) {
                    let slot = &mut self.slots[*p];
                    match slot.plugin.load_state(state) {
                        Ok(()) => changed = true,
                        Err(e) => log!("State of {} from the score not loaded: {e}", slot.info.info.name),
                    }
                }
            }
            for (p, w) in have.iter().zip(wanted) {
                let on = w.bypass.unwrap_or(false);
                if self.slots[*p].shared.bypass() != on {
                    self.slots[*p].shared.set_bypass(on);
                    changed = true;
                }
            }
            // Same plugins in the same order, perhaps at other places in the chain: moved in
            // place, not reloaded, so they keep their sound. (The audio thread runs by place and
            // slot, so the steps in between need no particular order.) A list out of signal order
            // is refused by the score reader and left alone here.
            let places: Vec<(usize, InsertPosition)> = have
                .iter()
                .zip(wanted)
                .map(|(p, w)| (self.slots[*p].index, w.position()))
                .collect();
            if in_signal_order(&places) {
                for (index, position) in places {
                    let p = self.position(&owner, index);
                    if let Some(p) = p
                        && self.slots[p].position != position
                    {
                        self.slots[p].position = position;
                        if let Some(target) = self.target_of(&owner)
                            && let Some(link) = LINK.lock().unwrap().as_mut()
                            && let Err(e) = link.sender.send(InsertMsg::Place {
                                target,
                                from: index,
                                to: index,
                                position,
                            })
                        {
                            log!("Plugin slot {} of track \"{track}\" not moved: {e}", index + 1);
                        }
                        changed = true;
                    }
                }
            }
            if changed {
                self.changed();
            }
            crate::pluginmix::found(track, false);
            return;
        }
        if crate::pluginmix::inserts_are_missing(track, wanted) {
            // Asked for again (an undo, a second apply) and still not here: nothing new to do.
            let present: Vec<&str> = have.iter().map(|p| self.slots[*p].info.info.id.as_str()).collect();
            if present.iter().all(|id| wanted.iter().any(|w| w.plugin == *id)) {
                return;
            }
        }
        // The states of what is there now, by plugin id, in slot order.
        let mut states: Vec<(String, Option<Vec<u8>>)> = have
            .iter()
            .map(|p| {
                let s = &self.slots[*p];
                (s.info.info.id.clone(), s.plugin.save_state().ok())
            })
            .collect();
        let indices: Vec<usize> = have.iter().map(|p| self.slots[*p].index).collect();
        for index in indices {
            if let Err(e) = self.remove_slot(&owner, index) {
                log!("Plugin slot {} of track \"{track}\" not emptied: {e}", index + 1);
            }
        }
        let mut missing = Vec::new();
        let mut index = 0;
        for (k, w) in wanted.iter().enumerate() {
            // The score's own settings first; then the state this plugin had on this track.
            let had = states
                .iter()
                .position(|(id, _)| *id == w.plugin)
                .and_then(|at| states.remove(at).1);
            let state = fresh.get(k).cloned().flatten().or(had);
            let how = LoadAs {
                position: w.position(),
                state: state.as_deref(),
                bypass: w.bypass.unwrap_or(false),
            };
            match self.load_as(&owner, Some(index), &w.plugin, how) {
                Ok(_) => index += 1,
                Err(e) => missing.push(format!("\"{}\" ({e})", w.plugin)),
            }
        }
        self.changed();
        if missing.is_empty() {
            crate::pluginmix::found(track, false);
        } else {
            crate::pluginmix::inserts_missing(track, wanted.to_vec());
            let message = format!(
                "Track \"{track}\": the score's plugin(s) {} are not available here. The track \
                 plays without them; the score keeps the entry.",
                missing.join(", ")
            );
            log!("{message}");
            emit(PluginEvent::Notice { message });
        }
    }

    fn params(&self, owner: &Owner, index: usize) -> Result<Vec<ParamView>, String> {
        let slot = self.slot(owner, index)?;
        let owner = midi_owner(owner, self.target_of(owner));
        Ok(slot
            .plugin
            .params()
            .into_iter()
            .map(|p| {
                let value = slot.plugin.param(p.id);
                ParamView {
                    text: slot.plugin.param_text(p.id, value),
                    address: format!("{owner}.insert.{}.param.{}", index + 1, p.id),
                    id: p.id,
                    name: p.name,
                    unit: p.unit,
                    value,
                    default: p.default,
                    steps: p.steps,
                    automatable: p.automatable,
                    read_only: p.read_only,
                    is_bypass: p.is_bypass,
                }
            })
            .collect())
    }

    fn set_param(
        &mut self,
        owner: &Owner,
        index: usize,
        id: u32,
        value: f64,
        announce: bool,
    ) -> Result<ParamValue, String> {
        let target = self.target_of(owner);
        let slot = self.slot_mut(owner, index)?;
        slot.plugin.set_param(id, value.clamp(0.0, 1.0));
        let now = slot.plugin.param(id);
        let out = ParamValue {
            id,
            value: now,
            text: slot.plugin.param_text(id, now),
        };
        if let Some(target) = target
            && let Some(s) = snapshot().lock().unwrap().get_mut(&(target, index))
        {
            s.params.insert(id, now);
        }
        self.dirty = true;
        self.last_change = Some(Instant::now());
        if announce && let Some(target) = target {
            emit(PluginEvent::Params {
                target: target.address(),
                slot: index,
                values: vec![out.clone()],
            });
        }
        Ok(out)
    }

    fn editor(&mut self, owner: &Owner, index: usize, open: bool) -> Result<RackView, String> {
        let (title, key) = {
            let slot = self.slot(owner, index)?;
            let place = match owner {
                Owner::Track(name) => name.clone(),
                Owner::Bus(b) => format!("{} bus", b.label()),
                Owner::Position(p) => format!("Track {}", p + 1),
            };
            (
                format!("{} \u{2013} {place} \u{b7} slot {}", slot.info.info.name, index + 1),
                format!("insert/{place}/{index}"),
            )
        };
        let slot = self.slot_mut(owner, index)?;
        if open {
            let started = Instant::now();
            slot.plugin.open_editor(&title, &key)?;
            log!(
                "Plugin window {title} open after {:?}: {}",
                started.elapsed(),
                slot.plugin.editor_report().unwrap_or_default()
            );
        } else {
            slot.plugin.close_editor();
        }
        slot.editor_was_open = open;
        emit(PluginEvent::Rack { rack: self.view() });
        Ok(self.view())
    }

    fn state_get(&self, owner: &Owner, index: usize) -> Result<String, String> {
        Ok(base64_encode(&self.slot(owner, index)?.plugin.save_state()?))
    }

    fn state_set(&mut self, owner: &Owner, index: usize, data: &str) -> Result<RackView, String> {
        let bytes = base64_decode(data).ok_or_else(|| "The state is not valid base64.".to_string())?;
        self.slot_mut(owner, index)?.plugin.load_state(&bytes)?;
        self.changed();
        Ok(self.view())
    }

    /// Editor changes, latency changes, meters, autosave. Every [`TICK`].
    fn tick(&mut self) {
        if let Some(link) = LINK.lock().unwrap().as_mut() {
            link.sender.drain_retired();
        }
        let mut rack_changed = false;
        let targets: Vec<Option<InsertTarget>> =
            self.slots.iter().map(|s| self.target_of(&s.owner)).collect();
        for (s, target) in self.slots.iter_mut().zip(targets) {
            let changed = s.plugin.take_changed();
            if !changed.is_empty() {
                let values: Vec<ParamValue> = changed
                    .iter()
                    .map(|&id| {
                        let value = s.plugin.param(id);
                        ParamValue {
                            id,
                            value,
                            text: s.plugin.param_text(id, value),
                        }
                    })
                    .collect();
                if let Some(target) = target {
                    if let Some(snap) = snapshot().lock().unwrap().get_mut(&(target, s.index)) {
                        for v in &values {
                            snap.params.insert(v.id, v.value);
                        }
                    }
                    emit(PluginEvent::Params {
                        target: target.address(),
                        slot: s.index,
                        values,
                    });
                }
                self.dirty = true;
                self.last_change = Some(Instant::now());
            }
            if s.active_gen.is_some() && !s.heard && s.shared.blocks() > 0 {
                s.heard = true;
                log!(
                    "Plugin {} in {} slot {}: audio is passing through ({} blocks).",
                    s.info.info.name,
                    s.owner.label(),
                    s.index + 1,
                    s.shared.blocks()
                );
            }
            if s.active_gen.is_some() {
                let latency = s.plugin.latency();
                if latency != s.shared.latency() {
                    s.shared.set_latency(latency);
                    rack_changed = true;
                }
            }
            let open = s.plugin.editor_open();
            if open != s.editor_was_open {
                s.editor_was_open = open;
                rack_changed = true;
                // What was done in the plugin's window need not be a parameter (a preset, a
                // sample loaded): its state is saved when the window is closed.
                if !open {
                    self.dirty = true;
                    self.last_change = Some(Instant::now());
                }
            }
        }
        let meter_due = self
            .last_meter
            .is_none_or(|t| t.elapsed() >= Duration::from_secs(1));
        if rack_changed || (meter_due && !self.slots.is_empty()) {
            self.last_meter = Some(Instant::now());
            emit(PluginEvent::Rack { rack: self.view() });
        }
        if self.dirty
            && self
                .last_change
                .is_none_or(|t| t.elapsed() >= Duration::from_secs(1))
        {
            self.save();
        }
    }

    fn shutdown(&mut self) -> usize {
        self.save();
        let mut kept = 0;
        for mut slot in std::mem::take(&mut self.slots) {
            slot.plugin.close_editor();
            if Arc::strong_count(&slot.shared) > 1 {
                log!(
                    "Plugin {} is still in the engine on the way out and stays loaded.",
                    slot.info.info.name
                );
                std::mem::forget(slot);
                kept += 1;
                continue;
            }
            if slot.active_gen.take().is_some() {
                slot.plugin.deactivate();
            }
            drop(slot);
        }
        kept
    }

    fn save(&mut self) {
        self.dirty = false;
        let mut rack = SavedRack {
            version: RACK_VERSION,
            ..SavedRack::default()
        };
        for s in &self.slots {
            rack.add(
                &s.owner,
                SavedSlot {
                    slot: s.index,
                    plugin_id: s.info.info.id.clone(),
                    name: s.info.info.name.clone(),
                    bypass: s.shared.bypass(),
                    state: s.plugin.save_state().ok().map(|b| base64_encode(&b)),
                    before: s.position.name().map(str::to_string),
                },
            );
        }
        for (owner, saved) in &self.lost {
            if !self.slots.iter().any(|s| s.owner == *owner && s.index == saved.slot) {
                rack.add(owner, saved.clone());
            }
        }
        let path = rack_path();
        if let Some(dir) = path.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        if let Ok(text) = serde_json::to_string_pretty(&rack)
            && let Err(e) = std::fs::write(&path, text)
        {
            log!("Plugin rack not saved: {e}");
        }
    }

    /// Load the rack of the last session.
    fn restore(&mut self) {
        let marker = restore_marker();
        if marker.exists() {
            let _ = std::fs::remove_file(&marker);
            let path = rack_path();
            let _ = std::fs::rename(&path, path.with_extension("json.bak"));
            let message = "The plugins of the last session were not loaded: the program stopped \
                           while loading them last time. The previous setup was kept as \
                           plugin-rack.json.bak."
                .to_string();
            log!("{message}");
            emit(PluginEvent::Notice { message });
            return;
        }
        // The track list the engine last ran with: what an old, position-keyed file is read against,
        // and what `track.<n>` means until an engine runs.
        if self.layout.is_empty() {
            self.layout = layout::load_at(&layout::path())
                .into_iter()
                .map(|t| t.name)
                .collect();
        }
        let Ok(text) = std::fs::read_to_string(rack_path()) else {
            return;
        };
        let Some(rack) = read_rack(&text, &self.layout) else {
            return;
        };
        let migrated = !text.contains("\"version\"");
        let entries = rack.entries();
        if entries.is_empty() {
            return;
        }
        let _ = std::fs::write(&marker, "restoring");
        // Slots that do not come back keep their place in the file, so a plugin missing today
        // (uninstalled, not scanned yet) is not forgotten by the next save.
        let mut lost = Vec::new();
        for (owner, saved) in entries {
            let state = saved.state.as_deref().and_then(base64_decode);
            let how = LoadAs {
                position: saved.position(),
                state: state.as_deref(),
                bypass: saved.bypass,
            };
            match self.load_as(&owner, Some(saved.slot), &saved.plugin_id, how) {
                Ok(_) => {}
                Err(e) => {
                    log!("Plugin {} not restored: {e}", saved.name);
                    emit(PluginEvent::Notice {
                        message: format!("\"{}\" was not loaded again: {e}", saved.name),
                    });
                    lost.push((owner, saved));
                }
            }
        }
        let _ = std::fs::remove_file(&marker);
        self.lost = lost;
        self.dirty = false;
        if migrated {
            // Written once in the new format, the old file kept beside it.
            let path = rack_path();
            let _ = std::fs::copy(&path, path.with_extension("json.v1"));
            self.save();
            log!("plugin-rack.json converted to slots by track name (the old file is plugin-rack.json.v1).");
        }
        self.refresh_snapshot();
        emit(PluginEvent::Rack { rack: self.view() });
    }
}

// ---------------------------------------------------------------------------------------------
// base64, for state chunks in JSON
// ---------------------------------------------------------------------------------------------

const B64: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

/// A plugin state chunk as text for a JSON file - shared with `crate::instruments`.
pub(crate) fn encode_state(data: &[u8]) -> String {
    base64_encode(data)
}

/// The other way round; `None` when the text is not base64.
pub(crate) fn decode_state(text: &str) -> Option<Vec<u8>> {
    base64_decode(text)
}

fn base64_encode(data: &[u8]) -> String {
    let mut out = String::with_capacity(data.len().div_ceil(3) * 4);
    for chunk in data.chunks(3) {
        let b = [
            chunk[0],
            chunk.get(1).copied().unwrap_or(0),
            chunk.get(2).copied().unwrap_or(0),
        ];
        let n = (u32::from(b[0]) << 16) | (u32::from(b[1]) << 8) | u32::from(b[2]);
        for i in 0..4 {
            if i <= chunk.len() {
                out.push(B64[((n >> (18 - 6 * i)) & 63) as usize] as char);
            } else {
                out.push('=');
            }
        }
    }
    out
}

fn base64_decode(text: &str) -> Option<Vec<u8>> {
    let mut out = Vec::with_capacity(text.len() / 4 * 3);
    let mut acc = 0u32;
    let mut bits = 0;
    for c in text.bytes() {
        if c == b'=' || c.is_ascii_whitespace() {
            continue;
        }
        let v = B64.iter().position(|&b| b == c)? as u32;
        acc = (acc << 6) | v;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((acc >> bits) as u8);
            acc &= (1 << bits) - 1;
        }
    }
    Some(out)
}

// ---------------------------------------------------------------------------------------------
// Tauri commands
// ---------------------------------------------------------------------------------------------

async fn blocking<T: Send + 'static>(
    work: impl FnOnce() -> Result<T, String> + Send + 'static,
) -> Result<T, String> {
    tauri::async_runtime::spawn_blocking(work)
        .await
        .map_err(|e| format!("The background work was cancelled: {e}"))?
}

fn catalog_view(plugins: Vec<Scanned>) -> CatalogView {
    CatalogView {
        supported: plugin::SUPPORTED,
        plugins,
        cache_path: scan::cache_path().display().to_string(),
    }
}

/// The plugins found by the last scan (from the cache; fast).
#[tauri::command]
pub async fn plugin_catalog() -> Result<CatalogView, String> {
    blocking(|| on_main(|m| {
        if m.catalog.is_empty() {
            m.catalog = scan::cached();
        }
        Ok(catalog_view(m.catalog.clone()))
    }))
    .await
}

/// Look for plugins. New or changed ones are probed in a child process each; `full` probes all
/// of them again. Progress arrives as `looper://plugins` events of kind `scan`.
#[tauri::command]
pub async fn plugin_scan(full: Option<bool>) -> Result<CatalogView, String> {
    blocking(move || {
        let exe = std::env::current_exe().map_err(|e| e.to_string())?;
        log!("Plugin scan started{}.", if full == Some(true) { " (full)" } else { "" });
        let plugins = scan::scan(&exe, full.unwrap_or(false), |label| {
            emit(PluginEvent::Scan {
                label: label.to_string(),
            });
        });
        log!(
            "Plugin scan done: {} found, {} loadable.",
            plugins.len(),
            plugins.iter().filter(|p| p.loadable()).count()
        );
        let view = catalog_view(plugins.clone());
        emit(PluginEvent::Catalog {
            catalog: view.clone(),
        });
        on_main(move |m| {
            m.catalog = plugins;
            Ok(())
        })?;
        Ok(view)
    })
    .await
}

/// Every occupied slot.
#[tauri::command]
pub async fn plugin_rack() -> Result<RackView, String> {
    blocking(|| on_main(|m| Ok(m.view()))).await
}

/// Load a plugin into a slot (`slot` omitted: the first free one). An occupied slot is replaced.
#[tauri::command(rename_all = "snake_case")]
pub async fn plugin_load(
    target: String,
    slot: Option<usize>,
    plugin_id: String,
) -> Result<RackView, String> {
    let target = parse_target(&target)?;
    blocking(move || {
        on_main(move |m| {
            let owner = m.owner(target)?;
            let view = m.load(&owner, slot, &plugin_id)?;
            m.touched(&owner);
            Ok(view)
        })
    })
    .await
}

/// A loaded score (or an undo in its `mixer:`) says which plugins sit on `track`. Handed to the
/// main thread without waiting.
pub fn apply_score(track: String, inserts: Vec<InsertLine>) {
    schedule(move |m| m.apply_score(&track, &inserts));
}

/// Empty a slot.
#[tauri::command(rename_all = "snake_case")]
pub async fn plugin_remove(target: String, slot: usize) -> Result<RackView, String> {
    let target = parse_target(&target)?;
    blocking(move || {
        on_main(move |m| {
            let owner = m.owner(target)?;
            m.remove_slot(&owner, slot)?;
            m.touched(&owner);
            Ok(m.view())
        })
    })
    .await
}

/// Move a plugin to another position in the same chain; the ones in between shift.
#[tauri::command(rename_all = "snake_case")]
pub async fn plugin_move(target: String, from: usize, to: usize) -> Result<RackView, String> {
    let target = parse_target(&target)?;
    blocking(move || {
        on_main(move |m| {
            let owner = m.owner(target)?;
            let view = m.move_slot(&owner, from, to)?;
            m.touched(&owner);
            Ok(view)
        })
    })
    .await
}

/// Move a plugin to slot `to` and to a place in the track's built-in chain: in front of `before`
/// (`octaver`, `high_pass`, `eq`, `comp`, `delay`, `reverb`) or, without it, after the chain.
/// Refused when it would put the plugins out of signal order.
#[tauri::command(rename_all = "snake_case")]
pub async fn plugin_place(
    target: String,
    from: usize,
    to: usize,
    before: Option<String>,
) -> Result<RackView, String> {
    let target = parse_target(&target)?;
    let position = parse_before(before.as_deref())?;
    blocking(move || {
        on_main(move |m| {
            let owner = m.owner(target)?;
            let view = m.place_slot(&owner, from, to, Some(position))?;
            m.touched(&owner);
            Ok(view)
        })
    })
    .await
}

/// `before:` as the UI sends it.
fn parse_before(before: Option<&str>) -> Result<InsertPosition, String> {
    InsertPosition::parse(before).ok_or_else(|| {
        format!(
            "'{}' is not a built-in effect. Use octaver, high_pass, eq, comp, delay or reverb.",
            before.unwrap_or_default()
        )
    })
}

/// Copy the plugin in `target`'s slot - with its full state, bypass and place in the chain - onto
/// `to` (`track.<n>` or `bus.<name>`), as a second, independent instance.
#[tauri::command(rename_all = "snake_case")]
pub async fn plugin_copy(target: String, slot: usize, to: String) -> Result<RackView, String> {
    let from = parse_target(&target)?;
    let to = parse_target(&to)?;
    blocking(move || {
        on_main(move |m| {
            let (from, to) = (m.owner(from)?, m.owner(to)?);
            let view = m.copy_slot(&from, slot, &to)?;
            m.touched(&to);
            Ok(view)
        })
    })
    .await
}

/// Replace every plugin of `to` with copies of `from`'s, in the same places - the plugin half of
/// "copy the whole chain" (`crate::host`, `chain_copy`).
pub fn copy_all(from: InsertTarget, to: InsertTarget) -> Result<RackView, String> {
    on_main(move |m| {
        let (from, to) = (m.owner(from)?, m.owner(to)?);
        let view = m.copy_all(&from, &to)?;
        m.touched(&to);
        Ok(view)
    })
}

/// The running engine's track names and every track's inserts with their states, for the score's
/// plugin settings file (`crate::scorestate`). Not on the main thread.
pub fn capture() -> Result<(Vec<String>, BTreeMap<String, Vec<scorestate::SidePlugin>>), String> {
    on_main(|m| Ok(m.capture()))
}

/// [`capture`] on the main thread itself (the way out of the app).
pub fn capture_here() -> (Vec<String>, BTreeMap<String, Vec<scorestate::SidePlugin>>) {
    MANAGER.with(|m| match m.try_borrow() {
        Ok(m) => m.capture(),
        Err(_) => (Vec::new(), BTreeMap::new()),
    })
}

/// Hand the loaded score's leftover settings back once the managers have applied its `mixer:`
/// (`crate::scorestate::finish_load`) - queued behind their work on the main thread.
pub fn after_score_applied() {
    match APP.get() {
        Some(app) => {
            let _ = app.run_on_main_thread(scorestate::finish_load);
        }
        None => scorestate::finish_load(),
    }
}

/// The host's bypass of a slot: the plugin is not called, the signal passes unchanged.
#[tauri::command(rename_all = "snake_case")]
pub async fn plugin_bypass(target: String, slot: usize, on: bool) -> Result<RackView, String> {
    let target = parse_target(&target)?;
    blocking(move || {
        on_main(move |m| {
            let owner = m.owner(target)?;
            let view = m.bypass(&owner, slot, on)?;
            m.touched(&owner);
            Ok(view)
        })
    })
    .await
}

/// Every parameter of the plugin in a slot, with value and display text.
#[tauri::command(rename_all = "snake_case")]
pub async fn plugin_params(target: String, slot: usize) -> Result<Vec<ParamView>, String> {
    let target = parse_target(&target)?;
    blocking(move || on_main(move |m| m.params(&m.owner(target)?, slot))).await
}

/// Set one parameter, normalised 0..1. Answers with the value the plugin actually took.
#[tauri::command(rename_all = "snake_case")]
pub async fn plugin_set_param(
    target: String,
    slot: usize,
    id: u32,
    value: f64,
) -> Result<ParamValue, String> {
    let target = parse_target(&target)?;
    blocking(move || on_main(move |m| m.set_param(&m.owner(target)?, slot, id, value, false))).await
}

/// Open or close the plugin's own window.
#[tauri::command(rename_all = "snake_case")]
pub async fn plugin_editor(target: String, slot: usize, open: bool) -> Result<RackView, String> {
    let target = parse_target(&target)?;
    blocking(move || on_main(move |m| m.editor(&m.owner(target)?, slot, open))).await
}

/// The plugin's complete state, base64 - a preset to keep.
#[tauri::command(rename_all = "snake_case")]
pub async fn plugin_state_get(target: String, slot: usize) -> Result<String, String> {
    let target = parse_target(&target)?;
    blocking(move || on_main(move |m| m.state_get(&m.owner(target)?, slot))).await
}

/// Put a state from [`plugin_state_get`] back.
#[tauri::command(rename_all = "snake_case")]
pub async fn plugin_state_set(target: String, slot: usize, data: String) -> Result<RackView, String> {
    let target = parse_target(&target)?;
    blocking(move || on_main(move |m| m.state_set(&m.owner(target)?, slot, &data))).await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base64_round_trips_every_length() {
        for n in 0..40usize {
            let data: Vec<u8> = (0..n as u32).map(|i| (i * 37 + 11) as u8).collect();
            let text = base64_encode(&data);
            assert_eq!(text.len() % 4, 0);
            assert_eq!(base64_decode(&text), Some(data));
        }
        assert_eq!(base64_encode(b"Man"), "TWFu");
        assert_eq!(base64_encode(b"Ma"), "TWE=");
    }

    #[test]
    fn slot_owners_are_spelled_as_the_ui_writes_them() {
        assert_eq!(parse_target("track.1"), Ok(InsertTarget::Track(0)));
        assert_eq!(parse_target("track.8"), Ok(InsertTarget::Track(7)));
        assert_eq!(parse_target("bus.main"), Ok(InsertTarget::Bus(Bus::Main)));
        assert_eq!(parse_target("bus.monitor"), Ok(InsertTarget::Bus(Bus::Monitor)));
        assert!(parse_target("track.0").is_err());
        assert!(parse_target("track.9").is_err());
        assert!(parse_target("bus.nowhere").is_err());
        assert_eq!(InsertTarget::Track(2).address(), "track.3");
        assert_eq!(midi_owner(&Owner::Bus(Bus::Main), None), "global.bus.main");
    }

    /// A slot's MIDI address follows the plugin, not the position: it names the track.
    #[test]
    fn midi_addresses_name_the_track_where_a_name_can_stand() {
        let at = Some(InsertTarget::Track(1));
        assert_eq!(midi_owner(&Owner::Track("stimme".into()), at), "track.stimme");
        assert_eq!(midi_owner(&Owner::Track("e-git_2".into()), at), "track.e-git_2");
        // Digits only would read as a position, a dot or a space would break the address.
        assert_eq!(midi_owner(&Owner::Track("42".into()), at), "track.2");
        assert_eq!(midi_owner(&Owner::Track("a.b".into()), at), "track.2");
        assert_eq!(midi_owner(&Owner::Track("lead vox".into()), at), "track.2");
    }

    fn saved(slot: usize, id: &str) -> SavedSlot {
        SavedSlot {
            slot,
            plugin_id: id.into(),
            name: id.to_uppercase(),
            bypass: slot == 1,
            state: Some("AAEC".into()),
            before: None,
        }
    }

    use looper_engine::engine::fx::FxSlot;

    fn at(slot: FxSlot) -> InsertPosition {
        InsertPosition::before(slot)
    }
    const END: InsertPosition = InsertPosition::END;

    /// A rack file from before positions reads with every plugin at the end - exactly as before -
    /// and a placed one round-trips; `before` is only written when it says something.
    #[test]
    fn positions_round_trip_through_the_rack_file_and_old_files_mean_the_end() {
        let rack = read_rack(r#"{"version": 2, "tracks": {"voice": [
            {"slot": 0, "plugin_id": "vst3:IRDX", "name": "IRDX", "bypass": false, "state": "AAEC"}
        ]}}"#, &[])
        .expect("liest sich");
        assert_eq!(rack.tracks["voice"][0].position(), END);

        let mut rack = SavedRack {
            version: RACK_VERSION,
            ..SavedRack::default()
        };
        let mut ir = saved(0, "vst3:irdx");
        ir.before = Some("octaver".into());
        rack.add(&Owner::Track("voice".into()), ir);
        rack.add(&Owner::Track("voice".into()), saved(1, "au:delay"));
        let text = serde_json::to_string_pretty(&rack).unwrap();
        assert_eq!(text.matches("\"before\"").count(), 1, "{text}");
        let back = read_rack(&text, &[]).unwrap();
        assert_eq!(back, rack);
        assert_eq!(back.tracks["voice"][0].position(), at(FxSlot::Octaver));
        assert_eq!(back.tracks["voice"][1].position(), END);
    }

    #[test]
    fn slot_order_must_be_signal_order() {
        assert!(in_signal_order(&[(0, at(FxSlot::Octaver)), (1, at(FxSlot::Eq)), (4, END)]));
        assert!(in_signal_order(&[(3, END), (0, at(FxSlot::Eq))]), "by index, not by list");
        assert!(!in_signal_order(&[(0, END), (1, at(FxSlot::Eq))]));
        assert!(in_signal_order(&[]));
    }

    /// Moving a plugin across built-in effects only - the IR loader from the end to the front,
    /// with nothing else passed - keeps its slot, and with it every MIDI address of the slot.
    #[test]
    fn a_move_across_built_ins_only_keeps_the_slot_and_its_midi_address() {
        let layout = [(0, at(FxSlot::Comp)), (1, END)];
        // Plugin 2 (the IR loader) to the front of the octaver: it must now be slot 1.
        let moved = after_place(&layout, 1, 0, at(FxSlot::Octaver));
        assert_eq!(moved, vec![(0, at(FxSlot::Octaver)), (1, at(FxSlot::Comp))]);
        assert!(in_signal_order(&moved));
        // Plugin 1 from before the compressor to before the EQ: same slot.
        let moved = after_place(&layout, 0, 0, at(FxSlot::Eq));
        assert_eq!(moved, vec![(0, at(FxSlot::Eq)), (1, END)]);
        // Plugin 1 to the end but kept in slot 1, in front of plugin 2: still in order (both end).
        assert!(in_signal_order(&after_place(&layout, 0, 0, END)));
        // Plugin 2 in front of the EQ but left in slot 2, behind plugin 1 at the compressor: refused.
        assert!(!in_signal_order(&after_place(&layout, 1, 1, at(FxSlot::Eq))));
        assert_eq!(moved_index(5, 1, 3), 5);
        assert_eq!(moved_index(2, 1, 3), 1);
        assert_eq!(moved_index(1, 1, 3), 3);
    }

    /// A plain reorder among the plugins (the old drag, or MIDI-free `plugin_move`) keeps the
    /// order valid by taking the neighbour's place where its own no longer fits.
    #[test]
    fn a_plain_reorder_adopts_the_place_of_its_new_neighbour() {
        let layout = [(0, at(FxSlot::Octaver)), (1, END), (2, END)];
        assert_eq!(adopted_position(&layout, 2, 1, END), END, "among equals: unchanged");
        assert_eq!(
            adopted_position(&layout, 2, 0, END),
            at(FxSlot::Octaver),
            "to the very front: the place of the one it now precedes"
        );
        assert_eq!(adopted_position(&layout, 0, 2, at(FxSlot::Octaver)), END, "to the back");
    }

    /// A copied plugin lands after every plugin at its place or further forward.
    #[test]
    fn a_copy_lands_behind_the_plugins_at_its_place() {
        let layout = [(0, at(FxSlot::Octaver)), (1, at(FxSlot::Eq)), (2, END)];
        assert_eq!(placement_index(&layout, 3, at(FxSlot::Eq)), 2, "between EQ and end");
        assert_eq!(placement_index(&layout, 3, END), 3, "at the end: stays");
        assert_eq!(placement_index(&layout, 3, at(FxSlot::Octaver)), 1);
        // A gap in front (slot 0 empty): the new one moves behind the octaver plugin in slot 2.
        let gappy = [(2, at(FxSlot::Octaver)), (3, END)];
        assert_eq!(placement_index(&gappy, 0, at(FxSlot::Eq)), 2);
        let mut result = after_place(&[(0, at(FxSlot::Eq)), (2, at(FxSlot::Octaver)), (3, END)], 0, 2, at(FxSlot::Eq));
        result.sort();
        assert!(in_signal_order(&result), "{result:?}");
        assert_eq!(placement_index(&[], 0, at(FxSlot::Eq)), 0);
    }

    const OLD_FILE: &str = r#"{"slots": [
        {"target": "track.1", "slot": 0, "plugin_id": "vst3:deess", "name": "DEESS", "bypass": false, "state": "AAEC"},
        {"target": "track.2", "slot": 1, "plugin_id": "au:comp", "name": "COMP", "bypass": true, "state": null},
        {"target": "track.5", "slot": 0, "plugin_id": "vst3:far", "name": "FAR", "bypass": false, "state": null},
        {"target": "bus.main", "slot": 0, "plugin_id": "vst3:limit", "name": "LIMIT", "bypass": false, "state": null}
    ]}"#;

    #[test]
    fn an_old_rack_file_is_converted_to_names_without_losing_a_slot() {
        let layout = vec!["stimme".to_string(), "gitarre".to_string()];
        let rack = read_rack(OLD_FILE, &layout).expect("liest sich");
        assert_eq!(rack.version, RACK_VERSION);
        assert_eq!(rack.tracks["stimme"][0].plugin_id, "vst3:deess");
        assert_eq!(rack.tracks["stimme"][0].state.as_deref(), Some("AAEC"));
        assert_eq!(rack.tracks["gitarre"][0].plugin_id, "au:comp");
        assert!(rack.tracks["gitarre"][0].bypass);
        // Track 5 did not exist in the last layout: kept as a position, not dropped.
        assert_eq!(rack.positions["5"][0].plugin_id, "vst3:far");
        assert_eq!(rack.buses["main"][0].plugin_id, "vst3:limit");
        assert_eq!(rack.entries().len(), 4);

        // Without any known layout every track slot waits for the first engine.
        let rack = read_rack(OLD_FILE, &[]).expect("liest sich");
        assert!(rack.tracks.is_empty());
        assert_eq!(rack.positions.len(), 3);
        let owners: Vec<Owner> = rack.entries().into_iter().map(|(o, _)| o).collect();
        assert!(owners.contains(&Owner::Position(0)) && owners.contains(&Owner::Bus(Bus::Main)));
    }

    #[test]
    fn the_new_file_reads_back_as_written() {
        let mut rack = SavedRack {
            version: RACK_VERSION,
            ..SavedRack::default()
        };
        rack.add(&Owner::Track("voice".into()), saved(1, "b"));
        rack.add(&Owner::Track("voice".into()), saved(0, "a"));
        rack.add(&Owner::Bus(Bus::Monitor), saved(0, "c"));
        rack.add(&Owner::Position(2), saved(0, "d"));
        let text = serde_json::to_string_pretty(&rack).unwrap();
        assert!(text.contains("\"voice\""), "{text}");
        let back = read_rack(&text, &[]).expect("liest sich");
        assert_eq!(back, rack);
        assert_eq!(back.tracks["voice"][0].slot, 0, "sorted by slot");
    }

    #[test]
    fn slots_follow_their_track_by_name_when_the_order_changes() {
        // Setup stimme, gitarre; the score has gitarre, voice. The host recognised voice as the
        // renamed stimme (same input).
        let owners = vec![
            Owner::Track("stimme".into()),
            Owner::Track("gitarre".into()),
            Owner::Bus(Bus::Main),
        ];
        let new = vec!["gitarre".to_string(), "voice".to_string()];
        let homes = rehome(&owners, &new, &[("stimme".into(), "voice".into())]);
        assert_eq!(
            homes,
            vec![Owner::Track("voice".into()), Owner::Track("gitarre".into()), Owner::Bus(Bus::Main)]
        );
        // Without a recognised rename, stimme's slot is parked - kept, not moved onto the guitar.
        let homes = rehome(&owners, &new, &[]);
        assert_eq!(homes[0], Owner::Track("stimme".into()));
        assert_eq!(homes[1], Owner::Track("gitarre".into()));
    }

    #[test]
    fn a_rename_does_not_overwrite_the_new_names_own_slots() {
        let owners = vec![Owner::Track("stimme".into()), Owner::Track("voice".into())];
        let homes = rehome(&owners, &["voice".to_string()], &[("stimme".into(), "voice".into())]);
        assert_eq!(homes, owners);
    }

    #[test]
    fn old_positions_are_bound_to_the_first_engines_tracks() {
        let owners = vec![Owner::Position(0), Owner::Position(1), Owner::Position(4)];
        let homes = rehome(&owners, &["stimme".to_string(), "gitarre".to_string()], &[]);
        assert_eq!(
            homes,
            vec![Owner::Track("stimme".into()), Owner::Track("gitarre".into()), Owner::Position(4)]
        );
    }

    // ---- the manager with stand-in plugins: no installed plugin, no engine -------------------

    /// A plugin whose whole state is a byte string; `fake:silent` never hands its state over.
    struct Fake {
        info: plugin::PluginInfo,
        state: Vec<u8>,
    }

    struct Passthrough;
    impl looper_engine::engine::insert::InsertProcessor for Passthrough {
        fn process(&mut self, _left: &mut [f32], _right: &mut [f32]) {}
    }

    impl PluginInstance for Fake {
        fn info(&self) -> &plugin::PluginInfo {
            &self.info
        }
        fn params(&self) -> Vec<plugin::ParamInfo> {
            Vec::new()
        }
        fn param(&self, _id: u32) -> f64 {
            0.0
        }
        /// The state as text: how a test reads what the plugin was given.
        fn param_text(&self, _id: u32, _value: f64) -> String {
            String::from_utf8_lossy(&self.state).into_owned()
        }
        fn set_param(&mut self, _id: u32, _value: f64) {}
        fn latency(&self) -> u32 {
            0
        }
        fn save_state(&self) -> Result<Vec<u8>, String> {
            if self.info.id == "fake:silent" {
                return Err("no state today".into());
            }
            Ok(self.state.clone())
        }
        fn load_state(&mut self, data: &[u8]) -> Result<(), String> {
            self.state = data.to_vec();
            Ok(())
        }
        fn activate(
            &mut self,
            _sample_rate: f64,
            _max_block: usize,
        ) -> Result<Box<dyn looper_engine::engine::insert::InsertProcessor>, String> {
            Ok(Box::new(Passthrough))
        }
        fn deactivate(&mut self) {}
        fn has_editor(&self) -> bool {
            false
        }
        fn open_editor(&mut self, _title: &str, _key: &str) -> Result<(), String> {
            Err("none".into())
        }
        fn close_editor(&mut self) {}
        fn editor_open(&self) -> bool {
            false
        }
        fn take_changed(&mut self) -> Vec<u32> {
            Vec::new()
        }
    }

    pub(super) fn fake_load(info: &plugin::PluginInfo) -> Result<Box<dyn PluginInstance>, String> {
        Ok(Box::new(Fake {
            info: info.clone(),
            state: b"default".to_vec(),
        }))
    }

    fn scanned(id: &str) -> Scanned {
        Scanned {
            info: plugin::PluginInfo {
                id: id.into(),
                format: plugin::Format::Vst3,
                name: id.trim_start_matches("fake:").to_uppercase(),
                vendor: "Test".into(),
                version: "1".into(),
                category: "Fx".into(),
                kind: plugin::Kind::Effect,
                path: None,
            },
            status: scan::Status::Ok,
            message: None,
            latency: 0,
            params: 0,
        }
    }

    fn manager(tracks: &[&str]) -> Manager {
        Manager {
            fake_plugins: true,
            layout: tracks.iter().map(|t| t.to_string()).collect(),
            catalog: ["fake:ir", "fake:dly", "fake:silent"].map(scanned).to_vec(),
            ..Manager::default()
        }
    }

    fn voice() -> Owner {
        Owner::Track("voice".into())
    }
    fn guitar() -> Owner {
        Owner::Track("guitar".into())
    }

    /// What a slot's plugin holds: (plugin id, state as text, place, bypass).
    fn held(m: &Manager, owner: &Owner, index: usize) -> (String, String, InsertPosition, bool) {
        let s = m.slot(owner, index).expect("belegt");
        (s.info.info.id.clone(), s.plugin.param_text(0, 0.0), s.position, s.shared.bypass())
    }

    fn put(m: &mut Manager, owner: &Owner, index: usize, id: &str, state: &[u8], position: InsertPosition, bypass: bool) {
        m.load_as(owner, Some(index), id, LoadAs { position, state: Some(state), bypass }).expect("laedt");
    }

    /// Copy one plugin, then the whole chain: same plugin, same state, same place, same bypass -
    /// and independent afterwards. Places that would break signal order are refused.
    #[test]
    fn plugins_are_copied_with_their_settings_and_places() {
        let mut m = manager(&["voice", "guitar"]);
        put(&mut m, &voice(), 0, "fake:ir", b"IR-A", at(FxSlot::Octaver), false);
        put(&mut m, &voice(), 1, "fake:dly", b"DLY", END, true);

        m.copy_slot(&voice(), 0, &guitar()).unwrap();
        assert_eq!(held(&m, &guitar(), 0), ("fake:ir".into(), "IR-A".into(), at(FxSlot::Octaver), false));
        // Independent: the original changes, the copy does not.
        m.slot_mut(&voice(), 0).unwrap().plugin.load_state(b"IR-B").unwrap();
        assert_eq!(held(&m, &guitar(), 0).1, "IR-A");

        // A copy onto a bus lands at the end of the bus chain.
        m.copy_slot(&voice(), 0, &Owner::Bus(Bus::Main)).unwrap();
        assert_eq!(held(&m, &Owner::Bus(Bus::Main), 0).2, END);

        // A copy of the delay (end) goes behind the IR loader; a second IR copy in front of it.
        m.copy_slot(&voice(), 1, &guitar()).unwrap();
        m.copy_slot(&voice(), 0, &guitar()).unwrap();
        let ids: Vec<String> = (0..3).map(|i| held(&m, &guitar(), i).0).collect();
        assert_eq!(ids, ["fake:ir", "fake:ir", "fake:dly"]);
        assert_eq!(held(&m, &guitar(), 1).1, "IR-B");

        // The whole chain: the guitar's plugins are replaced by copies of the voice's.
        m.copy_all(&voice(), &guitar()).unwrap();
        assert_eq!(held(&m, &guitar(), 0), ("fake:ir".into(), "IR-B".into(), at(FxSlot::Octaver), false));
        assert_eq!(held(&m, &guitar(), 1), ("fake:dly".into(), "DLY".into(), END, true));
        assert!(m.slot(&guitar(), 2).is_err());

        // Places: the delay in front of the EQ is fine (behind the IR at the octaver); the IR to
        // the end while the delay stays at the EQ is not.
        m.place_slot(&guitar(), 1, 1, Some(at(FxSlot::Eq))).unwrap();
        assert_eq!(held(&m, &guitar(), 1).2, at(FxSlot::Eq));
        assert!(m.place_slot(&guitar(), 0, 0, Some(END)).is_err());
        // Moved past the delay as well, it is allowed - and the slots swap.
        m.place_slot(&guitar(), 0, 1, Some(END)).unwrap();
        assert_eq!(held(&m, &guitar(), 0).0, "fake:dly");
        assert_eq!(held(&m, &guitar(), 1), ("fake:ir".into(), "IR-B".into(), END, false));

        // What the rack file and the score's settings file would keep.
        let (tracks, captured) = m.capture();
        assert_eq!(tracks, ["voice", "guitar"]);
        assert_eq!(captured["voice"][0].before.as_deref(), Some("octaver"));
        assert_eq!(captured["voice"][1].state.as_deref(), Some(base64_encode(b"DLY").as_str()));
        assert!(captured["voice"][1].bypass);
    }

    /// The score's settings file end to end: captured on one "computer", applied on another that
    /// has the same plugins - states, places and bypass arrive, a state a plugin would not deliver
    /// keeps the file's, Reload puts the file's settings back over a tweak, and a later undo in the
    /// `mixer:` section does not.
    #[test]
    fn a_scores_plugin_settings_travel_with_it() {
        let _turn = scorestate::TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let dir = std::env::temp_dir().join(format!("looper-plugins-score-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let score = dir.join("Song.yaml");

        // An older file with a state for the plugin that will not deliver one this time.
        let mut old = scorestate::Sidecar::default();
        old.tracks.insert(
            "voice".into(),
            scorestate::TrackPlugins {
                instrument: None,
                inserts: vec![scorestate::SidePlugin {
                    plugin_id: "fake:silent".into(),
                    name: "SILENT".into(),
                    slot: Some(1),
                    state: Some(base64_encode(b"KEPT")),
                    ..Default::default()
                }],
            },
        );
        scorestate::write(&score, &old).unwrap();

        let mut here = manager(&["voice"]);
        put(&mut here, &voice(), 0, "fake:ir", b"IR-1", at(FxSlot::Octaver), false);
        put(&mut here, &voice(), 1, "fake:silent", b"whatever", END, true);
        let (tracks, inserts) = here.capture();
        let captured = scorestate::Captured {
            tracks,
            inserts,
            ..scorestate::Captured::default()
        };
        let merged = scorestate::merge(scorestate::read(&score).as_ref(), &captured);
        scorestate::write(&score, &merged).unwrap();

        // Another computer: same track name, nothing loaded, an unrelated local state would be
        // there for the IR loader - the score's file wins.
        let mut there = manager(&["voice"]);
        scorestate::restore(None);
        scorestate::switch(Some(score.clone()));
        let wanted = vec![
            InsertLine {
                plugin: "fake:ir".into(),
                bypass: None,
                before: Some("octaver".into()),
            },
            InsertLine {
                plugin: "fake:silent".into(),
                bypass: Some(true),
                before: None,
            },
        ];
        there.apply_score("voice", &wanted);
        assert_eq!(held(&there, &voice(), 0), ("fake:ir".into(), "IR-1".into(), at(FxSlot::Octaver), false));
        assert_eq!(held(&there, &voice(), 1), ("fake:silent".into(), "KEPT".into(), END, true));
        scorestate::finish_load();

        // A tweak, then Reload (same file again): the file's settings come back.
        there.slot_mut(&voice(), 0).unwrap().plugin.load_state(b"TWEAK").unwrap();
        scorestate::switch(Some(score.clone()));
        there.apply_score("voice", &wanted);
        assert_eq!(held(&there, &voice(), 0).1, "IR-1");
        scorestate::finish_load();

        // An undo in the `mixer:` section later re-applies the list - and leaves the sound alone.
        there.slot_mut(&voice(), 0).unwrap().plugin.load_state(b"TWEAK-2").unwrap();
        there.apply_score("voice", &wanted);
        assert_eq!(held(&there, &voice(), 0).1, "TWEAK-2");

        scorestate::restore(None);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The owner's case: a score whose `inserts:` say `before: octaver` (written by hand) with a
    /// settings file beside it, loaded again while the plugin already sits at the end of one
    /// track and is missing on another. "Stays with its sound" must not mean "stays where it is":
    /// the loaded plugin is moved (same instance, not reloaded) and gets the file's state; the
    /// missing one is loaded at the named place with the file's state. The rack view and what the
    /// next save would write say the new place.
    #[test]
    fn a_reload_moves_a_loaded_plugin_to_the_scores_place() {
        let _turn = scorestate::TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let dir = std::env::temp_dir().join(format!("looper-plugins-reload-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let score = dir.join("The Hardest Part.yaml");
        let side = |state: &[u8]| scorestate::SidePlugin {
            plugin_id: "fake:ir".into(),
            name: "IRDX Studio".into(),
            slot: Some(0),
            before: Some("octaver".into()),
            state: Some(base64_encode(state)),
            ..Default::default()
        };
        let mut file = scorestate::Sidecar {
            version: 1,
            ..Default::default()
        };
        for (track, state) in [("theme", &b"THEME"[..]), ("guitar", &b"GUITAR"[..])] {
            file.tracks.insert(
                track.into(),
                scorestate::TrackPlugins {
                    instrument: None,
                    inserts: vec![side(state)],
                },
            );
        }
        scorestate::write(&score, &file).unwrap();
        let theme = Owner::Track("theme".into());

        let mut m = manager(&["theme", "guitar"]);
        put(&mut m, &theme, 0, "fake:ir", b"LOCAL", END, false);
        let instance = m.slot(&theme, 0).unwrap().shared.clone();

        scorestate::restore(None);
        scorestate::switch(Some(score.clone()));
        let wanted = vec![InsertLine {
            plugin: "fake:ir".into(),
            bypass: None,
            before: Some("octaver".into()),
        }];
        m.apply_score("theme", &wanted);
        m.apply_score("guitar", &wanted);
        scorestate::finish_load();

        assert_eq!(held(&m, &theme, 0), ("fake:ir".into(), "THEME".into(), at(FxSlot::Octaver), false));
        assert!(Arc::ptr_eq(&instance, &m.slot(&theme, 0).unwrap().shared), "moved, not reloaded");
        assert_eq!(held(&m, &guitar(), 0), ("fake:ir".into(), "GUITAR".into(), at(FxSlot::Octaver), false));
        // What the UI draws the column from.
        let view = m.view();
        assert_eq!(view.slots.len(), 2);
        assert!(view.slots.iter().all(|s| s.before.as_deref() == Some("octaver")), "{view:?}");
        let (_, captured) = m.capture();
        assert_eq!(captured["theme"][0].before.as_deref(), Some("octaver"));
        assert_eq!(captured["guitar"][0].before.as_deref(), Some("octaver"));

        // Applied again (an undo, the write-back coming round): nothing moves back.
        m.apply_score("theme", &wanted);
        assert_eq!(held(&m, &theme, 0).2, at(FxSlot::Octaver));
        // And a score without `before` puts it back at the end - also without reloading.
        let old = vec![InsertLine {
            plugin: "fake:ir".into(),
            bypass: None,
            before: None,
        }];
        m.apply_score("theme", &old);
        assert_eq!(held(&m, &theme, 0).2, END);
        assert!(Arc::ptr_eq(&instance, &m.slot(&theme, 0).unwrap().shared));

        scorestate::restore(None);
        let _ = std::fs::remove_dir_all(&dir);
    }
}

