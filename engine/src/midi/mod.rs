//! MIDI control: a pad or a knob operates whatever a mouse could operate.
//!
//! ```text
//!   Controller ──► input.rs ──► event.rs ──► binding.rs ──► router.rs ──► MidiAction
//!   (MPD218)       midir,       Bytes zu      was liegt      aufloesen,    (dasselbe, was
//!                  Queue        Ereignissen   auf welcher    Wert rechnen,  ein Mausklick
//!                                             Taste?         Grenze pruefen  erzeugt)
//!                                                ▲
//!                                    profile.rs ─┴─ score's midi: block
//!                                    (das Geraet)   (dieses Stueck)
//! ```
//!
//! # The two stages, and why there are two
//!
//! A mapping belongs to the **controller**, not to the song: learned once, valid in every piece.
//! Putting it in the score would mean re-learning every pad for every new piece and keeping twenty
//! slowly diverging copies of the same list. So a [`Profile`] holds the controller's mapping, and a
//! score may lay its own additions over it for the one piece that needs "Pad 5 springt zum
//! Refrain". The composition is [`MidiMap::overlay`], and it reports every control the score takes
//! over rather than letting a pad silently change its meaning.
//!
//! # Where the boundaries are
//!
//! | Datei | braucht Geraet | was drin steckt |
//! |---|---|---|
//! | [`event`] | nein | Bytes zu Ereignissen, laufender Status, Note-On mit Velocity 0 |
//! | [`target`] | nein | der Parameterbaum: jede bedienbare Sache hat eine Adresse |
//! | [`binding`] | nein | Taster gegen Umschalter, Wertebereiche, Wertesprung-Schutz |
//! | [`router`] | nein | Ereignis + Mapping -> Absicht, samt der Grenze zum laufenden Runner |
//! | [`profile`] | nein (nur Dateisystem) | Controller-Profil lesen und schreiben |
//! | [`learn`] | nein | "die naechste Taste wird X" |
//! | [`input`] | **ja** | midir, Geraeteliste, Oeffnen, Queue |
//! | [`cli`] | ja, ausser `targets` | das `midi`-Subkommando |
//!
//! Everything above [`input`] is provable without an instrument on the desk, which is the point:
//! the only thing that cannot be tested offline is opening a port, and that one is a dozen lines.

pub mod binding;
pub mod cli;
pub mod event;
pub mod input;
pub mod learn;
pub mod profile;
pub mod router;
pub mod target;

#[cfg(test)]
mod tests;

pub use binding::{Binding, ButtonMode, MidiMap, Takeover};
pub use event::{MidiDecoder, MidiEvent, MidiId, MidiIdKind};
pub use learn::{Learn, Learned};
pub use profile::{Profile, profile_dir, profile_for_port, profile_path};
pub use router::{
    Context, MidiAction, ParamState, Resolution, Router, StaticState, TrackLayout,
};
pub use target::{Control, Curve, FxKnob, Range, Target, TrackRef, catalogue};

use crate::score::CompiledScore;

/// The score's `midi:` block as a mapping.
///
/// A compiled score has been through the reader, which already refused a bad address and a control
/// bound twice. It can also arrive from JSON, though - the app hands compiled scores around - so
/// everything is checked again here rather than trusted.
pub fn score_map(score: &CompiledScore) -> Result<MidiMap, Vec<String>> {
    let mut map = MidiMap::new();
    let mut issues = Vec::new();
    for (key, binding) in score.midi.iter() {
        let id = match MidiId::parse(&binding.id) {
            Ok(id) => id,
            Err(message) => {
                issues.push(format!("'{key}': {message}"));
                continue;
            }
        };
        let target = match Target::parse(&binding.target) {
            Ok(target) => target,
            Err(message) => {
                issues.push(format!("'{key}': {message}"));
                continue;
            }
        };
        let mut entry = Binding::new(target);
        if let Some(mode) = binding.mode {
            entry.button = mode;
        }
        if let Some(takeover) = binding.takeover {
            entry.takeover = takeover;
        }
        entry = entry.with_bounds(binding.min, binding.max);
        if let Err(message) = entry.check() {
            issues.push(format!("'{key}': {message}"));
            continue;
        }
        if let Some(previous) = map.insert(id, entry) {
            issues.push(format!(
                "'{key}': {id} ist schon mit \"{}\" belegt. Eine Taste kann nur eine Sache tun.",
                previous.target
            ));
        }
    }
    if issues.is_empty() { Ok(map) } else { Err(issues) }
}

/// The mapping a session actually runs on: the controller's profile with the score laid over it.
///
/// The second half of the answer is one German line per control the score took over - not an error,
/// but the thing a musician has to be told before he presses it.
pub fn session_map(
    profile: Option<&Profile>,
    score: Option<&CompiledScore>,
) -> Result<(MidiMap, Vec<String>), Vec<String>> {
    let mut map = profile.map(|p| p.map.clone()).unwrap_or_default();
    let mut notes = Vec::new();
    if let Some(score) = score {
        notes = map.overlay(&score_map(score)?);
    }
    Ok((map, notes))
}
