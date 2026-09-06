//! A YAML document tree that remembers where every node came from.
//!
//! # Why not `serde_yaml`
//!
//! The score compiler's headline feature is its error messages, and an error message without a
//! line number is close to useless in a file a musician edits by hand. Deriving `Deserialize` for
//! the score would give neither: `serde_yaml` (0.9, deprecated) and its fork `serde_yml` (0.0.13,
//! now an unmaintained shim) both stop at the *first* problem, and their position information is
//! whatever the underlying libyaml happened to have on the stack - frequently the end of the
//! containing block rather than the offending key.
//!
//! So the input is parsed here into an explicit tree instead. Every node - **including every
//! mapping key** - carries the exact [`Pos`] the parser reported for it, and the reader in
//! `read.rs` walks that tree collecting as many problems as it can find. `serde` is still used,
//! but for the *output*: the compiled score is a serialisable contract.
//!
//! # Why the event stream and not `saphyr::MarkedYaml`
//!
//! `saphyr` builds a marked tree already, but stores mappings in a hash map. Two consequences make
//! it unusable here: a duplicated key silently overwrites its predecessor (a duplicate `bars:` in a
//! section would be swallowed instead of reported), and the key nodes are compared by value, so
//! their spans are not addressable. Consuming the event stream directly is about a hundred lines
//! and gives ordered entries, per-key spans and duplicate detection.
//!
//! This is control-thread code. It allocates freely; nothing here ever runs in an audio callback.

use std::borrow::Cow;
use std::fmt;

use saphyr_parser::{Event, Marker, Parser, ScalarStyle, ScanError, Span};

use super::error::{ScoreError, ScoreIssue};

/// A 1-based position in the source text.
///
/// Both fields come straight from the parser's [`Marker`], which counts lines and columns from 1
/// and 0 respectively; the column is shifted here so that "line 7, column 3" means what a text
/// editor shows.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct Pos {
    pub line: u32,
    pub column: u32,
}

impl Pos {
    pub fn from_marker(marker: Marker) -> Self {
        Self {
            line: marker.line() as u32,
            // saphyr's column is 0-based even though the field comment says otherwise; the unit
            // test `column_is_reported_one_based` pins this down.
            column: marker.col() as u32 + 1,
        }
    }
}

impl fmt::Display for Pos {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Zeile {}, Spalte {}", self.line, self.column)
    }
}

/// One `key: value` pair of a mapping. The key keeps its own position, which is where an
/// "unknown field" or "wrong type" message points.
#[derive(Debug)]
pub struct Entry<'a> {
    pub key: Node<'a>,
    pub value: Node<'a>,
}

#[derive(Debug)]
pub enum NodeKind<'a> {
    Null,
    Bool(bool),
    Int(i64),
    Float(f64),
    Str(Cow<'a, str>),
    Seq(Vec<Node<'a>>),
    /// Entries in document order, duplicates included - `read.rs` reports them.
    Map(Vec<Entry<'a>>),
}

#[derive(Debug)]
pub struct Node<'a> {
    pub pos: Pos,
    pub kind: NodeKind<'a>,
}

impl<'a> Node<'a> {
    /// German name of this node's type, for "... muss X sein, gefunden: Y" messages.
    pub fn type_name(&self) -> &'static str {
        match self.kind {
            NodeKind::Null => "null",
            NodeKind::Bool(_) => "true oder false",
            NodeKind::Int(_) => "eine ganze Zahl",
            NodeKind::Float(_) => "eine Zahl",
            NodeKind::Str(_) => "Text",
            NodeKind::Seq(_) => "eine Liste",
            NodeKind::Map(_) => "ein Mapping (Schluessel: Wert)",
        }
    }

    /// Short rendering of the value for an error message.
    pub fn describe(&self) -> String {
        match &self.kind {
            NodeKind::Null => "null".to_string(),
            NodeKind::Bool(b) => b.to_string(),
            NodeKind::Int(i) => i.to_string(),
            NodeKind::Float(f) => f.to_string(),
            NodeKind::Str(s) => {
                let mut text: String = s.chars().take(40).collect();
                if s.chars().count() > 40 {
                    text.push('…');
                }
                format!("'{text}'")
            }
            NodeKind::Seq(_) => "eine Liste".to_string(),
            NodeKind::Map(_) => "ein Mapping".to_string(),
        }
    }

    pub fn as_str(&self) -> Option<&str> {
        match &self.kind {
            NodeKind::Str(s) => Some(s.as_ref()),
            _ => None,
        }
    }
}

