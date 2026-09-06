//! What one physical control does: the binding, and the map of them.
//!
//! # Two kinds of control need two kinds of rule
//!
//! A pad and a knob are not the same thing wearing different clothes.
//!
//! * A **pad** sends a note on and, later, a note off. Whether that should *trigger*, *toggle* or
//!   act *while held* is not a property of the pad - it is a property of what it is bound to, and
//!   sometimes of taste. "Aufnahme" wants a trigger: press it and a take is armed; a toggle would
//!   mean the second press has to mean something, and it does not. "Mithoeren" wants a toggle: it
//!   is a state, and a state that is only on while a finger is down is unusable when both hands are
//!   holding an instrument. And "Mithoeren" *also* wants to be momentary sometimes - talkback, one
//!   phrase sung over the loop. So [`ButtonMode`] is per binding, with a default per target.
//! * A **knob** sends a stream of values 0..127 and has to land on the target's own range. That is
//!   [`super::target::Range`], and the ends are taken from the engine's own clamps.
//!
//! # The value jump, and why pickup
//!
//! A knob is somewhere. The parameter is somewhere else - because a preset moved it, because the
//! mouse moved it, or simply because the knob has not been touched since the program started. The
//! first millimetre of movement then makes the parameter jump from where it is to where the knob
//! is. On a reverb mix that is audible; on a layer gain during a take it is a ruined take.
//!
//! Three ways out are in common use:
//!
//! | Verfahren | wie es wirkt | warum hier |
//! |---|---|---|
//! | **Pickup** (Standard hier) | der Regler wirkt erst, wenn er den aktuellen Wert passiert | braucht nichts vom Geraet |
//! | Relativ | der Regler sendet Differenzen statt Absolutwerten | **setzt einen Endlos-Encoder voraus** |
//! | Skalierung | der Rest des Weges wird auf den Rest des Bereichs gedehnt | kein Sprung, aber der Regler lügt über seine Position |
//!
//! The controller this is built for, an Akai MPD218, has **potentiometers**: they have a stop at
//! each end and send absolute values. Relative mode is therefore not available at all, and scaling
//! would leave the knob pointing at a value it is not on. Pickup it is - and because pickup means
//! "the first movement does nothing", which is confusing if you do not know it is on, it is
//! switchable per binding ([`Takeover::Jump`]).

use std::fmt;

use serde::{Deserialize, Serialize};

use super::event::MidiId;
use super::target::{Control, Range, Target};

/// What a button does to its target.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ButtonMode {
    /// Acts when pressed. The default for anything that fires.
    #[default]
    Press,
    /// Acts when released. For a pad that should not act until the foot comes off it.
    Release,
    /// Flips the state on every press. The default for anything that is on or off.
    Toggle,
    /// On while held, off when let go. Talkback.
    Momentary,
}

impl ButtonMode {
    pub fn name(self) -> &'static str {
        match self {
            ButtonMode::Press => "press",
            ButtonMode::Release => "release",
            ButtonMode::Toggle => "toggle",
            ButtonMode::Momentary => "momentary",
        }
    }

    pub fn parse(text: &str) -> Option<Self> {
        [
            ButtonMode::Press,
            ButtonMode::Release,
            ButtonMode::Toggle,
            ButtonMode::Momentary,
        ]
        .into_iter()
        .find(|mode| mode.name() == text)
    }

    pub fn names() -> [&'static str; 4] {
        ["press", "release", "toggle", "momentary"]
    }
}

/// How a knob takes a parameter over. See the module comment.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Takeover {
    /// The knob does nothing until it passes the value the parameter is on.
    #[default]
    Pickup,
    /// The knob acts immediately, jump and all. Switched on deliberately - after a preset it is
    /// the faster way back to a known state.
    Jump,
}

impl Takeover {
    pub fn name(self) -> &'static str {
        match self {
            Takeover::Pickup => "pickup",
            Takeover::Jump => "jump",
        }
    }

    pub fn parse(text: &str) -> Option<Self> {
        [Takeover::Pickup, Takeover::Jump]
            .into_iter()
            .find(|t| t.name() == text)
    }

    pub fn names() -> [&'static str; 2] {
        ["pickup", "jump"]
    }
}

/// One physical control's job.
#[derive(Clone, Debug, PartialEq)]
pub struct Binding {
    pub target: Target,
    pub button: ButtonMode,
    pub takeover: Takeover,
    /// Narrowed ends of the target's range. `None` means the target's own range, which is the
    /// engine's clamp.
    pub min: Option<f32>,
    pub max: Option<f32>,
}

impl Binding {
    /// A binding with the defaults that suit its target: fire for a trigger, toggle for a switch,
    /// pickup for a knob.
    pub fn new(target: Target) -> Self {
        let button = match target.control() {
            Control::Switch => ButtonMode::Toggle,
            _ => ButtonMode::Press,
        };
        Self {
            target,
            button,
            takeover: Takeover::default(),
            min: None,
            max: None,
        }
    }

    #[must_use]
    pub fn with_button(mut self, button: ButtonMode) -> Self {
        self.button = button;
        self
    }

    #[must_use]
    pub fn with_takeover(mut self, takeover: Takeover) -> Self {
        self.takeover = takeover;
        self
    }

    #[must_use]
    pub fn with_bounds(mut self, min: Option<f32>, max: Option<f32>) -> Self {
        self.min = min;
        self.max = max;
        self
    }

