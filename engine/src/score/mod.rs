//! Phase 3, first half: the score compiler.
//!
//! ```text
//!  YAML-Text ──► yaml.rs ──► read.rs ──────────► compile.rs ──► CompiledScore
//!                (Baum mit    (pruefen,           (repeat aufloesen,   (statisch,
//!                 Positionen)  Fehler sammeln)     Tracks auffuellen)   serialisierbar)
//!                     │             │
//!                     └─────────────┴──► ScoreError { issues: [{line, column, message, suggestion}] }
//! ```
//!
//! A score is **declarative**: each section states what every track should be doing, and the
//! compiler turns that into a static structure a runner can walk without interpreting anything.
//! Two properties carry that:
//!
//! * `repeat:` is unrolled, so no section refers to another one any more.
//! * every section names **every** track - the ones it did not mention are on `stop`. A section is
//!   a complete state, never a delta.
//!
//! # What changed against the Ableton format
//!
//! `ableton_track`, `group`, `layers` and `reserve` are gone without replacement. They existed
//! because one Ableton track plays exactly one clip, so stacking overdubs meant pre-allocating a
//! pool of child tracks. Our engine stacks layers itself (`engine/src/engine/track.rs`), so a
//! track only needs to know **which input it listens on**: `input: 2`, 1-based, as printed on the
//! interface - or `input: [3, 4]` for a stereo source, plus an optional `pan`. Everything else -
//! `title`, `bpm`, `time_signature`, `midi`, `sections` with `bars`, `autorelease`, `quantize`,
//! `repeat` and the five track states - is unchanged.
//!
//! A score in the old format does not silently misbehave: the retired fields produce a German
//! message saying what replaced them.
//!
//! # Where this runs
//!
//! In the control thread, never in the audio callback. It allocates, formats strings and sorts -
//! all of which are forbidden on the audio side and all of which are fine here.

pub mod compile;
pub mod doc;
pub mod error;
pub mod map;
pub mod mixer;
pub mod model;
pub mod read;
pub mod sheet;
pub mod yaml;

#[cfg(test)]
mod clip_tests;
#[cfg(test)]
mod instrument_tests;
#[cfg(test)]
mod tests;

pub use doc::{ModelIssue, ScoreDoc, ScoreRef, parse_doc, render_doc};
pub use error::{ScoreError, ScoreIssue};
pub use map::OrderedMap;
pub use model::{
    ClipOptions, CompiledMidiBinding, MonitorFade, CompiledScore, CompiledSection, CompiledTrack, GRID_NAMES,
    InstrumentSource, LoopKind, MidiBindingSource, MidiKind, ScoreSource, SectionSource,
    TrackSource, TrackState,
};
pub use read::score_notes;
pub use yaml::Pos;

/// Compile a YAML score into its static form.
///
/// On failure the [`ScoreError`] holds **every** problem found, sorted by position - not just the
/// first one. See `error.rs` for why that is the point rather than a nicety.
pub fn compile_score(yaml_text: &str) -> Result<CompiledScore, ScoreError> {
    read::read_score(yaml_text).map(|source| compile::compile(&source))
}
