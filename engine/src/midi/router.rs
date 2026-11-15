//! Incoming event + mapping -> one resolved intent.
//!
//! This is the half of "MIDI" that never sees a device: bytes have already become
//! [`MidiEvent`](super::event::MidiEvent)s, and what comes out is a [`MidiAction`] with a resolved
//! track index and a finished value. Everything about it is provable offline, which is the point -
//! the only thing that needs an MPD218 on the desk is the ten lines in [`super::input`].
//!
//! Four things happen here, and each is a decision:
//!
//! 1. **A track reference becomes an index.** `track.stimme` only means something against a track
//!    list; a name that is not in it is refused with a sentence, never guessed at.
//! 2. **A button becomes an action, or does not.** A note off on a trigger binding is not an event
//!    that got lost, it is an event that correctly does nothing - and saying which of the two it
//!    was is the difference between a debuggable controller and a mysterious one. Hence
//!    [`Resolution::Absorbed`] with a reason.
//! 3. **A knob becomes a value**, through the target's range and, unless switched off, through
//!    pickup (see [`super::binding`]).
//! 4. **The runner's boundary is enforced.** While a score plays, the transport belongs to the
//!    runner. A pad that says "Aufnahme" is refused with exactly the sentence a mouse click gets,
//!    from exactly the same constant - see `docs/architektur.md` section 10.

use crate::engine::bus::{Bus, BusSend, TrackSource};
use crate::engine::command::Command;
use crate::engine::fx::{FxParam, FxPreset, FxSlot};
use crate::engine::insert::InsertTarget;
use crate::engine::runner::TRANSPORT_BELONGS_TO_SCORE;
use crate::engine::schedule::Quantize;
use crate::engine::track::TrackLatency;

use super::binding::{Binding, ButtonMode, MidiMap, Takeover};
use super::event::{MidiEvent, MidiId};
use super::target::{Control, InsertOwner, Target, TrackRef};

/// How close a knob has to come to the current value before it takes over, in controller steps.
///
/// One step is not enough: a potentiometer sitting between two values dithers, and a musician
/// sweeping past the pickup point at speed can skip a step outright. Two steps of 127 is 1.6 % of
/// the range - below what anybody hears as a jump, above what a pot's noise produces.
const PICKUP_TOLERANCE: f32 = 2.0;

// ---------------------------------------------------------------------------------------------
// What comes out
// ---------------------------------------------------------------------------------------------

/// One resolved intent: a track index instead of a name, a value instead of a controller step.
///
/// Deliberately *not* [`Command`]: half of these need a musical timestamp, and working out which
/// sample a take starts on happens in exactly one place in this program
/// (`engine::schedule`) - see `docs/architektur.md` section 3. What a MIDI event produces is the
/// same thing a mouse click produces: an intent, which the host then schedules.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum MidiAction {
    // ---- the score's transport ---------------------------------------------------------------
    ScoreStart,
    ScoreNext,
    ScoreStopAll,
    ScoreGoto(usize),

    // ---- global ------------------------------------------------------------------------------
    /// Start the clock of free looping if it stands, stop it if it runs.
    Transport,
    Click(bool),
    ClearAll,
    Tempo(f64),
    SetQuantize(Quantize),

    // ---- per track ---------------------------------------------------------------------------
    Record { track: usize },
    Overdub { track: usize },
    Play { track: usize },
    StopTrack { track: usize },
    ClearTrack { track: usize },
    Monitor { track: usize, on: bool },
    Pan { track: usize, pan: f32 },
    /// The channel fader, linear.
    Gain { track: usize, gain: f32 },
    /// Channel mute and solo - listening only.
    Mute { track: usize, on: bool },
    Solo { track: usize, on: bool },
    /// One source of one track on or off one bus. The router carries the whole resulting send, not
    /// just the bit, because a `BusSend` is what the engine takes - and the bit it changed was
    /// computed against the state the engine reported, so no private copy can drift.
    Send {
        track: usize,
        source: TrackSource,
        send: BusSend,
    },
    /// Volume of one output bus, linear.
    BusGain { bus: Bus, gain: f32 },
    /// Click volume and the volume of every monitored input, linear.
    ClickGain(f32),
    MonitorGain(f32),
    /// The manual half of the latency compensation, in frames. The measured half belongs to
    /// `calibrate` and is not touched from here.
    LatencyTrim { track: usize, trim: i32 },

    // ---- per layer ---------------------------------------------------------------------------
    LayerMute { track: usize, layer: usize, muted: bool },
    LayerRemove { track: usize, layer: usize },
    LayerGain { track: usize, layer: usize, gain: f32 },

    // ---- the effect chain --------------------------------------------------------------------
    FxBypass { track: usize, on: bool },
    FxEnable { track: usize, slot: FxSlot, on: bool },
    FxPreset { track: usize, preset: FxPreset },
    FxParam { track: usize, param: FxParam },

    // ---- plugin inserts ----------------------------------------------------------------------
    /// The host's bypass of one insert slot. Not an engine command: the slot belongs to the
    /// plugin manager of the app, which flips an atomic the engine reads.
    InsertBypass {
        target: InsertTarget,
        slot: usize,
        on: bool,
    },
    /// One plugin parameter, normalised 0..1, by the plugin's own id.
    InsertParam {
        target: InsertTarget,
        slot: usize,
        param: u32,
        value: f32,
    },
}

