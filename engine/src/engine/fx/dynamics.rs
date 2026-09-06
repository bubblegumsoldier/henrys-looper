//! The compressor - the part the musician actually asked for ("auf Voice muss Schmackes drauf").
//!
//! # What it does, in one sentence
//!
//! It measures how far the signal is above the threshold, turns that into an amount of gain
//! reduction, smooths that amount with an attack and a release time, and applies it.
//!
//! # Why the arithmetic happens in decibels
//!
//! A ratio is a statement about decibels ("4 dB over the threshold come out as 1 dB over it at
//! 4:1"), and so is a knee. Doing it in the linear domain means solving the same equations with
//! roots and powers and getting a knee that is not a parabola in the units anyone thinks in. The
//! price is a logarithm and an exponential per sample; both are done in base 2, where the hardware
//! is fastest, and scaled into decibels by a constant. `fx::bench` says what that costs.
//!
//! # Why a soft knee, and why 6 to 8 dB of it
//!
//! A hard knee switches the gain reduction on at one exact level. On a sustained instrument that
//! is inaudible, because the level crosses the threshold once and stays there. On a voice it is
//! not: a singer sits *at* the threshold for most of a phrase, so a hard knee toggles the
//! reduction on and off several times per second, and what that sounds like is a wobble in the
//! body of the voice rather than compression. A knee of 6 to 8 dB spreads the transition over
//! roughly the range a singer's level wanders inside one phrase, so the gain moves continuously
//! and the ear hears "even", not "processed". A guitar gets a slightly narrower knee because its
//! level is decided by the picking hand and is far less continuous.
//!
//! # Detector: smooth decoupled peak, in two stages
//!
//! Peak, not RMS: unlike an RMS detector it catches the one loud consonant that would otherwise
//! clip the output, and with a realistic attack time it behaves like an RMS detector on sustained
//! material anyway.
//!
//! The naive version - compute the wanted gain reduction from the instantaneous sample and smooth
//! it with attack and release - has a flaw that shows up on exactly the material this looper sees.
//! A sine passes through zero twice per period, and at those instants the "wanted" reduction is
//! nothing at all. On a 100 Hz note that happens every 5 ms, which is the same order as an attack
//! time, so the gain ripples along with the waveform instead of following the phrase. What that
//! sounds like is distortion, and what it does to a measurement is make the ratio come out wrong.
//!
//! So the smoothing is split, the way the literature does it (Giannoulis / Massberg / Reiss,
//! *Digital Dynamic Range Compressor Design*, 2012, the "smooth decoupled" arrangement):
//!
//! ```text
//! Stufe 1   y1 = min(soll, y1 + (soll - y1) * release)   Spitzenhalt: sofort tiefer,
//!                                                        nur mit der Release-Zeit zurueck
//! Stufe 2   gain += (y1 - gain) * attack                 die hoerbare Attack-Zeit
//! ```
//!
//! Stage 1 holds the deepest reduction the waveform asked for and lets it go at the release time,
//! so the zero crossings of a sine no longer reach the gain at all. Stage 2 is the attack. The two
//! numbers on the front panel therefore mean what they always meant: after one attack time 63 % of
//! the reduction has happened, after one release time 63 % of it has come back.
//!
//! # Stereo: one detector, one gain, both channels
//!
//! The detector sees the **louder of the two channels** and the resulting gain is applied to both.
//! Two independent compressors, one per channel, would be the obvious translation and it is the
//! wrong one: whenever the material is louder on one side - a piano chord in the left hand, a pad
//! whose movement is not symmetric - that side would be turned down and the other would not, and
//! the stereo image would slide towards the quieter speaker on every level peak. What the ear hears
//! is not "compression", it is the instrument wandering across the stage.
//!
//! Taking the maximum rather than the sum or the mean is the conservative choice for a *peak*
//! detector: whatever would clip the output is caught, whichever side it is on. On a mono track
//! fanned out to two identical channels the maximum equals the single channel, so a mono track
//! compresses exactly as it did before this module knew about stereo.

use super::super::frame::Frame;
use super::biquad::flush;
use super::smooth::Smoothed;

