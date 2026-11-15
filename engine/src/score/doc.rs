//! The score as an editable document: text -> structured model -> text.
//!
//! ```text
//!   YAML ──parse_doc──► ScoreDoc ──(grafisch bearbeiten)──► ScoreDoc ──render_doc──► YAML
//!     │                   │  Kommentare je Element, Positionen,             │
//!     │                   │  "selbst angegeben" vs. "per repeat geerbt"     │
//!     └──── compile_score ◄──────────────────────────────────────────────────┘
//! ```
//!
//! The text stays the one format on disk and the one the compiler reads. A graphical editor works
//! on [`ScoreDoc`], and every edit goes back to text through [`render_doc`], which is then compiled
//! like any typed text. Three consequences, all deliberate:
//!
//! * **Validation is the compiler's.** [`render_doc`] renders and compiles; every issue comes back
//!   with the compiler's own message *and* with a [`ScoreRef`] naming the element it belongs to.
//!   The reference is found by looking the issue's position up in the rendered text, whose layout
//!   this module controls - so there is no second set of rules that could disagree with the first.
//! * **The document keeps what a section states itself.** `bars`, `autorelease`, `quantize` and the
//!   per-track states are `Option`s / a partial list, exactly as written. What a section ends up
//!   with after `repeat:` is in [`DocSection::resolved`], with the origin of every value.
//! * **Comments survive, formatting does not.** See [`render_doc`] for the rule.
//!
//! Nothing here touches the engine; it is pure text processing for the control thread.

use serde::{Deserialize, Serialize};

use super::compile_score;
use super::error::{ScoreError, ScoreIssue};
use super::map::OrderedMap;
use super::mixer::{
    BandLine, BusLevels, CompLine, DelayLine, HighPassLine, InsertLine, ReverbLine, SwitchLine,
    TrackMixSource,
};
use super::model::{CompiledScore, InstrumentSource, TrackState};
use super::read::{read_score, score_notes};
use super::yaml::{Node, NodeKind, Pos, parse_document};
use crate::engine::schedule::Quantize;

// ---------------------------------------------------------------------------------------------
// The model
// ---------------------------------------------------------------------------------------------

/// Comments attached to one element. `before` are whole comment lines directly above it, `after`
/// is the comment at the end of its own line. Texts are stored without the leading `#` and without
/// the one space after it, so `#   Eingang 1` keeps its alignment as `"  Eingang 1"`.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Comments {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub before: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub after: Option<String>,
}

impl Comments {
    pub fn is_empty(&self) -> bool {
        self.before.is_empty() && self.after.is_none()
    }

    fn add_after(&mut self, text: String) {
        self.after = Some(match self.after.take() {
            Some(previous) => format!("{previous}; {text}"),
            None => text,
        });
    }
}

/// One track. Every field but `name` and `input` is optional: `None` means "not written, the
/// default applies", which keeps a written-out default (`monitor: true`) distinguishable from an
/// omitted one.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct DocTrack {
    pub name: String,
    /// 1-based input, the left one of a stereo pair. 0 on an instrument track.
    pub input: u32,
    /// `input: instrument` - played by an instrument plugin over MIDI.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub instrument: bool,
    /// `midi: {device, channel, notes}` of an instrument track.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub midi: Option<InstrumentSource>,
    /// 1-based right input of a stereo track; `None` for mono.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub input_right: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pan: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub monitor: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub latency: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub latency_trim: Option<i32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bus: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub monitor_bus: Option<String>,
    /// `loop: audio|midi`, as written (reserved).
    #[serde(default, rename = "loop", skip_serializing_if = "Option::is_none")]
    pub loop_kind: Option<String>,
    /// `grid: 1/16`, as written (reserved).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub grid: Option<String>,
    #[serde(default, skip_serializing_if = "Comments::is_empty")]
    pub comments: Comments,
    /// 1-based source line. Output only; ignored when a document comes back from the UI.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub line: Option<u32>,
}

impl DocTrack {
    /// A mono track with nothing but its input - what a new track in an editor starts as.
    pub fn new(name: impl Into<String>, input: u32) -> Self {
        Self {
            name: name.into(),
            input,
            instrument: false,
            midi: None,
            input_right: None,
            pan: None,
            monitor: None,
            latency: None,
            latency_trim: None,
            bus: None,
            monitor_bus: None,
            loop_kind: None,
            grid: None,
            comments: Comments::default(),
            line: None,
        }
    }

    /// An instrument track played from `midi` (every device, channel and key when `None`).
    pub fn instrument(name: impl Into<String>, midi: Option<InstrumentSource>) -> Self {
        let mut track = Self::new(name, 0);
        track.instrument = true;
        track.midi = midi.filter(|m| *m != InstrumentSource::default());
        track
    }
}

/// One entry of `midi:`. The key is kept exactly as written (`next_section` stays
/// `next_section`); `mode` and `takeover` are the words of the format, checked by the compiler.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct DocMidiBinding {
    pub key: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cc: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub channel: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mode: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub takeover: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub min: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max: Option<f64>,
    #[serde(default, skip_serializing_if = "Comments::is_empty")]
    pub comments: Comments,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub line: Option<u32>,
}

/// One `track: state` line inside a section - only the ones the section states itself.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct DocSectionTrack {
    pub track: String,
    pub state: TrackState,
    /// The clip's own note grid (`{state: play, grid: 1/16}`), reserved.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub grid: Option<String>,
    #[serde(default, skip_serializing_if = "Comments::is_empty")]
    pub comments: Comments,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub line: Option<u32>,
}

/// One section as written. `None` means "not stated here" - inherited through `repeat:` or the
/// default. What it resolves to is in `resolved`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct DocSection {
    pub id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub repeat: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bars: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub autorelease: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub quantize: Option<Quantize>,
    /// Track states this section states itself, in written order.
    #[serde(default)]
    pub tracks: Vec<DocSectionTrack>,
    /// Comments above the `- id:` line and at its end.
    #[serde(default, skip_serializing_if = "Comments::is_empty")]
    pub comments: Comments,
    /// Comments of the other field lines, keyed by field name (`bars`, `repeat`, `tracks`, ...).
    #[serde(default, skip_serializing_if = "OrderedMap::is_empty")]
    pub field_comments: OrderedMap<Comments>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub line: Option<u32>,
    /// The section after `repeat:` and the defaults - output only, recomputed on every parse and
    /// render, ignored on input.
    #[serde(default, skip_deserializing)]
    pub resolved: Option<ResolvedSection>,
}

impl DocSection {
    pub fn new(id: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            repeat: None,
            bars: None,
            autorelease: None,
            quantize: None,
            tracks: Vec::new(),
            comments: Comments::default(),
            field_comments: OrderedMap::new(),
            line: None,
            resolved: None,
        }
    }
}

/// One entry of `mixer.tracks`: the track it is about and its mix as written (see
/// `score/mixer.rs` for what every field means). Written as one `{...}` line while it says nothing
/// about single effects, as a block of one line per field otherwise.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct DocMixerTrack {
    pub track: String,
    #[serde(flatten)]
    pub mix: TrackMixSource,
    #[serde(default, skip_serializing_if = "Comments::is_empty")]
    pub comments: Comments,
    /// Comments of the field lines of a block entry, keyed by field name (`gain`, `comp`, ...).
    #[serde(default, skip_serializing_if = "OrderedMap::is_empty")]
    pub field_comments: OrderedMap<Comments>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub line: Option<u32>,
}