/// Parse one YAML document into a marked tree.
///
/// Syntax errors, an empty file and multi-document streams all come back as a [`ScoreError`] with
/// one German issue; everything else is left to the reader.
pub fn parse_document(text: &str) -> Result<Node<'_>, ScoreError> {
    let mut builder = Builder::default();
    for event in Parser::new_from_str(text) {
        let (event, span) = match event {
            Ok(pair) => pair,
            Err(err) => return Err(ScoreError::single(syntax_issue(&err))),
        };
        if let Err(issue) = builder.push(event, span) {
            return Err(ScoreError::single(issue));
        }
    }
    match builder.documents.len() {
        0 => Err(ScoreError::single(ScoreIssue::new(
            Some(Pos { line: 1, column: 1 }),
            "Die Partitur ist leer.",
        )
        .with_suggestion(
            "Eine Partitur braucht mindestens 'bpm', 'tracks' und 'sections'. \
             Vorlage: examples/henry-3-4.rust.yaml",
        ))),
        1 => Ok(builder.documents.pop().expect("genau ein Dokument")),
        _ => {
            let second = &builder.documents[1];
            Err(ScoreError::single(
                ScoreIssue::new(
                    Some(second.pos),
                    "Die Datei enthaelt mehrere YAML-Dokumente; eine Partitur ist genau eines.",
                )
                .with_suggestion("Den Trenner '---' entfernen oder die Dokumente auf zwei Dateien verteilen."),
            ))
        }
    }
}

fn syntax_issue(err: &ScanError) -> ScoreIssue {
    let pos = Pos::from_marker(*err.marker());
    let info = err.info();
    let lower = info.to_ascii_lowercase();
    let suggestion = if lower.contains("tab") {
        "Zum Einruecken Leerzeichen statt Tabs verwenden."
    } else if lower.contains("mapping") || lower.contains("could not find expected") {
        "Einrueckung und Doppelpunkte pruefen (nach 'schluessel:' gehoert ein Leerzeichen und ein Wert)."
    } else {
        "Einrueckung, Anfuehrungszeichen und Doppelpunkte in dieser Zeile pruefen."
    };
    ScoreIssue::new(Some(pos), format!("YAML-Syntaxfehler: {info}")).with_suggestion(suggestion)
}

// --------------------------------------------------------------------------------------------
// Event stream -> tree
// --------------------------------------------------------------------------------------------

#[derive(Default)]
struct Builder<'a> {
    stack: Vec<Frame<'a>>,
    documents: Vec<Node<'a>>,
}

enum Frame<'a> {
    Seq { pos: Pos, items: Vec<Node<'a>> },
    Map { pos: Pos, entries: Vec<Entry<'a>>, key: Option<Node<'a>> },
}

