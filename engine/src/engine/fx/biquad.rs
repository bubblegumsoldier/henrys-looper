//! Second-order IIR sections: the high-pass and the three EQ bands.
//!
//! Written by hand rather than pulled from a DSP crate, for the reason the plan gives: these are
//! twenty lines of arithmetic, and owning them means owning their allocation behaviour and their
//! denormal behaviour. Both matter inside an audio callback and neither is visible from the
//! outside of a library.
//!
//! # Direct form 1, deliberately
//!
//! The state of a direct form 1 section is four *signal* samples - the last two inputs and the
//! last two outputs. That is what makes a coefficient change survivable: when the cutoff moves,
//! the new coefficients meet a state that still describes real audio, so the filter continues from
//! where it was instead of from a scaled accumulator that belonged to the old transfer function.
//! Direct form 2 is one adder cheaper and does not have that property.
//!
//! Coefficients come from the Audio EQ Cookbook (Robert Bristow-Johnson). They are computed in
//! [`Coeffs`], which is where the `sin`, `cos` and `powf` calls live - once per parameter change,
//! never per sample.
//!
//! # Denormals
//!
//! A high-pass fed with silence after loud material decays exponentially towards zero and spends
//! a while in the denormal range on the way. On x86 a denormal multiply can cost two orders of
//! magnitude more than a normal one, which turns an idle track into a callback overrun. Every
//! state store therefore goes through [`flush`], which replaces anything below the smallest normal
//! float with a true zero. That is the "cut it off" variant of the two usual cures; the other one
//! (adding a tiny DC offset) leaves an offset behind and would have to be removed again.

/// Below this magnitude a float is either denormal or inaudible; either way it is set to zero.
///
/// `f32::MIN_POSITIVE` is 1.18e-38, the smallest *normal* value. 1e-30 sits comfortably above it,
/// so nothing that survives this test can be denormal, and 1e-30 is -600 dBFS - about ten
/// thousand orders of magnitude below anything a converter could reproduce.
pub const DENORMAL_FLOOR: f32 = 1.0e-30;

/// Replace a denormal (or simply inaudible) value with an exact zero.
#[inline(always)]
pub fn flush(x: f32) -> f32 {
    if x.abs() < DENORMAL_FLOOR { 0.0 } else { x }
}

/// What one EQ band does to the spectrum.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum BandKind {
    /// A bell around the centre frequency. The workhorse; use it wherever something has to be
    /// taken out, because a cut wants to be narrow.
    #[default]
    Peak,
    /// Everything below the corner is lifted or lowered.
    LowShelf,
    /// Everything above the corner is lifted or lowered. The sensible choice for the top band:
    /// "air" is a property of the whole top end, not of one frequency, and a shelf gives it with
    /// one control instead of a bell so wide that its skirts reach into the presence range.
    HighShelf,
}

impl BandKind {
    pub fn label(self) -> &'static str {
        match self {
            BandKind::Peak => "Glocke",
            BandKind::LowShelf => "Tiefen-Kuhschwanz",
            BandKind::HighShelf => "Hoehen-Kuhschwanz",
        }
    }
}

/// The five numbers of a normalised biquad, `a0` divided out.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Coeffs {
    b0: f32,
    b1: f32,
    b2: f32,
    a1: f32,
    a2: f32,
}

impl Default for Coeffs {
    /// A wire: `y = x`.
    fn default() -> Self {
        Self {
            b0: 1.0,
            b1: 0.0,
            b2: 0.0,
            a1: 0.0,
            a2: 0.0,
        }
    }
}

/// Keep a frequency inside the range where the bilinear transform behaves.
#[inline]
fn safe_hz(hz: f32, sample_rate: u32) -> f32 {
    let nyquist = sample_rate as f32 * 0.5;
    hz.clamp(10.0, nyquist * 0.95)
}

impl Coeffs {
    fn normalise(b0: f32, b1: f32, b2: f32, a0: f32, a1: f32, a2: f32) -> Self {
        // `a0` is never zero for any admissible Q, but a division by a denormal would produce
        // infinities inside an audio callback, and one branch is cheaper than that risk.
        let inv = if a0.abs() < 1.0e-12 { 0.0 } else { 1.0 / a0 };
        Self {
            b0: b0 * inv,
            b1: b1 * inv,
            b2: b2 * inv,
            a1: a1 * inv,
            a2: a2 * inv,
        }
    }

    /// 12 dB per octave high-pass. `q = 1/sqrt(2)` makes it a Butterworth, i.e. maximally flat in
    /// the pass band and exactly -3 dB at the cutoff - which is what "Grenzfrequenz" means.
    pub fn high_pass(sample_rate: u32, hz: f32, q: f32) -> Self {
        let w0 = std::f32::consts::TAU * safe_hz(hz, sample_rate) / sample_rate as f32;
        let (sin, cos) = w0.sin_cos();
        let alpha = sin / (2.0 * q.max(0.1));
        Self::normalise(
            (1.0 + cos) * 0.5,
            -(1.0 + cos),
            (1.0 + cos) * 0.5,
            1.0 + alpha,
            -2.0 * cos,
            1.0 - alpha,
        )
    }

