//! Metronome. The click position comes from a free-running sample counter inside the audio
//! callback, never from the wall clock.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::Duration;

use clap::Args;
use cpal::traits::StreamTrait;
use cpal::{ErrorKind, OutputCallbackInfo};

use crate::audio::{self, DeviceOpts};

const DOWNBEAT_HZ: f64 = 1600.0;
const OFFBEAT_HZ: f64 = 800.0;
/// Tone length. Long enough to be audible, short enough not to smear the beat.
const CLICK_MS: f64 = 30.0;
/// Fade-in, so the tone does not start with a step discontinuity (that would be a "knack").
const ATTACK_MS: f64 = 1.0;
const CLICK_AMP: f32 = 0.5;

#[derive(Args, Debug, Clone)]
pub struct ClickOpts {
    /// Tempo in Schlaegen pro Minute (bezogen auf Viertel)
    #[arg(long, default_value_t = 100.0)]
    pub bpm: f64,

    /// Schlaege pro Takt (3, 4, 5, 7 ... alles erlaubt)
    #[arg(long, default_value_t = 4)]
    pub beats_per_bar: u32,

    /// Zaehlzeit: 4 = Viertel, 8 = Achtel
    #[arg(long, default_value_t = 4)]
    pub beat_unit: u32,

    /// Anzahl Takte, 0 = endlos
    #[arg(long, default_value_t = 0)]
    pub bars: u32,
}

#[derive(Debug, Clone, Copy)]
pub struct ClickSpec {
    pub bpm: f64,
    pub beats_per_bar: u32,
    pub beat_unit: u32,
    pub bars: u32,
}

impl ClickSpec {
    pub fn validate(&self) -> Result<(), String> {
        if !(1.0..=400.0).contains(&self.bpm) {
            return Err(format!("BPM {} liegt ausserhalb von 1..400.", self.bpm));
        }
        if self.beats_per_bar == 0 {
            return Err("--beats-per-bar muss mindestens 1 sein.".to_string());
        }
        if ![1, 2, 4, 8, 16, 32].contains(&self.beat_unit) {
            return Err(format!(
                "--beat-unit {} ist keine Notenlaenge (erlaubt: 1, 2, 4, 8, 16, 32).",
                self.beat_unit
            ));
        }
        Ok(())
    }
}

impl From<&ClickOpts> for ClickSpec {
    fn from(o: &ClickOpts) -> Self {
        ClickSpec {
            bpm: o.bpm,
            beats_per_bar: o.beats_per_bar,
            beat_unit: o.beat_unit,
            bars: o.bars,
        }
    }
}

/// Sample-accurate click generator. Everything it needs is precomputed; `next_sample` allocates
/// nothing and takes no locks, so it is safe to call from the audio callback.
pub struct ClickGen {
    sample_rate: f64,
    /// Fractional beat length. Beat boundaries are derived from `beat_index * samples_per_beat`
    /// and rounded once, so rounding error never accumulates over long runs.
    samples_per_beat: f64,
    beats_per_bar: u64,
    total_beats: Option<u64>,
    frame: u64,
    next_beat_index: u64,
    next_beat_frame: u64,
    env_pos: u32,
    env_len: u32,
    attack_len: f32,
    decay_per_sample: f64,
    env: f64,
    phase: f64,
    phase_inc: f64,
    done: bool,
}

impl ClickGen {
    pub fn new(spec: ClickSpec, sample_rate: u32) -> Self {
        let sr = sample_rate as f64;
        // BPM counts quarter notes; a beat-unit of 8 halves the beat length.
        let samples_per_beat = sr * 60.0 / spec.bpm * (4.0 / spec.beat_unit as f64);
        let env_len = (CLICK_MS / 1000.0 * sr).round().max(1.0) as u32;
        // Exponential decay reaching -60 dB exactly at the end of the envelope.
        let decay_per_sample = (-(1000f64.ln()) / env_len as f64).exp();
        let total_beats = if spec.bars == 0 {
            None
        } else {
            Some(spec.bars as u64 * spec.beats_per_bar as u64)
        };
        Self {
            sample_rate: sr,
            samples_per_beat,
            beats_per_bar: spec.beats_per_bar as u64,
            total_beats,
            frame: 0,
            next_beat_index: 0,
            next_beat_frame: 0,
            env_pos: env_len, // idle
            env_len,
            attack_len: (ATTACK_MS / 1000.0 * sr).round().max(1.0) as f32,
            decay_per_sample,
            env: 0.0,
            phase: 0.0,
            phase_inc: 0.0,
            done: false,
        }
    }

