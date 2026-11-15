//! The engine's track layout as it last ran, and what a new one renamed.
//!
//! Plugins (`plugin-rack.json`) and the free-mode mix (`mixer.json`) are kept by **track name**,
//! so a score that orders its tracks differently from the setup still finds each track's plugins
//! and faders. The price of names is that a rename looks like a track going away and another one
//! arriving. This module tells the two apart where that can be told: every engine start compares
//! the new layout with the previous one (kept in `<config>/layout.json`, so it also works across
//! a restart of the program) and calls a pair a rename when
//!
//! * the old name is gone, the new name is new, and both sit on the same input(s) - and no other
//!   gone or new track does; or, failing that,
//! * exactly one name went and exactly one came, at the same position.
//!
//! Anything less certain is not a rename: the old name's plugins and mix stay where they are and
//! come back when a track of that name does.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

const FILE: &str = "layout.json";

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct LayoutTrack {
    pub name: String,
    /// 1-based, the left one of a stereo pair.
    pub input: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub input_right: Option<u32>,
}

impl LayoutTrack {
    pub fn new(name: impl Into<String>, input: u32, input_right: Option<u32>) -> Self {
        Self {
            name: name.into(),
            input,
            input_right,
        }
    }

    fn wiring(&self) -> (u32, Option<u32>) {
        (self.input, self.input_right)
    }
}

/// `(old name, new name)` for every track of `new` that is a renamed track of `old`.
pub fn renames(old: &[LayoutTrack], new: &[LayoutTrack]) -> Vec<(String, String)> {
    let gone: Vec<(usize, &LayoutTrack)> = old
        .iter()
        .enumerate()
        .filter(|(_, t)| !new.iter().any(|n| n.name == t.name))
        .collect();
    let came: Vec<(usize, &LayoutTrack)> = new
        .iter()
        .enumerate()
        .filter(|(_, t)| !old.iter().any(|o| o.name == t.name))
        .collect();
    let mut out = Vec::new();
    let mut used_gone = Vec::new();
    let mut used_came = Vec::new();
    for (ci, c) in &came {
        let same: Vec<&(usize, &LayoutTrack)> =
            gone.iter().filter(|(_, g)| g.wiring() == c.wiring()).collect();
        let rivals = came.iter().filter(|(_, o)| o.wiring() == c.wiring()).count();
        if same.len() == 1 && rivals == 1 {
            let (gi, g) = same[0];
            out.push((g.name.clone(), c.name.clone()));
            used_gone.push(*gi);
            used_came.push(*ci);
        }
    }
    let rest_gone: Vec<&(usize, &LayoutTrack)> =
        gone.iter().filter(|(i, _)| !used_gone.contains(i)).collect();
    let rest_came: Vec<&(usize, &LayoutTrack)> =
        came.iter().filter(|(i, _)| !used_came.contains(i)).collect();
    if let ([(gi, g)], [(ci, c)]) = (rest_gone.as_slice(), rest_came.as_slice())
        && gi == ci
    {
        out.push((g.name.clone(), c.name.clone()));
    }
    out
}

/// `<config>/layout.json`, next to the rack and the mix.
pub fn path() -> PathBuf {
    let midi = looper_engine::midi::profile::profile_dir();
    midi.parent().map(PathBuf::from).unwrap_or(midi).join(FILE)
}

pub fn load_at(path: &Path) -> Vec<LayoutTrack> {
    std::fs::read_to_string(path)
        .ok()
        .and_then(|text| serde_json::from_str(&text).ok())
        .unwrap_or_default()
}

pub fn save_at(path: &Path, layout: &[LayoutTrack]) -> Result<(), String> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    }
    let text = serde_json::to_string_pretty(layout).map_err(|e| e.to_string())?;
    std::fs::write(path, text).map_err(|e| e.to_string())
}

/// Remember `layout` as the one the engine runs with now, and say what it renamed against the
/// previous one.
pub fn replace(layout: &[LayoutTrack]) -> Vec<(String, String)> {
    let path = path();
    let previous = load_at(&path);
    let found = renames(&previous, layout);
    if previous != layout
        && let Err(e) = save_at(&path, layout)
    {
        crate::logfile::log!("Track layout not saved to {}: {e}", path.display());
    }
    found
}

#[cfg(test)]
mod tests {
    use super::*;

    fn t(name: &str, input: u32) -> LayoutTrack {
        LayoutTrack::new(name, input, None)
    }

    #[test]
    fn a_name_change_on_the_same_input_is_a_rename() {
        let old = [t("stimme", 1), t("gitarre", 2)];
        let new = [t("vocals", 1), t("gitarre", 2)];
        assert_eq!(renames(&old, &new), vec![("stimme".to_string(), "vocals".to_string())]);
    }

    #[test]
    fn a_reordered_score_with_one_new_name_is_matched_by_input() {
        // Setup: stimme, gitarre. Score: gitarre, voice - voice on the singer's input.
        let old = [t("stimme", 1), t("gitarre", 2)];
        let new = [t("gitarre", 2), t("voice", 1)];
        assert_eq!(renames(&old, &new), vec![("stimme".to_string(), "voice".to_string())]);
    }

    #[test]
    fn the_same_position_counts_when_the_input_moved_too() {
        let old = [t("a", 1), t("b", 2)];
        let new = [t("a", 1), t("c", 5)];
        assert_eq!(renames(&old, &new), vec![("b".to_string(), "c".to_string())]);
    }

    #[test]
    fn nothing_is_guessed() {
        // Two new tracks on the input of one old one: which one is it?
        let old = [t("a", 1)];
        let new = [t("b", 1), t("c", 1)];
        assert!(renames(&old, &new).is_empty());
        // Unchanged, added, removed: no renames.
        assert!(renames(&[t("a", 1)], &[t("a", 1), t("b", 2)]).is_empty());
        assert!(renames(&[t("a", 1), t("b", 2)], &[t("a", 1)]).is_empty());
        assert!(renames(&[], &[t("a", 1)]).is_empty());
        // Gone at one position, come at another, on different inputs.
        assert!(renames(&[t("a", 1), t("b", 2)], &[t("c", 3), t("a", 1)]).is_empty());
    }

    #[test]
    fn stereo_wiring_has_to_match_completely() {
        let old = [LayoutTrack::new("keys", 3, Some(4)), t("x", 1)];
        let new = [LayoutTrack::new("piano", 3, Some(4)), t("x", 1)];
        assert_eq!(renames(&old, &new), vec![("keys".to_string(), "piano".to_string())]);
        let mono = [t("piano", 3), t("x", 1)];
        // Not by wiring, but by position: the one gone and the one come stand at index 0.
        assert_eq!(renames(&old, &mono), vec![("keys".to_string(), "piano".to_string())]);
    }
}