    /// Bell. `q` is the classic EQ Q: bandwidth in octaves shrinks as `q` grows.
    pub fn peaking(sample_rate: u32, hz: f32, q: f32, gain_db: f32) -> Self {
        let a = 10f32.powf(gain_db / 40.0);
        let w0 = std::f32::consts::TAU * safe_hz(hz, sample_rate) / sample_rate as f32;
        let (sin, cos) = w0.sin_cos();
        let alpha = sin / (2.0 * q.max(0.1));
        Self::normalise(
            1.0 + alpha * a,
            -2.0 * cos,
            1.0 - alpha * a,
            1.0 + alpha / a,
            -2.0 * cos,
            1.0 - alpha / a,
        )
    }

    /// Shelf with slope 1, i.e. as steep as it can be without overshooting.
    fn shelf(sample_rate: u32, hz: f32, gain_db: f32, high: bool) -> Self {
        let a = 10f32.powf(gain_db / 40.0);
        let w0 = std::f32::consts::TAU * safe_hz(hz, sample_rate) / sample_rate as f32;
        let (sin, cos) = w0.sin_cos();
        // Slope S = 1: alpha = sin/2 * sqrt((A + 1/A)(1/S - 1) + 2) collapses to sin/2 * sqrt(2).
        let alpha = sin * 0.5 * std::f32::consts::SQRT_2;
        let two_sqrt_a_alpha = 2.0 * a.sqrt() * alpha;
        if high {
            Self::normalise(
                a * ((a + 1.0) + (a - 1.0) * cos + two_sqrt_a_alpha),
                -2.0 * a * ((a - 1.0) + (a + 1.0) * cos),
                a * ((a + 1.0) + (a - 1.0) * cos - two_sqrt_a_alpha),
                (a + 1.0) - (a - 1.0) * cos + two_sqrt_a_alpha,
                2.0 * ((a - 1.0) - (a + 1.0) * cos),
                (a + 1.0) - (a - 1.0) * cos - two_sqrt_a_alpha,
            )
        } else {
            Self::normalise(
                a * ((a + 1.0) - (a - 1.0) * cos + two_sqrt_a_alpha),
                2.0 * a * ((a - 1.0) - (a + 1.0) * cos),
                a * ((a + 1.0) - (a - 1.0) * cos - two_sqrt_a_alpha),
                (a + 1.0) + (a - 1.0) * cos + two_sqrt_a_alpha,
                -2.0 * ((a - 1.0) + (a + 1.0) * cos),
                (a + 1.0) + (a - 1.0) * cos - two_sqrt_a_alpha,
            )
        }
    }

    pub fn low_shelf(sample_rate: u32, hz: f32, gain_db: f32) -> Self {
        Self::shelf(sample_rate, hz, gain_db, false)
    }

    pub fn high_shelf(sample_rate: u32, hz: f32, gain_db: f32) -> Self {
        Self::shelf(sample_rate, hz, gain_db, true)
    }

    /// One EQ band, whichever kind it is.
    pub fn band(sample_rate: u32, kind: BandKind, hz: f32, q: f32, gain_db: f32) -> Self {
        match kind {
            BandKind::Peak => Self::peaking(sample_rate, hz, q, gain_db),
            BandKind::LowShelf => Self::low_shelf(sample_rate, hz, gain_db),
            BandKind::HighShelf => Self::high_shelf(sample_rate, hz, gain_db),
        }
    }

    /// Magnitude response at `hz`, as a linear factor. Only used by the tests - it is what turns
    /// "the band lifts 3 kHz by 6 dB" into something a machine can check.
    #[cfg(test)]
    pub fn magnitude_at(&self, sample_rate: u32, hz: f32) -> f32 {
        // Evaluate H(z) on the unit circle: z = e^{jw}.
        let w = std::f32::consts::TAU * hz / sample_rate as f32;
        let (s1, c1) = w.sin_cos();
        let (s2, c2) = (2.0 * w).sin_cos();
        let num_re = self.b0 + self.b1 * c1 + self.b2 * c2;
        let num_im = -(self.b1 * s1 + self.b2 * s2);
        let den_re = 1.0 + self.a1 * c1 + self.a2 * c2;
        let den_im = -(self.a1 * s1 + self.a2 * s2);
        ((num_re * num_re + num_im * num_im) / (den_re * den_re + den_im * den_im)).sqrt()
    }
}

/// One second-order section with its state.
#[derive(Clone, Copy, Debug, Default)]
pub struct Biquad {
    c: Coeffs,
    x1: f32,
    x2: f32,
    y1: f32,
    y2: f32,
}

impl Biquad {
    pub fn new(c: Coeffs) -> Self {
        Self {
            c,
            ..Default::default()
        }
    }

    /// Install new coefficients, keeping the signal state. See the module comment.
    #[inline]
    pub fn set(&mut self, c: Coeffs) {
        self.c = c;
    }

    pub fn coeffs(&self) -> Coeffs {
        self.c
    }

