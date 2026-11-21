//! Octaver: one octave down, for a monophonically played guitar that is meant to sound like a bass.
//!
//! # The method: half-speed playback, spliced on the period
//!
//! An octave down is the input played at half speed. Reading a delay line at half the speed it is
//! written makes exactly that - every frequency of the input halved, with no pitch tracker in the
//! signal path that could get the note wrong - but the read pointer falls behind by half a sample
//! per sample, so every now and then it has to jump forward again. Where it jumps is what decides
//! whether this sounds like a bass or like a broken tape machine:
//!
//! * A jump by an arbitrary distance lands at a different point of the waveform. Crossfaded, the
//!   two taps cancel and reinforce each other at the splice rate: the "warble" of the cheap
//!   delay-line shifters.
//! * A jump by a whole number of **periods** lands on the same point of the next cycle. For a
//!   sustained note the two taps then carry the same waveform and the crossfade between them is
//!   inaudible.
//!
//! So the jump is chosen by correlation, at the moment it is needed: the stretch of input around
//! the read position is compared with the stretches one candidate distance later, for every
//! distance between half and one longest period ([`LOWEST_HZ`]), and the best match wins - among
//! matches about as good as the best, the longest one, which keeps the delay short. That interval
//! always contains a multiple of the period of any note up to 70 Hz input, so there is always a
//! good splice for a single string. It does not matter which multiple is found, so the octave
//! errors that make pitch trackers hard are irrelevant here: the pitch is fixed by the read speed,
//! the search only picks a seamless place to cut.
//!
//! The search runs on a 12 kHz copy of the input (low-passed and decimated as it is written), then
//! is refined at full rate around the winner. That makes one splice cost about twenty thousand
//! multiply-adds, once every 15 to 30 ms - see `fxbench`.
//!
//! This is the time-domain relative of PSOLA, with one difference that is deliberate: PSOLA keeps
//! the formants where they were and moves only the pitch; half-speed playback moves the whole
//! spectrum down an octave, body resonance included. A guitar with its formants kept sounds like a
//! guitar with a sub-oscillator. Moved down with it, it sounds like a heavier string in a bigger
//! body - which is what a bass is.
//!
//! # Attacks
//!
//! The pointer is behind the input by anything from a few to fifteen milliseconds, depending on
//! where in its cycle it is. Left alone, a new note would therefore start at a random moment, and
//! the end of the old note would be crossfaded into its attack - gargling. So the octaver listens
//! for plucks (a fast envelope jumping well above a slow one, on a high-passed copy of the input)
//! and on each one moves the pointer straight to just before the attack, with a short crossfade
//! away from the old note. Every note therefore starts the same, short time after it was played:
//! [`Octaver::latency`] frames. That is the latency the engine compensates for on loop playback.
//! The sustain behind the attack drifts by up to a cycle of the read pointer later than that,
//! which nobody hears on a note that is already sounding.
//!
//! # What it does with things it was not made for
//!
//! * **Silence and noise**: played at half speed, which is what they are. No gate, no tracker that
//!   could lock onto hum.
//! * **Two strings ringing**: the input has no single period, so the best splice is a compromise
//!   and the crossfades become audible as a slight flutter. Nothing jumps or blows up.
//! * **Chords**: the same, more so. They come out an octave lower and smeared, never louder.
//!
//! # Real time
//!
//! Every buffer is allocated in [`Octaver::new`]; the audio path only reads and writes them. The
//! search is a bounded loop over a fixed number of lags. The detector's envelopes are flushed so a
//! silent octaver does not idle in the denormal range.

use super::super::frame::Frame;
use super::biquad::{Biquad, Coeffs, flush};
use super::smooth::Smoothed;

