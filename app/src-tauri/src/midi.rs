//! MIDI in the app: an open port, the mapping, and the learn mode the user interface drives.
//!
//! ```text
//!   Controller ──► midi::input ──► drain() ──► resolve() ──► MidiAction ──► Session::apply
//!   (Treiber-      lock-freie      Host-      Router +       (dieselbe      (derselbe Weg
//!    Thread)       Queue           Thread     Profil         Absicht wie     wie ein
//!                                             + Partitur     ein Klick)      Mausklick)
//! ```
//!
//! # What is here and what is in the engine library
//!
//! Everything *about* MIDI - the byte decoding, the address tree, the mapping, the resolution, the
//! learn mechanic, the profile file - lives in `looper_engine::midi` and is provable without a
//! device. This file is the part that only makes sense inside the app: it holds the open port, the
//! profile belonging to it, the score's supplement laid over it, and it turns every incoming event
//! into something the web view can show.
//!
//! # Why MIDI events do not ride in the status snapshot
//!
//! The status event is a **twenty-per-second sampling of a continuous state**, and `Status::latest`
//! deliberately throws older snapshots away - a position that is one snapshot old is worthless.
//! MIDI is the opposite: a rare, bursty stream of *discrete* facts where every single one matters.
//! A pad press that fell into the gap between two snapshots would be a pad press the monitor never
//! shows, and the monitor exists precisely to answer "did that pad send anything at all". So MIDI
//! travels on its own event ([`crate::host::MIDI_EVENT`]), queued rather than sampled.
//!
//! What it does borrow from the status path is the *rate discipline*: a knob sweep produces about a
//! hundred events a second, and each one turning into a message to the web view would cost more
//! than the whole status stream. So reports are collected and emitted at most every
//! [`EMIT_INTERVAL`] - except when the batch contains something discrete (a pad, a refusal, a
//! learned binding), which is flushed at once because that is what the eye is waiting for.

use std::path::PathBuf;
use std::time::{Duration, Instant};

use serde::Serialize;

use looper_engine::midi::input::{self, MidiIn};
use looper_engine::midi::{
    Binding, Context, Learn, MidiEvent, MidiId, MidiIdKind, MidiMap, Profile, Resolution, Router,
    Target, profile_for_port, profile_path, score_map,
};
use looper_engine::score::CompiledScore;

/// How often the web view is told about a knob that is being swept. Twenty-five updates a second
/// is smooth for a monitor list and two orders of magnitude below what a sweep produces.
const EMIT_INTERVAL: Duration = Duration::from_millis(40);
/// Reports kept while waiting for the next emit window. A knob sweep coalesces (see
/// [`MidiBridge::report`]), so this is only ever reached by something genuinely pathological.
const PENDING_MAX: usize = 64;
/// How often an open port is checked for still being there. Windows does not tell us when a device
/// is unplugged, so the port list is asked - at a rate where asking costs nothing.
const ALIVE_INTERVAL: Duration = Duration::from_secs(2);

// ---------------------------------------------------------------------------------------------
// What the web view gets
// ---------------------------------------------------------------------------------------------

/// What one incoming event did. The same four answers [`Resolution`] gives, plus the one that only
/// exists while the learn mode is on.
#[derive(Serialize, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum MidiOutcome {
    /// Nothing is bound to this control.
    Unbound,
    /// Bound, and correctly did nothing - the note off of a trigger, a knob that has not caught its
    /// parameter yet.
    Absorbed,
    /// Bound and carried out.
    Action,
    /// Bound, but not allowed right now - the runner owns the transport, or the track is not there.
    Refused,
    /// The learn mode was waiting and this control just became the pending target.
    Learned,
}

