//! The three small types the stereo engine is built from: a stereo sample pair, the channel count
//! of a loop buffer, and which device inputs a track listens on.
//!
//! # Frames, not samples
//!
//! The single most dangerous unit mix-up in a stereo rebuild is "sample" meaning two different
//! things. This module fixes the vocabulary and the rest of the engine sticks to it:
//!
//! * a **frame** is one instant of audio - one number for a mono buffer, two for a stereo one;
//! * a **sample** is one number in one channel.
//!
//! Every musical position (`origin`, `loop_len`, `filled`, the latency compensation `m = k - R`)
//! counts **frames**. Only the indexing into a `Vec<f32>` multiplies by the channel count, and that
//! multiplication happens in exactly two places, both in `track.rs`. Nothing else in the engine is
//! allowed to do it.
//!
//! # Mono in, stereo out
//!
//! A loop buffer is mono or stereo, whichever the source is: a microphone or a guitar has one
//! input channel, and writing it into a stereo buffer would double the memory without adding a
//! single sound. A piano from a sample library has two, and reducing it to one throws away exactly
//! what such a library is bought for.
//!
//! Everything *after* the buffer is stereo without exception - the effect chain, the panner and the
//! mix bus - because a reverb on a mono guitar has to be able to be stereo. A mono track is fanned
//! out to two channels on its way into the chain ([`Frame::mono`]) and placed in the stereo field
//! by its pan setting.
//!
//! # The pan law, and why it is unity in the centre
//!
//! [`pan_gains`] returns 1.0 for both sides at the centre, and turns one side down towards zero as
//! the knob travels: a hard-left mono track is at full level on the left and digitally silent on
//! the right. That is the "0 dB pan law", and it is a deliberate choice against the constant-power
//! (-3 dB) law that a mixing desk usually has:
//!
//! * **The bypass stays bit-exact.** `docs/architektur.md` promises that a bypassed effect chain
//!   passes the signal through unchanged - it is the panic switch on stage. A centre gain of
//!   0.70710678 would turn that promise into "almost unchanged", and a panic switch that alters
//!   the signal is not one.
//! * **Mono and stereo tracks obey the same rule.** For a stereo track the control is a *balance*,
//!   and a balance must be unity in the centre or every stereo source would sit 3 dB below every
//!   mono one. With the constant-power law the two kinds of track would need two different laws
//!   and a musician would have to know which is which.
//! * **Nothing pans automatically here.** The 3 dB dip a 0 dB law produces when a source is swept
//!   from hard left to centre is audible when a knob moves during a song. In this looper a track is
//!   placed once, before it is recorded against.
//!
//! The price, stated plainly: a mono track panned hard left is as loud in that speaker as it was in
//! both, so the *summed* level drops by 3 dB when it is moved out of the centre. That is corrected
//! with the layer gain, which every track has anyway.

/// One instant of stereo audio. The bus, every effect and every panner speak this.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Frame {
    pub l: f32,
    pub r: f32,
}

impl Frame {
    pub const SILENT: Frame = Frame { l: 0.0, r: 0.0 };

    #[inline(always)]
    pub fn new(l: f32, r: f32) -> Self {
        Self { l, r }
    }

    /// Fan a mono sample out to both channels at full level. This is the one place a mono source
    /// becomes stereo, and it happens *before* the effect chain - see the module comment.
    #[inline(always)]
    pub fn mono(x: f32) -> Self {
        Self { l: x, r: x }
    }

    #[inline(always)]
    pub fn is_silent(self) -> bool {
        self.l == 0.0 && self.r == 0.0
    }

    /// Louder of the two channels, as an absolute value. What a single-bar meter shows.
    #[inline(always)]
    pub fn max_abs(self) -> f32 {
        self.l.abs().max(self.r.abs())
    }

    #[inline(always)]
    pub fn added(self, other: Frame) -> Self {
        Self {
            l: self.l + other.l,
            r: self.r + other.r,
        }
    }

    /// Channel `i` (0 = left, 1 = right); anything else is the left one, so an index mistake
    /// cannot panic inside the audio callback.
    #[inline(always)]
    pub fn channel(self, i: usize) -> f32 {
        if i == 1 { self.r } else { self.l }
    }

    #[inline(always)]
    pub fn set_channel(&mut self, i: usize, value: f32) {
        if i == 1 {
            self.r = value;
        } else {
            self.l = value;
        }
    }
}

/// Gains for the left and the right side at pan position `pan`, -1.0 hard left to +1.0 hard right.
///
/// See the module comment for why the centre is unity on both sides rather than -3 dB.
#[inline(always)]
pub fn pan_gains(pan: f32) -> (f32, f32) {
    let p = pan.clamp(-1.0, 1.0);
    // Exact 1.0 on the side that stays, exact 0.0 at the far end: a hard-panned track has to be
    // digitally silent on the other side, not 1e-8 quiet.
    let left = if p <= 0.0 { 1.0 } else { 1.0 - p };
    let right = if p >= 0.0 { 1.0 } else { 1.0 + p };
    (left, right)
}

/// How many channels one loop buffer of a track holds.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Channels {
    /// One microphone, one pickup. Half the memory of a stereo buffer and not one sound less.
    #[default]
    Mono,
    /// A piano, a pad, anything that arrives on a pair of inputs.
    Stereo,
}

