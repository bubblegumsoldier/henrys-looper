//! A small WAV writer: 24-bit PCM, one or two channels, a header that is kept current while the
//! file grows, and RF64 once the file passes 4 GB.
//!
//! # Layout
//!
//! ```text
//!  0  RIFF <size> WAVE
//! 12  JUNK <28>  (28 Nullbytes - Platz fuer ds64)
//! 48  fmt  <16>  PCM, Kanaele, Rate, 24 Bit
//! 72  data <size> ...Samples...
//! ```
//!
//! The `JUNK` chunk is the standard trick (EBU Tech 3306): every reader skips it, and it reserves
//! exactly the room a `ds64` chunk needs. Should the file pass the 4 GB a 32-bit RIFF size can
//! describe, the header is rewritten as RF64 - `RF64`, `ds64` with the 64-bit sizes, and `0xFFFFFFFF`
//! in the 32-bit fields - without moving a single sample. At 24 bit stereo 48 kHz that happens
//! after about four hours; below it the file is a plain WAV.
//!
//! # Surviving a crash
//!
//! [`WavWriter::update_header`] flushes the samples and writes the current sizes into the header.
//! The writer thread calls it about once a second, so a file whose program died in the middle of a
//! piece is readable up to the last update; nothing after the header is ever left inconsistent
//! with it beyond that second.