impl MidiAction {
    /// Whether this puts a take somewhere on the timeline. The same question
    /// [`Target::moves_transport`] answers, kept here so a caller that only has the action can ask
    /// it too.
    pub fn moves_transport(&self) -> bool {
        matches!(
            self,
            MidiAction::Record { .. }
                | MidiAction::Overdub { .. }
                | MidiAction::Play { .. }
                | MidiAction::StopTrack { .. }
                | MidiAction::ClearTrack { .. }
                | MidiAction::ClearAll
                | MidiAction::Transport
                | MidiAction::Tempo(_)
                | MidiAction::SetQuantize(_)
        )
    }

    /// The engine command this action *is*, where there is one.
    ///
    /// `None` for three groups, each for its own reason:
    ///
    /// * the score's transport - that goes to the runner, not to the engine;
    /// * anything that needs a musical timestamp (record, overdub, play, stop, clear) - only
    ///   `engine::schedule` decides which sample that is;
    /// * `Tempo` and `LatencyTrim`, which are commands that need a second number the caller has and
    ///   this module does not: the layer length a tempo implies, and the *measured* half of the
    ///   track's compensation.
    pub fn command(&self) -> Option<Command> {
        Some(match *self {
            MidiAction::Click(on) => Command::SetClick { on },
            MidiAction::Monitor { track, on } => Command::SetMonitor { track, on },
            MidiAction::Pan { track, pan } => Command::SetPan { track, pan },
            MidiAction::Gain { track, gain } => Command::SetTrackGain { track, gain },
            MidiAction::Mute { track, on } => Command::SetTrackMute { track, on },
            MidiAction::Solo { track, on } => Command::SetTrackSolo { track, on },
            MidiAction::ClickGain(gain) => Command::SetClickGain { gain },
            MidiAction::MonitorGain(gain) => Command::SetMonitorGain { gain },
            MidiAction::Send {
                track,
                source,
                send,
            } => Command::SetTrackSend {
                track,
                source,
                send,
            },
            MidiAction::BusGain { bus, gain } => Command::SetBusGain { bus, gain },
            MidiAction::LayerMute {
                track,
                layer,
                muted,
            } => Command::SetLayerMute {
                track,
                layer,
                muted,
            },
            MidiAction::LayerRemove { track, layer } => Command::RemoveLayer { track, layer },
            MidiAction::LayerGain { track, layer, gain } => {
                Command::SetLayerGain { track, layer, gain }
            }
            MidiAction::FxBypass { track, on } => Command::SetFxBypass { track, on },
            MidiAction::FxEnable { track, slot, on } => Command::SetFxEnabled { track, slot, on },
            MidiAction::FxPreset { track, preset } => Command::LoadFxPreset { track, preset },
            MidiAction::FxParam { track, param } => Command::SetFxParam { track, param },
            _ => return None,
        })
    }

    /// The whole latency compensation, once the caller has supplied the measured half this action
    /// deliberately leaves alone.
    pub fn latency_with(&self, measured: Option<u32>) -> Option<TrackLatency> {
        match *self {
            MidiAction::LatencyTrim { trim, .. } => Some(TrackLatency { measured, trim }),
            _ => None,
        }
    }
}

