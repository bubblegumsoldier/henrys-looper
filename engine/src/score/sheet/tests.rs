//! Proofs for the sheet: both chord notations with their columns, `|N`, directions, the timing
//! sums, the spans per section, the anchor checks, and the round trip through the score document.
//! The texts are invented placeholders.

use super::*;
use crate::score::doc::{parse_doc, render_doc};
use crate::score::{compile_score, score_notes};

fn lines(items: &[SheetItem]) -> Vec<(&str, Vec<(&str, u32)>, f64, f64)> {
    items
        .iter()
        .filter_map(|item| match item {
            SheetItem::Line { text, chords, bars, start, .. } => Some((
                text.as_str(),
                chords.iter().map(|c| (c.name.as_str(), c.col)).collect(),
                *bars,
                *start,
            )),
            _ => None,
        })
        .collect()
}

#[test]
fn chords_above_the_text_keep_their_columns() {
    let (items, total) = parse_sheet("Am            C/G\nWalking down the river road\n", 4);
    let l = lines(&items);
    assert_eq!(l.len(), 1, "chord line and text are one line");
    assert_eq!(l[0].0, "Walking down the river road");
    assert_eq!(l[0].1, vec![("Am", 0), ("C/G", 14)]);
    // Two chords, two beats each, in 4/4: one bar.
    assert_eq!(l[0].2, 1.0);
    assert_eq!(total, 1.0);
}

#[test]
fn a_chord_line_that_starts_indented_keeps_the_indent() {
    let (items, _) = parse_sheet("    G       D\n  the lamps are low\n", 4);
    let l = lines(&items);
    assert_eq!(l[0].0, "  the lamps are low", "the text keeps its own indent too");
    assert_eq!(l[0].1, vec![("G", 4), ("D", 12)]);
}

#[test]
fn inline_chords_sit_where_they_were_written() {
    let (items, _) = parse_sheet("[F]Paper boats on [C]Sunday\n", 4);
    let l = lines(&items);
    assert_eq!(l[0].0, "Paper boats on Sunday");
    assert_eq!(l[0].1, vec![("F", 0), ("C", 15)]);
}

#[test]
fn brackets_around_words_stay_text() {
    let (items, _) = parse_sheet("[Chorus] sing it [Am]loud\n", 4);
    let l = lines(&items);
    assert_eq!(l[0].0, "[Chorus] sing it loud");
    assert_eq!(l[0].1, vec![("Am", 17)]);
}

#[test]
fn a_chord_line_without_text_is_a_line_of_its_own() {
    let (items, total) = parse_sheet("Am   C/G   F   C\n# Next\nA line\n", 4);
    let l = lines(&items);
    assert_eq!(l[0].0, "");
    assert_eq!(l[0].1.len(), 4);
    assert_eq!(l[0].2, 2.0, "four chords of two beats in 4/4");
    // The line without chords lasts one bar.
    assert_eq!(l[1], ("A line", vec![], 1.0, 2.0));
    assert_eq!(total, 3.0);
}

#[test]
fn the_suffix_sets_bars_with_decimals() {
    let (items, total) = parse_sheet("[Am]One |1.5\nTwo |2\nAm C |0.75\nThree\n", 4);
    let l = lines(&items);
    assert_eq!(l[0].2, 1.5);
    assert_eq!(l[1].2, 2.0);
    assert_eq!(l[2].0, "Three", "a chord line's suffix carries over to its text");
    assert_eq!(l[2].2, 0.75);
    assert_eq!(total, 4.25);
}

#[test]
fn a_pipe_that_is_not_a_number_is_text() {
    let (items, _) = parse_sheet("left | right\nfoo |x\n", 4);
    let l = lines(&items);
    assert_eq!(l[0].0, "left | right");
    assert_eq!(l[1].0, "foo |x");
    assert_eq!(l[1].2, 1.0);
}

#[test]
fn beats_per_chord_and_the_time_signature_set_the_length() {
    let text = "beats per chord: 3\nAm C\nfirst\nBeats per chord: 1\n[G]a [D]b [C]c\n";
    let (items, total) = parse_sheet(text, 3);
    let l = lines(&items);
    assert_eq!(l[0].2, 2.0, "two chords of three beats in 3/4");
    assert_eq!(l[1].2, 1.0, "three chords of one beat in 3/4");
    assert_eq!(total, 3.0);
}