/// One incoming event as the monitor prints it: what came in, and what the mapping made of it.
#[derive(Serialize, Clone, Debug)]
#[serde(rename_all = "snake_case")]
pub struct MidiEventReport {
    /// Counts up for the life of the app, so a list can be keyed by it.
    pub seq: u64,
    /// `ch1.note36` - the string a profile file is keyed by.
    pub id: String,
    /// `Pad 36`, `CC 3` - the badge on a bound control.
    pub short: String,
    pub channel: u8,
    /// German word for the kind: `Note an`, `Note aus`, `Regler`, `Pitchbend`.
    pub what: String,
    /// Note or controller number. Absent for a pitch bend, which has none.
    pub number: Option<u8>,
    /// Velocity, controller value, or the bend around zero.
    pub value: i32,
    /// The address this control is bound to, if any.
    pub target: Option<String>,
    /// Its German name.
    pub target_label: Option<String>,
    pub outcome: MidiOutcome,
    /// The German sentence belonging to the outcome, where there is one.
    pub note: Option<String>,
    /// True for the steady stream a knob produces. The one field the emit throttle reads.
    pub continuous: bool,
}

/// One MIDI input port as the system lists it, with the profile that would be loaded for it.
#[derive(Serialize, Clone, Debug)]
#[serde(rename_all = "snake_case")]
pub struct MidiPortView {
    pub index: usize,
    pub name: String,
    /// `device:` of the profile that matches this port, or `null` when there is none yet.
    pub profile: Option<String>,
    pub bindings: usize,
}

/// One entry of the profile overview.
#[derive(Serialize, Clone, Debug)]
#[serde(rename_all = "snake_case")]
pub struct MidiBindingView {
    pub id: String,
    pub short: String,
    pub channel: u8,
    /// `note`, `cc` or `bend`.
    pub kind: String,
    pub number: u8,
    pub address: String,
    /// German name of the target, from `Target::label`.
    pub label: String,
    /// The same plus the button behaviour or the range - what `midi profile` prints.
    pub describe: String,
    /// True when this binding comes from the loaded score rather than from the controller profile.
    /// A score binding is not saved into the profile; it comes back with the score.
    pub from_score: bool,
}

/// Everything about the MIDI side that is not a single event: the connection, the mapping and the
/// learn mode. Small enough to send whole whenever any of it changes.
#[derive(Serialize, Clone, Debug)]
#[serde(rename_all = "snake_case")]
pub struct MidiView {
    pub connected: bool,
    /// The port as the system names it.
    pub port: Option<String>,
    /// `device:` of the profile in use.
    pub device: Option<String>,
    /// Where the profile is (or would be) saved.
    pub path: Option<String>,
    /// True when something was learned or unbound and not yet written to the file.
    pub dirty: bool,
    /// The address the learn mode is waiting for, or `null`.
    pub learning: Option<String>,
    /// Its German name, for the banner.
    pub learning_label: Option<String>,
    pub bindings: Vec<MidiBindingView>,
    /// One German line per control the score took over from the profile.
    pub notes: Vec<String>,
    /// The newest German sentence about what happened here.
    pub message: Option<String>,
    /// Events lost between the driver callback and the host thread. Zero unless something is wrong.
    pub dropped: u64,
}

/// What travels on `looper://midi`.
#[derive(Serialize, Clone, Debug)]
#[serde(rename_all = "snake_case")]
pub struct MidiFeed {
    pub events: Vec<MidiEventReport>,
    /// Sent along whenever the connection, the mapping or the learn mode changed - so the web view
    /// never has to poll for it.
    pub view: Option<MidiView>,
}

/// Answer of "save the profile".
#[derive(Serialize, Clone, Debug)]
#[serde(rename_all = "snake_case")]
pub struct MidiSaved {
    pub path: String,
    /// German sentence naming the file, ready to show.
    pub message: String,
    pub view: MidiView,
}

// ---------------------------------------------------------------------------------------------
// The bridge
// ---------------------------------------------------------------------------------------------