/// 20 * log10(x) expressed in base 2: 20 / log2(10).
const DB_PER_LOG2: f32 = 6.020_6;
/// Level below which the input counts as silence, so the logarithm never sees a zero.
/// -180 dBFS, far under the noise floor of any converter.
const LEVEL_FLOOR: f32 = 1.0e-9;
/// Gain reduction under which the compressor is considered to be back at unity.
///
/// 0.0001 dB is a linear factor of 1.000012 - nothing, by any measure. Without this the release
/// approaches zero asymptotically and never arrives, and a value that hovers a hair below zero
/// forever is a denormal waiting to happen. Snapping is also what lets a silent chain be skipped.
const GAIN_SNAP_DB: f32 = 1.0e-4;

/// The knob positions. Kept apart from the running state so a preset is plain data.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct CompSettings {
    pub threshold_db: f32,
    /// 1.0 means no compression; 3.0 means 3:1.
    pub ratio: f32,
    pub attack_ms: f32,
    pub release_ms: f32,
    /// Total width of the soft knee in dB, centred on the threshold. 0 is a hard knee.
    pub knee_db: f32,
    pub makeup_db: f32,
}

impl Default for CompSettings {
    fn default() -> Self {
        Self {
            threshold_db: -18.0,
            ratio: 3.0,
            attack_ms: 12.0,
            release_ms: 180.0,
            knee_db: 8.0,
            makeup_db: 0.0,
        }
    }
}

pub struct Compressor {
    set: CompSettings,
    sample_rate: u32,
    attack_coeff: f32,
    release_coeff: f32,
    /// Stage 1 of the detector: the deepest reduction currently being held, in dB. Always <= 0.
    hold_db: f32,
    /// Stage 2: the gain reduction actually applied, in dB. Always <= 0.
    gain_db: f32,
    /// Slowest reduction seen since the last status snapshot, for the meter.
    peak_reduction_db: f32,
    makeup: Smoothed,
}

/// One-pole coefficient for a time constant of `ms` milliseconds.
#[inline]
fn time_coeff(ms: f32, sample_rate: u32) -> f32 {
    let samples = (ms * 0.001 * sample_rate as f32).max(1.0);
    1.0 - (-1.0 / samples).exp()
}

impl Compressor {
    pub fn new(sample_rate: u32, set: CompSettings) -> Self {
        let mut c = Self {
            set,
            sample_rate,
            attack_coeff: 0.0,
            release_coeff: 0.0,
            hold_db: 0.0,
            gain_db: 0.0,
            peak_reduction_db: 0.0,
            // The makeup gain is the one parameter a musician turns while singing, so it ramps.
            makeup: Smoothed::new(db_to_linear(set.makeup_db), 30.0, sample_rate),
        };
        c.recompute();
        c
    }

    fn recompute(&mut self) {
        self.attack_coeff = time_coeff(self.set.attack_ms, self.sample_rate);
        self.release_coeff = time_coeff(self.set.release_ms, self.sample_rate);
        self.makeup.set(db_to_linear(self.set.makeup_db));
    }

    pub fn settings(&self) -> CompSettings {
        self.set
    }

    /// Change one or more knobs. Times and ratio take effect on the next sample; the makeup gain
    /// ramps, because it is a level and a level that jumps is a click.
    pub fn set_settings(&mut self, set: CompSettings) {
        self.set = CompSettings {
            threshold_db: set.threshold_db.clamp(-60.0, 0.0),
            ratio: set.ratio.clamp(1.0, 20.0),
            attack_ms: set.attack_ms.clamp(0.1, 200.0),
            release_ms: set.release_ms.clamp(5.0, 2_000.0),
            knee_db: set.knee_db.clamp(0.0, 24.0),
            makeup_db: set.makeup_db.clamp(-24.0, 24.0),
        };
        self.recompute();
    }

    /// Load knobs into a chain that is not audible yet, without a ramp.
    pub fn snap_settings(&mut self, set: CompSettings) {
        self.set_settings(set);
        self.makeup.snap_to(db_to_linear(self.set.makeup_db));
        self.hold_db = 0.0;
        self.gain_db = 0.0;
    }

    /// Deepest gain reduction since the last call, in dB (negative). For the display.
    pub fn take_reduction_db(&mut self) -> f32 {
        std::mem::replace(&mut self.peak_reduction_db, 0.0)
    }

    #[inline]
    pub fn reset(&mut self) {
        self.hold_db = 0.0;
        self.gain_db = 0.0;
        self.peak_reduction_db = 0.0;
    }

    /// One stereo frame. The detector runs once on the louder channel and the gain it produces is
    /// applied to both - see the module comment.
    #[inline(always)]
    pub fn process(&mut self, x: Frame) -> Frame {
        let gain = self.gain_for(x.max_abs());
        Frame {
            l: x.l * gain,
            r: x.r * gain,
        }
    }

