//! Instrument plugins in the app: which instrument plays which track, its window, its state, and
//! getting it into the running engine's source slots (`looper_engine::engine::source`).
//!
//! ```text
//!  Tauri-Kommando ─► spawn_blocking ─► run_on_main_thread ─► Manager (Hauptthread)
//!                                                             │  laden, Zustand, Fenster
//!                                                             │  activate_instrument() ─► Prozessor
//!                                                             ▼
//!  Host-Thread: Session::start ─► engine_started(Sender) ──► LINK ─► InstrumentMsg ─► Audio-Thread
//!               Session::shutdown ─► engine_stopped()        ◄── Retire-Queue ◄──┘
//! ```
//!
//! The same shape as the insert slots in `crate::plugins`, one slot per track instead of eight, and
//! for the same reasons: every plugin lives on the main thread, and a rebuilt engine (device
//! change, score loaded) gets its instruments installed again by [`Manager::reconcile`].
//!
//! # Kept by track name
//!
//! Which instrument a track plays and the instrument's state are written to `instruments.json`
//! next to `plugin-rack.json`, keyed by **track name** like the insert rack: a score that orders
//! its tracks differently still finds the piano on "piano". A track that is not an instrument
//! track in the running engine keeps its entry; the instrument comes back when the track does.
//! What MIDI device and channel play the track is part of the track's setup (`TrackConfig`), not
//! of this file.

use std::cell::RefCell;
use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock, mpsc};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Emitter};

use looper_engine::engine::insert::MAX_BLOCK;
use looper_engine::engine::source::{
    Instrument, InstrumentMsg, InstrumentSender, SourceShared, TrackRoute,
};
use looper_engine::plugin::scan::{self, Scanned};
use looper_engine::plugin::{self, Kind, PluginInstance};

use crate::logfile::log;
use crate::proto::InstrumentInput;

/// Event for everything this module reports: see [`InstrumentEvent`].
pub const INSTRUMENTS_EVENT: &str = "looper://instruments";

/// How often the main thread looks after the instruments: latency, editor, meters, autosave.
const TICK: Duration = Duration::from_millis(250);
/// How long removing an instrument waits for the audio thread to hand its processor back.
const RETIRE_WAIT: Duration = Duration::from_millis(750);

// ---------------------------------------------------------------------------------------------
// The link to the running engine
// ---------------------------------------------------------------------------------------------

struct Link {
    sender: InstrumentSender,
    sample_rate: f64,
    generation: u64,
}

static LINK: Mutex<Option<Link>> = Mutex::new(None);
static GENERATION: AtomicU64 = AtomicU64::new(0);
static APP: OnceLock<AppHandle> = OnceLock::new();

/// One track of the running engine as this module needs it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TrackSlot {
    pub name: String,
    /// Whether the track is played by an instrument (`input: instrument`).
    pub instrument: bool,
}

