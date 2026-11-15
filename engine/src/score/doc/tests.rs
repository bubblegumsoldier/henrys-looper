//! Proofs for the document model: the round trip changes nothing the compiler sees, keeps every
//! comment, is idempotent, and every issue of an edited document names its element.

use super::*;
use crate::score::compile_score;

const HENRY: &str = include_str!("../../../../examples/henry-3-4.rust.yaml");

/// Everything the format has, in an untidy layout, with comments in awkward places.
const RICH: &str = "\
# Kopf: zwei Zeilen
#   eingerueckt
title: \"Song: Teil 1\"   # Titel mit Doppelpunkt
bpm: 98.5
beats_per_bar: 3
time_signature: 3/4

tracks:
  # die Stimme
  stimme: {input: 1, monitor: true}
  klavier:
    input: [3, 4]
    pan: -0.4        # etwas links
    latency: 512
  cantabile: {input: [5, 6], latency_trim: -96, bus: monitor, monitor_bus: none}

midi:
  next_section: {cc: 64}
  track.stimme.record: {note: 36, channel: 2, mode: press}
  track.klavier.fx.reverb.mix: {cc: 3, max: 0.4, takeover: jump}

sections:
  - id: a
    bars: 4
    autorelease: true
    tracks:
      stimme: record   # erste Aufnahme
      klavier: hear_through
  # b erbt von a
  - id: b
    repeat: a
    quantize: bar
    tracks:
      klavier: record
  - id: c
    repeat: b
    bars: 2

# Mischpult
mixer:
  buses: {main: -1.5, click: -12}   # leiser Klick
  tracks:
    stimme: {gain: -3, preset: voice}   # Gesang
    # Klavier mit eigenem Kompressor
    klavier:
      gain: 1.5
      mute: true
      preset: voice
      comp: {threshold: -10, ratio: 4}   # haerter
      band1: {kind: low_shelf, hz: 120}
# Ende der Datei
";

fn without_lines(mut score: CompiledScore) -> CompiledScore {
    for section in &mut score.sections {
        section.source_line = None;
    }
    score
}

fn comment_texts(text: &str) -> Vec<String> {
    let mut all: Vec<String> = find_comments(text).into_iter().map(|c| c.text).collect();
    all.sort();
    all
}

fn round_trip(text: &str) -> String {
    render_doc(&parse_doc(text).expect("die Partitur liest sich"))
}

#[test]
fn the_round_trip_changes_nothing_the_compiler_sees() {
    for text in [HENRY, RICH] {
        let once = round_trip(text);
        assert_eq!(
            without_lines(compile_score(text).expect("Original")),
            without_lines(compile_score(&once).expect("gerendert")),
            "Inhalt veraendert:\n{once}"
        );
    }
}

#[test]
fn the_round_trip_is_idempotent() {
    for text in [HENRY, RICH] {
        let once = round_trip(text);
        let twice = round_trip(&once);
        assert_eq!(once, twice, "die zweite Runde aendert noch etwas");
    }
}

#[test]
fn every_comment_survives_the_round_trip() {
    for text in [HENRY, RICH] {
        let once = round_trip(text);
        assert_eq!(comment_texts(text), comment_texts(&once), "\n{once}");
    }
}

#[test]
fn comments_land_on_the_element_they_describe() {
    let doc = parse_doc(RICH).expect("liest sich");
    assert_eq!(doc.header, vec!["Kopf: zwei Zeilen", "  eingerueckt"]);
    assert_eq!(
        doc.field_comments.get("title").and_then(|c| c.after.clone()).as_deref(),
        Some("Titel mit Doppelpunkt")
    );
    assert_eq!(doc.tracks[0].comments.before, vec!["die Stimme"]);
    // A comment inside a block-style track belongs to that track.
    assert_eq!(doc.tracks[1].comments.after.as_deref(), Some("etwas links"));
    assert_eq!(
        doc.sections[0].tracks[0].comments.after.as_deref(),
        Some("erste Aufnahme")
    );
    assert_eq!(doc.sections[1].comments.before, vec!["b erbt von a"]);
    assert_eq!(doc.footer, vec!["Ende der Datei"]);
}

#[test]
fn comments_move_with_their_section() {
    let mut doc = parse_doc(RICH).expect("liest sich");
    doc.sections.swap(1, 2);
    let text = render_doc(&doc);
    let c = text.find("- id: c").expect("c");
    let b = text.find("- id: b").expect("b");
    let note = text.find("# b erbt von a").expect("Kommentar");
    assert!(c < note && note < b, "der Kommentar steht ueber b:\n{text}");
}