#[test]
fn directions_are_red_when_they_record_and_last_only_with_a_suffix() {
    let text = "> REC theme · 2 bars |2\n> Theme plays\n> record player\n> rec\nla\n";
    let (items, total) = parse_sheet(text, 4);
    let dirs: Vec<(&str, bool, f64, f64)> = items
        .iter()
        .filter_map(|i| match i {
            SheetItem::Direction { text, rec, bars, start, .. } => Some((text.as_str(), *rec, *bars, *start)),
            _ => None,
        })
        .collect();
    assert_eq!(
        dirs,
        vec![
            ("REC theme · 2 bars", true, 2.0, 0.0),
            ("Theme plays", false, 0.0, 2.0),
            ("record player", false, 0.0, 2.0),
            ("rec", true, 0.0, 2.0),
        ]
    );
    assert_eq!(total, 3.0);
}

#[test]
fn part_headers_carry_their_anchor_and_bar() {
    let text = "# Intro\nAm C\n# Verse 1 @verse_1\nla la\n# @outro\n";
    let (items, _) = parse_sheet(text, 4);
    let parts: Vec<(&str, Option<&str>, f64, u32)> = items
        .iter()
        .filter_map(|i| match i {
            SheetItem::Part { name, section, at, line } => Some((name.as_str(), section.as_deref(), *at, *line)),
            _ => None,
        })
        .collect();
    assert_eq!(
        parts,
        vec![("Intro", None, 0.0, 1), ("Verse 1", Some("verse_1"), 1.0, 3), ("", Some("outro"), 2.0, 5)]
    );
}

#[test]
fn tabs_count_as_spaces_to_the_next_stop() {
    let (items, _) = parse_sheet("Am\tC\nabcdefgh\n", 4);
    assert_eq!(lines(&items)[0].1, vec![("Am", 0), ("C", 4)]);
}

#[test]
fn chord_symbols() {
    for chord in ["A", "Am", "C/G", "F#m7", "Bbmaj7", "G9sus4", "Fm6", "Dm7", "CM7", "C#", "Ab/Eb", "Bdim", "E+", "C#m7b5", "Cadd9", "Esus"] {
        assert!(is_chord(chord), "{chord}");
    }
    for word in ["Hello", "a", "H7", "C/", "Am/x", "The", "Am,", "[Am]", "Chorus", "Bad", "Daddy", "Ex"] {
        assert!(!is_chord(word), "{word}");
    }
}

// ---- spans --------------------------------------------------------------------------------------