/// Called by the host thread once a new engine runs, with its tracks in order and the renames since
/// the last start. The instruments are (re)installed on the main thread right after.
pub fn engine_started(
    sender: InstrumentSender,
    sample_rate: u32,
    tracks: Vec<TrackSlot>,
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

/// Called by the host thread after the engine's streams are gone, and with them every instrument
/// processor that was installed.
pub fn engine_stopped() {
    LINK.lock().unwrap().take();
    schedule(|m| m.reconcile());
}

// ---------------------------------------------------------------------------------------------
// Running things on the main thread
// ---------------------------------------------------------------------------------------------

thread_local! {
    static MANAGER: RefCell<Manager> = RefCell::new(Manager::default());
}

fn on_main<T, F>(work: F) -> Result<T, String>
where
    T: Send + 'static,
    F: FnOnce(&mut Manager) -> Result<T, String> + Send + 'static,
{
    let app = APP
        .get()
        .ok_or_else(|| "The instrument host is not ready yet.".to_string())?;
    let (tx, rx) = mpsc::channel();
    app.run_on_main_thread(move || {
        let result = MANAGER.with(|m| match m.try_borrow_mut() {
            Ok(mut m) => work(&mut m),
            Err(_) => Err("The instrument host is busy. Try again.".to_string()),
        });
        let _ = tx.send(result);
    })
    .map_err(|e| format!("The instrument host is not reachable: {e}"))?;
    rx.recv()
        .map_err(|_| "The instrument host did not answer.".to_string())?
}

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

/// Start the instrument host. Call from Tauri's `setup` (on the main thread).
pub fn init(app: &AppHandle) {
    let _ = APP.set(app.clone());
    schedule(|m| m.restore());
    std::thread::Builder::new()
        .name("instrument-tick".into())
        .spawn(|| {
            loop {
                std::thread::sleep(TICK);
                schedule(|m| m.tick());
            }
        })
        .ok();
}

/// Debug builds only, for checking a real instrument without a mouse:
/// `HENRYS_LOOPER_DEV_INSTRUMENT_CHECK=<plugin id>` puts the instrument on a track "Dev check"
/// (unless the last session's restore already did - then its state size is logged), opens its
/// window, logs the window's geometry twice, closes it and **quits the program** through the
/// normal way out. Run twice with the same config folder to see the state come back.
#[cfg(debug_assertions)]
pub fn dev_check(app: AppHandle) {
    let Ok(id) = std::env::var("HENRYS_LOOPER_DEV_INSTRUMENT_CHECK") else {
        return;
    };
    const TRACK: &str = "Dev check";
    std::thread::spawn(move || {
        std::thread::sleep(Duration::from_secs(4));
        let restored = on_main(|m| {
            Ok(m.position(TRACK)
                .map(|p| m.slots[p].plugin.save_state().map(|s| s.len())))
        });
        log!("dev check: restored from the last session: {restored:?}");
        if !matches!(restored, Ok(Some(_))) {
            let id = id.clone();
            let put = on_main(move |m| m.put(TRACK, &id, None).map(|_| ()));
            log!("dev check: put {put:?}");
        }
        let opened = on_main(|m| m.editor(TRACK, true).map(|_| ()));
        log!("dev check: editor {opened:?}");
        for _ in 0..2 {
            std::thread::sleep(Duration::from_secs(2));
            let report = on_main(|m| {
                let p = m.position(TRACK).ok_or("gone")?;
                let s = &m.slots[p];
                Ok(format!(
                    "{} | state {:?} bytes",
                    s.plugin.editor_report().unwrap_or_default(),
                    s.plugin.save_state().map(|s| s.len())
                ))
            });
            log!("dev check: {report:?}");
        }
        let _ = on_main(|m| m.editor(TRACK, false).map(|_| ()));
        std::thread::sleep(Duration::from_secs(1));
        log!("dev check: quitting");
        app.exit(0);
    });
}

/// On the way out of the program, on the main thread, **after** the engine has stopped: save,
/// then close every window and unload every instrument in order. One the engine still holds is
/// left alone. Returns how many were left alone. Like [`crate::plugins::shutdown`].
pub fn shutdown() -> usize {
    MANAGER.with(|m| match m.try_borrow_mut() {
        Ok(mut m) => m.shutdown(),
        Err(_) => {
            log!("Instrument host busy on the way out; instruments are left loaded.");
            usize::MAX
        }
    })
}

// ---------------------------------------------------------------------------------------------
// Wire format
// ---------------------------------------------------------------------------------------------

/// One track's instrument as the UI sees it.
#[derive(Clone, Debug, Serialize)]
pub struct InstrumentView {
    /// Track name - the key.
    pub track: String,
    /// 0-based index in the running engine, if the track exists there as an instrument track.
    pub index: Option<usize>,
    pub plugin_id: String,
    pub name: String,
    pub vendor: String,
    pub format: plugin::Format,
    /// Latency the instrument reports, in frames.
    pub latency: u32,
    pub has_editor: bool,
    pub editor_open: bool,
    /// Installed in the running engine.
    pub active: bool,
    pub blocks: u64,
    pub peak: f32,
    pub cpu_us: f32,
    pub error: Option<String>,
}

#[derive(Clone, Debug, Serialize)]
pub struct InstrumentRack {
    pub supported: bool,
    pub engine_running: bool,
    pub instruments: Vec<InstrumentView>,
    /// Tracks whose instrument is being loaded right now or waits to be restored. Loading a big
    /// instrument holds the main thread for a moment; the UI says "Loading instrument…" meanwhile.
    pub loading: Vec<String>,
}

#[derive(Clone, Debug, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum InstrumentEvent {
    Rack { rack: InstrumentRack },
    Notice { message: String },
}

