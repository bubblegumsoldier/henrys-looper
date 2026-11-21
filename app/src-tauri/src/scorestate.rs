//! The plugin settings of a score live **next to the score file**: `The Hardest Part.yaml` has
//! `The Hardest Part.plugins.json` beside it.
//!
//! ```text
//!  The Hardest Part.yaml            mixer: welches Plugin wo sitzt (Ids, before, bypass)
//!  The Hardest Part.plugins.json    wie es eingestellt ist: Zustand je Spur und Slot
//! ```
//!
//! # Why a sidecar
//!
//! The score's `mixer:` names *which* plugin sits where; the plugin's own state is an opaque
//! chunk - kilobytes for an EQ, megabytes for a sampler - that has no business in hand-edited YAML.
//! It used to live only in the app's local files (`plugin-rack.json`, `instruments.json`), keyed by
//! track name: two scores with a track called "theme" overwrote each other's sounds, and a score
//! copied to another computer arrived without them. The sidecar travels with the score; it is
//! found by name, nothing in the YAML points at it.
//!
//! One JSON file rather than a folder of binary files: it can be replaced atomically (written to a
//! temporary file, then renamed), so a crash or a plugin that fails half-way never leaves a mix of
//! old and new states. The cost is base64 (a third more bytes) for large states.
//!
//! # When it is read
//!
//! When a score is loaded from a file ([`switch`]): its sidecar is read into [`Current::pending`],
//! and the plugin managers take the states from there as they put the score's plugins on the tracks
//! (`plugins::Manager::apply_score`, `instruments::Manager::apply_score`) - each state once, matched
//! by track, order and plugin id (a state is never given to a different plugin). What the sidecar
//! does not have falls back to today's local behaviour: the state the plugin last had on a track of
//! that name. So an existing setup keeps its sounds on the first load after the update, and the
//! next save writes them into the sidecar. Reload reads the sidecar again: what was tweaked since
//! the last save is gone, which is what Reload means ("as on disk").
//!
//! # When it is written
//!
//! Plugin states change inside the plugin's own window without the app noticing, so the states are
//! captured at moments that mean "keep this", never in the background:
//!
//! * when the loaded score is saved (⌘S, Save As - then next to the new file);
//! * when another score is loaded (the previous one's states, before they are replaced);
//! * when the app quits.
//!
//! Always merged with what the file already has: a plugin that does not hand over its state keeps
//! the state the file had for it, a plugin missing on this computer keeps its entry, and tracks the
//! running engine does not have are left alone. A score without a file (never saved) and the free
//! mode without a score keep the local behaviour only.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use serde::{Deserialize, Serialize};

use crate::logfile::log;

const VERSION: u32 = 1;

/// The whole sidecar.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Sidecar {
    pub version: u32,
    /// Track name -> its plugins.
    #[serde(default)]
    pub tracks: BTreeMap<String, TrackPlugins>,
}

/// What plays a track and what is in its chain.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct TrackPlugins {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub instrument: Option<SidePlugin>,
    /// In slot order, which is signal order.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub inserts: Vec<SidePlugin>,
}

impl TrackPlugins {
    fn is_empty(&self) -> bool {
        self.instrument.is_none() && self.inserts.is_empty()
    }
}

/// One plugin with its settings.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct SidePlugin {
    /// Guards against a mismatch: a state is only ever given to the plugin it came from.
    pub plugin_id: String,
    pub name: String,
    /// Insert slot (0-based); absent for an instrument.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub slot: Option<usize>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub bypass: bool,
    /// Place in the built-in chain, as in the score (`before: eq`); absent = after it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub before: Option<String>,
    /// The plugin's state chunk, base64. `None` when the plugin did not hand one over.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub state: Option<String>,
}

impl SidePlugin {
    /// The state as bytes, if there is one and it is valid.
    pub fn bytes(&self) -> Option<Vec<u8>> {
        self.state.as_deref().and_then(crate::plugins::decode_state)
    }
}

/// What the running plugin managers hold right now, by track name.
#[derive(Debug, Default)]
pub struct Captured {
    /// Every track of the running engine.
    pub tracks: Vec<String>,
    pub inserts: BTreeMap<String, Vec<SidePlugin>>,
    pub instruments: BTreeMap<String, SidePlugin>,
    /// Tracks whose score plugins could not be loaded here (`crate::pluginmix`): their entries
    /// in the file are kept as they are.
    pub inserts_missing: Vec<String>,
    pub instrument_missing: Vec<String>,
}

