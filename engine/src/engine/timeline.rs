//! Musical timeline: sample position <-> bar / beat / offset.
//!
//! Pure arithmetic, no audio dependency, so every claim about it can be proven in a unit test.
//!
//! # Why every boundary is computed absolutely
//!
//! A beat is generally not a whole number of samples (100 BPM at 48 kHz is 28800 exactly, but
//! 137 BPM in 7/8 is 10510.9489... samples). Advancing a running counter by a rounded beat length
//! accumulates the rounding error: after 1000 bars of 7/8 that is 7000 roundings, and the grid
//! can sit several hundred samples off. Therefore the fractional beat length is kept as `f64` and
//! **every** boundary is derived from the beat index alone:
//!
//! ```text
//! beat_start(i) = round(i * samples_per_beat)
//! ```
//!
//! The error against the ideal grid is then bounded by half a sample forever, no matter how long
//! the session runs. Nothing in this module ever adds a beat length to a previous result.

/// Time signature. `beat_unit` is the note value that gets one beat (4 = quarter, 8 = eighth).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TimeSignature {
    pub beats_per_bar: u32,
    pub beat_unit: u32,
}

impl TimeSignature {
    pub const fn new(beats_per_bar: u32, beat_unit: u32) -> Self {
        Self {
            beats_per_bar,
            beat_unit,
        }
    }

    pub fn validate(&self) -> Result<(), String> {
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

/// A position on the musical grid. `bar` and `beat` are zero-based; user output adds one.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub struct MusicalPos {
    pub bar: u64,
    pub beat: u32,
    /// Samples elapsed since the start of this beat.
    pub offset: u64,
}

#[derive(Clone, Copy, Debug)]
pub struct Timeline {
    sample_rate: u32,
    bpm: f64,
    signature: TimeSignature,
    samples_per_beat: f64,
}

impl Timeline {
    pub fn new(sample_rate: u32, bpm: f64, signature: TimeSignature) -> Self {
        // BPM always counts quarter notes, so an eighth-note beat unit halves the beat length.
        let samples_per_beat =
            sample_rate as f64 * 60.0 / bpm * (4.0 / signature.beat_unit as f64);
        Self {
            sample_rate,
            bpm,
            signature,
            samples_per_beat,
        }
    }

    pub fn validate(sample_rate: u32, bpm: f64, signature: TimeSignature) -> Result<(), String> {
        signature.validate()?;
        if !(1.0..=400.0).contains(&bpm) {
            return Err(format!("BPM {bpm} liegt ausserhalb von 1..400."));
        }
        if sample_rate == 0 {
            return Err("Samplerate 0 ist nicht moeglich.".to_string());
        }
        Ok(())
    }

    pub fn sample_rate(&self) -> u32 {
        self.sample_rate
    }

    pub fn bpm(&self) -> f64 {
        self.bpm
    }

    pub fn signature(&self) -> TimeSignature {
        self.signature
    }

    pub fn beats_per_bar(&self) -> u64 {
        self.signature.beats_per_bar as u64
    }

    pub fn samples_per_beat(&self) -> f64 {
        self.samples_per_beat
    }

    /// Length of a quarter note in samples.
    ///
    /// Not the same as [`Timeline::samples_per_beat`]: with a beat unit of 8 a beat is an eighth,
    /// while BPM always counts quarters. Anything expressed in note values - the tempo-synchronous
    /// delay in `engine::fx` - has to reference the quarter, because that is what a musician means
    /// by "1/4" whatever the time signature is.
    pub fn samples_per_quarter(&self) -> f64 {
        self.sample_rate as f64 * 60.0 / self.bpm
    }

    /// First sample of beat number `beat_index` (counted from the start of the session).
    ///
    /// Absolute, never incremental - see the module comment.
    #[inline]
    pub fn beat_start(&self, beat_index: u64) -> u64 {
        (beat_index as f64 * self.samples_per_beat).round() as u64
    }

    /// First sample of bar number `bar_index`.
    #[inline]
    pub fn bar_start(&self, bar_index: u64) -> u64 {
        self.beat_start(bar_index * self.beats_per_bar())
    }

    /// (bar, beat) -> sample position. The inverse of [`Timeline::locate`].
    pub fn position_of(&self, bar: u64, beat: u32) -> u64 {
        self.beat_start(bar * self.beats_per_bar() + beat as u64)
    }

    /// Index of the beat containing `pos`.
    ///
    /// The division gives the answer up to one beat; the two corrections below fix the boundary
    /// cases introduced by rounding, so this is the exact inverse of `beat_start`.
    #[inline]
    pub fn beat_index_at(&self, pos: u64) -> u64 {
        let mut i = (pos as f64 / self.samples_per_beat) as u64;
        while self.beat_start(i + 1) <= pos {
            i += 1;
        }
        while i > 0 && self.beat_start(i) > pos {
            i -= 1;
        }
        i
    }

    pub fn bar_index_at(&self, pos: u64) -> u64 {
        self.beat_index_at(pos) / self.beats_per_bar()
    }