/// What one incoming event did.
#[derive(Clone, Debug, PartialEq)]
pub enum Resolution {
    /// Nothing is bound to this control.
    Unbound,
    /// Bound, and correctly did nothing - the note off of a trigger, a knob that has not caught its
    /// parameter yet. The string says which, in English.
    Absorbed(String),
    Action(MidiAction),
    /// Bound, but cannot be carried out right now. English sentence, ready to show (except
    /// `TRANSPORT_BELONGS_TO_SCORE`, which is the runner's own sentence).
    Refused(String),
}

impl Resolution {
    pub fn action(&self) -> Option<MidiAction> {
        match self {
            Resolution::Action(action) => Some(*action),
            _ => None,
        }
    }
}

// ---------------------------------------------------------------------------------------------
// What the router needs to know about the session
// ---------------------------------------------------------------------------------------------

/// The track list an address is resolved against.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TrackLayout {
    names: Vec<String>,
}

impl TrackLayout {
    pub fn new<I, S>(names: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        Self {
            names: names.into_iter().map(Into::into).collect(),
        }
    }

    pub fn len(&self) -> usize {
        self.names.len()
    }

    pub fn is_empty(&self) -> bool {
        self.names.is_empty()
    }

    pub fn names(&self) -> &[String] {
        &self.names
    }

    /// Address -> index, or an English sentence saying why not.
    pub fn resolve(&self, reference: &TrackRef) -> Result<usize, String> {
        // No tracks at all means no engine: say that, not "the session has no tracks".
        if self.names.is_empty() {
            return Err("The engine is not running - start it first.".to_string());
        }
        match reference {
            TrackRef::Index(index) if *index < self.names.len() => Ok(*index),
            TrackRef::Index(index) => Err(format!(
                "The binding means track {}, the session has {}.",
                index + 1,
                match self.names.len() {
                    0 => "no tracks".to_string(),
                    1 => "only track 1".to_string(),
                    n => format!("tracks 1 to {n}"),
                }
            )),
            TrackRef::Name(name) => self
                .names
                .iter()
                .position(|known| known == name)
                .ok_or_else(|| {
                    format!(
                        "The binding means the track \"{name}\"; this session has {}.",
                        if self.names.is_empty() {
                            "no tracks".to_string()
                        } else {
                            self.names.join(", ")
                        }
                    )
                }),
        }
    }
}

/// Where the current value of a parameter comes from.
///
/// Pickup cannot work without it - "has the knob passed the value" is not answerable without the
/// value - and neither can a toggle that is supposed to agree with what the engine actually does.
/// The default implementation knows nothing, which is a legitimate state (before the first status
/// snapshot arrives): a knob then takes over immediately and a toggle falls back to remembering its
/// own state.
pub trait ParamState {
    /// Current value of a continuous parameter, in the target's own units.
    fn value(&self, _target: &Target) -> Option<f32> {
        None
    }

    /// Current state of a switch.
    fn switch(&self, _target: &Target) -> Option<bool> {
        None
    }

    /// The whole bus assignment of one source of one track.
    ///
    /// A send switch changes **one bit of a set**, so the set has to be known before it is written
    /// back. Without it, "Main aus" on a track that was on both buses would silently take it off
    /// the headphones as well. `None` (before the first status snapshot) falls back to "heard
    /// everywhere", which is the state a fresh track's loop is in.
    fn send(&self, _track: usize, _source: TrackSource) -> Option<BusSend> {
        None
    }
}

/// Knows nothing. Every knob takes over at once, every toggle uses its own memory.
impl ParamState for () {}

/// Everything about the session an event is resolved against.
pub struct Context<'a> {
    pub layout: &'a TrackLayout,
    pub state: &'a dyn ParamState,
    /// True while a score is in its count-in or running. See [`TRANSPORT_BELONGS_TO_SCORE`].
    pub score_is_playing: bool,
}

impl<'a> Context<'a> {
    /// A context with no state and no score - what the tests and the CLI monitor use.
    pub fn bare(layout: &'a TrackLayout) -> Self {
        Self {
            layout,
            state: &(),
            score_is_playing: false,
        }
    }
}

// ---------------------------------------------------------------------------------------------
// The router
// ---------------------------------------------------------------------------------------------

