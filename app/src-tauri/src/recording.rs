//! Recording every run to disk - the app's half of `looper_engine::capture`.
//!
//! ```text
//!  Host-Thread (Session)                               Schreib-Thread je Mitschnitt
//!  ┌───────────────────────────────────────┐  Capture  ┌──────────────────────────┐
//!  │ Start der Partitur / der Uhr: begin   │ ────────► │ WAVs, score.yaml,        │
//!  │ Sektionswechsel: marker               │           │ session.json             │
//!  │ Ende / Stop all / Uhr-Stopp: stop     │           └──────────────────────────┘
//!  └───────────────────────────────────────┘                 │ fertig
//!            │ looper://recording (laufend, fertig)          ▼
//!            ▼                                        FINISHING (poll im Host-Thread)
//!         Web-View
//! ```
//!
//! **When a recording runs.** A score run is recorded from bar 1 of its first section (the count-in
//! is not in the files) until "End of the score" or "Stop all", plus a tail until silence or at most
//! six seconds. Free play is recorded from the moment the clock starts (that is bar 1) until it is
//! stopped. Stopping the engine, loading a score or rebuilding the engine ends a recording on the
//! spot. A run shorter than five seconds or one in which no take was recorded is thrown away.
//!
//! **Where.** `~/Documents/Henrys Looper/Recordings/<YYYY-MM-DD HH.MM.SS> <title>/`, or the folder
//! chosen in Setup. The settings live in the app's config folder as `recording.json` (honouring
//! `HENRYS_LOOPER_CONFIG_DIR`, like the MIDI profiles), so a test never touches the musician's.

use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Emitter};

use looper_engine::capture::run::{Mode, Outcome, RunInfo, RunTrack, MIN_SECONDS, Recording, SectionEvent};
use looper_engine::capture::session::{self, SESSION_FILE};
use looper_engine::capture::CapturePort;
use looper_engine::engine::frame::Channels;
use looper_engine::engine::timeline::Timeline;

use crate::logfile::log;

/// Event carrying a [`RecordingEvent`].
pub const RECORDING_EVENT: &str = "looper://recording";
/// Below this much free space the Setup page warns.
pub const LOW_SPACE_BYTES: u64 = 2 * 1024 * 1024 * 1024;
const SETTINGS_FILE: &str = "recording.json";
const RECORDINGS_FOLDER: &str = "Recordings";
/// How often a running recording reports its length.
const PROGRESS_INTERVAL: Duration = Duration::from_millis(250);
/// Entries in the list of recent recordings.
const LIST_LIMIT: usize = 20;

// ---------------------------------------------------------------------------------------------
// Settings
// ---------------------------------------------------------------------------------------------

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(default)]
pub struct Settings {
    /// "Record every run to disk".
    pub enabled: bool,
    /// "Also write dry stems".
    pub dry: bool,
    /// Chosen folder; `None` is the default under Documents.
    pub folder: Option<String>,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            enabled: true,
            dry: true,
            folder: None,
        }
    }
}

/// What the Setup page shows.
#[derive(Serialize, Clone, Debug)]
pub struct SettingsView {
    pub enabled: bool,
    pub dry: bool,
    /// The folder in use - the chosen one or the default.
    pub folder: String,
    /// True when no folder was chosen.
    pub default_folder: bool,
    /// Free bytes on the disk the folder is on; `null` when unknown.
    pub free_bytes: Option<u64>,
    /// True below [`LOW_SPACE_BYTES`].
    pub low_space: bool,
}

struct Places {
    config_dir: PathBuf,
    documents: Option<PathBuf>,
}

static PLACES: OnceLock<Places> = OnceLock::new();
static SETTINGS: Mutex<Option<Settings>> = Mutex::new(None);
/// Recordings whose writer is still finishing - kept here rather than in the session, so a
/// recording ended by stopping the engine still reports back.
static FINISHING: Mutex<Vec<Recording>> = Mutex::new(Vec::new());