/// Lowest input note the splice search is built for. 70 Hz is below the low E of a guitar in
/// standard tuning (82.4 Hz) and the low D of drop-D (73.4 Hz).
pub const LOWEST_HZ: f32 = 70.0;
/// How far ahead of the read position the correlation looks, and so the shortest delay the read
/// pointer may have. Short, because the attack latency is built on it.
const LOOKAHEAD_MS: f32 = 1.0;
/// How long before a detected attack the pointer is placed. Read at half speed this becomes twice
/// as long at the output, so it is the main ingredient of [`Octaver::latency`].
const PRE_ROLL_MS: f32 = 1.5;
/// Crossfade from the old note to the new one on an attack.
const ATTACK_FADE_MS: f32 = 3.0;
/// Past context of the full-rate refinement of a splice.
const REFINE_MS: f32 = 5.0;
/// The rate the coarse splice search runs at.
const SEARCH_RATE: u32 = 12_000;
/// A match counts as "as good as the best" within this much normalised correlation.
const MATCH_TOLERANCE: f32 = 0.04;
/// Attack detector: the fast envelope has to rise this far above the slow one ...
const ONSET_RATIO: f32 = 2.0;
/// ... and above this absolute level (-46 dBFS), so noise between notes does not trigger it.
const ONSET_FLOOR: f32 = 0.005;
/// Shortest time between two detected attacks.
const ONSET_HOLD_MS: f32 = 60.0;
const FAST_RELEASE_MS: f32 = 15.0;
const SLOW_MS: f32 = 80.0;
/// High-pass in front of the detector: a pluck is a burst of top end, a sustained note mostly
/// fundamental, so this is where the two differ most.
const ONSET_HP_HZ: f32 = 300.0;
/// Level changes are smoothed over this.
const GAIN_MS: f32 = 20.0;

/// The octaver's knobs.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct OctaverSettings {
    /// Level of the original signal, 0..1.
    pub dry: f32,
    /// Level of the octave below, 0..1.
    pub sub: f32,
    /// Low-pass on the octave voice, in Hz.
    pub tone_hz: f32,
    /// Saturation of the octave voice, 0..1. See [`Drive`].
    pub drive: f32,
}

impl Default for OctaverSettings {
    fn default() -> Self {
        Self {
            dry: 1.0,
            sub: 1.0,
            tone_hz: 2_000.0,
            drive: 0.0,
        }
    }
}

impl OctaverSettings {
    pub fn clamped(self) -> Self {
        Self {
            dry: clamp_level(self.dry),
            sub: clamp_level(self.sub),
            tone_hz: if self.tone_hz.is_nan() { 2_000.0 } else { self.tone_hz.clamp(100.0, 8_000.0) },
            drive: clamp_level(self.drive),
        }
    }
}

/// Largest gain in front of the saturation, at `drive` 1: +24 dB.
const MAX_DRIVE_GAIN: f32 = 16.0;
/// Release of the envelope the saturation is normalised to. Longer than a period of the lowest
/// octave (41 Hz = 24 ms), so the normalisation does not follow the waveform itself.
const DRIVE_RELEASE_MS: f32 = 60.0;
/// Below this envelope (-80 dBFS) nothing is normalised up: noise stays noise.
const DRIVE_FLOOR: f32 = 1.0e-4;

/// The soft clipper: the (3,2) Pade approximation of tanh, exact enough below |x| = 3 and clamped
/// to +-1 above, where tanh is 0.995. Smooth, odd-symmetric (no DC to remove), and no
/// transcendental call per sample.
#[inline(always)]
fn soft_clip(x: f32) -> f32 {
    let v = x.clamp(-3.0, 3.0);
    let v2 = v * v;
    v * (27.0 + v2) / (27.0 + 9.0 * v2)
}

/// Saturation of the octave voice, **at a fixed depth whatever the playing level**.
///
/// Why it exists at all: a guitar played an octave down has very little left above a few hundred
/// hertz - measured on a piezo stem, the octave voice is about 30 dB down at 1 kHz from its 100 Hz
/// band, where a bass DI is roughly 15 to 20 down. A bass on a small speaker is heard through its
/// upper harmonics rather than its fundamental, and an EQ cannot lift what is not there.
/// Saturation makes them from the fundamental, which is what a bass amp does too.
///
/// Why normalised: a plain clipper behind a gain saturates a loud note and leaves a quiet one
/// alone, and the same knob setting then means something different for every input gain. So the
/// signal is divided by its own envelope (instant attack, [`DRIVE_RELEASE_MS`] release), clipped
/// behind the drive gain, scaled so a peak comes out at the level it went in, and multiplied by the
/// envelope again. The result keeps the note's dynamics and gets the same harmonics at -30 dBFS as
/// at -6.
#[derive(Clone, Copy, Debug)]
struct Drive {
    gain: f32,
    /// `1 / soft_clip(gain)`: a full-envelope peak leaves at its own level.
    makeup: f32,
    env: f32,
    release: f32,
}

