//! A finished recording folder, read back: `session.json`, the files it names, the score copy and
//! the plugin settings copy. Everything is checked before anything heavy is done, so a broken
//! folder ends with one clear sentence instead of half a render.

use std::path::{Path, PathBuf};

use serde::Deserialize;

use crate::capture::session::{FORMAT, SESSION_FILE, VERSION};
use crate::capture::wav::{HEADER_BYTES, WavInfo, parse_header, read_wav};

/// `session.json`, as far as rendering needs it.
#[derive(Clone, Debug, Deserialize)]
pub struct Session {
    pub format: String,
    pub version: u32,
    #[serde(default)]
    pub title: String,
    pub sample_rate: u32,
    pub bpm: f64,
    pub beats_per_bar: u32,
    pub beat_unit: u32,
    pub frames: u64,
    /// The live engine's position of frame 0 - only its phase against the audio block is used.
    #[serde(default)]
    pub engine_position: u64,
    #[serde(default)]
    pub count_in_bars: u32,
    #[serde(default)]
    pub mix: Option<String>,
    pub tracks: Vec<SessionTrack>,
    #[serde(default)]
    pub markers: Vec<SessionMarker>,
    #[serde(default)]
    pub score_copy: Option<String>,
}

#[derive(Clone, Debug, Deserialize)]
pub struct SessionTrack {
    pub number: usize,
    pub name: String,
    pub input_channels: usize,
    pub stem: String,
    #[serde(default)]
    pub dry: Option<String>,
    #[serde(default)]
    pub midi: Option<String>,
    #[serde(default)]
    pub latency_frames: u64,
}

#[derive(Clone, Debug, Deserialize)]
pub struct SessionMarker {
    pub frame: u64,
    pub section: String,
}

/// A recording folder whose `session.json` has been read and whose files have been checked.
#[derive(Clone, Debug)]
pub struct Recording {
    pub dir: PathBuf,
    pub session: Session,
}

/// The name of the plugin settings copy inside a recording (architecture section 16).
pub const SIDECAR_COPY: &str = "score.plugins.json";

impl Recording {
    pub fn open(dir: &Path) -> Result<Self, String> {
        if !dir.is_dir() {
            return Err(format!("{} is not a folder.", dir.display()));
        }
        let path = dir.join(SESSION_FILE);
        let text = std::fs::read_to_string(&path).map_err(|e| {
            format!(
                "{} has no readable {SESSION_FILE} ({e}). Is it a Henrys Looper recording, and did \
                 the recording finish?",
                dir.display()
            )
        })?;
        let session: Session = serde_json::from_str(&text)
            .map_err(|e| format!("{} cannot be read: {e}", path.display()))?;
        if session.format != FORMAT {
            return Err(format!(
                "{} says format \"{}\", expected \"{FORMAT}\".",
                path.display(),
                session.format
            ));
        }
        if session.version != VERSION {
            return Err(format!(
                "{} has version {}; this program reads version {VERSION}.",
                path.display(),
                session.version
            ));
        }
        if session.sample_rate == 0 || session.bpm <= 0.0 || session.frames == 0 {
            return Err(format!(
                "{} names no sample rate, tempo or length.",
                path.display()
            ));
        }
        if session.tracks.is_empty() {
            return Err(format!("{} lists no tracks.", path.display()));
        }
        for t in &session.tracks {
            if !matches!(t.input_channels, 1 | 2) {
                return Err(format!(
                    "Track \"{}\" in {} has {} input channels; 1 or 2 expected.",
                    t.name,
                    path.display(),
                    t.input_channels
                ));
            }
        }
        let rec = Self {
            dir: dir.to_path_buf(),
            session,
        };
        for t in &rec.session.tracks {
            rec.check_wav(&t.stem, 2)?;
        }
        Ok(rec)
    }

    /// The score copy in the folder, if the recording has one.
    pub fn score_copy(&self) -> Option<PathBuf> {
        self.session
            .score_copy
            .as_deref()
            .map(|name| self.dir.join(name))
            .filter(|p| p.is_file())
    }

    /// The plugin settings copy in the folder, if the recording has one.
    pub fn sidecar_copy(&self) -> Option<PathBuf> {
        Some(self.dir.join(SIDECAR_COPY)).filter(|p| p.is_file())
    }

    /// Check one file of the folder by its header: it exists, is a WAV this program wrote, has the
    /// expected channel count, the session's sample rate and length.
    pub fn check_wav(&self, name: &str, channels: usize) -> Result<WavInfo, String> {
        let path = self.dir.join(name);
        let mut head = vec![0u8; HEADER_BYTES as usize];
        let mut file = std::fs::File::open(&path)
            .map_err(|e| format!("{} is missing from the recording ({e}).", path.display()))?;
        std::io::Read::read_exact(&mut file, &mut head)
            .map_err(|_| format!("{} is too short to be a WAV file.", path.display()))?;
        let info = parse_header(&head).map_err(|e| {
            format!(
                "{}: {e} (only the 24-bit WAV files Henrys Looper writes are read).",
                path.display()
            )
        })?;
        if info.bits != 24 {
            return Err(format!("{} is {}-bit; 24-bit expected.", path.display(), info.bits));
        }
        if info.channels as usize != channels {
            return Err(format!(
                "{} has {} channels; {channels} expected.",
                path.display(),
                info.channels
            ));
        }
        if info.sample_rate != self.session.sample_rate {
            return Err(format!(
                "{} is at {} Hz, the session at {} Hz.",
                path.display(),
                info.sample_rate,
                self.session.sample_rate
            ));
        }
        let expected = HEADER_BYTES + info.frames * info.channels as u64 * 3;
        let actual = std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
        if actual < expected {
            return Err(format!(
                "{} is cut short: {actual} bytes, its header promises {expected}.",
                path.display()
            ));
        }
        if info.frames != self.session.frames {
            return Err(format!(
                "{} has {} frames, session.json says {}. The file is cut short or does not belong \
                 to this recording.",
                path.display(),
                info.frames,
                self.session.frames
            ));
        }
        Ok(info)
    }

    /// A checked file of the folder, as floats (interleaved).
    pub fn read(&self, name: &str, channels: usize) -> Result<Vec<f32>, String> {
        self.check_wav(name, channels)?;
        let (_, samples) = read_wav(&self.dir.join(name))?;
        Ok(samples)
    }

    /// Frame ranges of the sections, `[from, to)`, from the markers.
    pub fn sections(&self) -> Vec<(String, u64, u64)> {
        let m = &self.session.markers;
        m.iter()
            .enumerate()
            .map(|(i, mk)| {
                let to = m.get(i + 1).map_or(self.session.frames, |n| n.frame);
                (mk.section.clone(), mk.frame.min(self.session.frames), to.min(self.session.frames))
            })
            .filter(|(_, a, b)| b > a)
            .collect()
    }
}
