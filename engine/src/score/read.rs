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
    MidiBindingSource, MidiKind, ScoreSource, SectionSource, TrackSource, TrackState,
};
use super::yaml::{Entry, Node, NodeKind, Pos, parse_document};
use crate::engine::command::MAX_TRACKS;
use crate::engine::schedule::Quantize;

const ROOT_FIELDS: [&str; 7] = [
    "title",
    "bpm",
    "time_signature",
    "beats_per_bar",
    "tracks",
    "midi",
    "sections",
];
const TRACK_FIELDS: [&str; 3] = ["input", "pan", "monitor"];
const SECTION_FIELDS: [&str; 6] = ["id", "repeat", "bars", "autorelease", "quantize", "tracks"];
const MIDI_FIELDS: [&str; 2] = ["next_section", "stop_all"];
const MIDI_BINDING_FIELDS: [&str; 3] = ["note", "cc", "channel"];
const QUANTIZE_NAMES: [&str; 2] = ["bar", "loop"];

/// Fields the Ableton-era format had. They are not "unknown" - they are *gone*, and saying so is
/// far more useful than "meintest du 'input'?".
const RETIRED_TRACK_FIELDS: [(&str, &str); 5] = [
    (
        "ableton_track",
        "Die Engine steuert kein Ableton mehr. Ein Track nennt seinen Eingangskanal: 'input: <1-basierter Kanal>'.",
    ),
    (
        "group",
        "Gruppenspuren gibt es nicht mehr - die Engine stapelt Ebenen selbst. Ersetze den Track durch 'input: <Kanal>'.",
    ),
    (
        "layers",
        "Die Zahl der Ebenen wird nicht mehr vorab reserviert; 'overdub' legt beliebig viele an.",
    ),
    (
        "reserve",
        "Reserve-Spuren gibt es nicht mehr; die Engine legt Ebenen zur Laufzeit an.",
    ),
    (
        "type",
        "Es gibt nur noch eine Art Track. 'type' entfernen und 'input: <Kanal>' angeben.",
    ),
];

/// Bounds. Tempo mirrors what [`crate::engine::timeline::Timeline::validate`] accepts, so a score
/// that compiles can also be started; the rest is what an interface and a song plausibly have.
const BPM_MIN: f64 = 20.0;
const BPM_MAX: f64 = 400.0;
const MAX_INPUT_CHANNEL: u32 = 64;
const MAX_BEATS_PER_BAR: u32 = 16;
const MAX_BARS: u32 = 999;
const BEAT_UNITS: [u32; 6] = [1, 2, 4, 8, 16, 32];

/// Parse and validate, without resolving `repeat:`. [`super::compile_score`] is this plus
/// `compile.rs`.
pub fn read_score(yaml_text: &str) -> Result<ScoreSource, ScoreError> {
    let document = parse_document(yaml_text)?;
    let mut reader = Reader::default();
    let score = reader.read_root(&document);
    if reader.issues.is_empty() {
        // Only a clean read produces a score; anything else would hand the compiler half a file.
        score.ok_or_else(|| {
            ScoreError::single(ScoreIssue::new(None, "Die Partitur konnte nicht gelesen werden."))
        })
    } else {
        Err(ScoreError::new(reader.issues))
    }
}

#[derive(Default)]
struct Reader {
    issues: Vec<ScoreIssue>,
}