impl Drive {
    fn new(sample_rate: u32) -> Self {
        Self {
            gain: 1.0,
            makeup: 1.0,
            env: 0.0,
            release: (-1.0 / (DRIVE_RELEASE_MS * 0.001 * sample_rate as f32)).exp(),
        }
    }

    /// `drive` 0..1, exponential so the travel is even in dB: 0 -> +0, 0.5 -> +12, 1 -> +24 dB.
    fn set(&mut self, drive: f32) {
        self.gain = MAX_DRIVE_GAIN.powf(drive);
        self.makeup = 1.0 / soft_clip(self.gain);
    }

    #[inline(always)]
    fn process(&mut self, x: f32) -> f32 {
        let a = x.abs();
        self.env = flush(if a > self.env { a } else { self.env * self.release });
        if self.gain <= 1.0 + 1.0e-4 || self.env < DRIVE_FLOOR {
            return x;
        }
        let u = x / self.env;
        self.env * soft_clip(u * self.gain) * self.makeup
    }
}

fn clamp_level(v: f32) -> f32 {
    if v.is_nan() { 0.0 } else { v.clamp(0.0, 1.0) }
}

/// The latency of an octaver at this rate, without building one: twice the pre-roll, because the
/// pre-roll is read at half speed.
pub fn latency_frames(sample_rate: u32) -> u32 {
    let lookahead = ms(LOOKAHEAD_MS, sample_rate);
    (2 * ms(PRE_ROLL_MS, sample_rate).max(lookahead + 4)) as u32
}

/// Smallest power of two that is at least `n`.
fn pow2(n: usize) -> usize {
    n.max(2).next_power_of_two()
}

fn ms(ms: f32, sample_rate: u32) -> usize {
    (ms * 0.001 * sample_rate as f32).round().max(1.0) as usize
}

pub struct Octaver {
    sample_rate: u32,
    set: OctaverSettings,
    dry_gain: Smoothed,
    sub_gain: Smoothed,
    drive: Drive,
    tone: Biquad,

    /// Mono input, indexed by absolute sample number masked to the length.
    buf: Vec<f32>,
    mask: usize,
    /// Samples written so far, offset by the buffer length so the past reads as silence.
    written: u64,

    /// Low-passed copy at roughly [`SEARCH_RATE`]: entry `k` belongs to input sample `k * factor`.
    coarse: Vec<f32>,
    coarse_mask: usize,
    factor: usize,
    coarse_lp: [Biquad; 2],

    /// Read pointer in half samples, so half-speed reading stays exact forever.
    tap2: u64,
    /// The tap being faded out, while a fade runs.
    old2: u64,
    fade_pos: usize,
    fade_len: usize,

    /// The dry signal, delayed by the latency so it stays together with the octave.
    dry_l: Vec<f32>,
    dry_r: Vec<f32>,
    dry_mask: usize,

    // Splice geometry, in input samples.
    min_delay: usize,
    jump_lo: usize,
    jump_hi: usize,
    lookahead: usize,
    pre_roll: usize,
    attack_fade: usize,
    refine: usize,
    max_delay: usize,

    // Attack detector.
    onset_hp: Biquad,
    env_fast: f32,
    env_slow: f32,
    fast_release: f32,
    slow_coeff: f32,
    hold: usize,
    since_onset: usize,
}