#[test]
fn the_document_says_what_a_section_states_and_what_it_inherits() {
    let doc = parse_doc(RICH).expect("liest sich");
    let c = &doc.sections[2];
    assert_eq!(c.bars, Some(2));
    assert_eq!(c.quantize, None, "c nennt quantize nicht selbst");
    assert!(c.tracks.is_empty());
    let r = c.resolved.as_ref().expect("aufgeloest");
    assert_eq!(r.bars, Resolved { value: 2, from: ValueSource::Own });
    assert_eq!(
        r.quantize,
        Resolved {
            value: Quantize::Bar,
            from: ValueSource::Repeat { index: 1, section: "b".into() }
        },
        "quantize kommt von b, das es selbst nennt"
    );
    assert_eq!(
        r.autorelease.from,
        ValueSource::Repeat { index: 0, section: "a".into() },
        "autorelease kommt ueber b von a"
    );
    let states: Vec<(&str, TrackState, &ValueSource)> = r
        .tracks
        .iter()
        .map(|t| (t.track.as_str(), t.state, &t.from))
        .collect();
    assert_eq!(states[0].1, TrackState::Record);
    assert_eq!(states[1].1, TrackState::Record, "klavier: b ueberschreibt a");
    assert_eq!(states[1].2, &ValueSource::Repeat { index: 1, section: "b".into() });
    assert_eq!(states[2], ("cantabile", TrackState::Stop, &ValueSource::Default));
}

#[test]
fn written_defaults_stay_written_and_omitted_ones_stay_omitted() {
    let doc = parse_doc(RICH).expect("liest sich");
    assert_eq!(doc.tracks[0].monitor, Some(true));
    assert_eq!(doc.tracks[0].pan, None);
    assert_eq!(doc.tracks[1].input_right, Some(4));
    assert_eq!(doc.tracks[2].latency_trim, Some(-96));
    assert_eq!(doc.beats_per_bar, Some(3));
    let text = render_doc(&doc);
    assert!(text.contains("stimme: {input: 1, monitor: true}"), "{text}");
    assert!(text.contains("klavier: {input: [3, 4], pan: -0.4, latency: 512}"), "{text}");
    assert!(text.contains("title: \"Song: Teil 1\""), "{text}");
}

#[test]
fn the_shipped_example_reads_into_the_expected_document() {
    let doc = parse_doc(HENRY).expect("liest sich");
    assert_eq!(doc.title.as_deref(), Some("Henry 3/4"));
    assert_eq!(doc.bpm, 141.0);
    assert_eq!(doc.tracks.len(), 2);
    assert_eq!(doc.sections.len(), 6);
    assert_eq!(doc.sections[3].repeat.as_deref(), Some("voice_2"));
    assert_eq!(doc.sections[3].bars, None);
    assert_eq!(doc.sections[3].resolved.as_ref().unwrap().bars.value, 8);
    assert_eq!(doc.sections[0].line, Some(23));
    assert!(doc.header.len() >= 8);
}

#[test]
fn awkward_strings_are_quoted_and_read_back_unchanged() {
    for title in ["true", "123", "3.5", "#hash", "a: b", " leading", "- x", "null", "Henry's", "x, y"] {
        let mut doc = parse_doc(HENRY).expect("liest sich");
        doc.title = Some(title.to_string());
        let outcome = render_doc_outcome(&doc);
        assert!(outcome.ok, "{title}: {:?}\n{}", outcome.errors, outcome.yaml);
        assert_eq!(outcome.doc.title.as_deref(), Some(title), "{}", outcome.yaml);
    }
}

fn edited(change: impl FnOnce(&mut ScoreDoc)) -> RenderOutcome {
    let mut doc = parse_doc(RICH).expect("liest sich");
    change(&mut doc);
    render_doc_outcome(&doc)
}

fn only_issue(outcome: &RenderOutcome) -> &ModelIssue {
    assert!(!outcome.ok, "das haette scheitern muessen:\n{}", outcome.yaml);
    assert_eq!(outcome.errors.len(), 1, "{:?}", outcome.errors);
    &outcome.errors[0]
}

