//! The writer thread: takes super-frames out of the capture queue, splits them into the files and
//! keeps every header current.
//!
//! It never talks to the audio thread except through the queue, and it never waits on it: when
//! there is nothing to read it sleeps for a few milliseconds and looks again. A failing disk
//! (full, unplugged) does not stop it either - it keeps draining the queue so the audio side never
//! sees a full queue because of it, and reports the error at the end.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use super::wav::WavWriter;
use super::{CaptureReader, Read};

/// How often every header is brought up to date while recording.
pub const HEADER_INTERVAL: Duration = Duration::from_millis(1000);
/// Sleep when the queue is empty.
const IDLE: Duration = Duration::from_millis(10);
/// Super-frames taken out of the queue per read.
const CHUNK: usize = 4096;

/// One file and the super-frame channels it is made of.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FileSpec {
    pub name: String,
    /// Offset of the first channel in a super-frame; `channels` channels follow.
    pub offset: usize,
    pub channels: u16,
}

/// What one capture wrote.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Written {
    /// Length of every file, in frames.
    pub frames: u64,
    /// Bytes of all audio files together.
    pub bytes: u64,
    /// `(file name, bytes)` per file.
    pub files: Vec<(String, u64)>,
    /// First disk error, if any. The files are as complete as the disk allowed.
    pub error: Option<String>,
}

/// Write everything `reader` delivers into the files of `specs` inside `dir`, until the capture is
/// over. Blocking - run it on a thread of its own.
pub fn write_all(mut reader: CaptureReader, dir: &Path, specs: &[FileSpec]) -> Written {
    let rate = reader.sample_rate();
    let mut error: Option<String> = None;
    let mut files: Vec<Option<WavWriter>> = specs
        .iter()
        .map(|s| match WavWriter::create(&dir.join(&s.name), s.channels, rate) {
            Ok(w) => Some(w),
            Err(e) => {
                error.get_or_insert_with(|| format!("{}: {e}", s.name));
                None
            }
        })
        .collect();
    let width = reader.width();
    let mut buf: Vec<f32> = Vec::with_capacity(CHUNK * width);
    let mut last_header = Instant::now();
    loop {
        buf.clear();
        match reader.read(&mut buf, CHUNK) {
            Read::Frames(_) => {
                for frame in buf.chunks_exact(width) {
                    for (spec, file) in specs.iter().zip(files.iter_mut()) {
                        let Some(w) = file else { continue };
                        let mut failed = None;
                        for c in 0..spec.channels as usize {
                            if let Err(e) = w.sample(frame[spec.offset + c]) {
                                failed = Some(e);
                                break;
                            }
                        }
                        if let Some(e) = failed {
                            error.get_or_insert_with(|| format!("{}: {e}", spec.name));
                            *file = None;
                        }
                    }
                }
            }
            Read::Empty => std::thread::sleep(IDLE),
            Read::Finished => break,
        }
        if last_header.elapsed() >= HEADER_INTERVAL {
            last_header = Instant::now();
            for (spec, file) in specs.iter().zip(files.iter_mut()) {
                if let Some(w) = file
                    && let Err(e) = w.update_header()
                {
                    error.get_or_insert_with(|| format!("{}: {e}", spec.name));
                    *file = None;
                }
            }
        }
    }
    let mut written = Written {
        frames: reader.delivered(),
        ..Written::default()
    };
    for (spec, file) in specs.iter().zip(files) {
        let Some(w) = file else { continue };
        match w.finish() {
            Ok(bytes) => {
                written.bytes += bytes;
                written.files.push((spec.name.clone(), bytes));
            }
            Err(e) => {
                error.get_or_insert_with(|| format!("{}: {e}", spec.name));
            }
        }
    }
    written.error = error;
    written
}

/// Paths of every file in `specs` under `dir`.
pub fn paths(dir: &Path, specs: &[FileSpec]) -> Vec<PathBuf> {
    specs.iter().map(|s| dir.join(&s.name)).collect()
}