/// Called once from `setup`.
pub fn init(config_dir: PathBuf, documents: Option<PathBuf>) {
    let settings = std::fs::read_to_string(config_dir.join(SETTINGS_FILE))
        .ok()
        .and_then(|text| serde_json::from_str::<Settings>(&text).ok())
        .unwrap_or_default();
    let _ = PLACES.set(Places {
        config_dir,
        documents,
    });
    if let Ok(mut slot) = SETTINGS.lock() {
        *slot = Some(settings);
    }
}

pub fn settings() -> Settings {
    SETTINGS
        .lock()
        .ok()
        .and_then(|s| s.clone())
        .unwrap_or_default()
}

/// The default folder: `~/Documents/Henrys Looper/Recordings`.
fn default_root() -> PathBuf {
    let docs = PLACES
        .get()
        .and_then(|p| p.documents.clone())
        .unwrap_or_else(std::env::temp_dir);
    docs.join(crate::files::DEFAULT_FOLDER).join(RECORDINGS_FOLDER)
}

/// The folder recordings go into right now.
pub fn root() -> PathBuf {
    match settings().folder {
        Some(folder) if !folder.trim().is_empty() => PathBuf::from(folder),
        _ => default_root(),
    }
}

pub fn view() -> SettingsView {
    let s = settings();
    let folder = root();
    // The free space of the folder, or of the nearest parent that exists yet.
    let free = folder.ancestors().find(|p| p.exists()).and_then(free_bytes);
    SettingsView {
        enabled: s.enabled,
        dry: s.dry,
        folder: folder.display().to_string(),
        default_folder: s.folder.is_none(),
        free_bytes: free,
        low_space: free.is_some_and(|b| b < LOW_SPACE_BYTES),
    }
}

pub fn set(settings: Settings) -> Result<SettingsView, String> {
    let settings = Settings {
        folder: settings.folder.filter(|f| !f.trim().is_empty()),
        ..settings
    };
    if let Some(folder) = &settings.folder {
        let path = Path::new(folder);
        if !path.is_absolute() {
            return Err(format!("\"{folder}\" is not a full path to a folder."));
        }
        std::fs::create_dir_all(path)
            .map_err(|e| format!("Cannot use \"{folder}\" for recordings: {e}"))?;
    }
    if let Some(places) = PLACES.get() {
        std::fs::create_dir_all(&places.config_dir).map_err(|e| e.to_string())?;
        let json = serde_json::to_string_pretty(&settings).map_err(|e| e.to_string())?;
        std::fs::write(places.config_dir.join(SETTINGS_FILE), json)
            .map_err(|e| format!("Cannot save the recording settings: {e}"))?;
    }
    if let Ok(mut slot) = SETTINGS.lock() {
        *slot = Some(settings);
    }
    Ok(view())
}

#[cfg(unix)]
pub fn free_bytes(path: &Path) -> Option<u64> {
    use std::os::unix::ffi::OsStrExt;
    let c = std::ffi::CString::new(path.as_os_str().as_bytes()).ok()?;
    let mut stat: libc::statvfs = unsafe { std::mem::zeroed() };
    // SAFETY: a valid NUL-terminated path and a zeroed struct of the right type.
    let rc = unsafe { libc::statvfs(c.as_ptr(), &mut stat) };
    (rc == 0).then(|| stat.f_bavail as u64 * stat.f_frsize as u64)
}

#[cfg(not(unix))]
pub fn free_bytes(_path: &Path) -> Option<u64> {
    None
}

/// Local wall-clock time `offset` from now, as `(folder stamp, display stamp)`.
fn local_stamp(offset: Duration) -> (String, String) {
    let when = std::time::SystemTime::now() + offset;
    let secs = when
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    #[cfg(unix)]
    {
        let t: libc::time_t = secs as libc::time_t;
        let mut tm: libc::tm = unsafe { std::mem::zeroed() };
        // SAFETY: both pointers are valid for the duration of the call.
        if !unsafe { libc::localtime_r(&t, &mut tm) }.is_null() {
            return session::stamp(
                tm.tm_year + 1900,
                (tm.tm_mon + 1) as u32,
                tm.tm_mday as u32,
                tm.tm_hour as u32,
                tm.tm_min as u32,
                tm.tm_sec as u32,
            );
        }
    }
    // UTC fallback, civil-from-days (Howard Hinnant).
    let days = secs.div_euclid(86_400);
    let rem = secs.rem_euclid(86_400) as u32;
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    let y = (yoe + era * 400 + if m <= 2 { 1 } else { 0 }) as i32;
    session::stamp(y, m, d, rem / 3600, rem / 60 % 60, rem % 60)
}