impl DocMixerTrack {
    pub fn new(track: impl Into<String>, mix: TrackMixSource) -> Self {
        Self {
            track: track.into(),
            mix,
            comments: Comments::default(),
            field_comments: OrderedMap::new(),
            line: None,
        }
    }
}

/// The `mixer:` section. Comments of the `mixer:` key itself are in the root's `field_comments`,
/// those of its `buses:` and `tracks:` lines here.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct DocMixer {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub buses: Option<BusLevels>,
    #[serde(default)]
    pub tracks: Vec<DocMixerTrack>,
    #[serde(default, skip_serializing_if = "OrderedMap::is_empty")]
    pub field_comments: OrderedMap<Comments>,
}

/// Where a section's value comes from.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ValueSource {
    /// The section states it itself.
    Own,
    /// Inherited through `repeat:`, from the section that actually states it (the end of the
    /// chain, not necessarily the section named in `repeat:`).
    Repeat { index: usize, section: String },
    /// Nobody states it: the compiler's default (4 bars, no autorelease, `loop`, track `stop`).
    Default,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Resolved<T> {
    pub value: T,
    pub from: ValueSource,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResolvedTrackState {
    pub track: String,
    pub state: TrackState,
    pub from: ValueSource,
}

/// A section as the runner will play it, every value with its origin.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResolvedSection {
    pub bars: Resolved<u32>,
    pub autorelease: Resolved<bool>,
    pub quantize: Resolved<Quantize>,
    /// One entry per track of the score, in track order.
    pub tracks: Vec<ResolvedTrackState>,
}

/// The whole score as an editable document.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ScoreDoc {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    pub bpm: f64,
    /// `"3/4"`, as written.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub time_signature: Option<String>,
    /// The old alias for the numerator. Kept when written, so a file is not rewritten behind the
    /// musician's back; a new document uses `time_signature`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub beats_per_bar: Option<u32>,
    #[serde(default)]
    pub tracks: Vec<DocTrack>,
    #[serde(default)]
    pub midi: Vec<DocMidiBinding>,
    #[serde(default)]
    pub sections: Vec<DocSection>,
    /// The `mixer:` section; `None` when the score has none.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mixer: Option<DocMixer>,
    /// Comment lines at the top of the file, before the first key.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub header: Vec<String>,
    /// Comment lines after the last element.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub footer: Vec<String>,
    /// Comments of the top-level keys (`title`, `bpm`, `time_signature`, `beats_per_bar`,
    /// `tracks`, `midi`, `sections`, `mixer`).
    #[serde(default, skip_serializing_if = "OrderedMap::is_empty")]
    pub field_comments: OrderedMap<Comments>,
}

/// Which element of a document something belongs to. `field` names one field of it; `None` means
/// the element as a whole.
///
/// Indices are positions in the document's lists, names are carried along for display - and
/// because a duplicated name (the error being reported) is exactly when an index is needed.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ScoreRef {
    Score {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        field: Option<String>,
    },
    Track {
        index: usize,
        name: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        field: Option<String>,
    },
    Midi {
        index: usize,
        key: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        field: Option<String>,
    },
    Section {
        index: usize,
        id: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        field: Option<String>,
    },
    /// One `track: state` line of a section; `index` is its position in `DocSection::tracks`.
    SectionTrack {
        section: usize,
        section_id: String,
        index: usize,
        track: String,
    },
    /// The `mixer:` section; `field` is `buses` or `tracks`.
    Mixer {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        field: Option<String>,
    },
    /// One entry of `mixer.tracks`; `index` is its position in `DocMixer::tracks`.
    MixerTrack {
        index: usize,
        track: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        field: Option<String>,
    },
}

/// Where an element's key stands in the text.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Location {
    pub at: ScoreRef,
    pub line: u32,
    pub column: u32,
}

/// A compiler issue, plus the element it belongs to when that can be told.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModelIssue {
    pub line: Option<u32>,
    pub column: Option<u32>,
    pub message: String,
    pub suggestion: Option<String>,
    pub at: Option<ScoreRef>,
}

/// Answer of [`parse_doc_outcome`]: the document when the text compiles, every issue otherwise.
#[derive(Clone, Debug, Serialize)]
pub struct ParseOutcome {
    pub ok: bool,
    pub doc: Option<ScoreDoc>,
    pub score: Option<CompiledScore>,
    /// Every key's position in the text - also when the text has errors, as far as it parses.
    pub locations: Vec<Location>,
    pub errors: Vec<ModelIssue>,
    /// Not errors: what is written correctly but does nothing yet (`loop: midi`, `grid:`).
    pub notes: Vec<ModelIssue>,
}

/// Answer of [`render_doc_outcome`]: the canonical text, always, and the compiler's verdict on it.
#[derive(Clone, Debug, Serialize)]
pub struct RenderOutcome {
    pub ok: bool,
    pub yaml: String,
    /// When `ok`: the document parsed back from `yaml` (fresh lines and `resolved`). Otherwise the
    /// document as it was sent, with `resolved` recomputed.
    pub doc: ScoreDoc,
    pub score: Option<CompiledScore>,
    pub locations: Vec<Location>,
    /// Positions refer to `yaml`.
    pub errors: Vec<ModelIssue>,
    /// See [`ParseOutcome::notes`]; positions refer to `yaml`.
    pub notes: Vec<ModelIssue>,
}

// ---------------------------------------------------------------------------------------------
// Text -> document
// ---------------------------------------------------------------------------------------------

/// Parse a score into its document. Fails with the compiler's issues when the text does not
/// compile - a graphical editor only ever sees a valid score.
pub fn parse_doc(text: &str) -> Result<ScoreDoc, ScoreError> {
    read_score(text)?;
    let tree = parse_document(text)?;
    let mut anchors = Vec::new();
    let mut doc = build(&tree, &mut anchors);
    attach_comments(text, &mut doc, &mut anchors);
    resolve_doc(&mut doc);
    Ok(doc)
}

/// [`parse_doc`] plus locations and element references for the issues, as the app hands it out.
pub fn parse_doc_outcome(text: &str) -> ParseOutcome {
    let locations = parse_document(text)
        .map(|tree| locate(&tree))
        .unwrap_or_default();
    let notes = map_issues(&score_notes(text), &locations);
    match parse_doc(text) {
        Ok(doc) => ParseOutcome {
            ok: true,
            score: compile_score(text).ok(),
            doc: Some(doc),
            locations,
            errors: Vec::new(),
            notes,
        },
        Err(error) => ParseOutcome {
            ok: false,
            doc: None,
            score: None,
            errors: map_issues(&error.issues, &locations),
            locations,
            notes,
        },
    }
}

/// Which element an anchor stands for, while comments are being sorted in.
#[derive(Clone, Debug, PartialEq)]
enum Anchor {
    Root(String),
    Track(usize),
    Midi(usize),
    Section(usize),
    SectionField(usize, String),
    SectionTrack(usize, usize),
    MixerField(String),
    MixerTrack(usize),
    MixerTrackField(usize, String),
}

fn number(node: &Node<'_>) -> Option<f64> {
    match node.kind {
        NodeKind::Int(i) => Some(i as f64),
        NodeKind::Float(f) => Some(f),
        _ => None,
    }
}

fn int(node: &Node<'_>) -> Option<i64> {
    match node.kind {
        NodeKind::Int(i) => Some(i),
        _ => None,
    }
}

fn text_of(node: &Node<'_>) -> Option<String> {
    node.as_str().map(str::to_string)
}

fn boolean(node: &Node<'_>) -> Option<bool> {
    match node.kind {
        NodeKind::Bool(b) => Some(b),
        _ => None,
    }
}

