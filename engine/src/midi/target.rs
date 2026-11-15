//! The parameter tree: a stable, textual address for everything that can be operated.
//!
//! # Why an address at all
//!
//! A learn mode has to *write down* what it learned, and a file has to say what it binds without
//! referring to a Rust enum. So every operable thing needs a name that survives being written to
//! disk, read back in another session and shown in a list. That name is a dotted path:
//!
//! ```text
//! transport.next                     der Release-Knopf
//! transport.goto.3                   Sprung zu Sektion 3
//! global.click                       Klick an/aus
//! track.1.record                     Aufnahme auf Track 1
//! track.stimme.monitor               Mithoeren auf dem Track namens "stimme"
//! track.1.gain                       Kanal-Fader von Track 1
//! track.1.mute / track.1.solo        Kanal stumm / solo
//! track.1.layer.2.gain               Lautstaerke der zweiten Ebene
//! track.1.fx.bypass                  ganze Effektkette umgehen
//! track.1.fx.reverb.on               Hall an/aus
//! track.1.fx.reverb.mix              Hall-Anteil
//! track.1.fx.eq.2.gain               Verstaerkung des zweiten EQ-Bandes
//! track.1.fx.preset.stimme           Preset "stimme" laden
//! ```
//!
//! Everything is 1-based in the address and 0-based inside: `track.1` is `tracks[0]`, `layer.2` is
//! `layers[1]`, `eq.3` is `bands[2]`. The address is what a musician reads, and a musician counts
//! from one - the same rule the CLI and the status display already follow.
//!
//! # Tracks by number *and* by name, and why both are needed
//!
//! A controller profile outlives the song it was made for. If it says `track.stimme.record`, it
//! stops working the moment a score calls the same voice `gesang`, and the musician has to learn
//! his pads again for every new piece - which is exactly the thing the two-stage design exists to
//! prevent. So a **profile** should say `track.1`: position is what a control surface actually
//! means ("the first pad row is the first track").
//!
//! A score, on the other hand, knows its own track names and is written by hand. `track.stimme` is
//! readable and cannot silently point at the wrong track when the track order changes. So a
//! **score supplement** says `track.stimme`.
//!
//! Hence [`TrackRef`] carries both, and the rule for reading one is mechanical: **a segment made
//! only of digits is a position, anything else is a name.** A track whose name is a number is
//! therefore unaddressable by name, and so is one whose name contains a dot; both are noted where
//! the score reads its track list.
//!
//! # Three kinds of control
//!
//! [`Control`] says what a target *is*, and that decides what a MIDI event may do to it:
//!
//! | Kind | Beispiel | Was ein Pad tut | Was ein Regler tut |
//! |---|---|---|---|
//! | [`Control::Trigger`] | `record`, `transport.next` | ausloesen | ausloesen beim Ueberschreiten von 64 |
//! | [`Control::Switch`] | `monitor`, `fx.reverb.on` | umschalten oder halten | an ab 64 |
//! | [`Control::Range`] | `pan`, `fx.reverb.mix` | Anschlagstaerke wird zum Wert | Wert fahren |
//!
//! The ranges are not invented here. Every one of them is the clamp the engine itself applies, read
//! off `engine::fx` (`dynamics.rs`, `delay.rs`, `reverb.rs`, `fx/mod.rs`) and the checks in the
//! app's command layer - so a knob turned to its stop lands exactly on the value the engine would
//! have clamped it to anyway, and no binding can produce something the engine refuses.

use std::fmt;

use crate::engine::bus::{Bus, MAX_BUS_GAIN, TrackSource};
use crate::engine::command::MAX_LEVEL_GAIN;
use crate::engine::track::MAX_TRACK_GAIN;
use crate::engine::fx::{EQ_BANDS, FxParam, FxPreset, FxSlot};
use crate::engine::fx::{BandKind, DelayNote};
use crate::engine::insert::MAX_INSERTS;
use crate::engine::schedule::Quantize;

/// Which track an address means.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TrackRef {
    /// Position in the session's track list, 0-based. Written 1-based.
    Index(usize),
    /// The name the score gives the track.
    Name(String),
}

impl TrackRef {
    /// A segment made only of digits is a position, anything else is a name.
    pub fn parse(text: &str) -> Result<Self, String> {
        if text.is_empty() {
            return Err("The address is missing the track (e.g. track.1.record).".to_string());
        }
        if text.bytes().all(|b| b.is_ascii_digit()) {
            let number: usize = text
                .parse()
                .map_err(|_| format!("'{text}' is not a track number."))?;
            if number == 0 {
                return Err("Tracks are counted from 1, there is no 'track.0'.".to_string());
            }
            return Ok(TrackRef::Index(number - 1));
        }
        Ok(TrackRef::Name(text.to_string()))
    }
}

impl fmt::Display for TrackRef {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            TrackRef::Index(i) => write!(f, "{}", i + 1),
            TrackRef::Name(name) => f.write_str(name),
        }
    }
}

/// How a knob's 0..127 is spread over a target's range.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Curve {
    /// Equal steps. Right for anything measured in dB, in a ratio or in a share of 0..1 - those
    /// scales are already perceptual.
    Linear,
    /// Equal *factors*. Right for frequencies and times: from 20 Hz to 20 kHz linearly, the whole
    /// bass register would sit in the first two of 127 steps.
    Logarithmic,
    /// A mixing-desk fader on a linear gain: controller 0 is silence, 1..127 are equal steps in
    /// **dB** from [`FADER_FLOOR_DB`] up to the range's top. Equal steps in gain would spend half
    /// the travel on the top 6 dB; equal steps in dB is what a fader feels like. With the top at
    /// +12 dB, unity (0 dB) lands exactly on controller 106.
    Fader,
}

/// Bottom of a [`Curve::Fader`] above silence: controller 1.
pub const FADER_FLOOR_DB: f32 = -60.0;

/// A continuous target's range, and how a controller's 0..127 lands in it.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Range {
    pub min: f32,
    pub max: f32,
    pub curve: Curve,
}

impl Range {
    pub const fn linear(min: f32, max: f32) -> Self {
        Self {
            min,
            max,
            curve: Curve::Linear,
        }
    }