    /// Total gain (reduction times makeup) for a detector level, advancing the detector by one
    /// sample. Split out so the stereo path calls it exactly once per frame.
    #[inline(always)]
    fn gain_for(&mut self, magnitude: f32) -> f32 {
        let level = magnitude.max(LEVEL_FLOOR);
        let level_db = level.log2() * DB_PER_LOG2;
        let over = level_db - self.set.threshold_db;

        // Gain reduction the static curve asks for, in dB and never positive.
        let slope = 1.0 / self.set.ratio - 1.0;
        let half_knee = self.set.knee_db * 0.5;
        let target = if over <= -half_knee {
            0.0
        } else if over >= half_knee || self.set.knee_db <= 0.0 {
            slope * over
        } else {
            // Parabola through both ends of the knee. At over = +half_knee it equals
            // slope * half_knee, and its derivative there equals the slope, so the curve is
            // continuous in value *and* in gradient - which is what stops the knee from being
            // audible as a corner.
            let t = over + half_knee;
            slope * t * t / (2.0 * self.set.knee_db)
        };

        // Stage 1: hold the deepest reduction the waveform asked for, let it go at the release
        // time. Deeper is taken immediately, so a transient is never missed.
        let released = self.hold_db + (target - self.hold_db) * self.release_coeff;
        self.hold_db = if target < released { target } else { released };

        // Stage 2: the audible attack.
        self.gain_db += (self.hold_db - self.gain_db) * self.attack_coeff;

        // Arrive exactly at unity instead of asymptotically - see GAIN_SNAP_DB.
        if self.hold_db > -GAIN_SNAP_DB {
            self.hold_db = 0.0;
            if self.gain_db > -GAIN_SNAP_DB {
                self.gain_db = 0.0;
            }
        }
        self.gain_db = flush(self.gain_db);
        if self.gain_db < self.peak_reduction_db {
            self.peak_reduction_db = self.gain_db;
        }

        db_to_linear(self.gain_db) * self.makeup.next()
    }
}

/// dB to a linear factor, in base 2 for the same reason as above.
#[inline(always)]
pub fn db_to_linear(db: f32) -> f32 {
    (db * (1.0 / DB_PER_LOG2)).exp2()
}

#[cfg(test)]
mod tests {
    use super::*;

    const RATE: u32 = 48_000;

    fn linear_to_db(x: f32) -> f32 {
        20.0 * x.log10()
    }

    /// Peak of the output for a steady 440 Hz sine, after the detector has settled.
    fn steady_output(comp: &mut Compressor, amplitude: f32, seconds: f32) -> f32 {
        let n = (RATE as f32 * seconds) as u64;
        let mut peak: f32 = 0.0;
        for i in 0..n {
            let x = amplitude * (std::f32::consts::TAU * 440.0 * i as f32 / RATE as f32).sin();
            let y = comp.process(Frame::mono(x)).l;
            // Only the last tenth counts, by which time attack and release have settled.
            if i > n * 9 / 10 {
                peak = peak.max(y.abs());
            }
        }
        peak
    }

    /// Times that make the detector's own ripple negligible, so a steady-state measurement really
    /// measures the static curve: a release far longer than one period of the test tone.
    fn steady_times() -> CompSettings {
        CompSettings {
            attack_ms: 5.0,
            release_ms: 300.0,
            ..Default::default()
        }
    }

    /// The definition of a ratio, checked as a number: 12 dB above a -24 dB threshold at 4:1 have
    /// to come out 3 dB above it, i.e. 9 dB of reduction.
    #[test]
    fn a_signal_over_the_threshold_is_reduced_by_the_ratio() {
        let set = CompSettings {
            threshold_db: -24.0,
            ratio: 4.0,
            // Hard knee, so the arithmetic is the plain ratio and nothing else.
            knee_db: 0.0,
            makeup_db: 0.0,
            ..steady_times()
        };
        let mut comp = Compressor::new(RATE, set);
        // -12 dBFS in, i.e. 12 dB over the threshold.
        let amplitude = 10f32.powf(-12.0 / 20.0);
        let out_db = linear_to_db(steady_output(&mut comp, amplitude, 1.0));
        assert!(
            (out_db - (-21.0)).abs() < 0.3,
            "erwartet -21 dBFS, gemessen {out_db}"
        );

        // Below the threshold nothing happens at all.
        let mut comp = Compressor::new(RATE, set);
        let quiet = 10f32.powf(-36.0 / 20.0);
        let out_db = linear_to_db(steady_output(&mut comp, quiet, 1.0));
        assert!((out_db - (-36.0)).abs() < 0.1, "unter Schwelle: {out_db}");
    }