impl Octaver {
    /// Allocates every buffer. **Control thread only.** About 40 kB at 48 kHz.
    pub fn new(sample_rate: u32) -> Self {
        let jump_hi = (sample_rate as f32 / LOWEST_HZ).round() as usize;
        let lookahead = ms(LOOKAHEAD_MS, sample_rate);
        let refine = ms(REFINE_MS, sample_rate);
        // Interpolation reads two samples past the pointer.
        let min_delay = lookahead + 4;
        let pre_roll = ms(PRE_ROLL_MS, sample_rate).max(min_delay);
        let attack_fade = ms(ATTACK_FADE_MS, sample_rate);
        // The pointer drifts to at most min_delay + jump_hi before a splice, plus whatever an
        // attack fade adds; the search reaches one more longest period into the past.
        let max_delay = min_delay + jump_hi + attack_fade + 8;
        let len = pow2(max_delay + 2 * jump_hi + refine + 64);
        let factor = (sample_rate / SEARCH_RATE).max(1) as usize;
        let coarse_len = pow2(len / factor + 8);
        let latency = 2 * pre_roll;
        let dry_len = pow2(latency + 1);
        let coarse_lp = Coeffs::low_pass(
            sample_rate,
            0.18 * sample_rate as f32 / factor as f32,
            std::f32::consts::FRAC_1_SQRT_2,
        );
        let set = OctaverSettings::default();
        let mut o = Self {
            sample_rate,
            set,
            dry_gain: Smoothed::new(set.dry, GAIN_MS, sample_rate),
            sub_gain: Smoothed::new(set.sub, GAIN_MS, sample_rate),
            drive: Drive::new(sample_rate),
            tone: Biquad::new(Coeffs::low_pass(
                sample_rate,
                set.tone_hz,
                std::f32::consts::FRAC_1_SQRT_2,
            )),
            buf: vec![0.0; len],
            mask: len - 1,
            written: 0,
            coarse: vec![0.0; coarse_len],
            coarse_mask: coarse_len - 1,
            factor,
            coarse_lp: [Biquad::new(coarse_lp); 2],
            tap2: 0,
            old2: 0,
            fade_pos: 0,
            fade_len: 0,
            dry_l: vec![0.0; dry_len],
            dry_r: vec![0.0; dry_len],
            dry_mask: dry_len - 1,
            min_delay,
            jump_lo: jump_hi / 2,
            jump_hi,
            lookahead,
            pre_roll,
            attack_fade,
            refine,
            max_delay,
            onset_hp: Biquad::new(Coeffs::high_pass(
                sample_rate,
                ONSET_HP_HZ,
                std::f32::consts::FRAC_1_SQRT_2,
            )),
            env_fast: 0.0,
            env_slow: 0.0,
            fast_release: (-1.0 / (FAST_RELEASE_MS * 0.001 * sample_rate as f32)).exp(),
            slow_coeff: 1.0 - (-1.0 / (SLOW_MS * 0.001 * sample_rate as f32)).exp(),
            hold: ms(ONSET_HOLD_MS, sample_rate),
            since_onset: 0,
        };
        o.reset();
        o
    }

    /// Frames between a note going in and its octave (and the delayed dry signal) coming out.
    pub fn latency(&self) -> u32 {
        (2 * self.pre_roll) as u32
    }

    pub fn settings(&self) -> OctaverSettings {
        self.set
    }

    pub fn set_settings(&mut self, s: OctaverSettings) {
        let s = s.clamped();
        if s.tone_hz != self.set.tone_hz {
            self.tone.set(Coeffs::low_pass(
                self.sample_rate,
                s.tone_hz,
                std::f32::consts::FRAC_1_SQRT_2,
            ));
        }
        self.dry_gain.set(s.dry);
        self.sub_gain.set(s.sub);
        self.drive.set(s.drive);
        self.set = s;
    }

    /// Back to silence. Called when the octaver is switched on, so it does not start with whatever
    /// it heard the last time. Clears about 40 kB - no allocation.
    pub fn reset(&mut self) {
        self.buf.fill(0.0);
        self.coarse.fill(0.0);
        self.dry_l.fill(0.0);
        self.dry_r.fill(0.0);
        // Start "one buffer in", so every index into the past is valid and reads silence.
        self.written = self.buf.len() as u64;
        self.tap2 = 2 * (self.written - self.min_delay as u64);
        self.old2 = self.tap2;
        self.fade_pos = 0;
        self.fade_len = 0;
        for f in &mut self.coarse_lp {
            f.reset();
        }
        self.tone.reset();
        self.onset_hp.reset();
        self.env_fast = 0.0;
        self.env_slow = 0.0;
        self.drive.env = 0.0;
        self.since_onset = self.hold;
        self.dry_gain.snap_to(self.set.dry);
        self.sub_gain.snap_to(self.set.sub);
    }