    /// A fader from silence (`min`, normally 0) to `max`, in dB steps. See [`Curve::Fader`].
    pub const fn fader(min: f32, max: f32) -> Self {
        Self {
            min,
            max,
            curve: Curve::Fader,
        }
    }

    pub const fn log(min: f32, max: f32) -> Self {
        Self {
            min,
            max,
            curve: Curve::Logarithmic,
        }
    }

    /// Replace the ends, keeping the curve. This is what `min:`/`max:` in a binding does - a knob
    /// that only has to cover 0 to 40 % reverb has 127 steps for those 40 %, not 51.
    #[must_use]
    pub fn with_bounds(self, min: Option<f32>, max: Option<f32>) -> Self {
        Self {
            min: min.unwrap_or(self.min),
            max: max.unwrap_or(self.max),
            curve: self.curve,
        }
    }

    /// A range that straddles zero has a musical centre, and a controller has a detent at 64. Pan,
    /// EQ gain and makeup gain are all of this kind, and on all three "put it exactly back to the
    /// middle" is a thing a musician does constantly.
    fn bipolar(&self) -> bool {
        self.min < 0.0 && self.max > 0.0 && self.curve == Curve::Linear
    }

    /// Controller value -> parameter value.
    ///
    /// Exact at both ends by construction (0 gives `min`, 127 gives `max`, without arithmetic that
    /// could land a hair off), and exact in the middle for a bipolar range: 64 gives 0.0.
    pub fn value_of(&self, cc: u8) -> f32 {
        let cc = cc.min(127);
        if cc == 0 {
            return self.min;
        }
        if cc == 127 {
            return self.max;
        }
        if self.bipolar() {
            return match cc.cmp(&64) {
                std::cmp::Ordering::Equal => 0.0,
                std::cmp::Ordering::Less => self.min + (f32::from(cc) / 64.0) * (0.0 - self.min),
                std::cmp::Ordering::Greater => (f32::from(cc) - 64.0) / 63.0 * self.max,
            };
        }
        if self.curve == Curve::Fader && self.max > 0.0 {
            let top = 20.0 * self.max.log10();
            let db = FADER_FLOOR_DB + f32::from(cc - 1) * (top - FADER_FLOOR_DB) / 126.0;
            return 10f32.powf(db / 20.0).clamp(self.min.min(self.max), self.max);
        }
        let t = f32::from(cc) / 127.0;
        match self.curve {
            Curve::Linear => self.min + t * (self.max - self.min),
            // Both ends of a logarithmic range are positive by construction (frequencies, Q, times);
            // a non-positive end falls back to linear rather than producing a NaN.
            Curve::Logarithmic if self.min > 0.0 && self.max > 0.0 => {
                self.min * (self.max / self.min).powf(t)
            }
            Curve::Logarithmic | Curve::Fader => self.min + t * (self.max - self.min),
        }
    }

    /// Parameter value -> the controller position that would produce it, as a fraction of 0..127.
    ///
    /// The exact inverse of [`Self::value_of`]. Only the pickup logic needs it, and it needs it to
    /// be the inverse: "has the knob passed the current value" is a question about controller
    /// positions, not about parameter values.
    pub fn cc_of(&self, value: f32) -> f32 {
        let (lo, hi) = (self.min.min(self.max), self.min.max(self.max));
        let value = value.clamp(lo, hi);
        if self.bipolar() {
            return if value <= 0.0 {
                64.0 * (value - self.min) / (0.0 - self.min)
            } else {
                64.0 + 63.0 * value / self.max
            };
        }
        if self.curve == Curve::Fader && self.max > 0.0 {
            let top = 20.0 * self.max.log10();
            let floor = 10f32.powf(FADER_FLOOR_DB / 20.0);
            if value <= 0.0 {
                return 0.0;
            }
            if value < floor {
                // Between silence and the floor: the stretch between controller 0 and 1.
                return value / floor;
            }
            let db = 20.0 * value.log10();
            return (1.0 + (db - FADER_FLOOR_DB) / (top - FADER_FLOOR_DB) * 126.0).clamp(0.0, 127.0);
        }
        let t = match self.curve {
            Curve::Logarithmic if self.min > 0.0 && self.max > 0.0 && self.max != self.min => {
                (value / self.min).ln() / (self.max / self.min).ln()
            }
            _ if self.max != self.min => (value - self.min) / (self.max - self.min),
            _ => 0.0,
        };
        (t * 127.0).clamp(0.0, 127.0)
    }
}

/// What a target is, and therefore what a MIDI control may do to it.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Control {
    /// Fires. Has no state to read back and no value to carry.
    Trigger,
    /// On or off.
    Switch,
    /// A number in a range.
    Range(Range),
}

// ---------------------------------------------------------------------------------------------
// The knobs of the effect chain
// ---------------------------------------------------------------------------------------------

/// One numeric parameter of a track's effect chain.
///
/// A separate enum from [`FxParam`] because that one carries the *value* and this one is the
/// *address*: a binding names a knob once and turns it a thousand times.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FxKnob {
    HighPassHz,
    /// Band index, 0-based.
    BandHz(usize),
    BandQ(usize),
    BandGainDb(usize),
    CompThresholdDb,
    CompRatio,
    CompAttackMs,
    CompReleaseMs,
    CompKneeDb,
    CompMakeupDb,
    DelayFeedback,
    DelayMix,
    ReverbSize,
    ReverbDamping,
    ReverbMix,
}

impl FxKnob {
    /// The range the engine itself clamps this parameter to. Every number here is quoted from the
    /// module that owns the parameter, not chosen again.
    pub fn range(self) -> Range {
        match self {
            // fx/mod.rs: `high_pass_hz.clamp(20.0, 1_000.0)`.
            FxKnob::HighPassHz => Range::log(20.0, 1_000.0),
            // fx/mod.rs, `BandSettings::clamped`.
            FxKnob::BandHz(_) => Range::log(20.0, 20_000.0),
            FxKnob::BandQ(_) => Range::log(0.1, 12.0),
            FxKnob::BandGainDb(_) => Range::linear(-24.0, 24.0),
            // fx/dynamics.rs, `CompSettings::clamped`.
            FxKnob::CompThresholdDb => Range::linear(-60.0, 0.0),
            FxKnob::CompRatio => Range::log(1.0, 20.0),
            FxKnob::CompAttackMs => Range::log(0.1, 200.0),
            FxKnob::CompReleaseMs => Range::log(5.0, 2_000.0),
            FxKnob::CompKneeDb => Range::linear(0.0, 24.0),
            FxKnob::CompMakeupDb => Range::linear(-24.0, 24.0),
            // fx/delay.rs: feedback is clamped at 0.95, above which it never decays.
            FxKnob::DelayFeedback => Range::linear(0.0, 0.95),
            FxKnob::DelayMix => Range::linear(0.0, 1.0),
            // fx/reverb.rs.
            FxKnob::ReverbSize => Range::linear(0.0, 1.0),
            FxKnob::ReverbDamping => Range::linear(0.0, 1.0),
            FxKnob::ReverbMix => Range::linear(0.0, 1.0),
        }
    }

