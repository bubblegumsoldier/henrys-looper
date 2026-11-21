//! Marked YAML tree -> validated [`ScoreSource`], collecting every problem on the way.
//!
//! This is where all the checking happens. The rule throughout: **never stop, never guess.** A
//! malformed value produces an issue and is then skipped, so one mistake never turns into a cascade
//! of follow-up messages about the parts that depend on it - but the rest of the file is still
//! read, so a musician sees all five typos in one pass instead of five.
//!
//! Semantic checks live here too (unknown track in a section, unknown or circular `repeat:`),
//! because this is the last place with positions in hand. That leaves `compile.rs` with nothing
//! that can fail.

use std::collections::HashMap;

use super::error::{ScoreError, ScoreIssue, allowed, suggest, suggest_or_list};
use super::map::OrderedMap;
use super::model::{
    ClipOptions, GRID_NAMES, InstrumentSource, LoopKind, MidiBindingSource, MidiKind, ScoreSource,
    SectionSource, TrackSource, TrackState,
};
use super::sheet::{SectionInfo, SheetIssue, check_sheet, compile_sheet};
use super::yaml::{Entry, Node, NodeKind, Pos, parse_document};
use crate::engine::bus::BusSend;
use crate::engine::command::MAX_TRACKS;
use crate::engine::schedule::Quantize;
use crate::midi::{Binding, ButtonMode, Control, Takeover, Target, TrackRef};

mod mixer;

const ROOT_FIELDS: [&str; 10] = [
    "title",
    "bpm",
    "time_signature",
    "beats_per_bar",
    "count_in",
    "tracks",
    "midi",
    "sections",
    "mixer",
    "sheet",
];
const TRACK_FIELDS: [&str; 10] = [
    "input",
    "midi",
    "pan",
    "monitor",
    "latency",
    "latency_trim",
    "bus",
    "monitor_bus",
    "loop",
    "grid",
];
const TRACK_MIDI_FIELDS: [&str; 4] = ["device", "channel", "notes", "transpose"];
const CLIP_FIELDS: [&str; 5] = ["state", "grid", "restart", "bars", "monitor"];
const SECTION_FIELDS: [&str; 6] = ["id", "repeat", "bars", "autorelease", "quantize", "tracks"];
/// Fields one binding may carry. The *keys* of `midi:` are not a closed list any more - any address
/// of the parameter tree does, and the two Ableton-era names `next_section` and `stop_all` are
/// among them, so a score that compiled before this existed compiles now.
const MIDI_BINDING_FIELDS: [&str; 7] =
    ["note", "cc", "channel", "mode", "takeover", "min", "max"];
const QUANTIZE_NAMES: [&str; 2] = ["bar", "loop"];

/// Fields the Ableton-era format had. They are not "unknown" - they are *gone*, and saying so is
/// far more useful than "meintest du 'input'?".
const RETIRED_TRACK_FIELDS: [(&str, &str); 5] = [
    (
        "ableton_track",
        "The engine no longer controls Ableton. A track names its input channel: 'input: <1-based channel>'.",
    ),
    (
        "group",
        "Group tracks no longer exist - the engine stacks layers by itself. Replace the track with 'input: <channel>'.",
    ),
    (
        "layers",
        "Layers are no longer reserved in advance; 'overdub' adds as many as you like.",
    ),
    (
        "reserve",
        "Reserve tracks no longer exist; the engine adds layers while it runs.",
    ),
    (
        "type",
        "There is only one kind of track now. Remove 'type' and give 'input: <channel>'.",
    ),
];

/// Bounds. Tempo mirrors what [`crate::engine::timeline::Timeline::validate`] accepts, so a score
/// that compiles can also be started; the rest is what an interface and a song plausibly have.
const BPM_MIN: f64 = 20.0;
const BPM_MAX: f64 = 400.0;
const MAX_INPUT_CHANNEL: u32 = 64;
/// Bound on both latency fields, in frames: two seconds at 48 kHz. Not a technical limit but a
/// typo catch - see `engine::live::MAX_LATENCY_FRAMES`, which is the same number for the CLI.
const MAX_LATENCY: i64 = crate::engine::live::MAX_LATENCY_FRAMES;
const MAX_BEATS_PER_BAR: u32 = 16;
const MAX_BARS: u32 = 999;
const BEAT_UNITS: [u32; 6] = [1, 2, 4, 8, 16, 32];

/// Parse and validate, without resolving `repeat:`. [`super::compile_score`] is this plus
/// `compile.rs`.
pub fn read_score(yaml_text: &str) -> Result<ScoreSource, ScoreError> {
    read_score_noted(yaml_text).0
}

/// [`read_score`] plus the notes: things that are written correctly but do something other than
/// the writer most likely meant (`stack` with `autorelease: true` on an empty track records one
/// pass). They never make a score fail.
pub fn read_score_noted(yaml_text: &str) -> (Result<ScoreSource, ScoreError>, Vec<ScoreIssue>) {
    let document = match parse_document(yaml_text) {
        Ok(document) => document,
        Err(error) => return (Err(error), Vec::new()),
    };
    let mut reader = Reader::default();
    let score = reader.read_root(&document);
    let mut notes = std::mem::take(&mut reader.notes);
    if reader.issues.is_empty()
        && let Some(score) = &score
    {
        notes.extend(stack_notes(score, &reader.state_pos));
        // Needs `repeat:` resolved, so it runs on a score that is otherwise clean.
        let issues = take_length_issues(score, &reader.clip_bars_pos);
        reader.issues.extend(issues);
        let issues = clip_monitor_issues(score, &reader.clip_monitor_pos);
        reader.issues.extend(issues);
        if reader.issues.is_empty() {
            // Played through without a device: an overdub that cannot happen in its section.
            let (errors, overdub_notes) = overdub_issues(score, &reader.state_pos);
            reader.issues.extend(errors);
            notes.extend(overdub_notes);
        }
        // Needs the resolved sections too: anchors name them, and their bars are what the text
        // between two anchors is measured against.
        let (errors, sheet_notes) = sheet_issues(score, &document, yaml_text);
        reader.issues.extend(errors);
        notes.extend(sheet_notes);
    }
    if reader.issues.is_empty() {
        // Only a clean read produces a score; anything else would hand the compiler half a file.
        let score = score.ok_or_else(|| {
            ScoreError::single(ScoreIssue::new(None, "The score could not be read."))
        });
        (score, notes)
    } else {
        (Err(ScoreError::new(reader.issues)), notes)
    }
}

/// Only the notes of a score (see [`read_score_noted`]), sorted by position. Empty when the text
/// does not parse at all.
pub fn score_notes(yaml_text: &str) -> Vec<ScoreIssue> {
    let mut notes = read_score_noted(yaml_text).1;
    notes.sort_by_key(|n| (n.line.is_none(), n.line.unwrap_or(0), n.column.unwrap_or(0)));
    notes
}

#[derive(Default)]
struct Reader {
    issues: Vec<ScoreIssue>,
    /// Not errors: written correctly, but worth a word.
    notes: Vec<ScoreIssue>,
    /// Where each written clip state stands: `(section id, track)` -> position of the value.
    state_pos: HashMap<(String, String), Pos>,
    /// Where each clip's `bars:` value stands, the same way.
    clip_bars_pos: HashMap<(String, String), Pos>,
    /// Where each clip's `monitor:` value stands, the same way.
    clip_monitor_pos: HashMap<(String, String), Pos>,
}

/// A take of its own length (`{state: record, bars: N}`) has to fit into its section: a take never
/// runs across a section boundary. Judged on the resolved sections, because the section's `bars`
/// may come from a `repeat:`, and a repeat carries the clip along.
fn take_length_issues(
    score: &ScoreSource,
    bars_pos: &HashMap<(String, String), Pos>,
) -> Vec<ScoreIssue> {
    let compiled = super::compile::compile(score);
    let mut issues = Vec::new();
    for section in &compiled.sections {
        for (track, clip) in section.clips.iter() {
            let Some(bars) = clip.bars else { continue };
            if bars <= section.bars {
                continue;
            }
            let pos = bars_pos.get(&(section.id.clone(), track.to_string())).copied();
            let mut issue = ScoreIssue::new(
                pos,
                format!(
                    "Track '{track}' records {bars} bars in Section '{}', but the section has only {}.",
                    section.id, section.bars
                ),
            )
            .with_suggestion(format!(
                "A take cannot run into the next section - at most 'bars: {}'.",
                section.bars
            ));
            if pos.is_none() {
                issue.line = section.source_line;
            }
            issues.push(issue);
        }
    }
    issues
}