// ---------------------------------------------------------------------------------------------
// Events
// ---------------------------------------------------------------------------------------------

#[derive(Serialize, Clone, Debug, Default)]
pub struct RecordingEvent {
    /// The recording in progress, or `null`.
    pub active: Option<ActiveView>,
    /// Set once, when a recording has been finished (kept or thrown away).
    pub finished: Option<FinishedView>,
}

#[derive(Serialize, Clone, Debug)]
pub struct ActiveView {
    pub name: String,
    pub folder: String,
    pub seconds: f64,
    /// True once the run has ended and only the tail is still written.
    pub stopping: bool,
    pub mode: String,
    /// Incidents of the disk not keeping up so far.
    pub overflows: u64,
}

#[derive(Serialize, Clone, Debug)]
pub struct FinishedView {
    pub name: String,
    pub folder: String,
    pub kept: bool,
    pub reason: Option<String>,
    pub seconds: f64,
    pub bytes: u64,
    pub overflows: u64,
    pub silent_frames: u64,
    pub error: Option<String>,
    /// One sentence for the notice.
    pub text: String,
}

fn finished_view(o: &Outcome) -> FinishedView {
    let text = if o.kept {
        let mut text = format!(
            "Recording saved: \"{}\" ({}, {}).",
            o.name,
            duration(o.seconds),
            size(o.bytes)
        );
        if o.dropouts.overflow_incidents > 0 {
            text.push_str(&format!(
                " The disk fell behind {}x; {} filled with silence in every file.",
                o.dropouts.overflow_incidents,
                duration(o.dropouts.frames_filled_with_silence as f64 * o.seconds / o.frames.max(1) as f64)
            ));
        }
        text
    } else {
        format!(
            "Recording discarded ({}).",
            o.reason.as_deref().unwrap_or("nothing to keep")
        )
    };
    let text = match &o.error {
        Some(e) => format!("{text} Disk error: {e}"),
        None => text,
    };
    FinishedView {
        name: o.name.clone(),
        folder: o.folder.display().to_string(),
        kept: o.kept,
        reason: o.reason.clone(),
        seconds: o.seconds,
        bytes: o.bytes,
        overflows: o.dropouts.overflow_incidents,
        silent_frames: o.dropouts.frames_filled_with_silence,
        error: o.error.clone(),
        text,
    }
}

pub fn duration(seconds: f64) -> String {
    let s = seconds.max(0.0).round() as u64;
    format!("{}:{:02}", s / 60, s % 60)
}

pub fn size(bytes: u64) -> String {
    let mb = bytes as f64 / 1_048_576.0;
    if mb >= 1024.0 {
        format!("{:.1} GB", mb / 1024.0)
    } else {
        format!("{mb:.0} MB")
    }
}

/// Report every recording whose writer has finished. Called on every turn of the host thread.
pub fn poll(app: &AppHandle) {
    let Ok(mut list) = FINISHING.lock() else {
        return;
    };
    let mut i = 0;
    while i < list.len() {
        if let Some(outcome) = list[i].poll().cloned() {
            let view = finished_view(&outcome);
            log!("Recording: {}", view.text);
            let _ = app.emit(
                RECORDING_EVENT,
                RecordingEvent {
                    active: None,
                    finished: Some(view),
                },
            );
            list.swap_remove(i);
        } else {
            i += 1;
        }
    }
}

// ---------------------------------------------------------------------------------------------
// The session's recorder
// ---------------------------------------------------------------------------------------------

/// What the session knows about its tracks and grid when a recording begins.
pub struct Context<'a> {
    pub timeline: &'a Timeline,
    pub tracks: Vec<RunTrack>,
    /// Engine position right now, for the wall-clock time of frame 0.
    pub now: u64,
    pub sample_rate: u32,
}

