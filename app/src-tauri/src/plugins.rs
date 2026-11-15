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

use std::cell::RefCell;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock, mpsc};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Emitter};

use looper_engine::engine::bus::Bus;
use looper_engine::engine::command::MAX_TRACKS;
use looper_engine::engine::insert::{
    Insert, InsertEndpoint, InsertMsg, InsertSender, InsertShared, InsertTarget, MAX_BLOCK,
    MAX_INSERTS, insert_channel,
};
use looper_engine::midi::MidiAction;
use looper_engine::plugin::scan::{self, Scanned};
use looper_engine::plugin::{self, PluginInstance};

use crate::logfile::log;

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

/// Called by the host thread once a new engine runs. Its plugins are (re)installed on the main
/// thread right after.
pub fn engine_started(sender: InsertSender, sample_rate: u32) {
    let generation = GENERATION.fetch_add(1, Ordering::Relaxed) + 1;
    *LINK.lock().unwrap() = Some(Link {
        sender,
        sample_rate: f64::from(sample_rate),
        generation,
    });
    schedule(|m| m.reconcile());
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
                let _ = m.bypass(target, slot, on);
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
                let _ = m.set_param(target, slot, param, f64::from(value), true);
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
    /// MIDI address of the slot's bypass, e.g. `track.1.insert.2.bypass`.
    pub bypass_address: String,
}

/// Every occupied slot, plus whether plugins can run at all.
#[derive(Clone, Debug, Serialize)]
pub struct RackView {
    pub supported: bool,
    pub engine_running: bool,
    pub sample_rate: Option<f64>,
    pub max_slots: usize,
    pub slots: Vec<SlotView>,
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
    /// MIDI address, e.g. `track.1.insert.1.param.42`.
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

/// The MIDI owner prefix: `track.1` or `global.bus.main`.
fn midi_owner(target: InsertTarget) -> String {
    match target {
        InsertTarget::Track(t) => format!("track.{}", t + 1),
        InsertTarget::Bus(b) => format!("global.bus.{}", b.name()),
    }
}

// ---------------------------------------------------------------------------------------------
// The manager
// ---------------------------------------------------------------------------------------------

struct Slot {
    target: InsertTarget,
    index: usize,
    info: Scanned,
    plugin: Box<dyn PluginInstance>,
    shared: Arc<InsertShared>,
    /// Engine generation the processor was installed into, `None` while not running.
    active_gen: Option<u64>,
    error: Option<String>,
    editor_was_open: bool,
}

#[derive(Default)]
struct Manager {
    slots: Vec<Slot>,
    catalog: Vec<Scanned>,
    /// The rack changed since it was last written to disk.
    dirty: bool,
    last_change: Option<Instant>,
    last_meter: Option<Instant>,
}

#[derive(Serialize, Deserialize)]
struct SavedRack {
    slots: Vec<SavedSlot>,
}

#[derive(Serialize, Deserialize)]
struct SavedSlot {
    target: String,
    slot: usize,
    plugin_id: String,
    name: String,
    bypass: bool,
    /// The plugin's state chunk, base64.
    state: Option<String>,
}

fn rack_path() -> PathBuf {
    scan::cache_path().with_file_name("plugin-rack.json")
}

fn restore_marker() -> PathBuf {
    scan::cache_path().with_file_name("plugin-rack.restoring")
}

impl Manager {
    fn position(&self, target: InsertTarget, index: usize) -> Option<usize> {
        self.slots
            .iter()
            .position(|s| s.target == target && s.index == index)
    }

    fn slot(&self, target: InsertTarget, index: usize) -> Result<&Slot, String> {
        self.position(target, index)
            .map(|p| &self.slots[p])
            .ok_or_else(|| empty_slot(target, index))
    }

    fn slot_mut(&mut self, target: InsertTarget, index: usize) -> Result<&mut Slot, String> {
        let p = self
            .position(target, index)
            .ok_or_else(|| empty_slot(target, index))?;
        Ok(&mut self.slots[p])
    }