    /// The engine command this knob produces at `value`.
    pub fn to_param(self, value: f32) -> FxParam {
        match self {
            FxKnob::HighPassHz => FxParam::HighPassHz(value),
            FxKnob::BandHz(band) => FxParam::BandHz { band, hz: value },
            FxKnob::BandQ(band) => FxParam::BandQ { band, q: value },
            FxKnob::BandGainDb(band) => FxParam::BandGainDb { band, db: value },
            FxKnob::CompThresholdDb => FxParam::CompThresholdDb(value),
            FxKnob::CompRatio => FxParam::CompRatio(value),
            FxKnob::CompAttackMs => FxParam::CompAttackMs(value),
            FxKnob::CompReleaseMs => FxParam::CompReleaseMs(value),
            FxKnob::CompKneeDb => FxParam::CompKneeDb(value),
            FxKnob::CompMakeupDb => FxParam::CompMakeupDb(value),
            FxKnob::DelayFeedback => FxParam::DelayFeedback(value),
            FxKnob::DelayMix => FxParam::DelayMix(value),
            FxKnob::ReverbSize => FxParam::ReverbSize(value),
            FxKnob::ReverbDamping => FxParam::ReverbDamping(value),
            FxKnob::ReverbMix => FxParam::ReverbMix(value),
        }
    }

    /// English name, for a list a human reads.
    pub fn label(self) -> String {
        match self {
            FxKnob::HighPassHz => "High-pass frequency".to_string(),
            FxKnob::BandHz(b) => format!("EQ band {} frequency", b + 1),
            FxKnob::BandQ(b) => format!("EQ band {} Q", b + 1),
            FxKnob::BandGainDb(b) => format!("EQ band {} gain", b + 1),
            FxKnob::CompThresholdDb => "Compressor threshold".to_string(),
            FxKnob::CompRatio => "Compressor ratio".to_string(),
            FxKnob::CompAttackMs => "Compressor attack".to_string(),
            FxKnob::CompReleaseMs => "Compressor release".to_string(),
            FxKnob::CompKneeDb => "Compressor knee".to_string(),
            FxKnob::CompMakeupDb => "Compressor makeup".to_string(),
            FxKnob::DelayFeedback => "Delay feedback".to_string(),
            FxKnob::DelayMix => "Delay mix".to_string(),
            FxKnob::ReverbSize => "Reverb size".to_string(),
            FxKnob::ReverbDamping => "Reverb damping".to_string(),
            FxKnob::ReverbMix => "Reverb mix".to_string(),
        }
    }

    /// The tail of the address, after `track.<ref>.fx.`.
    pub fn address(self) -> String {
        match self {
            FxKnob::HighPassHz => "high_pass.hz".to_string(),
            FxKnob::BandHz(b) => format!("eq.{}.hz", b + 1),
            FxKnob::BandQ(b) => format!("eq.{}.q", b + 1),
            FxKnob::BandGainDb(b) => format!("eq.{}.gain", b + 1),
            FxKnob::CompThresholdDb => "comp.threshold".to_string(),
            FxKnob::CompRatio => "comp.ratio".to_string(),
            FxKnob::CompAttackMs => "comp.attack".to_string(),
            FxKnob::CompReleaseMs => "comp.release".to_string(),
            FxKnob::CompKneeDb => "comp.knee".to_string(),
            FxKnob::CompMakeupDb => "comp.makeup".to_string(),
            FxKnob::DelayFeedback => "delay.feedback".to_string(),
            FxKnob::DelayMix => "delay.mix".to_string(),
            FxKnob::ReverbSize => "reverb.size".to_string(),
            FxKnob::ReverbDamping => "reverb.damping".to_string(),
            FxKnob::ReverbMix => "reverb.mix".to_string(),
        }
    }

    /// Every knob of a chain, in signal order - what `midi targets` prints and what the later
    /// learn UI lists.
    pub fn all() -> Vec<FxKnob> {
        let mut out = vec![FxKnob::HighPassHz];
        for band in 0..EQ_BANDS {
            out.push(FxKnob::BandHz(band));
            out.push(FxKnob::BandQ(band));
            out.push(FxKnob::BandGainDb(band));
        }
        out.extend([
            FxKnob::CompThresholdDb,
            FxKnob::CompRatio,
            FxKnob::CompAttackMs,
            FxKnob::CompReleaseMs,
            FxKnob::CompKneeDb,
            FxKnob::CompMakeupDb,
            FxKnob::DelayFeedback,
            FxKnob::DelayMix,
            FxKnob::ReverbSize,
            FxKnob::ReverbDamping,
            FxKnob::ReverbMix,
        ]);
        out
    }
}

// ---------------------------------------------------------------------------------------------
// The tree
// ---------------------------------------------------------------------------------------------

/// Widest sensible sweep for the manual latency surcharge, in frames: 100 ms at 48 kHz either way.
///
/// Not the engine's limit (`engine::live::MAX_LATENCY_FRAMES` is two seconds) but the range a knob
/// is *useful* over. The surcharge is dialled in by ear against a click, and everything that
/// matters happens inside a few milliseconds; a knob spread over two seconds would move by 1.5 ms
/// per step and never find the spot. A binding may widen it with `min:`/`max:`.
pub const LATENCY_TRIM_SWEEP: f32 = 4_800.0;

/// Whose insert rack a plugin address names: a track's or a bus's.
#[derive(Clone, Debug, PartialEq)]
pub enum InsertOwner {
    Track(TrackRef),
    Bus(Bus),
}