/// Walk a tree the reader has already accepted. Nothing here reports anything: shapes that do not
/// fit are skipped, which on a validated tree never happens.
fn build(tree: &Node<'_>, anchors: &mut Vec<(u32, Anchor)>) -> ScoreDoc {
    let mut doc = ScoreDoc {
        title: None,
        bpm: 0.0,
        time_signature: None,
        beats_per_bar: None,
        tracks: Vec::new(),
        midi: Vec::new(),
        sections: Vec::new(),
        mixer: None,
        header: Vec::new(),
        footer: Vec::new(),
        field_comments: OrderedMap::new(),
    };
    let NodeKind::Map(root) = &tree.kind else {
        return doc;
    };
    for entry in root {
        let Some(key) = entry.key.as_str() else { continue };
        anchors.push((entry.key.pos.line, Anchor::Root(key.to_string())));
        let value = &entry.value;
        match key {
            "title" => doc.title = text_of(value),
            "bpm" => doc.bpm = number(value).unwrap_or(0.0),
            "time_signature" => doc.time_signature = text_of(value),
            "beats_per_bar" => doc.beats_per_bar = int(value).map(|v| v as u32),
            "tracks" => {
                if let NodeKind::Map(entries) = &value.kind {
                    for track in entries {
                        let Some(name) = track.key.as_str() else { continue };
                        anchors.push((track.key.pos.line, Anchor::Track(doc.tracks.len())));
                        let mut t = DocTrack::new(name, 0);
                        t.line = Some(track.key.pos.line);
                        if let NodeKind::Map(fields) = &track.value.kind {
                            for field in fields {
                                let v = &field.value;
                                match field.key.as_str().unwrap_or("") {
                                    "input" => match &v.kind {
                                        NodeKind::Seq(items) if items.len() == 2 => {
                                            t.input = int(&items[0]).unwrap_or(0) as u32;
                                            t.input_right = int(&items[1]).map(|i| i as u32);
                                        }
                                        NodeKind::Str(_) => t.instrument = true,
                                        _ => t.input = int(v).unwrap_or(0) as u32,
                                    },
                                    "midi" => t.midi = Some(track_midi(v)),
                                    "loop" => t.loop_kind = text_of(v),
                                    "grid" => t.grid = text_of(v),
                                    "pan" => t.pan = number(v),
                                    "monitor" => t.monitor = boolean(v),
                                    "latency" => t.latency = int(v).map(|i| i as u32),
                                    "latency_trim" => t.latency_trim = int(v).map(|i| i as i32),
                                    "bus" => t.bus = text_of(v),
                                    "monitor_bus" => t.monitor_bus = text_of(v),
                                    _ => {}
                                }
                            }
                        }
                        doc.tracks.push(t);
                    }
                }
            }
            "midi" => {
                if let NodeKind::Map(entries) = &value.kind {
                    for binding in entries {
                        let Some(name) = binding.key.as_str() else { continue };
                        anchors.push((binding.key.pos.line, Anchor::Midi(doc.midi.len())));
                        let mut b = DocMidiBinding {
                            key: name.to_string(),
                            note: None,
                            cc: None,
                            channel: None,
                            mode: None,
                            takeover: None,
                            min: None,
                            max: None,
                            comments: Comments::default(),
                            line: Some(binding.key.pos.line),
                        };
                        if let NodeKind::Map(fields) = &binding.value.kind {
                            for field in fields {
                                let v = &field.value;
                                match field.key.as_str().unwrap_or("") {
                                    "note" => b.note = int(v).map(|i| i as u32),
                                    "cc" => b.cc = int(v).map(|i| i as u32),
                                    "channel" => b.channel = int(v).map(|i| i as u32),
                                    "mode" => b.mode = text_of(v),
                                    "takeover" => b.takeover = text_of(v),
                                    "min" => b.min = number(v),
                                    "max" => b.max = number(v),
                                    _ => {}
                                }
                            }
                        }
                        doc.midi.push(b);
                    }
                }
            }
            "sections" => {
                if let NodeKind::Seq(items) = &value.kind {
                    for item in items {
                        let NodeKind::Map(fields) = &item.kind else { continue };
                        let index = doc.sections.len();
                        anchors.push((item.pos.line, Anchor::Section(index)));
                        let mut s = DocSection::new("");
                        s.line = Some(item.pos.line);
                        for field in fields {
                            let name = field.key.as_str().unwrap_or("");
                            if name != "id" {
                                anchors.push((
                                    field.key.pos.line,
                                    Anchor::SectionField(index, name.to_string()),
                                ));
                            }
                            let v = &field.value;
                            match name {
                                "id" => s.id = text_of(v).unwrap_or_default(),
                                "repeat" => s.repeat = text_of(v),
                                "bars" => s.bars = int(v).map(|i| i as u32),
                                "autorelease" => s.autorelease = boolean(v),
                                "quantize" => {
                                    s.quantize = match v.as_str() {
                                        Some("bar") => Some(Quantize::Bar),
                                        Some("loop") => Some(Quantize::Loop),
                                        _ => None,
                                    }
                                }
                                "tracks" => {
                                    if let NodeKind::Map(states) = &v.kind {
                                        for state in states {
                                            // `voice: record` or `keys: {state: play, grid: 1/16}`.
                                            let (word, grid) = match &state.value.kind {
                                                NodeKind::Map(fields) => {
                                                    let get = |k: &str| {
                                                        fields
                                                            .iter()
                                                            .find(|f| f.key.as_str() == Some(k))
                                                            .and_then(|f| f.value.as_str())
                                                    };
                                                    (get("state"), get("grid").map(str::to_string))
                                                }
                                                _ => (state.value.as_str(), None),
                                            };
                                            let (Some(track), Some(word)) = (state.key.as_str(), word)
                                            else {
                                                continue;
                                            };
                                            let Some(parsed) = TrackState::parse(word) else {
                                                continue;
                                            };
                                            anchors.push((
                                                state.key.pos.line,
                                                Anchor::SectionTrack(index, s.tracks.len()),
                                            ));
                                            s.tracks.push(DocSectionTrack {
                                                track: track.to_string(),
                                                state: parsed,
                                                grid,
                                                comments: Comments::default(),
                                                line: Some(state.key.pos.line),
                                            });
                                        }
                                    }
                                }
                                _ => {}
                            }
                        }
                        doc.sections.push(s);
                    }
                }
            }
            "mixer" => doc.mixer = Some(build_mixer(value, anchors)),
            _ => {}
        }
    }
    doc
}

/// `midi: {device, channel, notes}` of a track, from a tree the reader has already accepted.
fn track_midi(node: &Node<'_>) -> InstrumentSource {
    let mut out = InstrumentSource::default();
    let NodeKind::Map(fields) = &node.kind else {
        return out;
    };
    for field in fields {
        let v = &field.value;
        match field.key.as_str().unwrap_or("") {
            "device" => out.device = text_of(v),
            "channel" => out.channel = int(v).map(|c| c as u8),
            "notes" => {
                if let Some((low, high)) = v.as_str().and_then(|t| t.split_once('-')) {
                    out.note_low = low.trim().parse().ok();
                    out.note_high = high.trim().parse().ok();
                }
            }
            _ => {}
        }
    }
    out
}

