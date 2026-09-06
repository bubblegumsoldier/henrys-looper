//! The score format and its compiled form, as Rust types.
//!
//! These structs *are* the schema. There is deliberately no JSON Schema file beside them:
//!
//! * The reader (`read.rs`) does not validate against a schema, it walks the marked YAML tree and
//!   reports German messages with positions. A JSON Schema could not produce those, so it would be
//!   a second, silently diverging description of the same format.
//! * `serde` derives here give the compiled form its serialisation for free, which is what the
//!   contract in `docs/contracts-v0.md` actually needs.
//!
//! # What the Ableton era left behind
//!
//! The old format addressed Ableton objects: `ableton_track`, `group`, `layers`, `reserve`. All
//! four are gone. Our engine stacks layers without limit, so a track no longer needs a pool of
//! child tracks - it needs to know **which input it listens on**. Hence [`TrackSource::input`],
//! 1-based, the way the channel is labelled on the front of the interface. The engine counts from
//! zero, so the compiled track carries both numbers.
//!
//! Since the engine became stereo, `input` may also be a *pair* - `input: [3, 4]` - and a track may
//! carry a `pan`. Both are written out and never inferred: which two inputs form a stereo return is
//! a fact about the cabling, and where an instrument sits between the speakers is a fact about the
//! arrangement.

use serde::{Deserialize, Serialize};

use super::map::OrderedMap;
use crate::engine::frame::TrackInput;
use crate::engine::schedule::Quantize;
use crate::engine::timeline::TimeSignature;

/// What a track is supposed to do during a section. The complete state, not a delta: a section
/// that does not mention a track puts it on [`TrackState::Stop`].
///
/// Not to be confused with [`crate::engine::track::TrackState`], which is what a track *is*
/// (empty, armed, recording, ...) at a given moment. This one is what the score *asks for*.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TrackState {
    /// Start a new loop: the take defines origin and loop length, older layers are dropped.
    Record,
    /// One more layer on top of the existing loop.
    Overdub,
    /// Play what is recorded.
    Play,
    /// Silent.
    Stop,
    /// Not playing, but the input is audible - singing along without recording.
    HearThrough,
}

impl TrackState {
    pub const ALL: [TrackState; 5] = [
        TrackState::Record,
        TrackState::Overdub,
        TrackState::Play,
        TrackState::Stop,
        TrackState::HearThrough,
    ];

    /// The spelling used in the YAML.
    pub fn as_str(self) -> &'static str {
        match self {
            TrackState::Record => "record",
            TrackState::Overdub => "overdub",
            TrackState::Play => "play",
            TrackState::Stop => "stop",
            TrackState::HearThrough => "hear_through",
        }
    }

    pub fn parse(text: &str) -> Option<Self> {
        TrackState::ALL.into_iter().find(|state| state.as_str() == text)
    }

    /// All spellings, for "Erlaubt: ..." and typo suggestions.
    pub fn names() -> [&'static str; 5] {
        [
            TrackState::Record.as_str(),
            TrackState::Overdub.as_str(),
            TrackState::Play.as_str(),
            TrackState::Stop.as_str(),
            TrackState::HearThrough.as_str(),
        ]
    }
}

// ---------------------------------------------------------------------------------------------
// Source format (what the musician writes)
// ---------------------------------------------------------------------------------------------

/// One entry of the top-level `tracks:` mapping.
///
/// ```yaml
/// tracks:
///   voice:   {input: 1}
///   gitarre: {input: 2, monitor: false}
///   klavier: {input: [3, 4], pan: 0.2}
/// ```
///
/// `input` is either one channel (the track records mono) or a pair (it records stereo). Which two
/// inputs belong together is a wiring decision, so it is always written out and never guessed.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct TrackSource {
    /// Input channel of the audio interface, **1-based**, as printed on the device. On a stereo
    /// track this is the left one of the pair.
    pub input: u32,
    /// Right-hand input of a stereo track, likewise 1-based. `None` means the track is mono.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub input_right: Option<u32>,
    /// Position in the stereo field: -1.0 hard left, 0.0 centre, +1.0 hard right. Default centre.
    #[serde(default)]
    pub pan: f32,
    /// Whether this track may be monitored (hearing the live input through the engine) at all.
    /// `hear_through` and the takes switch monitoring on for a track; `monitor: false` keeps it off
    /// throughout, which is what a track fed from a line source that is already audible wants.
    /// Default `true`.
    #[serde(default = "default_true")]
    pub monitor: bool,
}

fn default_true() -> bool {
    true
}

/// A MIDI binding, before it is turned into an id.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct MidiBindingSource {
    /// 1-based MIDI channel, default 1.
    pub channel: u8,
    pub kind: MidiKind,
    /// Note number or controller number, 0..=127.
    pub number: u8,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum MidiKind {
    Note,
    Cc,
}