/// Per-control memory: the edge detector, the toggle's fallback state, and the pickup.
#[derive(Clone, Copy, Debug, Default)]
struct ControlState {
    /// Was this control in its upper half at the last event?
    high: bool,
    /// The toggle's own idea of the state, used when the engine cannot be asked.
    on: bool,
    /// Has this knob caught its parameter?
    caught: bool,
    /// Where the knob was at the last event, in controller steps.
    last: Option<f32>,
}

/// Resolves events against a mapping, and remembers the little that has to be remembered.
pub struct Router {
    map: MidiMap,
    controls: Vec<(MidiId, ControlState)>,
}

impl Router {
    pub fn new(map: MidiMap) -> Self {
        Self {
            map,
            controls: Vec::new(),
        }
    }

    pub fn map(&self) -> &MidiMap {
        &self.map
    }

    /// Replace the mapping. Every knob has to catch its parameter again - the new mapping may point
    /// the same knob at something else entirely.
    pub fn set_map(&mut self, map: MidiMap) {
        self.map = map;
        self.controls.clear();
    }

    /// Make every knob catch its parameter again.
    ///
    /// Call this whenever the values moved without the knobs moving: a preset was loaded, a score
    /// was loaded, the engine was restarted. Without it, pickup protects the first touch after
    /// startup and nothing afterwards.
    pub fn rearm(&mut self) {
        for (_, state) in self.controls.iter_mut() {
            state.caught = false;
            state.last = None;
        }
    }

    fn state_of(&mut self, id: MidiId) -> &mut ControlState {
        if let Some(position) = self.controls.iter().position(|(key, _)| *key == id) {
            return &mut self.controls[position].1;
        }
        self.controls.push((id, ControlState::default()));
        let last = self.controls.len() - 1;
        &mut self.controls[last].1
    }

    /// One incoming event.
    pub fn resolve(&mut self, event: &MidiEvent, ctx: &Context<'_>) -> Resolution {
        let id = event.id();
        let Some(binding) = self.map.get(id).cloned() else {
            return Resolution::Unbound;
        };

        // The runner's boundary, checked before anything else is worked out: while a score plays it
        // owns the transport, and it makes no difference at all whether the press came from a pad
        // or from the mouse.
        if ctx.score_is_playing && binding.target.moves_transport() {
            return Resolution::Refused(TRANSPORT_BELONGS_TO_SCORE.to_string());
        }

        let track = match binding.target.track() {
            Some(reference) => match ctx.layout.resolve(reference) {
                Ok(index) => index,
                Err(message) => return Resolution::Refused(message),
            },
            None => 0,
        };

        match binding.target.control() {
            Control::Trigger => self.trigger(event, &binding, track),
            Control::Switch => self.switch(event, &binding, track, ctx),
            Control::Range(_) => self.knob(event, &binding, track, ctx),
        }
    }

    // ---- the three kinds ---------------------------------------------------------------------

    fn trigger(&mut self, event: &MidiEvent, binding: &Binding, track: usize) -> Resolution {
        let (rising, falling) = self.edges(event);
        let fire = match binding.button {
            ButtonMode::Release => falling,
            // A trigger cannot be a toggle - `Binding::check` refuses that combination when a file
            // is read - so anything else acts on the press.
            _ => rising,
        };
        if !fire {
            return Resolution::Absorbed(format!(
                "\"{}\" fires on {}.",
                binding.target,
                match binding.button {
                    ButtonMode::Release => "release",
                    _ => "press",
                }
            ));
        }
        Resolution::Action(match &binding.target {
            Target::ScoreStart => MidiAction::ScoreStart,
            Target::ScoreNext => MidiAction::ScoreNext,
            Target::ScoreStopAll => MidiAction::ScoreStopAll,
            Target::ScoreGoto(section) => MidiAction::ScoreGoto(*section),
            Target::ClearAll => MidiAction::ClearAll,
            Target::Transport => MidiAction::Transport,
            Target::Quantize(quantize) => MidiAction::SetQuantize(*quantize),
            Target::Record(_) => MidiAction::Record { track },
            Target::Overdub(_) => MidiAction::Overdub { track },
            Target::Play(_) => MidiAction::Play { track },
            Target::StopTrack(_) => MidiAction::StopTrack { track },
            Target::ClearTrack(_) => MidiAction::ClearTrack { track },
            Target::LayerRemove(_, layer) => MidiAction::LayerRemove {
                track,
                layer: *layer,
            },
            Target::FxPreset(_, preset) => MidiAction::FxPreset {
                track,
                preset: *preset,
            },
            Target::FxDelayNote(_, note) => MidiAction::FxParam {
                track,
                param: FxParam::DelayNote(*note),
            },
            Target::FxBandKind(_, band, kind) => MidiAction::FxParam {
                track,
                param: FxParam::BandKind {
                    band: *band,
                    kind: *kind,
                },
            },
            // Everything else is not a trigger; `Target::control` decided that above.
            other => {
                return Resolution::Absorbed(format!("\"{other}\" does not fire."));
            }
        })
    }