fn build_mixer(node: &Node<'_>, anchors: &mut Vec<(u32, Anchor)>) -> DocMixer {
    let mut mixer = DocMixer::default();
    let NodeKind::Map(fields) = &node.kind else {
        return mixer;
    };
    for field in fields {
        let Some(key) = field.key.as_str() else { continue };
        anchors.push((field.key.pos.line, Anchor::MixerField(key.to_string())));
        match (key, &field.value.kind) {
            ("buses", NodeKind::Map(levels)) => {
                let mut buses = BusLevels::default();
                for level in levels {
                    let value = number(&level.value);
                    match level.key.as_str().unwrap_or("") {
                        "main" => buses.main = value,
                        "monitor" => buses.monitor = value,
                        "click" => buses.click = value,
                        "inputs" => buses.inputs = value,
                        _ => {}
                    }
                }
                mixer.buses = Some(buses);
            }
            ("tracks", NodeKind::Map(entries)) => {
                for entry in entries {
                    let Some(name) = entry.key.as_str() else { continue };
                    let index = mixer.tracks.len();
                    anchors.push((entry.key.pos.line, Anchor::MixerTrack(index)));
                    let mut t = DocMixerTrack::new(name, TrackMixSource::default());
                    t.line = Some(entry.key.pos.line);
                    if let NodeKind::Map(items) = &entry.value.kind {
                        for item in items {
                            let Some(field) = item.key.as_str() else { continue };
                            anchors.push((
                                item.key.pos.line,
                                Anchor::MixerTrackField(index, field.to_string()),
                            ));
                            read_mix_field(&mut t.mix, field, &item.value);
                        }
                    }
                    mixer.tracks.push(t);
                }
            }
            _ => {}
        }
    }
    mixer
}

/// One field of a mixer entry, from a tree the reader has already accepted.
fn read_mix_field(mix: &mut TrackMixSource, field: &str, value: &Node<'_>) {
    let pairs: Vec<(&str, &Node<'_>)> = match &value.kind {
        NodeKind::Map(entries) => entries
            .iter()
            .filter_map(|e| e.key.as_str().map(|k| (k, &e.value)))
            .collect(),
        _ => Vec::new(),
    };
    let get = |name: &str| pairs.iter().find(|(k, _)| *k == name).map(|(_, v)| *v);
    let num = |name: &str| get(name).and_then(number);
    let on = || get("on").and_then(boolean);
    let word = |name: &str| get(name).and_then(text_of);
    match field {
        "gain" => mix.gain = number(value),
        "mute" => mix.mute = boolean(value),
        "preset" => mix.preset = text_of(value),
        "bypass" => mix.bypass = boolean(value),
        "instrument" => mix.instrument = text_of(value),
        "inserts" => {
            if let NodeKind::Seq(items) = &value.kind {
                mix.inserts = Some(
                    items
                        .iter()
                        .filter_map(|item| {
                            let NodeKind::Map(fields) = &item.kind else { return None };
                            let get = |k: &str| {
                                fields.iter().find(|f| f.key.as_str() == Some(k)).map(|f| &f.value)
                            };
                            Some(InsertLine {
                                plugin: get("plugin").and_then(text_of)?,
                                bypass: get("bypass").and_then(boolean),
                            })
                        })
                        .collect(),
                );
            }
        }
        "high_pass" => {
            mix.high_pass = Some(HighPassLine {
                on: on(),
                hz: num("hz"),
            })
        }
        "eq" => mix.eq = Some(SwitchLine { on: on() }),
        "band1" | "band2" | "band3" => {
            let line = Some(BandLine {
                kind: word("kind"),
                hz: num("hz"),
                q: num("q"),
                gain: num("gain"),
            });
            match field {
                "band1" => mix.band1 = line,
                "band2" => mix.band2 = line,
                _ => mix.band3 = line,
            }
        }
        "comp" => {
            mix.comp = Some(CompLine {
                on: on(),
                threshold: num("threshold"),
                ratio: num("ratio"),
                attack: num("attack"),
                release: num("release"),
                knee: num("knee"),
                makeup: num("makeup"),
            })
        }
        "delay" => {
            mix.delay = Some(DelayLine {
                on: on(),
                note: word("note"),
                feedback: num("feedback"),
                mix: num("mix"),
            })
        }
        "reverb" => {
            mix.reverb = Some(ReverbLine {
                on: on(),
                size: num("size"),
                damping: num("damping"),
                mix: num("mix"),
            })
        }
        _ => {}
    }
}

/// One comment found in the text.
struct FoundComment {
    line: u32,
    text: String,
    /// True when code stands in front of it on the same line.
    inline: bool,
}

/// Byte index where a comment starts on this line, if any. A `#` starts a comment at the start of
/// the line or after whitespace, and never inside a quoted scalar.
fn comment_start(line: &str) -> Option<usize> {
    let bytes = line.as_bytes();
    let mut quote: Option<u8> = None;
    let mut prev_significant: Option<u8> = None;
    let mut i = 0;
    while i < bytes.len() {
        let c = bytes[i];
        match quote {
            Some(b'\'') => {
                if c == b'\'' {
                    if bytes.get(i + 1) == Some(&b'\'') {
                        i += 1;
                    } else {
                        quote = None;
                    }
                }
            }
            Some(_) => {
                if c == b'\\' {
                    i += 1;
                } else if c == b'"' {
                    quote = None;
                }
            }
            None => {
                if c == b'#' && (i == 0 || bytes[i - 1] == b' ' || bytes[i - 1] == b'\t') {
                    return Some(i);
                }
                // A quote opens a quoted scalar only where a scalar can begin.
                if (c == b'\'' || c == b'"')
                    && matches!(prev_significant, None | Some(b':' | b'-' | b'[' | b'{' | b','))
                {
                    quote = Some(c);
                }
                if c != b' ' && c != b'\t' {
                    prev_significant = Some(c);
                }
            }
        }
        i += 1;
    }
    None
}

fn find_comments(text: &str) -> Vec<FoundComment> {
    let mut found = Vec::new();
    for (n, line) in text.lines().enumerate() {
        let Some(start) = comment_start(line) else { continue };
        let raw = &line[start + 1..];
        let body = raw.strip_prefix(' ').unwrap_or(raw).trim_end().to_string();
        found.push(FoundComment {
            line: n as u32 + 1,
            text: body,
            inline: !line[..start].trim().is_empty(),
        });
    }
    found
}

fn comments_of<'a>(doc: &'a mut ScoreDoc, anchor: &Anchor) -> Option<&'a mut Comments> {
    fn field<'b>(map: &'b mut OrderedMap<Comments>, name: &str) -> &'b mut Comments {
        if !map.contains_key(name) {
            map.insert(name, Comments::default());
        }
        map.get_mut(name).expect("eben eingefuegt")
    }
    match anchor {
        Anchor::Root(name) => Some(field(&mut doc.field_comments, name)),
        Anchor::Track(i) => doc.tracks.get_mut(*i).map(|t| &mut t.comments),
        Anchor::Midi(i) => doc.midi.get_mut(*i).map(|b| &mut b.comments),
        Anchor::Section(i) => doc.sections.get_mut(*i).map(|s| &mut s.comments),
        Anchor::SectionField(i, name) => doc
            .sections
            .get_mut(*i)
            .map(|s| field(&mut s.field_comments, name)),
        Anchor::SectionTrack(i, j) => doc
            .sections
            .get_mut(*i)
            .and_then(|s| s.tracks.get_mut(*j))
            .map(|t| &mut t.comments),
        Anchor::MixerField(name) => doc
            .mixer
            .as_mut()
            .map(|m| field(&mut m.field_comments, name)),
        Anchor::MixerTrack(i) => doc
            .mixer
            .as_mut()
            .and_then(|m| m.tracks.get_mut(*i))
            .map(|t| &mut t.comments),
        Anchor::MixerTrackField(i, name) => doc
            .mixer
            .as_mut()
            .and_then(|m| m.tracks.get_mut(*i))
            .map(|t| field(&mut t.field_comments, name)),
    }
}