impl fmt::Display for InsertOwner {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            InsertOwner::Track(t) => write!(f, "track.{t}"),
            InsertOwner::Bus(b) => write!(f, "global.bus.{}", b.name()),
        }
    }
}

impl InsertOwner {
    fn label(&self) -> String {
        match self {
            InsertOwner::Track(t) => format!("Track {t}"),
            InsertOwner::Bus(b) => format!("Bus {}", b.label()),
        }
    }
}

/// Everything that can be bound to a MIDI control.
#[derive(Clone, Debug, PartialEq)]
pub enum Target {
    // ---- the score's transport. Available while a score runs - it *is* the score's transport ---
    /// Count-in and first section.
    ScoreStart,
    /// The release button.
    ScoreNext,
    ScoreStopAll,
    /// Jump to a section, 0-based inside, 1-based in the address.
    ScoreGoto(usize),

    // ---- global ----------------------------------------------------------------------------
    /// Start or stop the clock of free looping (`transport.toggle`). Not part of the score's
    /// transport: it needs no score, and while a score runs it is refused like a record pad.
    Transport,
    Click,
    ClearAll,
    Tempo,
    Quantize(Quantize),
    /// Volume of one output bus. The obvious thing to put on a fader: turning the headphones down
    /// mid-song without touching the room is half the reason there are two buses.
    BusGain(Bus),
    /// Volume of the click, linear `0..=MAX_LEVEL_GAIN`. On the monitor bus only.
    ClickGain,
    /// Volume of every monitored live input, linear `0..=MAX_LEVEL_GAIN`. Output side only.
    MonitorGain,
    /// "All notes off" on every instrument (`global.panic`).
    Panic,

    // ---- per track -------------------------------------------------------------------------
    Record(TrackRef),
    Overdub(TrackRef),
    Play(TrackRef),
    StopTrack(TrackRef),
    ClearTrack(TrackRef),
    Monitor(TrackRef),
    Pan(TrackRef),
    /// The channel fader, linear `0..=MAX_TRACK_GAIN` (+12 dB), on a dB-stepped fader curve.
    Gain(TrackRef),
    /// Channel mute and solo. Listening only - neither reaches a recording.
    Mute(TrackRef),
    Solo(TrackRef),
    /// Whether one source of a track (its loop or its monitored input) is heard on one bus. One
    /// address per source *and* per bus, rather than one address carrying a set: a pad is a
    /// switch, and "geht dieser Track auf den Saal" is the question a musician actually presses.
    Send(TrackRef, TrackSource, Bus),
    /// The *manual surcharge* of the latency compensation - the half a human sets by ear. The
    /// measured half belongs to `calibrate` and is never overwritten from here; see
    /// `docs/architektur.md` section 4.
    LatencyTrim(TrackRef),
    /// "All notes off" on this track's instrument (`track.<n>.instrument.panic`).
    InstrumentPanic(TrackRef),

    // ---- per layer -------------------------------------------------------------------------
    LayerMute(TrackRef, usize),
    LayerRemove(TrackRef, usize),
    LayerGain(TrackRef, usize),

    // ---- the effect chain --------------------------------------------------------------------
    FxBypass(TrackRef),
    FxEnabled(TrackRef, FxSlot),
    FxPreset(TrackRef, FxPreset),
    FxDelayNote(TrackRef, DelayNote),
    FxBandKind(TrackRef, usize, BandKind),
    FxKnob(TrackRef, FxKnob),

    // ---- plugin inserts (`engine::insert`) ------------------------------------------------------
    /// The host's bypass of one insert slot, 0-based inside, 1-based in the address.
    InsertBypass(InsertOwner, usize),
    /// One parameter of the plugin in a slot, by the plugin's own parameter id, normalised 0..1.
    /// The id is what the plugin calls it - stable across sessions, unlike a list position.
    InsertParam(InsertOwner, usize, u32),
}

impl Target {
    /// What kind of control this is, and with it the range a knob covers.
    pub fn control(&self) -> Control {
        match self {
            Target::ScoreStart
            | Target::ScoreNext
            | Target::ScoreStopAll
            | Target::ScoreGoto(_)
            | Target::ClearAll
            | Target::Transport
            | Target::Quantize(_)
            | Target::Record(_)
            | Target::Overdub(_)
            | Target::Play(_)
            | Target::StopTrack(_)
            | Target::ClearTrack(_)
            | Target::Panic
            | Target::InstrumentPanic(_)
            | Target::LayerRemove(..)
            | Target::FxPreset(..)
            | Target::FxDelayNote(..)
            | Target::FxBandKind(..) => Control::Trigger,

            Target::Click
            | Target::Monitor(_)
            | Target::Mute(_)
            | Target::Solo(_)
            | Target::LayerMute(..)
            | Target::Send(..)
            | Target::FxBypass(_)
            | Target::FxEnabled(..)
            | Target::InsertBypass(..) => Control::Switch,

            // Every plugin parameter travels normalised (`plugin::ParamInfo`).
            Target::InsertParam(..) => Control::Range(Range::linear(0.0, 1.0)),

            // The pan law: -1 hard left, 0 centre, +1 hard right (`engine::frame`).
            Target::Pan(_) => Control::Range(Range::linear(-1.0, 1.0)),
            // The app refuses a layer gain outside 0..4; 1.0 sits at CC 32.
            Target::LayerGain(..) => Control::Range(Range::linear(0.0, 4.0)),
            // `engine::bus::MAX_BUS_GAIN`, the same headroom a layer gain has.
            Target::BusGain(_) => Control::Range(Range::linear(0.0, MAX_BUS_GAIN)),
            Target::ClickGain | Target::MonitorGain => {
                Control::Range(Range::linear(0.0, MAX_LEVEL_GAIN))
            }
            // Silence at 0, then dB steps from -60 to +12; unity on controller 106.
            Target::Gain(_) => Control::Range(Range::fader(0.0, MAX_TRACK_GAIN)),
            // The tempo the score compiler and `Timeline::validate` both accept.
            Target::Tempo => Control::Range(Range::linear(20.0, 400.0)),
            Target::LatencyTrim(_) => {
                Control::Range(Range::linear(-LATENCY_TRIM_SWEEP, LATENCY_TRIM_SWEEP))
            }
            Target::FxKnob(_, knob) => Control::Range(knob.range()),
        }
    }