fn emit(event: InstrumentEvent) {
    if let Some(app) = APP.get() {
        let _ = app.emit(INSTRUMENTS_EVENT, event);
    }
}

// ---------------------------------------------------------------------------------------------
// The manager
// ---------------------------------------------------------------------------------------------

struct Slot {
    track: String,
    info: Scanned,
    plugin: Box<dyn PluginInstance>,
    shared: Arc<SourceShared>,
    /// Engine generation and track index the processor was installed into.
    active: Option<(u64, usize)>,
    error: Option<String>,
    editor_was_open: bool,
    heard: bool,
}

#[derive(Default)]
struct Manager {
    slots: Vec<Slot>,
    /// States of instruments that were taken off a track, by (track, plugin id): a score that puts
    /// the same instrument back gets its sound back, not the plugin's default.
    shelf: Vec<SavedInstrument>,
    layout: Vec<TrackSlot>,
    dirty: bool,
    last_change: Option<Instant>,
    last_meter: Option<Instant>,
    /// Instruments of the last session still to be loaded, one per turn of the main loop.
    restoring: VecDeque<SavedInstrument>,
    /// The track whose instrument is loading in this very call.
    loading: Option<String>,
}

#[derive(Serialize, Deserialize, Default)]
struct SavedInstruments {
    tracks: Vec<SavedInstrument>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    shelf: Vec<SavedInstrument>,
}

/// How many taken-off instruments keep their state.
const SHELF: usize = 32;

#[derive(Serialize, Deserialize, Clone)]
struct SavedInstrument {
    track: String,
    plugin_id: String,
    name: String,
    /// The plugin's state chunk, base64.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    state: Option<String>,
}

fn store_path() -> PathBuf {
    scan::cache_path().with_file_name("instruments.json")
}

fn restore_marker() -> PathBuf {
    scan::cache_path().with_file_name("instruments.restoring")
}

impl Manager {
    fn index_of(&self, track: &str) -> Option<usize> {
        self.layout
            .iter()
            .position(|t| t.name == track && t.instrument)
    }

    fn position(&self, track: &str) -> Option<usize> {
        self.slots.iter().position(|s| s.track == track)
    }

    fn set_layout(&mut self, tracks: Vec<TrackSlot>, renamed: &[(String, String)]) {
        for (from, to) in renamed {
            // A renamed track takes its instrument along - unless the new name already has one.
            if self.position(to).is_none()
                && let Some(p) = self.position(from)
            {
                self.slots[p].track = to.clone();
                self.dirty = true;
            }
        }
        self.layout = tracks;
        self.publish();
    }

    /// Tell the score side (`crate::pluginmix`) which instrument sits on which track.
    fn publish(&self) {
        crate::pluginmix::publish_instruments(
            self.slots
                .iter()
                .map(|s| (s.track.clone(), s.info.info.id.clone()))
                .collect(),
        );
    }

    /// Keep the state of an instrument that is taken off its track.
    fn shelve(&mut self, p: usize) {
        let slot = &self.slots[p];
        let Some(state) = slot.plugin.save_state().ok().map(|b| crate::plugins::encode_state(&b))
        else {
            return;
        };
        let (track, id) = (slot.track.clone(), slot.info.info.id.clone());
        self.shelf.retain(|e| !(e.track == track && e.plugin_id == id));
        self.shelf.push(SavedInstrument {
            track,
            plugin_id: id,
            name: slot.info.info.name.clone(),
            state: Some(state),
        });
        if self.shelf.len() > SHELF {
            self.shelf.remove(0);
        }
    }