/// Sort every comment of the text onto an element. See [`render_doc`] for the rule.
fn attach_comments(text: &str, doc: &mut ScoreDoc, anchors: &mut [(u32, Anchor)]) {
    // Stable: on one line the outer element was registered first and wins.
    anchors.sort_by_key(|(line, _)| *line);
    let first = anchors.first().map(|(line, _)| *line).unwrap_or(u32::MAX);
    for comment in find_comments(text) {
        if comment.inline {
            let owner = anchors
                .iter()
                .find(|(line, _)| *line == comment.line)
                .or_else(|| anchors.iter().rev().find(|(line, _)| *line < comment.line));
            match owner.and_then(|(_, anchor)| comments_of(doc, anchor)) {
                Some(slot) => slot.add_after(comment.text),
                None => doc.header.push(comment.text),
            }
        } else if comment.line < first {
            doc.header.push(comment.text);
        } else {
            match anchors
                .iter()
                .find(|(line, _)| *line > comment.line)
                .and_then(|(_, anchor)| comments_of(doc, anchor))
            {
                Some(slot) => slot.before.push(comment.text),
                None => doc.footer.push(comment.text),
            }
        }
    }
}

/// Every key's position as an element reference. Lenient: works on any tree that parses, valid or
/// not, which is what lets an issue in an invalid text still name its element.
pub fn locate(tree: &Node<'_>) -> Vec<Location> {
    let mut out = Vec::new();
    let at = |at: ScoreRef, pos: Pos| Location {
        at,
        line: pos.line,
        column: pos.column,
    };
    let NodeKind::Map(root) = &tree.kind else {
        return out;
    };
    for entry in root {
        let Some(key) = entry.key.as_str() else { continue };
        out.push(at(
            ScoreRef::Score {
                field: Some(key.to_string()),
            },
            entry.key.pos,
        ));
        match (key, &entry.value.kind) {
            ("tracks", NodeKind::Map(tracks)) => {
                for (index, track) in tracks.iter().enumerate() {
                    let name = track.key.as_str().unwrap_or("").to_string();
                    out.push(at(
                        ScoreRef::Track {
                            index,
                            name: name.clone(),
                            field: None,
                        },
                        track.key.pos,
                    ));
                    if let NodeKind::Map(fields) = &track.value.kind {
                        for field in fields {
                            out.push(at(
                                ScoreRef::Track {
                                    index,
                                    name: name.clone(),
                                    field: field.key.as_str().map(str::to_string),
                                },
                                field.key.pos,
                            ));
                        }
                    }
                }
            }
            ("midi", NodeKind::Map(bindings)) => {
                for (index, binding) in bindings.iter().enumerate() {
                    let key = binding.key.as_str().unwrap_or("").to_string();
                    out.push(at(
                        ScoreRef::Midi {
                            index,
                            key: key.clone(),
                            field: None,
                        },
                        binding.key.pos,
                    ));
                    if let NodeKind::Map(fields) = &binding.value.kind {
                        for field in fields {
                            out.push(at(
                                ScoreRef::Midi {
                                    index,
                                    key: key.clone(),
                                    field: field.key.as_str().map(str::to_string),
                                },
                                field.key.pos,
                            ));
                        }
                    }
                }
            }
            ("sections", NodeKind::Seq(items)) => {
                for (index, item) in items.iter().enumerate() {
                    let NodeKind::Map(fields) = &item.kind else {
                        out.push(at(
                            ScoreRef::Section {
                                index,
                                id: String::new(),
                                field: None,
                            },
                            item.pos,
                        ));
                        continue;
                    };
                    let id = fields
                        .iter()
                        .find(|f| f.key.as_str() == Some("id"))
                        .and_then(|f| f.value.as_str())
                        .unwrap_or("")
                        .to_string();
                    // The item itself one column left of its first key, i.e. on the `-`, so a key
                    // on the same line is still the more specific hit.
                    out.push(at(
                        ScoreRef::Section {
                            index,
                            id: id.clone(),
                            field: None,
                        },
                        Pos {
                            line: item.pos.line,
                            column: item.pos.column.saturating_sub(2).max(1),
                        },
                    ));
                    for field in fields {
                        let name = field.key.as_str().map(str::to_string);
                        out.push(at(
                            ScoreRef::Section {
                                index,
                                id: id.clone(),
                                field: name.clone(),
                            },
                            field.key.pos,
                        ));
                        if name.as_deref() == Some("tracks")
                            && let NodeKind::Map(states) = &field.value.kind
                        {
                            for (j, state) in states.iter().enumerate() {
                                out.push(at(
                                    ScoreRef::SectionTrack {
                                        section: index,
                                        section_id: id.clone(),
                                        index: j,
                                        track: state.key.as_str().unwrap_or("").to_string(),
                                    },
                                    state.key.pos,
                                ));
                            }
                        }
                    }
                }
            }
            ("mixer", NodeKind::Map(fields)) => {
                for field in fields {
                    let name = field.key.as_str().map(str::to_string);
                    out.push(at(ScoreRef::Mixer { field: name.clone() }, field.key.pos));
                    if name.as_deref() == Some("tracks")
                        && let NodeKind::Map(entries) = &field.value.kind
                    {
                        for (index, entry) in entries.iter().enumerate() {
                            let track = entry.key.as_str().unwrap_or("").to_string();
                            out.push(at(
                                ScoreRef::MixerTrack {
                                    index,
                                    track: track.clone(),
                                    field: None,
                                },
                                entry.key.pos,
                            ));
                            if let NodeKind::Map(items) = &entry.value.kind {
                                for item in items {
                                    out.push(at(
                                        ScoreRef::MixerTrack {
                                            index,
                                            track: track.clone(),
                                            field: item.key.as_str().map(str::to_string),
                                        },
                                        item.key.pos,
                                    ));
                                }
                            }
                        }
                    }
                }
            }
            _ => {}
        }
    }
    out
}

/// The element an issue belongs to: the rightmost key on its line at or before its column, else
/// the last key above it.
fn element_at(line: u32, column: u32, locations: &[Location]) -> Option<ScoreRef> {
    let mut best: Option<&Location> = None;
    for location in locations {
        if location.line == line && location.column <= column {
            if best.is_none_or(|b| b.line != line || location.column >= b.column) {
                best = Some(location);
            }
        } else if location.line < line && best.is_none_or(|b| b.line < line && location.line >= b.line) {
            best = Some(location);
        }
    }
    best.map(|l| l.at.clone())
}

/// Attach element references to compiler issues.
///
/// A handful of messages are positioned on a whole block (the compiler points at the node, which
/// for a block mapping is its first key); those are corrected here by what they say, so "too many
/// tracks" names the track list rather than its first track.
pub fn map_issues(issues: &[ScoreIssue], locations: &[Location]) -> Vec<ModelIssue> {
    issues
        .iter()
        .map(|issue| {
            let mut at = match (issue.line, issue.column) {
                (Some(line), column) => element_at(line, column.unwrap_or(u32::MAX), locations),
                _ => None,
            };
            if issue.message.contains("tracks; the engine handles at most") {
                at = Some(ScoreRef::Score {
                    field: Some("tracks".to_string()),
                });
            }
            if issue.message.contains("neither 'bars' nor 'repeat'")
                && let Some(ScoreRef::Section { field, .. }) = at.as_mut()
            {
                *field = Some("bars".to_string());
            }
            if at.is_none() {
                at = Some(ScoreRef::Score { field: None });
            }
            ModelIssue {
                line: issue.line,
                column: issue.column,
                message: issue.message.clone(),
                suggestion: issue.suggestion.clone(),
                at,
            }
        })
        .collect()
}

// ---------------------------------------------------------------------------------------------
// Inheritance
// ---------------------------------------------------------------------------------------------