use std::fs::File;
use std::io::{BufWriter, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

pub const HEADER_BYTES: u64 = 80;
pub const BITS: u16 = 24;
const BYTES_PER_SAMPLE: u64 = 3;
/// Largest RIFF size a 32-bit field can hold.
const RIFF_LIMIT: u64 = u32::MAX as u64;

pub struct WavWriter {
    file: BufWriter<File>,
    path: PathBuf,
    channels: u16,
    sample_rate: u32,
    data_bytes: u64,
    rf64: bool,
}

/// Float to signed 24 bit, clipped.
#[inline]
pub fn to_i24(x: f32) -> i32 {
    let v = if x.is_finite() { x.clamp(-1.0, 1.0) } else { 0.0 };
    (v * 8_388_607.0).round() as i32
}

impl WavWriter {
    pub fn create(path: &Path, channels: u16, sample_rate: u32) -> std::io::Result<Self> {
        let file = File::create(path)?;
        let mut w = Self {
            file: BufWriter::with_capacity(1 << 18, file),
            path: path.to_path_buf(),
            channels: channels.max(1),
            sample_rate,
            data_bytes: 0,
            rf64: false,
        };
        w.write_header_bytes()?;
        Ok(w)
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn channels(&self) -> u16 {
        self.channels
    }

    pub fn frames(&self) -> u64 {
        self.data_bytes / (self.channels as u64 * BYTES_PER_SAMPLE)
    }

    /// Bytes on disk once finished.
    pub fn file_bytes(&self) -> u64 {
        HEADER_BYTES + self.data_bytes + (self.data_bytes & 1)
    }

    /// Append one sample (call `channels` times per frame).
    #[inline]
    pub fn sample(&mut self, x: f32) -> std::io::Result<()> {
        let v = to_i24(x);
        let b = v.to_le_bytes();
        self.file.write_all(&b[..3])?;
        self.data_bytes += BYTES_PER_SAMPLE;
        Ok(())
    }

    fn write_header_bytes(&mut self) -> std::io::Result<()> {
        let ch = self.channels;
        let block_align = ch as u32 * BYTES_PER_SAMPLE as u32;
        let riff_size = 72 + self.data_bytes + (self.data_bytes & 1);
        let rf64 = self.rf64 || riff_size > RIFF_LIMIT;
        self.rf64 = rf64;
        let mut h = Vec::with_capacity(HEADER_BYTES as usize);
        h.extend_from_slice(if rf64 { b"RF64" } else { b"RIFF" });
        h.extend_from_slice(&(if rf64 { u32::MAX } else { riff_size as u32 }).to_le_bytes());
        h.extend_from_slice(b"WAVE");
        if rf64 {
            h.extend_from_slice(b"ds64");
            h.extend_from_slice(&28u32.to_le_bytes());
            h.extend_from_slice(&riff_size.to_le_bytes());
            h.extend_from_slice(&self.data_bytes.to_le_bytes());
            h.extend_from_slice(&self.frames().to_le_bytes());
            h.extend_from_slice(&0u32.to_le_bytes());
        } else {
            h.extend_from_slice(b"JUNK");
            h.extend_from_slice(&28u32.to_le_bytes());
            h.extend_from_slice(&[0u8; 28]);
        }
        h.extend_from_slice(b"fmt ");
        h.extend_from_slice(&16u32.to_le_bytes());
        h.extend_from_slice(&1u16.to_le_bytes());
        h.extend_from_slice(&ch.to_le_bytes());
        h.extend_from_slice(&self.sample_rate.to_le_bytes());
        h.extend_from_slice(&(self.sample_rate * block_align).to_le_bytes());
        h.extend_from_slice(&(block_align as u16).to_le_bytes());
        h.extend_from_slice(&BITS.to_le_bytes());
        h.extend_from_slice(b"data");
        h.extend_from_slice(&(if rf64 { u32::MAX } else { self.data_bytes as u32 }).to_le_bytes());
        debug_assert_eq!(h.len() as u64, HEADER_BYTES);
        self.file.write_all(&h)
    }

    /// Make the writer believe it holds this many data bytes, to test the RF64 switch without
    /// writing four gigabytes.
    #[cfg(test)]
    pub(crate) fn pretend_data_bytes(&mut self, bytes: u64) {
        self.data_bytes = bytes;
    }

    /// Flush what is buffered and bring the header up to date, so the file is readable as it is.
    pub fn update_header(&mut self) -> std::io::Result<()> {
        self.file.flush()?;
        let end = HEADER_BYTES + self.data_bytes;
        self.file.seek(SeekFrom::Start(0))?;
        self.write_header_bytes()?;
        self.file.seek(SeekFrom::Start(end))?;
        self.file.flush()
    }

    /// Pad to an even data length (RIFF wants word alignment), write the final header and close.
    pub fn finish(mut self) -> std::io::Result<u64> {
        if self.data_bytes & 1 == 1 {
            self.file.write_all(&[0])?;
        }
        self.update_header_padded()?;
        let bytes = self.file_bytes();
        let file = self.file.into_inner().map_err(|e| e.into_error())?;
        file.sync_all()?;
        Ok(bytes)
    }

    fn update_header_padded(&mut self) -> std::io::Result<()> {
        self.file.flush()?;
        self.file.seek(SeekFrom::Start(0))?;
        self.write_header_bytes()?;
        self.file.flush()
    }
}

/// What a WAV file says about itself - for tests and for the list of recordings.
#[derive(Clone, Debug, PartialEq)]
pub struct WavInfo {
    pub channels: u16,
    pub sample_rate: u32,
    pub bits: u16,
    pub frames: u64,
    pub rf64: bool,
}

/// Read a file written by [`WavWriter`] back as floats, interleaved. Only the subset this module
/// writes is understood.
pub fn read_wav(path: &Path) -> Result<(WavInfo, Vec<f32>), String> {
    let bytes = std::fs::read(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let info = parse_header(&bytes)?;
    let start = HEADER_BYTES as usize;
    let len = (info.frames * info.channels as u64 * BYTES_PER_SAMPLE) as usize;
    let data = bytes
        .get(start..start + len)
        .ok_or_else(|| "data chunk shorter than its header says".to_string())?;
    let samples = data
        .chunks_exact(3)
        .map(|b| {
            let v = i32::from_le_bytes([0, b[0], b[1], b[2]]) >> 8;
            v as f32 / 8_388_607.0
        })
        .collect();
    Ok((info, samples))
}

/// Parse the 80-byte header this module writes.
pub fn parse_header(bytes: &[u8]) -> Result<WavInfo, String> {
    if bytes.len() < HEADER_BYTES as usize {
        return Err("too short for a WAV header".into());
    }
    let u16_at = |i: usize| u16::from_le_bytes([bytes[i], bytes[i + 1]]);
    let u32_at = |i: usize| u32::from_le_bytes(bytes[i..i + 4].try_into().unwrap());
    let u64_at = |i: usize| u64::from_le_bytes(bytes[i..i + 8].try_into().unwrap());
    let rf64 = &bytes[0..4] == b"RF64";
    if !(rf64 || &bytes[0..4] == b"RIFF") || &bytes[8..12] != b"WAVE" {
        return Err("not a WAV file".into());
    }
    if &bytes[48..52] != b"fmt " || &bytes[72..76] != b"data" {
        return Err("unexpected chunk layout".into());
    }
    let channels = u16_at(58);
    let sample_rate = u32_at(60);
    let bits = u16_at(70);
    let data_bytes = if rf64 { u64_at(28) } else { u32_at(76) as u64 };
    let frames = data_bytes / (channels.max(1) as u64 * (bits as u64 / 8).max(1));
    Ok(WavInfo {
        channels,
        sample_rate,
        bits,
        frames,
        rf64,
    })
}
