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
            bars: Some(2),
            monitor: false,
            skip_end: None
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

// ---------------------------------------------------------------------------------------------
// `{state: play, monitor: true}`
// ---------------------------------------------------------------------------------------------

#[test]
fn monitor_goes_with_play_and_nowhere_else() {
    assert!(issues(&with_clip(2, "{state: play, monitor: true}")).is_empty());
    assert!(issues(&with_clip(2, "{state: play, restart: true, monitor: true}")).is_empty());
    // `monitor: false` says nothing and is allowed everywhere.
    assert!(issues(&with_clip(2, "{state: record, monitor: false}")).is_empty());
    for state in ["record", "overdub", "stack"] {
        let (line, message) = one_issue(&with_clip(2, &format!("{{state: {state}, monitor: true}}")));
        assert_eq!(line, 9);
        assert!(message.contains(&format!("'monitor' on {state} does nothing")), "{message}");
    }
    let (_, message) = one_issue(&with_clip(2, "{state: hear_through, monitor: true}"));
    assert!(message.contains("hear_through is the input alone"), "{message}");
    let (_, message) = one_issue(&with_clip(2, "{state: stop, monitor: true}"));
    assert!(message.contains("is on stop"), "{message}");
}

#[test]
fn monitor_is_refused_on_a_track_that_is_never_heard_or_always_heard() {
    let text = "bpm: 100\ntracks:\n  a: {input: 1, monitor: false}\nsections:\n  - id: s\n    bars: 2\n    tracks:\n      a: {state: play, monitor: true}\n";
    let (line, message) = one_issue(text);
    assert_eq!(line, 8);
    assert!(message.contains("'monitor: false'"), "{message}");
    let text = "bpm: 100\ntracks:\n  keys: {input: instrument}\nsections:\n  - id: s\n    bars: 2\n    tracks:\n      keys: {state: play, monitor: true}\n";
    let (_, message) = one_issue(text);
    assert!(message.contains("instrument track"), "{message}");
}

#[test]
fn monitor_compiles_into_the_clip_and_round_trips() {
    let text = with_clip(2, "{state: play, restart: true, monitor: true}");
    let score = compile_score(&text).expect("compiles");
    let clip = score.sections[0].clip("a");
    assert!(clip.monitor && clip.restart);
    let doc = parse_doc(&text).expect("parses");
    assert!(doc.sections[0].tracks[0].monitor);
    let rendered = render_doc(&doc);
    assert!(rendered.contains("      a: {state: play, restart: true, monitor: true}\n"), "{rendered}");
    assert_eq!(render_doc(&parse_doc(&rendered).expect("parses")), rendered);
}

// ---------------------------------------------------------------------------------------------
// An overdub that has no pass of its loop inside its section
// ---------------------------------------------------------------------------------------------

/// The owner's score of the 2026-11-21 14:57 run, as far as it matters: a ten-bar loop recorded in
/// `chorus_2`, silent in the bridge, back in a four-bar `chorus_3` (re-anchored there), and
/// `chorus_3_all` six bars later - where the loop's next pass would begin in bar 7 of a six-bar
/// section.
fn owners_chorus_3(chorus_3_all: &str) -> String {
    format!(
        "bpm: 94\ntracks:\n  bv: {{input: 1}}\nsections:\n  - id: chorus_2\n    bars: 10\n    autorelease: true\n    tracks:\n      bv: record\n  - id: bridge\n    bars: 8\n    autorelease: true\n  - id: chorus_3\n    bars: 4\n    autorelease: true\n    tracks:\n      bv: overdub\n  - id: chorus_3_all\n    bars: 6\n    autorelease: true\n    tracks:\n      bv: {chorus_3_all}\n  - id: end\n    bars: 1\n"
    )
}

