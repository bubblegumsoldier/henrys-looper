//! Would this binding work right now? The answer every user interface shows next to a binding.
//!
//! A binding can be dead for three different reasons, and each one has a different fix:
//!
//! | Zustand | Ursache | was hilft |
//! |---|---|---|
//! | [`BindingState::DeviceMissing`] | der Controller ist nicht angesteckt | anstecken |
//! | [`BindingState::TargetMissing`] | das Ziel gibt es in dieser Sitzung nicht (Track 5 bei 2 Tracks) | andere Sitzung oder neu lernen |
//! | [`BindingState::Overridden`] | die geladene Partitur belegt dieselbe Taste anders | Partitur entladen |
//!
//! plus [`BindingState::Unchecked`] for the honest "cannot tell yet": without a running engine
//! there is no track list to check `track.3` against. That is not shown as an error, because the
//! most common moment to look at the list is right after starting the app, before the engine runs.
//!
//! Everything here is a pure function of the target and a [`SessionShape`], so the whole table is
//! testable without a device and without an engine.

use serde::Serialize;

use super::router::TrackLayout;
use super::target::{Target, TrackRef};

/// The little a binding's status depends on: which tracks exist and whether a score is loaded.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SessionShape {
    /// The session's track names in order, or `None` while no engine runs.
    pub tracks: Option<Vec<String>>,
    /// Number of sections of the loaded score, or `None` without a score.
    pub sections: Option<usize>,
}

impl SessionShape {
    pub fn running(&self) -> bool {
        self.tracks.is_some()
    }
}

/// Whether one binding works at the moment. Serialised in `snake_case` for the user interface.
#[derive(Serialize, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum BindingState {
    /// Device connected, target exists: pressing it does something.
    Active,
    /// Device connected, but without a running engine there is nothing to check the target
    /// against yet. Not an error.
    Unchecked,
    /// The controller this binding belongs to is not connected.
    DeviceMissing,
    /// The target does not exist in the current session.
    TargetMissing,
    /// The loaded score binds the same control to something else for this piece.
    Overridden,
}

impl BindingState {
    /// True for the two states in which pressing the control can do its job.
    pub fn works(self) -> bool {
        matches!(self, BindingState::Active | BindingState::Unchecked)
    }
}

/// The answer for one target.
#[derive(Clone, Debug, PartialEq)]
pub struct TargetCheck {
    /// [`BindingState::Active`], [`BindingState::Unchecked`] or [`BindingState::TargetMissing`].
    pub state: BindingState,
    /// English sentence for anything but `Active`.
    pub reason: Option<String>,
    /// The target with its track as a position (`track.1...`), where that is known. This is the
    /// address the user interface element carries, so a score binding written as
    /// `track.vocals.record` finds the button `track.1.record`.
    pub resolved: Option<Target>,
}

const ENGINE_OFF: &str = "Engine is not running - checked as soon as it starts.";

