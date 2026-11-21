//! The folder of one recording: file names, and the `session.json` that says what is in it.
//!
//! ```text
//!  2026-11-15 20.31.07 Mein Stueck/
//!  ├── 01 voice.wav           Stem: Beitrag der Spur zu Main, mit Effekten (+ Live-Signal)
//!  ├── 01 voice (dry).wav     trocken: Loop-Wiedergabe + Live-Eingang, vor allen Effekten
//!  ├── 02 guitar.wav
//!  ├── 02 guitar (dry).wav
//!  ├── Mix.wav                Main-Bus nach Bus-Inserts, ohne Klick, vor Main-Lautstaerke
//!  ├── score.yaml             Kopie der Partitur (nur bei einem Partitur-Durchlauf)
//!  └── session.json
//! ```
//!
//! Every per-track entry in `session.json` carries a `midi` field. A track that sent its instrument
//! anything during the run - live keys and MIDI-loop playback alike - names its Standard MIDI File
//! there (`01 keys.mid`: same number, same name, one more file); every other track has `null`.

use serde::Serialize;

use super::writer::FileSpec;
use super::Layout;

pub const FORMAT: &str = "henrys-looper-recording";
pub const VERSION: u32 = 1;
pub const MIX_FILE: &str = "Mix.wav";
pub const SESSION_FILE: &str = "session.json";
pub const SCORE_FILE: &str = "score.yaml";

/// A name made safe for a file or folder: no path separators, no control characters, no leading
/// dot, and never empty.
pub fn safe_name(name: &str) -> String {
    let cleaned: String = name
        .chars()
        .map(|c| match c {
            '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|' => '-',
            c if c.is_control() => ' ',
            c => c,
        })
        .collect();
    let trimmed = cleaned.trim().trim_start_matches('.').trim();
    let short: String = trimmed.chars().take(80).collect();
    if short.is_empty() {
        "untitled".to_string()
    } else {
        short
    }
}

/// `01 voice` - the stem of every file that belongs to track `index` (zero-based).
pub fn track_stem(index: usize, name: &str) -> String {
    format!("{:02} {}", index + 1, safe_name(name))
}

pub fn stem_file(index: usize, name: &str) -> String {
    format!("{}.wav", track_stem(index, name))
}

pub fn midi_file(index: usize, name: &str) -> String {
    format!("{}.mid", track_stem(index, name))
}

pub fn dry_file(index: usize, name: &str) -> String {
    format!("{} (dry).wav", track_stem(index, name))
}

/// The files one capture writes, in the order of the layout.
pub fn file_specs(layout: &Layout, names: &[String]) -> Vec<FileSpec> {
    let mut specs = vec![FileSpec {
        name: MIX_FILE.to_string(),
        offset: 0,
        channels: 2,
    }];
    for (t, channels) in layout.tracks.iter().enumerate() {
        let name = names.get(t).map(String::as_str).unwrap_or("track");
        specs.push(FileSpec {
            name: stem_file(t, name),
            offset: layout.stem_offset(t),
            channels: 2,
        });
        if let Some(offset) = layout.dry_offset(t) {
            specs.push(FileSpec {
                name: dry_file(t, name),
                offset,
                channels: channels.count() as u16,
            });
        }
    }
    specs
}

/// One section change, on the files' own axis.
#[derive(Serialize, Clone, Debug, PartialEq)]
pub struct Marker {
    /// Sample frame in every file.
    pub frame: u64,
    pub seconds: f64,
    /// Bar number counted from the start of the files: bar 1 is frame 0.
    pub bar: u64,
    /// Section id as the score names it.
    pub section: String,
    /// Zero-based index of the section in the score.
    pub index: usize,
}

/// One audio overrun during the run, on the files' own axis - so a click heard in a WAV can be
/// matched to what the engine measured (`crate::load`).
#[derive(Serialize, Clone, Debug, PartialEq)]
pub struct OverrunMarker {
    /// Sample frame in every file: the start of the block that was late, or - for an overload only
    /// the driver reported - the engine's position when the host noticed it (a few ms later).
    pub frame: u64,
    pub seconds: f64,
    /// What was seen, in words: "late block", "late wake-up", "driver overload", joined by " + ".
    pub kind: String,
    /// Processing time of the late callback in percent of its time budget; 0 when unknown.
    pub load_percent: f32,
}