    /// What a loaded score says plays `track`. The same plugin already there is kept as it is,
    /// with its sound; another one is loaded (with the state it last had on this track, if it
    /// had one here); one that is not installed leaves the track silent and says so.
    fn apply_score(&mut self, track: &str, plugin_id: &str) {
        if self
            .position(track)
            .is_some_and(|p| self.slots[p].info.info.id == plugin_id)
        {
            crate::pluginmix::found(track, true);
            return;
        }
        if self.position(track).is_none() && crate::pluginmix::instrument_is_missing(track, plugin_id) {
            return;
        }
        // Still waiting to be restored from the last session: the same one stays queued (with its
        // sound); another one's sound goes on the shelf before the score's replaces it.
        if let Some(pending) = self.restoring.iter().find(|e| e.track == track).cloned() {
            if pending.plugin_id == plugin_id {
                crate::pluginmix::found(track, true);
                return;
            }
            if pending.state.is_some() {
                self.shelf
                    .retain(|e| !(e.track == track && e.plugin_id == pending.plugin_id));
                self.shelf.push(pending);
            }
        }
        match self.set(track, Some(plugin_id)) {
            Ok(_) => {
                crate::pluginmix::found(track, true);
                log!("Instrument {plugin_id} on track \"{track}\" from the score.");
            }
            Err(e) => {
                // The score is the arrangement: an instrument it does not ask for does not play
                // in its place. Its state is shelved, the score keeps its entry.
                let _ = self.remove(track);
                crate::pluginmix::instrument_missing(track, plugin_id);
                let message = format!(
                    "Track \"{track}\": the score's instrument \"{plugin_id}\" is not available \
                     here ({e}). The track stays silent; the score keeps the entry."
                );
                log!("{message}");
                emit(InstrumentEvent::Notice { message });
            }
        }
    }

    fn view(&self) -> InstrumentRack {
        let link = LINK.lock().unwrap();
        let generation = link.as_ref().map(|l| l.generation);
        let instruments = self
            .slots
            .iter()
            .map(|s| InstrumentView {
                track: s.track.clone(),
                index: self.index_of(&s.track),
                plugin_id: s.info.info.id.clone(),
                name: s.info.info.name.clone(),
                vendor: s.info.info.vendor.clone(),
                format: s.info.info.format,
                latency: s.shared.latency(),
                has_editor: s.plugin.has_editor(),
                editor_open: s.plugin.editor_open(),
                active: s.active.is_some_and(|(g, _)| Some(g) == generation),
                blocks: s.shared.blocks(),
                peak: s.shared.take_peak(),
                cpu_us: s.shared.take_peak_us(),
                error: s.error.clone(),
            })
            .collect();
        InstrumentRack {
            supported: plugin::SUPPORTED,
            engine_running: link.is_some(),
            instruments,
            loading: self
                .loading
                .iter()
                .chain(self.restoring.iter().map(|e| &e.track))
                .cloned()
                .collect(),
        }
    }

    fn changed(&mut self) {
        self.dirty = true;
        self.last_change = Some(Instant::now());
        self.publish();
        emit(InstrumentEvent::Rack { rack: self.view() });
    }

    fn catalog_entry(plugin_id: &str) -> Result<Scanned, String> {
        let entry = scan::cached()
            .into_iter()
            .find(|p| p.info.id == plugin_id)
            .ok_or_else(|| {
                format!("No plugin with the id '{plugin_id}' is known. Scan for plugins first.")
            })?;
        if entry.info.kind != Kind::Instrument {
            return Err(format!(
                "\"{}\" is not an instrument. Effects go into the plugin slots of the mixer.",
                entry.info.name
            ));
        }
        if !entry.loadable() {
            return Err(format!(
                "\"{}\" cannot be played: {}",
                entry.info.name,
                entry
                    .message
                    .clone()
                    .unwrap_or_else(|| "it did not pass the scan.".to_string())
            ));
        }
        Ok(entry)
    }