/// `{state: play, monitor: true}` asks for the track's live input on top of its loop. Two tracks
/// cannot give that, and saying so beats silently not doing it: a track the score set to
/// `monitor: false` (the input is never heard - that is what the option is for), and an instrument
/// track (its keys are heard whenever it plays anyway; to silence them it is `monitor: false`).
fn clip_monitor_issues(
    score: &ScoreSource,
    monitor_pos: &HashMap<(String, String), Pos>,
) -> Vec<ScoreIssue> {
    let compiled = super::compile::compile(score);
    let mut issues = Vec::new();
    for section in &compiled.sections {
        for (name, clip) in section.clips.iter() {
            if !clip.monitor {
                continue;
            }
            let Some(track) = compiled.tracks.iter().find(|t| t.name == name) else {
                continue;
            };
            let (why, hint) = if track.is_instrument() {
                (
                    format!(
                        "'monitor' does nothing on the instrument track '{name}' (Section '{}'): \
                         what is played on it is heard whenever it plays.",
                        section.id
                    ),
                    "Remove 'monitor'.".to_string(),
                )
            } else if !track.monitor {
                (
                    format!(
                        "Track '{name}' is set to 'monitor: false', so its input is never heard - \
                         but Section '{}' asks to hear it ('monitor: true').",
                        section.id
                    ),
                    format!("Remove one of the two: 'monitor: false' under tracks, or 'monitor' on the clip."),
                )
            } else {
                continue;
            };
            let pos = monitor_pos.get(&(section.id.clone(), name.to_string())).copied();
            let mut issue = ScoreIssue::new(pos, why).with_suggestion(hint);
            if pos.is_none() {
                issue.line = section.source_line;
            }
            issues.push(issue);
        }
    }
    issues
}

/// An `overdub` (or the first pass of a `stack`) on a loop that keeps playing into a section is
/// recorded on the loop's own grid - and a section that is shorter than the loop, entered in the
/// middle of it, may have no point of that grid inside it at all. Nothing can be recorded there.
/// The runner then schedules nothing (`engine::runner`, "An overdub stays inside its section");
/// this says so before the run. Found by playing the score through the runner without a device
/// (`runner::dry_run`), so the rule is the runner's own and not a second copy of it.
///
/// An error while every section up to that one goes on by itself (`autorelease: true`) - then
/// where each section begins is certain. After a section that waits for Release it depends on
/// when Release is pressed, so it is a note: "with Release at the earliest moment".
fn overdub_issues(
    score: &ScoreSource,
    state_pos: &HashMap<(String, String), Pos>,
) -> (Vec<ScoreIssue>, Vec<ScoreIssue>) {
    let compiled = super::compile::compile(score);
    let mut errors = Vec::new();
    let mut notes = Vec::new();
    for skipped in crate::engine::runner::dry_run(&compiled) {
        let section = &compiled.sections[skipped.section];
        let certain = compiled.sections[..=skipped.section]
            .iter()
            .all(|s| s.autorelease);
        let pos = state_pos
            .get(&(section.id.clone(), skipped.track.clone()))
            .copied();
        let message = format!(
            "Track '{}' is on {} in Section '{}', but no pass of its {}-bar loop starts inside the \
             section's {} bar{}{} - nothing would be recorded.",
            skipped.track,
            skipped.state.as_str(),
            section.id,
            skipped.loop_bars,
            section.bars,
            if section.bars == 1 { "" } else { "s" },
            if certain { "" } else { " (with Release pressed as early as possible)" },
        );
        let mut issue = ScoreIssue::new(pos, message).with_suggestion(format!(
            "Start the loop from its top here ('{}: {{state: {}, restart: true}}'), or make the \
             section at least {} bars long - its next pass begins in bar {} of it.",
            skipped.track,
            skipped.state.as_str(),
            skipped.next_pass + 1,
            skipped.next_pass + 1
        ));
        if pos.is_none() {
            issue.line = section.source_line;
        }
        if certain {
            errors.push(issue);
        } else {
            notes.push(issue);
        }
    }
    (errors, notes)
}

/// The sheet's anchors and timing, checked against the sections (`score::sheet`). Positions are
/// lines of the file: line `n` of a `sheet: |` block is `n` lines below its header. A sheet written
/// as a quoted string has no such lines, so everything about it points at the `sheet:` key.
fn sheet_issues(score: &ScoreSource, document: &Node<'_>, yaml_text: &str) -> (Vec<ScoreIssue>, Vec<ScoreIssue>) {
    let Some(text) = score.sheet.as_deref() else {
        return (Vec::new(), Vec::new());
    };
    let compiled = super::compile::compile(score);
    let infos: Vec<SectionInfo<'_>> = compiled
        .sections
        .iter()
        .map(|s| SectionInfo { id: &s.id, bars: s.bars, autorelease: s.autorelease })
        .collect();
    let sheet = compile_sheet(text, compiled.beats_per_bar, &infos);
    let (errors, notes) = check_sheet(&sheet, &infos);
    let place = sheet_place(document, yaml_text);
    let issue = |i: SheetIssue| {
        let pos = match place {
            Some((key, Some(first))) => Pos { line: first + i.line - 1, column: key.column + 2 },
            Some((key, None)) => key,
            None => Pos { line: 1, column: 1 },
        };
        ScoreIssue::new(Some(pos), i.message).maybe_suggestion(i.suggestion)
    };
    (errors.into_iter().map(issue).collect(), notes.into_iter().map(issue).collect())
}

/// Where the `sheet:` key stands, and the file line of the sheet's first line when it is a block
/// scalar (`sheet: |`), whose content starts on the line below its header.
fn sheet_place(document: &Node<'_>, yaml_text: &str) -> Option<(Pos, Option<u32>)> {
    let NodeKind::Map(entries) = &document.kind else { return None };
    let entry = entries.iter().find(|e| e.key.as_str() == Some("sheet"))?;
    let key = entry.key.pos;
    let header = yaml_text.lines().nth(key.line.saturating_sub(1) as usize)?;
    let after = header.split_once(':').map_or("", |(_, rest)| rest.trim_start());
    let block = after.starts_with(['|', '>']);
    Some((key, block.then_some(key.line + 1)))
}

/// `stack` with `autorelease: true` on a track that nothing has recorded on yet: the section's
/// `bars` are the loop length, so it ends after exactly one pass - one layer, which is `record`.
/// Judged on the sections in written order, `repeat:` resolved.
fn stack_notes(score: &ScoreSource, state_pos: &HashMap<(String, String), Pos>) -> Vec<ScoreIssue> {
    let compiled = super::compile::compile(score);
    let mut recorded: Vec<&str> = Vec::new();
    let mut notes = Vec::new();
    for section in &compiled.sections {
        for (track, state) in section.tracks.iter() {
            if *state == TrackState::Stack && section.autorelease && !recorded.contains(&track) {
                let pos = state_pos
                    .get(&(section.id.clone(), track.to_string()))
                    .copied();
                let mut note = ScoreIssue::new(
                    pos,
                    format!(
                        "Section '{}': stack with autorelease records a single pass on the empty                          track '{track}' - use record, or switch autorelease off.",
                        section.id
                    ),
                );
                if pos.is_none() {
                    note.line = section.source_line;
                }
                notes.push(note);
            }
        }
        for (track, state) in section.tracks.iter() {
            if state.records() && !recorded.contains(&track) {
                recorded.push(track);
            }
        }
    }
    notes
}

impl Reader {
    /// Something written correctly that the engine cannot do yet. Nothing is in that state at the
    /// moment (`loop: midi` and `grid:` were the last); the channel stays for the next one.
    #[allow(dead_code)]
    fn note(&mut self, pos: Option<Pos>, message: impl Into<String>) {
        self.notes.push(ScoreIssue::new(pos, message));
    }

