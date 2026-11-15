//! The mix of the free mode, kept on disk: `mixer.json` next to the MIDI profiles and the plugin
//! rack, keyed by track name.
//!
//! A score keeps its mix in its own `mixer:` section. Without one loaded - the engine started from
//! the setup screen - there is no file the faders could belong to, and a restart of the program or
//! of the engine used to put every one of them back to 0 dB. This is where they go instead.
//!
//! What is kept is exactly what a `mixer:` entry says (`looper_engine::score::mixer`): fader in
//! dB, mute, the effect chain. Solo is not, for the same reason the score does not keep it. The
//! bus levels are not here either: in the free mode they belong to the setup (`bus_gain_main`,
//! `click_gain`, ... of the start configuration), which the setup screen edits and the engine is
//! started with.
//!
//! Entries are never dropped because a track is missing today: a setup with fewer tracks, or a
//! score with other names, leaves the other entries alone, and they apply again once a track of
//! that name is back.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use looper_engine::score::OrderedMap;
use looper_engine::score::mixer::TrackMixSource;

const FILE: &str = "mixer.json";
const VERSION: u32 = 1;

#[derive(Debug, Default, Serialize, Deserialize, PartialEq)]
pub struct StoredMix {
    #[serde(default)]
    pub version: u32,
    #[serde(default)]
    pub tracks: OrderedMap<TrackMixSource>,
}

/// `<config>/mixer.json`. Follows `HENRYS_LOOPER_CONFIG_DIR` like everything else there.
pub fn path() -> PathBuf {
    let midi = looper_engine::midi::profile::profile_dir();
    midi.parent().map(PathBuf::from).unwrap_or(midi).join(FILE)
}

/// What is stored; empty when there is nothing or it cannot be read.
pub fn load() -> StoredMix {
    load_at(&path())
}

pub fn load_at(path: &Path) -> StoredMix {
    std::fs::read_to_string(path)
        .ok()
        .and_then(|text| serde_json::from_str::<StoredMix>(&text).ok())
        .unwrap_or_default()
}

/// Put these entries in, keeping every other one, and write the file.
pub fn merge_and_save(entries: impl IntoIterator<Item = (String, TrackMixSource)>) -> Result<(), String> {
    merge_and_save_at(&path(), entries)
}

pub fn merge_and_save_at(
    path: &Path,
    entries: impl IntoIterator<Item = (String, TrackMixSource)>,
) -> Result<(), String> {
    let mut stored = load_at(path);
    stored.version = VERSION;
    for (name, entry) in entries {
        stored.tracks.insert(name, entry);
    }
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    }
    let text = serde_json::to_string_pretty(&stored).map_err(|e| e.to_string())?;
    // Written next to it and renamed, so a crash never leaves half a file.
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, text).map_err(|e| e.to_string())?;
    std::fs::rename(&tmp, path).map_err(|e| e.to_string())
}

/// Carry the entries of renamed tracks over to their new names (`(old, new)` pairs). An entry
/// that already exists under the new name wins; the old one stays, in case the name comes back.
pub fn rename(renames: &[(String, String)]) -> Result<(), String> {
    rename_at(&path(), renames)
}

pub fn rename_at(path: &Path, renames: &[(String, String)]) -> Result<(), String> {
    let stored = load_at(path);
    let moved: Vec<(String, TrackMixSource)> = renames
        .iter()
        .filter(|(_, to)| !stored.tracks.contains_key(to))
        .filter_map(|(from, to)| stored.tracks.get(from).map(|e| (to.clone(), e.clone())))
        .collect();
    if moved.is_empty() {
        return Ok(());
    }
    merge_and_save_at(path, moved)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("looper-mixstore-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        dir.join(FILE)
    }

    fn gain(db: f64) -> TrackMixSource {
        TrackMixSource {
            gain: Some(db),
            ..Default::default()
        }
    }

    #[test]
    fn entries_of_tracks_that_are_not_there_today_survive_a_save() {
        let path = scratch("keep");
        merge_and_save_at(&path, [("stimme".into(), gain(-3.0)), ("gitarre".into(), gain(-6.0))]).unwrap();
        // A session with only the guitar writes only the guitar.
        merge_and_save_at(&path, [("gitarre".into(), gain(-1.0))]).unwrap();
        let stored = load_at(&path);
        assert_eq!(stored.version, VERSION);
        assert_eq!(stored.tracks.get("stimme"), Some(&gain(-3.0)));
        assert_eq!(stored.tracks.get("gitarre"), Some(&gain(-1.0)));
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn a_renamed_track_takes_its_mix_along() {
        let path = scratch("rename");
        merge_and_save_at(&path, [("stimme".into(), gain(-3.0)), ("voice".into(), gain(2.0))]).unwrap();
        rename_at(&path, &[("stimme".into(), "vocals".into()), ("stimme".into(), "voice".into())]).unwrap();
        let stored = load_at(&path);
        assert_eq!(stored.tracks.get("vocals"), Some(&gain(-3.0)));
        assert_eq!(stored.tracks.get("voice"), Some(&gain(2.0)), "an existing entry wins");
        assert_eq!(stored.tracks.get("stimme"), Some(&gain(-3.0)), "kept for when it comes back");
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn a_missing_or_broken_file_is_an_empty_mix() {
        let path = scratch("broken");
        assert_eq!(load_at(&path), StoredMix::default());
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, "{ nicht json").unwrap();
        assert!(load_at(&path).tracks.is_empty());
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }
}