pub struct Recorder {
    port: CapturePort,
    active: Option<Recording>,
    mode: Mode,
    max_block: usize,
    last_progress: Instant,
    /// Engine dropouts counted before the recording began.
    dropout_base: u64,
    /// A recording ended since the last turn; the next one tells the web view.
    announce_idle: bool,
}

impl Recorder {
    pub fn new(port: CapturePort, max_block: usize) -> Self {
        Self {
            port,
            active: None,
            mode: Mode::Free,
            max_block,
            last_progress: Instant::now(),
            dropout_base: 0,
            announce_idle: false,
        }
    }

    pub fn active(&self) -> bool {
        self.active.is_some()
    }

    /// Begin a recording with frame 0 at `start` (bar index `start_bar`). Ends one still running.
    /// Returns a sentence when something is worth saying (it could not start), `None` otherwise.
    #[allow(clippy::too_many_arguments)]
    pub fn begin(
        &mut self,
        ctx: Context<'_>,
        mode: Mode,
        title: &str,
        start: u64,
        start_bar: u64,
        count_in_bars: u32,
        score_yaml: Option<String>,
        dropouts: u64,
    ) -> Option<String> {
        let settings = settings();
        if !settings.enabled {
            return None;
        }
        self.end(start.min(ctx.now));
        let rate = ctx.sample_rate.max(1);
        let ahead = Duration::from_secs_f64(start.saturating_sub(ctx.now) as f64 / rate as f64);
        let (folder_stamp, display) = local_stamp(ahead);
        let folder = root().join(session::folder_name(&folder_stamp, title));
        let signature = ctx.timeline.signature();
        let info = RunInfo {
            title: title.to_string(),
            mode,
            folder,
            started_at: display,
            sample_rate: rate,
            bpm: ctx.timeline.bpm(),
            beats_per_bar: signature.beats_per_bar,
            beat_unit: signature.beat_unit,
            count_in_bars,
            start,
            start_bar,
            tracks: ctx.tracks,
            dry: settings.dry,
            max_block: self.max_block,
            score_yaml,
            score_path: None,
            min_seconds: MIN_SECONDS,
            queue_seconds: looper_engine::capture::QUEUE_SECONDS,
        };
        match Recording::start(info, &mut self.port) {
            Ok(rec) => {
                log!("Recording to {}", rec.folder().display());
                // The score's plugin settings as last saved, beside its `score.yaml`: the
                // recording documents the sounds it was played with. A copy of a file, nothing
                // asked of the plugins here.
                if mode == Mode::Score
                    && let Some(side) = crate::scorestate::current_sidecar()
                    && let Err(e) = std::fs::copy(&side, rec.folder().join("score.plugins.json"))
                {
                    log!("Plugin settings not copied into the recording: {e}");
                }
                self.mode = mode;
                self.dropout_base = dropouts;
                self.active = Some(rec);
                self.last_progress = Instant::now() - PROGRESS_INTERVAL;
                None
            }
            Err(e) => {
                log!("Recording not started: {e}");
                Some(format!("Recording to disk did not start: {e}"))
            }
        }
    }

    /// End the running recording at engine position `at` (plus the tail). It finishes on its own
    /// and reports through [`poll`].
    pub fn end(&mut self, at: u64) {
        if let Some(rec) = self.active.take() {
            rec.stop(at);
            self.announce_idle = true;
            if let Ok(mut list) = FINISHING.lock() {
                list.push(rec);
            }
        }
    }

    /// The engine is going away: whatever runs ends where the engine stops - the writer finishes
    /// with what reached it.
    pub fn abandon(&mut self) {
        self.end(0);
    }

    pub fn mode(&self) -> Option<Mode> {
        self.active.as_ref().map(|_| self.mode)
    }

    /// The score moved on: a section began (marker) or the score ended (stop).
    pub fn section(&mut self, event: &SectionEvent) {
        match event {
            SectionEvent::Section {
                at,
                at_bar,
                index,
                id,
            } => {
                if let Some(rec) = &self.active {
                    rec.marker(*at, *at_bar, id, *index);
                }
            }
            SectionEvent::End { at } => {
                if self.mode == Mode::Score {
                    self.end(*at);
                }
            }
        }
    }