impl<'a> Builder<'a> {
    fn push(&mut self, event: Event<'a>, span: Span) -> Result<(), ScoreIssue> {
        let pos = Pos::from_marker(span.start);
        match event {
            Event::Nothing
            | Event::StreamStart
            | Event::StreamEnd
            | Event::DocumentStart(_)
            | Event::DocumentEnd => Ok(()),
            Event::SequenceStart(..) => {
                self.stack.push(Frame::Seq { pos, items: Vec::new() });
                Ok(())
            }
            Event::MappingStart(..) => {
                self.stack.push(Frame::Map { pos, entries: Vec::new(), key: None });
                Ok(())
            }
            Event::SequenceEnd | Event::MappingEnd => {
                let node = match self.stack.pop() {
                    Some(Frame::Seq { pos, items }) => Node { pos, kind: NodeKind::Seq(items) },
                    Some(Frame::Map { pos, entries, .. }) => {
                        Node { pos, kind: NodeKind::Map(entries) }
                    }
                    None => return Ok(()),
                };
                self.add(node)
            }
            Event::Scalar(value, style, _, _) => self.add(Node { pos, kind: scalar_kind(value, style) }),
            Event::Alias(_) => Err(ScoreIssue::new(
                Some(pos),
                "YAML-Anker und -Referenzen (&name / *name) werden in Partituren nicht unterstuetzt.",
            )
            .with_suggestion(
                "Eine Sektion wiederholt man mit 'repeat: <id>', nicht mit einer YAML-Referenz.",
            )),
        }
    }

    fn add(&mut self, node: Node<'a>) -> Result<(), ScoreIssue> {
        match self.stack.last_mut() {
            None => {
                self.documents.push(node);
                Ok(())
            }
            Some(Frame::Seq { items, .. }) => {
                items.push(node);
                Ok(())
            }
            Some(Frame::Map { entries, key, .. }) => match key.take() {
                None => {
                    if matches!(node.kind, NodeKind::Seq(_) | NodeKind::Map(_)) {
                        return Err(ScoreIssue::new(
                            Some(node.pos),
                            "Zusammengesetzte Schluessel (Listen oder Mappings als Schluessel) sind in einer Partitur nicht erlaubt.",
                        )
                        .with_suggestion("Als Schluessel nur einfache Namen verwenden."));
                    }
                    *key = Some(node);
                    Ok(())
                }
                Some(k) => {
                    entries.push(Entry { key: k, value: node });
                    Ok(())
                }
            },
        }
    }
}

