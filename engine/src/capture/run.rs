//! One recording, seen from the control thread: create the folder, hand the capture to the engine,
//! run the writer, collect section markers, and at the end either write `session.json` or throw
//! the folder away because there was nothing worth keeping.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;

use super::session::{
    self, Dropouts, Marker, SCORE_FILE, SESSION_FILE, SessionFile, TrackEntry, file_specs,
};
use super::writer::{self, Written};
use super::{CapturePort, CaptureSpec, Layout, Shared, capture_pair};
use crate::engine::frame::Channels;

/// Runs shorter than this are not kept.
pub const MIN_SECONDS: f64 = 5.0;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    /// A score run: frame 0 is bar 1 of the first section.
    Score,
    /// Free play: frame 0 is the bar line the first take started in.
    Free,
}

impl Mode {
    pub fn name(self) -> &'static str {
        match self {
            Mode::Score => "score",
            Mode::Free => "free",
        }
    }
}

#[derive(Clone, Debug)]
pub struct RunTrack {
    pub name: String,
    pub channels: Channels,
    pub latency_frames: u64,
}

/// Everything a recording needs to know when it starts.
#[derive(Clone, Debug)]
pub struct RunInfo {
    pub title: String,
    pub mode: Mode,
    /// The folder to create. Made unique (" (2)") if it exists already.
    pub folder: PathBuf,
    /// Local time of frame 0, `YYYY-MM-DD HH:MM:SS`.
    pub started_at: String,
    pub sample_rate: u32,
    pub bpm: f64,
    pub beats_per_bar: u32,
    pub beat_unit: u32,
    pub count_in_bars: u32,
    /// Absolute engine position of frame 0, and the bar index (zero-based) that position starts.
    pub start: u64,
    pub start_bar: u64,
    pub tracks: Vec<RunTrack>,
    pub dry: bool,
    /// Longest block the engine is called with.
    pub max_block: usize,
    pub score_yaml: Option<String>,
    pub score_path: Option<String>,
    /// Runs shorter than this are thrown away.
    pub min_seconds: f64,
}

/// What the control thread adds while the run goes on.
#[derive(Debug, Default)]
struct RunMeta {
    markers: Vec<Marker>,
    /// Whether any take was recorded during the run.
    takes: bool,
    engine_dropouts: u64,
    notes: Vec<String>,
}

/// How a recording ended.
#[derive(Clone, Debug, PartialEq)]
pub struct Outcome {
    pub folder: PathBuf,
    /// Folder name without its parent.
    pub name: String,
    /// False when the run was too short or recorded nothing and the folder was removed.
    pub kept: bool,
    /// Why it was not kept.
    pub reason: Option<String>,
    pub frames: u64,
    pub seconds: f64,
    pub bytes: u64,
    pub dropouts: Dropouts,
    /// A disk error, if one happened. The files are as complete as the disk allowed.
    pub error: Option<String>,
}

pub struct Recording {
    folder: PathBuf,
    name: String,
    start: u64,
    start_bar: u64,
    sample_rate: u32,
    shared: Arc<Shared>,
    meta: Arc<Mutex<RunMeta>>,
    thread: Option<JoinHandle<Outcome>>,
    outcome: Option<Outcome>,
}

/// `folder`, or `folder (2)`, `folder (3)` ... - the first that does not exist yet.
fn unique(folder: &Path) -> PathBuf {
    if !folder.exists() {
        return folder.to_path_buf();
    }
    let base = folder.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let parent = folder.parent().map(Path::to_path_buf).unwrap_or_default();
    (2..)
        .map(|n| parent.join(format!("{base} ({n})")))
        .find(|p| !p.exists())
        .expect("an unused name")
}

impl Recording {
    /// Create the folder, build the capture and hand it to the engine, start the writer.
    pub fn start(info: RunInfo, port: &mut CapturePort) -> Result<Recording, String> {
        let folder = unique(&info.folder);
        std::fs::create_dir_all(&folder)
            .map_err(|e| format!("Cannot create the recording folder \"{}\": {e}", folder.display()))?;
        let layout = Layout {
            tracks: info.tracks.iter().map(|t| t.channels).collect(),
            dry: info.dry,
        };
        let names: Vec<String> = info.tracks.iter().map(|t| t.name.clone()).collect();
        let specs = file_specs(&layout, &names);
        let max_latency = info.tracks.iter().map(|t| t.latency_frames).max().unwrap_or(0);
        let (capture, reader) = capture_pair(&CaptureSpec {
            start: info.start,
            layout: layout.clone(),
            sample_rate: info.sample_rate,
            max_block: info.max_block,
            max_latency,
        });
        let shared = Arc::clone(capture.shared());
        if let Err(e) = port.install(capture) {
            let _ = std::fs::remove_dir(&folder);
            return Err(e);
        }
        let meta = Arc::new(Mutex::new(RunMeta::default()));
        let name = folder
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        let thread = {
            let folder = folder.clone();
            let meta = Arc::clone(&meta);
            let shared = Arc::clone(&shared);
            let info = info.clone();
            std::thread::Builder::new()
                .name("looper-recorder".into())
                .spawn(move || {
                    let written = writer::write_all(reader, &folder, &specs);
                    finalize(&info, &folder, &written, &meta, &shared, &specs)
                })
                .map_err(|e| format!("The recorder thread did not start: {e}"))?
        };
        Ok(Recording {
            folder,
            name,
            start: info.start,
            start_bar: info.start_bar,
            sample_rate: info.sample_rate,
            shared,
            meta,
            thread: Some(thread),
            outcome: None,
        })
    }