    /// One turn of the host thread: note takes, count dropouts, report progress, and drop
    /// captures the engine is done with.
    pub fn service(&mut self, recording_takes: bool, dropouts: u64, app: &AppHandle) {
        self.port.collect();
        if std::mem::take(&mut self.announce_idle) && self.active.is_none() {
            let _ = app.emit(RECORDING_EVENT, RecordingEvent::default());
        }
        let Some(rec) = &self.active else {
            return;
        };
        if recording_takes {
            rec.note_take();
        }
        rec.set_engine_dropouts(dropouts.saturating_sub(self.dropout_base));
        if self.last_progress.elapsed() >= PROGRESS_INTERVAL {
            self.last_progress = Instant::now();
            let _ = app.emit(
                RECORDING_EVENT,
                RecordingEvent {
                    active: Some(ActiveView {
                        name: rec.name().to_string(),
                        folder: rec.folder().display().to_string(),
                        seconds: rec.seconds(),
                        stopping: rec.stopping(),
                        mode: self.mode.name().to_string(),
                        overflows: rec.overflows(),
                    }),
                    finished: None,
                },
            );
        }
    }
}

/// Track list for a recording: names, channel counts and the compensation each one uses.
pub fn run_tracks(items: impl Iterator<Item = (String, Channels, u64)>) -> Vec<RunTrack> {
    items
        .map(|(name, channels, latency_frames)| RunTrack {
            name,
            channels,
            latency_frames,
        })
        .collect()
}

// ---------------------------------------------------------------------------------------------
// The list of recent recordings
// ---------------------------------------------------------------------------------------------

#[derive(Serialize, Clone, Debug)]
pub struct RecordingEntry {
    pub name: String,
    pub path: String,
    pub title: String,
    pub started_at: String,
    pub seconds: f64,
    pub bytes: u64,
}

fn folder_bytes(dir: &Path) -> u64 {
    std::fs::read_dir(dir)
        .map(|entries| {
            entries
                .flatten()
                .filter_map(|e| e.metadata().ok())
                .filter(|m| m.is_file())
                .map(|m| m.len())
                .sum()
        })
        .unwrap_or(0)
}

/// The newest recordings in `dir`, newest first - every subfolder with a `session.json`.
pub fn list_in(dir: &Path) -> Vec<RecordingEntry> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut list: Vec<RecordingEntry> = entries
        .flatten()
        .filter(|e| e.path().join(SESSION_FILE).is_file())
        .filter_map(|e| {
            let path = e.path();
            let json: serde_json::Value =
                serde_json::from_str(&std::fs::read_to_string(path.join(SESSION_FILE)).ok()?)
                    .ok()?;
            Some(RecordingEntry {
                name: e.file_name().to_string_lossy().into_owned(),
                path: path.display().to_string(),
                title: json["title"].as_str().unwrap_or("").to_string(),
                started_at: json["started_at"].as_str().unwrap_or("").to_string(),
                seconds: json["seconds"].as_f64().unwrap_or(0.0),
                bytes: folder_bytes(&path),
            })
        })
        .collect();
    // The folder name starts with the time stamp, so the name sorts by time.
    list.sort_by(|a, b| b.name.cmp(&a.name));
    list.truncate(LIST_LIMIT);
    list
}

pub fn list() -> Vec<RecordingEntry> {
    list_in(&root())
}

/// A path the list handed out, checked: a folder directly inside the recordings folder that holds
/// a `session.json`. Nothing else is revealed or deleted through these commands.
fn checked(path: &str, base: &Path) -> Result<PathBuf, String> {
    let p = PathBuf::from(path);
    let canonical = p
        .canonicalize()
        .map_err(|_| format!("\"{path}\" does not exist any more."))?;
    let base = base.canonicalize().unwrap_or_else(|_| base.to_path_buf());
    if canonical.parent() != Some(base.as_path()) || !canonical.join(SESSION_FILE).is_file() {
        return Err(format!("\"{path}\" is not a recording of this app."));
    }
    Ok(canonical)
}