impl MidiBindingSource {
    /// `ch1.note36` / `ch1.cc64` - the same id the MIDI monitor shows for an incoming event.
    pub fn id(&self) -> String {
        match self.kind {
            MidiKind::Note => format!("ch{}.note{}", self.channel, self.number),
            MidiKind::Cc => format!("ch{}.cc{}", self.channel, self.number),
        }
    }
}

/// One entry of the `sections:` list, before `repeat:` is resolved.
///
/// Everything except `id` is optional, because a section with `repeat:` inherits it from the
/// section it repeats and may override any part of it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SectionSource {
    pub id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub repeat: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bars: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub autorelease: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub quantize: Option<Quantize>,
    #[serde(default)]
    pub tracks: OrderedMap<TrackState>,
    /// 1-based line the section starts on. Not part of the written format - the reader fills it in
    /// so the block preview can point back at the source.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_line: Option<u32>,
}

/// A whole score as written, after validation but before `repeat:` is unrolled.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ScoreSource {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    pub bpm: f64,
    /// `"3/4"`. Optional; `beats_per_bar` is an alias for the numerator.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub time_signature: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub beats_per_bar: Option<u32>,
    pub tracks: OrderedMap<TrackSource>,
    #[serde(default, skip_serializing_if = "OrderedMap::is_empty")]
    pub midi: OrderedMap<MidiBindingSource>,
    pub sections: Vec<SectionSource>,
}

// ---------------------------------------------------------------------------------------------
// Compiled format (what the runner and the UI get)
// ---------------------------------------------------------------------------------------------

/// A track after compilation. Carries both numberings so nothing has to convert twice: `input` is
/// what the musician wrote, `input_channel` is what [`crate::engine::command::Command`] and the
/// status snapshot use.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct CompiledTrack {
    pub name: String,
    /// Position in `tracks`, which is the index the engine addresses this track by.
    pub index: usize,
    /// 1-based input channel as written in the score; the left one of a stereo pair.
    pub input: u32,
    /// The same channel 0-based, ready for the engine.
    pub input_channel: u32,
    /// Right-hand input of a stereo track, 1-based; `None` on a mono track.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub input_right: Option<u32>,
    /// The same channel 0-based.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub input_channel_right: Option<u32>,
    /// 1 for a mono loop buffer, 2 for a stereo one. Redundant with `input_right` and written out
    /// anyway, because it is the number every reader actually wants.
    pub channels: u32,
    /// Position in the stereo field, -1.0 to +1.0.
    pub pan: f32,
    pub monitor: bool,
}

impl CompiledTrack {
    /// The input in the form the engine wants it.
    pub fn track_input(&self) -> TrackInput {
        match self.input_channel_right {
            Some(right) => TrackInput::Stereo {
                left: self.input_channel as usize,
                right: right as usize,
            },
            None => TrackInput::Mono(self.input_channel as usize),
        }
    }
}

/// A fully resolved section: `repeat:` unrolled, defaults filled in, **every** track named.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CompiledSection {
    pub id: String,
    pub index: usize,
    pub bars: u32,
    pub autorelease: bool,
    pub quantize: Quantize,
    /// One entry per track of the score, in track order. Missing in the source means `stop`.
    pub tracks: OrderedMap<TrackState>,
    /// 1-based line this section starts on, for the block preview.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_line: Option<u32>,
}

/// A MIDI binding in the compiled score. An object rather than a bare string because the contract
/// says so and because a binding may grow fields (a device name, say) without breaking readers.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CompiledMidiBinding {
    pub id: String,
}

/// The compiled score - static, fully resolved, serialisable.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct CompiledScore {
    pub title: String,
    pub bpm: f64,
    /// `"3/4"`, kept as written for display.
    pub time_signature: String,
    /// Numerator of the time signature.
    pub beats_per_bar: u32,
    /// Denominator: the note value that gets one beat (4 = quarter, 8 = eighth).
    pub beat_unit: u32,
    pub tracks: Vec<CompiledTrack>,
    #[serde(default)]
    pub midi: OrderedMap<CompiledMidiBinding>,
    pub sections: Vec<CompiledSection>,
}

impl CompiledScore {
    /// The time signature in the form [`crate::engine::timeline::Timeline`] wants.
    pub fn signature(&self) -> TimeSignature {
        TimeSignature::new(self.beats_per_bar, self.beat_unit)
    }

    pub fn track(&self, name: &str) -> Option<&CompiledTrack> {
        self.tracks.iter().find(|track| track.name == name)
    }

    pub fn section(&self, id: &str) -> Option<&CompiledSection> {
        self.sections.iter().find(|section| section.id == id)
    }

    /// Total length of the score in bars, if it were played once without repeats of the release
    /// kind. Handy for a progress display; the runner does not depend on it.
    pub fn total_bars(&self) -> u64 {
        self.sections.iter().map(|section| u64::from(section.bars)).sum()
    }
}
