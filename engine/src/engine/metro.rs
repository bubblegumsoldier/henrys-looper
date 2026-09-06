//! Metronome as a pure function of sample position.
//!
//! The phase 0 click generator kept its own frame counter and stepped it once per sample. That is
//! fine for a measuring tool, but here the click is the reference the musician plays to, and it
//! has to stay locked to the engine timeline even across a tempo change or a rendering that is
//! split into segments by a command. Deriving every value from the absolute position removes the
//! whole class of "the counter got out of step" bugs, and it makes the click reproducible in an
//! offline test: the same position always yields the same sample.

use super::timeline::Timeline;

const DOWNBEAT_HZ: f64 = 1600.0;
const OFFBEAT_HZ: f64 = 800.0;
/// Tone length. Long enough to be audible, short enough not to smear the beat.
const CLICK_MS: f64 = 30.0;
/// Fade-in, so the tone does not start with a step discontinuity.
const ATTACK_MS: f64 = 1.0;
const CLICK_AMP: f32 = 0.5;

#[derive(Clone, Copy, Debug)]
pub struct Metronome {
    sample_rate: f64,
    env_len: u64,
    attack_len: f32,
    decay_per_sample: f64,
}

impl Metronome {
    pub fn new(sample_rate: u32) -> Self {
        let sr = sample_rate as f64;
        let env_len = (CLICK_MS / 1000.0 * sr).round().max(1.0) as u64;
        Self {
            sample_rate: sr,
            env_len,
            attack_len: (ATTACK_MS / 1000.0 * sr).round().max(1.0) as f32,
            // Exponential decay reaching -60 dB exactly at the end of the envelope.
            decay_per_sample: (-(1000f64.ln()) / env_len as f64).exp(),
        }
    }

    /// Length of one click tone in samples, envelope included.
    ///
    /// The calibration uses it to size the blanking time of its onset detection, so that the decay
    /// of one click cannot be counted as a second one.
    #[inline]
    pub fn tone_len(&self) -> u64 {
        self.env_len
    }

    /// Value of the click signal at absolute sample position `pos`.
    ///
    /// Callback-safe: no allocation, no locking, no branching on shared state.
    #[inline]
    pub fn sample_at(&self, timeline: &Timeline, pos: u64) -> f32 {
        let beat = timeline.beat_index_at(pos);
        let n = pos - timeline.beat_start(beat);
        if n >= self.env_len {
            return 0.0;
        }
        let downbeat = beat % timeline.beats_per_bar() == 0;
        let hz = if downbeat { DOWNBEAT_HZ } else { OFFBEAT_HZ };
        let phase = std::f64::consts::TAU * hz * n as f64 / self.sample_rate;
        let env = self.decay_per_sample.powi(n as i32);
        let attack = (n as f32 / self.attack_len).min(1.0);
        (phase.sin() * env) as f32 * attack * CLICK_AMP
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::timeline::TimeSignature;

    #[test]
    fn click_sits_exactly_on_the_beat_grid() {
        let t = Timeline::new(48_000, 137.0, TimeSignature::new(7, 8));
        let m = Metronome::new(48_000);
        for beat in 0..200u64 {
            let start = t.beat_start(beat);
            // The very first sample of the envelope is silent by construction (attack 0, sin 0),
            // the second one is not - that is where the tone actually begins.
            assert_eq!(m.sample_at(&t, start), 0.0);
            assert_ne!(m.sample_at(&t, start + 1), 0.0);
            // Just before a beat the previous tone must have died away completely.
            if start > 2000 {
                assert_eq!(m.sample_at(&t, start - 1), 0.0);
            }
        }
    }

    #[test]
    fn downbeat_differs_from_offbeat() {
        let t = Timeline::new(48_000, 120.0, TimeSignature::new(4, 4));
        let m = Metronome::new(48_000);
        let down = m.sample_at(&t, t.bar_start(3) + 20);
        let off = m.sample_at(&t, t.position_of(3, 1) + 20);
        assert_ne!(down, off);
    }

    #[test]
    fn amplitude_stays_inside_the_headroom() {
        let t = Timeline::new(48_000, 120.0, TimeSignature::new(4, 4));
        let m = Metronome::new(48_000);
        for pos in 0..200_000u64 {
            assert!(m.sample_at(&t, pos).abs() <= CLICK_AMP);
        }
    }
}
