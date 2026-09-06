//! Reverb, Freeverb style - eight parallel comb filters into four allpass filters in series, per
//! channel.
//!
//! The plan calls for "ein einfaches Modell nach Freeverb-Art"; this is that model with its
//! original tunings. Two things are worth knowing about it:
//!
//! * The comb delays (1116, 1188, ... samples) are mutually prime at 44.1 kHz. That is the whole
//!   trick: eight decaying repeats whose periods share no common factor sum into something that
//!   has no audible period of its own, which is what a room sounds like. They are scaled by
//!   `sample_rate / 44100` here so the character does not change with the sample rate.
//! * The allpass filters do not change the magnitude spectrum at all; they smear the phase, which
//!   is what turns eight distinct echoes into a wash.
//!
//! # Stereo, the way Freeverb does it: `stereospread`
//!
//! The right-hand bank of combs and allpasses is the same set of filters with every tuning
//! lengthened by [`STEREO_SPREAD`] = 23 samples - Freeverb's own constant. That single offset is
//! what makes the reverb stereo:
//!
//! * The two channels then have **different** comb periods, so their echo patterns are
//!   uncorrelated. Uncorrelated noise on the two ears is what the brain reads as "a room around
//!   me"; identical noise on both ears is read as "a point source in the middle of my head".
//! * The offset is small enough (half a millisecond) that a transient still arrives on both sides
//!   at effectively the same instant, so nothing in the *dry* image is pulled sideways.
//!
//! Both banks are fed the same signal, `(l + r) * 0.5` - the reverb is a room, and a room does not
//! keep the left and the right half of what is played in it apart. The 0.5 is what makes a mono
//! track, which arrives with `l == r`, excite the room exactly as loudly as it did before this
//! module knew about stereo; without it switching to stereo would have raised every reverb by 6 dB.
//!
//! # Memory
//!
//! Comb and allpass buffers together are about 28 000 samples at 48 kHz, i.e. **112 kB per track**,
//! 900 kB for the eight tracks the engine allows. All of it is allocated in the control thread
//! when the track is built, and never resized.
//!
//! # Denormals - this is the module the whole denormal discussion is about
//!
//! A reverb is a bank of feedback loops with a decay time of seconds. When the input stops, every
//! one of those loops keeps multiplying its content by something like 0.85 forever, and after a
//! few seconds the values are in the denormal range. On x86 without flush-to-zero a denormal
//! multiply is one to two orders of magnitude slower than a normal one, so a *silent* reverb is
//! the most expensive state a reverb can be in - which is exactly the case a looper is in most of
//! the time. Every feedback store therefore goes through `flush` (see `fx::biquad`), and the chain
//! on top of that stops calling the reverb at all once the tail has run out.

use super::super::frame::Frame;
use super::biquad::flush;
use super::smooth::Smoothed;

/// Freeverb's comb tunings, in samples at 44.1 kHz.
const COMB_TUNING: [usize; 8] = [1116, 1188, 1277, 1356, 1422, 1491, 1557, 1617];
/// Freeverb's allpass tunings, in samples at 44.1 kHz.
const ALLPASS_TUNING: [usize; 4] = [556, 441, 341, 225];
const TUNING_RATE: f64 = 44_100.0;
/// How much longer every delay of the right-hand bank is, in samples at 44.1 kHz. Freeverb's own
/// `stereospread`; see the module comment for what it buys.
const STEREO_SPREAD: usize = 23;

/// Input attenuation. Eight combs with a feedback near 0.85 have a lot of gain between them; this
/// is the factor that brings the sum back to something like unity. Freeverb's own value.
const FIXED_GAIN: f32 = 0.015;
/// Output scaling, likewise Freeverb's.
const WET_SCALE: f32 = 3.0;
/// Room size maps to comb feedback in this range: 0.7 is a small room, 0.98 is a hall that rings
/// for many seconds. Above 1.0 it would not decay at all, which is a freeze effect, not a reverb.
const ROOM_OFFSET: f32 = 0.7;
const ROOM_SCALE: f32 = 0.28;
/// Damping maps to the one-pole inside each comb. A full 1.0 would kill the loop completely, so
/// the range stops well short of it.
const DAMP_SCALE: f32 = 0.4;

/// One comb filter with a one-pole lowpass in its feedback path. The lowpass is the damping: each
/// pass through the loop loses a little top end, which is what a real room does with soft walls.
struct Comb {
    buffer: Vec<f32>,
    index: usize,
    /// State of the damping lowpass.
    store: f32,
    feedback: f32,
    damp1: f32,
    damp2: f32,
}