/// `The Hardest Part.yaml` -> `The Hardest Part.plugins.json`.
pub fn sidecar_path(score: &Path) -> PathBuf {
    score.with_extension("plugins.json")
}

/// The sidecar of `score`, if it has one. A file that cannot be read is reported and treated as
/// absent - and is not overwritten without a copy (see [`write`]).
pub fn read(score: &Path) -> Option<Sidecar> {
    let path = sidecar_path(score);
    let text = std::fs::read_to_string(&path).ok()?;
    match serde_json::from_str::<Sidecar>(&text) {
        Ok(sidecar) => Some(sidecar),
        Err(e) => {
            log!("{} is not readable and is ignored: {e}", path.display());
            None
        }
    }
}

/// Write `sidecar` next to `score`, atomically: a temporary file in the same folder, then a
/// rename over the old one. An existing file that does not parse is kept as `….plugins.json.bak`.
pub fn write(score: &Path, sidecar: &Sidecar) -> Result<PathBuf, String> {
    let path = sidecar_path(score);
    if let Ok(old) = std::fs::read_to_string(&path)
        && serde_json::from_str::<Sidecar>(&old).is_err()
    {
        let _ = std::fs::copy(&path, path.with_extension("json.bak"));
    }
    let text = serde_json::to_string_pretty(sidecar).map_err(|e| e.to_string())?;
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, text).map_err(|e| format!("{}: {e}", tmp.display()))?;
    std::fs::rename(&tmp, &path).map_err(|e| {
        let _ = std::fs::remove_file(&tmp);
        format!("{}: {e}", path.display())
    })?;
    Ok(path)
}

/// The file's new content: what the managers hold now for the engine's tracks, merged into what
/// the file had. Pure, so the rules can be tested without a plugin:
///
/// * a captured plugin without a state takes the file's state for the same plugin at the same
///   place (same slot for an insert) - a plugin that fails to deliver never erases its settings;
/// * a track whose score plugins are missing here keeps its file entry unchanged;
/// * tracks the running engine does not have are kept unchanged;
/// * a track with nothing on it any more loses its entry.
pub fn merge(old: Option<&Sidecar>, captured: &Captured) -> Sidecar {
    let mut out = old.cloned().unwrap_or_default();
    out.version = VERSION;
    for track in &captured.tracks {
        let before = out.tracks.get(track).cloned().unwrap_or_default();
        let mut entry = TrackPlugins::default();
        entry.inserts = if captured.inserts_missing.contains(track) {
            before.inserts.clone()
        } else {
            captured
                .inserts
                .get(track)
                .cloned()
                .unwrap_or_default()
                .into_iter()
                .map(|mut p| {
                    if p.state.is_none() {
                        p.state = before
                            .inserts
                            .iter()
                            .find(|o| o.plugin_id == p.plugin_id && o.slot == p.slot)
                            .and_then(|o| o.state.clone());
                    }
                    p
                })
                .collect()
        };
        entry.instrument = if captured.instrument_missing.contains(track) {
            before.instrument.clone()
        } else {
            captured.instruments.get(track).cloned().map(|mut p| {
                if p.state.is_none() {
                    p.state = before
                        .instrument
                        .as_ref()
                        .filter(|o| o.plugin_id == p.plugin_id)
                        .and_then(|o| o.state.clone());
                }
                p
            })
        };
        if entry.is_empty() {
            out.tracks.remove(track);
        } else {
            out.tracks.insert(track.clone(), entry);
        }
    }
    out
}

/// The states waiting to be given to the plugins of the score being loaded.
#[derive(Debug, Default)]
pub struct Current {
    /// The file of the loaded score; `None` without a score or for one that was never saved.
    pub score: Option<PathBuf>,
    /// From its sidecar, not yet handed out.
    pub pending: Sidecar,
}

static CURRENT: Mutex<Option<Current>> = Mutex::new(None);

/// Tests that use [`CURRENT`] (a process-wide static) take turns.
#[cfg(test)]
pub static TEST_LOCK: Mutex<()> = Mutex::new(());