    fn error(&mut self, pos: Option<Pos>, message: impl Into<String>) {
        self.issues.push(ScoreIssue::new(pos, message));
    }

    fn error_with(&mut self, pos: Option<Pos>, message: impl Into<String>, suggestion: impl Into<String>) {
        self.issues
            .push(ScoreIssue::new(pos, message).with_suggestion(suggestion));
    }

    // -----------------------------------------------------------------------------------------
    // Generic accessors. Each reports a type error and returns None rather than substituting a
    // value, so a follow-up check never runs on invented data.
    // -----------------------------------------------------------------------------------------

    fn as_map<'a>(&mut self, node: &'a Node<'a>, what: &str) -> Option<&'a [Entry<'a>]> {
        match &node.kind {
            NodeKind::Map(entries) => Some(entries),
            _ => {
                self.error(
                    Some(node.pos),
                    format!(
                        "{what} must be a mapping (key: value), found: {}.",
                        node.describe()
                    ),
                );
                None
            }
        }
    }

    fn as_seq<'a>(&mut self, node: &'a Node<'a>, what: &str) -> Option<&'a [Node<'a>]> {
        match &node.kind {
            NodeKind::Seq(items) => Some(items),
            _ => {
                self.error(
                    Some(node.pos),
                    format!("{what} must be a list, found: {}.", node.describe()),
                );
                None
            }
        }
    }

    fn as_str<'a>(&mut self, node: &'a Node<'a>, what: &str) -> Option<&'a str> {
        match &node.kind {
            NodeKind::Str(text) => Some(text.as_ref()),
            _ => {
                self.error(
                    Some(node.pos),
                    format!("{what} must be text, found: {}.", node.describe()),
                );
                None
            }
        }
    }

    fn as_bool(&mut self, node: &Node<'_>, what: &str) -> Option<bool> {
        match &node.kind {
            NodeKind::Bool(value) => Some(*value),
            _ => {
                let issue = ScoreIssue::new(
                    Some(node.pos),
                    format!("{what} must be true or false, found: {}.", node.describe()),
                )
                .with_suggestion("Write true or false (without quotes; 'yes'/'no' don't count).");
                self.issues.push(issue);
                None
            }
        }
    }

    /// A whole number in `min..=max`. Range and type are two different messages on purpose: the
    /// first says what to write, the second says what is allowed.
    fn as_int(&mut self, node: &Node<'_>, what: &str, min: u32, max: u32) -> Option<u32> {
        let value = match &node.kind {
            NodeKind::Int(value) => *value,
            _ => {
                self.error(
                    Some(node.pos),
                    format!(
                        "{what} must be a whole number, found: {}.",
                        node.describe()
                    ),
                );
                return None;
            }
        };
        if value < i64::from(min) || value > i64::from(max) {
            self.error_with(
                Some(node.pos),
                format!("{what} must be between {min} and {max}, found: {value}."),
                format!("Allowed: {min} to {max}."),
            );
            return None;
        }
        Some(value as u32)
    }

    /// A whole number in a **signed** range. Separate from [`Reader::as_int`] because that one
    /// takes `u32` bounds, and a latency surcharge is the one value in the format that may be
    /// negative - a digital return can arrive earlier than the converter path.
    fn as_signed_int(&mut self, node: &Node<'_>, what: &str, min: i64, max: i64) -> Option<i64> {
        let value = match &node.kind {
            NodeKind::Int(value) => *value,
            _ => {
                self.error(
                    Some(node.pos),
                    format!(
                        "{what} must be a whole number, found: {}.",
                        node.describe()
                    ),
                );
                return None;
            }
        };
        if value < min || value > max {
            self.error_with(
                Some(node.pos),
                format!("{what} must be between {min} and {max}, found: {value}."),
                format!("Allowed: {min} to {max}."),
            );
            return None;
        }
        Some(value)
    }

    fn as_number(&mut self, node: &Node<'_>, what: &str) -> Option<f64> {
        match &node.kind {
            NodeKind::Int(value) => Some(*value as f64),
            NodeKind::Float(value) => Some(*value),
            _ => {
                self.error(
                    Some(node.pos),
                    format!("{what} must be a number, found: {}.", node.describe()),
                );
                None
            }
        }
    }