/// Check one target against the session.
pub fn check_target(target: &Target, shape: &SessionShape) -> TargetCheck {
    let unchecked = |resolved: Option<Target>| TargetCheck {
        state: BindingState::Unchecked,
        reason: Some(ENGINE_OFF.to_string()),
        resolved,
    };
    let missing = |reason: String| TargetCheck {
        state: BindingState::TargetMissing,
        reason: Some(reason),
        resolved: None,
    };
    let active = |resolved: Target| TargetCheck {
        state: BindingState::Active,
        reason: None,
        resolved: Some(resolved),
    };

    if target.is_score_transport() {
        let Some(sections) = shape.sections else {
            if !shape.running() {
                return unchecked(Some(target.clone()));
            }
            return missing("No score loaded - the score transport only exists with one.".into());
        };
        if let Target::ScoreGoto(index) = target
            && *index >= sections
        {
            return missing(format!(
                "Section {} does not exist - the loaded score has {sections}.",
                index + 1
            ));
        }
        return active(target.clone());
    }

    let Some(reference) = target.track() else {
        return match shape.tracks {
            None => unchecked(Some(target.clone())),
            Some(_) => active(target.clone()),
        };
    };
    let Some(names) = &shape.tracks else {
        // A position is already the spelling of the user interface; a name is not known yet.
        let resolved = matches!(reference, TrackRef::Index(_)).then(|| target.clone());
        return unchecked(resolved);
    };
    match TrackLayout::new(names.iter().cloned()).resolve(reference) {
        Ok(index) => active(target.with_track(TrackRef::Index(index))),
        Err(_) => missing(match reference {
            TrackRef::Index(index) => format!(
                "Track {} does not exist - this session has {}.",
                index + 1,
                match names.len() {
                    0 => "no tracks".to_string(),
                    1 => "1 track".to_string(),
                    n => format!("{n} tracks"),
                }
            ),
            TrackRef::Name(name) => format!(
                "There is no track \"{name}\" in this session ({}).",
                if names.is_empty() {
                    "no tracks".to_string()
                } else {
                    names.join(", ")
                }
            ),
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn shape(tracks: &[&str], sections: Option<usize>) -> SessionShape {
        SessionShape {
            tracks: Some(tracks.iter().map(|s| s.to_string()).collect()),
            sections,
        }
    }

    fn check(address: &str, shape: &SessionShape) -> TargetCheck {
        check_target(&Target::parse(address).expect("Adresse"), shape)
    }

    #[test]
    fn a_track_beyond_the_session_is_missing_and_says_how_many_there_are() {
        let two = shape(&["a", "b"], None);
        let result = check("track.5.record", &two);
        assert_eq!(result.state, BindingState::TargetMissing);
        assert!(result.reason.as_deref().unwrap().contains("2 tracks"), "{result:?}");
        assert_eq!(check("track.2.record", &two).state, BindingState::Active);
    }

    #[test]
    fn a_named_track_resolves_to_the_position_the_buttons_carry() {
        let result = check("track.stimme.fx.reverb.mix", &shape(&["bass", "stimme"], None));
        assert_eq!(result.state, BindingState::Active);
        assert_eq!(
            result.resolved.map(|t| t.to_string()).as_deref(),
            Some("track.2.fx.reverb.mix")
        );
        let unknown = check("track.gesang.record", &shape(&["bass"], None));
        assert_eq!(unknown.state, BindingState::TargetMissing);
        assert!(unknown.reason.unwrap().contains("bass"));
    }

    #[test]
    fn without_an_engine_nothing_is_called_broken() {
        let off = SessionShape::default();
        for address in ["track.9.record", "global.click", "transport.next", "track.x.pan"] {
            let result = check(address, &off);
            assert_eq!(result.state, BindingState::Unchecked, "{address}");
            assert!(result.state.works());
        }
        // A position needs no session to be spelled the way the buttons spell it; a name does.
        assert!(check("track.1.pan", &off).resolved.is_some());
        assert!(check("track.x.pan", &off).resolved.is_none());
    }

    #[test]
    fn the_score_transport_needs_a_score_and_a_goto_needs_its_section() {
        let no_score = shape(&["a"], None);
        assert_eq!(check("transport.next", &no_score).state, BindingState::TargetMissing);
        let three = shape(&["a"], Some(3));
        assert_eq!(check("transport.next", &three).state, BindingState::Active);
        assert_eq!(check("transport.goto.3", &three).state, BindingState::Active);
        let beyond = check("transport.goto.4", &three);
        assert_eq!(beyond.state, BindingState::TargetMissing);
        assert!(beyond.reason.unwrap().contains("has 3"));
    }

    #[test]
    fn global_targets_exist_in_every_running_session() {
        let one = shape(&["a"], None);
        for address in ["global.click", "global.tempo", "global.bus.main.gain", "global.quantize.bar"] {
            assert_eq!(check(address, &one).state, BindingState::Active, "{address}");
        }
    }
}