    /// Sample position -> (bar, beat, offset inside the beat).
    pub fn locate(&self, pos: u64) -> MusicalPos {
        let beat_index = self.beat_index_at(pos);
        MusicalPos {
            bar: beat_index / self.beats_per_bar(),
            beat: (beat_index % self.beats_per_bar()) as u32,
            offset: pos - self.beat_start(beat_index),
        }
    }

    /// Smallest bar boundary that is at or after `pos`. Used to quantise user actions.
    pub fn bar_start_at_or_after(&self, pos: u64) -> u64 {
        let bar = self.bar_index_at(pos);
        let start = self.bar_start(bar);
        if start >= pos {
            start
        } else {
            self.bar_start(bar + 1)
        }
    }

    /// Smallest boundary of a `bars`-bar loop grid that is at or after `pos`.
    ///
    /// The grid is counted from the start of the session: bar 0, bar `bars`, bar `2*bars` ... (in
    /// the one-based numbering the musician reads, bar 1, `bars+1`, `2*bars+1`). This is the raster
    /// a fresh recording snaps to when there is no loop yet to align against; a track that already
    /// has one uses its own `origin` / `loop_len` instead - see `engine::schedule`.
    ///
    /// `bars == 0` would have no grid at all, so it degrades to the bar grid.
    pub fn loop_start_at_or_after(&self, pos: u64, bars: u32) -> u64 {
        if bars == 0 {
            return self.bar_start_at_or_after(pos);
        }
        let bars = bars as u64;
        // Round the current bar up to the next multiple of `bars`. That boundary can still lie
        // before `pos` when `pos` sits inside a bar that is itself on the grid, so one more step is
        // taken in that case - never two, because the rounded-up bar is at most one grid step away.
        let grid_bar = self.bar_index_at(pos).div_ceil(bars) * bars;
        let start = self.bar_start(grid_bar);
        if start >= pos {
            start
        } else {
            self.bar_start(grid_bar + bars)
        }
    }

    /// Length in samples of `bars` bars beginning at bar `from_bar`.
    ///
    /// Not a constant: because bar boundaries are rounded individually, the same number of bars
    /// can be one sample longer or shorter at a different place on the grid.
    pub fn span_bars(&self, from_bar: u64, bars: u32) -> u64 {
        self.bar_start(from_bar + bars as u64) - self.bar_start(from_bar)
    }