    /// Current distance between the newest input sample and the read pointer, in samples.
    #[inline]
    fn delay_of(&self, tap2: u64) -> usize {
        (self.written - tap2 / 2) as usize
    }

    #[inline(always)]
    fn at(&self, i: u64) -> f32 {
        self.buf[i as usize & self.mask]
    }

    /// The input at a half-sample position. Whole positions are the sample itself; half positions
    /// use the four-point cubic, which at exactly one half is `(-a + 9b + 9c - d) / 16`.
    #[inline(always)]
    fn read(&self, tap2: u64) -> f32 {
        let i = tap2 / 2;
        if tap2 & 1 == 0 {
            self.at(i)
        } else {
            let a = self.at(i - 1);
            let b = self.at(i);
            let c = self.at(i + 1);
            let d = self.at(i + 2);
            (9.0 * (b + c) - (a + d)) * 0.0625
        }
    }

    /// One frame. The octave is made from the mid signal; the dry signal keeps its two channels.
    #[inline]
    pub fn process(&mut self, x: Frame) -> Frame {
        let m = 0.5 * (x.l + x.r);
        let n = self.written;
        self.buf[n as usize & self.mask] = m;
        let c = self.coarse_lp[0].process(m);
        let c = self.coarse_lp[1].process(c);
        if n as usize % self.factor == 0 {
            self.coarse[(n as usize / self.factor) & self.coarse_mask] = c;
        }
        self.written = n + 1;

        let onset = self.detect_onset(m);
        if onset {
            self.splice_to((self.written - self.pre_roll as u64) * 2, self.attack_fade);
        } else if self.fade_len == 0 {
            let delay = self.delay_of(self.tap2);
            if delay >= self.min_delay + self.jump_hi {
                let jump = self.find_jump(delay - self.min_delay);
                self.splice_to(self.tap2 + 2 * jump as u64, jump);
            }
        } else if self.delay_of(self.tap2) > self.max_delay {
            // Cannot happen with the geometry above; if it ever did, the pointer would read
            // overwritten samples. Better a hard jump than that.
            self.splice_to((self.written - self.min_delay as u64) * 2, self.attack_fade);
        }

        let mut y = self.read(self.tap2);
        if self.fade_len > 0 {
            let t = self.fade_pos as f32 / self.fade_len as f32;
            // Smoothstep: equal-gain fade with a gentle start and end. The taps are in phase, so
            // their amplitudes add - an equal-power fade would bulge.
            let g = t * t * (3.0 - 2.0 * t);
            let old = self.read(self.old2);
            y = old + (y - old) * g;
            self.old2 += 1;
            self.fade_pos += 1;
            if self.fade_pos >= self.fade_len {
                self.fade_len = 0;
            }
        }
        self.tap2 += 1;
        let sub = self.tone.process(self.drive.process(y));

        let d = n as usize & self.dry_mask;
        let lag = (n as usize).wrapping_sub(self.latency() as usize) & self.dry_mask;
        self.dry_l[d] = x.l;
        self.dry_r[d] = x.r;
        let dry = Frame::new(self.dry_l[lag], self.dry_r[lag]);

        let dg = self.dry_gain.next();
        let sg = self.sub_gain.next();
        let s = sub * sg;
        Frame::new(dry.l * dg + s, dry.r * dg + s)
    }

    /// Start reading at `to2`, crossfading from where the pointer is now over `fade` samples.
    fn splice_to(&mut self, to2: u64, fade: usize) {
        // A splice in the middle of a fade drops the tap that was already on its way out - it is
        // at most a few percent of the mix by then, or this is an attack and it is the old note.
        self.old2 = self.tap2;
        self.tap2 = to2;
        self.fade_pos = 0;
        self.fade_len = fade.max(1);
    }

