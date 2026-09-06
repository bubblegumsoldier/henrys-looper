//! Tempo-synchronous delay.
//!
//! # Why this one is not measured in milliseconds
//!
//! Every plugin delay offers a note value, and every plugin delay gets it from a host that guesses
//! the tempo from a MIDI clock or a transport message. This engine does not guess: the timeline is
//! the thing that defines musical time here, and it is exact to the sample
//! (`engine::timeline`). A quarter note at 120 BPM and 48 kHz is 24 000 samples, not "about 500
//! ms", and that is the number this delay uses. It is one of the few places where owning the whole
//! signal path pays a musical dividend rather than an engineering one.
//!
//! The reference is the **quarter note**, deliberately not the engine's "beat": with a beat unit
//! of 8 a beat is an eighth, and a musician who asks for a quarter-note delay means a quarter note
//! in both cases. So the length comes from `sample_rate * 60 / bpm`, times the factor of the note
//! value.
//!
//! # Size of the delay line
//!
//! The line is allocated once, in the control thread, and never resized - a reallocation inside
//! the audio callback is exactly what the architecture forbids. It therefore has to be long enough
//! for the longest delay that can ever be asked for:
//!
//! * longest note value offered: a quarter note (the dotted eighth, the eighth and the eighth
//!   triplet are all shorter);
//! * slowest tempo this is sized for: [`MIN_SYNC_BPM`] = 30 BPM, which is half of the slowest
//!   tempo anyone plays a song at and a third of the slowest the engine's timeline accepts;
//! * so the longest tap is `60 / 30 = 2.0` seconds, [`MAX_DELAY_SECONDS`].
//!
//! At 48 kHz that is 96 000 samples, **384 kB per track**, or 3 MB for the eight tracks the engine
//! allows. Below 30 BPM the note value is clamped to the line length and the delay simply stops
//! getting longer; the alternative would be a 60-second line for the 1 BPM the timeline formally
//! permits, which is 11 MB per track for a tempo nobody will ever play.
//!
//! # What happens on a tempo change
//!
//! The tap moves. Moving a read pointer in one step is a jump into unrelated material, and that is
//! a click. So the old and the new tap are crossfaded over [`TAP_FADE_MS`]: for twenty
//! milliseconds both are read and mixed, then the old one is dropped. What is already in the line
//! keeps playing out at the new spacing and dies away with the feedback, which is what a delay
//! does when you change its time - the alternative, wiping the line, would mean a memset of a
//! third of a megabyte inside the callback.
//!
//! # Denormals
//!
//! Every value written into the line goes through `flush`. A delay with feedback below 1.0 decays
//! exponentially after the input stops, and without the flush it would spend minutes in the
//! denormal range, where a multiply can cost a hundred times what it should.

use super::biquad::flush;
use super::smooth::Smoothed;

/// Slowest tempo the line is sized for. See the module comment.
pub const MIN_SYNC_BPM: f64 = 30.0;
/// Longest tap the line has to hold: one quarter note at [`MIN_SYNC_BPM`].
pub const MAX_DELAY_SECONDS: f64 = 60.0 / MIN_SYNC_BPM;
/// Crossfade when the tap moves.
const TAP_FADE_MS: f32 = 20.0;

/// Note values the delay can be set to, as musicians name them.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum DelayNote {
    /// Straight quarter. The safe choice; it disappears into the beat.
    Quarter,
    /// Dotted eighth. The one that makes a strummed part sound like two guitars.
    #[default]
    DottedEighth,
    Eighth,
    /// Eighth triplet - a shuffle against a straight part.
    TripletEighth,
}

impl DelayNote {
    /// Length as a fraction of a quarter note.
    #[inline]
    pub fn factor(self) -> f64 {
        match self {
            DelayNote::Quarter => 1.0,
            DelayNote::DottedEighth => 0.75,
            DelayNote::Eighth => 0.5,
            DelayNote::TripletEighth => 1.0 / 3.0,
        }
    }