impl Reader {
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
                        "{what} muss ein Mapping (Schluessel: Wert) sein, gefunden: {}.",
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
                    format!("{what} muss eine Liste sein, gefunden: {}.", node.describe()),
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
                    format!("{what} muss Text sein, gefunden: {}.", node.describe()),
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
                    format!("{what} muss true oder false sein, gefunden: {}.", node.describe()),
                )
                .with_suggestion("Schreibe true oder false (ohne Anfuehrungszeichen; 'yes'/'no' gelten nicht).");
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
                        "{what} muss eine ganze Zahl sein, gefunden: {}.",
                        node.describe()
                    ),
                );
                return None;
            }
        };
        if value < i64::from(min) || value > i64::from(max) {
            self.error_with(
                Some(node.pos),
                format!("{what} muss zwischen {min} und {max} liegen, gefunden: {value}."),
                format!("Erlaubt sind {min} bis {max}."),
            );
            return None;
        }
        Some(value as u32)
    }

    fn as_number(&mut self, node: &Node<'_>, what: &str) -> Option<f64> {
        match &node.kind {
            NodeKind::Int(value) => Some(*value as f64),
            NodeKind::Float(value) => Some(*value),
            _ => {
                self.error(
                    Some(node.pos),
                    format!("{what} muss eine Zahl sein, gefunden: {}.", node.describe()),
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
                    format!("Schluessel in {what} muessen Text sein, gefunden: {}.", entry.key.describe()),
                );
                continue;
            };
            if let Some((_, hint)) = retired.iter().find(|(field, _)| *field == name) {
                self.error_with(
                    Some(entry.key.pos),
                    format!("{what}: das Feld '{name}' gibt es nicht mehr."),
                    *hint,
                );
                continue;
            }
            if !known.contains(&name) {
                let message = format!("Unbekanntes Feld '{name}' in {what}.");
                self.error_with(Some(entry.key.pos), message, suggest_or_list(name, known));
                continue;
            }
            if found.contains_key(name) {
                self.error_with(
                    Some(entry.key.pos),
                    format!("Feld '{name}' kommt in {what} mehrfach vor."),
                    "Jeder Schluessel darf pro Mapping nur einmal stehen; der zweite wuerde den ersten still ueberschreiben.",
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
                    "Die Partitur muss ein Mapping mit 'bpm', 'tracks' und 'sections' sein.",
                    "Vorlage: examples/henry-3-4.rust.yaml",
                );
                return None;
            }
        };
        let fields = self.index_fields(entries, &ROOT_FIELDS, &[], "der Partitur");

        for required in ["bpm", "tracks", "sections"] {
            if !fields.contains_key(required) {
                self.error_with(
                    Some(document.pos),
                    format!("Pflichtfeld '{required}' fehlt in der Partitur."),
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
                    format!("'bpm' muss zwischen {BPM_MIN} und {BPM_MAX} liegen, gefunden: {value}."),
                    "Die Engine spielt Tempi von 20 bis 400 BPM.",
                );
                return None;
            }
            Some(value)
        });

        let (time_signature, beats_per_bar) = self.read_time_signature(&fields);
        let tracks = fields
            .get("tracks")
            .and_then(|node| self.read_tracks(node))
            .unwrap_or_default();
        let midi = fields
            .get("midi")
            .and_then(|node| self.read_midi(node))
            .unwrap_or_default();
        let track_names: Vec<&str> = tracks.keys().collect();
        let sections = fields
            .get("sections")
            .and_then(|node| self.read_sections(node, &track_names))
            .unwrap_or_default();

        Some(ScoreSource {
            title,
            bpm: bpm?,
            time_signature,
            beats_per_bar,
            tracks,
            midi,
            sections,
        })
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
                format!("Ungueltige Taktart '{text}'."),
                "Schreibe 'Zaehler/Nenner', z. B. time_signature: 3/4 oder \"7/8\".",
            );
            return (None, beats_per_bar);
        };
        if !(1..=MAX_BEATS_PER_BAR).contains(&numerator) {
            self.error_with(
                Some(node.pos),
                format!("Taktart '{text}': der Zaehler muss zwischen 1 und {MAX_BEATS_PER_BAR} liegen."),
                "z. B. 3/4, 4/4, 6/8, 7/8.",
            );
            return (None, beats_per_bar);
        }
        if !BEAT_UNITS.contains(&denominator) {
            self.error_with(
                Some(node.pos),
                format!("Taktart '{text}': der Nenner muss eine Zweierpotenz sein."),
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
                format!("'beats_per_bar: {alias}' widerspricht 'time_signature: {text}'."),
                "Nur eines von beiden angeben (bevorzugt time_signature), oder beats_per_bar auf den Zaehler setzen.",
            );
        }
        (Some(text.to_string()), beats_per_bar)
    }

    fn read_tracks(&mut self, node: &Node<'_>) -> Option<OrderedMap<TrackSource>> {
        let entries = self.as_map(node, "'tracks'")?;
        if entries.is_empty() {
            self.error_with(
                Some(node.pos),
                "'tracks' ist leer; eine Partitur braucht mindestens einen Track.",
                "z. B. 'gitarre: {input: 2}'.",
            );
            return None;
        }

        let mut tracks = OrderedMap::new();
        for entry in entries {
            let Some(name) = entry.key.as_str() else {
                self.error(
                    Some(entry.key.pos),
                    format!("Track-Namen muessen Text sein, gefunden: {}.", entry.key.describe()),
                );
                continue;
            };
            if !is_identifier(name) {
                self.error_with(
                    Some(entry.key.pos),
                    format!("Ungueltiger Track-Name '{name}'."),
                    "Erlaubt sind Buchstaben, Ziffern, '_' und '-', beginnend mit einem Buchstaben oder '_'.",
                );
                continue;
            }
            if tracks.contains_key(name) {
                self.error_with(
                    Some(entry.key.pos),
                    format!("Track '{name}' ist doppelt definiert."),
                    "Jeder Track darf nur einmal unter 'tracks' stehen.",
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
                    "Die Partitur hat {} Tracks; die Engine fuehrt hoechstens {MAX_TRACKS}.",
                    tracks.len()
                ),
                format!("Hoechstens {MAX_TRACKS} Tracks unter 'tracks' auffuehren."),
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
                        format!("Track '{name}' hat kein 'input'."),
                        "Ein Track nennt seinen Eingangskanal am Interface, 1-basiert: 'input: 1' \
                         fuer mono, 'input: [3, 4]' fuer stereo.",
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
            Some(node) => self.as_bool(node, &format!("'monitor' von Track '{name}'")),
            None => Some(true),
        };
        let (input, input_right) = input?;
        Some(TrackSource {
            input,
            input_right,
            pan: pan?,
            monitor: monitor?,
        })
    }

    /// `input: 1` for a mono track, `input: [3, 4]` for a stereo one.
    ///
    /// A list of exactly two entries and nothing else: one entry is a mono track written oddly, and
    /// three would be a surround format the engine does not have. Both get their own sentence
    /// rather than being quietly truncated.
    fn read_track_input(&mut self, name: &str, node: &Node<'_>) -> Option<(u32, Option<u32>)> {
        let what = format!("'input' von Track '{name}'");
        match &node.kind {
            NodeKind::Seq(items) => {
                if items.len() != 2 {
                    self.error_with(
                        Some(node.pos),
                        format!(
                            "{what} ist eine Liste mit {} Eintraegen; ein Stereo-Track nennt genau zwei Eingaenge.",
                            items.len()
                        ),
                        "z. B. 'input: [3, 4]' fuer stereo oder 'input: 3' fuer mono.",
                    );
                    return None;
                }
                let left = self.as_int(&items[0], &format!("der linke Eingang in {what}"), 1, MAX_INPUT_CHANNEL);
                let right = self.as_int(&items[1], &format!("der rechte Eingang in {what}"), 1, MAX_INPUT_CHANNEL);
                let (left, right) = (left?, right?);
                if left == right {
                    self.error_with(
                        Some(node.pos),
                        format!("{what} nennt zweimal Eingang {left}."),
                        format!(
                            "Ein Stereo-Track braucht zwei verschiedene Eingaenge; fuer einen \
                             Mono-Track reicht 'input: {left}'."
                        ),
                    );
                    return None;
                }
                Some((left, Some(right)))
            }
            _ => Some((self.as_int(node, &what, 1, MAX_INPUT_CHANNEL)?, None)),
        }
    }

    /// `pan: -0.4`. A number, and inside the range the engine's panner actually has.
    fn read_pan(&mut self, name: &str, node: &Node<'_>) -> Option<f32> {
        let what = format!("'pan' von Track '{name}'");
        let value = self.as_number(node, &what)?;
        if !(-1.0..=1.0).contains(&value) {
            self.error_with(
                Some(node.pos),
                format!("{what} muss zwischen -1 und 1 liegen, gefunden: {value}."),
                "-1 ist ganz links, 0 die Mitte, 1 ganz rechts.",
            );
            return None;
        }
        Some(value as f32)
    }

    fn read_midi(&mut self, node: &Node<'_>) -> Option<OrderedMap<MidiBindingSource>> {
        let entries = self.as_map(node, "'midi'")?;
        let fields = self.index_fields(entries, &MIDI_FIELDS, &[], "'midi'");
        let mut midi = OrderedMap::new();
        // Iterate over MIDI_FIELDS rather than the map so the order is stable regardless of how the
        // file is written.
        for action in MIDI_FIELDS {
            let Some(node) = fields.get(action) else { continue };
            if let Some(binding) = self.read_midi_binding(action, node) {
                midi.insert(action, binding);
            }
        }
        Some(midi)
    }

    fn read_midi_binding(&mut self, action: &str, node: &Node<'_>) -> Option<MidiBindingSource> {
        let what = format!("MIDI-Bindung '{action}'");
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
                    format!("{what} nennt 'note' und 'cc' gleichzeitig."),
                    "Genau eines von beiden angeben.",
                );
                return None;
            }
            (None, None) => {
                self.error_with(
                    Some(node.pos),
                    format!("{what} nennt weder 'note' noch 'cc'."),
                    "z. B. 'next_section: {cc: 64}' oder 'stop_all: {note: 37}'.",
                );
                return None;
            }
        };
        let number = self.as_int(value_node, &format!("Nummer in {what}"), 0, 127)? as u8;
        Some(MidiBindingSource { channel, kind, number })
    }

    fn read_sections(&mut self, node: &Node<'_>, track_names: &[&str]) -> Option<Vec<SectionSource>> {
        let items = self.as_seq(node, "'sections'")?;
        if items.is_empty() {
            self.error_with(
                Some(node.pos),
                "'sections' ist leer; eine Partitur braucht mindestens eine Sektion.",
                "Eine Sektion besteht mindestens aus 'id' und 'bars'.",
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
        let entries = self.as_map(node, "Eine Sektion")?;
        // Same rule as in `read_track`: a section whose field names are already being complained
        // about does not additionally get told that 'bars' is missing.
        let before = self.issues.len();
        let fields = self.index_fields(entries, &SECTION_FIELDS, &[], "einer Sektion");
        let had_field_issue = self.issues.len() > before;

        let Some(id_node) = fields.get("id") else {
            self.error_with(
                Some(node.pos),
                "Pflichtfeld 'id' fehlt in einer Sektion.",
                "Jede Sektion braucht eine eindeutige 'id', z. B. 'id: intro'.",
            );
            return None;
        };
        let id = self.as_str(id_node, "'id' einer Sektion")?;
        if !is_identifier(id) {
            self.error_with(
                Some(id_node.pos),
                format!("Ungueltige Sektions-ID '{id}'."),
                "Erlaubt sind Buchstaben, Ziffern, '_' und '-', beginnend mit einem Buchstaben oder '_'.",
            );
            return None;
        }
        if previous.iter().any(|section| section.id == id) {
            self.error_with(
                Some(id_node.pos),
                format!("Sektions-ID '{id}' ist doppelt vergeben."),
                "IDs muessen eindeutig sein; 'repeat: <id>' koennte sonst nicht sagen, welche gemeint ist.",
            );
            return None;
        }

        let repeat_node = fields.get("repeat");
        let repeat = repeat_node
            .and_then(|node| self.as_str(node, &format!("'repeat' in Sektion '{id}'")))
            .map(str::to_string);
        let bars = fields
            .get("bars")
            .and_then(|node| self.as_int(node, &format!("'bars' in Sektion '{id}'"), 1, MAX_BARS));
        let autorelease = fields
            .get("autorelease")
            .and_then(|node| self.as_bool(node, &format!("'autorelease' in Sektion '{id}'")));
        let quantize = fields
            .get("quantize")
            .and_then(|node| self.read_quantize(id, node));

        // `bars` present but out of range is a range error, not a missing field - hence
        // `contains_key` rather than `bars.is_none()`. The section is kept either way: its id has
        // to stay visible so that a `repeat:` elsewhere is judged against the complete list.
        if !fields.contains_key("bars") && repeat_node.is_none() && !had_field_issue {
            self.error_with(
                Some(id_node.pos),
                format!("Sektion '{id}' hat weder 'bars' noch 'repeat'."),
                "Gib 'bars: <Anzahl Takte>' an, oder wiederhole eine Sektion mit 'repeat: <id>'.",
            );
        }

        let tracks = fields
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
                source_line: Some(node.pos.line),
            },
            repeat_node.map(|node| node.pos),
        ))
    }

    fn read_quantize(&mut self, section: &str, node: &Node<'_>) -> Option<Quantize> {
        let text = self.as_str(node, &format!("'quantize' in Sektion '{section}'"))?;
        match text {
            "bar" => Some(Quantize::Bar),
            "loop" => Some(Quantize::Loop),
            _ => {
                self.error_with(
                    Some(node.pos),
                    format!("Unbekannte Quantisierung '{text}' in Sektion '{section}'."),
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
    ) -> Option<OrderedMap<TrackState>> {
        let entries = self.as_map(node, &format!("'tracks' in Sektion '{section}'"))?;
        let mut states = OrderedMap::new();
        let state_names = TrackState::names();
        for entry in entries {
            let Some(name) = entry.key.as_str() else {
                self.error(
                    Some(entry.key.pos),
                    format!("Track-Namen muessen Text sein, gefunden: {}.", entry.key.describe()),
                );
                continue;
            };
            if !track_names.contains(&name) {
                self.error_with(
                    Some(entry.key.pos),
                    format!("Sektion '{section}' referenziert Track '{name}', den es nicht gibt."),
                    suggest(name, track_names).unwrap_or_else(|| {
                        if track_names.is_empty() {
                            "Unter 'tracks' ist kein Track definiert.".to_string()
                        } else {
                            format!("Bekannte Tracks: {}.", track_names.join(", "))
                        }
                    }),
                );
                continue;
            }
            if states.contains_key(name) {
                self.error_with(
                    Some(entry.key.pos),
                    format!("Track '{name}' kommt in Sektion '{section}' mehrfach vor."),
                    "Ein Track hat pro Sektion genau einen Zustand.",
                );
                continue;
            }
            let Some(text) = entry.value.as_str() else {
                self.error_with(
                    Some(entry.value.pos),
                    format!(
                        "Zustand von Track '{name}' in Sektion '{section}' muss Text sein, gefunden: {}.",
                        entry.value.describe()
                    ),
                    allowed(&state_names),
                );
                continue;
            };
            match TrackState::parse(text) {
                Some(state) => states.insert(name, state),
                None => self.error_with(
                    Some(entry.value.pos),
                    format!("Unbekannter Zustand '{text}' fuer Track '{name}' in Sektion '{section}'."),
                    suggest_or_list(text, &state_names),
                ),
            }
        }
        Some(states)
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
                        "Sektion '{}' wiederholt die unbekannte Sektion '{target}'.",
                        section.id
                    ),
                    suggest(target, &ids).unwrap_or_else(|| {
                        format!("Bekannte Sektionen: {}.", ids.join(", "))
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
                        "Sektion '{}': 'repeat: {}' ist zirkulaer.",
                        sections[start].id,
                        sections[start].repeat.as_deref().unwrap_or("?")
                    ),
                    "Eine Sektion kann sich nicht selbst wiederholen, auch nicht ueber Umwege.",
                );
            }
        }
    }
}

fn required_hint(field: &str) -> &'static str {
    match field {
        "bpm" => "z. B. 'bpm: 120'.",
        "tracks" => "z. B. 'tracks:' mit 'gitarre: {input: 2}' darunter.",
        _ => "Die Sektionen stehen als Liste unter 'sections:'.",
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