    pub fn samples_per_beat(&self) -> f64 {
        self.samples_per_beat
    }

    /// True once the requested number of bars has been played out completely.
    #[inline]
    pub fn is_done(&self) -> bool {
        self.done
    }

    #[inline]
    pub fn next_sample(&mut self) -> f32 {
        let beats_left = match self.total_beats {
            Some(total) => self.next_beat_index < total,
            None => true,
        };
        if beats_left && self.frame >= self.next_beat_frame {
            let downbeat = self.next_beat_index % self.beats_per_bar == 0;
            let hz = if downbeat { DOWNBEAT_HZ } else { OFFBEAT_HZ };
            self.phase = 0.0;
            self.phase_inc = std::f64::consts::TAU * hz / self.sample_rate;
            self.env = 1.0;
            self.env_pos = 0;
            self.next_beat_index += 1;
            self.next_beat_frame =
                (self.next_beat_index as f64 * self.samples_per_beat).round() as u64;
        }

        let out = if self.env_pos < self.env_len {
            let attack = (self.env_pos as f32 / self.attack_len).min(1.0);
            let s = (self.phase.sin() * self.env) as f32 * attack * CLICK_AMP;
            self.phase += self.phase_inc;
            if self.phase > std::f64::consts::TAU {
                self.phase -= std::f64::consts::TAU;
            }
            self.env *= self.decay_per_sample;
            self.env_pos += 1;
            s
        } else {
            if !beats_left {
                self.done = true;
            }
            0.0
        };
        self.frame += 1;
        out
    }
}

pub fn cmd_click(dev: &DeviceOpts, click: &ClickOpts) -> Result<(), String> {
    let spec = ClickSpec::from(click);
    spec.validate()?;
    let setup = audio::open_output(dev)?;
    let rate = setup.plan.config.sample_rate;
    let channels = setup.plan.config.channels as usize;

    let mut clicker = ClickGen::new(spec, rate);
    println!("Host:     {}", setup.host_name);
    println!(
        "Ausgang:  {} ({} Kanaele, {:?})",
        setup.name, channels, setup.plan.format
    );
    println!(
        "Format:   {} Hz, {} Frames Puffer angefordert (Geraet meldet {})",
        rate,
        dev.buffer,
        audio::describe_buffer_size(&setup.plan.reported_buffer)
    );
    println!(
        "Klick:    {} BPM, {}/{}, {:.3} Samples pro Schlag, {}",
        spec.bpm,
        spec.beats_per_bar,
        spec.beat_unit,
        clicker.samples_per_beat(),
        if spec.bars == 0 {
            "endlos".to_string()
        } else {
            format!("{} Takte", spec.bars)
        }
    );

    let finished = Arc::new(AtomicBool::new(false));
    let xruns = Arc::new(AtomicU64::new(0));
    let errors = Arc::new(AtomicU64::new(0));

    let finished_cb = Arc::clone(&finished);
    let xruns_cb = Arc::clone(&xruns);
    let errors_cb = Arc::clone(&errors);

    let stream = audio::build_output(
        &setup.device,
        &setup.plan,
        move |data: &mut [f32], _: &OutputCallbackInfo, _offset: usize| {
            for frame in data.chunks_exact_mut(channels) {
                let s = clicker.next_sample();
                for sample in frame.iter_mut() {
                    *sample = s;
                }
            }
            if clicker.is_done() {
                finished_cb.store(true, Ordering::Relaxed);
            }
        },
        move |err| {
            if err.kind() == ErrorKind::Xrun {
                xruns_cb.fetch_add(1, Ordering::Relaxed);
            } else {
                errors_cb.fetch_add(1, Ordering::Relaxed);
            }
        },
    )?;

    if let Ok(actual) = stream.buffer_size() {
        println!("Tatsaechliche Puffergroesse laut Treiber: {actual} Frames");
    }
    stream
        .play()
        .map_err(|e| format!("Ausgabestream startet nicht: {e}"))?;

    println!("\nLaeuft. Enter beendet.");
    let enter = crate::wait_for_enter();
    loop {
        if enter.recv_timeout(Duration::from_millis(100)).is_ok() {
            break;
        }
        if finished.load(Ordering::Relaxed) {
            // Let the last tone leave the buffer before tearing the stream down.
            std::thread::sleep(Duration::from_millis(200));
            break;
        }
    }
    drop(stream);
    println!(
        "\nBeendet. Xruns: {}, sonstige Stream-Fehler: {}",
        xruns.load(Ordering::Relaxed),
        errors.load(Ordering::Relaxed)
    );
    Ok(())
}