impl Comb {
    fn new(len: usize) -> Self {
        Self {
            buffer: vec![0.0; len.max(1)],
            index: 0,
            store: 0.0,
            feedback: 0.5,
            damp1: 0.5,
            damp2: 0.5,
        }
    }

    fn set_damp(&mut self, damp: f32) {
        self.damp1 = damp;
        self.damp2 = 1.0 - damp;
    }

    #[inline(always)]
    fn process(&mut self, x: f32) -> f32 {
        let len = self.buffer.len();
        let out = match self.buffer.get(self.index) {
            Some(&v) => v,
            None => 0.0,
        };
        self.store = flush(out * self.damp2 + self.store * self.damp1);
        if let Some(slot) = self.buffer.get_mut(self.index) {
            *slot = flush(x + self.store * self.feedback);
        }
        self.index += 1;
        if self.index >= len {
            self.index = 0;
        }
        out
    }

    fn clear(&mut self) {
        self.buffer.fill(0.0);
        self.store = 0.0;
        self.index = 0;
    }
}

/// Schroeder allpass, feedback fixed at 0.5 as in Freeverb.
struct Allpass {
    buffer: Vec<f32>,
    index: usize,
}

impl Allpass {
    fn new(len: usize) -> Self {
        Self {
            buffer: vec![0.0; len.max(1)],
            index: 0,
        }
    }

    #[inline(always)]
    fn process(&mut self, x: f32) -> f32 {
        let len = self.buffer.len();
        let buffered = match self.buffer.get(self.index) {
            Some(&v) => v,
            None => 0.0,
        };
        let out = buffered - x;
        if let Some(slot) = self.buffer.get_mut(self.index) {
            *slot = flush(x + buffered * 0.5);
        }
        self.index += 1;
        if self.index >= len {
            self.index = 0;
        }
        out
    }

    fn clear(&mut self) {
        self.buffer.fill(0.0);
        self.index = 0;
    }
}

/// One channel's worth of the model: eight combs in parallel into four allpasses in series.
struct Bank {
    combs: Vec<Comb>,
    allpasses: Vec<Allpass>,
}

impl Bank {
    /// `spread` lengthens every delay of this bank, which is what makes the right-hand bank
    /// different from the left-hand one.
    fn new(sample_rate: u32, spread: usize) -> Self {
        let scale = sample_rate as f64 / TUNING_RATE;
        let len = |n: usize| (((n + spread) as f64) * scale).round() as usize;
        Self {
            combs: COMB_TUNING.iter().map(|&n| Comb::new(len(n))).collect(),
            allpasses: ALLPASS_TUNING.iter().map(|&n| Allpass::new(len(n))).collect(),
        }
    }

    fn buffered_samples(&self) -> usize {
        self.combs.iter().map(|c| c.buffer.len()).sum::<usize>()
            + self.allpasses.iter().map(|a| a.buffer.len()).sum::<usize>()
    }

    fn longest_comb(&self) -> usize {
        self.combs.iter().map(|c| c.buffer.len()).max().unwrap_or(1)
    }

    fn clear(&mut self) {
        for comb in &mut self.combs {
            comb.clear();
        }
        for ap in &mut self.allpasses {
            ap.clear();
        }
    }

    #[inline(always)]
    fn process(&mut self, input: f32) -> f32 {
        let mut wet = 0.0f32;
        for comb in &mut self.combs {
            wet += comb.process(input);
        }
        for ap in &mut self.allpasses {
            wet = ap.process(wet);
        }
        wet
    }
}

pub struct Reverb {
    /// Left and right, the right one detuned by [`STEREO_SPREAD`].
    banks: [Bank; 2],
    size: f32,
    damping: f32,
    mix: Smoothed,
    /// Longest tail this setting can produce, in samples. The chain uses it to decide when a
    /// silent track may be skipped entirely.
    tail_samples: u32,
    sample_rate: u32,
}

impl Reverb {
    /// Allocates every comb and allpass buffer of both banks. Control thread only.
    pub fn new(sample_rate: u32, size: f32, damping: f32, mix: f32) -> Self {
        let mut r = Self {
            banks: [
                Bank::new(sample_rate, 0),
                Bank::new(sample_rate, STEREO_SPREAD),
            ],
            size: 0.0,
            damping: 0.0,
            mix: Smoothed::new(mix.clamp(0.0, 1.0), 60.0, sample_rate),
            tail_samples: sample_rate,
            sample_rate,
        };
        r.set_size(size);
        r.set_damping(damping);
        r
    }

    /// Total number of samples the buffers occupy, both banks together - the number the module
    /// comment quotes.
    pub fn buffered_samples(&self) -> usize {
        self.banks.iter().map(Bank::buffered_samples).sum()
    }