    /// The attack detector. True on the sample a new note is recognised.
    #[inline]
    fn detect_onset(&mut self, m: f32) -> bool {
        let h = self.onset_hp.process(m).abs();
        self.env_fast = flush(if h > self.env_fast { h } else { self.env_fast * self.fast_release });
        let fired = self.since_onset >= self.hold
            && self.env_fast > ONSET_FLOOR
            && self.env_fast > ONSET_RATIO * self.env_slow;
        // The slow envelope follows after the comparison, so a jump is seen before it is absorbed.
        self.env_slow = flush(self.env_slow + (self.env_fast - self.env_slow) * self.slow_coeff);
        if fired {
            self.since_onset = 0;
        } else {
            self.since_onset = self.since_onset.saturating_add(1);
        }
        fired
    }

    /// The best distance to jump the pointer forward by, at most `max_jump`. See the module comment.
    fn find_jump(&self, max_jump: usize) -> usize {
        let p = self.tap2 / 2;
        let hi = max_jump.min(self.jump_hi);
        let lo = self.jump_lo.min(hi);
        let f = self.factor;

        // Coarse: on the decimated copy, over one longest period of past plus the lookahead.
        let pd = (p / f as u64) as usize;
        let back = self.jump_hi / f;
        let ahead = (self.lookahead / f).max(1);
        let jd_lo = lo.div_ceil(f);
        let jd_hi = hi / f;
        let coarse = |k: usize| self.coarse[k & self.coarse_mask];
        let start = pd + self.coarse.len() - back; // never underflows: back < coarse.len()
        let width = back + ahead;
        let mut ea = 0.0f32;
        for i in 0..width {
            let a = coarse(start + i);
            ea += a * a;
        }
        let mut best_j = hi;
        if jd_hi >= jd_lo && ea > 1.0e-12 {
            let mut eb = 0.0f32;
            for i in 0..width {
                let b = coarse(start + jd_lo + i);
                eb += b * b;
            }
            // Scores per lag, kept on the stack; the lag count is bounded by the geometry.
            let mut scores = [0.0f32; 256];
            let count = (jd_hi - jd_lo + 1).min(scores.len());
            let mut best = f32::MIN;
            for (s, jd) in (jd_lo..jd_lo + count).enumerate() {
                if s > 0 {
                    let out = coarse(start + jd - 1);
                    let inn = coarse(start + jd - 1 + width);
                    eb = (eb - out * out + inn * inn).max(0.0);
                }
                let mut dot = 0.0f32;
                for i in 0..width {
                    dot += coarse(start + i) * coarse(start + jd + i);
                }
                let score = dot / (ea * eb).sqrt().max(1.0e-12);
                scores[s] = score;
                best = best.max(score);
            }
            // The longest lag that is a *peak* and about as good as the best one. A peak, because
            // the correlation of a low note is broad: the flank of its peak is still within the
            // tolerance tens of samples past the period, and a splice there is a phase jump.
            let mut chosen = jd_lo;
            for s in (0..count).rev() {
                let left = if s > 0 { scores[s - 1] } else { f32::MIN };
                let right = if s + 1 < count { scores[s + 1] } else { f32::MIN };
                if scores[s] >= best - MATCH_TOLERANCE && scores[s] >= left && scores[s] >= right {
                    chosen = jd_lo + s;
                    break;
                }
            }
            best_j = chosen * f;
        } else if ea <= 1.0e-12 {
            // Silence: every jump is as good as any other; the longest keeps the delay short.
            return hi.max(1);
        }

        // Fine: at full rate, around the coarse winner.
        let from = best_j.saturating_sub(f).max(lo);
        let to = (best_j + f).min(hi);
        let width = self.refine + self.lookahead;
        let base = p - self.refine as u64;
        let mut ea = 0.0f32;
        for i in 0..width as u64 {
            let a = self.at(base + i);
            ea += a * a;
        }
        let mut chosen = best_j.clamp(lo, hi);
        let mut best = f32::MIN;
        for j in from..=to {
            let mut dot = 0.0f32;
            let mut eb = 0.0f32;
            for i in 0..width as u64 {
                let b = self.at(base + j as u64 + i);
                dot += self.at(base + i) * b;
                eb += b * b;
            }
            let score = dot / (ea * eb).sqrt().max(1.0e-12);
            if score > best {
                best = score;
                chosen = j;
            }
        }
        chosen.max(1)
    }
}