/// The open port, the mapping it resolves against, and the learn mode.
///
/// Lives on the host thread, next to the session, and is never touched from anywhere else - which
/// is why it may hold a `MidiIn` (whose connection is bound to the thread that opened it).
pub struct MidiBridge {
    input: Option<MidiIn>,
    /// The controller profile, as loaded and as edited by the learn mode.
    profile: Option<Profile>,
    /// Where that profile is written. Set at open time, even when the file does not exist yet.
    path: Option<PathBuf>,
    /// The loaded score's `midi:` block, laid over the profile.
    score: MidiMap,
    router: Router,
    learn: Learn,
    /// Learned or unbound since the last save.
    dirty: bool,
    /// One German line per control the score took over from the profile.
    notes: Vec<String>,
    message: Option<String>,
    seq: u64,
    pending: Vec<MidiEventReport>,
    /// Set when the connection, the mapping or the learn mode changed and the web view has not been
    /// told yet.
    view_changed: bool,
    last_emit: Instant,
    last_alive: Instant,
}

impl Default for MidiBridge {
    fn default() -> Self {
        Self::new()
    }
}

impl MidiBridge {
    pub fn new() -> Self {
        Self {
            input: None,
            profile: None,
            path: None,
            score: MidiMap::new(),
            router: Router::new(MidiMap::new()),
            learn: Learn::new(),
            dirty: false,
            notes: Vec::new(),
            message: None,
            seq: 0,
            pending: Vec::new(),
            view_changed: false,
            last_emit: Instant::now(),
            last_alive: Instant::now(),
        }
    }

    pub fn is_open(&self) -> bool {
        self.input.is_some()
    }

    /// True while the learn mode waits for a control. The pump routes events here instead of into
    /// the session.
    pub fn is_learning(&self) -> bool {
        self.learn.is_armed()
    }

    // ---- the connection --------------------------------------------------------------------

    /// Every MIDI input, with the profile that belongs to it. Opens nothing, so it works even while
    /// another program holds the controller.
    pub fn ports() -> Result<Vec<MidiPortView>, String> {
        Ok(input::list_ports()?
            .into_iter()
            .map(|port| {
                let found = profile_for_port(&port.name);
                MidiPortView {
                    index: port.index,
                    profile: found.as_ref().map(|(_, p)| p.device.clone()),
                    bindings: found.as_ref().map(|(_, p)| p.map.len()).unwrap_or(0),
                    name: port.name,
                }
            })
            .collect())
    }

    /// Open a port by number or by part of its name, and load the profile belonging to it.
    pub fn open(&mut self, selector: &str) -> Result<MidiView, String> {
        // Give the old port back first: Windows hands a MIDI device out to one program at a time,
        // and that includes handing it to this program twice.
        self.input = None;
        let midi = input::open(selector)?;
        let port = midi.name().to_string();

        let (path, profile) = match profile_for_port(&port) {
            Some((path, profile)) => (path, profile),
            // No profile yet is the normal state before the first learn, not an error. An empty one
            // named after the port gives the learn mode somewhere to put what it learns.
            None => (profile_path(&port), Profile::new(&port)),
        };
        // Opening another device replaces the profile in memory. Say so when something learned was
        // never written - a silently dropped mapping is half an hour of pad pressing gone.
        let lost = if self.dirty {
            " Achtung: die ungespeicherten Bindungen des vorigen Profils sind damit weg."
        } else {
            ""
        };
        let bindings = profile.map.len();
        self.message = Some(if bindings == 0 {
            format!(
                "MIDI-Eingang \"{port}\" offen. Noch keine Bindungen - \"MIDI lernen\" einschalten \
                 und ein Bedienelement anklicken.{lost}"
            )
        } else {
            format!(
                "MIDI-Eingang \"{port}\" offen, Profil \"{}\" mit {bindings} Bindungen.{lost}",
                profile.device
            )
        });
        self.input = Some(midi);
        self.profile = Some(profile);
        self.path = Some(path);
        self.dirty = false;
        self.rebuild();
        self.last_alive = Instant::now();
        Ok(self.view())
    }