    /// Put one slot's instrument into the running engine, if its track is an instrument track
    /// there and it is not installed yet.
    fn install(&mut self, p: usize) {
        let (sample_rate, generation) = {
            let link = LINK.lock().unwrap();
            let Some(link) = link.as_ref() else {
                return;
            };
            (link.sample_rate, link.generation)
        };
        let index = self.index_of(&self.slots[p].track);
        let slot = &mut self.slots[p];
        if slot.active.is_some_and(|a| a == (generation, index.unwrap_or(usize::MAX))) {
            return;
        }
        if slot.active.take().is_some() {
            // Its processor went down with the previous engine (or is on its way back).
            slot.plugin.deactivate();
        }
        let Some(index) = index else {
            return;
        };
        match slot.plugin.activate_instrument(sample_rate, MAX_BLOCK) {
            Ok(processor) => {
                slot.shared.set_latency(slot.plugin.latency());
                let msg = InstrumentMsg::Install {
                    track: index,
                    instrument: Instrument::new(processor, slot.shared.clone()),
                };
                let mut link = LINK.lock().unwrap();
                match link.as_mut() {
                    Some(link) if link.generation == generation => match link.sender.send(msg) {
                        Ok(()) => {
                            slot.active = Some((generation, index));
                            slot.error = None;
                            slot.heard = false;
                            log!(
                                "Instrument {} on track \"{}\" started: {} Hz, latency {} frames.",
                                slot.info.info.name,
                                slot.track,
                                sample_rate,
                                slot.shared.latency()
                            );
                        }
                        Err(e) => slot.error = Some(e),
                    },
                    _ => {
                        drop(msg);
                        slot.plugin.deactivate();
                    }
                }
            }
            Err(e) => {
                log!("Instrument {} does not start: {e}", slot.info.info.name);
                slot.error = Some(e);
            }
        }
    }

    fn reconcile(&mut self) {
        let running = LINK.lock().unwrap().is_some();
        for p in 0..self.slots.len() {
            if running {
                self.install(p);
            } else {
                let slot = &mut self.slots[p];
                if slot.active.take().is_some() {
                    slot.plugin.deactivate();
                }
            }
        }
        emit(InstrumentEvent::Rack { rack: self.view() });
    }

    /// Take a slot's instrument out of the engine and wait until the audio thread let go of it.
    /// `false` when it did not come back in time - it must then not be destroyed.
    fn retire(&mut self, p: usize) -> bool {
        let Some((generation, index)) = self.slots[p].active.take() else {
            return true;
        };
        let sent = LINK
            .lock()
            .unwrap()
            .as_mut()
            .filter(|l| l.generation == generation)
            .map(|l| l.sender.send(InstrumentMsg::Remove { track: index }));
        if let Some(Err(e)) = sent {
            log!("Instrument not removed: {e}");
            self.slots[p].active = Some((generation, index));
            return false;
        }
        let started = Instant::now();
        while Arc::strong_count(&self.slots[p].shared) > 1 && started.elapsed() < RETIRE_WAIT {
            if let Some(link) = LINK.lock().unwrap().as_mut() {
                link.sender.drain_retired();
            }
            std::thread::sleep(Duration::from_millis(2));
        }
        if Arc::strong_count(&self.slots[p].shared) > 1 {
            return false;
        }
        self.slots[p].plugin.deactivate();
        true
    }

    fn remove(&mut self, track: &str) -> Result<(), String> {
        let Some(p) = self.position(track) else {
            return Ok(());
        };
        self.shelve(p);
        if !self.retire(p) {
            let slot = self.slots.remove(p);
            log!(
                "Instrument {} did not come back from the engine and stays in memory.",
                slot.info.info.name
            );
            std::mem::forget(slot);
            self.changed();
            return Ok(());
        }
        let slot = self.slots.remove(p);
        log!(
            "Instrument removed: {} from track \"{}\"",
            slot.info.info.name,
            slot.track
        );
        drop(slot);
        self.changed();
        Ok(())
    }

    fn set(&mut self, track: &str, plugin_id: Option<&str>) -> Result<InstrumentRack, String> {
        if !plugin::SUPPORTED {
            return Err(plugin::UNSUPPORTED.to_string());
        }
        // The musician's choice wins over a restore still waiting for this track.
        self.restoring.retain(|e| e.track != track);
        let Some(plugin_id) = plugin_id else {
            self.remove(track)?;
            return Ok(self.view());
        };
        self.put(track, plugin_id, None)
    }