/// Fill every section's `resolved` from the document itself. Lenient on purpose - an unknown or
/// circular `repeat:` simply ends the chain - so it also works on a document the UI has just made
/// invalid, where it shows what the section would be if the reference were fixed.
pub fn resolve_doc(doc: &mut ScoreDoc) {
    let resolved: Vec<ResolvedSection> = (0..doc.sections.len())
        .map(|index| resolve_section(doc, index))
        .collect();
    for (section, value) in doc.sections.iter_mut().zip(resolved) {
        section.resolved = Some(value);
    }
}

fn resolve_section(doc: &ScoreDoc, index: usize) -> ResolvedSection {
    // The chain this section inherits through, itself first.
    let mut chain = vec![index];
    let mut current = index;
    while let Some(target) = doc.sections[current].repeat.as_deref() {
        match doc.sections.iter().position(|s| s.id == target) {
            Some(next) if !chain.contains(&next) => {
                chain.push(next);
                current = next;
            }
            _ => break,
        }
    }
    let origin = |at: usize| {
        if at == index {
            ValueSource::Own
        } else {
            ValueSource::Repeat {
                index: at,
                section: doc.sections[at].id.clone(),
            }
        }
    };
    fn pick<T: Copy>(
        chain: &[usize],
        get: impl Fn(usize) -> Option<T>,
        origin: &dyn Fn(usize) -> ValueSource,
        default: T,
    ) -> Resolved<T> {
        chain
            .iter()
            .find_map(|&at| get(at).map(|value| Resolved { value, from: origin(at) }))
            .unwrap_or(Resolved {
                value: default,
                from: ValueSource::Default,
            })
    }
    let s = &doc.sections;
    ResolvedSection {
        bars: pick(&chain, |at| s[at].bars, &origin, super::compile::DEFAULT_BARS),
        autorelease: pick(
            &chain,
            |at| s[at].autorelease,
            &origin,
            super::compile::DEFAULT_AUTORELEASE,
        ),
        quantize: pick(&chain, |at| s[at].quantize, &origin, Quantize::default()),
        tracks: doc
            .tracks
            .iter()
            .map(|track| {
                let hit = chain.iter().find_map(|&at| {
                    s[at]
                        .tracks
                        .iter()
                        .find(|t| t.track == track.name)
                        .map(|t| (t.state, origin(at)))
                });
                let (state, from) = hit.unwrap_or((TrackState::Stop, ValueSource::Default));
                ResolvedTrackState {
                    track: track.name.clone(),
                    state,
                    from,
                }
            })
            .collect(),
    }
}

// ---------------------------------------------------------------------------------------------
// Document -> text
// ---------------------------------------------------------------------------------------------

/// Render a document as canonical YAML.
///
/// # The rule for comments and formatting
///
/// **Content and comments are kept, layout is canonical.**
///
/// * Every comment belongs to one element: a comment at the end of a line to the element whose
///   key stands on that line (the outermost one, e.g. the section on `- id: x`), whole comment
///   lines to the element directly below them, comments above the first key to the file header,
///   comments after the last element to the footer. They are written back in the same place
///   relative to their element - so they move with it when sections are reordered, and they go
///   when their element is deleted.
/// * The layout is fixed: two spaces of indentation, keys in a fixed order (`title`, `bpm`,
///   `time_signature`, `beats_per_bar`, `tracks`, `midi`, `sections`; inside a section `id`,
///   `repeat`, `bars`, `autorelease`, `quantize`, `tracks`), tracks and MIDI bindings as one
///   `{...}` line each, sections as blocks, one blank line between top-level blocks and between
///   sections, strings quoted only where YAML needs it, numbers in shortest form.
/// * What is written stays written: an explicit default (`monitor: true`) is kept, an omitted
///   field stays omitted, `beats_per_bar` stays when the file used it.
///
/// Text -> document -> text therefore changes nothing a compiler sees (proved by the tests against
/// every example score) and is idempotent: a second round trip returns the identical text.
pub fn render_doc(doc: &ScoreDoc) -> String {
    let mut out = String::new();
    let mut w = Writer { out: &mut out };

    for line in &doc.header {
        w.comment(0, line);
    }
    if !doc.header.is_empty() {
        w.blank();
    }

    let root = |name: &str| doc.field_comments.get(name).cloned().unwrap_or_default();

    if let Some(title) = &doc.title {
        w.line(0, &format!("title: {}", scalar(title)), &root("title"));
    }
    w.line(0, &format!("bpm: {}", num(doc.bpm)), &root("bpm"));
    if let Some(sig) = &doc.time_signature {
        w.line(0, &format!("time_signature: {}", scalar(sig)), &root("time_signature"));
    }
    if let Some(beats) = doc.beats_per_bar {
        w.line(0, &format!("beats_per_bar: {beats}"), &root("beats_per_bar"));
    }

    w.blank();
    if doc.tracks.is_empty() {
        w.line(0, "tracks: {}", &root("tracks"));
    } else {
        w.line(0, "tracks:", &root("tracks"));
        for track in &doc.tracks {
            w.line(1, &format!("{}: {}", scalar(&track.name), track_flow(track)), &track.comments);
        }
    }

    if !doc.midi.is_empty() || !root("midi").is_empty() {
        w.blank();
        if doc.midi.is_empty() {
            w.line(0, "midi: {}", &root("midi"));
        } else {
            w.line(0, "midi:", &root("midi"));
            for binding in &doc.midi {
                w.line(1, &format!("{}: {}", scalar(&binding.key), midi_flow(binding)), &binding.comments);
            }
        }
    }

    w.blank();
    if doc.sections.is_empty() {
        w.line(0, "sections: []", &root("sections"));
    } else {
        w.line(0, "sections:", &root("sections"));
        for (n, section) in doc.sections.iter().enumerate() {
            if n > 0 {
                w.blank();
            }
            let field = |name: &str| section.field_comments.get(name).cloned().unwrap_or_default();
            w.line(1, &format!("- id: {}", scalar(&section.id)), &section.comments);
            // The fields hang two columns right of the dash.
            if let Some(repeat) = &section.repeat {
                w.line(2, &format!("repeat: {}", scalar(repeat)), &field("repeat"));
            }
            if let Some(bars) = section.bars {
                w.line(2, &format!("bars: {bars}"), &field("bars"));
            }
            if let Some(autorelease) = section.autorelease {
                w.line(2, &format!("autorelease: {autorelease}"), &field("autorelease"));
            }
            if let Some(quantize) = section.quantize {
                let word = match quantize {
                    Quantize::Bar => "bar",
                    Quantize::Loop => "loop",
                };
                w.line(2, &format!("quantize: {word}"), &field("quantize"));
            }
            if !section.tracks.is_empty() {
                w.line(2, "tracks:", &field("tracks"));
                for state in &section.tracks {
                    let value = match &state.grid {
                        Some(grid) => format!(
                            "{{state: {}, grid: {}}}",
                            state.state.as_str(),
                            flow_scalar(grid)
                        ),
                        None => state.state.as_str().to_string(),
                    };
                    w.line(3, &format!("{}: {value}", scalar(&state.track)), &state.comments);
                }
            } else if !field("tracks").is_empty() {
                w.line(2, "tracks: {}", &field("tracks"));
            }
        }
    }

    if let Some(mixer) = &doc.mixer {
        w.blank();
        render_mixer(&mut w, mixer, &root("mixer"));
    }

    if !doc.footer.is_empty() {
        w.blank();
        for line in &doc.footer {
            w.comment(0, line);
        }
    }
    out
}

