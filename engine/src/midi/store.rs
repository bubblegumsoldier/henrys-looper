//! All controller profiles of this computer, kept in step with the files on disk.
//!
//! [`super::profile`] knows what *one* file says. This is the directory: which profiles exist,
//! which of them belongs to a port that just appeared, and writing one back after every learned
//! binding without destroying what a human wrote into it.
//!
//! # Three promises
//!
//! * **A learned binding is on disk the moment it is learned.** There is no "save" step to forget;
//!   [`ProfileStore::save`] is called by the app after every change.
//! * **Nothing a human wrote is destroyed.** Saving edits the existing file line by line
//!   ([`Profile::rewrite`]), so comments, grouping and order survive. If a file cannot be edited
//!   that way, the original is copied to `<datei>.yaml.bak` before it is replaced. A file that does
//!   not parse is never overwritten at all - it is reported, and it stays exactly as it is until
//!   somebody fixes it.
//! * **A file edited by hand while the app runs is picked up.** [`ProfileStore::scan`] compares
//!   modification times and reloads what changed; the file wins over memory.

use std::path::{Path, PathBuf};
use std::time::SystemTime;

use super::profile::{Profile, device_hint, slug};

/// One profile file.
#[derive(Clone, Debug)]
pub struct StoredProfile {
    pub path: PathBuf,
    /// `device:` of the file - from the parsed profile, or read leniently out of a broken file.
    pub device: String,
    /// `None` when the file does not parse; `error` says why.
    pub profile: Option<Profile>,
    pub error: Option<String>,
    /// True when memory holds changes the file does not (the last save failed).
    pub unsaved: bool,
    /// Modification time of the file as last read or written - how a hand edit is noticed.
    stamp: Option<SystemTime>,
}

impl StoredProfile {
    /// Number of bindings, zero for a broken file.
    pub fn len(&self) -> usize {
        self.profile.as_ref().map_or(0, |p| p.map.len())
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

/// What a [`ProfileStore::scan`] found.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum StoreChange {
    Added(PathBuf),
    /// The file changed on disk and was read again.
    Reloaded(PathBuf),
    Removed(PathBuf),
}

/// What a save did.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Saved {
    pub path: PathBuf,
    /// Where the original was copied before it was written afresh, if that was necessary.
    pub backup: Option<PathBuf>,
}

/// The profile directory.
#[derive(Debug)]
pub struct ProfileStore {
    dir: PathBuf,
    entries: Vec<StoredProfile>,
}