    fn switch(
        &mut self,
        event: &MidiEvent,
        binding: &Binding,
        track: usize,
        ctx: &Context<'_>,
    ) -> Resolution {
        let (rising, falling) = self.edges(event);
        let known = ctx.state.switch(&binding.target);
        let id = event.id();
        let on = match binding.button {
            ButtonMode::Momentary => {
                if !rising && !falling {
                    return Resolution::Absorbed(format!(
                        "\"{}\" is on while the key is held.",
                        binding.target
                    ));
                }
                rising
            }
            ButtonMode::Press => {
                if !rising {
                    return Resolution::Absorbed(format!(
                        "\"{}\" switches on when pressed.",
                        binding.target
                    ));
                }
                true
            }
            ButtonMode::Release => {
                if !falling {
                    return Resolution::Absorbed(format!(
                        "\"{}\" switches off when released.",
                        binding.target
                    ));
                }
                false
            }
            ButtonMode::Toggle => {
                if !rising {
                    return Resolution::Absorbed(format!(
                        "\"{}\" toggles when pressed.",
                        binding.target
                    ));
                }
                // The engine's own state wins over the router's memory whenever it is available:
                // the mouse and the score change these too, and a toggle that flips its private
                // copy would be out of step from the first time anything else touched them.
                let current = known.unwrap_or_else(|| self.state_of(id).on);
                !current
            }
        };
        self.state_of(id).on = on;

        Resolution::Action(match &binding.target {
            Target::Click => MidiAction::Click(on),
            Target::Monitor(_) => MidiAction::Monitor { track, on },
            Target::Mute(_) => MidiAction::Mute { track, on },
            Target::Solo(_) => MidiAction::Solo { track, on },
            Target::Send(_, source, bus) => {
                // The switch is one bus of a two-bus set, so the set has to be read back before the
                // bit is flipped - otherwise switching Main off would also throw Monitor away.
                let current = ctx.state.send(track, *source).unwrap_or(BusSend::BOTH);
                MidiAction::Send {
                    track,
                    source: *source,
                    send: current.with(*bus, on),
                }
            }
            Target::LayerMute(_, layer) => MidiAction::LayerMute {
                track,
                layer: *layer,
                muted: on,
            },
            Target::FxBypass(_) => MidiAction::FxBypass { track, on },
            Target::FxEnabled(_, slot) => MidiAction::FxEnable {
                track,
                slot: *slot,
                on,
            },
            Target::InsertBypass(owner, slot) => MidiAction::InsertBypass {
                target: insert_target(owner, track),
                slot: *slot,
                on,
            },
            other => return Resolution::Absorbed(format!("\"{other}\" is not a switch.")),
        })
    }