    /// Index a mapping by key, reporting unknown fields and duplicates once each.
    ///
    /// `retired` names fields that used to exist. They get their own explanation instead of an
    /// "unknown field" with a typo suggestion, because "meintest du 'input'?" for `ableton_track`
    /// would be actively misleading.
    ///
    /// The returned lookup keeps the *first* occurrence of a duplicated key, which is the one the
    /// musician sees at the top of the block.
    fn index_fields<'a>(
        &mut self,
        entries: &'a [Entry<'a>],
        known: &[&str],
        retired: &[(&str, &str)],
        what: &str,
    ) -> HashMap<&'a str, &'a Node<'a>> {
        let mut found: HashMap<&str, &Node> = HashMap::new();
        for entry in entries {
            let Some(name) = entry.key.as_str() else {
                self.error(
                    Some(entry.key.pos),
                    format!("Keys in {what} must be text, found: {}.", entry.key.describe()),
                );
                continue;
            };
            if let Some((_, hint)) = retired.iter().find(|(field, _)| *field == name) {
                self.error_with(
                    Some(entry.key.pos),
                    format!("{what}: the field '{name}' no longer exists."),
                    *hint,
                );
                continue;
            }
            if !known.contains(&name) {
                let message = format!("Unknown field '{name}' in {what}.");
                self.error_with(Some(entry.key.pos), message, suggest_or_list(name, known));
                continue;
            }
            if found.contains_key(name) {
                self.error_with(
                    Some(entry.key.pos),
                    format!("Field '{name}' appears more than once in {what}."),
                    "Each key may appear only once per mapping; the second one would silently overwrite the first.",
                );
                continue;
            }
            found.insert(name, &entry.value);
        }
        found
    }

    // -----------------------------------------------------------------------------------------
    // The score itself
    // -----------------------------------------------------------------------------------------

    fn read_root(&mut self, document: &Node<'_>) -> Option<ScoreSource> {
        let entries = match &document.kind {
            NodeKind::Map(entries) => entries,
            _ => {
                self.error_with(
                    Some(document.pos),
                    "The score must be a mapping with 'bpm', 'tracks' and 'sections'.",
                    "Template: examples/henry-3-4.rust.yaml",
                );
                return None;
            }
        };
        let fields = self.index_fields(entries, &ROOT_FIELDS, &[], "the score");

        for required in ["bpm", "tracks", "sections"] {
            if !fields.contains_key(required) {
                self.error_with(
                    Some(document.pos),
                    format!("Required field '{required}' is missing from the score."),
                    required_hint(required),
                );
            }
        }

        let title = fields
            .get("title")
            .and_then(|node| self.as_str(node, "'title'").map(str::to_string));

        let bpm = fields.get("bpm").and_then(|node| {
            let value = self.as_number(node, "'bpm'")?;
            if !(BPM_MIN..=BPM_MAX).contains(&value) {
                self.error_with(
                    Some(node.pos),
                    format!("'bpm' must be between {BPM_MIN} and {BPM_MAX}, found: {value}."),
                    "The engine plays tempos from 20 to 400 BPM.",
                );
                return None;
            }
            Some(value)
        });

        let (time_signature, beats_per_bar) = self.read_time_signature(&fields);
        let count_in = fields
            .get("count_in")
            .and_then(|node| self.read_count_in(node));
        let tracks = fields
            .get("tracks")
            .and_then(|node| self.read_tracks(node))
            .unwrap_or_default();
        let track_names: Vec<&str> = tracks.keys().collect();
        let sections = fields
            .get("sections")
            .and_then(|node| self.read_sections(node, &track_names))
            .unwrap_or_default();
        // After the sections on purpose: a binding may name one (`transport.goto.3`), and checking
        // it needs to know how many there are.
        let midi = fields
            .get("midi")
            .and_then(|node| self.read_midi(node, &track_names, sections.len()))
            .unwrap_or_default();
        // Optional, and absent from every score written before it existed.
        let instrument_tracks: Vec<&str> = tracks
            .iter()
            .filter(|(_, t)| t.instrument.is_some())
            .map(|(name, _)| name)
            .collect();
        let mixer = fields
            .get("mixer")
            .and_then(|node| self.read_mixer(node, &track_names, &instrument_tracks));
        // Optional. The text is kept as written; its anchors are checked once the sections are
        // resolved (`sheet_issues`).
        let sheet = fields
            .get("sheet")
            .and_then(|node| self.as_str(node, "'sheet'").map(str::to_string));

        Some(ScoreSource {
            title,
            bpm: bpm?,
            time_signature,
            beats_per_bar,
            count_in,
            tracks,
            midi,
            sections,
            mixer,
            sheet,
        })
    }

    /// `count_in: 2` - bars of click before the first section. Zero is refused with the reason: the
    /// runner always counts in, because without it the first take would start on the very next bar
    /// line, with nothing to play along to.
    fn read_count_in(&mut self, node: &Node<'_>) -> Option<u32> {
        let max = crate::engine::runner::MAX_COUNT_IN_BARS;
        let bars = self.as_int(node, "'count_in'", 0, max)?;
        if bars == 0 {
            self.error_with(
                Some(node.pos),
                "'count_in' must be at least 1 bar, found: 0.".to_string(),
                format!(
                    "A score always counts in: without it the first take would start on the very \
                     next bar line, with nothing to play along to. Write 1 to {max} bars, or leave \
                     the field out for the default of {} bars.",
                    crate::engine::runner::DEFAULT_COUNT_IN_BARS
                ),
            );
            return None;
        }
        Some(bars)
    }

    /// `time_signature: "3/4"` and the older alias `beats_per_bar: 3`. Both may appear, but they
    /// have to agree - silently preferring one of two contradicting statements is how a score ends
    /// up counting in the wrong metre on stage.
    fn read_time_signature(
        &mut self,
        fields: &HashMap<&str, &Node<'_>>,
    ) -> (Option<String>, Option<u32>) {
        let beats_per_bar = fields.get("beats_per_bar").and_then(|node| {
            self.as_int(node, "'beats_per_bar'", 1, MAX_BEATS_PER_BAR)
        });

        let Some(node) = fields.get("time_signature") else {
            return (None, beats_per_bar);
        };
        let Some(text) = self.as_str(node, "'time_signature'") else {
            return (None, beats_per_bar);
        };
        let Some((numerator, denominator)) = split_time_signature(text) else {
            self.error_with(
                Some(node.pos),
                format!("Invalid time signature '{text}'."),
                "Write 'beats/note value', e.g. time_signature: 3/4 or \"7/8\".",
            );
            return (None, beats_per_bar);
        };
        if !(1..=MAX_BEATS_PER_BAR).contains(&numerator) {
            self.error_with(
                Some(node.pos),
                format!("Time signature '{text}': the top number must be between 1 and {MAX_BEATS_PER_BAR}."),
                "e.g. 3/4, 4/4, 6/8, 7/8.",
            );
            return (None, beats_per_bar);
        }
        if !BEAT_UNITS.contains(&denominator) {
            self.error_with(
                Some(node.pos),
                format!("Time signature '{text}': the bottom number must be a power of two."),
                allowed(&["1", "2", "4", "8", "16", "32"]),
            );
            return (None, beats_per_bar);
        }
        if let Some(alias) = beats_per_bar
            && alias != numerator
        {
            let pos = fields.get("beats_per_bar").map(|node| node.pos);
            self.error_with(
                pos,
                format!("'beats_per_bar: {alias}' contradicts 'time_signature: {text}'."),
                "Give only one of the two (preferably time_signature), or set beats_per_bar to the top number.",
            );
        }
        (Some(text.to_string()), beats_per_bar)
    }

    fn read_tracks(&mut self, node: &Node<'_>) -> Option<OrderedMap<TrackSource>> {
        let entries = self.as_map(node, "'tracks'")?;
        if entries.is_empty() {
            self.error_with(
                Some(node.pos),
                "'tracks' is empty; a score needs at least one track.",
                "e.g. 'guitar: {input: 2}'.",
            );
            return None;
        }

        let mut tracks = OrderedMap::new();
        for entry in entries {
            let Some(name) = entry.key.as_str() else {
                self.error(
                    Some(entry.key.pos),
                    format!("Track names must be text, found: {}.", entry.key.describe()),
                );
                continue;
            };
            if !is_identifier(name) {
                self.error_with(
                    Some(entry.key.pos),
                    format!("Invalid track name '{name}'."),
                    "Use letters, digits, '_' and '-', starting with a letter or '_'.",
                );
                continue;
            }
            if tracks.contains_key(name) {
                self.error_with(
                    Some(entry.key.pos),
                    format!("Track '{name}' is defined twice."),
                    "Each track may appear only once under 'tracks'.",
                );
                continue;
            }
            if let Some(track) = self.read_track(name, &entry.value) {
                tracks.insert(name, track);
            }
        }

        if tracks.len() > MAX_TRACKS {
            self.error_with(
                Some(node.pos),
                format!(
                    "The score has {} tracks; the engine handles at most {MAX_TRACKS}.",
                    tracks.len()
                ),
                format!("List at most {MAX_TRACKS} tracks under 'tracks'."),
            );
        }
        Some(tracks)
    }

    fn read_track(&mut self, name: &str, node: &Node<'_>) -> Option<TrackSource> {
        let what = format!("Track '{name}'");
        let entries = self.as_map(node, &what)?;

        // A misspelt or retired field is one mistake, not two: `{inpt: 1}` and `{ableton_track: 0}`
        // must not *also* be told that 'input' is missing. See `had_field_issue`.
        let before = self.issues.len();
        let fields = self.index_fields(entries, &TRACK_FIELDS, &RETIRED_TRACK_FIELDS, &what);
        let had_field_issue = self.issues.len() > before;

        let input = match fields.get("input") {
            Some(node) => self.read_track_input(name, node),
            None => {
                if !had_field_issue {
                    self.error_with(
                        Some(node.pos),
                        format!("Track '{name}' has no 'input'."),
                        "A track names its input channel on the interface, 1-based: 'input: 1' \
                         for mono, 'input: [3, 4]' for stereo - or 'input: instrument' for an \
                         instrument plugin played over MIDI.",
                    );
                }
                None
            }
        };
        // Every field is checked before any of them is given up on, so a track with three mistakes
        // reports three messages rather than one at a time.
        let pan = match fields.get("pan") {
            Some(node) => self.read_pan(name, node),
            None => Some(0.0),
        };
        let monitor = match fields.get("monitor") {
            Some(node) => self.as_bool(node, &format!("'monitor' of Track '{name}'")),
            None => Some(true),
        };
        // Two fields rather than one, and they mean different things: `latency` is what a
        // measurement found and gets overwritten by the next one, `latency_trim` is what a human
        // added by ear and survives it. See `engine::track::TrackLatency`.
        let latency = match fields.get("latency") {
            Some(node) => self
                .as_int(node, &format!("'latency' of Track '{name}'"), 0, MAX_LATENCY as u32)
                .map(Some),
            None => Some(None),
        };
        let latency_trim = match fields.get("latency_trim") {
            Some(node) => self
                .as_signed_int(
                    node,
                    &format!("'latency_trim' of Track '{name}'"),
                    -MAX_LATENCY,
                    MAX_LATENCY,
                )
                .map(|v| v as i32),
            None => Some(0),
        };
        // Where this track is heard, and where its live input is heard. Two fields because they
        // answer two questions: a guide figure belongs on the headphones while the singer over it
        // belongs everywhere, and one field could not say that.
        let bus = match fields.get("bus") {
            Some(node) => self.read_bus_send(name, "bus", node),
            None => Some(BusSend::BOTH),
        };
        let is_instrument = matches!(input, Some(InputSpec::Instrument));
        let monitor_bus = match fields.get("monitor_bus") {
            Some(node) => self.read_bus_send(name, "monitor_bus", node),
            // An instrument's "live input" is music, not a voice the PA already carries: it is
            // heard in the room as well, as the engine does it for a setup track.
            None if is_instrument => Some(BusSend::BOTH),
            None => Some(BusSend::MONITOR),
        };
        let midi = match fields.get("midi") {
            Some(node) => {
                if input.is_some() && !is_instrument {
                    self.error_with(
                        Some(node.pos),
                        format!("Track '{name}' has 'midi', but it records an audio input."),
                        "'midi' says which keyboard plays an instrument track; write \
                         'input: instrument' or remove 'midi'.",
                    );
                    None
                } else {
                    self.read_track_midi(name, node).map(Some)
                }
            }
            None => Some(None),
        };
        // An instrument track's compensation is not a cable: the engine knows it exactly (output
        // latency, one block and the plugin's own), so only the surcharge may be written.
        if is_instrument && let Some(node) = fields.get("latency") {
            self.error_with(
                Some(node.pos),
                format!("Track '{name}' is an instrument track and has no measured 'latency'."),
                "Its compensation is automatic (output latency + one block + the plugin's own); \
                 use 'latency_trim' to shift it by ear.",
            );
        }
        let loop_kind = match fields.get("loop") {
            Some(node) => self.read_loop_kind(name, node, input.is_some() && !is_instrument),
            None => Some(LoopKind::Audio),
        };
        let grid = match fields.get("grid") {
            Some(node) => self.read_grid(&format!("'grid' of Track '{name}'"), node).map(Some),
            None => Some(None),
        };
        let (input, input_right, instrument) = match input? {
            InputSpec::Channels(left, right) => (left, right, None),
            InputSpec::Instrument => (0, None, Some(midi?.unwrap_or_default())),
        };
        Some(TrackSource {
            input,
            input_right,
            pan: pan?,
            monitor: monitor?,
            latency: latency?,
            latency_trim: latency_trim?,
            bus: bus?.name().to_string(),
            monitor_bus: monitor_bus?.name().to_string(),
            instrument,
            loop_kind: loop_kind?,
            grid: grid?,
        })
    }

    /// `midi: {device: "KeyLab 49", channel: 1, notes: 36-51}` of an instrument track.
    fn read_track_midi(&mut self, name: &str, node: &Node<'_>) -> Option<InstrumentSource> {
        let what = format!("'midi' of Track '{name}'");
        let entries = self.as_map(node, &what)?;
        let fields = self.index_fields(entries, &TRACK_MIDI_FIELDS, &[], &what);
        let mut out = InstrumentSource::default();
        let mut ok = true;
        if let Some(node) = fields.get("device") {
            match self.as_str(node, &format!("'device' in {what}")) {
                Some(text) if !text.trim().is_empty() => out.device = Some(text.to_string()),
                Some(_) => {
                    self.error_with(
                        Some(node.pos),
                        format!("'device' in {what} is empty."),
                        "Name the MIDI device (or a unique part of its name), or leave 'device' \
                         out to listen to every device.",
                    );
                    ok = false;
                }
                None => ok = false,
            }
        }
        if let Some(node) = fields.get("channel") {
            match &node.kind {
                NodeKind::Str(text) if text.as_ref() == "all" => {}
                NodeKind::Int(_) => match self.as_int(node, &format!("'channel' in {what}"), 1, 16) {
                    Some(c) => out.channel = Some(c as u8),
                    None => ok = false,
                },
                _ => {
                    self.error_with(
                        Some(node.pos),
                        format!("'channel' in {what} must be 1 to 16 or 'all', found: {}.", node.describe()),
                        "e.g. 'channel: 1', or 'channel: all' (the same as leaving it out).",
                    );
                    ok = false;
                }
            }
        }
        if let Some(node) = fields.get("transpose") {
            match &node.kind {
                NodeKind::Int(v) if (-48..=48).contains(v) => {
                    if *v != 0 {
                        out.transpose = Some(*v as i8);
                    }
                }
                _ => {
                    self.error_with(
                        Some(node.pos),
                        format!(
                            "'transpose' in {what} must be a number of semitones from -48 to 48, found: {}.",
                            node.describe()
                        ),
                        "e.g. 'transpose: 12' for pads that send an octave too low.",
                    );
                    ok = false;
                }
            }
        }
        if let Some(node) = fields.get("notes") {
            match self.read_note_range(&format!("'notes' in {what}"), node) {
                Some((low, high)) => {
                    out.note_low = Some(low);
                    out.note_high = Some(high);
                }
                None => ok = false,
            }
        }
        ok.then_some(out)
    }

    /// `notes: 36-51` - the lowest and the highest key, MIDI note numbers, both included.
    fn read_note_range(&mut self, what: &str, node: &Node<'_>) -> Option<(u8, u8)> {
        let hint = "Write the lowest and the highest key as MIDI note numbers, e.g. 'notes: 36-51' \
                    (C1 to D#2 on most pads); leave 'notes' out for all keys.";
        let text = match &node.kind {
            NodeKind::Str(text) => text.to_string(),
            _ => {
                self.error_with(
                    Some(node.pos),
                    format!("{what} must be a range like 36-51, found: {}.", node.describe()),
                    hint,
                );
                return None;
            }
        };
        let parsed = text.split_once('-').and_then(|(a, b)| {
            Some((a.trim().parse::<u8>().ok()?, b.trim().parse::<u8>().ok()?))
        });
        match parsed {
            Some((low, high)) if low <= high && high <= 127 => Some((low, high)),
            Some((low, high)) if low > high => {
                self.error_with(
                    Some(node.pos),
                    format!("{what}: '{text}' goes downwards."),
                    format!("Write the lower key first: '{high}-{low}'."),
                );
                None
            }
            _ => {
                self.error_with(
                    Some(node.pos),
                    format!("{what}: '{text}' is not a range of MIDI notes (0 to 127)."),
                    hint,
                );
                None
            }
        }
    }

    /// `loop: audio|midi`. `midi` needs an instrument track.
    fn read_loop_kind(&mut self, name: &str, node: &Node<'_>, audio_input: bool) -> Option<LoopKind> {
        let what = format!("'loop' of Track '{name}'");
        let text = self.as_str(node, &what)?;
        let kind = match text {
            "audio" => LoopKind::Audio,
            "midi" => LoopKind::Midi,
            _ => {
                self.error_with(
                    Some(node.pos),
                    format!("{what}: '{text}' is not a loop kind."),
                    suggest_or_list(text, &["audio", "midi"]),
                );
                return None;
            }
        };
        if kind == LoopKind::Midi {
            if audio_input {
                self.error_with(
                    Some(node.pos),
                    format!("{what}: a MIDI loop needs an instrument track."),
                    "Only an instrument track plays notes; write 'input: instrument' or 'loop: audio'.",
                );
                return None;
            }
        }
        Some(kind)
    }

    /// `grid: 1/16` - the note grid of a MIDI loop, for a track or for one clip of a section.
    fn read_grid(&mut self, what: &str, node: &Node<'_>) -> Option<String> {
        let text = match &node.kind {
            NodeKind::Str(text) => text.to_string(),
            _ => {
                self.error_with(
                    Some(node.pos),
                    format!("{what} must be a note value, found: {}.", node.describe()),
                    allowed(&GRID_NAMES),
                );
                return None;
            }
        };
        if !GRID_NAMES.contains(&text.as_str()) {
            self.error_with(
                Some(node.pos),
                format!("{what}: '{text}' is not a note grid."),
                suggest_or_list(&text, &GRID_NAMES),
            );
            return None;
        }
        Some(text)
    }

    /// `bus: monitor`, `bus: main+monitor`, `bus: none` - which output buses a source is heard on.
    ///
    /// Written as a word rather than a list, because that is how it is spoken: "der geht nur auf
    /// den Monitor". The accepted words are exactly the ones `BusSend` writes back out, so a score
    /// round-trips through the compiler unchanged.
    fn read_bus_send(&mut self, name: &str, field: &str, node: &Node<'_>) -> Option<BusSend> {
        let what = format!("'{field}' of Track '{name}'");
        let text = self.as_str(node, &what)?.to_string();
        match BusSend::parse(&text) {
            Some(send) => Some(send),
            None => {
                self.error_with(
                    Some(node.pos),
                    format!("{what}: '{text}' is not a bus."),
                    "Use 'main' (room only), 'monitor' (headphones only), \
                     'main+monitor' or 'none'. The click is always on 'monitor' and nowhere else.",
                );
                None
            }
        }
    }

    /// `input: 1` for a mono track, `input: [3, 4]` for a stereo one.
    ///
    /// A list of exactly two entries and nothing else: one entry is a mono track written oddly, and
    /// three would be a surround format the engine does not have. Both get their own sentence
    /// rather than being quietly truncated.
    fn read_track_input(&mut self, name: &str, node: &Node<'_>) -> Option<InputSpec> {
        let what = format!("'input' of Track '{name}'");
        match &node.kind {
            NodeKind::Str(text) if text.as_ref() == "instrument" => Some(InputSpec::Instrument),
            NodeKind::Str(text) => {
                self.error_with(
                    Some(node.pos),
                    format!("{what}: '{text}' is not an input."),
                    "An input channel of the interface, 1-based ('input: 1', 'input: [3, 4]'), \
                     or 'input: instrument'.",
                );
                None
            }
            NodeKind::Seq(items) => {
                if items.len() != 2 {
                    self.error_with(
                        Some(node.pos),
                        format!(
                            "{what} is a list with {} entries; a stereo track names exactly two inputs.",
                            items.len()
                        ),
                        "e.g. 'input: [3, 4]' for stereo or 'input: 3' for mono.",
                    );
                    return None;
                }
                let left = self.as_int(&items[0], &format!("The left input in {what}"), 1, MAX_INPUT_CHANNEL);
                let right = self.as_int(&items[1], &format!("The right input in {what}"), 1, MAX_INPUT_CHANNEL);
                let (left, right) = (left?, right?);
                if left == right {
                    self.error_with(
                        Some(node.pos),
                        format!("{what} names input {left} twice."),
                        format!(
                            "A stereo track needs two different inputs; for a mono \
                             track, 'input: {left}' is enough."
                        ),
                    );
                    return None;
                }
                Some(InputSpec::Channels(left, Some(right)))
            }
            _ => Some(InputSpec::Channels(
                self.as_int(node, &what, 1, MAX_INPUT_CHANNEL)?,
                None,
            )),
        }
    }

    /// `pan: -0.4`. A number, and inside the range the engine's panner actually has.
    fn read_pan(&mut self, name: &str, node: &Node<'_>) -> Option<f32> {
        let what = format!("'pan' of Track '{name}'");
        let value = self.as_number(node, &what)?;
        if !(-1.0..=1.0).contains(&value) {
            self.error_with(
                Some(node.pos),
                format!("{what} must be between -1 and 1, found: {value}."),
                "-1 is hard left, 0 is the center, 1 is hard right.",
            );
            return None;
        }
        Some(value as f32)
    }

    /// The `midi:` block: any address of the parameter tree on the left, one control on the right.
    ///
    /// Unlike every other mapping in this file the keys are **not** a closed list, so
    /// [`Self::index_fields`] cannot be used: the check is not "is this one of six words" but "is
    /// this a valid address", and that lives in [`crate::midi::Target::parse`], which owns the
    /// tree. What is still checked here, because only this file knows it, is that the address
    /// points at something *this score has*: a track it declares, a section it contains.
    ///
    /// A control bound twice is reported rather than letting the second entry win. Two pads on one
    /// target is fine and useful; one pad on two targets is a file that does something other than
    /// it says.
    fn read_midi(
        &mut self,
        node: &Node<'_>,
        track_names: &[&str],
        sections: usize,
    ) -> Option<OrderedMap<MidiBindingSource>> {
        let entries = self.as_map(node, "'midi'")?;
        let mut midi: OrderedMap<MidiBindingSource> = OrderedMap::new();
        // Which control is already taken, and under which key - for the conflict message.
        let mut taken: Vec<(String, String)> = Vec::new();

        for entry in entries {
            let Some(key) = entry.key.as_str() else {
                self.error(
                    Some(entry.key.pos),
                    format!(
                        "Keys in 'midi' must be text, found: {}.",
                        entry.key.describe()
                    ),
                );
                continue;
            };
            if midi.contains_key(key) {
                self.error_with(
                    Some(entry.key.pos),
                    format!("'{key}' appears more than once in 'midi'."),
                    "Each key may appear only once per mapping; the second one would silently overwrite the first.",
                );
                continue;
            }
            let Some(target) = self.read_midi_target(key, entry.key.pos, track_names, sections)
            else {
                continue;
            };
            let Some(binding) = self.read_midi_binding(key, &target, &entry.value) else {
                continue;
            };
            let id = binding.id();
            if let Some((_, other)) = taken.iter().find(|(used, _)| *used == id) {
                self.error_with(
                    Some(entry.key.pos),
                    format!("{id} is already used by '{other}'."),
                    format!(
                        "A button can only do one thing. Use a different note or \
                         controller for '{key}'."
                    ),
                );
                continue;
            }
            taken.push((id, key.to_string()));
            midi.insert(key, binding);
        }
        Some(midi)
    }

    /// The key of a `midi:` entry as an address, checked against this score.
    fn read_midi_target(
        &mut self,
        key: &str,
        pos: Pos,
        track_names: &[&str],
        sections: usize,
    ) -> Option<Target> {
        let target = match Target::parse(key) {
            Ok(target) => target,
            Err(message) => {
                self.error_with(
                    Some(pos),
                    format!("'{key}' in 'midi' is not a known target."),
                    message,
                );
                return None;
            }
        };
        match target.track() {
            Some(TrackRef::Name(name)) if !track_names.contains(&name.as_str()) => {
                self.error_with(
                    Some(pos),
                    format!("'{key}' binds Track '{name}', which is not in 'tracks'."),
                    suggest_or_list(name, track_names),
                );
                return None;
            }
            Some(TrackRef::Index(index)) if *index >= track_names.len() => {
                self.error_with(
                    Some(pos),
                    format!(
                        "'{key}' binds Track {}, but the score has {}.",
                        index + 1,
                        track_names.len()
                    ),
                    "In a binding, a track is addressed by its number (from 1) or by its \
                     name.",
                );
                return None;
            }
            _ => {}
        }
        if let Target::ScoreGoto(section) = &target
            && *section >= sections
        {
            self.error_with(
                Some(pos),
                format!(
                    "'{key}' jumps to Section {}, but the score has {sections}.",
                    section + 1
                ),
                "Sections are counted from 1, in the order they appear under 'sections'.",
            );
            return None;
        }
        Some(target)
    }

    fn read_midi_binding(
        &mut self,
        key: &str,
        target: &Target,
        node: &Node<'_>,
    ) -> Option<MidiBindingSource> {
        let what = format!("MIDI binding '{key}'");
        let entries = self.as_map(node, &what)?;
        let fields = self.index_fields(entries, &MIDI_BINDING_FIELDS, &[], &what);

        let channel = match fields.get("channel") {
            Some(node) => self.as_int(node, &format!("'channel' in {what}"), 1, 16)? as u8,
            None => 1,
        };
        let note = fields.get("note");
        let cc = fields.get("cc");
        let (kind, value_node) = match (note, cc) {
            (Some(note), None) => (MidiKind::Note, *note),
            (None, Some(cc)) => (MidiKind::Cc, *cc),
            (Some(_), Some(cc)) => {
                self.error_with(
                    Some(cc.pos),
                    format!("{what} gives both 'note' and 'cc'."),
                    "Give exactly one of the two.",
                );
                return None;
            }
            (None, None) => {
                self.error_with(
                    Some(node.pos),
                    format!("{what} gives neither 'note' nor 'cc'."),
                    "e.g. 'next_section: {cc: 64}' or 'track.voice.record: {note: 36}'.",
                );
                return None;
            }
        };
        let number = self.as_int(value_node, &format!("The number in {what}"), 0, 127)? as u8;

        let mode = match fields.get("mode") {
            Some(node) => {
                let text = self.as_str(node, &format!("'mode' in {what}"))?;
                match ButtonMode::parse(text) {
                    Some(mode) => Some(mode),
                    None => {
                        self.error_with(
                            Some(node.pos),
                            format!("'{text}' is not a button mode in {what}."),
                            suggest_or_list(text, &ButtonMode::names()),
                        );
                        return None;
                    }
                }
            }
            None => None,
        };
        let takeover = match fields.get("takeover") {
            Some(node) => {
                let text = self.as_str(node, &format!("'takeover' in {what}"))?;
                match Takeover::parse(text) {
                    Some(takeover) => Some(takeover),
                    None => {
                        self.error_with(
                            Some(node.pos),
                            format!("'{text}' is not a takeover mode in {what}."),
                            suggest_or_list(text, &Takeover::names()),
                        );
                        return None;
                    }
                }
            }
            None => None,
        };
        let min = match fields.get("min") {
            Some(node) => Some(self.as_number(node, &format!("'min' in {what}"))? as f32),
            None => None,
        };
        let max = match fields.get("max") {
            Some(node) => Some(self.as_number(node, &format!("'max' in {what}"))? as f32),
            None => None,
        };

        // The combination check belongs to the binding itself - "Aufnahme, aber umgeschaltet" is
        // nonsense whether it arrives from a score or from a controller profile.
        let mut binding = Binding::new(target.clone());
        if let Some(mode) = mode {
            binding.button = mode;
        }
        binding = binding.with_bounds(min, max);
        if let Err(message) = binding.check() {
            self.error_with(Some(node.pos), format!("{what}: {message}"), match target.control() {
                Control::Trigger => "Only 'press' and 'release' are allowed here.".to_string(),
                _ => allowed(&ButtonMode::names()),
            });
            return None;
        }

        Some(MidiBindingSource {
            channel,
            kind,
            number,
            target: target.to_string(),
            mode,
            takeover,
            min,
            max,
        })
    }

    fn read_sections(&mut self, node: &Node<'_>, track_names: &[&str]) -> Option<Vec<SectionSource>> {
        let items = self.as_seq(node, "'sections'")?;
        if items.is_empty() {
            self.error_with(
                Some(node.pos),
                "'sections' is empty; a score needs at least one section.",
                "A section needs at least 'id' and 'bars'.",
            );
            return None;
        }

        let mut sections: Vec<SectionSource> = Vec::new();
        let mut repeat_pos: Vec<Option<Pos>> = Vec::new();
        for item in items {
            if let Some((section, pos)) = self.read_section(item, track_names, &sections) {
                sections.push(section);
                repeat_pos.push(pos);
            }
        }
        self.check_repeats(&sections, &repeat_pos);
        Some(sections)
    }

    /// One section. Returns the section plus the position of its `repeat:` key, which the
    /// reference check below needs.
    fn read_section(
        &mut self,
        node: &Node<'_>,
        track_names: &[&str],
        previous: &[SectionSource],
    ) -> Option<(SectionSource, Option<Pos>)> {
        let entries = self.as_map(node, "A section")?;
        // Same rule as in `read_track`: a section whose field names are already being complained
        // about does not additionally get told that 'bars' is missing.
        let before = self.issues.len();
        let fields = self.index_fields(entries, &SECTION_FIELDS, &[], "a section");
        let had_field_issue = self.issues.len() > before;

        let Some(id_node) = fields.get("id") else {
            self.error_with(
                Some(node.pos),
                "Required field 'id' is missing in a section.",
                "Every section needs a unique 'id', e.g. 'id: intro'.",
            );
            return None;
        };
        let id = self.as_str(id_node, "The 'id' of a section")?;
        if !is_identifier(id) {
            self.error_with(
                Some(id_node.pos),
                format!("Invalid section id '{id}'."),
                "Use letters, digits, '_' and '-', starting with a letter or '_'.",
            );
            return None;
        }
        if previous.iter().any(|section| section.id == id) {
            self.error_with(
                Some(id_node.pos),
                format!("Section id '{id}' is used twice."),
                "Ids must be unique; otherwise 'repeat: <id>' could not tell which one is meant.",
            );
            return None;
        }

        let repeat_node = fields.get("repeat");
        let repeat = repeat_node
            .and_then(|node| self.as_str(node, &format!("'repeat' in Section '{id}'")))
            .map(str::to_string);
        let bars = fields
            .get("bars")
            .and_then(|node| self.as_int(node, &format!("'bars' in Section '{id}'"), 1, MAX_BARS));
        let autorelease = fields
            .get("autorelease")
            .and_then(|node| self.as_bool(node, &format!("'autorelease' in Section '{id}'")));
        let quantize = fields
            .get("quantize")
            .and_then(|node| self.read_quantize(id, node));

        // `bars` present but out of range is a range error, not a missing field - hence
        // `contains_key` rather than `bars.is_none()`. The section is kept either way: its id has
        // to stay visible so that a `repeat:` elsewhere is judged against the complete list.
        if !fields.contains_key("bars") && repeat_node.is_none() && !had_field_issue {
            self.error_with(
                Some(id_node.pos),
                format!("Section '{id}' has neither 'bars' nor 'repeat'."),
                "Give 'bars: <number of bars>', or repeat a section with 'repeat: <id>'.",
            );
        }

        let (tracks, grids, clips) = fields
            .get("tracks")
            .and_then(|node| self.read_section_tracks(id, node, track_names))
            .unwrap_or_default();

        Some((
            SectionSource {
                id: id.to_string(),
                repeat,
                bars,
                autorelease,
                quantize,
                tracks,
                grids,
                clips,
                source_line: Some(node.pos.line),
            },
            repeat_node.map(|node| node.pos),
        ))
    }

    fn read_quantize(&mut self, section: &str, node: &Node<'_>) -> Option<Quantize> {
        let text = self.as_str(node, &format!("'quantize' in Section '{section}'"))?;
        match text {
            "bar" => Some(Quantize::Bar),
            "loop" => Some(Quantize::Loop),
            _ => {
                self.error_with(
                    Some(node.pos),
                    format!("Unknown quantize value '{text}' in Section '{section}'."),
                    suggest_or_list(text, &QUANTIZE_NAMES),
                );
                None
            }
        }
    }

    fn read_section_tracks(
        &mut self,
        section: &str,
        node: &Node<'_>,
        track_names: &[&str],
    ) -> Option<(OrderedMap<TrackState>, OrderedMap<String>, OrderedMap<ClipOptions>)> {
        let entries = self.as_map(node, &format!("'tracks' in Section '{section}'"))?;
        let mut states = OrderedMap::new();
        let mut grids = OrderedMap::new();
        let mut clips = OrderedMap::new();
        let state_names = TrackState::names();
        for entry in entries {
            let Some(name) = entry.key.as_str() else {
                self.error(
                    Some(entry.key.pos),
                    format!("Track names must be text, found: {}.", entry.key.describe()),
                );
                continue;
            };
            if !track_names.contains(&name) {
                self.error_with(
                    Some(entry.key.pos),
                    format!("Section '{section}' refers to Track '{name}', which does not exist."),
                    suggest(name, track_names).unwrap_or_else(|| {
                        if track_names.is_empty() {
                            "No track is defined under 'tracks'.".to_string()
                        } else {
                            format!("Known tracks: {}.", track_names.join(", "))
                        }
                    }),
                );
                continue;
            }
            if states.contains_key(name) {
                self.error_with(
                    Some(entry.key.pos),
                    format!("Track '{name}' appears more than once in Section '{section}'."),
                    "A track has exactly one state per section.",
                );
                continue;
            }
            // `voice: record`, or - with a clip setting - `keys: {state: play, grid: 1/16}`.
            let mut clip = ClipOptions::default();
            let mut clip_nodes: Vec<(&str, Pos)> = Vec::new();
            let state_node = match &entry.value.kind {
                NodeKind::Map(fields) => {
                    let what = format!("Track '{name}' in Section '{section}'");
                    let fields = self.index_fields(fields, &CLIP_FIELDS, &[], &what);
                    if let Some(node) = fields.get("grid")
                        && let Some(grid) = self.read_grid(&format!("'grid' of {what}"), node)
                    {
                        grids.insert(name, grid);
                    }
                    if let Some(node) = fields.get("restart")
                        && let Some(on) = self.as_bool(node, &format!("'restart' of {what}"))
                    {
                        clip.restart = on;
                        clip_nodes.push(("restart", node.pos));
                    }
                    if let Some(node) = fields.get("monitor")
                        && let Some(on) = self.as_bool(node, &format!("'monitor' of {what}"))
                    {
                        clip.monitor = on;
                        clip_nodes.push(("monitor", node.pos));
                        self.clip_monitor_pos
                            .insert((section.to_string(), name.to_string()), node.pos);
                    }
                    if let Some(node) = fields.get("bars")
                        && let Some(bars) =
                            self.as_int(node, &format!("'bars' of {what}"), 1, MAX_BARS)
                    {
                        clip.bars = Some(bars);
                        clip_nodes.push(("bars", node.pos));
                        self.clip_bars_pos
                            .insert((section.to_string(), name.to_string()), node.pos);
                    }
                    match fields.get("state") {
                        Some(node) => *node,
                        None => {
                            self.error_with(
                                Some(entry.value.pos),
                                format!("{what} has no 'state'."),
                                format!(
                                    "With a clip setting the state is written out: \
                                     '{name}: {{state: play, grid: 1/16}}'."
                                ),
                            );
                            continue;
                        }
                    }
                }
                _ => &entry.value,
            };
            let Some(text) = state_node.as_str() else {
                self.error_with(
                    Some(state_node.pos),
                    format!(
                        "The state of Track '{name}' in Section '{section}' must be text, found: {}.",
                        state_node.describe()
                    ),
                    allowed(&state_names),
                );
                continue;
            };
            match TrackState::parse(text) {
                Some(state) => {
                    self.state_pos
                        .insert((section.to_string(), name.to_string()), state_node.pos);
                    self.check_clip(section, name, state, clip, &clip_nodes);
                    if !clip.is_empty() {
                        clips.insert(name, clip);
                    }
                    states.insert(name, state)
                }
                None => self.error_with(
                    Some(state_node.pos),
                    format!("Unknown state '{text}' for Track '{name}' in Section '{section}'."),
                    suggest_or_list(text, &state_names),
                ),
            }
        }
        Some((states, grids, clips))
    }

    /// The clip options have to fit the state they are written with. Both are rejected rather than
    /// ignored where they would do nothing: a score that says something the player does not do
    /// is worse than one that does not compile.
    fn check_clip(
        &mut self,
        section: &str,
        name: &str,
        state: TrackState,
        clip: ClipOptions,
        nodes: &[(&str, Pos)],
    ) {
        let pos = |field: &str| nodes.iter().find(|(f, _)| *f == field).map(|(_, p)| *p);
        if clip.restart {
            match state {
                TrackState::Play | TrackState::Overdub | TrackState::Stack => {}
                TrackState::Record => self.error_with(
                    pos("restart"),
                    format!(
                        "'restart' on record does nothing (Track '{name}' in Section '{section}'): \
                         the new take starts the loop at the section start anyway."
                    ),
                    "Remove 'restart'; it goes with play, overdub or stack.",
                ),
                TrackState::Stop | TrackState::HearThrough => self.error_with(
                    pos("restart"),
                    format!(
                        "'restart' needs a loop that plays, but Track '{name}' is on {} in \
                         Section '{section}'.",
                        state.as_str()
                    ),
                    "Remove 'restart'; it goes with play, overdub or stack.",
                ),
            }
        }
        if clip.monitor {
            let why = match state {
                TrackState::Play => None,
                TrackState::Record | TrackState::Overdub | TrackState::Stack => Some(format!(
                    "'monitor' on {} does nothing (Track '{name}' in Section '{section}'): a take \
                     always lets the input be heard.",
                    state.as_str()
                )),
                TrackState::HearThrough => Some(format!(
                    "'monitor' on hear_through does nothing (Track '{name}' in Section \
                     '{section}'): hear_through is the input alone."
                )),
                TrackState::Stop => Some(format!(
                    "'monitor' goes with a loop that plays, but Track '{name}' is on stop in \
                     Section '{section}'."
                )),
            };
            if let Some(why) = why {
                self.error_with(
                    pos("monitor"),
                    why,
                    "'monitor: true' goes with play: the loop plays and the input is heard on top. \
                     For the input alone, use hear_through.",
                );
            }
        }
        if clip.bars.is_some() && state != TrackState::Record {
            let why = match state {
                TrackState::Overdub => "an overdub always lasts one pass of the loop",
                TrackState::Stack => "a stack records one pass after the other until the section ends",
                _ => "it is the length of a take",
            };
            self.error_with(
                pos("bars"),
                format!(
                    "'bars' of a clip only goes with record (Track '{name}' is on {} in \
                     Section '{section}'): {why}.",
                    state.as_str()
                ),
                format!("'{name}: {{state: record, bars: 2}}' records two bars, then plays them as a loop."),
            );
        }
    }

    /// `repeat:` must name an existing section, and the graph it spans must be acyclic.
    ///
    /// Both checks happen here, with the position of the `repeat:` key in hand, so the resolver in
    /// `compile.rs` can be a plain recursion that cannot hang: by the time it runs, every chain is
    /// known to terminate.
    fn check_repeats(&mut self, sections: &[SectionSource], repeat_pos: &[Option<Pos>]) {
        let ids: Vec<&str> = sections.iter().map(|section| section.id.as_str()).collect();
        let index: HashMap<&str, usize> = ids.iter().enumerate().map(|(i, id)| (*id, i)).collect();

        // Unknown targets first; a chain through an unknown target cannot be a cycle.
        let mut valid_target: Vec<Option<usize>> = vec![None; sections.len()];
        for (i, section) in sections.iter().enumerate() {
            let Some(target) = &section.repeat else { continue };
            match index.get(target.as_str()) {
                Some(&j) => valid_target[i] = Some(j),
                None => self.error_with(
                    repeat_pos[i],
                    format!(
                        "Section '{}' repeats the unknown section '{target}'.",
                        section.id
                    ),
                    suggest(target, &ids).unwrap_or_else(|| {
                        format!("Known sections: {}.", ids.join(", "))
                    }),
                ),
            }
        }

        // Walk each chain with a step budget of one hop per section. A chain that has not ended by
        // then is walking in a circle - no visited set, no recursion, no way to hang.
        for start in 0..sections.len() {
            if valid_target[start].is_none() {
                continue;
            }
            let mut node = start;
            let mut seen_self = false;
            for _ in 0..=sections.len() {
                match valid_target[node] {
                    Some(next) => {
                        if next == start {
                            seen_self = true;
                            break;
                        }
                        node = next;
                    }
                    None => break,
                }
            }
            if seen_self {
                self.error_with(
                    repeat_pos[start],
                    format!(
                        "Section '{}': 'repeat: {}' is circular.",
                        sections[start].id,
                        sections[start].repeat.as_deref().unwrap_or("?")
                    ),
                    "A section cannot repeat itself, not even through other sections.",
                );
            }
        }
    }
}