    /// The track this target belongs to, if any.
    pub fn track(&self) -> Option<&TrackRef> {
        match self {
            Target::Record(t)
            | Target::Overdub(t)
            | Target::Play(t)
            | Target::StopTrack(t)
            | Target::ClearTrack(t)
            | Target::Monitor(t)
            | Target::Pan(t)
            | Target::Gain(t)
            | Target::Mute(t)
            | Target::Solo(t)
            | Target::Send(t, ..)
            | Target::LatencyTrim(t)
            | Target::InstrumentPanic(t)
            | Target::LayerMute(t, _)
            | Target::LayerRemove(t, _)
            | Target::LayerGain(t, _)
            | Target::FxBypass(t)
            | Target::FxEnabled(t, _)
            | Target::FxPreset(t, _)
            | Target::FxDelayNote(t, _)
            | Target::FxBandKind(t, ..)
            | Target::FxKnob(t, _)
            | Target::InsertBypass(InsertOwner::Track(t), _)
            | Target::InsertParam(InsertOwner::Track(t), ..) => Some(t),
            Target::InsertBypass(InsertOwner::Bus(_), _)
            | Target::InsertParam(InsertOwner::Bus(_), ..) => None,
            Target::ScoreStart
            | Target::ScoreNext
            | Target::ScoreStopAll
            | Target::ScoreGoto(_)
            | Target::Click
            | Target::ClearAll
            | Target::Transport
            | Target::Tempo
            | Target::BusGain(_)
            | Target::ClickGain
            | Target::MonitorGain
            | Target::Panic
            | Target::Quantize(_) => None,
        }
    }

    /// The same target on another track; a target without a track comes back unchanged.
    ///
    /// What turns `track.stimme.record` into `track.1.record` once the session says which track
    /// "stimme" is - the spelling a user interface element carries in its `data-midi` attribute.
    #[must_use]
    pub fn with_track(&self, track: TrackRef) -> Target {
        let t = track;
        match self.clone() {
            Target::Record(_) => Target::Record(t),
            Target::Overdub(_) => Target::Overdub(t),
            Target::Play(_) => Target::Play(t),
            Target::StopTrack(_) => Target::StopTrack(t),
            Target::ClearTrack(_) => Target::ClearTrack(t),
            Target::Monitor(_) => Target::Monitor(t),
            Target::Pan(_) => Target::Pan(t),
            Target::Gain(_) => Target::Gain(t),
            Target::Mute(_) => Target::Mute(t),
            Target::Solo(_) => Target::Solo(t),
            Target::Send(_, source, bus) => Target::Send(t, source, bus),
            Target::LatencyTrim(_) => Target::LatencyTrim(t),
            Target::InstrumentPanic(_) => Target::InstrumentPanic(t),
            Target::LayerMute(_, l) => Target::LayerMute(t, l),
            Target::LayerRemove(_, l) => Target::LayerRemove(t, l),
            Target::LayerGain(_, l) => Target::LayerGain(t, l),
            Target::FxBypass(_) => Target::FxBypass(t),
            Target::FxEnabled(_, slot) => Target::FxEnabled(t, slot),
            Target::FxPreset(_, preset) => Target::FxPreset(t, preset),
            Target::FxDelayNote(_, note) => Target::FxDelayNote(t, note),
            Target::FxBandKind(_, band, kind) => Target::FxBandKind(t, band, kind),
            Target::FxKnob(_, knob) => Target::FxKnob(t, knob),
            Target::InsertBypass(InsertOwner::Track(_), slot) => {
                Target::InsertBypass(InsertOwner::Track(t), slot)
            }
            Target::InsertParam(InsertOwner::Track(_), slot, id) => {
                Target::InsertParam(InsertOwner::Track(t), slot, id)
            }
            other => other,
        }
    }

    /// Whether this is part of the score's transport, which only exists while a score is loaded.
    pub fn is_score_transport(&self) -> bool {
        matches!(
            self,
            Target::ScoreStart | Target::ScoreNext | Target::ScoreStopAll | Target::ScoreGoto(_)
        )
    }

    /// Whether operating this moves the transport, i.e. whether it puts a take somewhere on the
    /// timeline.
    ///
    /// **This is the same line the mouse runs into** (`Action::moves_transport` in the app's
    /// `host.rs`): while a score plays, the runner owns the transport and the human owns the mix.
    /// A pad that started a recording against a running score would put a take on the timeline the
    /// runner did not schedule, and its predicted loop geometry - the thing that makes every later
    /// overdub land on the right sample - would be wrong from that moment on. See
    /// `docs/architektur.md` section 10.
    ///
    /// The score's own transport (`transport.*`) is deliberately **not** in this list. It is the
    /// runner's own release button; refusing it while the score runs would refuse the one thing the
    /// musician is holding the controller for.
    pub fn moves_transport(&self) -> bool {
        matches!(
            self,
            Target::Record(_)
                | Target::Overdub(_)
                | Target::Play(_)
                | Target::StopTrack(_)
                | Target::ClearTrack(_)
                | Target::ClearAll
                | Target::Transport
                | Target::Tempo
                | Target::Quantize(_)
        )
    }