/// Render, compile and map every issue to its element - the one command a graphical editor needs.
pub fn render_doc_outcome(doc: &ScoreDoc) -> RenderOutcome {
    let yaml = render_doc(doc);
    let locations = parse_document(&yaml)
        .map(|tree| locate(&tree))
        .unwrap_or_default();
    let notes = map_issues(&score_notes(&yaml), &locations);
    match compile_score(&yaml) {
        Ok(score) => {
            let refreshed = parse_doc(&yaml).unwrap_or_else(|_| {
                let mut copy = doc.clone();
                resolve_doc(&mut copy);
                copy
            });
            RenderOutcome {
                ok: true,
                yaml,
                doc: refreshed,
                score: Some(score),
                locations,
                errors: Vec::new(),
                notes,
            }
        }
        Err(error) => {
            let mut copy = doc.clone();
            resolve_doc(&mut copy);
            RenderOutcome {
                ok: false,
                errors: map_issues(&error.issues, &locations),
                yaml,
                doc: copy,
                score: None,
                locations,
                notes,
            }
        }
    }
}

struct Writer<'a> {
    out: &'a mut String,
}

impl Writer<'_> {
    fn indent(&mut self, level: usize) {
        for _ in 0..level {
            self.out.push_str("  ");
        }
    }

    fn comment(&mut self, level: usize, text: &str) {
        self.indent(level);
        if text.is_empty() {
            self.out.push_str("#\n");
        } else {
            self.out.push_str("# ");
            self.out.push_str(text);
            self.out.push('\n');
        }
    }

    fn blank(&mut self) {
        if !self.out.is_empty() && !self.out.ends_with("\n\n") {
            self.out.push('\n');
        }
    }

    fn line(&mut self, level: usize, content: &str, comments: &Comments) {
        for text in &comments.before {
            self.comment(level, text);
        }
        self.indent(level);
        self.out.push_str(content);
        if let Some(after) = &comments.after {
            self.out.push_str("  #");
            if !after.is_empty() {
                self.out.push(' ');
                self.out.push_str(after);
            }
        }
        self.out.push('\n');
    }
}

fn render_mixer(w: &mut Writer<'_>, mixer: &DocMixer, comments: &Comments) {
    let field = |name: &str| mixer.field_comments.get(name).cloned().unwrap_or_default();
    let has_tracks = !mixer.tracks.is_empty() || !field("tracks").is_empty();
    if mixer.buses.is_none() && !has_tracks {
        w.line(0, "mixer: {}", comments);
        return;
    }
    w.line(0, "mixer:", comments);
    if let Some(buses) = &mixer.buses {
        let mut parts = Vec::new();
        for (name, value) in [
            ("main", buses.main),
            ("monitor", buses.monitor),
            ("click", buses.click),
            ("inputs", buses.inputs),
        ] {
            if let Some(v) = value {
                parts.push(format!("{name}: {}", num(v)));
            }
        }
        w.line(1, &format!("buses: {{{}}}", parts.join(", ")), &field("buses"));
    }
    if mixer.tracks.is_empty() {
        if has_tracks {
            w.line(1, "tracks: {}", &field("tracks"));
        }
        return;
    }
    w.line(1, "tracks:", &field("tracks"));
    for entry in &mixer.tracks {
        let key = scalar(&entry.track);
        let lines = mix_lines(&entry.mix);
        let simple = !entry.mix.has_effect_lines()
            && !entry.mix.has_plugins()
            && entry.field_comments.is_empty();
        if simple {
            let parts: Vec<String> = lines.iter().map(|(k, v)| format!("{k}: {v}")).collect();
            w.line(2, &format!("{key}: {{{}}}", parts.join(", ")), &entry.comments);
        } else {
            w.line(2, &format!("{key}:"), &entry.comments);
            for (name, value) in &lines {
                let comments = entry.field_comments.get(name).cloned().unwrap_or_default();
                w.line(3, &format!("{name}: {value}"), &comments);
            }
        }
    }
}

/// The fields of one mixer entry in their fixed order, each as `(key, value text)`.
fn mix_lines(mix: &TrackMixSource) -> Vec<(&'static str, String)> {
    fn flow(parts: Vec<(&str, Option<String>)>) -> String {
        let written: Vec<String> = parts
            .into_iter()
            .filter_map(|(k, v)| v.map(|v| format!("{k}: {v}")))
            .collect();
        format!("{{{}}}", written.join(", "))
    }
    let n = |v: Option<f64>| v.map(num);
    let b = |v: Option<bool>| v.map(|v| v.to_string());
    let t = |v: &Option<String>| v.as_deref().map(flow_scalar);
    let mut out = Vec::new();
    if let Some(gain) = mix.gain {
        out.push(("gain", num(gain)));
    }
    if let Some(mute) = mix.mute {
        out.push(("mute", mute.to_string()));
    }
    if let Some(instrument) = &mix.instrument {
        out.push(("instrument", flow_scalar(instrument)));
    }
    if let Some(preset) = &mix.preset {
        out.push(("preset", flow_scalar(preset)));
    }
    if let Some(bypass) = mix.bypass {
        out.push(("bypass", bypass.to_string()));
    }
    if let Some(l) = &mix.high_pass {
        out.push(("high_pass", flow(vec![("on", b(l.on)), ("hz", n(l.hz))])));
    }
    if let Some(l) = &mix.eq {
        out.push(("eq", flow(vec![("on", b(l.on))])));
    }
    for (name, band) in [("band1", &mix.band1), ("band2", &mix.band2), ("band3", &mix.band3)] {
        if let Some(l) = band {
            out.push((
                name,
                flow(vec![("kind", t(&l.kind)), ("hz", n(l.hz)), ("q", n(l.q)), ("gain", n(l.gain))]),
            ));
        }
    }
    if let Some(l) = &mix.comp {
        out.push((
            "comp",
            flow(vec![
                ("on", b(l.on)),
                ("threshold", n(l.threshold)),
                ("ratio", n(l.ratio)),
                ("attack", n(l.attack)),
                ("release", n(l.release)),
                ("knee", n(l.knee)),
                ("makeup", n(l.makeup)),
            ]),
        ));
    }
    if let Some(l) = &mix.delay {
        out.push((
            "delay",
            flow(vec![
                ("on", b(l.on)),
                ("note", t(&l.note)),
                ("feedback", n(l.feedback)),
                ("mix", n(l.mix)),
            ]),
        ));
    }
    if let Some(l) = &mix.reverb {
        out.push((
            "reverb",
            flow(vec![
                ("on", b(l.on)),
                ("size", n(l.size)),
                ("damping", n(l.damping)),
                ("mix", n(l.mix)),
            ]),
        ));
    }
    // Last: the plugins sit after the built-in chain in the signal path.
    if let Some(inserts) = &mix.inserts {
        let items: Vec<String> = inserts
            .iter()
            .map(|i| {
                flow(vec![
                    ("plugin", Some(flow_scalar(&i.plugin))),
                    ("bypass", b(i.bypass)),
                ])
            })
            .collect();
        out.push(("inserts", format!("[{}]", items.join(", "))));
    }
    out
}