#[test]
fn an_invalid_edit_is_reported_with_the_compilers_message_and_its_element() {
    let o = edited(|d| d.bpm = 900.0);
    let issue = only_issue(&o);
    assert!(issue.message.contains("'bpm' must be between"), "{}", issue.message);
    assert_eq!(issue.at, Some(ScoreRef::Score { field: Some("bpm".into()) }));

    // A track with a bad value is dropped by the compiler, so its uses elsewhere follow up; the
    // first issue is the cause and names the field.
    let o = edited(|d| d.tracks[1].pan = Some(2.0));
    assert!(!o.ok);
    assert_eq!(
        o.errors[0].at,
        Some(ScoreRef::Track { index: 1, name: "klavier".into(), field: Some("pan".into()) })
    );

    let o = edited(|d| d.sections[0].tracks[1].track = "klavir".into());
    let issue = only_issue(&o);
    assert_eq!(issue.suggestion.as_deref(), Some("did you mean 'klavier'?"));
    assert_eq!(
        issue.at,
        Some(ScoreRef::SectionTrack {
            section: 0,
            section_id: "a".into(),
            index: 1,
            track: "klavir".into()
        })
    );

    let o = edited(|d| d.sections[2].repeat = Some("x".into()));
    assert_eq!(
        only_issue(&o).at,
        Some(ScoreRef::Section { index: 2, id: "c".into(), field: Some("repeat".into()) })
    );

    let o = edited(|d| d.sections[2].id = "a".into());
    assert!(o.errors.iter().any(|i| i.message.contains("used twice")
        && i.at == Some(ScoreRef::Section { index: 2, id: "a".into(), field: Some("id".into()) })));

    let o = edited(|d| {
        d.sections[2].repeat = None;
        d.sections[2].bars = None;
    });
    assert_eq!(
        only_issue(&o).at,
        Some(ScoreRef::Section { index: 2, id: "c".into(), field: Some("bars".into()) })
    );

    let o = edited(|d| d.tracks[0].name = "zwei worte".into());
    assert!(o.errors.iter().any(|i| i.message.contains("Invalid track name")
        && matches!(&i.at, Some(ScoreRef::Track { index: 0, .. }))));

    let o = edited(|d| d.midi[1].mode = Some("halten".into()));
    assert!(matches!(
        &only_issue(&o).at,
        Some(ScoreRef::Midi { index: 1, field: Some(f), .. }) if f == "mode"
    ));

    let o = edited(|d| {
        for n in 0..6 {
            d.tracks.push(DocTrack::new(format!("t{n}"), 7));
        }
    });
    assert_eq!(only_issue(&o).at, Some(ScoreRef::Score { field: Some("tracks".into()) }));
}

#[test]
fn a_document_the_ui_broke_still_shows_its_inheritance() {
    let o = edited(|d| d.bpm = 900.0);
    assert!(!o.ok);
    assert!(o.doc.sections[2].resolved.is_some());
}

#[test]
fn text_with_errors_has_no_document_but_its_issues_name_elements() {
    let broken = RICH.replace("klavier: hear_through", "klavier: hear_thru");
    let outcome = parse_doc_outcome(&broken);
    assert!(!outcome.ok);
    assert!(outcome.doc.is_none());
    assert_eq!(outcome.errors.len(), 1);
    assert!(matches!(
        &outcome.errors[0].at,
        Some(ScoreRef::SectionTrack { section: 0, track, .. }) if track == "klavier"
    ));
    assert!(!outcome.locations.is_empty());
}

#[test]
fn a_valid_parse_carries_the_compiled_score_and_every_location() {
    let outcome = parse_doc_outcome(HENRY);
    assert!(outcome.ok);
    assert_eq!(outcome.score.expect("kompiliert").sections.len(), 6);
    assert!(outcome.locations.iter().any(|l| l.at
        == ScoreRef::Section { index: 0, id: "gitarre_rec".into(), field: Some("bars".into()) }));
}

#[test]
fn the_document_survives_json_unchanged() {
    let doc = parse_doc(RICH).expect("liest sich");
    let json = serde_json::to_string(&doc).expect("JSON");
    let mut back: ScoreDoc = serde_json::from_str(&json).expect("zurueck");
    // `resolved` is output only and not read back; the rendering is what counts.
    resolve_doc(&mut back);
    assert_eq!(render_doc(&back), render_doc(&doc));
    let value: serde_json::Value = serde_json::from_str(&json).unwrap();
    assert_eq!(value["sections"][2]["resolved"]["quantize"]["from"]["kind"], "repeat");
    assert_eq!(value["tracks"][1]["input_right"], 4);
}

#[test]
fn a_document_from_the_ui_needs_only_the_essentials() {
    let doc: ScoreDoc = serde_json::from_str(
        r#"{"bpm": 120, "tracks": [{"name": "a", "input": 1}],
            "sections": [{"id": "x", "bars": 2, "tracks": [{"track": "a", "state": "record"}]}]}"#,
    )
    .expect("minimales Dokument");
    let outcome = render_doc_outcome(&doc);
    assert!(outcome.ok, "{:?}\n{}", outcome.errors, outcome.yaml);
    assert_eq!(
        outcome.yaml,
        "bpm: 120\n\ntracks:\n  a: {input: 1}\n\nsections:\n  - id: x\n    bars: 2\n    tracks:\n      a: record\n"
    );
}