    /// English name, for a list a human reads.
    ///
    /// Deliberately not built from the engine's own `label()`s (`Bus`, `TrackSource`, `FxSlot`,
    /// `BandKind`, `Quantize`), which are German and belong to the terminal display; the words
    /// here come from the small private helpers at the end of this file.
    pub fn label(&self) -> String {
        let track = |t: &TrackRef| format!("Track {t}");
        match self {
            Target::ScoreStart => "Start score".to_string(),
            Target::ScoreNext => "Release (next section)".to_string(),
            Target::ScoreStopAll => "Stop all".to_string(),
            Target::ScoreGoto(i) => format!("Go to section {}", i + 1),
            Target::Transport => "Start/stop transport".to_string(),
            Target::Click => "Click".to_string(),
            Target::ClearAll => "Clear all".to_string(),
            Target::Tempo => "Tempo".to_string(),
            Target::Quantize(q) => format!("Quantize: {}", quantize_word(*q)),
            Target::Record(t) => format!("{}: Record", track(t)),
            Target::Overdub(t) => format!("{}: Overdub", track(t)),
            Target::Play(t) => format!("{}: Play", track(t)),
            Target::StopTrack(t) => format!("{}: Stop", track(t)),
            Target::ClearTrack(t) => format!("{}: Clear", track(t)),
            Target::BusGain(bus) => format!("Volume {}", bus_word(*bus)),
            Target::Monitor(t) => format!("{}: Monitor", track(t)),
            Target::Pan(t) => format!("{}: Pan", track(t)),
            Target::Gain(t) => format!("{}: Channel fader", track(t)),
            Target::Mute(t) => format!("{}: Mute", track(t)),
            Target::Solo(t) => format!("{}: Solo", track(t)),
            Target::ClickGain => "Click volume".to_string(),
            Target::MonitorGain => "Monitoring volume".to_string(),
            Target::Panic => "All notes off".to_string(),
            Target::InstrumentPanic(t) => format!("{}: All notes off", track(t)),
            Target::Send(t, source, bus) => format!(
                "{}: {} to {}",
                track(t),
                source_word(*source),
                bus_word(*bus)
            ),
            Target::LatencyTrim(t) => format!("{}: Latency trim", track(t)),
            Target::LayerMute(t, l) => format!("{}: Layer {} mute", track(t), l + 1),
            Target::LayerRemove(t, l) => format!("{}: Layer {} remove", track(t), l + 1),
            Target::LayerGain(t, l) => format!("{}: Layer {} volume", track(t), l + 1),
            Target::FxBypass(t) => format!("{}: Bypass effects", track(t)),
            Target::FxEnabled(t, slot) => format!("{}: {}", track(t), slot_word(*slot)),
            Target::FxPreset(t, preset) => {
                format!("{}: Load preset \"{}\"", track(t), preset.label())
            }
            Target::FxDelayNote(t, note) => {
                format!("{}: Delay at {}", track(t), note.label())
            }
            Target::FxBandKind(t, band, kind) => {
                format!(
                    "{}: EQ band {} as {}",
                    track(t),
                    band + 1,
                    band_kind_word(*kind)
                )
            }
            Target::FxKnob(t, knob) => format!("{}: {}", track(t), knob.label()),
            Target::InsertBypass(owner, slot) => {
                format!("{}: plugin slot {} bypass", owner.label(), slot + 1)
            }
            Target::InsertParam(owner, slot, id) => {
                format!("{}: plugin slot {}, parameter {id}", owner.label(), slot + 1)
            }
        }
    }

    /// Parse an address. English errors with a way out, because these are read out of files people
    /// write by hand.
    pub fn parse(address: &str) -> Result<Self, String> {
        let address = address.trim();
        // The two names the Ableton-era score format used for its only two bindings. They keep
        // working: a file that compiled yesterday compiles today.
        match address {
            "next_section" => return Ok(Target::ScoreNext),
            "stop_all" => return Ok(Target::ScoreStopAll),
            _ => {}
        }
        let parts: Vec<&str> = address.split('.').collect();
        match parts.as_slice() {
            ["transport", "start"] => Ok(Target::ScoreStart),
            ["transport", "next"] => Ok(Target::ScoreNext),
            ["transport", "stop_all"] => Ok(Target::ScoreStopAll),
            ["transport", "toggle"] => Ok(Target::Transport),
            ["transport", "goto", n] => Ok(Target::ScoreGoto(one_based(n, "section")?)),
            ["global", "click"] => Ok(Target::Click),
            ["global", "clear_all"] => Ok(Target::ClearAll),
            ["global", "tempo"] => Ok(Target::Tempo),
            ["global", "click_gain"] => Ok(Target::ClickGain),
            ["global", "monitor_gain"] => Ok(Target::MonitorGain),
            ["global", "panic"] => Ok(Target::Panic),
            ["global", "quantize", "bar"] => Ok(Target::Quantize(Quantize::Bar)),
            ["global", "quantize", "loop"] => Ok(Target::Quantize(Quantize::Loop)),
            ["global", "bus", name, "gain"] => match Bus::parse(name) {
                Some(bus) => Ok(Target::BusGain(bus)),
                None => Err(bus_unknown(address, name)),
            },
            ["global", "bus", name, "insert", rest @ ..] => match Bus::parse(name) {
                Some(bus) => parse_insert(InsertOwner::Bus(bus), rest, address),
                None => Err(bus_unknown(address, name)),
            },
            ["track", track, rest @ ..] => {
                let track = TrackRef::parse(track)?;
                parse_track(track, rest, address)
            }
            _ => Err(unknown(address)),
        }
    }
}

fn parse_track(track: TrackRef, rest: &[&str], address: &str) -> Result<Target, String> {
    match rest {
        ["record"] => Ok(Target::Record(track)),
        ["overdub"] => Ok(Target::Overdub(track)),
        ["play"] => Ok(Target::Play(track)),
        ["stop"] => Ok(Target::StopTrack(track)),
        ["clear"] => Ok(Target::ClearTrack(track)),
        ["monitor"] => Ok(Target::Monitor(track)),
        ["pan"] => Ok(Target::Pan(track)),
        ["gain"] => Ok(Target::Gain(track)),
        ["mute"] => Ok(Target::Mute(track)),
        ["solo"] => Ok(Target::Solo(track)),
        ["bus", name] => match Bus::parse(name) {
            Some(bus) => Ok(Target::Send(track, TrackSource::Loop, bus)),
            None => Err(bus_unknown(address, name)),
        },
        ["monitor_bus", name] => match Bus::parse(name) {
            Some(bus) => Ok(Target::Send(track, TrackSource::Monitor, bus)),
            None => Err(bus_unknown(address, name)),
        },
        ["latency_trim"] => Ok(Target::LatencyTrim(track)),
        ["instrument", "panic"] => Ok(Target::InstrumentPanic(track)),
        ["layer", n, what] => {
            let layer = one_based(n, "layer")?;
            match *what {
                "mute" => Ok(Target::LayerMute(track, layer)),
                "remove" => Ok(Target::LayerRemove(track, layer)),
                "gain" => Ok(Target::LayerGain(track, layer)),
                _ => Err(format!(
                    "'{address}': a layer has 'mute', 'remove' and 'gain', not '{what}'."
                )),
            }
        }
        ["fx", rest @ ..] => parse_fx(track, rest, address),
        ["insert", rest @ ..] => parse_insert(InsertOwner::Track(track), rest, address),
        _ => Err(unknown(address)),
    }
}