#[derive(Serialize, Clone, Debug, PartialEq)]
pub struct TrackEntry {
    /// One-based, as in the file names.
    pub number: usize,
    pub name: String,
    /// Channel count of the track's input (1 or 2) - the dry stem has this many.
    pub input_channels: usize,
    /// Stereo stem with effects, fader, mute/solo and pan.
    pub stem: String,
    /// Dry stem, or `null` when dry stems were switched off.
    pub dry: Option<String>,
    /// Reserved for instrument tracks and MIDI loops: `NN name.mid`. Always `null` for now.
    pub midi: Option<String>,
    /// The compensation `R` this track's live input was aligned with, in frames.
    pub latency_frames: u64,
}

#[derive(Serialize, Clone, Debug, PartialEq, Default)]
pub struct Dropouts {
    /// How often the disk could not keep up and the queue ran full.
    pub overflow_incidents: u64,
    /// Frames replaced by silence because of that - in every file at the same place.
    pub frames_filled_with_silence: u64,
    /// Live input frames that arrived after their place had already been written.
    pub late_live_frames: u64,
    /// Driver xruns and input FIFO problems during the run, as the engine counted them.
    pub engine_dropouts: u64,
}

/// The whole of `session.json`.
#[derive(Serialize, Clone, Debug, PartialEq)]
pub struct SessionFile {
    pub format: &'static str,
    pub version: u32,
    pub title: String,
    /// `score` for a score run, `free` for free play.
    pub mode: String,
    /// Local wall-clock time of file frame 0, `YYYY-MM-DD HH:MM:SS`.
    pub started_at: String,
    pub sample_rate: u32,
    pub bit_depth: u16,
    pub bpm: f64,
    pub time_signature: String,
    pub beats_per_bar: u32,
    pub beat_unit: u32,
    /// Length of every audio file in frames.
    pub frames: u64,
    pub seconds: f64,
    /// Where bar 1 is: always frame 0. For a score run that is bar 1 of the first section; the
    /// count-in is not in the files.
    pub bar_one_frame: u64,
    /// The engine's own sample position of frame 0, for cross-referencing logs.
    pub engine_position: u64,
    pub count_in_bars: u32,
    pub mix: String,
    pub tracks: Vec<TrackEntry>,
    pub markers: Vec<Marker>,
    /// Audio overruns during the run, oldest first (at most [`super::run::MAX_OVERRUN_MARKERS`]).
    pub overruns: Vec<OverrunMarker>,
    pub dropouts: Dropouts,
    /// `score.yaml` when a copy of the score is in the folder.
    pub score_copy: Option<String>,
    /// Where the score was loaded from, when the app knew it.
    pub score_path: Option<String>,
    /// Anything worth knowing about this recording, as sentences.
    pub notes: Vec<String>,
    /// What each file contains, in words - so the folder explains itself without this program.
    pub contents: Contents,
}

#[derive(Serialize, Clone, Debug, PartialEq)]
pub struct Contents {
    pub stem: &'static str,
    pub dry: &'static str,
    pub mix: &'static str,
    pub live_signal: &'static str,
}

impl Default for Contents {
    fn default() -> Self {
        Self {
            stem: "The track's contribution to the Main bus: built-in effects, plugin inserts, \
                   fader, mute/solo and pan applied; before the Main bus inserts and volume. Plus \
                   the live complement (see live_signal).",
            dry: "Loop playback (audible layers with their layer gains) plus the raw live input \
                  while the track records, overdubs or hears through. Before effects, inserts, \
                  fader and pan; mono for a mono track.",
            mix: "The Main bus after its inserts, before the Main volume, without the click.",
            live_signal: "While a track records, overdubs or hears through, its input is placed at \
                          the position it was played at (input index minus the track's latency \
                          compensation), the same place the recording puts it - so a live pass and \
                          the loop playback after it line up. In the stem it is added through \
                          fader, mute/solo and pan but without effects, and only when the monitored \
                          input does not already reach Main; when it does, the stem has it as the \
                          room heard it (with effects, one roundtrip late). In Mix.wav the live \
                          input appears only where it really reached Main.",
        }
    }
}

/// Local time as `YYYY-MM-DD HH.MM.SS` (for a folder name) and `YYYY-MM-DD HH:MM:SS`, from the
/// broken-down fields the caller got from its clock.
pub fn stamp(y: i32, mo: u32, d: u32, h: u32, mi: u32, s: u32) -> (String, String) {
    (
        format!("{y:04}-{mo:02}-{d:02} {h:02}.{mi:02}.{s:02}"),
        format!("{y:04}-{mo:02}-{d:02} {h:02}:{mi:02}:{s:02}"),
    )
}

/// Folder name of one recording.
pub fn folder_name(stamp: &str, title: &str) -> String {
    format!("{stamp} {}", safe_name(title))
}