    fn view(&self) -> RackView {
        let link = LINK.lock().unwrap();
        let mut slots: Vec<SlotView> = self
            .slots
            .iter()
            .map(|s| SlotView {
                target: s.target.address(),
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
                bypass_address: format!("{}.insert.{}.bypass", midi_owner(s.target), s.index + 1),
            })
            .collect();
        slots.sort_by(|a, b| (a.target.clone(), a.slot).cmp(&(b.target.clone(), b.slot)));
        RackView {
            supported: plugin::SUPPORTED,
            engine_running: link.is_some(),
            sample_rate: link.as_ref().map(|l| l.sample_rate),
            max_slots: MAX_INSERTS,
            slots,
        }
    }

    fn changed(&mut self) {
        self.dirty = true;
        self.last_change = Some(Instant::now());
        self.refresh_snapshot();
        emit(PluginEvent::Rack { rack: self.view() });
    }

    fn refresh_snapshot(&self) {
        let mut snap = snapshot().lock().unwrap();
        snap.clear();
        for s in &self.slots {
            let params = s
                .plugin
                .params()
                .into_iter()
                .map(|p| (p.id, s.plugin.param(p.id)))
                .collect();
            snap.insert(
                (s.target, s.index),
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
        let slot = &mut self.slots[p];
        if slot.active_gen == Some(generation) {
            return;
        }
        if slot.active_gen.take().is_some() {
            slot.plugin.deactivate();
        }
        match slot.plugin.activate(sample_rate, MAX_BLOCK) {
            Ok(processor) => {
                slot.shared.set_latency(slot.plugin.latency());
                let msg = InsertMsg::Install {
                    target: slot.target,
                    slot: slot.index,
                    insert: Insert::new(processor, slot.shared.clone()),
                };
                let mut link_guard = LINK.lock().unwrap();
                match link_guard.as_mut() {
                    Some(link) if link.generation == generation => {
                        match link.sender.send(msg) {
                            Ok(()) => {
                                slot.active_gen = Some(generation);
                                slot.error = None;
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
                log!("Plugin {} startet nicht: {e}", slot.info.info.name);
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
        target: InsertTarget,
        index: Option<usize>,
        plugin_id: &str,
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
                .find(|i| self.position(target, *i).is_none())
                .ok_or_else(|| {
                    format!(
                        "All {MAX_INSERTS} plugin slots of {} are taken.",
                        target_label(target)
                    )
                })?,
        };
        let entry = self.catalog_entry(plugin_id)?;
        if self.position(target, index).is_some() {
            self.remove_slot(target, index)?;
        }
        let instance = plugin::load(&entry.info)?;
        log!(
            "Plugin geladen: {} ({}) in {} Slot {}",
            entry.info.name,
            entry.info.format.label(),
            target.address(),
            index + 1
        );
        self.slots.push(Slot {
            target,
            index,
            info: entry,
            plugin: instance,
            shared: InsertShared::new(0),
            active_gen: None,
            error: None,
            editor_was_open: false,
        });
        let p = self.slots.len() - 1;
        self.install(p);
        self.changed();
        Ok(self.view())
    }

    /// Take a slot out of the engine and unload its plugin.
    fn remove_slot(&mut self, target: InsertTarget, index: usize) -> Result<(), String> {
        let p = self
            .position(target, index)
            .ok_or_else(|| empty_slot(target, index))?;
        let mut slot = self.slots.remove(p);
        if slot.active_gen.is_some() {
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
                    "Plugin {} kam nicht aus der Engine zurueck und bleibt im Speicher.",
                    slot.info.info.name
                );
                std::mem::forget(slot);
                self.changed();
                return Ok(());
            }
            slot.plugin.deactivate();
        }
        log!(
            "Plugin entfernt: {} aus {} Slot {}",
            slot.info.info.name,
            target.address(),
            index + 1
        );
        drop(slot);
        self.changed();
        Ok(())
    }

    fn move_slot(&mut self, target: InsertTarget, from: usize, to: usize) -> Result<RackView, String> {
        if from >= MAX_INSERTS || to >= MAX_INSERTS {
            return Err(format!("There are slots 1 to {MAX_INSERTS}."));
        }
        if from == to {
            return Ok(self.view());
        }
        self.slot(target, from)?;
        if let Some(link) = LINK.lock().unwrap().as_mut() {
            link.sender.send(InsertMsg::Move { target, from, to })?;
        }
        // The same rotation the rack does.
        for s in self.slots.iter_mut().filter(|s| s.target == target) {
            s.index = if s.index == from {
                to
            } else if from < to && s.index > from && s.index <= to {
                s.index - 1
            } else if to < from && s.index >= to && s.index < from {
                s.index + 1
            } else {
                s.index
            };
        }
        self.changed();
        Ok(self.view())
    }

    fn bypass(&mut self, target: InsertTarget, index: usize, on: bool) -> Result<RackView, String> {
        self.slot(target, index)?.shared.set_bypass(on);
        self.changed();
        Ok(self.view())
    }

    fn params(&self, target: InsertTarget, index: usize) -> Result<Vec<ParamView>, String> {
        let slot = self.slot(target, index)?;
        let owner = midi_owner(target);
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
        target: InsertTarget,
        index: usize,
        id: u32,
        value: f64,
        announce: bool,
    ) -> Result<ParamValue, String> {
        let slot = self.slot_mut(target, index)?;
        slot.plugin.set_param(id, value.clamp(0.0, 1.0));
        let now = slot.plugin.param(id);
        let out = ParamValue {
            id,
            value: now,
            text: slot.plugin.param_text(id, now),
        };
        if let Some(s) = snapshot().lock().unwrap().get_mut(&(target, index)) {
            s.params.insert(id, now);
        }
        self.dirty = true;
        self.last_change = Some(Instant::now());
        if announce {
            emit(PluginEvent::Params {
                target: target.address(),
                slot: index,
                values: vec![out.clone()],
            });
        }
        Ok(out)
    }

    fn editor(&mut self, target: InsertTarget, index: usize, open: bool) -> Result<RackView, String> {
        let title = {
            let slot = self.slot(target, index)?;
            format!(
                "{} - {} slot {}",
                slot.info.info.name,
                match target {
                    InsertTarget::Track(t) => format!("Track {}", t + 1),
                    InsertTarget::Bus(b) => format!("{} bus", b.label()),
                },
                index + 1
            )
        };
        let slot = self.slot_mut(target, index)?;
        if open {
            slot.plugin.open_editor(&title)?;
        } else {
            slot.plugin.close_editor();
        }
        slot.editor_was_open = open;
        emit(PluginEvent::Rack { rack: self.view() });
        Ok(self.view())
    }

    fn state_get(&self, target: InsertTarget, index: usize) -> Result<String, String> {
        Ok(base64_encode(&self.slot(target, index)?.plugin.save_state()?))
    }

    fn state_set(&mut self, target: InsertTarget, index: usize, data: &str) -> Result<RackView, String> {
        let bytes = base64_decode(data).ok_or_else(|| "The state is not valid base64.".to_string())?;
        self.slot_mut(target, index)?.plugin.load_state(&bytes)?;
        self.changed();
        Ok(self.view())
    }

    /// Editor changes, latency changes, meters, autosave. Every [`TICK`].
    fn tick(&mut self) {
        if let Some(link) = LINK.lock().unwrap().as_mut() {
            link.sender.drain_retired();
        }
        let mut rack_changed = false;
        for s in self.slots.iter_mut() {
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
                if let Some(snap) = snapshot().lock().unwrap().get_mut(&(s.target, s.index)) {
                    for v in &values {
                        snap.params.insert(v.id, v.value);
                    }
                }
                emit(PluginEvent::Params {
                    target: s.target.address(),
                    slot: s.index,
                    values,
                });
                self.dirty = true;
                self.last_change = Some(Instant::now());
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

    fn save(&mut self) {
        self.dirty = false;
        let rack = SavedRack {
            slots: self
                .slots
                .iter()
                .map(|s| SavedSlot {
                    target: s.target.address(),
                    slot: s.index,
                    plugin_id: s.info.info.id.clone(),
                    name: s.info.info.name.clone(),
                    bypass: s.shared.bypass(),
                    state: s.plugin.save_state().ok().map(|b| base64_encode(&b)),
                })
                .collect(),
        };
        let path = rack_path();
        if let Some(dir) = path.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        if let Ok(text) = serde_json::to_string_pretty(&rack)
            && let Err(e) = std::fs::write(&path, text)
        {
            log!("Plugin-Belegung nicht gespeichert: {e}");
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
        let Ok(text) = std::fs::read_to_string(rack_path()) else {
            return;
        };
        let Ok(rack) = serde_json::from_str::<SavedRack>(&text) else {
            return;
        };
        if rack.slots.is_empty() {
            return;
        }
        let _ = std::fs::write(&marker, "restoring");
        for saved in rack.slots {
            let Ok(target) = parse_target(&saved.target) else {
                continue;
            };
            match self.load(target, Some(saved.slot), &saved.plugin_id) {
                Ok(_) => {
                    if let Some(state) = saved.state.as_deref().and_then(base64_decode)
                        && let Ok(slot) = self.slot_mut(target, saved.slot)
                        && let Err(e) = slot.plugin.load_state(&state)
                    {
                        log!("Zustand von {} nicht geladen: {e}", saved.name);
                    }
                    if saved.bypass
                        && let Ok(slot) = self.slot(target, saved.slot)
                    {
                        slot.shared.set_bypass(true);
                    }
                }
                Err(e) => {
                    log!("Plugin {} nicht wiederhergestellt: {e}", saved.name);
                    emit(PluginEvent::Notice {
                        message: format!("\"{}\" was not loaded again: {e}", saved.name),
                    });
                }
            }
        }
        let _ = std::fs::remove_file(&marker);
        self.dirty = false;
        self.refresh_snapshot();
        emit(PluginEvent::Rack { rack: self.view() });
    }
}

// ---------------------------------------------------------------------------------------------
// base64, for state chunks in JSON
// ---------------------------------------------------------------------------------------------

const B64: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

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
        log!("Plugin-Scan gestartet{}.", if full == Some(true) { " (vollstaendig)" } else { "" });
        let plugins = scan::scan(&exe, full.unwrap_or(false), |label| {
            emit(PluginEvent::Scan {
                label: label.to_string(),
            });
        });
        log!(
            "Plugin-Scan fertig: {} gefunden, {} ladbar.",
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
    blocking(move || on_main(move |m| m.load(target, slot, &plugin_id))).await
}

/// Empty a slot.
#[tauri::command(rename_all = "snake_case")]
pub async fn plugin_remove(target: String, slot: usize) -> Result<RackView, String> {
    let target = parse_target(&target)?;
    blocking(move || {
        on_main(move |m| {
            m.remove_slot(target, slot)?;
            Ok(m.view())
        })
    })
    .await
}

/// Move a plugin to another position in the same chain; the ones in between shift.
#[tauri::command(rename_all = "snake_case")]
pub async fn plugin_move(target: String, from: usize, to: usize) -> Result<RackView, String> {
    let target = parse_target(&target)?;
    blocking(move || on_main(move |m| m.move_slot(target, from, to))).await
}

/// The host's bypass of a slot: the plugin is not called, the signal passes unchanged.
#[tauri::command(rename_all = "snake_case")]
pub async fn plugin_bypass(target: String, slot: usize, on: bool) -> Result<RackView, String> {
    let target = parse_target(&target)?;
    blocking(move || on_main(move |m| m.bypass(target, slot, on))).await
}

/// Every parameter of the plugin in a slot, with value and display text.
#[tauri::command(rename_all = "snake_case")]
pub async fn plugin_params(target: String, slot: usize) -> Result<Vec<ParamView>, String> {
    let target = parse_target(&target)?;
    blocking(move || on_main(move |m| m.params(target, slot))).await
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
    blocking(move || on_main(move |m| m.set_param(target, slot, id, value, false))).await
}

/// Open or close the plugin's own window.
#[tauri::command(rename_all = "snake_case")]
pub async fn plugin_editor(target: String, slot: usize, open: bool) -> Result<RackView, String> {
    let target = parse_target(&target)?;
    blocking(move || on_main(move |m| m.editor(target, slot, open))).await
}

/// The plugin's complete state, base64 - a preset to keep.
#[tauri::command(rename_all = "snake_case")]
pub async fn plugin_state_get(target: String, slot: usize) -> Result<String, String> {
    let target = parse_target(&target)?;
    blocking(move || on_main(move |m| m.state_get(target, slot))).await
}

/// Put a state from [`plugin_state_get`] back.
#[tauri::command(rename_all = "snake_case")]
pub async fn plugin_state_set(target: String, slot: usize, data: String) -> Result<RackView, String> {
    let target = parse_target(&target)?;
    blocking(move || on_main(move |m| m.state_set(target, slot, &data))).await
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
        assert_eq!(midi_owner(InsertTarget::Bus(Bus::Main)), "global.bus.main");
    }
}