    /// Load `plugin_id` for `track`, give it `state` while it is not running yet, and start it.
    fn put(
        &mut self,
        track: &str,
        plugin_id: &str,
        state: Option<&[u8]>,
    ) -> Result<InstrumentRack, String> {
        let entry = Self::catalog_entry(plugin_id)?;
        self.remove(track)?;
        // Without a state of its own: the sound this instrument last had on this track, if it was
        // taken off here before (a score that puts it back gets it back).
        let shelved = match state {
            Some(_) => None,
            None => self
                .shelf
                .iter()
                .position(|e| e.track == track && e.plugin_id == plugin_id)
                .and_then(|at| self.shelf.remove(at).state)
                .and_then(|text| crate::plugins::decode_state(&text)),
        };
        let state = state.or(shelved.as_deref());
        self.loading = Some(track.to_string());
        emit(InstrumentEvent::Rack { rack: self.view() });
        let started = Instant::now();
        let loaded = plugin::load(&entry.info).map(|mut instance| {
            let restored = state.map(|s| instance.load_state(s));
            (instance, restored)
        });
        self.loading = None;
        let (instance, restored) = loaded?;
        log!(
            "Instrument loaded: {} ({}) for track \"{track}\" in {} ms",
            entry.info.name,
            entry.info.format.label(),
            started.elapsed().as_millis()
        );
        if let Some(Err(e)) = restored {
            log!("State of {} not loaded: {e}", entry.info.name);
            emit(InstrumentEvent::Notice {
                message: format!(
                    "\"{}\" on track \"{track}\" was loaded without its saved sound: {e}",
                    entry.info.name
                ),
            });
        }
        self.slots.push(Slot {
            track: track.to_string(),
            info: entry,
            plugin: instance,
            shared: SourceShared::new(0),
            active: None,
            error: None,
            editor_was_open: false,
            heard: false,
        });
        let p = self.slots.len() - 1;
        self.install(p);
        self.changed();
        Ok(self.view())
    }

    fn editor(&mut self, track: &str, open: bool) -> Result<InstrumentRack, String> {
        let p = self
            .position(track)
            .ok_or_else(|| format!("Track \"{track}\" has no instrument."))?;
        let slot = &mut self.slots[p];
        if open {
            let title = format!("{} \u{2013} {}", slot.info.info.name, slot.track);
            let started = std::time::Instant::now();
            slot.plugin
                .open_editor(&title, &format!("instrument/{}", slot.track))?;
            log!(
                "Instrument window {title} open after {:?}: {}",
                started.elapsed(),
                slot.plugin.editor_report().unwrap_or_default()
            );
        } else {
            slot.plugin.close_editor();
        }
        slot.editor_was_open = open;
        let view = self.view();
        emit(InstrumentEvent::Rack { rack: view.clone() });
        Ok(view)
    }

    fn panic(&self, track: Option<&str>) {
        for s in &self.slots {
            if track.is_none_or(|t| t == s.track) {
                s.shared.request_panic();
            }
        }
    }

    fn tick(&mut self) {
        // A restore step whose turn was lost (the manager was busy) is picked up here.
        if !self.restoring.is_empty() {
            self.restore_next();
        }
        if let Some(link) = LINK.lock().unwrap().as_mut() {
            link.sender.drain_retired();
        }
        let mut changed = false;
        for s in self.slots.iter_mut() {
            if s.active.is_some() {
                let latency = s.plugin.latency();
                if latency != s.shared.latency() {
                    s.shared.set_latency(latency);
                    changed = true;
                }
                if !s.heard && s.shared.blocks() > 0 {
                    s.heard = true;
                    log!(
                        "Instrument {} on track \"{}\" is rendering ({} blocks).",
                        s.info.info.name,
                        s.track,
                        s.shared.blocks()
                    );
                }
            }
            let open = s.plugin.editor_open();
            if open != s.editor_was_open {
                s.editor_was_open = open;
                changed = true;
                // A sound loaded in the window (Kontakt: an instrument) need not move a
                // parameter: the state is saved when the window is closed.
                if !open {
                    self.dirty = true;
                    self.last_change = Some(Instant::now());
                }
            }
            // A sound chosen in the instrument's own window is state that has to be kept.
            if !s.plugin.take_changed().is_empty() {
                self.dirty = true;
                self.last_change = Some(Instant::now());
            }
        }
        let meter_due = self
            .last_meter
            .is_none_or(|t| t.elapsed() >= Duration::from_secs(1));
        if changed || (meter_due && !self.slots.is_empty()) {
            self.last_meter = Some(Instant::now());
            emit(InstrumentEvent::Rack { rack: self.view() });
        }
        // Saving half a restore would forget the instruments that are still to come.
        if self.dirty
            && self.restoring.is_empty()
            && self
                .last_change
                .is_none_or(|t| t.elapsed() >= Duration::from_secs(1))
        {
            self.save();
        }
    }