    /// Give the port back. Whatever was learned and not saved stays in memory, so closing by
    /// accident does not cost the work.
    pub fn close(&mut self) -> MidiView {
        let port = self.input.take().map(|midi| midi.name().to_string());
        self.learn.cancel();
        self.message = Some(match port {
            Some(name) => format!("MIDI-Eingang \"{name}\" geschlossen."),
            None => "Es war kein MIDI-Eingang offen.".to_string(),
        });
        self.view_changed = true;
        self.view()
    }

    /// Whether the open port is still there. Windows does not announce an unplugged device, so the
    /// port list is asked - rarely, and only while something is open.
    ///
    /// Returns the German sentence when the device went away, so the caller can show it.
    pub fn check_alive(&mut self) -> Option<String> {
        if self.input.is_none() || self.last_alive.elapsed() < ALIVE_INTERVAL {
            return None;
        }
        self.last_alive = Instant::now();
        let name = self.input.as_ref().map(|midi| midi.name().to_string())?;
        // A port list that cannot be read at all is not evidence that the device is gone; it is
        // evidence that the MIDI system is busy. Keep the connection and try again in two seconds.
        let ports = input::list_ports().ok()?;
        if ports.iter().any(|port| port.name == name) {
            return None;
        }
        self.input = None;
        self.learn.cancel();
        self.view_changed = true;
        let message = format!(
            "Der MIDI-Eingang \"{name}\" ist verschwunden - Kabel ab, oder ein anderes Programm hat \
             ihn genommen. Die Bindungen bleiben; nach dem Anstecken wieder verbinden."
        );
        self.message = Some(message.clone());
        Some(message)
    }

    // ---- the mapping -----------------------------------------------------------------------

    /// The score's `midi:` block, laid over the profile. Called when a score is loaded.
    ///
    /// The overlay is exactly what the CLI does (`looper_engine::midi::session_map`): the profile
    /// says what the box does, the score adds what this one piece needs, and every control the
    /// score takes over comes back as a German line rather than silently changing meaning.
    pub fn set_score(&mut self, score: Option<&CompiledScore>) -> Result<Vec<String>, Vec<String>> {
        self.score = match score {
            Some(score) => score_map(score)?,
            None => MidiMap::new(),
        };
        self.rebuild();
        Ok(self.notes.clone())
    }

    /// Profile plus score into the router, and work out what the score took over.
    fn rebuild(&mut self) {
        let mut map = self
            .profile
            .as_ref()
            .map(|p| p.map.clone())
            .unwrap_or_default();
        self.notes = map.overlay(&self.score);
        self.router.set_map(map);
        self.view_changed = true;
    }

    /// Make every knob catch its parameter again. After a preset, a score, or a fresh engine the
    /// values moved without the knobs moving - without this, pickup would protect the first touch
    /// after startup and nothing afterwards.
    pub fn rearm(&mut self) {
        self.router.rearm();
    }

    // ---- learn -----------------------------------------------------------------------------