    pub fn size(&self) -> f32 {
        self.size
    }

    pub fn damping(&self) -> f32 {
        self.damping
    }

    pub fn mix(&self) -> f32 {
        self.mix.target()
    }

    pub fn set_size(&mut self, size: f32) {
        self.size = size.clamp(0.0, 1.0);
        let feedback = ROOM_OFFSET + self.size * ROOM_SCALE;
        for bank in &mut self.banks {
            for comb in &mut bank.combs {
                comb.feedback = feedback;
            }
        }
        self.recompute_tail();
    }

    pub fn set_damping(&mut self, damping: f32) {
        self.damping = damping.clamp(0.0, 1.0);
        let damp = self.damping * DAMP_SCALE;
        for bank in &mut self.banks {
            for comb in &mut bank.combs {
                comb.set_damp(damp);
            }
        }
        self.recompute_tail();
    }

    pub fn set_mix(&mut self, mix: f32) {
        self.mix.set(mix.clamp(0.0, 1.0));
    }

    pub fn snap_mix(&mut self, mix: f32) {
        self.mix.snap_to(mix.clamp(0.0, 1.0));
    }

    /// How long the tail takes to fall by 60 dB, worked out from the longest comb and its
    /// feedback. Only an estimate - the damping shortens it further - so it is rounded generously
    /// upwards and used purely as a "when may this be skipped" bound.
    fn recompute_tail(&mut self) {
        let feedback = (ROOM_OFFSET + self.size * ROOM_SCALE).clamp(0.01, 0.999) as f64;
        // The right-hand bank has the longer combs, so its tail is the one that bounds both.
        let longest = self
            .banks
            .iter()
            .map(Bank::longest_comb)
            .max()
            .unwrap_or(1) as f64;
        // passes * 20*log10(feedback) = -60 dB
        let passes = -60.0 / (20.0 * feedback.log10());
        let samples = passes * longest * 2.0;
        self.tail_samples = samples.clamp(0.0, (self.sample_rate * 30) as f64) as u32;
    }

    /// Upper bound of the tail in samples, for the chain's idle detection.
    pub fn tail_samples(&self) -> u32 {
        self.tail_samples
    }

    /// Empty every buffer. Control thread only - this is a memset of the whole reverb.
    pub fn clear(&mut self) {
        for bank in &mut self.banks {
            bank.clear();
        }
    }