    /// How it is written on paper, for the display.
    pub fn label(self) -> &'static str {
        match self {
            DelayNote::Quarter => "1/4",
            DelayNote::DottedEighth => "1/8.",
            DelayNote::Eighth => "1/8",
            DelayNote::TripletEighth => "1/8T",
        }
    }

    /// Parse what the CLI accepts.
    pub fn parse(text: &str) -> Option<Self> {
        match text.trim().to_lowercase().as_str() {
            "1/4" | "4" | "viertel" => Some(DelayNote::Quarter),
            "1/8." | "8." | "punktiert" => Some(DelayNote::DottedEighth),
            "1/8" | "8" | "achtel" => Some(DelayNote::Eighth),
            "1/8t" | "8t" | "triole" => Some(DelayNote::TripletEighth),
            _ => None,
        }
    }

    pub fn all() -> [DelayNote; 4] {
        [
            DelayNote::Quarter,
            DelayNote::DottedEighth,
            DelayNote::Eighth,
            DelayNote::TripletEighth,
        ]
    }
}

pub struct Delay {
    /// Allocated in the control thread, never resized. See the module comment for its length.
    line: Vec<f32>,
    write: usize,
    /// Tap distance in samples, currently in effect.
    taps: usize,
    /// Tap distance being faded in, if a fade is running.
    fading_to: usize,
    /// 0.0 = fully on `taps`, 1.0 = fully on `fading_to`.
    fade: f32,
    fade_step: f32,
    note: DelayNote,
    /// Samples per quarter note, from the engine timeline.
    quarter_samples: f64,
    /// False until the first tempo has been seen. The very first tap is set without a crossfade,
    /// because there is nothing to fade *from* - the line is empty and nothing has been heard yet.
    tuned: bool,
    feedback: Smoothed,
    mix: Smoothed,
}

impl Delay {
    /// Allocates the line. Control thread only.
    pub fn new(sample_rate: u32, note: DelayNote, feedback: f32, mix: f32) -> Self {
        let len = (MAX_DELAY_SECONDS * sample_rate as f64).ceil() as usize + 2;
        let fade_samples = (TAP_FADE_MS * 0.001 * sample_rate as f32).max(1.0);
        Self {
            line: vec![0.0; len],
            write: 0,
            taps: 1,
            fading_to: 0,
            fade: 0.0,
            fade_step: 1.0 / fade_samples,
            note,
            quarter_samples: 0.0,
            tuned: false,
            feedback: Smoothed::new(feedback, 40.0, sample_rate),
            mix: Smoothed::new(mix, 40.0, sample_rate),
        }
    }

    /// Length of the allocated line in samples - the number the module comment derives.
    pub fn line_len(&self) -> usize {
        self.line.len()
    }

    /// Tap distance in samples that is currently sounding.
    pub fn tap_samples(&self) -> u64 {
        self.taps as u64
    }

    /// Tap distance the delay is heading for. The same as [`Delay::tap_samples`] unless a
    /// crossfade is running - and that is the number to report and to reason with, because the
    /// crossfade only advances while the chain is actually processing. On a silent track it can
    /// stay half-finished for as long as the silence lasts, which would make a status display
    /// claim the old tempo forever.
    pub fn target_tap_samples(&self) -> u64 {
        if self.fade > 0.0 {
            self.fading_to as u64
        } else {
            self.taps as u64
        }
    }

    pub fn note(&self) -> DelayNote {
        self.note
    }

    pub fn feedback(&self) -> f32 {
        self.feedback.target()
    }

    pub fn mix(&self) -> f32 {
        self.mix.target()
    }

    pub fn set_feedback(&mut self, value: f32) {
        self.feedback.set(value.clamp(0.0, 0.95));
    }

    pub fn set_mix(&mut self, value: f32) {
        self.mix.set(value.clamp(0.0, 1.0));
    }

    pub fn snap_feedback(&mut self, value: f32) {
        self.feedback.snap_to(value.clamp(0.0, 0.95));
    }

    pub fn snap_mix(&mut self, value: f32) {
        self.mix.snap_to(value.clamp(0.0, 1.0));
    }

    pub fn set_note(&mut self, note: DelayNote) {
        self.note = note;
        self.retune();
    }

    /// Tell the delay what a quarter note is worth right now. Called once per audio block; doing
    /// nothing when the value has not changed is what keeps it free.
    #[inline]
    pub fn set_quarter_samples(&mut self, quarter_samples: f64) {
        if quarter_samples != self.quarter_samples {
            self.quarter_samples = quarter_samples;
            self.retune();
        }
    }