fn with<T>(f: impl FnOnce(&mut Current) -> T) -> T {
    let mut guard = CURRENT.lock().unwrap_or_else(|e| e.into_inner());
    f(guard.get_or_insert_with(Current::default))
}

/// The loaded score's file, if it has one.
pub fn current_score() -> Option<PathBuf> {
    with(|c| c.score.clone())
}

/// The sidecar of the loaded score, if it exists on disk - for a recording to keep a copy.
pub fn current_sidecar() -> Option<PathBuf> {
    current_score().map(|s| sidecar_path(&s)).filter(|p| p.exists())
}

/// A score is about to be loaded from `score` (`None`: no file). The states of the previous
/// score are written first when it is another file; the new score's sidecar is read. Returns what
/// was current before, for [`restore`] when the load is refused. Not on the main thread.
pub fn switch(score: Option<PathBuf>) -> Option<PathBuf> {
    let previous = current_score();
    if let Some(old) = &previous
        && score.as_ref() != Some(old)
        && let Err(e) = save(old)
    {
        log!("Plugin settings of {} not saved: {e}", old.display());
    }
    let pending = score.as_deref().and_then(read).unwrap_or_default();
    if let Some(s) = &score {
        log!(
            "Score {}: plugin settings for {} track(s) from {}.",
            s.display(),
            pending.tracks.len(),
            sidecar_path(s).display()
        );
    }
    with(|c| {
        c.score = score;
        c.pending = pending;
    });
    previous
}

/// The loaded score was saved under `score` (Save As): from now on its settings belong there.
pub fn saved_as(score: PathBuf) {
    with(|c| c.score = Some(score));
}

/// The load was refused: the previous score stays the current one, nothing is pending.
pub fn restore(previous: Option<PathBuf>) {
    with(|c| {
        c.score = previous;
        c.pending = Sidecar::default();
    });
}

/// The load has been applied: what the managers did not take is dropped, so a later undo in the
/// `mixer:` section does not pull states from the file again. Runs on the main thread after the
/// managers' own work (same queue).
pub fn finish_load() {
    with(|c| c.pending = Sidecar::default());
}

/// The insert states the loaded score's sidecar has for `track`, handed out once.
pub fn take_inserts(track: &str) -> Vec<SidePlugin> {
    with(|c| {
        c.pending
            .tracks
            .get_mut(track)
            .map(|t| std::mem::take(&mut t.inserts))
            .unwrap_or_default()
    })
}

/// The instrument state the loaded score's sidecar has for `track`, handed out once.
pub fn take_instrument(track: &str) -> Option<SidePlugin> {
    with(|c| c.pending.tracks.get_mut(track).and_then(|t| t.instrument.take()))
}

/// For `wanted` plugin ids in order, the matching sidecar state of each: by order first (the
/// k-th plugin of the score gets the k-th of the file if it is the same plugin), then the first
/// unused one with the same id. Never a state of another plugin.
pub fn match_states(wanted: &[&str], side: &[SidePlugin]) -> Vec<Option<Vec<u8>>> {
    let mut used = vec![false; side.len()];
    wanted
        .iter()
        .enumerate()
        .map(|(k, id)| {
            let at = if side.get(k).is_some_and(|s| s.plugin_id == *id) && !used[k] {
                Some(k)
            } else {
                (0..side.len()).find(|i| !used[*i] && side[*i].plugin_id == *id)
            }?;
            used[at] = true;
            side[at].bytes()
        })
        .collect()
}

/// Capture the plugin managers and write the sidecar of `score`. Not on the main thread.
pub fn save(score: &Path) -> Result<PathBuf, String> {
    let captured = capture()?;
    write_captured(score, &captured)
}

/// [`save`] for the loaded score, on the main thread (the way out of the app).
pub fn save_current_on_main() {
    let Some(score) = current_score() else {
        return;
    };
    let captured = capture_on_main();
    match write_captured(&score, &captured) {
        Ok(path) => log!("Plugin settings saved to {}.", path.display()),
        Err(e) => log!("Plugin settings of {} not saved: {e}", score.display()),
    }
}

fn write_captured(score: &Path, captured: &Captured) -> Result<PathBuf, String> {
    let old = read(score);
    let merged = merge(old.as_ref(), captured);
    if old.is_none() && merged.tracks.is_empty() {
        // Nothing to keep and no file yet: no empty file next to every score.
        return Ok(sidecar_path(score));
    }
    write(score, &merged)
}