    fn shutdown(&mut self) -> usize {
        // Saving half a restore would forget the instruments that are still to come.
        if self.restoring.is_empty() {
            self.save();
        }
        let mut kept = 0;
        for mut slot in std::mem::take(&mut self.slots) {
            slot.plugin.close_editor();
            if Arc::strong_count(&slot.shared) > 1 {
                log!(
                    "Instrument {} is still in the engine on the way out and stays loaded.",
                    slot.info.info.name
                );
                std::mem::forget(slot);
                kept += 1;
                continue;
            }
            if slot.active.take().is_some() {
                slot.plugin.deactivate();
            }
            drop(slot);
        }
        kept
    }

    fn save(&mut self) {
        self.dirty = false;
        let saved = SavedInstruments {
            tracks: self
                .slots
                .iter()
                .map(|s| SavedInstrument {
                    track: s.track.clone(),
                    plugin_id: s.info.info.id.clone(),
                    name: s.info.info.name.clone(),
                    state: s.plugin.save_state().ok().map(|b| crate::plugins::encode_state(&b)),
                })
                .collect(),
            shelf: self.shelf.clone(),
        };
        let path = store_path();
        if let Some(dir) = path.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        if let Ok(text) = serde_json::to_string_pretty(&saved)
            && let Err(e) = std::fs::write(&path, text)
        {
            log!("Instruments not saved: {e}");
        }
    }

    /// Load the instruments of the last session. Guarded by a marker file like the insert rack: an
    /// instrument that takes the program down while it loads is not loaded the next time.
    fn restore(&mut self) {
        let marker = restore_marker();
        if marker.exists() {
            let _ = std::fs::remove_file(&marker);
            let path = store_path();
            let _ = std::fs::rename(&path, path.with_extension("json.bak"));
            let message = "The instruments of the last session were not loaded: the program \
                           stopped while loading them last time. The previous setup was kept as \
                           instruments.json.bak."
                .to_string();
            log!("{message}");
            emit(InstrumentEvent::Notice { message });
            return;
        }
        let Ok(text) = std::fs::read_to_string(store_path()) else {
            return;
        };
        let Ok(saved) = serde_json::from_str::<SavedInstruments>(&text) else {
            return;
        };
        self.shelf = saved.shelf;
        if saved.tracks.is_empty() {
            return;
        }
        let _ = std::fs::write(&marker, "restoring");
        self.restoring = saved.tracks.into();
        emit(InstrumentEvent::Rack { rack: self.view() });
        schedule(|m| m.restore_next());
    }

    /// Load the next instrument of the last session, then let the main loop breathe (the window
    /// draws, the UI shows which instruments are still loading) before the next one.
    fn restore_next(&mut self) {
        let Some(entry) = self.restoring.pop_front() else {
            return;
        };
        let state = entry.state.as_deref().and_then(crate::plugins::decode_state);
        if let Err(e) = self.put(&entry.track, &entry.plugin_id, state.as_deref()) {
            log!("Instrument {} not restored: {e}", entry.name);
            emit(InstrumentEvent::Notice {
                message: format!(
                    "\"{}\" was not loaded again for track \"{}\": {e}",
                    entry.name, entry.track
                ),
            });
        }
        if self.restoring.is_empty() {
            let _ = std::fs::remove_file(restore_marker());
            self.dirty = false;
        } else {
            schedule(|m| m.restore_next());
        }
        self.publish();
        emit(InstrumentEvent::Rack { rack: self.view() });
    }
}

// ---------------------------------------------------------------------------------------------
// Routing: which port, channel and keys play which track
// ---------------------------------------------------------------------------------------------