    /// One frame. Wired as an aux send, like the delay: the dry signal comes out untouched and the
    /// tail is added on top, so raising the reverb never makes the source quieter.
    ///
    /// Both banks are excited by the same `(l + r) * 0.5` - see the module comment - and each
    /// returns its own tail, so the wet signal is genuinely two different rooms' worth of noise
    /// while the dry signal keeps its own image untouched.
    #[inline(always)]
    pub fn process(&mut self, x: Frame) -> Frame {
        let input = (x.l + x.r) * 0.5 * FIXED_GAIN;
        let left = self.banks[0].process(input);
        let right = self.banks[1].process(input);
        let gain = WET_SCALE * self.mix.next();
        Frame {
            l: x.l + left * gain,
            r: x.r + right * gain,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const RATE: u32 = 48_000;

    fn reverb(size: f32, damping: f32, mix: f32) -> Reverb {
        let mut r = Reverb::new(RATE, size, damping, mix);
        r.snap_mix(mix);
        r
    }

    /// The two properties that decide whether a reverb is usable at all: it has to die away, and
    /// it must never produce a value that is not a number.
    #[test]
    fn the_tail_decays_and_never_runs_away() {
        // The biggest room the model offers with no damping at all, i.e. the worst case for
        // stability. Anything unstable in this model shows up here or nowhere.
        let mut r = reverb(1.0, 0.0, 1.0);
        r.process(Frame::mono(1.0));
        let window = RATE as usize;
        let mut energies = Vec::new();
        for _ in 0..20 {
            let mut sum = 0.0f64;
            for _ in 0..window {
                let y = r.process(Frame::SILENT);
                assert!(y.l.is_finite() && y.r.is_finite(), "Ausgabe ist NaN oder unendlich");
                sum += (y.l as f64) * (y.l as f64) + (y.r as f64) * (y.r as f64);
            }
            energies.push((sum / window as f64).sqrt());
        }
        // A reverb builds up before it decays, so the first seconds are exempt; from then on every
        // second has to be quieter than the one before.
        for i in 3..energies.len() {
            assert!(
                energies[i] < energies[i - 1],
                "Sekunde {i} ({}) ist nicht leiser als Sekunde {} ({})",
                energies[i],
                i - 1,
                energies[i - 1]
            );
        }
        let loudest = energies.iter().cloned().fold(0.0f64, f64::max);
        let last = energies[energies.len() - 1];
        assert!(
            last < loudest * 1e-3,
            "nach 20 Sekunden noch {last} gegen einen Hoechstwert von {loudest}"
        );
    }

    /// The denormal cure, at the place it matters most: a reverb left running in silence.
    #[test]
    fn a_silent_reverb_reaches_exact_zero_instead_of_denormals() {
        let mut r = reverb(0.5, 0.5, 1.0);
        for _ in 0..1_000 {
            r.process(Frame::mono(0.5));
        }
        for _ in 0..RATE * 20 {
            r.process(Frame::SILENT);
        }
        assert_eq!(
            r.process(Frame::SILENT),
            Frame::SILENT,
            "Ausgabe muss echt null werden"
        );
        for bank in &r.banks {
            for comb in &bank.combs {
                assert!(comb.buffer.iter().all(|&v| v == 0.0), "Comb nicht leer");
                assert_eq!(comb.store, 0.0);
            }
            for ap in &bank.allpasses {
                assert!(ap.buffer.iter().all(|&v| v == 0.0), "Allpass nicht leer");
            }
        }
    }

    /// A wet share of zero is a wire, bit for bit - the chain relies on it when the reverb is off.
    #[test]
    fn a_wet_share_of_zero_passes_the_signal_through_unchanged() {
        let mut r = reverb(0.7, 0.4, 0.0);
        for i in 0..10_000 {
            let x = Frame::new(((i % 131) as f32 / 131.0) - 0.5, ((i % 97) as f32 / 97.0) - 0.5);
            assert_eq!(r.process(x), x, "Sample {i}");
        }
    }

    /// A bigger room has to ring longer than a small one, and damping has to shorten the tail.
    /// Those are the two knobs, so they had better do what their names say.
    #[test]
    fn size_makes_the_tail_longer_and_damping_makes_it_shorter() {
        fn tail_energy(size: f32, damping: f32) -> f64 {
            let mut r = reverb(size, damping, 1.0);
            for _ in 0..64 {
                r.process(Frame::mono(1.0));
            }
            let mut energy = 0.0f64;
            for i in 0..RATE * 3 {
                let y = r.process(Frame::SILENT);
                // Only what is left after the first second counts as "tail".
                if i > RATE {
                    energy += (y.l as f64) * (y.l as f64) + (y.r as f64) * (y.r as f64);
                }
            }
            energy
        }
        let small = tail_energy(0.1, 0.2);
        let large = tail_energy(0.95, 0.2);
        assert!(large > small * 4.0, "gross {large}, klein {small}");

        let bright = tail_energy(0.8, 0.0);
        let dark = tail_energy(0.8, 1.0);
        assert!(dark < bright, "gedaempft {dark}, offen {bright}");
    }

    /// The point of the whole stereo rebuild of this module: the two channels have to carry
    /// *different* noise. Identical tails on both ears are a point source in the middle of the
    /// head, which is exactly what a mono reverb sounds like.
    #[test]
    fn the_two_channels_carry_different_tails() {
        let mut r = reverb(0.7, 0.3, 1.0);
        // A mono impulse - the hard case: the two banks see the identical excitation and only
        // their own tunings can tell them apart.
        r.process(Frame::mono(1.0));
        let mut identical = 0usize;
        let mut different = 0usize;
        let mut energy_l = 0.0f64;
        let mut energy_r = 0.0f64;
        for _ in 0..RATE {
            let y = r.process(Frame::SILENT);
            if y.l == y.r {
                identical += 1;
            } else {
                different += 1;
            }
            energy_l += (y.l as f64) * (y.l as f64);
            energy_r += (y.r as f64) * (y.r as f64);
        }
        assert!(
            different > identical * 10,
            "die beiden Kanaele sind zu oft gleich ({identical} gleich, {different} verschieden)"
        );
        // Different, but not lopsided: both sides have to carry about the same amount of reverb.
        let ratio = energy_l / energy_r;
        assert!(
            (0.5..2.0).contains(&ratio),
            "die Seiten sind ungleich laut, Verhaeltnis {ratio}"
        );
    }

    #[test]
    fn the_buffers_are_the_size_the_comment_promises() {
        let r = reverb(0.5, 0.5, 0.2);
        let samples = r.buffered_samples();
        // 12 587 samples at 44.1 kHz per bank, plus 12 * 23 samples of stereo spread on the right
        // one, scaled to 48 kHz - and then twice, because there are two banks.
        assert!(
            (26_000..30_000).contains(&samples),
            "{samples} Samples in den Puffern"
        );
        assert!(r.tail_samples() > RATE / 2);
    }
}