impl ProfileStore {
    /// A store on that directory. Reads nothing yet - call [`Self::scan`].
    pub fn new(dir: impl Into<PathBuf>) -> Self {
        Self {
            dir: dir.into(),
            entries: Vec::new(),
        }
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    pub fn entries(&self) -> &[StoredProfile] {
        &self.entries
    }

    pub fn get(&self, path: &Path) -> Option<&StoredProfile> {
        self.entries.iter().find(|e| e.path == path)
    }

    pub fn get_mut(&mut self, path: &Path) -> Option<&mut StoredProfile> {
        self.entries.iter_mut().find(|e| e.path == path)
    }

    /// Bring the list in step with the directory: new files, files edited by hand, files deleted.
    /// Cheap when nothing changed - one directory listing and one `stat` per file.
    pub fn scan(&mut self) -> Vec<StoreChange> {
        let mut changes = Vec::new();
        let mut present: Vec<PathBuf> = Vec::new();
        if let Ok(listing) = std::fs::read_dir(&self.dir) {
            for item in listing.flatten() {
                let path = item.path();
                if path.extension().and_then(|e| e.to_str()) != Some("yaml") {
                    continue;
                }
                let stamp = item.metadata().ok().and_then(|m| m.modified().ok());
                present.push(path.clone());
                match self.entries.iter().position(|e| e.path == path) {
                    Some(index) if self.entries[index].stamp == stamp => {}
                    Some(index) => {
                        if let Some(entry) = load(&path) {
                            self.entries[index] = entry;
                            changes.push(StoreChange::Reloaded(path));
                        }
                    }
                    None => {
                        if let Some(entry) = load(&path) {
                            self.entries.push(entry);
                            changes.push(StoreChange::Added(path));
                        }
                    }
                }
            }
        }
        self.entries.retain(|entry| {
            let keep = present.contains(&entry.path);
            if !keep {
                changes.push(StoreChange::Removed(entry.path.clone()));
            }
            keep
        });
        self.entries.sort_by(|a, b| a.path.cmp(&b.path));
        changes
    }

    /// The profile meant for a port of that name: the longest matching `device:` wins, so
    /// "MPD218 Port A" is not taken by a profile for "MPD". Broken files take part - a broken
    /// profile for this device must be reported, not silently replaced by a fresh one.
    pub fn best_for_port(&self, port: &str) -> Option<&Path> {
        self.entries
            .iter()
            .filter(|entry| Profile::new(entry.device.clone()).matches(port))
            .max_by_key(|entry| entry.device.len())
            .map(|entry| entry.path.as_path())
    }

    /// A fresh, empty profile for a device, written at once so the device is remembered from now
    /// on. Returns the existing one if there is one for that device already.
    pub fn create(&mut self, device: &str) -> Result<PathBuf, String> {
        if let Some(entry) = self.entries.iter().find(|e| e.device == device) {
            return Ok(entry.path.clone());
        }
        let base = slug(device);
        let mut path = self.dir.join(format!("{base}.yaml"));
        let mut n = 2;
        while path.exists() {
            path = self.dir.join(format!("{base}-{n}.yaml"));
            n += 1;
        }
        self.entries.push(StoredProfile {
            path: path.clone(),
            device: device.to_string(),
            profile: Some(Profile::new(device)),
            error: None,
            unsaved: true,
            stamp: None,
        });
        self.save(&path)?;
        Ok(path)
    }

    /// Write one profile back, keeping what a human wrote into the file (see the module comment).
    pub fn save(&mut self, path: &Path) -> Result<Saved, String> {
        let entry = self
            .entries
            .iter_mut()
            .find(|e| e.path == path)
            .ok_or_else(|| format!("No profile at {}.", path.display()))?;
        let Some(profile) = entry.profile.as_ref() else {
            return Err(format!(
                "{} has errors and is left untouched - fix it first: {}",
                path.display(),
                entry.error.as_deref().unwrap_or("")
            ));
        };
        let original = std::fs::read_to_string(path).ok();
        if let Some(text) = &original
            && let Err(issues) = Profile::parse(text)
        {
            // Changed on disk into something broken since it was read: never overwrite that.
            entry.unsaved = true;
            return Err(format!(
                "{} has errors and was not overwritten: {}",
                path.display(),
                issues.join("; ")
            ));
        }
        let (text, backup) = match &original {
            Some(original) => {
                let rewritten = profile.rewrite(original);
                let backup = if rewritten.kept || *original == rewritten.text {
                    None
                } else {
                    let backup = path.with_extension("yaml.bak");
                    std::fs::write(&backup, original).map_err(|e| {
                        format!("Backup {} could not be written: {e}", backup.display())
                    })?;
                    Some(backup)
                };
                (rewritten.text, backup)
            }
            None => (profile.to_yaml(), None),
        };
        let result = write_atomically(path, &text);
        if let Err(e) = result {
            entry.unsaved = true;
            return Err(e);
        }
        entry.unsaved = false;
        entry.stamp = std::fs::metadata(path).and_then(|m| m.modified()).ok();
        Ok(Saved {
            path: path.to_path_buf(),
            backup,
        })
    }
}

/// Read one file into an entry. `None` only when it cannot be read at all.
fn load(path: &Path) -> Option<StoredProfile> {
    let stamp = std::fs::metadata(path).and_then(|m| m.modified()).ok();
    let text = std::fs::read_to_string(path).ok()?;
    let stem = path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("controller")
        .to_string();
    Some(match Profile::parse(&text) {
        Ok(profile) => StoredProfile {
            path: path.to_path_buf(),
            device: profile.device.clone(),
            profile: Some(profile),
            error: None,
            unsaved: false,
            stamp,
        },
        Err(issues) => StoredProfile {
            path: path.to_path_buf(),
            device: device_hint(&text).unwrap_or(stem),
            profile: None,
            error: Some(issues.join("; ")),
            unsaved: false,
            stamp,
        },
    })
}

/// Write next to the file and rename over it, so a crash in the middle leaves the old file and not
/// half of the new one.
fn write_atomically(path: &Path, text: &str) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| format!("Folder {} could not be created: {e}", parent.display()))?;
    }
    let temp = path.with_extension("yaml.tmp");
    std::fs::write(&temp, text)
        .and_then(|()| std::fs::rename(&temp, path))
        .map_err(|e| format!("Profile {} could not be written: {e}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::midi::{Binding, ButtonMode, MidiId, Target};

    /// A directory of its own per test, so tests run in parallel without sharing files.
    fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "looper-store-{name}-{}",
            std::process::id(),
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("Testverzeichnis");
        dir
    }

    fn bind(address: &str) -> Binding {
        Binding::new(Target::parse(address).expect("Adresse"))
    }

    const HAND_WRITTEN: &str = "# Mein MPD218 - bitte nicht wegwerfen.\n\
device: MPD218\n\
bindings:\n  \
  # --- untere Reihe ---\n  \
  ch10.note37: {target: track.1.overdub}\n  \
  ch10.note36: {target: track.1.record}   # der wichtigste\n\
\n  \
  # --- Regler ---\n  \
  ch1.cc3: {target: track.1.fx.reverb.mix, max: 0.4}\n";

    #[test]
    fn a_rewrite_keeps_comments_order_and_untouched_lines() {
        assert!(HAND_WRITTEN.contains("\n  # --- untere Reihe ---\n  ch10.note37"), "{HAND_WRITTEN}");
        let mut profile = Profile::parse(HAND_WRITTEN).expect("Profil");
        // change one, remove one, add one
        profile.map.insert(
            MidiId::note(10, 36),
            bind("track.1.record").with_button(ButtonMode::Release),
        );
        profile.map.remove(MidiId::cc(1, 3));
        profile.map.insert(MidiId::note(10, 40), bind("track.2.record"));

        let out = profile.rewrite(HAND_WRITTEN);
        assert!(out.kept, "{}", out.text);
        let text = &out.text;
        assert!(text.starts_with("# Mein MPD218 - bitte nicht wegwerfen.\n"), "{text}");
        assert!(
            text.contains("  # --- untere Reihe ---\n  ch10.note37: {target: track.1.overdub}\n"),
            "{text}"
        );
        assert!(
            text.contains("  ch10.note36: {target: track.1.record, mode: release}   # der wichtigste"),
            "{text}"
        );
        assert!(text.contains("# --- Regler ---"), "die Gruppenueberschrift bleibt: {text}");
        assert!(!text.contains("cc3"), "{text}");
        assert!(text.contains("  ch10.note40: {target: track.2.record}"), "{text}");
        assert_eq!(Profile::parse(text).expect("liest sich zurueck"), profile);

        // Nothing changed -> byte for byte the same file.
        let same = Profile::parse(HAND_WRITTEN).expect("Profil");
        assert_eq!(same.rewrite(HAND_WRITTEN).text, HAND_WRITTEN);
    }

    #[test]
    fn the_shipped_example_survives_a_learned_pad_with_every_comment() {
        let original = std::fs::read_to_string(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../examples/midi-mpd218.yaml"
        ))
        .expect("Beispielprofil");
        let mut profile = Profile::parse(&original).expect("Profil");
        profile.map.insert(MidiId::note(10, 70), bind("global.tempo"));
        let out = profile.rewrite(&original);
        assert!(out.kept);
        let comments = |t: &str| t.lines().filter(|l| l.trim_start().starts_with('#')).count();
        assert_eq!(comments(&out.text), comments(&original));
        assert_eq!(out.text.lines().count(), original.lines().count() + 1);
    }

    #[test]
    fn a_nested_binding_is_written_afresh_but_keeps_the_header() {
        let nested = "# Kopf\ndevice: Pedal\nbindings:\n  ch1.cc64:\n    target: transport.next\n";
        let mut profile = Profile::parse(nested).expect("Profil");
        profile.map.insert(MidiId::cc(1, 65), bind("transport.stop_all"));
        let out = profile.rewrite(nested);
        assert!(!out.kept);
        assert!(out.text.starts_with("# Kopf\n"), "{}", out.text);
        assert_eq!(Profile::parse(&out.text).expect("liest sich"), profile);
    }

    #[test]
    fn saving_edits_in_place_and_backs_up_only_when_it_had_to_rewrite() {
        let dir = temp_dir("save");
        let path = dir.join("mpd218.yaml");
        std::fs::write(&path, HAND_WRITTEN).unwrap();
        let mut store = ProfileStore::new(&dir);
        assert_eq!(store.scan(), vec![StoreChange::Added(path.clone())]);
        let entry = store.get_mut(&path).unwrap();
        entry
            .profile
            .as_mut()
            .unwrap()
            .map
            .insert(MidiId::note(10, 41), bind("track.2.play"));
        let saved = store.save(&path).expect("gespeichert");
        assert_eq!(saved.backup, None);
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.contains("bitte nicht wegwerfen") && text.contains("ch10.note41"), "{text}");
        assert!(store.scan().is_empty(), "das eigene Schreiben ist keine Aenderung von aussen");

        // nested file -> fresh text, original kept as .bak
        let nested = dir.join("pedal.yaml");
        std::fs::write(
            &nested,
            "device: Pedal\nbindings:\n  ch1.cc64:\n    target: transport.next\n",
        )
        .unwrap();
        store.scan();
        store
            .get_mut(&nested)
            .unwrap()
            .profile
            .as_mut()
            .unwrap()
            .map
            .remove(MidiId::cc(1, 64));
        let saved = store.save(&nested).expect("gespeichert");
        let backup = saved.backup.expect("Sicherung");
        assert!(std::fs::read_to_string(backup).unwrap().contains("target: transport.next"));
        assert!(store.scan().is_empty(), ".bak und .tmp sind keine Profile");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_broken_file_is_recognised_by_device_and_never_overwritten() {
        let dir = temp_dir("broken");
        let path = dir.join("mpd218.yaml");
        let broken =
            "# meine Notizen\ndevice: MPD218\nbindings:\n  ch10.note36: {target: track.1.recrod}\n";
        std::fs::write(&path, broken).unwrap();
        let mut store = ProfileStore::new(&dir);
        store.scan();
        let entry = store.get(&path).unwrap();
        assert_eq!(entry.device, "MPD218");
        assert!(entry.profile.is_none());
        assert!(entry.error.as_deref().unwrap().contains("recrod"));
        assert_eq!(store.best_for_port("MPD218"), Some(path.as_path()));
        let err = store.save(&path).expect_err("kaputt bleibt unangetastet");
        assert!(err.contains("untouched"), "{err}");
        assert_eq!(std::fs::read_to_string(&path).unwrap(), broken);
        // create() for that device hands back the broken file rather than a second profile
        assert_eq!(store.create("MPD218").unwrap(), path);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_file_edited_by_hand_is_reloaded_and_a_deleted_one_disappears() {
        let dir = temp_dir("reload");
        let mut store = ProfileStore::new(&dir);
        let path = store.create("nanoKONTROL2").expect("angelegt");
        assert!(path.exists());
        assert_eq!(store.get(&path).unwrap().len(), 0);
        // mtime resolution: make sure the edit is visibly later
        std::thread::sleep(std::time::Duration::from_millis(20));
        std::fs::write(
            &path,
            "device: nanoKONTROL2\nbindings:\n  ch1.cc0: {target: track.1.pan}\n",
        )
        .unwrap();
        assert_eq!(store.scan(), vec![StoreChange::Reloaded(path.clone())]);
        assert_eq!(store.get(&path).unwrap().len(), 1);
        std::fs::remove_file(&path).unwrap();
        assert_eq!(store.scan(), vec![StoreChange::Removed(path.clone())]);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_longest_device_name_wins_a_port() {
        let dir = temp_dir("match");
        let mut store = ProfileStore::new(&dir);
        let short = store.create("MPD").unwrap();
        let long = store.create("MPD218").unwrap();
        assert_ne!(short, long);
        assert_eq!(store.best_for_port("2- MPD218"), Some(long.as_path()));
        assert_eq!(store.best_for_port("Launchpad"), None);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