    /// The range this binding actually spans, target range narrowed by `min`/`max`.
    pub fn range(&self) -> Option<Range> {
        match self.target.control() {
            Control::Range(range) => Some(range.with_bounds(self.min, self.max)),
            _ => None,
        }
    }

    /// Reject a combination that cannot mean anything, in German, before it is written to a file.
    ///
    /// The one that matters: a *toggle* or *momentary* on something that only fires. "Aufnahme,
    /// aber umgeschaltet" has no second state to switch to, and silently treating it as a press
    /// would leave a musician with a file that says one thing and does another.
    pub fn check(&self) -> Result<(), String> {
        match self.target.control() {
            Control::Trigger
                if matches!(self.button, ButtonMode::Toggle | ButtonMode::Momentary) =>
            {
                Err(format!(
                    "\"{}\" loest nur aus, es hat keinen Zustand zum Umschalten. Erlaubt sind hier \
                     'press' und 'release'.",
                    self.target
                ))
            }
            Control::Range(_) => {
                let range = self.range().expect("Wertebereich");
                if range.min == range.max {
                    return Err(format!(
                        "\"{}\": 'min' und 'max' sind gleich ({}), damit laesst sich nichts fahren.",
                        self.target, range.min
                    ));
                }
                Ok(())
            }
            _ => Ok(()),
        }
    }

    /// German one-liner for a list.
    pub fn describe(&self) -> String {
        let mut text = self.target.label();
        match self.target.control() {
            Control::Range(_) => {
                let range = self.range().expect("Wertebereich");
                text.push_str(&format!(
                    "  [{} .. {}, {}]",
                    trim_number(range.min),
                    trim_number(range.max),
                    self.takeover.name()
                ));
            }
            _ => text.push_str(&format!("  [{}]", self.button.name())),
        }
        text
    }
}

impl fmt::Display for Binding {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.describe())
    }
}

/// Print a float the way a human would write it: `0.4`, not `0.4000000059604645`.
pub fn trim_number(value: f32) -> String {
    let text = format!("{value:.4}");
    let text = text.trim_end_matches('0').trim_end_matches('.');
    if text.is_empty() || text == "-" {
        "0".to_string()
    } else {
        text.to_string()
    }
}

/// What is bound to what, keyed by the physical control.
///
/// Keyed by [`MidiId`] and not by target, and that is the whole reason a conflict is even
/// detectable: one pad can only do one thing, while one thing may perfectly well be reachable from
/// two pads. The score writes its block the other way round (target -> control, which is what reads
/// well in a score), and the reader turns it around here - and reports it when two targets land on
/// the same pad.
#[derive(Clone, Debug, Default)]
pub struct MidiMap {
    entries: Vec<(MidiId, Binding)>,
}

/// Two mappings are the same when they bind the same controls to the same things. The order the
/// entries happen to sit in is a rendering detail - a file is written sorted, a learn session
/// appends - and comparing it would make "written and read back gives the same mapping" false for
/// a mapping that is, in every way that matters, the same.
impl PartialEq for MidiMap {
    fn eq(&self, other: &Self) -> bool {
        self.entries.len() == other.entries.len() && self.sorted() == other.sorted()
    }
}

impl MidiMap {
    pub fn new() -> Self {
        Self::default()
    }

    /// Add a binding. Returns what was there before, if anything.
    pub fn insert(&mut self, id: MidiId, binding: Binding) -> Option<Binding> {
        match self.entries.iter_mut().find(|(key, _)| *key == id) {
            Some(slot) => Some(std::mem::replace(&mut slot.1, binding)),
            None => {
                self.entries.push((id, binding));
                None
            }
        }
    }

    pub fn get(&self, id: MidiId) -> Option<&Binding> {
        self.entries
            .iter()
            .find(|(key, _)| *key == id)
            .map(|(_, binding)| binding)
    }

    pub fn remove(&mut self, id: MidiId) -> Option<Binding> {
        let position = self.entries.iter().position(|(key, _)| *key == id)?;
        Some(self.entries.remove(position).1)
    }

    pub fn iter(&self) -> impl Iterator<Item = (MidiId, &Binding)> {
        self.entries.iter().map(|(id, binding)| (*id, binding))
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Sort by id, so a written file is stable and two files can be diffed.
    pub fn sorted(&self) -> Vec<(MidiId, &Binding)> {
        let mut all: Vec<(MidiId, &Binding)> = self.iter().collect();
        all.sort_by_key(|(id, _)| *id);
        all
    }

    /// Lay another map over this one: the song's additions on top of the controller's profile.
    ///
    /// Returns one German line per control the score took over, because that is exactly the thing
    /// a musician needs told. It is **not** an error - overriding the profile for one piece is what
    /// the score's `midi:` block is for - but a pad that silently stopped doing what it does in
    /// every other song is how a live set goes wrong.
    pub fn overlay(&mut self, other: &MidiMap) -> Vec<String> {
        let mut notes = Vec::new();
        for (id, binding) in other.iter() {
            if let Some(previous) = self.insert(id, binding.clone())
                && previous.target != binding.target
            {
                notes.push(format!(
                    "{id}: die Partitur belegt diese Taste mit \"{}\" statt \"{}\" aus dem Profil.",
                    binding.target, previous.target
                ));
            }
        }
        notes
    }
}
