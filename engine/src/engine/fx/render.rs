//! Offline rendering: a recorded stem through one track's effect chain, into a new WAV file.
//!
//! ```text
//! looper-engine fxrender "03 bass (dry).wav" bass.wav --mix "{preset: bass_from_guitar}"
//! ```
//!
//! For listening to a chain without an audio interface, and for measuring what it does to real
//! material rather than to test tones. `--mix` takes one track entry exactly as it is written under
//! `mixer.tracks` in a score - preset, effect lines, fader - and goes through the same score reader
//! and the same commands a loaded song does, so what is rendered is what the track would play.
//!
//! Reads the WAV files the looper records (24-bit PCM, mono or stereo) and writes 24-bit stereo.
//! Opens no device and makes no sound.

use std::path::Path;

use super::super::frame::Frame;
use super::Chain;
use crate::capture::wav::{WavWriter, read_wav};
use crate::engine::command::{Command, TrackStatus};
use crate::score::compile_score;
use crate::score::mixer::{TrackMixSource, track_commands};

/// Options of `fxrender`.
#[derive(Clone, Debug)]
pub struct RenderOpts {
    /// The track entry, as written under `mixer.tracks` (flow style).
    pub mix: String,
    /// Tempo for a tempo-synchronous delay.
    pub bpm: f64,
    /// Where to start in the input, seconds.
    pub start: f64,
    /// How much to render, seconds; `None` = to the end.
    pub seconds: Option<f64>,
}

/// One mixer entry, read the way a score reads it.
pub fn parse_entry(yaml: &str) -> Result<TrackMixSource, String> {
    let text = format!(
        "bpm: 100\ntracks:\n  track: {{input: 1}}\nsections:\n  - {{id: a, bars: 1}}\nmixer:\n  tracks:\n    track: {yaml}\n"
    );
    let score = compile_score(&text).map_err(|e| e.to_string())?;
    score
        .mixer
        .and_then(|m| m.tracks.get("track").cloned())
        .ok_or_else(|| "the mixer entry is empty".to_string())
}

/// A chain put into the state an entry describes, through the commands a score would send.
pub fn chain_for(entry: &TrackMixSource, sample_rate: u32) -> (Chain, f32) {
    let mix = entry.resolve();
    let mut chain = Chain::new(sample_rate);
    for command in track_commands(0, &mix, &TrackStatus::default()) {
        match command {
            Command::LoadFxPreset { preset, .. } => chain.load_preset(preset),
            Command::SetFxEnabled { slot, on, .. } => chain.set_enabled(slot, on),
            Command::SetFxParam { param, .. } => chain.set_param(param),
            Command::SetFxBypass { on, .. } => chain.set_bypass(on),
            _ => {}
        }
    }
    (chain, mix.gain)
}

/// `fxrender` subcommand.
pub fn cmd_fxrender(input: &Path, output: &Path, opts: &RenderOpts) -> Result<(), String> {
    let entry = parse_entry(&opts.mix)?;
    let (info, samples) = read_wav(input)?;
    let channels = info.channels.max(1) as usize;
    let rate = info.sample_rate;
    let frames = samples.len() / channels;
    let first = ((opts.start.max(0.0) * rate as f64) as usize).min(frames);
    let last = match opts.seconds {
        Some(s) => (first + (s.max(0.0) * rate as f64) as usize).min(frames),
        None => frames,
    };
    let (mut chain, gain) = chain_for(&entry, rate);
    chain.set_quarter_samples(rate as f64 * 60.0 / opts.bpm.max(1.0));
    let mut writer = WavWriter::create(output, 2, rate).map_err(|e| format!("{}: {e}", output.display()))?;
    let mut peak = 0.0f32;
    for i in first..last {
        let x = if channels == 1 {
            Frame::mono(samples[i])
        } else {
            Frame::new(samples[i * channels], samples[i * channels + 1])
        };
        let y = chain.process(x);
        let (l, r) = (y.l * gain, y.r * gain);
        peak = peak.max(l.abs()).max(r.abs());
        writer.sample(l).and_then(|_| writer.sample(r)).map_err(|e| e.to_string())?;
    }
    writer.finish().map_err(|e| e.to_string())?;
    let status = chain.status();
    println!(
        "{} -> {}: {:.2} s at {} Hz, chain {} {}, latency {} frames, peak {:.1} dBFS",
        input.display(),
        output.display(),
        (last - first) as f64 / rate as f64,
        rate,
        status.preset.label(),
        status.letters(),
        chain.latency(),
        20.0 * peak.max(1.0e-9).log10()
    );
    Ok(())
}