/// `insert.<n>.bypass` and `insert.<n>.param.<id>`, after the owner.
fn parse_insert(owner: InsertOwner, rest: &[&str], address: &str) -> Result<Target, String> {
    let slot_of = |n: &str| -> Result<usize, String> {
        let slot = one_based(n, "Slot")?;
        if slot >= MAX_INSERTS {
            return Err(format!(
                "'{address}': there are plugin slots 1 to {MAX_INSERTS}."
            ));
        }
        Ok(slot)
    };
    match rest {
        [n, "bypass"] => Ok(Target::InsertBypass(owner, slot_of(n)?)),
        [n, "param", id] => {
            let id: u32 = id
                .parse()
                .map_err(|_| format!("'{address}': '{id}' is not a parameter id."))?;
            Ok(Target::InsertParam(owner, slot_of(n)?, id))
        }
        _ => Err(format!(
            "'{address}': a plugin slot has 'bypass' and 'param.<id>'."
        )),
    }
}

/// The preset's name inside a MIDI address. Fixed on purpose: profiles and scores on disk carry
/// these, so they must not follow the display label (`FxPreset::label`).
fn preset_address(preset: FxPreset) -> &'static str {
    match preset {
        FxPreset::Dry => "trocken",
        FxPreset::Voice => "stimme",
        FxPreset::PiezoGuitar => "gitarre",
        FxPreset::Custom => "eigen",
    }
}

fn parse_fx(track: TrackRef, rest: &[&str], address: &str) -> Result<Target, String> {
    match rest {
        ["bypass"] => Ok(Target::FxBypass(track)),
        ["preset", name] => match FxPreset::parse(name) {
            Some(FxPreset::Custom) | None => Err(format!(
                "'{address}': there is no preset '{name}'. There are {}.",
                FxPreset::loadable()
                    .iter()
                    .map(|p| preset_address(*p))
                    .collect::<Vec<_>>()
                    .join(", ")
            )),
            Some(preset) => Ok(Target::FxPreset(track, preset)),
        },
        ["eq", band, what] => {
            let band = one_based(band, "EQ band")?;
            if band >= EQ_BANDS {
                return Err(format!(
                    "'{address}': the EQ bands are 1 to {EQ_BANDS}."
                ));
            }
            match *what {
                "hz" => Ok(Target::FxKnob(track, FxKnob::BandHz(band))),
                "q" => Ok(Target::FxKnob(track, FxKnob::BandQ(band))),
                "gain" => Ok(Target::FxKnob(track, FxKnob::BandGainDb(band))),
                "peak" => Ok(Target::FxBandKind(track, band, BandKind::Peak)),
                "low_shelf" => Ok(Target::FxBandKind(track, band, BandKind::LowShelf)),
                "high_shelf" => Ok(Target::FxBandKind(track, band, BandKind::HighShelf)),
                _ => Err(format!(
                    "'{address}': an EQ band has 'hz', 'q', 'gain' and the band types \
                     'peak', 'low_shelf', 'high_shelf'."
                )),
            }
        }
        ["delay", "note", value] => match DelayNote::parse(value) {
            Some(note) => Ok(Target::FxDelayNote(track, note)),
            None => Err(format!(
                "'{address}': there is no note value '{value}'. There are {}.",
                DelayNote::all()
                    .iter()
                    .map(|n| n.name())
                    .collect::<Vec<_>>()
                    .join(", ")
            )),
        },
        [slot, "on"] => match FxSlot::parse(slot) {
            Some(slot) => Ok(Target::FxEnabled(track, slot)),
            None => Err(format!(
                "'{address}': there is no effect '{slot}'. There are {}.",
                slot_names().join(", ")
            )),
        },
        [group, knob] => {
            let wanted = format!("{group}.{knob}");
            FxKnob::all()
                .into_iter()
                .find(|k| k.address() == wanted)
                .map(|k| Target::FxKnob(track, k))
                .ok_or_else(|| unknown(address))
        }
        _ => Err(unknown(address)),
    }
}

fn bus_unknown(address: &str, name: &str) -> String {
    format!("'{address}': there is no bus '{name}'. There are main and monitor.")
}

fn slot_names() -> Vec<&'static str> {
    FxSlot::all().iter().map(|s| s.name()).collect()
}

/// `what` is a lower-case noun ("section", "layer", "EQ band") that the messages put in a sentence.
fn one_based(text: &str, what: &str) -> Result<usize, String> {
    let number: usize = text
        .parse()
        .map_err(|_| format!("'{text}' is not a {what} number."))?;
    if number == 0 {
        return Err(format!("{what}s are counted from 1, there is no {what} 0."));
    }
    Ok(number - 1)
}

fn unknown(address: &str) -> String {
    format!(
        "'{address}' is not a known address. 'looper-engine midi targets' lists every \
         address."
    )
}

