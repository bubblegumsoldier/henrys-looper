//! Score files on disk: open, save, the recent list, and the template behind "New".
//!
//! # Why our own commands and not the FS plugin
//!
//! The native pickers are Tauri's dialog plugin (permissions in `capabilities/default.json`). The
//! file *contents* go through the four commands below instead of `tauri-plugin-fs`, for one
//! reason: the recent list. The FS plugin only lets the web view touch paths in its scope; a path
//! a picker returned is added to that scope for the running session, but the recent list is
//! reopened in the *next* session, which would need a blanket scope such as `$HOME/**`. That is a
//! far wider door than four commands that only ever read or write one `.yaml`/`.yml` file of at
//! most a megabyte, as UTF-8 text.
//!
//! # Format and place
//!
//! A score file is plain YAML with the extension `.yaml` (`.yml` is read too) - it opens in any
//! editor, and the examples in the repository are named that way. "Save as" proposes
//! `~/Documents/Henrys Looper/<title>.yaml`; the folder is created when it is first proposed.
//! The recent list (ten entries) lives in the app's config folder as `recent-scores.json`.
//!
//! Writing is atomic: the text goes into a temporary file beside the target, which is then renamed
//! over it, so a crash in the middle of a save never leaves half a score behind.

use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// Largest file that is accepted as a score. Real scores are a few kilobytes.
const MAX_BYTES: u64 = 1024 * 1024;
const MAX_RECENT: usize = 10;
const RECENT_FILE: &str = "recent-scores.json";
/// Folder under the user's documents that "Save as" proposes.
pub const DEFAULT_FOLDER: &str = "Henrys Looper";

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct RecentFile {
    pub path: String,
    /// File name without folder, ready to print.
    pub name: String,
    /// False when the file has gone since it was last used.
    pub exists: bool,
}

#[derive(Serialize, Clone, Debug)]
pub struct FilesInfo {
    /// The folder "Save as" proposes, already created. `null` if there is no documents folder.
    pub default_dir: Option<String>,
    /// Newest first.
    pub recent: Vec<RecentFile>,
}

#[derive(Serialize, Clone, Debug)]
pub struct OpenedFile {
    pub path: String,
    pub name: String,
    pub yaml: String,
    pub recent: Vec<RecentFile>,
}

#[derive(Serialize, Clone, Debug)]
pub struct SavedFile {
    /// The path actually written - with `.yaml` appended when the name had no extension.
    pub path: String,
    pub name: String,
    pub recent: Vec<RecentFile>,
}

fn name_of(path: &Path) -> String {
    path.file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.display().to_string())
}

fn is_score_extension(path: &Path) -> bool {
    matches!(
        path.extension().and_then(|e| e.to_str()).map(str::to_ascii_lowercase).as_deref(),
        Some("yaml" | "yml")
    )
}

/// Read a score file. Refuses anything that is not a `.yaml`/`.yml` file of sensible size.
pub fn read_score_file(path: &Path) -> Result<String, String> {
    if !is_score_extension(path) {
        return Err(format!(
            "\"{}\" is not a score file - scores end in .yaml.",
            name_of(path)
        ));
    }
    let meta = fs::metadata(path)
        .map_err(|e| format!("Cannot open \"{}\": {e}", path.display()))?;
    if !meta.is_file() {
        return Err(format!("\"{}\" is not a file.", path.display()));
    }
    if meta.len() > MAX_BYTES {
        return Err(format!(
            "\"{}\" is {} KB - too large for a score (at most 1024 KB).",
            name_of(path),
            meta.len() / 1024
        ));
    }
    let bytes = fs::read(path).map_err(|e| format!("Cannot read \"{}\": {e}", path.display()))?;
    String::from_utf8(bytes)
        .map_err(|_| format!("\"{}\" is not a text file (not UTF-8).", name_of(path)))
}

/// Where a save really goes: a name without extension gets `.yaml`; any other extension is refused
/// rather than silently replaced.
pub fn target_path(path: &Path) -> Result<PathBuf, String> {
    match path.extension() {
        None => Ok(path.with_extension("yaml")),
        Some(_) if is_score_extension(path) => Ok(path.to_path_buf()),
        Some(_) => Err(format!(
            "\"{}\": a score is saved as .yaml.",
            name_of(path)
        )),
    }
}

/// Write atomically: temporary file in the same folder, then rename over the target.
pub fn write_score_file(path: &Path, yaml: &str) -> Result<PathBuf, String> {
    let target = target_path(path)?;
    let dir = target
        .parent()
        .filter(|d| !d.as_os_str().is_empty())
        .ok_or_else(|| format!("\"{}\" has no folder.", target.display()))?;
    if !dir.is_dir() {
        return Err(format!("The folder \"{}\" does not exist.", dir.display()));
    }
    let tmp = dir.join(format!(".{}.saving", name_of(&target)));
    fs::write(&tmp, yaml.as_bytes())
        .map_err(|e| format!("Cannot save \"{}\": {e}", target.display()))?;
    fs::rename(&tmp, &target).map_err(|e| {
        let _ = fs::remove_file(&tmp);
        format!("Cannot save \"{}\": {e}", target.display())
    })?;
    Ok(target)
}