#[test]
fn an_overdub_without_a_pass_in_its_section_is_an_error_with_its_line() {
    let (line, message) = one_issue(&owners_chorus_3("overdub"));
    assert_eq!(line, 22, "the state in chorus_3_all");
    assert!(
        message.contains("Track 'bv' is on overdub in Section 'chorus_3_all'")
            && message.contains("10-bar loop"),
        "{message}"
    );
    let Err(ScoreError { issues }) = compile_score(&owners_chorus_3("overdub")) else {
        unreachable!()
    };
    let hint = issues[0].suggestion.clone().unwrap_or_default();
    assert!(hint.contains("restart: true") && hint.contains("bar 7"), "{hint}");
    // What the hint says, works - and so does play, or a stack that restarts.
    for fixed in ["{state: overdub, restart: true}", "play", "{state: stack, restart: true}"] {
        assert!(issues_of(&owners_chorus_3(fixed)).is_empty(), "{fixed}");
    }
    let (_, message) = one_issue(&owners_chorus_3("stack"));
    assert!(message.contains("is on stack"), "{message}");
}

/// After a section that waits for Release, where the overdub falls depends on the press: a note,
/// not an error.
#[test]
fn after_a_release_section_it_is_a_note() {
    let text = "bpm: 120\ntracks:\n  a: {input: 1}\nsections:\n  - id: rec\n    bars: 4\n    autorelease: true\n    tracks:\n      a: record\n  - id: wait\n    bars: 1\n    autorelease: false\n    tracks:\n      a: play\n  - id: short\n    bars: 2\n    autorelease: true\n    tracks:\n      a: overdub\n";
    assert!(issues_of(text).is_empty());
    let notes = super::score_notes(text);
    assert!(
        notes.iter().any(|n| n.message.contains("as early as possible") && n.line == Some(19)),
        "{notes:?}"
    );
}

fn issues_of(text: &str) -> Vec<(Option<u32>, String)> {
    issues(text)
}

// ---------------------------------------------------------------------------------------------
// `skip_end`
// ---------------------------------------------------------------------------------------------

#[test]
fn skip_end_goes_with_record_and_overdub_and_has_to_leave_something() {
    for clip in ["{state: record, skip_end: 2}", "{state: overdub, skip_end: 1.5}", "{state: record, bars: 1, skip_end: 3.75}"] {
        assert!(issues(&with_clip(2, clip)).is_empty(), "{clip}");
    }
    // 0 switches a track default off and is allowed everywhere.
    assert!(issues(&with_clip(2, "{state: play, skip_end: 0}")).is_empty());
    for state in ["stack", "play", "stop", "hear_through"] {
        let (line, message) = one_issue(&with_clip(2, &format!("{{state: {state}, skip_end: 1}}")));
        assert_eq!(line, 9);
        assert!(message.contains("goes with record and overdub"), "{state}: {message}");
    }
    let (_, message) = one_issue(&with_clip(1, "{state: record, skip_end: 4}"));
    assert!(message.contains("4 beats out of a take of 4 beats"), "{message}");
    let (_, message) = one_issue(&with_clip(2, "{state: record, skip_end: -1}"));
    assert!(message.contains("between 0 and 64"), "{message}");
    // A track default, judged on the takes it applies to; a clip overrides it.
    let text = "bpm: 100\ntracks:\n  a: {input: 1, skip_end: 6}\nsections:\n  - id: s\n    bars: 1\n    tracks:\n      a: record\n";
    let (line, message) = one_issue(text);
    assert_eq!(line, 3, "at the track default");
    assert!(message.contains("of the track"), "{message}");
    assert!(issues(&text.replace("a: record", "a: {state: record, skip_end: 1}")).is_empty());
    let score = compile_score(&text.replace("bars: 1", "bars: 2")).expect("compiles");
    assert_eq!(score.tracks[0].skip_end, Some(6000));
}

#[test]
fn skip_end_round_trips_through_the_document() {
    let text = "bpm: 100\ntracks:\n  a: {input: 1, skip_end: 2}\nsections:\n  - id: s\n    bars: 2\n    tracks:\n      a: {state: record, bars: 1, skip_end: 1.5}\n";
    let score = compile_score(text).expect("compiles");
    assert_eq!(score.sections[0].clip("a").skip_end, Some(1500));
    let doc = parse_doc(text).expect("parses");
    assert_eq!(doc.tracks[0].skip_end, Some(2.0));
    assert_eq!(doc.sections[0].tracks[0].skip_end, Some(1.5));
    let rendered = render_doc(&doc);
    assert!(rendered.contains("a: {input: 1, skip_end: 2}"), "{rendered}");
    assert!(rendered.contains("a: {state: record, bars: 1, skip_end: 1.5}"), "{rendered}");
}