    /// Work out the new tap and start the crossfade to it.
    fn retune(&mut self) {
        if self.quarter_samples <= 0.0 {
            return;
        }
        let wanted = (self.quarter_samples * self.note.factor()).round() as usize;
        let wanted = wanted.clamp(1, self.line.len() - 1);
        if !self.tuned {
            self.tuned = true;
            self.taps = wanted;
            self.fade = 0.0;
            return;
        }
        if wanted == self.taps && self.fade == 0.0 {
            return;
        }
        if self.fade > 0.0 {
            // A second change while a fade is running: finish the running one instantly rather
            // than juggling three taps. Twenty milliseconds in, nobody hears the difference.
            self.taps = self.fading_to;
        }
        if wanted == self.taps {
            self.fade = 0.0;
            return;
        }
        self.fading_to = wanted;
        self.fade = f32::MIN_POSITIVE; // strictly greater than zero: a fade is running
    }

    /// Silence the line. Control thread only - this is a memset of the whole buffer.
    pub fn clear(&mut self) {
        self.line.fill(0.0);
        self.write = 0;
        self.fade = 0.0;
    }

    #[inline(always)]
    fn read_at(&self, distance: usize) -> f32 {
        let len = self.line.len();
        let idx = (self.write + len - distance.min(len - 1)) % len;
        // `get` rather than indexing: a panic inside an audio callback would take the process
        // down, and the arithmetic above already guarantees the index is in range.
        match self.line.get(idx) {
            Some(&v) => v,
            None => 0.0,
        }
    }