impl Channels {
    /// Samples per frame: 1 or 2.
    #[inline(always)]
    pub fn count(self) -> usize {
        match self {
            Channels::Mono => 1,
            Channels::Stereo => 2,
        }
    }

    /// Index into the two-kind buffer pool: mono is 0, stereo is 1.
    #[inline(always)]
    pub fn index(self) -> usize {
        self.count() - 1
    }

    /// From a channel count. Anything but 2 is mono, so a bad number cannot invent a channel.
    pub fn from_count(count: usize) -> Self {
        if count >= 2 {
            Channels::Stereo
        } else {
            Channels::Mono
        }
    }

    /// German word for the display.
    pub fn label(self) -> &'static str {
        match self {
            Channels::Mono => "mono",
            Channels::Stereo => "stereo",
        }
    }
}

/// Which device input channels a track records, zero-based inside an interleaved input frame.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TrackInput {
    /// One channel; the loop buffer is mono.
    Mono(usize),
    /// A pair; the loop buffer is stereo. The two need not be adjacent - a router can put the two
    /// halves of a stereo return wherever it likes.
    Stereo { left: usize, right: usize },
}

impl TrackInput {
    #[inline(always)]
    pub fn channels(self) -> Channels {
        match self {
            TrackInput::Mono(_) => Channels::Mono,
            TrackInput::Stereo { .. } => Channels::Stereo,
        }
    }

    /// Device channel feeding buffer channel `i`. For a mono track every `i` is the one channel,
    /// which is what makes the recording loop in `process.rs` free of special cases.
    #[inline(always)]
    pub fn device_channel(self, i: usize) -> usize {
        match self {
            TrackInput::Mono(c) => c,
            TrackInput::Stereo { left, right } => {
                if i == 1 {
                    right
                } else {
                    left
                }
            }
        }
    }

    /// The first (or only) device channel.
    #[inline(always)]
    pub fn first(self) -> usize {
        self.device_channel(0)
    }

    /// Highest device channel this track needs, for checking against what the device offers.
    pub fn highest(self) -> usize {
        match self {
            TrackInput::Mono(c) => c,
            TrackInput::Stereo { left, right } => left.max(right),
        }
    }

    /// Both channels as a fixed pair for the status snapshot; the second entry is meaningless on a
    /// mono track and repeats the first.
    pub fn pair(self) -> [usize; 2] {
        [self.device_channel(0), self.device_channel(1)]
    }

    /// `1` or `3+4`, one-based, as the channels are printed on the interface.
    pub fn label(self) -> String {
        match self {
            TrackInput::Mono(c) => format!("{}", c + 1),
            TrackInput::Stereo { left, right } => format!("{}+{}", left + 1, right + 1),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_mono_sample_fans_out_to_both_sides_untouched() {
        let f = Frame::mono(0.25);
        assert_eq!((f.l, f.r), (0.25, 0.25));
        assert_eq!(f.max_abs(), 0.25);
        assert!(Frame::SILENT.is_silent());
        assert!(!f.is_silent());
        assert_eq!(f.channel(0), 0.25);
        assert_eq!(f.channel(1), 0.25);
        // An index that does not exist must not panic in an audio callback.
        assert_eq!(f.channel(7), 0.25);
    }

    /// The pan law, as three exact numbers: unity in the centre, silence on the far side.
    #[test]
    fn the_pan_law_is_unity_in_the_centre_and_silent_at_the_far_end() {
        assert_eq!(pan_gains(0.0), (1.0, 1.0), "Mitte ist Einheitsverstaerkung");
        assert_eq!(pan_gains(-1.0), (1.0, 0.0), "ganz links laesst rechts still");
        assert_eq!(pan_gains(1.0), (0.0, 1.0), "ganz rechts laesst links still");
        assert_eq!(pan_gains(-0.5), (1.0, 0.5));
        assert_eq!(pan_gains(0.5), (0.5, 1.0));
        // Out of range is clamped rather than inverted.
        assert_eq!(pan_gains(-4.0), (1.0, 0.0));
        assert_eq!(pan_gains(4.0), (0.0, 1.0));
    }

    #[test]
    fn a_track_input_knows_its_channel_count_and_its_device_channels() {
        let mono = TrackInput::Mono(0);
        assert_eq!(mono.channels(), Channels::Mono);
        assert_eq!(mono.channels().count(), 1);
        assert_eq!(mono.device_channel(0), 0);
        assert_eq!(mono.device_channel(1), 0, "mono liest immer denselben Kanal");
        assert_eq!(mono.label(), "1");

        let stereo = TrackInput::Stereo { left: 2, right: 3 };
        assert_eq!(stereo.channels(), Channels::Stereo);
        assert_eq!(stereo.channels().count(), 2);
        assert_eq!(stereo.device_channel(0), 2);
        assert_eq!(stereo.device_channel(1), 3);
        assert_eq!(stereo.highest(), 3);
        assert_eq!(stereo.pair(), [2, 3]);
        assert_eq!(stereo.label(), "3+4");

        assert_eq!(Channels::from_count(1), Channels::Mono);
        assert_eq!(Channels::from_count(2), Channels::Stereo);
        assert_eq!(Channels::Mono.index(), 0);
        assert_eq!(Channels::Stereo.index(), 1);
    }
}
