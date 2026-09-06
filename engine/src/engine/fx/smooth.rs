//! Click-free parameter changes.
//!
//! Every gain, every wet share and every crossfade in the chain goes through [`Smoothed`]. A gain
//! that jumps from 0.0 to 1.0 between two samples is a step discontinuity, and a step
//! discontinuity is a click - the one thing a musician notices immediately and cannot explain.
//!
//! # Why a one-pole and not a linear ramp
//!
//! A one-pole costs one multiply and one add per sample, has no counter that could get out of
//! step, and needs no "how many samples are left" bookkeeping when the target changes again in
//! the middle of a ramp. Its price is that it approaches the target asymptotically and never
//! reaches it exactly - which matters here, because two things in this crate depend on an *exact*
//! value:
//!
//! * a bypassed chain has to be bit-identical with its input, which needs a crossfade of exactly
//!   0.0, not 1e-8;
//! * a value that hovers a hair above zero forever is a denormal, and denormals are the reason
//!   this module exists in the first place (see the module comment of [`super`]).
//!
//! Therefore [`Smoothed::next`] snaps to the target once the remaining distance falls under
//! [`SNAP`]. At the time constants used here that happens a few milliseconds after the target was
//! set, which is inaudible, and from that moment the value is exact and cheap.

/// Distance at which the ramp is considered finished and the value is set exactly.
///
/// -100 dB relative to a full-scale gain change: far below the resolution of anything that could
/// be heard, and far above the denormal range.
const SNAP: f32 = 1.0e-5;

#[derive(Clone, Copy, Debug)]
pub struct Smoothed {
    current: f32,
    target: f32,
    /// Fraction of the remaining distance covered per sample.
    coeff: f32,
}

impl Smoothed {
    /// A value that is already at `value`, ramping over roughly `ms` milliseconds when it changes.
    ///
    /// "Roughly" is the time constant of the one-pole: after `ms` the value has covered 63 % of
    /// the distance, after three times `ms` it is done for all practical purposes and [`SNAP`]
    /// has taken over.
    pub fn new(value: f32, ms: f32, sample_rate: u32) -> Self {
        let samples = (ms * 0.001 * sample_rate as f32).max(1.0);
        Self {
            current: value,
            target: value,
            // 1 - exp(-1/n) is the exact one-pole coefficient for a time constant of n samples.
            coeff: 1.0 - (-1.0 / samples).exp(),
        }
    }

    #[inline]
    pub fn set(&mut self, target: f32) {
        self.target = target;
    }

    /// Jump to a value without a ramp. For building a state that was never audible - loading a
    /// preset into a silent chain, say - not for changing one that is.
    #[inline]
    pub fn snap_to(&mut self, value: f32) {
        self.current = value;
        self.target = value;
    }

    #[inline]
    pub fn target(&self) -> f32 {
        self.target
    }

    #[inline]
    pub fn value(&self) -> f32 {
        self.current
    }

    /// True once the value is exactly at its target, so a caller can skip work.
    #[inline]
    pub fn settled(&self) -> bool {
        self.current == self.target
    }

    /// Advance by one sample and return the new value.
    ///
    /// The second half of the snap condition is not redundant. Close to the target the step
    /// `(target - current) * coeff` becomes smaller than the spacing between two neighbouring
    /// `f32` values, so the addition rounds back to where it started and the ramp is stuck - at a
    /// distance that depends on the time constant and can sit just above [`SNAP`]. Catching "the
    /// value did not move" makes the arrival exact for every time constant, not only for fast
    /// ones.
    #[inline]
    pub fn next(&mut self) -> f32 {
        if self.current != self.target {
            let before = self.current;
            self.current += (self.target - self.current) * self.coeff;
            if self.current == before || (self.target - self.current).abs() < SNAP {
                self.current = self.target;
            }
        }
        self.current
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const RATE: u32 = 48_000;

    #[test]
    fn a_ramp_reaches_its_target_exactly_and_stays_there() {
        let mut s = Smoothed::new(0.0, 10.0, RATE);
        s.set(1.0);
        assert!(!s.settled());
        // 10 ms time constant: 63 % after 480 samples.
        for _ in 0..480 {
            s.next();
        }
        assert!((s.value() - 0.632).abs() < 0.01, "{}", s.value());
        for _ in 0..48_000 {
            s.next();
        }
        assert_eq!(s.value(), 1.0, "die Rampe muss exakt ankommen");
        assert!(s.settled());
        assert_eq!(s.next(), 1.0);
    }

    /// The property the bit-exact bypass rests on: a ramp down really becomes zero, not almost.
    #[test]
    fn a_ramp_to_zero_becomes_zero() {
        let mut s = Smoothed::new(1.0, 20.0, RATE);
        s.set(0.0);
        for _ in 0..RATE {
            s.next();
        }
        assert_eq!(s.value(), 0.0, "kein Rest, sonst waere es ein Denormal");
        assert!(s.value().is_finite());
    }

    #[test]
    fn snapping_skips_the_ramp_entirely() {
        let mut s = Smoothed::new(0.0, 50.0, RATE);
        s.snap_to(0.75);
        assert_eq!(s.next(), 0.75);
    }
}