/// Resolve a scalar the way the YAML 1.2 core schema does.
///
/// Only *plain* scalars resolve to null / bool / number; anything quoted or in a block stays text,
/// so `bpm: "141"` is a type error rather than a silent success.
fn scalar_kind(value: Cow<'_, str>, style: ScalarStyle) -> NodeKind<'_> {
    if style != ScalarStyle::Plain {
        return NodeKind::Str(value);
    }
    let text = value.as_ref();
    match text {
        "" | "~" | "null" | "Null" | "NULL" => return NodeKind::Null,
        "true" | "True" | "TRUE" => return NodeKind::Bool(true),
        "false" | "False" | "FALSE" => return NodeKind::Bool(false),
        _ => {}
    }
    if let Some(int) = parse_int(text) {
        return NodeKind::Int(int);
    }
    if let Some(float) = parse_float(text) {
        return NodeKind::Float(float);
    }
    NodeKind::Str(value)
}

fn parse_int(text: &str) -> Option<i64> {
    let (sign, digits) = match text.strip_prefix('-') {
        Some(rest) => (-1i64, rest),
        None => (1i64, text.strip_prefix('+').unwrap_or(text)),
    };
    let magnitude = if let Some(hex) = digits.strip_prefix("0x") {
        i64::from_str_radix(hex, 16).ok()?
    } else if let Some(oct) = digits.strip_prefix("0o") {
        i64::from_str_radix(oct, 8).ok()?
    } else {
        if digits.is_empty() || !digits.bytes().all(|b| b.is_ascii_digit()) {
            return None;
        }
        digits.parse::<i64>().ok()?
    };
    Some(sign * magnitude)
}

fn parse_float(text: &str) -> Option<f64> {
    match text {
        ".inf" | ".Inf" | ".INF" | "+.inf" => return Some(f64::INFINITY),
        "-.inf" | "-.Inf" | "-.INF" => return Some(f64::NEG_INFINITY),
        ".nan" | ".NaN" | ".NAN" => return Some(f64::NAN),
        _ => {}
    }
    // Rust's float parser is more permissive than YAML's (it accepts "inf", "NaN", "1e5" without a
    // dot). Requiring a digit and a '.' or exponent keeps plain words like `nan` as text.
    if !text.bytes().any(|b| b.is_ascii_digit()) {
        return None;
    }
    if !text.contains('.') && !text.contains('e') && !text.contains('E') {
        return None;
    }
    text.parse::<f64>().ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn map<'a>(node: &'a Node<'a>) -> &'a [Entry<'a>] {
        match &node.kind {
            NodeKind::Map(entries) => entries,
            other => panic!("Mapping erwartet, gefunden {other:?}"),
        }
    }

    /// The whole error-message feature rests on this: a key's reported position has to be the
    /// position an editor shows for it.
    #[test]
    fn column_is_reported_one_based() {
        let text = "a: 1\nbb:\n  ccc: 2\n";
        let doc = parse_document(text).unwrap();
        let root = map(&doc);
        assert_eq!(root[0].key.pos, Pos { line: 1, column: 1 });
        assert_eq!(root[0].value.pos, Pos { line: 1, column: 4 });
        assert_eq!(root[1].key.pos, Pos { line: 2, column: 1 });
        let inner = map(&root[1].value);
        assert_eq!(inner[0].key.pos, Pos { line: 3, column: 3 });
        assert_eq!(inner[0].value.pos, Pos { line: 3, column: 8 });
    }

    #[test]
    fn list_items_keep_their_own_line() {
        let text = "sections:\n  - id: a\n    bars: 4\n  - id: b\n    bars: 8\n";
        let doc = parse_document(text).unwrap();
        let items = match &map(&doc)[0].value.kind {
            NodeKind::Seq(items) => items,
            other => panic!("Liste erwartet, gefunden {other:?}"),
        };
        assert_eq!(items[0].pos.line, 2);
        assert_eq!(items[1].pos.line, 4);
    }

    #[test]
    fn duplicate_keys_survive_as_two_entries() {
        let doc = parse_document("bars: 4\nbars: 8\n").unwrap();
        let root = map(&doc);
        assert_eq!(root.len(), 2, "beide Vorkommen bleiben erhalten");
        assert_eq!(root[1].key.pos.line, 2);
    }

    #[test]
    fn plain_scalars_resolve_like_the_core_schema() {
        let doc = parse_document(
            "i: 141\nf: 141.5\nb: true\nn: ~\ns: 3/4\nq: \"141\"\nneg: -3\nempty:\n",
        )
        .unwrap();
        let root = map(&doc);
        assert!(matches!(root[0].value.kind, NodeKind::Int(141)));
        assert!(matches!(root[1].value.kind, NodeKind::Float(_)));
        assert!(matches!(root[2].value.kind, NodeKind::Bool(true)));
        assert!(matches!(root[3].value.kind, NodeKind::Null));
        assert_eq!(root[4].value.as_str(), Some("3/4"));
        assert_eq!(root[5].value.as_str(), Some("141"), "Quoting macht Text daraus");
        assert!(matches!(root[6].value.kind, NodeKind::Int(-3)));
        assert!(matches!(root[7].value.kind, NodeKind::Null));
    }

    #[test]
    fn a_syntax_error_reports_its_line() {
        let err = parse_document("tracks:\n  gitarre: {input: 2\nsections: []\n").unwrap_err();
        assert_eq!(err.issues.len(), 1);
        assert!(err.issues[0].message.contains("YAML-Syntaxfehler"));
        assert!(err.issues[0].line.is_some());
    }

    #[test]
    fn an_empty_file_is_a_score_error_not_a_panic() {
        let err = parse_document("").unwrap_err();
        assert!(err.issues[0].message.contains("leer"));
    }

    #[test]
    fn aliases_are_refused_with_an_explanation() {
        let err = parse_document("a: &x 1\nb: *x\n").unwrap_err();
        assert!(err.issues[0].message.contains("Anker"));
        assert_eq!(err.issues[0].line, Some(2));
    }
}