/// The engine routing for every track, from what the setup says plays it and the ports that are
/// open (slot, name). A track that names a device takes the ports that match it; one that names
/// none takes every open port. A track that is no instrument track gets nothing.
pub fn track_routes(inputs: &[Option<InstrumentInput>], ports: &[(usize, &str)]) -> Vec<TrackRoute> {
    inputs
        .iter()
        .map(|input| {
            let Some(input) = input else {
                return TrackRoute::NONE;
            };
            let ports = ports
                .iter()
                .filter(|(slot, name)| {
                    *slot < 32
                        && input
                            .device
                            .as_deref()
                            .is_none_or(|d| crate::midi::device_matches(d, name))
                })
                .fold(0u32, |mask, (slot, _)| mask | (1 << slot));
            TrackRoute {
                ports,
                channels: match input.channel {
                    Some(c @ 1..=16) => 1 << (c - 1),
                    _ => TrackRoute::ALL_CHANNELS,
                },
                low: input.note_low.unwrap_or(0).min(127),
                high: input.note_high.unwrap_or(127).min(127),
            }
        })
        .collect()
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

/// Every track's instrument.
#[tauri::command]
pub async fn instrument_rack() -> Result<InstrumentRack, String> {
    blocking(|| on_main(|m| Ok(m.view()))).await
}

/// Put an instrument on a track (by name), or take it off with `plugin_id: null`.
#[tauri::command(rename_all = "snake_case")]
pub async fn instrument_set(track: String, plugin_id: Option<String>) -> Result<InstrumentRack, String> {
    blocking(move || {
        on_main(move |m| {
            let view = m.set(&track, plugin_id.as_deref())?;
            // A human choice: the open score writes it into its `mixer:` section.
            crate::pluginmix::touch_instrument(&track);
            Ok(view)
        })
    })
    .await
}

/// A loaded score (or an undo in its `mixer:` section) says which instrument plays `track`.
/// Handed to the main thread without waiting.
pub fn apply_score(track: String, plugin_id: String) {
    schedule(move |m| m.apply_score(&track, &plugin_id));
}

/// Open or close the instrument's own window.
#[tauri::command(rename_all = "snake_case")]
pub async fn instrument_editor(track: String, open: bool) -> Result<InstrumentRack, String> {
    blocking(move || on_main(move |m| m.editor(&track, open))).await
}

/// "All notes off" on one track's instrument, or on all of them.
#[tauri::command(rename_all = "snake_case")]
pub async fn instrument_panic(track: Option<String>) -> Result<(), String> {
    blocking(move || {
        on_main(move |m| {
            m.panic(track.as_deref());
            Ok(())
        })
    })
    .await
}

#[cfg(test)]
mod tests {
    use super::*;

    fn input(device: Option<&str>, channel: Option<u8>) -> Option<InstrumentInput> {
        Some(InstrumentInput {
            device: device.map(str::to_string),
            channel,
            monitor: true,
            ..InstrumentInput::default()
        })
    }

    /// A track names its keyboard; another takes whatever is plugged in; an audio track gets
    /// nothing. Channels and key ranges travel as masks.
    #[test]
    fn every_track_gets_the_ports_channels_and_keys_it_names() {
        let ports = [(0, "KeyLab 49"), (3, "MPD218 Port A")];
        let mut pads = input(Some("mpd"), Some(10));
        if let Some(p) = pads.as_mut() {
            p.note_low = Some(36);
            p.note_high = Some(51);
        }
        let routes = track_routes(
            &[input(Some("KeyLab"), Some(1)), None, input(None, None), pads],
            &ports,
        );
        assert_eq!(routes[0].ports, 0b1);
        assert_eq!(routes[0].channels, 1);
        assert_eq!(routes[1], TrackRoute::NONE);
        assert_eq!(routes[2].ports, 0b1001, "kein Geraet genannt: alle offenen");
        assert_eq!(routes[2].channels, TrackRoute::ALL_CHANNELS);
        assert_eq!(routes[3].ports, 0b1000);
        assert_eq!(routes[3].channels, 1 << 9);
        assert_eq!((routes[3].low, routes[3].high), (36, 51));
        assert!(track_routes(&[input(Some("Nord"), None)], &ports)[0].ports == 0);
    }
}