impl fmt::Display for Target {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Target::ScoreStart => f.write_str("transport.start"),
            Target::ScoreNext => f.write_str("transport.next"),
            Target::ScoreStopAll => f.write_str("transport.stop_all"),
            Target::ScoreGoto(i) => write!(f, "transport.goto.{}", i + 1),
            Target::Transport => f.write_str("transport.toggle"),
            Target::Click => f.write_str("global.click"),
            Target::ClearAll => f.write_str("global.clear_all"),
            Target::Tempo => f.write_str("global.tempo"),
            Target::Quantize(Quantize::Bar) => f.write_str("global.quantize.bar"),
            Target::Quantize(Quantize::Loop) => f.write_str("global.quantize.loop"),
            Target::BusGain(bus) => write!(f, "global.bus.{}.gain", bus.name()),
            Target::Record(t) => write!(f, "track.{t}.record"),
            Target::Overdub(t) => write!(f, "track.{t}.overdub"),
            Target::Play(t) => write!(f, "track.{t}.play"),
            Target::StopTrack(t) => write!(f, "track.{t}.stop"),
            Target::ClearTrack(t) => write!(f, "track.{t}.clear"),
            Target::Monitor(t) => write!(f, "track.{t}.monitor"),
            Target::Pan(t) => write!(f, "track.{t}.pan"),
            Target::Gain(t) => write!(f, "track.{t}.gain"),
            Target::Mute(t) => write!(f, "track.{t}.mute"),
            Target::Solo(t) => write!(f, "track.{t}.solo"),
            Target::ClickGain => f.write_str("global.click_gain"),
            Target::MonitorGain => f.write_str("global.monitor_gain"),
            Target::Panic => f.write_str("global.panic"),
            Target::Send(t, TrackSource::Loop, bus) => write!(f, "track.{t}.bus.{}", bus.name()),
            Target::Send(t, TrackSource::Monitor, bus) => {
                write!(f, "track.{t}.monitor_bus.{}", bus.name())
            }
            Target::LatencyTrim(t) => write!(f, "track.{t}.latency_trim"),
            Target::InstrumentPanic(t) => write!(f, "track.{t}.instrument.panic"),
            Target::LayerMute(t, l) => write!(f, "track.{t}.layer.{}.mute", l + 1),
            Target::LayerRemove(t, l) => write!(f, "track.{t}.layer.{}.remove", l + 1),
            Target::LayerGain(t, l) => write!(f, "track.{t}.layer.{}.gain", l + 1),
            Target::FxBypass(t) => write!(f, "track.{t}.fx.bypass"),
            Target::FxEnabled(t, slot) => write!(f, "track.{t}.fx.{}.on", slot.name()),
            Target::FxPreset(t, preset) => {
                write!(f, "track.{t}.fx.preset.{}", preset_address(*preset))
            }
            Target::FxDelayNote(t, note) => write!(f, "track.{t}.fx.delay.note.{}", note.name()),
            Target::FxBandKind(t, band, kind) => {
                write!(f, "track.{t}.fx.eq.{}.{}", band + 1, band_kind_name(*kind))
            }
            Target::FxKnob(t, knob) => write!(f, "track.{t}.fx.{}", knob.address()),
            Target::InsertBypass(owner, slot) => write!(f, "{owner}.insert.{}.bypass", slot + 1),
            Target::InsertParam(owner, slot, id) => {
                write!(f, "{owner}.insert.{}.param.{id}", slot + 1)
            }
        }
    }
}

fn band_kind_name(kind: BandKind) -> &'static str {
    match kind {
        BandKind::Peak => "peak",
        BandKind::LowShelf => "low_shelf",
        BandKind::HighShelf => "high_shelf",
    }
}

// English words for the engine types whose own `label()` is German. Only for `Target::label`.

fn bus_word(bus: Bus) -> &'static str {
    match bus {
        Bus::Main => "main",
        Bus::Monitor => "monitor",
    }
}

fn source_word(source: TrackSource) -> &'static str {
    match source {
        TrackSource::Loop => "Loop",
        TrackSource::Monitor => "Monitor",
    }
}

fn slot_word(slot: FxSlot) -> &'static str {
    match slot {
        FxSlot::HighPass => "High-pass",
        FxSlot::Eq => "EQ",
        FxSlot::Comp => "Compressor",
        FxSlot::Delay => "Delay",
        FxSlot::Reverb => "Reverb",
    }
}

fn band_kind_word(kind: BandKind) -> &'static str {
    match kind {
        BandKind::Peak => "peak",
        BandKind::LowShelf => "low shelf",
        BandKind::HighShelf => "high shelf",
    }
}

fn quantize_word(q: Quantize) -> &'static str {
    match q {
        Quantize::Bar => "bar",
        Quantize::Loop => "loop",
    }
}

/// Every address of a session with `tracks` tracks, `layers` layers each and `sections` sections.
///
/// This is the catalogue: what `looper-engine midi targets` prints, and what a learn mode in the
/// user interface will later offer as a list. It is generated from the same [`Target`] values the
/// parser produces, so the catalogue cannot drift away from what is actually bindable.
pub fn catalogue(tracks: usize, layers: usize, sections: usize) -> Vec<Target> {
    let mut out = vec![Target::ScoreStart, Target::ScoreNext, Target::ScoreStopAll];
    out.extend((0..sections).map(Target::ScoreGoto));
    out.extend([
        Target::Transport,
        Target::Click,
        Target::ClearAll,
        Target::Tempo,
        Target::Quantize(Quantize::Bar),
        Target::Quantize(Quantize::Loop),
    ]);
    out.extend(Bus::ALL.into_iter().map(Target::BusGain));
    out.extend([Target::ClickGain, Target::MonitorGain, Target::Panic]);
    for track in 0..tracks {
        let t = || TrackRef::Index(track);
        out.extend([
            Target::Record(t()),
            Target::Overdub(t()),
            Target::Play(t()),
            Target::StopTrack(t()),
            Target::ClearTrack(t()),
            Target::Monitor(t()),
            Target::Pan(t()),
            Target::Gain(t()),
            Target::Mute(t()),
            Target::Solo(t()),
            Target::LatencyTrim(t()),
            Target::InstrumentPanic(t()),
        ]);
        for source in TrackSource::ALL {
            out.extend(Bus::ALL.into_iter().map(|bus| Target::Send(t(), source, bus)));
        }
        for layer in 0..layers {
            out.extend([
                Target::LayerMute(t(), layer),
                Target::LayerGain(t(), layer),
                Target::LayerRemove(t(), layer),
            ]);
        }
        out.push(Target::FxBypass(t()));
        out.extend(FxSlot::all().into_iter().map(|s| Target::FxEnabled(t(), s)));
        out.extend(
            FxPreset::loadable()
                .into_iter()
                .map(|p| Target::FxPreset(t(), p)),
        );
        out.extend(
            DelayNote::all()
                .into_iter()
                .map(|n| Target::FxDelayNote(t(), n)),
        );
        for band in 0..EQ_BANDS {
            for kind in [BandKind::Peak, BandKind::LowShelf, BandKind::HighShelf] {
                out.push(Target::FxBandKind(t(), band, kind));
            }
        }
        out.extend(FxKnob::all().into_iter().map(|k| Target::FxKnob(t(), k)));
        out.extend((0..MAX_INSERTS).map(|s| Target::InsertBypass(InsertOwner::Track(t()), s)));
    }
    for bus in Bus::ALL {
        out.extend((0..MAX_INSERTS).map(|s| Target::InsertBypass(InsertOwner::Bus(bus), s)));
    }
    out
}
