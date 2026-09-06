//! Validated [`ScoreSource`] -> static [`CompiledScore`].
//!
//! Nothing here can fail. `read.rs` has already rejected everything that could go wrong - unknown
//! `repeat:` targets and circular chains included - so this file is pure resolution:
//!
//! * **`repeat:` is unrolled.** A repeating section starts from a full copy of its target (which is
//!   itself already resolved, so chains work) and overrides whatever it names itself.
//! * **Every section lists every track.** A section is a complete desired state, not a delta; a
//!   track the section does not mention is [`TrackState::Stop`]. Without this the runner would have
//!   to remember what the previous section did, which is exactly the kind of hidden state that made
//!   the Ableton version unpredictable.
//! * **The time signature becomes numbers.** `"7/8"` -> `beats_per_bar = 7`, `beat_unit = 8`, which
//!   is what [`crate::engine::timeline::Timeline`] needs.
//! * **MIDI bindings become ids.** `{cc: 64}` -> `ch1.cc64`, the same string the MIDI monitor shows
//!   for an incoming event, so a binding can be matched by comparing one string.

use std::collections::HashMap;

use super::map::OrderedMap;
use super::model::{
    CompiledMidiBinding, CompiledScore, CompiledSection, CompiledTrack, ScoreSource, SectionSource,
    TrackState,
};
use crate::engine::schedule::Quantize;

/// Defaults for a section that inherits nothing. `autorelease: false` means "wait for the release
/// button"; `Quantize::Loop` is the engine's default grid (see `schedule.rs`).
const DEFAULT_AUTORELEASE: bool = false;
const DEFAULT_BARS: u32 = 4;
const DEFAULT_BEATS_PER_BAR: u32 = 4;
const DEFAULT_BEAT_UNIT: u32 = 4;

pub fn compile(source: &ScoreSource) -> CompiledScore {
    let (beats_per_bar, beat_unit) = resolve_time_signature(source);

    let tracks: Vec<CompiledTrack> = source
        .tracks
        .iter()
        .enumerate()
        .map(|(index, (name, spec))| CompiledTrack {
            name: name.to_string(),
            index,
            input: spec.input,
            input_channel: spec.input - 1,
            monitor: spec.monitor,
        })
        .collect();

    let midi: OrderedMap<CompiledMidiBinding> = source
        .midi
        .iter()
        .map(|(action, binding)| (action.to_string(), CompiledMidiBinding { id: binding.id() }))
        .collect();

    let by_id: HashMap<&str, &SectionSource> = source
        .sections
        .iter()
        .map(|section| (section.id.as_str(), section))
        .collect();
    let mut resolved: HashMap<&str, Resolved> = HashMap::new();
    let sections = source
        .sections
        .iter()
        .enumerate()
        .map(|(index, section)| {
            let state = resolve(section, &by_id, &mut resolved, source.sections.len());
            CompiledSection {
                id: section.id.clone(),
                index,
                bars: state.bars,
                autorelease: state.autorelease,
                quantize: state.quantize,
                // Every track, in track order - the missing ones on `stop`.
                tracks: tracks
                    .iter()
                    .map(|track| {
                        let assigned = state
                            .tracks
                            .get(track.name.as_str())
                            .copied()
                            .unwrap_or(TrackState::Stop);
                        (track.name.clone(), assigned)
                    })
                    .collect(),
                source_line: section.source_line,
            }
        })
        .collect();

    CompiledScore {
        title: source.title.clone().unwrap_or_default(),
        bpm: source.bpm,
        time_signature: format!("{beats_per_bar}/{beat_unit}"),
        beats_per_bar,
        beat_unit,
        tracks,
        midi,
        sections,
    }
}

/// A section with `repeat:` already folded in, before the missing tracks are filled up.
#[derive(Clone)]
struct Resolved {
    bars: u32,
    autorelease: bool,
    quantize: Quantize,
    tracks: OrderedMap<TrackState>,
}

/// Fold `repeat:` chains, memoising each section by id.
///
/// `budget` is one hop per section in the score. `read.rs` guarantees the chains are acyclic, so
/// the budget is never exhausted; it exists so that a future change to the reader can only ever
/// produce a wrong score, never a hung control thread.
fn resolve<'a>(
    section: &'a SectionSource,
    by_id: &HashMap<&'a str, &'a SectionSource>,
    memo: &mut HashMap<&'a str, Resolved>,
    budget: usize,
) -> Resolved {
    if let Some(hit) = memo.get(section.id.as_str()) {
        return hit.clone();
    }

    let base = match (&section.repeat, budget) {
        (Some(target), 1..) => by_id
            .get(target.as_str())
            .map(|target| resolve(target, by_id, memo, budget - 1)),
        _ => None,
    };
    let mut state = base.unwrap_or(Resolved {
        bars: DEFAULT_BARS,
        autorelease: DEFAULT_AUTORELEASE,
        quantize: Quantize::default(),
        tracks: OrderedMap::new(),
    });

    if let Some(bars) = section.bars {
        state.bars = bars;
    }
    if let Some(autorelease) = section.autorelease {
        state.autorelease = autorelease;
    }
    if let Some(quantize) = section.quantize {
        state.quantize = quantize;
    }
    for (name, track_state) in section.tracks.iter() {
        state.tracks.insert(name, *track_state);
    }

    memo.insert(section.id.as_str(), state.clone());
    state
}

/// `time_signature` wins, `beats_per_bar` is the alias for its numerator, `4/4` is the fallback.
/// The reader has already rejected the case where the two contradict each other.
fn resolve_time_signature(source: &ScoreSource) -> (u32, u32) {
    if let Some(text) = &source.time_signature
        && let Some((numerator, denominator)) = text.split_once('/')
        && let (Ok(numerator), Ok(denominator)) = (numerator.parse(), denominator.parse())
    {
        return (numerator, denominator);
    }
    (
        source.beats_per_bar.unwrap_or(DEFAULT_BEATS_PER_BAR),
        DEFAULT_BEAT_UNIT,
    )
}