fn capture() -> Result<Captured, String> {
    let (tracks, inserts) = crate::plugins::capture()?;
    let instruments = crate::instruments::capture()?;
    Ok(finish_capture(tracks, inserts, instruments))
}

fn capture_on_main() -> Captured {
    let (tracks, inserts) = crate::plugins::capture_here();
    let instruments = crate::instruments::capture_here();
    finish_capture(tracks, inserts, instruments)
}

fn finish_capture(
    tracks: Vec<String>,
    inserts: BTreeMap<String, Vec<SidePlugin>>,
    instruments: BTreeMap<String, SidePlugin>,
) -> Captured {
    let (inserts_missing, instrument_missing) = tracks
        .iter()
        .fold((Vec::new(), Vec::new()), |(mut i, mut n), t| {
            let (ins, inst) = crate::pluginmix::missing(t);
            if ins {
                i.push(t.clone());
            }
            if inst {
                n.push(t.clone());
            }
            (i, n)
        });
    Captured {
        tracks,
        inserts,
        instruments,
        inserts_missing,
        instrument_missing,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn plugin(id: &str, slot: Option<usize>, state: Option<&str>) -> SidePlugin {
        SidePlugin {
            plugin_id: id.into(),
            name: id.to_uppercase(),
            slot,
            state: state.map(str::to_string),
            ..SidePlugin::default()
        }
    }

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "looper-scorestate-{name}-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn the_sidecar_sits_next_to_the_score_by_name() {
        assert_eq!(
            sidecar_path(Path::new("/x/The Hardest Part.yaml")),
            PathBuf::from("/x/The Hardest Part.plugins.json")
        );
        assert_eq!(sidecar_path(Path::new("/x/song.v2.yml")), PathBuf::from("/x/song.v2.plugins.json"));
    }

    /// Positions, bypass, states of inserts and an instrument: written and read back unchanged,
    /// atomically (no temporary file left), and an unreadable old file is kept as a copy.
    #[test]
    fn a_sidecar_round_trips_and_a_broken_one_is_kept_aside() {
        let dir = scratch("roundtrip");
        let score = dir.join("Song.yaml");
        let mut ir = plugin("vst3:IRDX", Some(0), Some("AAEC"));
        ir.before = Some("octaver".into());
        let mut delay = plugin("au:delay", Some(1), Some("AQID"));
        delay.bypass = true;
        let mut sidecar = Sidecar {
            version: VERSION,
            ..Sidecar::default()
        };
        sidecar.tracks.insert(
            "guitar".into(),
            TrackPlugins {
                instrument: None,
                inserts: vec![ir, delay],
            },
        );
        sidecar.tracks.insert(
            "keys".into(),
            TrackPlugins {
                instrument: Some(plugin("au:aumu:dls :appl", None, Some("BBBB"))),
                inserts: Vec::new(),
            },
        );
        let path = write(&score, &sidecar).unwrap();
        assert_eq!(path, dir.join("Song.plugins.json"));
        assert_eq!(read(&score), Some(sidecar.clone()));
        assert!(!dir.join("Song.plugins.json.tmp").exists());
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.contains("\"before\": \"octaver\""), "{text}");

        std::fs::write(&path, "{ not json").unwrap();
        assert_eq!(read(&score), None, "unreadable counts as absent");
        write(&score, &sidecar).unwrap();
        assert_eq!(std::fs::read_to_string(dir.join("Song.plugins.json.bak")).unwrap(), "{ not json");
        assert_eq!(read(&score), Some(sidecar));
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The merge never loses settings: a plugin without a state keeps the file's, a plugin missing
    /// here keeps its entry, tracks of other setups stay, an emptied track goes.
    #[test]
    fn merging_never_erases_settings_it_could_not_read() {
        let mut old = Sidecar::default();
        old.tracks.insert(
            "voice".into(),
            TrackPlugins {
                instrument: None,
                inserts: vec![plugin("vst3:deess", Some(0), Some("OLD1")), plugin("au:comp", Some(1), Some("OLD2"))],
            },
        );
        old.tracks.insert("bass".into(), TrackPlugins { instrument: None, inserts: vec![plugin("vst3:amp", Some(0), Some("AMP"))] });
        old.tracks.insert("other".into(), TrackPlugins { instrument: None, inserts: vec![plugin("x", Some(0), Some("X"))] });
        old.tracks.insert("empty".into(), TrackPlugins { instrument: None, inserts: vec![plugin("y", Some(0), Some("Y"))] });

        let mut captured = Captured {
            tracks: vec!["voice".into(), "bass".into(), "empty".into(), "keys".into()],
            inserts_missing: vec!["bass".into()],
            ..Captured::default()
        };
        captured.inserts.insert(
            "voice".into(),
            // The de-esser delivered a new state, the compressor none at all.
            vec![plugin("vst3:deess", Some(0), Some("NEW1")), plugin("au:comp", Some(1), None)],
        );
        captured.instruments.insert("keys".into(), plugin("au:dls", None, Some("KEYS")));

        let merged = merge(Some(&old), &captured);
        let voice = &merged.tracks["voice"].inserts;
        assert_eq!(voice[0].state.as_deref(), Some("NEW1"));
        assert_eq!(voice[1].state.as_deref(), Some("OLD2"), "no state delivered: the file's stays");
        assert_eq!(merged.tracks["bass"], old.tracks["bass"], "missing here: kept as it was");
        assert_eq!(merged.tracks["other"], old.tracks["other"], "not a track of this engine");
        assert!(!merged.tracks.contains_key("empty"), "nothing on it any more");
        assert_eq!(merged.tracks["keys"].instrument.as_ref().unwrap().state.as_deref(), Some("KEYS"));
        assert_eq!(merged.version, VERSION);
        // Another plugin in the same slot does not inherit a state it did not produce.
        let mut other = Captured { tracks: vec!["voice".into()], ..Captured::default() };
        other.inserts.insert("voice".into(), vec![plugin("vst3:other", Some(0), None)]);
        assert_eq!(merge(Some(&old), &other).tracks["voice"].inserts[0].state, None);
    }

    /// A state goes to the plugin it came from - by order, then by id - never to another plugin.
    #[test]
    fn states_are_matched_by_order_and_plugin_id() {
        let side = vec![
            plugin("a", Some(0), Some("AAEC")),
            plugin("b", Some(1), Some("AQID")),
            plugin("a", Some(2), Some("AgME")),
        ];
        let bytes = |s: &str| crate::plugins::decode_state(s);
        assert_eq!(match_states(&["a", "b", "a"], &side), vec![bytes("AAEC"), bytes("AQID"), bytes("AgME")]);
        // The score dropped the first plugin: the second `a` takes the next unused `a`.
        assert_eq!(match_states(&["b", "a"], &side), vec![bytes("AQID"), bytes("AAEC")]);
        assert_eq!(match_states(&["c"], &side), vec![None]);
        assert_eq!(match_states(&[], &side), Vec::<Option<Vec<u8>>>::new());
    }

    /// Loading a score reads its sidecar into the pending states; each is handed out once; a
    /// refused load leaves nothing behind; the end of a load drops what nobody took.
    #[test]
    fn pending_states_are_handed_out_once() {
        let _turn = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let dir = scratch("pending");
        let score = dir.join("A.yaml");
        let mut sidecar = Sidecar::default();
        sidecar.tracks.insert(
            "keys".into(),
            TrackPlugins {
                instrument: Some(plugin("dls", None, Some("AAEC"))),
                inserts: vec![plugin("dly", Some(0), Some("AQID"))],
            },
        );
        write(&score, &sidecar).unwrap();
        // `switch` would save the previous score through the plugin managers; start clean.
        restore(None);
        let previous = switch(Some(score.clone()));
        assert_eq!(previous, None);
        assert_eq!(current_score(), Some(score.clone()));
        assert_eq!(current_sidecar(), Some(sidecar_path(&score)));
        assert_eq!(take_inserts("keys").len(), 1);
        assert!(take_inserts("keys").is_empty(), "once");
        assert!(take_instrument("keys").is_some());
        assert!(take_instrument("keys").is_none());
        restore(None);
        assert_eq!(current_score(), None);
        assert!(take_instrument("keys").is_none());
        finish_load();
        let _ = std::fs::remove_dir_all(&dir);
    }
}