    fn knob(
        &mut self,
        event: &MidiEvent,
        binding: &Binding,
        track: usize,
        ctx: &Context<'_>,
    ) -> Resolution {
        if matches!(event, MidiEvent::NoteOff { .. }) {
            return Resolution::Absorbed(format!(
                "\"{}\" is a value; releasing does not change it.",
                binding.target
            ));
        }
        let range = binding.range().expect("Wertebereich eines Reglers");
        let incoming = f32::from(event.value7());
        let id = event.id();

        if binding.takeover == Takeover::Pickup {
            // Where the parameter is, expressed as the controller position that would produce it.
            // Comparing in controller steps and not in parameter units is what makes one tolerance
            // work for a pan (-1..1) and for a frequency (20..20000) alike.
            let current = ctx.state.value(&binding.target).map(|v| range.cc_of(v));
            let state = self.state_of(id);
            if !state.caught {
                match current {
                    // Nothing known to protect - take over at once. Refusing here would leave a
                    // knob dead until the first status snapshot, which looks exactly like a broken
                    // cable.
                    None => state.caught = true,
                    Some(current) => {
                        let crossed = state.last.is_some_and(|last| {
                            (last - current).signum() != (incoming - current).signum()
                        });
                        if (incoming - current).abs() <= PICKUP_TOLERANCE || crossed {
                            state.caught = true;
                        } else {
                            state.last = Some(incoming);
                            let direction = if incoming > current { "down" } else { "up" };
                            return Resolution::Absorbed(format!(
                                "\"{}\" is at {:.0}, the knob at {incoming:.0} - turn it {direction} \
                                 until it reaches the value.",
                                binding.target, current
                            ));
                        }
                    }
                }
            }
            state.last = Some(incoming);
        }

        let value = range.value_of(event.value7());
        Resolution::Action(match &binding.target {
            Target::Pan(_) => MidiAction::Pan { track, pan: value },
            Target::Gain(_) => MidiAction::Gain { track, gain: value },
            Target::ClickGain => MidiAction::ClickGain(value),
            Target::MonitorGain => MidiAction::MonitorGain(value),
            Target::BusGain(bus) => MidiAction::BusGain { bus: *bus, gain: value },
            Target::LayerGain(_, layer) => MidiAction::LayerGain {
                track,
                layer: *layer,
                gain: value,
            },
            Target::Tempo => MidiAction::Tempo(f64::from(value)),
            Target::LatencyTrim(_) => MidiAction::LatencyTrim {
                track,
                trim: value.round() as i32,
            },
            Target::FxKnob(_, knob) => MidiAction::FxParam {
                track,
                param: knob.to_param(value),
            },
            Target::InsertParam(owner, slot, param) => MidiAction::InsertParam {
                target: insert_target(owner, track),
                slot: *slot,
                param: *param,
                value,
            },
            other => return Resolution::Absorbed(format!("\"{other}\" is not a value.")),
        })
    }

    /// Rising and falling edge of a control, from its own last position.
    ///
    /// A note on is a rising edge and a note off a falling one; a controller crosses 64. The edge
    /// and not the level is what a toggle needs, and it is what keeps a knob that happens to be
    /// bound to a pad's job from firing 60 times on the way up.
    fn edges(&mut self, event: &MidiEvent) -> (bool, bool) {
        let high = event.is_high();
        let state = self.state_of(event.id());
        let was = state.high;
        state.high = high;
        (high && !was, !high && was)
    }
}

/// The rack an insert address names, with the track already resolved by the caller.
fn insert_target(owner: &InsertOwner, track: usize) -> InsertTarget {
    match owner {
        InsertOwner::Track(_) => InsertTarget::Track(track),
        InsertOwner::Bus(bus) => InsertTarget::Bus(*bus),
    }
}

/// A small [`ParamState`] built from lists, for tests and for a caller that has the numbers lying
/// around anyway.
#[derive(Default)]
pub struct StaticState {
    values: Vec<(String, f32)>,
    switches: Vec<(String, bool)>,
    sends: Vec<(usize, TrackSource, BusSend)>,
}

impl StaticState {
    pub fn new() -> Self {
        Self::default()
    }

    #[must_use]
    pub fn with_send(mut self, track: usize, source: TrackSource, send: BusSend) -> Self {
        self.sends.push((track, source, send));
        self
    }

    #[must_use]
    pub fn with_value(mut self, target: &Target, value: f32) -> Self {
        self.values.push((target.to_string(), value));
        self
    }

    #[must_use]
    pub fn with_switch(mut self, target: &Target, on: bool) -> Self {
        self.switches.push((target.to_string(), on));
        self
    }
}

impl ParamState for StaticState {
    fn value(&self, target: &Target) -> Option<f32> {
        let key = target.to_string();
        self.values
            .iter()
            .find(|(name, _)| *name == key)
            .map(|(_, value)| *value)
    }

    fn switch(&self, target: &Target) -> Option<bool> {
        let key = target.to_string();
        self.switches
            .iter()
            .find(|(name, _)| *name == key)
            .map(|(_, on)| *on)
    }

    fn send(&self, track: usize, source: TrackSource) -> Option<BusSend> {
        self.sends
            .iter()
            .find(|(t, s, _)| *t == track && *s == source)
            .map(|(_, _, send)| *send)
    }
}