    /// The next control that arrives becomes this address.
    ///
    /// The address is the one the clicked element carries; it is parsed here, so a typo in the user
    /// interface is a German sentence rather than a binding on nothing.
    pub fn arm(&mut self, address: &str) -> Result<MidiView, String> {
        let target = Target::parse(address)?;
        let mut message = self.learn.arm(target);
        if self.input.is_none() {
            message.push_str(
                " Es ist allerdings kein MIDI-Eingang offen - erst verbinden, dann kommt hier etwas an.",
            );
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

    /// One event while the learn mode is armed. Returns the report to show, or `None` when the
    /// event was not one that can be learned (the note off of a pad).
    pub fn feed_learn(&mut self, event: &MidiEvent) -> Option<MidiEventReport> {
        let previous = self.router.map().get(event.id()).cloned();
        let learned = self.learn.feed(event, previous.as_ref())?;
        let port = self.port_name();
        let profile = self.profile.get_or_insert_with(|| Profile::new(port));
        profile.map.insert(learned.id, learned.binding.clone());
        if self.path.is_none() {
            self.path = Some(profile.path());
        }
        self.dirty = true;
        self.rebuild();
        // A freshly bound knob must not act on the position it happens to sit at.
        self.router.rearm();
        self.message = Some(learned.message.clone());
        let report = self.make_report(
            event,
            MidiOutcome::Learned,
            Some(&learned.binding.target),
            Some(learned.message),
        );
        // The monitor shows it like every other event - "was that pad the one I pressed?" is
        // exactly the question the monitor exists for, and it is asked hardest while learning.
        self.push(report.clone());
        Some(report)
    }

    /// Take a control's job away. The id is the one the overview shows, `ch1.note36`.
    pub fn unbind(&mut self, id: &str) -> Result<MidiView, String> {
        let key = MidiId::parse(id)?;
        let profile = self
            .profile
            .as_mut()
            .ok_or("Es ist kein Profil geladen - erst einen MIDI-Eingang verbinden.")?;
        let Some(removed) = profile.map.remove(key) else {
            // The score's own bindings are not in the profile and cannot be taken away from here.
            let from_score = self.score.get(key).is_some();
            return Err(if from_score {
                format!(
                    "{id} kommt aus der Partitur, nicht aus dem Profil. Diese Bindung steht im \
                     'midi:'-Block der Partitur und geht mit ihr wieder weg."
                )
            } else {
                format!("{id} ist gar nicht belegt.")
            });
        };
        self.dirty = true;
        self.rebuild();
        self.message = Some(format!(
            "{id} ist wieder frei (war \"{}\").",
            removed.target.label()
        ));
        Ok(self.view())
    }

    /// Write the profile. Returns the path, so the answer can say where it went.
    pub fn save(&mut self) -> Result<MidiSaved, String> {
        let profile = self
            .profile
            .as_ref()
            .ok_or("Es ist kein Profil zum Speichern da - erst einen MIDI-Eingang verbinden.")?;
        let path = self
            .path
            .clone()
            .unwrap_or_else(|| profile.path());
        profile.save(&path)?;
        self.dirty = false;
        let message = format!(
            "Profil \"{}\" mit {} Bindungen gespeichert: {}",
            profile.device,
            profile.map.len(),
            path.display()
        );
        self.message = Some(message.clone());
        self.view_changed = true;
        Ok(MidiSaved {
            path: path.display().to_string(),
            message,
            view: self.view(),
        })
    }

    // ---- the stream ------------------------------------------------------------------------

    /// Everything the driver has queued since the last call. Nothing is decoded here - that already
    /// happened in the callback - so this is a memcpy out of a lock-free ring buffer.
    pub fn drain(&mut self, out: &mut Vec<MidiEvent>) {
        let Some(midi) = self.input.as_mut() else {
            return;
        };
        while let Some(timed) = midi.try_recv() {
            out.push(timed.event);
        }
    }

    /// One event against the mapping. The boundary of `docs/architektur.md` section 10 is inside
    /// [`Router::resolve`], from the same constant a mouse click runs into.
    pub fn resolve(&mut self, event: &MidiEvent, ctx: &Context<'_>) -> Resolution {
        self.router.resolve(event, ctx)
    }

    /// What is bound to this control, for a report that wants to name it.
    pub fn target_of(&self, event: &MidiEvent) -> Option<Target> {
        self.router
            .map()
            .get(event.id())
            .map(|binding| binding.target.clone())
    }

    /// Put one report in the queue to the web view.
    ///
    /// A knob that is being swept coalesces: the newest value of a control replaces the one before
    /// it inside the same window, because a monitor showing "CC 3 = 64" one fortieth of a second
    /// late is right and one showing all sixty steps in between is unreadable.
    pub fn report(
        &mut self,
        event: &MidiEvent,
        outcome: MidiOutcome,
        target: Option<&Target>,
        note: Option<String>,
    ) {
        let report = self.make_report(event, outcome, target, note);
        self.push(report);
    }

    fn push(&mut self, report: MidiEventReport) {
        if report.continuous
            && let Some(slot) = self
                .pending
                .iter_mut()
                .rev()
                .find(|old| old.id == report.id && old.continuous)
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
        event: &MidiEvent,
        outcome: MidiOutcome,
        target: Option<&Target>,
        note: Option<String>,
    ) -> MidiEventReport {
        self.seq += 1;
        let id = event.id();
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
        }
    }

    /// The batch to emit, if it is time. See the module comment for the two rates.
    pub fn take_feed(&mut self) -> Option<MidiFeed> {
        if self.pending.is_empty() && !self.view_changed {
            return None;
        }
        // Anything discrete - a pad, a refusal, a learned binding - goes out at once. Only a knob's
        // stream waits for the window.
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

    /// Say something in the next feed - a refusal, a note about the connection.
    pub fn say(&mut self, message: impl Into<String>) {
        self.message = Some(message.into());
        self.view_changed = true;
    }

    // ---- the overview ----------------------------------------------------------------------

    pub fn view(&self) -> MidiView {
        let bindings = self
            .router
            .map()
            .sorted()
            .into_iter()
            .map(|(id, binding)| binding_view(id, binding, self.score.get(id) == Some(binding)))
            .collect();
        MidiView {
            connected: self.input.is_some(),
            port: self.input.as_ref().map(|midi| midi.name().to_string()),
            device: self.profile.as_ref().map(|p| p.device.clone()),
            path: self.path.as_ref().map(|p| p.display().to_string()),
            dirty: self.dirty,
            learning: self.learn.pending().map(|t| t.to_string()),
            learning_label: self.learn.pending().map(|t| t.label()),
            bindings,
            notes: self.notes.clone(),
            message: self.message.clone(),
            dropped: self.input.as_ref().map(|midi| midi.dropped()).unwrap_or(0),
        }
    }

    fn port_name(&self) -> String {
        self.input
            .as_ref()
            .map(|midi| midi.name().to_string())
            .unwrap_or_else(|| "Controller".to_string())
    }
}

fn binding_view(id: MidiId, binding: &Binding, from_score: bool) -> MidiBindingView {
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
        label: binding.target.label(),
        describe: binding.describe(),
        from_score,
    }
}

/// German word for the kind of message, for the monitor's first column.
fn what(event: &MidiEvent) -> &'static str {
    match event {
        MidiEvent::NoteOn { .. } => "Note an",
        MidiEvent::NoteOff { .. } => "Note aus",
        MidiEvent::ControlChange { .. } => "Regler",
        MidiEvent::PitchBend { .. } => "Pitchbend",
    }
}