/// What `input:` says.
enum InputSpec {
    /// 1-based left (or only) channel, and the right one of a stereo pair.
    Channels(u32, Option<u32>),
    Instrument,
}

fn required_hint(field: &str) -> &'static str {
    match field {
        "bpm" => "e.g. 'bpm: 120'.",
        "tracks" => "e.g. 'tracks:' with 'guitar: {input: 2}' below it.",
        _ => "The sections go as a list under 'sections:'.",
    }
}

fn split_time_signature(text: &str) -> Option<(u32, u32)> {
    let (numerator, denominator) = text.split_once('/')?;
    if numerator.is_empty()
        || denominator.is_empty()
        || numerator.len() > 2
        || denominator.len() > 2
        || !numerator.bytes().all(|b| b.is_ascii_digit())
        || !denominator.bytes().all(|b| b.is_ascii_digit())
    {
        return None;
    }
    Some((numerator.parse().ok()?, denominator.parse().ok()?))
}

/// `^[A-Za-z_][A-Za-z0-9_-]*$` - the identifier rule the old schema used, unchanged.
fn is_identifier(text: &str) -> bool {
    let mut chars = text.chars();
    match chars.next() {
        Some(first) if first.is_ascii_alphabetic() || first == '_' => {}
        _ => return false,
    }
    chars.all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
}