    /// Forget the past. Used when a switched-off band is switched on again, where the stored
    /// samples are stale and would ring once before the filter settles.
    #[inline]
    pub fn reset(&mut self) {
        self.x1 = 0.0;
        self.x2 = 0.0;
        self.y1 = 0.0;
        self.y2 = 0.0;
    }

    #[inline(always)]
    pub fn process(&mut self, x: f32) -> f32 {
        let y = self.c.b0 * x + self.c.b1 * self.x1 + self.c.b2 * self.x2
            - self.c.a1 * self.y1
            - self.c.a2 * self.y2;
        let y = flush(y);
        self.x2 = self.x1;
        self.x1 = x;
        self.y2 = self.y1;
        self.y1 = y;
        y
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const RATE: u32 = 48_000;

    fn db(linear: f32) -> f32 {
        20.0 * linear.log10()
    }

    /// The definition of a cutoff frequency, checked against the arithmetic rather than against a
    /// comment: -3 dB at the corner, flat above it, gone below it.
    #[test]
    fn the_high_pass_is_minus_three_decibels_at_its_cutoff() {
        let c = Coeffs::high_pass(RATE, 80.0, std::f32::consts::FRAC_1_SQRT_2);
        assert!(
            (db(c.magnitude_at(RATE, 80.0)) + 3.01).abs() < 0.1,
            "{} dB bei 80 Hz",
            db(c.magnitude_at(RATE, 80.0))
        );
        assert!(db(c.magnitude_at(RATE, 1_000.0)).abs() < 0.05, "oben flach");
        // 12 dB per octave: one octave below the corner has to be about 12 dB down.
        let one_octave_down = db(c.magnitude_at(RATE, 40.0));
        assert!(
            (one_octave_down + 12.3).abs() < 1.0,
            "{one_octave_down} dB bei 40 Hz"
        );
    }

    #[test]
    fn a_bell_lifts_its_own_frequency_and_leaves_its_neighbours_alone() {
        let c = Coeffs::peaking(RATE, 3_000.0, 1.0, 6.0);
        assert!((db(c.magnitude_at(RATE, 3_000.0)) - 6.0).abs() < 0.05);
        // Three octaves away nothing is left of a Q of 1.
        assert!(db(c.magnitude_at(RATE, 375.0)).abs() < 0.5);
        assert!(db(c.magnitude_at(RATE, 100.0)).abs() < 0.1);
    }

    #[test]
    fn a_high_shelf_lifts_everything_above_its_corner() {
        let c = Coeffs::high_shelf(RATE, 8_000.0, 4.0);
        // Half the gain at the corner is what a shelf is defined as.
        assert!((db(c.magnitude_at(RATE, 8_000.0)) - 2.0).abs() < 0.2);
        assert!((db(c.magnitude_at(RATE, 18_000.0)) - 4.0).abs() < 0.3);
        assert!(db(c.magnitude_at(RATE, 200.0)).abs() < 0.2, "unten unberuehrt");
    }

    /// A high-pass has to remove a constant offset completely - that is the whole point of it for
    /// a piezo, whose preamp likes to sit a little off zero.
    #[test]
    fn direct_current_disappears_and_a_high_tone_survives() {
        let mut hp = Biquad::new(Coeffs::high_pass(
            RATE,
            80.0,
            std::f32::consts::FRAC_1_SQRT_2,
        ));
        let mut last = 0.0;
        for _ in 0..RATE {
            last = hp.process(0.5);
        }
        assert!(last.abs() < 1e-6, "Gleichanteil bleibt uebrig: {last}");

        let mut hp = Biquad::new(Coeffs::high_pass(
            RATE,
            80.0,
            std::f32::consts::FRAC_1_SQRT_2,
        ));
        let mut peak: f32 = 0.0;
        for i in 0..RATE {
            let x = (std::f32::consts::TAU * 1_000.0 * i as f32 / RATE as f32).sin();
            let y = hp.process(x);
            if i > RATE / 2 {
                peak = peak.max(y.abs());
            }
        }
        assert!((peak - 1.0).abs() < 0.02, "1 kHz bleibt stehen: {peak}");
    }

    /// The denormal cure, checked where it actually bites: silence after loud material.
    #[test]
    fn a_decaying_filter_reaches_exact_zero_instead_of_a_denormal() {
        let mut hp = Biquad::new(Coeffs::high_pass(RATE, 80.0, 0.707));
        for _ in 0..64 {
            hp.process(1.0);
        }
        for _ in 0..RATE {
            hp.process(0.0);
        }
        assert_eq!(hp.process(0.0), 0.0, "Zustand muss echt null sein");
        assert_eq!(hp.y1, 0.0);
        assert_eq!(hp.y2, 0.0);
        assert!(flush(1.0e-35) == 0.0);
        assert!(flush(0.5) == 0.5);
    }

    #[test]
    fn a_default_biquad_is_a_wire() {
        let mut b = Biquad::default();
        for x in [0.0, 0.25, -1.0, 0.7] {
            assert_eq!(b.process(x), x);
        }
    }
}