    /// One sample. Returns dry plus the echoes - the delay is wired like an aux send, so turning
    /// the repeats up never makes the dry signal quieter.
    #[inline(always)]
    pub fn process(&mut self, x: f32) -> f32 {
        let wet = if self.fade > 0.0 {
            let old = self.read_at(self.taps);
            let new = self.read_at(self.fading_to);
            let f = self.fade;
            self.fade += self.fade_step;
            if self.fade >= 1.0 {
                self.taps = self.fading_to;
                self.fade = 0.0;
            }
            old + (new - old) * f
        } else {
            self.read_at(self.taps)
        };

        let feedback = self.feedback.next();
        let len = self.line.len();
        if let Some(slot) = self.line.get_mut(self.write) {
            *slot = flush(x + wet * feedback);
        }
        self.write += 1;
        if self.write >= len {
            self.write = 0;
        }
        x + wet * self.mix.next()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const RATE: u32 = 48_000;

    fn delay_at(bpm: f64, note: DelayNote, feedback: f32, mix: f32) -> Delay {
        let mut d = Delay::new(RATE, note, feedback, mix);
        d.snap_feedback(feedback);
        d.snap_mix(mix);
        d.set_quarter_samples(RATE as f64 * 60.0 / bpm);
        d
    }

    /// The claim from the brief, as a number: 120 BPM, quarter note, the repeat of an impulse sits
    /// exactly 24 000 samples later. Not "about", exactly.
    #[test]
    fn a_quarter_note_at_120_bpm_repeats_after_exactly_24000_samples() {
        let mut d = delay_at(120.0, DelayNote::Quarter, 0.0, 1.0);
        assert_eq!(d.tap_samples(), 24_000);

        let mut echo_at = None;
        for i in 0..60_000u64 {
            let x = if i == 0 { 1.0 } else { 0.0 };
            let y = d.process(x);
            // The dry impulse itself is not an echo.
            if i > 0 && y.abs() > 1e-6 {
                echo_at = Some(i);
                break;
            }
        }
        assert_eq!(echo_at, Some(24_000), "Wiederholung exakt eine Viertel spaeter");
    }

    #[test]
    fn every_note_value_lands_on_its_own_grid() {
        // 100 BPM at 48 kHz: a quarter is 28 800 samples.
        for (note, want) in [
            (DelayNote::Quarter, 28_800u64),
            (DelayNote::DottedEighth, 21_600),
            (DelayNote::Eighth, 14_400),
            (DelayNote::TripletEighth, 9_600),
        ] {
            let d = delay_at(100.0, note, 0.0, 1.0);
            assert_eq!(d.tap_samples(), want, "{}", note.label());
        }
    }

    /// Feedback has to decay, and it has to decay to an exact zero rather than into the denormal
    /// range where the CPU falls off a cliff.
    #[test]
    fn the_repeats_die_away_and_the_line_reaches_exact_zero() {
        // Half feedback: after about a hundred passes the value is under the denormal floor and
        // the flush turns it into a true zero rather than leaving a value the CPU chokes on.
        let mut d = delay_at(240.0, DelayNote::Eighth, 0.5, 1.0);
        let tap = d.tap_samples() as usize;
        assert_eq!(tap, 6_000);
        d.process(1.0);
        for _ in 0..tap * 120 {
            d.process(0.0);
        }
        assert!(d.line.iter().all(|&v| v == 0.0), "Leitung nicht ausgeraeumt");
        assert_eq!(d.process(0.0), 0.0);
    }

    /// The most feedback the delay allows must still decay rather than run away. That is what the
    /// clamp at 0.95 is for; the check is that the echo shrinks and nothing turns into NaN.
    #[test]
    fn the_highest_feedback_still_decays() {
        let mut d = delay_at(240.0, DelayNote::Eighth, 1.5, 1.0);
        assert_eq!(d.feedback(), 0.95, "Feedback wird gedeckelt");
        let tap = d.tap_samples() as usize;
        d.process(1.0);
        let mut previous = f32::INFINITY;
        for pass in 1..=40 {
            let mut peak: f32 = 0.0;
            for _ in 0..tap {
                peak = peak.max(d.process(0.0).abs());
            }
            assert!(peak.is_finite(), "Durchlauf {pass} ist NaN oder unendlich");
            assert!(peak < previous, "Durchlauf {pass} wird nicht leiser: {peak}");
            previous = peak;
        }
        assert!(previous < 0.2, "nach 40 Wiederholungen noch {previous}");
    }

    /// The size the module comment promises, and the clamp that protects it.
    #[test]
    fn the_line_holds_a_quarter_note_at_the_slowest_supported_tempo() {
        let d = Delay::new(RATE, DelayNote::Quarter, 0.3, 0.25);
        assert_eq!(d.line_len(), 96_002);
        // Exactly at 30 BPM the quarter note still fits.
        let mut d = delay_at(30.0, DelayNote::Quarter, 0.0, 1.0);
        assert_eq!(d.tap_samples(), 96_000);
        // Slower than that it is clamped instead of writing out of bounds. The new tap arrives
        // after the crossfade, so the line is run for longer than the fade takes.
        d.set_quarter_samples(RATE as f64 * 60.0 / 5.0);
        for _ in 0..2_000 {
            d.process(0.0);
        }
        assert_eq!(d.tap_samples(), (d.line_len() - 1) as u64);
    }

    /// A tempo change must not produce a step in the signal. The crossfade is checked by feeding a
    /// constant: whatever the tap does, a constant delayed by anything is still that constant, and
    /// the output may not jump.
    #[test]
    fn a_tempo_change_crossfades_instead_of_jumping() {
        let mut d = delay_at(120.0, DelayNote::Quarter, 0.0, 1.0);
        // Fill the line with a slow, smooth sweep: two taps 8000 samples apart then read clearly
        // different values, while the material itself has no step the test could mistake for one.
        for i in 0..90_000u64 {
            d.process((i as f32 * 0.000_1).sin() * 0.5);
        }
        let before = d.process(0.5);
        d.set_quarter_samples(RATE as f64 * 60.0 / 90.0);
        assert_ne!(d.tap_samples(), 32_000, "der neue Tap ist noch nicht aktiv");
        let mut previous = before;
        let mut worst_step: f32 = 0.0;
        for _ in 0..2_000 {
            let y = d.process(0.5);
            worst_step = worst_step.max((y - previous).abs());
            previous = y;
        }
        assert_eq!(d.tap_samples(), 32_000, "danach steht der neue Tap");
        // Without the crossfade the two taps differ by up to a full unit in one sample.
        assert!(
            worst_step < 0.02,
            "Sprung von {worst_step} beim Tempowechsel - die Blende greift nicht"
        );
    }

    /// A wet share of zero is a wire, bit for bit. The chain relies on it when the delay is off.
    #[test]
    fn a_wet_share_of_zero_passes_the_signal_through_unchanged() {
        let mut d = delay_at(120.0, DelayNote::Eighth, 0.5, 0.0);
        for i in 0..5_000 {
            let x = ((i % 97) as f32 / 97.0) - 0.5;
            assert_eq!(d.process(x), x, "Sample {i}");
        }
    }

    #[test]
    fn note_values_survive_the_round_trip_through_their_labels() {
        for note in DelayNote::all() {
            assert_eq!(DelayNote::parse(note.label()), Some(note), "{}", note.label());
        }
        assert_eq!(DelayNote::parse("Triole"), Some(DelayNote::TripletEighth));
        assert_eq!(DelayNote::parse("halbe"), None);
    }
}