/// Show a recording (or the recordings folder) in the Finder.
pub fn reveal(path: Option<&str>) -> Result<(), String> {
    let target = match path {
        Some(p) => checked(p, &root())?,
        None => {
            let r = root();
            std::fs::create_dir_all(&r).map_err(|e| e.to_string())?;
            r
        }
    };
    #[cfg(target_os = "macos")]
    let result = std::process::Command::new("open").arg("-R").arg(&target).status();
    #[cfg(target_os = "windows")]
    let result = std::process::Command::new("explorer")
        .arg(format!("/select,{}", target.display()))
        .status();
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    let result = std::process::Command::new("xdg-open").arg(&target).status();
    result.map(|_| ()).map_err(|e| format!("Cannot open the folder: {e}"))
}

pub fn delete_in(path: &str, base: &Path) -> Result<(), String> {
    let target = checked(path, base)?;
    std::fs::remove_dir_all(&target).map_err(|e| format!("Cannot delete \"{path}\": {e}"))
}

pub fn delete(path: &str) -> Result<Vec<RecordingEntry>, String> {
    delete_in(path, &root())?;
    Ok(list())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "henrys-looper-recording-{name}-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn the_list_shows_only_recordings_newest_first_and_delete_stays_inside() {
        let base = scratch("list");
        for (name, seconds) in [("2026-11-14 20.00.00 A", 61.0), ("2026-11-15 09.30.00 B", 5.5)] {
            let dir = base.join(name);
            std::fs::create_dir_all(&dir).unwrap();
            std::fs::write(
                dir.join(SESSION_FILE),
                format!("{{\"title\":\"{name}\",\"seconds\":{seconds},\"started_at\":\"x\"}}"),
            )
            .unwrap();
            std::fs::write(dir.join("Mix.wav"), vec![0u8; 100]).unwrap();
        }
        std::fs::create_dir_all(base.join("not a recording")).unwrap();
        let list = list_in(&base);
        assert_eq!(list.len(), 2);
        assert_eq!(list[0].name, "2026-11-15 09.30.00 B");
        assert_eq!(list[1].seconds, 61.0);
        assert!(list[0].bytes >= 100);

        // Only a recording folder directly inside the base can be deleted.
        assert!(delete_in(base.join("not a recording").to_str().unwrap(), &base).is_err());
        assert!(delete_in(base.to_str().unwrap(), &base).is_err());
        let elsewhere = scratch("elsewhere");
        std::fs::write(elsewhere.join(SESSION_FILE), "{}").unwrap();
        assert!(delete_in(elsewhere.to_str().unwrap(), &base).is_err());
        assert!(elsewhere.join(SESSION_FILE).is_file());
        delete_in(&list[0].path, &base).unwrap();
        assert_eq!(list_in(&base).len(), 1);
        let _ = std::fs::remove_dir_all(&base);
        let _ = std::fs::remove_dir_all(&elsewhere);
    }

    #[test]
    fn durations_and_sizes_read_like_a_musician_says_them() {
        assert_eq!(duration(0.0), "0:00");
        assert_eq!(duration(61.4), "1:01");
        assert_eq!(size(5 * 1_048_576), "5 MB");
        assert_eq!(size(3 * 1024 * 1_048_576 / 2), "1.5 GB");
    }

    #[test]
    fn the_settings_default_to_recording_with_dry_stems() {
        let s = Settings::default();
        assert!(s.enabled && s.dry && s.folder.is_none());
        let parsed: Settings = serde_json::from_str("{\"dry\": false}").unwrap();
        assert!(parsed.enabled);
        assert!(!parsed.dry);
    }

    #[test]
    fn the_local_stamp_has_the_folder_shape() {
        let (folder, display) = local_stamp(Duration::ZERO);
        assert_eq!(folder.len(), "2026-11-15 20.31.07".len());
        assert_eq!(&folder[4..5], "-");
        assert_eq!(&folder[13..14], ".");
        assert_eq!(&display[13..14], ":");
    }
}