/// The number the monitor prints: velocity, controller value, or the bend around zero.
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
mod tests {
    use super::*;
    use looper_engine::midi::{Control, MidiId};
    use std::sync::Mutex;

    /// `HENRYS_LOOPER_CONFIG_DIR` is process-wide, so the tests that write a profile take turns.
    static CONFIG_DIR: Mutex<()> = Mutex::new(());

    fn press(note: u8) -> MidiEvent {
        MidiEvent::NoteOn {
            channel: 1,
            note,
            velocity: 100,
        }
    }

    fn knob(number: u8, value: u8) -> MidiEvent {
        MidiEvent::ControlChange {
            channel: 1,
            controller: number,
            value,
        }
    }

    /// **The heart of the whole learn mode**: the address of the clicked element goes in, the next
    /// control that arrives comes out bound to it - and lands in the profile, not just in the
    /// router's memory.
    #[test]
    fn learn_binds_the_next_control_to_the_address_of_the_clicked_element() {
        let mut bridge = MidiBridge::new();
        bridge.arm("track.1.record").expect("gueltige Adresse");
        assert!(bridge.is_learning());

        // The note off of the same pad must not learn a second time - it is the other half of one
        // press, and learning it would report "Pad 36 ist jetzt ..." twice.
        let report = bridge.feed_learn(&press(36)).expect("die Taste wird gelernt");
        assert_eq!(report.outcome, MidiOutcome::Learned);
        assert_eq!(report.short, "Pad 36");
        assert_eq!(report.target.as_deref(), Some("track.1.record"));
        assert!(!bridge.is_learning(), "der Modus entwaffnet sich selbst");
        assert!(
            bridge
                .feed_learn(&MidiEvent::NoteOff {
                    channel: 1,
                    note: 36,
                    velocity: 0
                })
                .is_none()
        );

        // The monitor sees it too: "was that the pad I pressed?" is asked hardest while learning.
        let feed = bridge.take_feed().expect("das Gelernte geht sofort raus");
        assert_eq!(feed.events.len(), 1);
        assert_eq!(feed.events[0].outcome, MidiOutcome::Learned);
        assert!(
            feed.view.is_some(),
            "die neue Belegung faehrt mit, damit die Oberflaeche nicht nachfragen muss"
        );

        let view = bridge.view();
        assert!(view.dirty, "gelernt und noch nicht gespeichert");
        assert_eq!(view.bindings.len(), 1);
        assert_eq!(view.bindings[0].address, "track.1.record");
        assert_eq!(view.bindings[0].short, "Pad 36");
        assert!(!view.bindings[0].from_score);
    }