    #[test]
    fn makeup_gain_is_added_after_the_reduction() {
        let set = CompSettings {
            threshold_db: -24.0,
            ratio: 4.0,
            knee_db: 0.0,
            makeup_db: 6.0,
            ..steady_times()
        };
        let mut comp = Compressor::new(RATE, set);
        let amplitude = 10f32.powf(-12.0 / 20.0);
        let out_db = linear_to_db(steady_output(&mut comp, amplitude, 1.0));
        assert!(
            (out_db - (-15.0)).abs() < 0.3,
            "-21 dBFS plus 6 dB Makeup, gemessen {out_db}"
        );
    }

    /// Attack and release are time constants of the gain: after one attack time 63 % of the
    /// reduction has happened, after one release time 63 % of it has recovered.
    #[test]
    fn attack_and_release_take_the_time_they_say() {
        // Full-scale DC, 24 dB over the threshold: the static curve asks for -18 dB.
        let expected_full = -18.0;

        // --- attack: a long release, so only stage 2 is being measured -----------------------
        let attack_ms = 20.0;
        let mut comp = Compressor::new(
            RATE,
            CompSettings {
                threshold_db: -24.0,
                ratio: 4.0,
                attack_ms,
                release_ms: 500.0,
                knee_db: 0.0,
                makeup_db: 0.0,
            },
        );
        let attack_samples = (attack_ms * 0.001 * RATE as f32) as u64;
        for _ in 0..attack_samples {
            comp.process(Frame::mono(1.0));
        }
        let after_attack = comp.gain_db;
        assert!(
            (after_attack - expected_full * 0.632).abs() < 0.4,
            "nach einer Attack-Zeit erwartet {:.2} dB, gemessen {after_attack:.2} dB",
            expected_full * 0.632
        );
        for _ in 0..(attack_samples * 8) {
            comp.process(Frame::mono(1.0));
        }
        assert!((comp.gain_db - expected_full).abs() < 0.2, "{}", comp.gain_db);

        // --- release: a short attack, so stage 2 barely lags behind stage 1 -------------------
        let release_ms = 200.0;
        let mut comp = Compressor::new(
            RATE,
            CompSettings {
                threshold_db: -24.0,
                ratio: 4.0,
                attack_ms: 2.0,
                release_ms,
                knee_db: 0.0,
                makeup_db: 0.0,
            },
        );
        for _ in 0..RATE / 2 {
            comp.process(Frame::mono(1.0));
        }
        let start = comp.gain_db;
        assert!((start - expected_full).abs() < 0.1, "Ausgangspunkt {start}");
        let release_samples = (release_ms * 0.001 * RATE as f32) as u64;
        for _ in 0..release_samples {
            comp.process(Frame::mono(0.0));
        }
        let after_release = comp.gain_db;
        assert!(
            (after_release - start * (1.0 - 0.632)).abs() < 0.5,
            "nach einer Release-Zeit erwartet {:.2} dB, gemessen {after_release:.2} dB",
            start * (1.0 - 0.632)
        );
    }

    /// The knee is the reason a voice does not wobble: at the threshold itself a little reduction
    /// is already happening, and the curve has no corner anywhere.
    #[test]
    fn the_soft_knee_bends_where_a_hard_knee_would_have_a_corner() {
        let set = CompSettings {
            threshold_db: -20.0,
            ratio: 4.0,
            knee_db: 8.0,
            makeup_db: 0.0,
            ..steady_times()
        };
        let mut comp = Compressor::new(RATE, set);
        // Exactly at the threshold a soft knee already reduces, a hard one does not.
        let at_threshold = linear_to_db(steady_output(&mut comp, 10f32.powf(-20.0 / 20.0), 1.0));
        assert!(
            at_threshold < -20.2 && at_threshold > -21.5,
            "an der Schwelle erwartet leicht reduziert, gemessen {at_threshold}"
        );
        // Below the knee nothing at all.
        let mut comp = Compressor::new(RATE, set);
        let under = linear_to_db(steady_output(&mut comp, 10f32.powf(-30.0 / 20.0), 1.0));
        assert!((under + 30.0).abs() < 0.1, "unter dem Knie: {under}");
        // Above the knee the plain ratio again: 10 dB over -20 at 4:1 is -17.5 dBFS.
        let mut comp = Compressor::new(RATE, set);
        let over = linear_to_db(steady_output(&mut comp, 10f32.powf(-10.0 / 20.0), 1.0));
        assert!((over + 17.5).abs() < 0.3, "ueber dem Knie: {over}");
    }