/// The recent list as stored, newest first, with `exists` refreshed.
pub fn load_recent(config_dir: &Path) -> Vec<RecentFile> {
    let stored: Vec<String> = fs::read_to_string(config_dir.join(RECENT_FILE))
        .ok()
        .and_then(|text| serde_json::from_str(&text).ok())
        .unwrap_or_default();
    stored
        .into_iter()
        .take(MAX_RECENT)
        .map(|path| {
            let p = PathBuf::from(&path);
            RecentFile {
                name: name_of(&p),
                exists: p.is_file(),
                path,
            }
        })
        .collect()
}

/// Put `path` at the top of the recent list and store it. A failure to store is not an error for
/// the open or save that triggered it; the list is a convenience.
pub fn remember(config_dir: &Path, path: &Path) -> Vec<RecentFile> {
    let path_text = path.display().to_string();
    let mut list: Vec<String> = load_recent(config_dir)
        .into_iter()
        .map(|r| r.path)
        .filter(|p| *p != path_text)
        .collect();
    list.insert(0, path_text);
    list.truncate(MAX_RECENT);
    if fs::create_dir_all(config_dir).is_ok()
        && let Ok(json) = serde_json::to_string_pretty(&list)
    {
        let _ = fs::write(config_dir.join(RECENT_FILE), json);
    }
    load_recent(config_dir)
}

/// Drop one entry - for a recent file that has gone.
pub fn forget(config_dir: &Path, path: &str) -> Vec<RecentFile> {
    let list: Vec<String> = load_recent(config_dir)
        .into_iter()
        .map(|r| r.path)
        .filter(|p| p != path)
        .collect();
    if let Ok(json) = serde_json::to_string_pretty(&list) {
        let _ = fs::write(config_dir.join(RECENT_FILE), json);
    }
    load_recent(config_dir)
}

/// The proposed folder, created if needed.
pub fn default_dir(documents: Option<PathBuf>) -> Option<PathBuf> {
    let dir = documents?.join(DEFAULT_FOLDER);
    fs::create_dir_all(&dir).ok()?;
    Some(dir)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "henrys-looper-files-{name}-{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn a_score_is_written_and_read_back_unchanged() {
        let dir = scratch("roundtrip");
        let text = "title: Ä Ö\nbpm: 100\n";
        let written = write_score_file(&dir.join("song.yaml"), text).unwrap();
        assert_eq!(read_score_file(&written).unwrap(), text);
        assert!(
            fs::read_dir(&dir).unwrap().count() == 1,
            "keine temporaere Datei bleibt liegen"
        );
        // Overwriting works too.
        write_score_file(&written, "bpm: 120\n").unwrap();
        assert_eq!(read_score_file(&written).unwrap(), "bpm: 120\n");
    }

    #[test]
    fn a_name_without_extension_gets_yaml_and_a_foreign_one_is_refused() {
        let dir = scratch("ext");
        let written = write_score_file(&dir.join("song"), "bpm: 1\n").unwrap();
        assert_eq!(written, dir.join("song.yaml"));
        assert!(write_score_file(&dir.join("song.txt"), "x").is_err());
        assert!(target_path(Path::new("/a/b.YML")).is_ok());
    }

    #[test]
    fn only_sensible_score_files_are_read() {
        let dir = scratch("read");
        fs::write(dir.join("notes.txt"), "x").unwrap();
        assert!(read_score_file(&dir.join("notes.txt")).unwrap_err().contains(".yaml"));
        fs::write(dir.join("big.yaml"), vec![b'#'; (MAX_BYTES + 1) as usize]).unwrap();
        assert!(read_score_file(&dir.join("big.yaml")).unwrap_err().contains("too large"));
        fs::write(dir.join("bin.yaml"), [0xff, 0xfe, 0x00]).unwrap();
        assert!(read_score_file(&dir.join("bin.yaml")).unwrap_err().contains("UTF-8"));
        assert!(read_score_file(&dir.join("gone.yaml")).is_err());
    }

    #[test]
    fn the_recent_list_is_newest_first_without_duplicates_and_bounded() {
        let config = scratch("recent");
        let files = scratch("recent-files");
        for n in 0..12 {
            let path = files.join(format!("s{n}.yaml"));
            fs::write(&path, "bpm: 1\n").unwrap();
            remember(&config, &path);
        }
        let again = files.join("s3.yaml");
        let list = remember(&config, &again);
        assert_eq!(list.len(), MAX_RECENT);
        assert_eq!(list[0].name, "s3.yaml");
        assert_eq!(list[1].name, "s11.yaml");
        assert_eq!(list.iter().filter(|r| r.name == "s3.yaml").count(), 1);
        assert!(list.iter().all(|r| r.exists));

        fs::remove_file(files.join("s11.yaml")).unwrap();
        let list = load_recent(&config);
        assert!(!list[1].exists, "eine verschwundene Datei wird als solche gemeldet");
        let list = forget(&config, &list[1].path.clone());
        assert!(list.iter().all(|r| r.name != "s11.yaml"));
    }

    #[test]
    fn the_default_folder_is_created_under_documents() {
        let docs = scratch("docs");
        let dir = default_dir(Some(docs.clone())).unwrap();
        assert_eq!(dir, docs.join(DEFAULT_FOLDER));
        assert!(dir.is_dir());
        assert_eq!(default_dir(None), None);
    }
}