    /// A typo in the user interface has to be a German sentence, not a binding on nothing.
    #[test]
    fn an_unknown_address_is_refused_with_the_way_to_the_list() {
        let mut bridge = MidiBridge::new();
        let err = bridge.arm("track.1.recrd").expect_err("Tippfehler");
        assert!(err.contains("midi targets"), "{err}");
        assert!(!bridge.is_learning());
    }

    /// Unbinding takes the entry out of the profile, and the file written afterwards is without it.
    #[test]
    fn unbinding_removes_the_entry_from_the_profile_and_from_the_file() {
        let _guard = CONFIG_DIR.lock().unwrap_or_else(|e| e.into_inner());
        let dir = std::env::temp_dir().join(format!("looper-midi-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        unsafe { std::env::set_var("HENRYS_LOOPER_CONFIG_DIR", &dir) };

        let mut bridge = MidiBridge::new();
        bridge.arm("track.1.record").expect("Adresse");
        bridge.feed_learn(&press(36)).expect("gelernt");
        bridge.arm("track.2.record").expect("Adresse");
        bridge.feed_learn(&press(37)).expect("gelernt");
        let saved = bridge.save().expect("speichern");
        let text = std::fs::read_to_string(&saved.path).expect("Datei");
        assert!(text.contains("ch1.note36"), "{text}");
        assert!(text.contains("ch1.note37"), "{text}");
        assert!(!bridge.view().dirty);

        bridge.unbind("ch1.note36").expect("loesen");
        assert!(bridge.view().dirty, "geloest und noch nicht gespeichert");
        assert_eq!(bridge.view().bindings.len(), 1);
        bridge.save().expect("speichern");
        let text = std::fs::read_to_string(&saved.path).expect("Datei");
        assert!(!text.contains("ch1.note36"), "{text}");
        assert!(text.contains("ch1.note37"), "{text}");

        // A control that is not bound says so rather than pretending to have done something.
        let err = bridge.unbind("ch1.note99").expect_err("nicht belegt");
        assert!(err.contains("nicht belegt"), "{err}");

        unsafe { std::env::remove_var("HENRYS_LOOPER_CONFIG_DIR") };
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The score's `midi:` block supplements the profile when the score is loaded - and every pad
    /// the score takes over is reported as a German sentence, because a pad that silently means
    /// something else in one piece is how a live set goes wrong.
    #[test]
    fn a_scores_bindings_supplement_the_profile_and_a_takeover_is_reported() {
        let yaml = "title: T\nbpm: 100\ntracks:\n  stimme: {input: 1}\nmidi:\n  \
                    track.stimme.record: {note: 36}\n  transport.next: {note: 45}\nsections:\n  \
                    - id: eins\n    bars: 4\n    tracks:\n      stimme: record\n";
        let score = looper_engine::score::compile_score(yaml).expect("Partitur");

        let mut bridge = MidiBridge::new();
        bridge.arm("track.1.record").expect("Adresse");
        bridge.feed_learn(&press(36)).expect("gelernt");

        let notes = bridge.set_score(Some(&score)).expect("Bindungen der Partitur");
        let view = bridge.view();
        // Two ids: the pad the score took over, and the one it added.
        assert_eq!(view.bindings.len(), 2, "{:?}", view.bindings);
        let taken = view
            .bindings
            .iter()
            .find(|b| b.id == "ch1.note36")
            .expect("Pad 36");
        assert_eq!(taken.address, "track.stimme.record");
        assert!(taken.from_score);
        let added = view
            .bindings
            .iter()
            .find(|b| b.id == "ch1.note45")
            .expect("Pad 45");
        assert_eq!(added.address, "transport.next");

        assert_eq!(notes.len(), 1, "{notes:?}");
        assert!(notes[0].contains("Partitur belegt"), "{}", notes[0]);

        // A score binding is not the profile's to remove.
        let err = bridge.unbind("ch1.note45").expect_err("gehoert der Partitur");
        assert!(err.contains("Partitur"), "{err}");

        // Unloading the score gives the pad back to the profile.
        bridge.set_score(None).expect("keine Partitur");
        let view = bridge.view();
        assert_eq!(view.bindings.len(), 1);
        assert_eq!(view.bindings[0].address, "track.1.record");
    }

    /// A knob's stream is coalesced and waits for its window; a pad goes out at once. Without the
    /// first the web view would get a hundred messages a second, without the second the learn mode
    /// would feel broken.
    #[test]
    fn a_knob_sweep_coalesces_and_a_pad_is_emitted_at_once() {
        let mut bridge = MidiBridge::new();
        // Drain whatever the constructor's `view_changed` would emit.
        bridge.take_feed();

        for value in 0..40u8 {
            bridge.report(&knob(3, value), MidiOutcome::Unbound, None, None);
        }
        let pending = bridge.pending.len();
        assert_eq!(pending, 1, "vierzig Schritte eines Reglers sind ein Eintrag");
        assert!(
            bridge.take_feed().is_none(),
            "ein Regler wartet auf sein Fenster"
        );

        bridge.report(&press(36), MidiOutcome::Unbound, None, None);
        let feed = bridge.take_feed().expect("ein Pad geht sofort raus");
        assert_eq!(feed.events.len(), 2);
        assert_eq!(feed.events[0].value, 39, "der neueste Wert des Reglers");
        assert_eq!(feed.events[1].what, "Note an");
    }

    /// The badge on a bound control is what the musician reads off the screen - and it has to be
    /// the same wording for a pad and for a knob.
    #[test]
    fn every_kind_of_control_has_a_short_name_for_the_badge() {
        assert_eq!(MidiId::note(1, 36).short(), "Pad 36");
        assert_eq!(MidiId::cc(1, 3).short(), "CC 3");
        assert_eq!(MidiId::bend(1).short(), "Bend");
        // The full id stays the file's key; the badge is only for the screen.
        assert_eq!(MidiId::note(10, 36).to_string(), "ch10.note36");
    }

    /// The learn mode allows a pad on a knob's target - the velocity becomes the value - and says
    /// so. Refusing would be tidier and would also make a learned file behave differently from a
    /// hand-written one.
    #[test]
    fn a_pad_on_a_value_target_is_allowed_and_explained() {
        let mut bridge = MidiBridge::new();
        bridge.arm("track.1.fx.reverb.mix").expect("Adresse");
        let report = bridge.feed_learn(&press(40)).expect("gelernt");
        let note = report.note.expect("Satz");
        assert!(note.contains("Anschlagstaerke"), "{note}");
        let target = Target::parse("track.1.fx.reverb.mix").expect("Adresse");
        assert!(matches!(target.control(), Control::Range(_)));
    }
}