fn sections(list: &[(&'static str, u32, bool)]) -> Vec<SectionInfo<'static>> {
    list.iter().map(|&(id, bars, autorelease)| SectionInfo { id, bars, autorelease }).collect()
}

#[test]
fn without_anchors_the_sections_follow_each_other() {
    let s = sections(&[("a", 2, true), ("b", 4, false), ("c", 2, true)]);
    let sheet = compile_sheet("A |2\nB |4\nC |2\n", 4, &s);
    let spans: Vec<(f64, f64)> = sheet.spans.iter().map(|s| (s.start, s.end)).collect();
    assert_eq!(spans, vec![(0.0, 2.0), (2.0, 6.0), (6.0, 8.0)]);
}

#[test]
fn an_anchor_starts_its_section_and_unanchored_ones_run_on() {
    let s = sections(&[("intro", 2, true), ("loop", 2, false), ("verse", 4, true), ("bridge", 2, true)]);
    let text = "# Intro\nA |2\n# Loop @loop\nB |8\n# Verse @verse\nC |3\nD |1\nE |2\n";
    let sheet = compile_sheet(text, 4, &s);
    let spans: Vec<(f64, f64)> = sheet.spans.iter().map(|s| (s.start, s.end)).collect();
    // `loop` wraps around 8 bars of text; `bridge` runs on 4 bars after `verse`.
    assert_eq!(spans, vec![(0.0, 2.0), (2.0, 10.0), (10.0, 14.0), (14.0, 16.0)]);
    let (errors, notes) = check_sheet(&sheet, &s);
    assert!(errors.is_empty(), "{errors:?}");
    assert!(notes.is_empty(), "8 bars are four whole passes of the loop: {notes:?}");
}

#[test]
fn run_on_text_never_passes_the_next_anchor() {
    let s = sections(&[("a", 4, true), ("b", 4, true), ("c", 4, true)]);
    let sheet = compile_sheet("# A @a\nx |2\n# C @c\ny\n", 4, &s);
    let spans: Vec<(f64, f64)> = sheet.spans.iter().map(|s| (s.start, s.end)).collect();
    assert_eq!(spans, vec![(0.0, 2.0), (2.0, 2.0), (2.0, 3.0)]);
}

#[test]
fn unknown_anchors_are_errors_with_a_suggestion() {
    let s = sections(&[("verse_1", 4, true), ("chorus", 4, true)]);
    let sheet = compile_sheet("# V @verse_1\nla\n# C @chrous\nla\n", 4, &s);
    let (errors, _) = check_sheet(&sheet, &s);
    assert_eq!(errors.len(), 1);
    assert_eq!(errors[0].line, 3);
    assert!(errors[0].message.contains("'@chrous' names no section"), "{}", errors[0].message);
    assert_eq!(errors[0].suggestion.as_deref(), Some("did you mean '@chorus'?"));
}

#[test]
fn anchors_out_of_order_or_twice_are_errors() {
    let s = sections(&[("a", 1, true), ("b", 1, true)]);
    let sheet = compile_sheet("# @b\nx\n# @a\ny\n# @b\nz\n", 4, &s);
    let (errors, _) = check_sheet(&sheet, &s);
    assert_eq!(errors.iter().map(|e| e.line).collect::<Vec<_>>(), vec![3, 5]);
    assert!(errors[0].message.contains("comes first in the score"));
    assert!(errors[1].message.contains("anchored twice"));
}

#[test]
fn text_that_does_not_fit_its_sections_is_a_note() {
    let s = sections(&[("intro", 2, true), ("verse", 4, true), ("solo", 2, false), ("end", 1, true)]);
    let text = "x |1\n# Verse @verse\na |3.5\n# Solo @solo\nb |3\n# End @end\nc\n";
    let sheet = compile_sheet(text, 4, &s);
    let (errors, notes) = check_sheet(&sheet, &s);
    assert!(errors.is_empty());
    let messages: Vec<(u32, &str)> = notes.iter().map(|n| (n.line, n.message.as_str())).collect();
    assert_eq!(messages.len(), 3, "{messages:?}");
    assert_eq!(messages[0].0, 2);
    assert!(messages[0].1.contains("begins at bar 1 of the text"), "{}", messages[0].1);
    assert!(messages[1].1.contains("lasts 3.5 bars, section 'verse' has 4 bars"), "{}", messages[1].1);
    assert_eq!(messages[2].0, 4);
    assert!(messages[2].1.contains("not a whole number of passes"), "{}", messages[2].1);
}

// ---- in the score ------------------------------------------------------------------------------

const SCORE: &str = "\
title: Sheet test
bpm: 100
time_signature: 4/4

tracks:
  voice: {input: 1}

sections:
  - id: intro
    bars: 2
    autorelease: true
    tracks:
      voice: record
  - id: verse
    bars: 4
    autorelease: false
    tracks:
      voice: play
";

fn with_sheet(sheet_yaml: &str) -> String {
    format!("{SCORE}\n{sheet_yaml}")
}

#[test]
fn a_score_without_a_sheet_compiles_as_before() {
    let score = compile_score(SCORE).unwrap();
    assert!(score.sheet.is_none());
    let json = serde_json::to_string(&score).unwrap();
    assert!(!json.contains("sheet"), "nothing new on the wire");
}

#[test]
fn the_compiled_score_carries_the_parsed_sheet() {
    let text = with_sheet("sheet: |\n  # Intro\n  > REC theme |2\n  # Verse @verse\n  [Am]la la [C]la |4\n");
    let score = compile_score(&text).unwrap();
    let sheet = score.sheet.expect("sheet");
    assert_eq!(sheet.total, 6.0);
    assert_eq!(sheet.spans, vec![SheetSpan { start: 0.0, end: 2.0 }, SheetSpan { start: 2.0, end: 6.0 }]);
    assert!(score_notes(&text).is_empty(), "{:?}", score_notes(&text));
}

#[test]
fn an_unknown_anchor_fails_the_score_with_its_file_line() {
    let text = with_sheet("sheet: |\n  # Intro\n  la\n  # Verse @vers\n  la |4\n");
    let err = compile_score(&text).unwrap_err();
    assert_eq!(err.issues.len(), 1);
    let issue = &err.issues[0];
    let line = text.lines().position(|l| l.contains("@vers")).unwrap() as u32 + 1;
    assert_eq!(issue.line, Some(line), "{issue}");
    assert_eq!(issue.suggestion.as_deref(), Some("did you mean '@verse'?"));
}

#[test]
fn a_sheet_that_does_not_fit_is_a_note_not_an_error() {
    let text = with_sheet("sheet: |\n  # Verse @verse\n  la |3\n");
    assert!(compile_score(&text).is_ok());
    let notes = score_notes(&text);
    assert_eq!(notes.len(), 2, "{notes:?}");
    let line = text.lines().position(|l| l.contains("@verse")).unwrap() as u32 + 1;
    assert!(notes.iter().all(|n| n.line == Some(line)));
}

#[test]
fn a_sheet_must_be_text() {
    let err = compile_score(&with_sheet("sheet: [1, 2]\n")).unwrap_err();
    assert!(err.issues[0].message.contains("'sheet' must be text"), "{}", err.issues[0]);
}

// ---- round trip through the document ----------------------------------------------------------

/// The sheet's text after text -> document -> text.
fn round_trip(sheet_text: &str) -> (String, String) {
    let mut doc = parse_doc(SCORE).unwrap();
    doc.sheet = Some(sheet_text.to_string());
    let yaml = render_doc(&doc);
    let back = parse_doc(&yaml).unwrap_or_else(|e| panic!("{e}\n{yaml}"));
    assert_eq!(render_doc(&back), yaml, "idempotent");
    (back.sheet.expect("sheet survives"), yaml)
}

#[test]
fn the_sheet_comes_back_byte_for_byte() {
    let texts = [
        "beats per chord: 2\n\n# Intro\n> REC theme · 2 bars |2\nAm   C/G   F   C\n",
        "   Am            C/G\nleading spaces on the first line\n",
        "no newline at the end",
        "two newlines at the end\n\n",
        "\n\nblank lines first\n",
        "trailing spaces   \n  # indented hash\n\twith a tab\n",
        "quotes \" and ' and \\ and: colons # hashes\n- dash\n? question\n",
        "Ümlaute, emoji 🎸 and ‒ dashes\n",
        "windows\r\nline endings\r\n",
        "   \n",
        "\n",
    ];
    for text in texts {
        let (back, yaml) = round_trip(text);
        assert_eq!(back, text, "{yaml}");
    }
}

#[test]
fn the_sheet_is_a_literal_block_after_everything_else() {
    let (_, yaml) = round_trip("# Intro\n> REC theme |2\nAm C\nla la\n");
    assert!(yaml.ends_with("sheet: |\n  # Intro\n  > REC theme |2\n  Am C\n  la la\n"), "{yaml}");
    let (_, yaml) = round_trip("  Am\nla");
    assert!(yaml.contains("sheet: |2-\n    Am\n  la\n"), "{yaml}");
}

#[test]
fn hash_lines_in_the_sheet_are_not_comments_but_comments_around_it_stay() {
    let text = with_sheet(
        "# the words\nsheet: |  # written by hand\n  # Intro\n  la\n\n  # Verse @verse\n  la |4\n# the end\n",
    );
    let doc = parse_doc(&text).unwrap();
    assert_eq!(doc.sheet.as_deref(), Some("# Intro\nla\n\n# Verse @verse\nla |4\n"));
    assert_eq!(doc.footer, vec!["the end".to_string()]);
    let comments = doc.field_comments.get("sheet").cloned().unwrap_or_default();
    assert_eq!(comments.before, vec!["the words".to_string()]);
    assert_eq!(comments.after.as_deref(), Some("written by hand"));
    let again = render_doc(&doc);
    assert_eq!(render_doc(&parse_doc(&again).unwrap()), again);
    assert_eq!(parse_doc(&again).unwrap().sheet, doc.sheet);
}

#[test]
fn an_arrangement_edit_keeps_the_sheet() {
    let text = with_sheet("sheet: |\n  # Verse @verse\n  la |4\n");
    let mut doc = parse_doc(&text).unwrap();
    let sheet = doc.sheet.clone();
    doc.sections[1].bars = Some(8);
    doc.sections.swap(0, 1);
    let yaml = render_doc(&doc);
    assert_eq!(parse_doc(&yaml).unwrap().sheet, sheet);
}
