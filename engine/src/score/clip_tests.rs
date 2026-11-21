//! Proofs for the clip options `restart` and `bars`: they compile into the section, a `repeat:`
//! carries them with the clip, they are refused where they would mean nothing, and they survive
//! the document round trip.

use super::doc::{parse_doc, render_doc};
use super::model::ClipOptions;
use super::{ScoreError, compile_score};

fn issues(text: &str) -> Vec<(Option<u32>, String)> {
    match compile_score(text) {
        Ok(_) => Vec::new(),
        Err(ScoreError { issues }) => issues.into_iter().map(|i| (i.line, i.message)).collect(),
    }
}

fn one_issue(text: &str) -> (u32, String) {
    let all = issues(text);
    assert_eq!(all.len(), 1, "genau ein Fehler erwartet: {all:?}");
    (all[0].0.expect("mit Zeile"), all[0].1.clone())
}

/// A score with one section of `bars` bars whose `a` clip is `clip`.
fn with_clip(bars: u32, clip: &str) -> String {
    format!(
        "bpm: 100\ntracks:\n  a: {{input: 1}}\n  b: {{input: 2}}\nsections:\n  - id: s\n    bars: {bars}\n    tracks:\n      a: {clip}\n"
    )
}

const CHORUS: &str = "\
bpm: 92

tracks:
  chorus_guitar: {input: 2}
  mic: {input: 1}
  theme: {input: 2}

sections:
  - id: theme
    bars: 2
    tracks:
      theme: record

  - id: chorus
    bars: 6
    tracks:
      # two bars, then the loop
      chorus_guitar: {state: record, bars: 2}
      mic: record
      theme: {state: play, restart: true}

  - id: chorus_again
    repeat: chorus
    tracks:
      mic: play
";

#[test]
fn clip_options_compile_and_a_repeat_carries_them_with_the_clip() {
    let score = compile_score(CHORUS).expect("compiles");
    let chorus = &score.sections[1];
    assert_eq!(
        chorus.clip("chorus_guitar"),
        ClipOptions {
            restart: false,
            bars: Some(2)
        }
    );
    assert!(chorus.clip("theme").restart);
    assert!(chorus.clip("mic").is_empty());
    // The repeat takes the clips it inherits along; the one it states itself has its own (none).
    let again = &score.sections[2];
    assert_eq!(again.clip("chorus_guitar").bars, Some(2));
    assert!(again.clip("theme").restart);
    assert!(again.clip("mic").is_empty());
    // A state that is restated without options drops the inherited ones.
    let text = CHORUS.replace("      mic: play\n", "      mic: play\n      theme: play\n");
    let score = compile_score(&text).expect("compiles");
    assert!(!score.sections[2].clip("theme").restart);
}

#[test]
fn restart_is_refused_where_it_means_nothing() {
    let (line, message) = one_issue(&with_clip(2, "{state: record, restart: true}"));
    assert_eq!(line, 9);
    assert!(message.contains("'restart' on record does nothing"), "{message}");
    for state in ["stop", "hear_through"] {
        let (_, message) = one_issue(&with_clip(2, &format!("{{state: {state}, restart: true}}")));
        assert!(message.contains("needs a loop that plays"), "{message}");
    }
    let (_, message) = one_issue(&with_clip(2, "{state: play, restart: yes}"));
    assert!(message.contains("true or false"), "{message}");
    for state in ["play", "overdub", "stack"] {
        assert!(issues(&with_clip(2, &format!("{{state: {state}, restart: true}}"))).is_empty());
    }
    // `restart: false` says nothing and is allowed everywhere.
    assert!(issues(&with_clip(2, "{state: record, restart: false}")).is_empty());
}

#[test]
fn a_take_length_is_refused_where_it_means_nothing_or_does_not_fit() {
    assert!(issues(&with_clip(6, "{state: record, bars: 2}")).is_empty());
    // Not a divisor of the section: allowed, the loop simply runs on.
    assert!(issues(&with_clip(6, "{state: record, bars: 4}")).is_empty());
    // The whole section: allowed, the same as without.
    assert!(issues(&with_clip(6, "{state: record, bars: 6}")).is_empty());

    let (line, message) = one_issue(&with_clip(6, "{state: record, bars: 8}"));
    assert_eq!(line, 9);
    assert!(message.contains("records 8 bars in Section 's', but the section has only 6"), "{message}");
    let (_, message) = one_issue(&with_clip(6, "{state: record, bars: 0}"));
    assert!(message.contains("'bars' of Track 'a'"), "{message}");
    let (_, message) = one_issue(&with_clip(6, "{state: overdub, bars: 2}"));
    assert!(message.contains("only goes with record") && message.contains("one pass"), "{message}");
    for state in ["stack", "play", "stop", "hear_through"] {
        let (_, message) = one_issue(&with_clip(6, &format!("{{state: {state}, bars: 2}}")));
        assert!(message.contains("only goes with record"), "{state}: {message}");
    }

    // Through a repeat that shortens the section, the inherited take no longer fits.
    let text = "bpm: 100\ntracks:\n  a: {input: 1}\nsections:\n  - id: s\n    bars: 6\n    tracks:\n      a: {state: record, bars: 4}\n  - id: t\n    repeat: s\n    bars: 2\n";
    let (line, message) = one_issue(text);
    assert_eq!(line, 9, "on the section that is too short for the clip it inherits");
    assert!(message.contains("Section 't'"), "{message}");
}

#[test]
fn clip_options_round_trip_through_the_document() {
    let doc = parse_doc(CHORUS).expect("parses");
    let chorus = &doc.sections[1];
    assert_eq!(chorus.tracks[0].bars, Some(2));
    assert!(!chorus.tracks[0].restart);
    assert!(chorus.tracks[2].restart);
    let text = render_doc(&doc);
    assert_eq!(text, CHORUS);

    // Every combination is written in one fixed order: grid, bars, restart.
    let mut doc = doc;
    doc.sections[1].tracks[2].bars = None;
    doc.sections[1].tracks[2].grid = Some("1/16".to_string());
    let text = render_doc(&doc);
    assert!(text.contains("      theme: {state: play, grid: 1/16, restart: true}\n"), "{text}");
    doc.sections[1].tracks[2].restart = false;
    doc.sections[1].tracks[2].grid = None;
    let text = render_doc(&doc);
    assert!(text.contains("      theme: play\n"), "{text}");
}
