//! Learn: "the next control I touch means *this*".
//!
//! The mode itself belongs in the user interface - ctrl-click a button, the next pad becomes that
//! button. What belongs here is the mechanic underneath it, and it is deliberately small: a pending
//! target, one incoming event, a binding, and a German sentence saying what happened.
//!
//! Three decisions are worth their lines:
//!
//! * **Note off is not a learn event.** A pad sends a note on and, a moment later, a note off with
//!   the same number. Learning on the note on and ignoring the off is the difference between "Pad 5
//!   ist jetzt Aufnahme" and the same message twice.
//! * **A knob learns from its first message and not from the loudest.** Turning a potentiometer
//!   sends a stream; the first one names the control, so nothing is gained by waiting.
//! * **A pad on a value target is allowed**, and says so. The velocity becomes the value, which is
//!   a real if blunt way to set one ("Pad hart = viel Hall"). Refusing would be tidier and would
//!   also mean that a file written by hand behaves differently from one learned, which is worse.

use super::binding::Binding;
use super::event::{MidiEvent, MidiId};
use super::target::{Control, Target};

/// What a completed learn produced.
#[derive(Clone, Debug, PartialEq)]
pub struct Learned {
    pub id: MidiId,
    pub binding: Binding,
    /// German sentence, ready to show.
    pub message: String,
    /// What was on this control before, if anything - so the caller can say "Pad 5 war vorher X".
    pub replaced: Option<Target>,
}

/// The learn state machine: at most one target waiting for a control.
#[derive(Debug, Default)]
pub struct Learn {
    pending: Option<Target>,
}

impl Learn {
    pub fn new() -> Self {
        Self::default()
    }

    /// From now on, the next incoming control belongs to this target. Returns the sentence to show
    /// while waiting.
    pub fn arm(&mut self, target: Target) -> String {
        let message = format!(
            "Lernmodus: die naechste Taste oder der naechste Regler wird \"{}\". \
             Abbruch mit Escape.",
            target.label()
        );
        self.pending = Some(target);
        message
    }

    pub fn cancel(&mut self) -> Option<String> {
        let target = self.pending.take()?;
        Some(format!("Lernmodus abgebrochen - \"{}\" bleibt unbelegt.", target.label()))
    }

    pub fn is_armed(&self) -> bool {
        self.pending.is_some()
    }

    pub fn pending(&self) -> Option<&Target> {
        self.pending.as_ref()
    }

    /// Feed an incoming event. Returns the binding once one has been learned; the mode disarms
    /// itself in the same breath.
    ///
    /// `previous` is what the map currently holds for that control, so the message can mention it.
    pub fn feed(&mut self, event: &MidiEvent, previous: Option<&Binding>) -> Option<Learned> {
        // A pad's note off would learn the same control a second time and, with a two-pad setup,
        // steal the second half of a press-and-release gesture.
        if self.pending.is_none() || matches!(event, MidiEvent::NoteOff { .. }) {
            return None;
        }
        let target = self.pending.take().expect("gerade geprueft");
        let id = event.id();
        let binding = Binding::new(target.clone());

        let mut message = format!("{id} ist jetzt \"{}\"", binding.describe());
        if let Some(previous) = previous
            && previous.target != binding.target
        {
            message.push_str(&format!(" (vorher \"{}\")", previous.target.label()));
        }
        message.push('.');
        if let (Control::Range(_), MidiEvent::NoteOn { .. }) = (target.control(), event) {
            message.push_str(
                " Das ist eine Taste auf einem Wert - die Anschlagstaerke wird zum Wert. \
                 Zum stufenlosen Fahren stattdessen einen Regler bewegen.",
            );
        }
        Some(Learned {
            id,
            binding,
            message,
            replaced: previous.map(|b| b.target.clone()),
        })
    }
}