    /// A ratio of 1 is a wire, whatever the other knobs say. Worth a test because it is the state
    /// a "Kompressor aus" could accidentally be built from.
    #[test]
    fn a_ratio_of_one_changes_nothing() {
        let mut comp = Compressor::new(
            RATE,
            CompSettings {
                ratio: 1.0,
                threshold_db: -40.0,
                makeup_db: 0.0,
                ..Default::default()
            },
        );
        for i in 0..1_000 {
            let x = (i as f32 / 500.0) - 1.0;
            assert!((comp.process(Frame::mono(x)).l - x).abs() < 1e-6, "Sample {i}");
        }
    }

    /// The reason there is one detector and not two: the ratio between left and right has to
    /// survive the compressor unchanged, however hard it works. Two independent compressors would
    /// pull the loud side down and leave the quiet one, i.e. move the instrument across the stage.
    #[test]
    fn a_stereo_signal_keeps_its_image_however_hard_the_compressor_works() {
        let set = CompSettings {
            threshold_db: -30.0,
            ratio: 10.0,
            knee_db: 0.0,
            makeup_db: 0.0,
            ..steady_times()
        };
        let mut comp = Compressor::new(RATE, set);
        // The right channel is a quarter of the left one throughout - 12 dB of image.
        for i in 0..RATE {
            let x = (std::f32::consts::TAU * 220.0 * i as f32 / RATE as f32).sin();
            let out = comp.process(Frame::new(0.9 * x, 0.225 * x));
            if i > RATE / 2 && out.l.abs() > 1e-4 {
                let ratio = out.r / out.l;
                assert!(
                    (ratio - 0.25).abs() < 1e-5,
                    "Sample {i}: das Stereobild ist auf {ratio} gewandert"
                );
            }
        }
        assert!(comp.gain_db < -6.0, "der Kompressor muss hier wirklich arbeiten");
    }

    /// A mono track is fanned out to two identical channels before the chain, so the detector's
    /// maximum is that one channel and the compressor behaves exactly as it did in the mono engine.
    #[test]
    fn a_fanned_out_mono_signal_compresses_exactly_as_one_channel_would() {
        let set = CompSettings {
            threshold_db: -24.0,
            ratio: 4.0,
            knee_db: 8.0,
            makeup_db: 3.0,
            ..steady_times()
        };
        let mut comp = Compressor::new(RATE, set);
        for i in 0..RATE {
            let x = (std::f32::consts::TAU * 330.0 * i as f32 / RATE as f32).sin() * 0.6;
            let out = comp.process(Frame::mono(x));
            assert_eq!(out.l, out.r, "Sample {i}: die beiden Kanaele muessen gleich bleiben");
        }
    }

    #[test]
    fn silence_drives_the_state_to_exact_zero() {
        let mut comp = Compressor::new(RATE, CompSettings::default());
        for _ in 0..1_000 {
            comp.process(Frame::mono(1.0));
        }
        for _ in 0..RATE * 4 {
            comp.process(Frame::mono(0.0));
        }
        assert_eq!(comp.gain_db, 0.0, "kein Denormal im Detektor");
        assert_eq!(comp.process(Frame::mono(0.0)), Frame::SILENT);
    }

    #[test]
    fn settings_are_clamped_to_something_usable() {
        let mut comp = Compressor::new(RATE, CompSettings::default());
        comp.set_settings(CompSettings {
            threshold_db: 40.0,
            ratio: -3.0,
            attack_ms: 0.0,
            release_ms: 100_000.0,
            knee_db: -5.0,
            makeup_db: 99.0,
        });
        let s = comp.settings();
        assert_eq!(s.threshold_db, 0.0);
        assert_eq!(s.ratio, 1.0);
        assert_eq!(s.attack_ms, 0.1);
        assert_eq!(s.release_ms, 2_000.0);
        assert_eq!(s.knee_db, 0.0);
        assert_eq!(s.makeup_db, 24.0);
    }
}