#[test]
fn the_template_for_a_new_score_compiles() {
    let text = template(&[]);
    let score = compile_score(&text).expect("Vorlage kompiliert");
    assert_eq!(score.tracks.len(), 2);
    assert_eq!(score.sections.len(), 3);
    let text = template(&[("stimme".into(), 1, None), ("keys".into(), 3, Some(4))]);
    let score = compile_score(&text).expect("Vorlage mit Engine-Tracks kompiliert");
    assert_eq!(score.tracks[1].channels, 2);
    assert_eq!(round_trip(&text), text, "die Vorlage ist schon kanonisch");
}

#[test]
fn a_hash_inside_quotes_is_not_a_comment() {
    assert_eq!(comment_start("title: \"a # b\"  # echt"), Some(16));
    assert_eq!(comment_start("title: 'it''s # x'"), None);
    assert_eq!(comment_start("bus: a#b"), None);
    assert_eq!(comment_start("# ganz"), Some(0));
}

#[test]
fn the_mixer_section_reads_into_the_document_with_its_comments() {
    let doc = parse_doc(RICH).expect("liest sich");
    let mixer = doc.mixer.as_ref().expect("mixer");
    assert_eq!(
        doc.field_comments.get("mixer").map(|c| c.before.clone()),
        Some(vec!["Mischpult".to_string()])
    );
    assert_eq!(mixer.buses.as_ref().and_then(|b| b.main), Some(-1.5));
    assert_eq!(mixer.buses.as_ref().and_then(|b| b.monitor), None);
    assert_eq!(
        mixer.field_comments.get("buses").and_then(|c| c.after.clone()).as_deref(),
        Some("leiser Klick")
    );
    assert_eq!(mixer.tracks.len(), 2);
    assert_eq!(mixer.tracks[0].track, "stimme");
    assert_eq!(mixer.tracks[0].mix.gain, Some(-3.0));
    assert_eq!(mixer.tracks[0].comments.after.as_deref(), Some("Gesang"));
    let klavier = &mixer.tracks[1];
    assert_eq!(klavier.comments.before, vec!["Klavier mit eigenem Kompressor"]);
    assert_eq!(klavier.mix.mute, Some(true));
    assert_eq!(klavier.mix.comp.as_ref().and_then(|c| c.ratio), Some(4.0));
    assert_eq!(
        klavier.field_comments.get("comp").and_then(|c| c.after.clone()).as_deref(),
        Some("haerter")
    );
    assert_eq!(doc.footer, vec!["Ende der Datei"]);
}

#[test]
fn a_score_without_a_mixer_section_stays_without_one() {
    let doc = parse_doc(HENRY).expect("liest sich");
    assert!(doc.mixer.is_none());
    assert!(!round_trip(HENRY).contains("mixer"));
    assert!(compile_score(HENRY).expect("kompiliert").mixer.is_none());
}

#[test]
fn a_mixer_entry_written_by_the_app_is_rendered_canonically_and_compiles() {
    use crate::engine::fx::FxPreset;
    use crate::score::mixer::TrackMixSource;
    let mut doc = parse_doc(HENRY).expect("liest sich");
    let mut tweaked = FxPreset::Voice.settings();
    tweaked.reverb_mix = 0.3;
    let first = doc.tracks[0].name.clone();
    doc.mixer = Some(DocMixer {
        buses: None,
        tracks: vec![DocMixerTrack::new(
            first.clone(),
            TrackMixSource::from_state(0.5, false, false, FxPreset::Custom, &tweaked),
        )],
        field_comments: OrderedMap::new(),
    });
    let outcome = render_doc_outcome(&doc);
    assert!(outcome.ok, "{:?}\n{}", outcome.errors, outcome.yaml);
    assert!(
        outcome.yaml.ends_with(&format!(
            "mixer:\n  tracks:\n    {first}:\n      gain: -6\n      preset: voice\n      reverb: {{mix: 0.3}}\n"
        )),
        "{}",
        outcome.yaml
    );
    let mixer = outcome.score.expect("kompiliert").mixer.expect("mixer");
    let back = mixer.tracks.get(&first).expect("eintrag").resolve();
    assert_eq!(back.settings, tweaked);
}

#[test]
fn a_mixer_entry_for_an_unknown_track_names_its_element() {
    let o = edited(|d| d.mixer.as_mut().unwrap().tracks[1].track = "klavir".into());
    let issue = only_issue(&o);
    assert_eq!(issue.suggestion.as_deref(), Some("did you mean 'klavier'?"));
    assert_eq!(
        issue.at,
        Some(ScoreRef::MixerTrack { index: 1, track: "klavir".into(), field: None })
    );
    let o = edited(|d| d.mixer.as_mut().unwrap().tracks[0].mix.gain = Some(20.0));
    let issue = only_issue(&o);
    assert!(issue.message.contains("dB"), "{}", issue.message);
    assert_eq!(
        issue.at,
        Some(ScoreRef::MixerTrack { index: 0, track: "stimme".into(), field: Some("gain".into()) })
    );
}