    pub fn samples_to_secs(&self, samples: u64) -> f64 {
        samples as f64 / self.sample_rate as f64
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const RATE: u32 = 48_000;

    fn tl(bpm: f64, bpb: u32, unit: u32) -> Timeline {
        Timeline::new(RATE, bpm, TimeSignature::new(bpb, unit))
    }

    /// Beat boundaries must equal `round(i * samples_per_beat)` for every single beat of 1000+
    /// bars. A counter that adds a rounded beat length each time drifts away from this; that is
    /// exactly what the assertion catches.
    fn check_no_drift(t: Timeline, bars: u64) {
        let spb = t.samples_per_beat();
        let beats = bars * t.beats_per_bar();
        let mut naive_accumulator: u64 = 0;
        let rounded_step = spb.round() as u64;
        let mut worst_naive: i64 = 0;
        for i in 0..=beats {
            let exact = i as f64 * spb;
            let got = t.beat_start(i);
            // Absolute rule: never more than half a sample away from the ideal grid.
            assert!(
                (got as f64 - exact).abs() <= 0.5 + 1e-9,
                "beat {i}: {got} weicht von {exact} ab"
            );
            // The inverse must land on the same beat again.
            assert_eq!(t.beat_index_at(got), i, "beat_index_at(beat_start({i}))");
            if got > 0 {
                assert_eq!(
                    t.beat_index_at(got - 1),
                    i - 1,
                    "sample vor Schlag {i} gehoert noch zum vorigen Schlag"
                );
            }
            worst_naive = worst_naive.max((naive_accumulator as i64 - got as i64).abs());
            naive_accumulator += rounded_step;
        }
        // Sanity check of the test itself: for a fractional beat length the naive counter really
        // does drift, so the assertions above are testing something.
        if (spb - spb.round()).abs() > 1e-6 {
            assert!(
                worst_naive > 100,
                "Bei {spb} Samples pro Schlag muesste ein aufaddierender Zaehler deutlich driften"
            );
        }
    }

    #[test]
    fn four_four_exact_positions() {
        let t = tl(120.0, 4, 4);
        // 120 BPM at 48 kHz: 24000 samples per quarter, 96000 per bar - integers, so the grid is
        // exact and can be checked against hand-computed numbers.
        assert_eq!(t.samples_per_beat(), 24_000.0);
        assert_eq!(t.bar_start(0), 0);
        assert_eq!(t.bar_start(1), 96_000);
        assert_eq!(t.bar_start(1000), 96_000_000);
        assert_eq!(t.position_of(999, 3), 96_000_000 - 24_000);
        assert_eq!(t.span_bars(0, 8), 768_000);
        check_no_drift(t, 1200);
    }

    #[test]
    fn three_four_exact_positions() {
        let t = tl(100.0, 3, 4);
        assert_eq!(t.samples_per_beat(), 28_800.0);
        assert_eq!(t.bar_start(1), 86_400);
        assert_eq!(t.bar_start(1000), 86_400_000);
        let p = t.locate(86_400 + 28_800 + 17);
        assert_eq!(
            p,
            MusicalPos {
                bar: 1,
                beat: 1,
                offset: 17
            }
        );
        check_no_drift(t, 1500);
    }

    #[test]
    fn seven_eight_fractional_beat_has_no_drift() {
        // Deliberately awkward: 7/8 at 137 BPM gives 10510.9489... samples per beat.
        let t = tl(137.0, 7, 8);
        assert!((t.samples_per_beat() - 10_510.948_905_109_49).abs() < 1e-6);
        check_no_drift(t, 1000);
        // Spot check far out on the grid against an independently computed value.
        let expect = (1000.0 * 7.0 * t.samples_per_beat()).round() as u64;
        assert_eq!(t.bar_start(1000), expect);
    }

    /// A beat is not always a quarter, and the delay depends on knowing the difference.
    #[test]
    fn a_quarter_note_is_independent_of_the_beat_unit() {
        let four_four = tl(120.0, 4, 4);
        assert_eq!(four_four.samples_per_quarter(), 24_000.0);
        assert_eq!(four_four.samples_per_beat(), 24_000.0);

        let seven_eight = tl(120.0, 7, 8);
        assert_eq!(seven_eight.samples_per_beat(), 12_000.0, "ein Schlag ist ein Achtel");
        assert_eq!(
            seven_eight.samples_per_quarter(),
            24_000.0,
            "eine Viertel bleibt eine Viertel"
        );
    }

    #[test]
    fn locate_and_position_of_are_inverse() {
        for (bpm, bpb, unit) in [(120.0, 4, 4), (100.0, 3, 4), (137.0, 7, 8), (93.5, 5, 4)] {
            let t = tl(bpm, bpb, unit);
            for bar in [0u64, 1, 7, 63, 999, 1001] {
                for beat in 0..bpb {
                    let pos = t.position_of(bar, beat);
                    let got = t.locate(pos);
                    assert_eq!(got.bar, bar);
                    assert_eq!(got.beat, beat);
                    assert_eq!(got.offset, 0);
                }
            }
        }
    }

    #[test]
    fn bar_start_at_or_after_is_idempotent_on_boundaries() {
        let t = tl(137.0, 7, 8);
        for bar in 0..200u64 {
            let start = t.bar_start(bar);
            assert_eq!(t.bar_start_at_or_after(start), start);
            assert_eq!(t.bar_start_at_or_after(start + 1), t.bar_start(bar + 1));
        }
    }

    /// The loop grid is what the musician actually counts in: every `bars`-th bar, from the start
    /// of the session. A position exactly on a grid point must stay there, everything else must
    /// move forward to the next one - never two forward, never backwards.
    #[test]
    fn the_loop_grid_snaps_to_every_nth_bar() {
        for (bpm, bpb, unit) in [(120.0, 4, 4), (100.0, 3, 4), (137.0, 7, 8)] {
            let t = tl(bpm, bpb, unit);
            for bars in [1u32, 2, 4, 8] {
                for step in 0..40u64 {
                    let grid = t.bar_start(step * bars as u64);
                    assert_eq!(t.loop_start_at_or_after(grid, bars), grid, "auf der Grenze");
                    assert_eq!(
                        t.loop_start_at_or_after(grid + 1, bars),
                        t.bar_start((step + 1) * bars as u64),
                        "einen Sample dahinter"
                    );
                }
                // Every bar inside a loop lands on that loop's end, not one grid further.
                for bar in 0..40u64 {
                    let want = t.bar_start(bar.div_ceil(bars as u64) * bars as u64);
                    assert_eq!(t.loop_start_at_or_after(t.bar_start(bar), bars), want);
                }
            }
        }
    }

    /// Eight bars, the case from the report: pressing in bar 3 or bar 8 must both land on bar 9.
    #[test]
    fn eight_bar_loops_collect_bar_three_and_bar_eight_on_bar_nine() {
        let t = tl(120.0, 4, 4);
        // One-based bar 3 is index 2, bar 8 is index 7; both belong to the loop that ends at
        // index 8, i.e. one-based bar 9.
        for bar in [2u64, 7] {
            assert_eq!(
                t.loop_start_at_or_after(t.bar_start(bar) + 17, 8),
                t.bar_start(8)
            );
        }
        assert_eq!(t.loop_start_at_or_after(t.bar_start(8), 8), t.bar_start(8));
        // A bar grid still gives the next bar - the two modes really differ.
        assert_eq!(t.bar_start_at_or_after(t.bar_start(2) + 17), t.bar_start(3));
    }

    /// `bars = 0` has no grid; it must not divide by zero but behave like the bar grid.
    #[test]
    fn a_loop_of_zero_bars_falls_back_to_the_bar_grid() {
        let t = tl(120.0, 4, 4);
        assert_eq!(t.loop_start_at_or_after(100_000, 0), t.bar_start_at_or_after(100_000));
    }
}