fn track_flow(track: &DocTrack) -> String {
    let mut fields = Vec::new();
    match track.input_right {
        _ if track.instrument => fields.push("input: instrument".to_string()),
        Some(right) => fields.push(format!("input: [{}, {right}]", track.input)),
        None => fields.push(format!("input: {}", track.input)),
    }
    if let Some(midi) = &track.midi {
        let mut parts = Vec::new();
        if let Some(device) = &midi.device {
            parts.push(format!("device: {}", flow_scalar(device)));
        }
        if let Some(channel) = midi.channel {
            parts.push(format!("channel: {channel}"));
        }
        if let (Some(low), Some(high)) = (midi.note_low, midi.note_high) {
            parts.push(format!("notes: {low}-{high}"));
        }
        fields.push(format!("midi: {{{}}}", parts.join(", ")));
    }
    if let Some(pan) = track.pan {
        fields.push(format!("pan: {}", num(pan)));
    }
    if let Some(monitor) = track.monitor {
        fields.push(format!("monitor: {monitor}"));
    }
    if let Some(latency) = track.latency {
        fields.push(format!("latency: {latency}"));
    }
    if let Some(trim) = track.latency_trim {
        fields.push(format!("latency_trim: {trim}"));
    }
    if let Some(bus) = &track.bus {
        fields.push(format!("bus: {}", flow_scalar(bus)));
    }
    if let Some(bus) = &track.monitor_bus {
        fields.push(format!("monitor_bus: {}", flow_scalar(bus)));
    }
    if let Some(kind) = &track.loop_kind {
        fields.push(format!("loop: {}", flow_scalar(kind)));
    }
    if let Some(grid) = &track.grid {
        fields.push(format!("grid: {}", flow_scalar(grid)));
    }
    format!("{{{}}}", fields.join(", "))
}

fn midi_flow(binding: &DocMidiBinding) -> String {
    let mut fields = Vec::new();
    if let Some(note) = binding.note {
        fields.push(format!("note: {note}"));
    }
    if let Some(cc) = binding.cc {
        fields.push(format!("cc: {cc}"));
    }
    if let Some(channel) = binding.channel {
        fields.push(format!("channel: {channel}"));
    }
    if let Some(mode) = &binding.mode {
        fields.push(format!("mode: {}", flow_scalar(mode)));
    }
    if let Some(takeover) = &binding.takeover {
        fields.push(format!("takeover: {}", flow_scalar(takeover)));
    }
    if let Some(min) = binding.min {
        fields.push(format!("min: {}", num(min)));
    }
    if let Some(max) = binding.max {
        fields.push(format!("max: {}", num(max)));
    }
    format!("{{{}}}", fields.join(", "))
}

/// Shortest form that reads back as the same number: `141`, `141.5`, `-0.4`.
fn num(value: f64) -> String {
    if value.is_finite() {
        format!("{value}")
    } else {
        // Not reachable from JSON; written so the compiler, not a panic, has the last word.
        ".nan".to_string()
    }
}

/// A string as a YAML scalar: plain where that reads back as the same string, double-quoted
/// otherwise.
fn scalar(text: &str) -> String {
    if plain_ok(text, false) {
        text.to_string()
    } else {
        quoted(text)
    }
}

/// The same inside `{...}`, where `,`, `[]` and `{}` end a plain scalar too.
fn flow_scalar(text: &str) -> String {
    if plain_ok(text, true) {
        text.to_string()
    } else {
        quoted(text)
    }
}

fn plain_ok(text: &str, flow: bool) -> bool {
    if text.is_empty() || text.trim() != text {
        return false;
    }
    let first = text.chars().next().unwrap_or(' ');
    if "-?:,[]{}#&*!|>'\"%@`".contains(first) {
        // `-x` is fine as a plain scalar, but a lone `-` or `- x` is not; keep it simple.
        return false;
    }
    if text.contains(": ") || text.contains(" #") || text.ends_with(':') || text.contains('\n') {
        return false;
    }
    if flow && text.contains([',', '[', ']', '{', '}']) {
        return false;
    }
    // Anything the core schema would read as something other than a string.
    let probe = format!("k: {text}\n");
    let reread = super::yaml::parse_document(&probe);
    match reread {
        Ok(Node {
            kind: NodeKind::Map(entries),
            ..
        }) => entries
            .first()
            .and_then(|entry| entry.value.as_str())
            .is_some_and(|s| s == text),
        _ => false,
    }
}

fn quoted(text: &str) -> String {
    let mut out = String::with_capacity(text.len() + 2);
    out.push('"');
    for c in text.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\t' => out.push_str("\\t"),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

// ---------------------------------------------------------------------------------------------
// A new score
// ---------------------------------------------------------------------------------------------

/// The template behind "New": the given tracks (name, 1-based input, optional right input), or a
/// voice and a guitar when there are none, and three sections that show the three things a score
/// does - record a loop, stack a layer, play it out.
pub fn template(tracks: &[(String, u32, Option<u32>)]) -> String {
    template_with(
        tracks
            .iter()
            .map(|(name, input, right)| {
                let mut t = DocTrack::new(name.clone(), *input);
                t.input_right = *right;
                t
            })
            .collect(),
        Vec::new(),
    )
}

/// The same with tracks as the document has them - instrument tracks included - and, for those,
/// the instrument plugin each one plays (track name, plugin id), which goes into `mixer:`.
pub fn template_with(tracks: Vec<DocTrack>, instruments: Vec<(String, String)>) -> String {
    let tracks: Vec<DocTrack> = if tracks.is_empty() {
        vec![DocTrack::new("voice", 1), DocTrack::new("guitar", 2)]
    } else {
        tracks
    };
    let mixer = (!instruments.is_empty()).then(|| DocMixer {
        buses: None,
        tracks: tracks
            .iter()
            .filter_map(|t| {
                let (_, id) = instruments.iter().find(|(name, _)| *name == t.name)?;
                Some(DocMixerTrack::new(
                    t.name.clone(),
                    TrackMixSource {
                        instrument: Some(id.clone()),
                        ..TrackMixSource::default()
                    },
                ))
            })
            .collect(),
        field_comments: OrderedMap::new(),
    });
    let first = tracks[0].name.clone();
    let state = |track: &str, state: TrackState| DocSectionTrack {
        track: track.to_string(),
        state,
        grid: None,
        comments: Comments::default(),
        line: None,
    };
    let mut intro = DocSection::new("intro");
    intro.bars = Some(4);
    intro.autorelease = Some(true);
    intro.tracks = vec![state(&first, TrackState::Record)];
    intro.comments.before = vec![format!(
        "Record a {}-bar loop on '{first}', then move on by itself.",
        4
    )];
    let mut layer = DocSection::new("layer");
    layer.bars = Some(4);
    layer.autorelease = Some(false);
    layer.tracks = vec![state(&first, TrackState::Overdub)];
    // An instrument track is heard anyway; `hear_through` would say nothing about it.
    for track in tracks.iter().skip(1).filter(|t| !t.instrument) {
        layer.tracks.push(state(&track.name, TrackState::HearThrough));
    }
    layer.comments.before =
        vec!["One more layer on top. Loops until you press Release.".to_string()];
    let mut outro = DocSection::new("outro");
    outro.bars = Some(4);
    outro.autorelease = Some(true);
    outro.tracks = vec![state(&first, TrackState::Play)];
    outro.comments.before =
        vec!["Everything plays; tracks not named here are stopped.".to_string()];

    let mut doc = ScoreDoc {
        title: Some("New Song".to_string()),
        bpm: 100.0,
        time_signature: Some("4/4".to_string()),
        beats_per_bar: None,
        tracks,
        midi: Vec::new(),
        sections: vec![intro, layer, outro],
        mixer,
        header: vec![
            "Henrys Looper score. Each section states what every track does;".to_string(),
            "a track a section does not name is stopped.".to_string(),
        ],
        footer: Vec::new(),
        field_comments: OrderedMap::new(),
    };
    doc.field_comments.insert(
        "tracks",
        Comments {
            before: Vec::new(),
            after: Some("input: 1-based interface input, [3, 4] for a stereo pair".to_string()),
        },
    );
    render_doc(&doc)
}

#[cfg(test)]
mod tests;