    pub fn folder(&self) -> &Path {
        &self.folder
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    /// Absolute engine position of frame 0.
    pub fn start_pos(&self) -> u64 {
        self.start
    }

    /// End the capture at `pos` (an engine position), plus a tail until silence or at most a few
    /// seconds. A second call can only move the end earlier.
    pub fn stop(&self, pos: u64) {
        self.shared.request_stop(pos);
    }

    pub fn stopping(&self) -> bool {
        self.shared.stop_at().is_some()
    }

    /// Frames written so far.
    pub fn frames(&self) -> u64 {
        self.shared.frames()
    }

    pub fn seconds(&self) -> f64 {
        self.frames() as f64 / self.sample_rate.max(1) as f64
    }

    pub fn overflows(&self) -> u64 {
        self.shared.overflows()
    }

    /// A section began at engine position `at`, bar index `at_bar` (zero-based, absolute).
    pub fn marker(&self, at: u64, at_bar: u64, section: &str, index: usize) {
        if at < self.start {
            return;
        }
        let frame = at - self.start;
        let marker = Marker {
            frame,
            seconds: frame as f64 / self.sample_rate.max(1) as f64,
            bar: at_bar.saturating_sub(self.start_bar) + 1,
            section: section.to_string(),
            index,
        };
        if let Ok(mut meta) = self.meta.lock()
            && meta.markers.last() != Some(&marker)
        {
            meta.markers.push(marker);
        }
    }

    /// A take was recorded during this run - the difference between a run worth keeping and a
    /// count-in somebody stopped.
    pub fn note_take(&self) {
        if let Ok(mut meta) = self.meta.lock() {
            meta.takes = true;
        }
    }

    /// Dropouts the engine counted (xruns, input FIFO trouble) since the run started.
    pub fn set_engine_dropouts(&self, count: u64) {
        if let Ok(mut meta) = self.meta.lock() {
            meta.engine_dropouts = count;
        }
    }

    pub fn note(&self, text: impl Into<String>) {
        if let Ok(mut meta) = self.meta.lock() {
            meta.notes.push(text.into());
        }
    }

    /// The outcome, once the writer is done. Non-blocking.
    pub fn poll(&mut self) -> Option<&Outcome> {
        if self.outcome.is_none()
            && self.thread.as_ref().is_some_and(JoinHandle::is_finished)
            && let Some(thread) = self.thread.take()
        {
            self.outcome = Some(thread.join().unwrap_or_else(|_| Outcome {
                folder: self.folder.clone(),
                name: self.name.clone(),
                kept: false,
                reason: None,
                frames: 0,
                seconds: 0.0,
                bytes: 0,
                dropouts: Dropouts::default(),
                error: Some("The recorder thread failed.".to_string()),
            }));
        }
        self.outcome.as_ref()
    }

    /// Block until the writer is done. For tests and for shutting the program down.
    pub fn wait(&mut self) -> Option<&Outcome> {
        if let Some(thread) = self.thread.take() {
            self.outcome = thread.join().ok();
        }
        self.outcome.as_ref()
    }
}

/// The end of a run, on the writer thread: keep it (score copy, `session.json`) or remove it.
fn finalize(
    info: &RunInfo,
    folder: &Path,
    written: &Written,
    meta: &Mutex<RunMeta>,
    shared: &Shared,
    specs: &[writer::FileSpec],
) -> Outcome {
    let rate = info.sample_rate.max(1) as f64;
    let seconds = written.frames as f64 / rate;
    let meta = meta.lock().map(|m| RunMetaView::from(&*m)).unwrap_or_default();
    let dropouts = Dropouts {
        overflow_incidents: shared.overflows(),
        frames_filled_with_silence: shared.dropped(),
        late_live_frames: shared.late(),
        engine_dropouts: meta.engine_dropouts,
    };
    let name = folder
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let reason = if seconds < info.min_seconds {
        Some(format!(
            "shorter than {} seconds",
            info.min_seconds.round() as u64
        ))
    } else if !meta.takes {
        Some("nothing was recorded".to_string())
    } else {
        None
    };
    if let Some(reason) = reason {
        // Only what this run wrote, then the folder if that left it empty - never a blanket
        // recursive delete of something that might have been there before.
        for path in writer::paths(folder, specs) {
            let _ = std::fs::remove_file(path);
        }
        let _ = std::fs::remove_dir(folder);
        return Outcome {
            folder: folder.to_path_buf(),
            name,
            kept: false,
            reason: Some(reason),
            frames: written.frames,
            seconds,
            bytes: 0,
            dropouts,
            error: written.error.clone(),
        };
    }
    let mut error = written.error.clone();
    let mut bytes = written.bytes;
    let score_copy = match &info.score_yaml {
        Some(yaml) => match std::fs::write(folder.join(SCORE_FILE), yaml) {
            Ok(()) => {
                bytes += yaml.len() as u64;
                Some(SCORE_FILE.to_string())
            }
            Err(e) => {
                error.get_or_insert_with(|| format!("{SCORE_FILE}: {e}"));
                None
            }
        },
        None => None,
    };
    let file = SessionFile {
        format: session::FORMAT,
        version: session::VERSION,
        title: info.title.clone(),
        mode: info.mode.name().to_string(),
        started_at: info.started_at.clone(),
        sample_rate: info.sample_rate,
        bit_depth: super::wav::BITS,
        bpm: info.bpm,
        time_signature: format!("{}/{}", info.beats_per_bar, info.beat_unit),
        beats_per_bar: info.beats_per_bar,
        beat_unit: info.beat_unit,
        frames: written.frames,
        seconds,
        bar_one_frame: 0,
        engine_position: info.start,
        count_in_bars: info.count_in_bars,
        mix: session::MIX_FILE.to_string(),
        tracks: info
            .tracks
            .iter()
            .enumerate()
            .map(|(t, track)| TrackEntry {
                number: t + 1,
                name: track.name.clone(),
                input_channels: track.channels.count(),
                stem: session::stem_file(t, &track.name),
                dry: info.dry.then(|| session::dry_file(t, &track.name)),
                midi: None,
                latency_frames: track.latency_frames,
            })
            .collect(),
        markers: meta.markers.clone(),
        dropouts: dropouts.clone(),
        score_copy,
        score_path: info.score_path.clone(),
        notes: meta.notes.clone(),
        contents: session::Contents::default(),
    };
    match serde_json::to_string_pretty(&file) {
        Ok(json) => {
            let path = folder.join(SESSION_FILE);
            match std::fs::write(&path, &json) {
                Ok(()) => bytes += json.len() as u64,
                Err(e) => {
                    error.get_or_insert_with(|| format!("{SESSION_FILE}: {e}"));
                }
            }
        }
        Err(e) => {
            error.get_or_insert_with(|| format!("{SESSION_FILE}: {e}"));
        }
    }
    Outcome {
        folder: folder.to_path_buf(),
        name,
        kept: true,
        reason: None,
        frames: written.frames,
        seconds,
        bytes,
        dropouts,
        error,
    }
}

/// A copy of [`RunMeta`] taken under the lock, plus the counters of the capture itself.
#[derive(Debug, Default)]
struct RunMetaView {
    markers: Vec<Marker>,
    takes: bool,
    engine_dropouts: u64,
    notes: Vec<String>,
}

impl From<&RunMeta> for RunMetaView {
    fn from(m: &RunMeta) -> Self {
        Self {
            markers: m.markers.clone(),
            takes: m.takes,
            engine_dropouts: m.engine_dropouts,
            notes: m.notes.clone(),
        }
    }
}

/// What happened at one turn of the runner, as far as a recording cares.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SectionEvent {
    /// A section began at engine position `at`, absolute bar index `at_bar`.
    Section {
        at: u64,
        at_bar: u64,
        index: usize,
        id: String,
    },
    /// The last section ended at `at`; the score is over.
    End { at: u64 },
}

/// Watches the runner across one `tick` and says when an armed change took effect - the moment a
/// marker belongs in the recording. The runner itself only keeps the sentence; this keeps the
/// sample position and the bar.
#[derive(Clone, Debug, Default)]
pub struct SectionWatch {
    armed: Option<(u64, u64)>,
}

impl SectionWatch {
    /// Call right before `runner.tick(pos)`.
    pub fn before(&mut self, runner: &crate::engine::runner::Runner) {
        self.armed = runner.armed_at();
    }

    /// Call right after `runner.tick(pos)`.
    pub fn after(
        &mut self,
        runner: &crate::engine::runner::Runner,
        pos: u64,
    ) -> Option<SectionEvent> {
        use crate::engine::runner::Phase;
        let (at, at_bar) = self.armed.take()?;
        if pos < at || runner.armed_at() == Some((at, at_bar)) {
            return None;
        }
        match runner.phase() {
            Phase::Finished => Some(SectionEvent::End { at }),
            Phase::Running => {
                let index = runner.view(pos).section?;
                let id = runner.section_ids().get(index)?.to_string();
                Some(SectionEvent::Section {
                    at,
                    at_bar,
                    index,
                    id,
                })
            }
            _ => None,
        }
    }
}
