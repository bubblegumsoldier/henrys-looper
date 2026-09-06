//! Proofs for the score compiler. Pure logic, so everything here is decided offline - no device,
//! no audio, no timing.
//!
//! Two groups:
//!
//! * **It compiles the real thing.** The three scores that exist in `examples/`, carried over to
//!   the new format, plus the properties the runner will depend on (`repeat` unrolled, every
//!   section complete, odd time signatures).
//! * **It says where the mistake is.** Every diagnostic is checked against a file whose offending
//!   line is known, and the reported line number is compared exactly. A line number that is off by
//!   three is worse than none at all, so these are equality assertions, not "contains".

use super::model::TrackState;
use super::{ScoreError, compile_score, read};
use crate::engine::frame::TrackInput;
use crate::engine::schedule::Quantize;

// ---------------------------------------------------------------------------------------------
// The three existing scores, in the new format
// ---------------------------------------------------------------------------------------------

/// `examples/henry-3-4.rust.yaml`, read from disk so the shipped example can never rot.
const HENRY_GROUP: &str = include_str!("../../../examples/henry-3-4.rust.yaml");

/// `examples/henry-3-4.yaml` carried over: four separate Ableton tracks become four tracks with
/// input channels, `voice_live` becomes an ordinary track that only ever hears through.
const HENRY_FLAT: &str = "\
title: Henry 3/4
bpm: 141
time_signature: 3/4

tracks:
  gitarre:    {input: 2}
  voice:      {input: 1}
  voice2:     {input: 1}
  voice_live: {input: 1}

sections:
  - id: gitarre_rec
    bars: 8
    autorelease: true
    tracks:
      gitarre: record
      voice_live: hear_through

  - id: voice_rec
    bars: 8
    autorelease: true
    tracks:
      gitarre: play
      voice: record
      voice_live: hear_through

  - id: voice_overdub
    bars: 8
    autorelease: true
    tracks:
      gitarre: play
      voice: play
      voice2: record
      voice_live: hear_through

  - id: alles
    bars: 8
    autorelease: false
    quantize: bar
    tracks:
      gitarre: play
      voice: play
      voice2: play
      voice_live: hear_through

  - id: ende
    bars: 1
    autorelease: false
    tracks:
      voice_live: hear_through
";

/// `examples/example-song.yaml` carried over. Keeps the MIDI block and the `repeat:` in `outro`.
const EXAMPLE_SONG: &str = "\
title: Beispiel-Song
bpm: 100
beats_per_bar: 4

tracks:
  gitarre: {input: 2}
  voice:   {input: 1}

midi:
  next_section: {cc: 64}
  stop_all:     {note: 37}

sections:
  - id: intro
    bars: 4
    autorelease: false
    quantize: loop
    tracks:
      gitarre: record
      voice: hear_through

  - id: verse
    bars: 4
    autorelease: true
    tracks:
      gitarre: play
      voice: record

  - id: outro
    repeat: verse
    autorelease: false
    tracks:
      gitarre: overdub
      voice: play
";

fn compiled(yaml: &str) -> super::CompiledScore {
    match compile_score(yaml) {
        Ok(score) => score,
        Err(err) => panic!("Partitur sollte fehlerfrei kompilieren:\n{err}"),
    }
}

fn errors(yaml: &str) -> ScoreError {
    match compile_score(yaml) {
        Ok(_) => panic!("Partitur haette Fehler melden muessen"),
        Err(err) => err,
    }
}

/// The states of one section, in track order.
fn states(section: &super::CompiledSection) -> Vec<(&str, TrackState)> {
    section.tracks.iter().map(|(name, state)| (name, *state)).collect()
}

#[test]
fn the_shipped_example_compiles_and_keeps_its_section_order() {
    let score = compiled(HENRY_GROUP);
    assert_eq!(score.title, "Henry 3/4");
    assert_eq!(score.bpm, 141.0);
    assert_eq!(score.beats_per_bar, 3);
    assert_eq!(score.beat_unit, 4);
    assert_eq!(
        score.sections.iter().map(|s| s.id.as_str()).collect::<Vec<_>>(),
        vec!["gitarre_rec", "voice_1", "voice_2", "voice_3", "alles", "ende"]
    );
    assert_eq!(
        score.tracks.iter().map(|t| (t.name.as_str(), t.input, t.input_channel)).collect::<Vec<_>>(),
        vec![("gitarre", 2, 1), ("voice", 1, 0)]
    );
    assert!(score.tracks.iter().all(|t| t.monitor), "monitor ist Default true");
    assert_eq!(score.total_bars(), 8 + 8 + 8 + 8 + 8 + 1);
}

#[test]
fn the_flat_four_track_score_compiles() {
    let score = compiled(HENRY_FLAT);
    assert_eq!(score.tracks.len(), 4);
    assert_eq!(
        score.sections.iter().map(|s| s.id.as_str()).collect::<Vec<_>>(),
        vec!["gitarre_rec", "voice_rec", "voice_overdub", "alles", "ende"]
    );
    // Two tracks may share one input channel - one microphone, two independent loop stacks.
    assert_eq!(score.track("voice").unwrap().input_channel, 0);
    assert_eq!(score.track("voice2").unwrap().input_channel, 0);
    assert_eq!(score.sections[3].quantize, Quantize::Bar);
}

#[test]
fn the_example_song_compiles_with_its_midi_bindings() {
    let score = compiled(EXAMPLE_SONG);
    assert_eq!(score.beats_per_bar, 4);
    assert_eq!(score.time_signature, "4/4");
    assert_eq!(score.midi.get("next_section").unwrap().id, "ch1.cc64");
    assert_eq!(score.midi.get("stop_all").unwrap().id, "ch1.note37");
}

// ---------------------------------------------------------------------------------------------
// Resolution
// ---------------------------------------------------------------------------------------------

#[test]
fn repeat_copies_everything_and_the_section_overrides_what_it_names() {
    let score = compiled(EXAMPLE_SONG);
    let verse = score.section("verse").unwrap();
    let outro = score.section("outro").unwrap();
    assert_eq!(outro.bars, verse.bars, "bars kommt aus der wiederholten Sektion");
    assert_eq!(outro.quantize, verse.quantize);
    assert!(!outro.autorelease, "autorelease ist ueberschrieben");
    assert_eq!(states(outro), vec![("gitarre", TrackState::Overdub), ("voice", TrackState::Play)]);
}

#[test]
fn a_section_with_only_repeat_is_a_full_copy() {
    let score = compiled(HENRY_GROUP);
    let source = score.section("voice_2").unwrap();
    let copy = score.section("voice_3").unwrap();
    assert_eq!(copy.bars, source.bars);
    assert_eq!(states(copy), states(source));
    assert!(!copy.autorelease);
    assert_eq!(copy.quantize, Quantize::Loop);
}

#[test]
fn repeat_chains_resolve_through_several_hops() {
    let yaml = "\
bpm: 120
tracks: {gitarre: {input: 1}}
sections:
  - {id: a, bars: 6, quantize: bar, tracks: {gitarre: record}}
  - {id: b, repeat: a}
  - {id: c, repeat: b}
  - {id: d, repeat: c, bars: 2}
";
    let score = compiled(yaml);
    for id in ["a", "b", "c"] {
        assert_eq!(score.section(id).unwrap().bars, 6, "Sektion {id}");
        assert_eq!(score.section(id).unwrap().quantize, Quantize::Bar);
        assert_eq!(states(score.section(id).unwrap()), vec![("gitarre", TrackState::Record)]);
    }
    let d = score.section("d").unwrap();
    assert_eq!(d.bars, 2, "die eigene Angabe schlaegt die geerbte");
    assert_eq!(states(d), vec![("gitarre", TrackState::Record)]);
}

/// A `repeat:` may point forwards; the resolver memoises by id, not by position.
#[test]
fn repeat_may_point_at_a_later_section() {
    let yaml = "\
bpm: 120
tracks: {gitarre: {input: 1}}
sections:
  - {id: first, repeat: second}
  - {id: second, bars: 3, tracks: {gitarre: play}}
";
    let score = compiled(yaml);
    assert_eq!(score.section("first").unwrap().bars, 3);
    assert_eq!(states(score.section("first").unwrap()), vec![("gitarre", TrackState::Play)]);
}

#[test]
fn tracks_a_section_does_not_mention_are_stopped() {
    let yaml = "\
bpm: 120
tracks:
  gitarre: {input: 1}
  voice:   {input: 2}
  bass:    {input: 3}
sections:
  - id: nur_gitarre
    bars: 4
    tracks: {gitarre: record}
  - id: gar_nichts
    bars: 4
";
    let score = compiled(yaml);
    assert_eq!(
        states(&score.sections[0]),
        vec![
            ("gitarre", TrackState::Record),
            ("voice", TrackState::Stop),
            ("bass", TrackState::Stop)
        ]
    );
    assert!(
        states(&score.sections[1]).iter().all(|(_, state)| *state == TrackState::Stop),
        "eine Sektion ohne 'tracks' stoppt alles"
    );
}

#[test]
fn section_defaults_are_no_autorelease_and_loop_quantisation() {
    let score = compiled("bpm: 120\ntracks: {a: {input: 1}}\nsections: [{id: x, bars: 4}]\n");
    assert!(!score.sections[0].autorelease);
    assert_eq!(score.sections[0].quantize, Quantize::Loop);
}

#[test]
fn every_section_remembers_the_line_it_starts_on() {
    let score = compiled(HENRY_GROUP);
    let text = HENRY_GROUP.lines().collect::<Vec<_>>();
    for section in &score.sections {
        let line = section.source_line.expect("source_line") as usize;
        assert!(
            text[line - 1].contains(&format!("id: {}", section.id)),
            "Sektion '{}' zeigt auf Zeile {line}: {:?}",
            section.id,
            text[line - 1]
        );
    }
}

// ---------------------------------------------------------------------------------------------
// Time signature
// ---------------------------------------------------------------------------------------------

#[test]
fn odd_time_signatures_become_beats_per_bar_and_note_value() {
    for (written, beats, unit) in [("3/4", 3, 4), ("7/8", 7, 8), ("4/4", 4, 4), ("12/16", 12, 16)] {
        let yaml = format!(
            "bpm: 120\ntime_signature: \"{written}\"\ntracks: {{a: {{input: 1}}}}\nsections: [{{id: x, bars: 1}}]\n"
        );
        let score = compiled(&yaml);
        assert_eq!((score.beats_per_bar, score.beat_unit), (beats, unit), "{written}");
        assert_eq!(score.time_signature, written);
        assert_eq!(score.signature().beats_per_bar, beats);
        assert_eq!(score.signature().beat_unit, unit);
    }
}

#[test]
fn beats_per_bar_alone_still_works_and_means_quarters() {
    let score = compiled("bpm: 120\nbeats_per_bar: 5\ntracks: {a: {input: 1}}\nsections: [{id: x, bars: 1}]\n");
    assert_eq!((score.beats_per_bar, score.beat_unit), (5, 4));
    assert_eq!(score.time_signature, "5/4");
}

#[test]
fn a_time_signature_contradicting_beats_per_bar_is_refused_at_the_alias_line() {
    let yaml = "\
bpm: 120
time_signature: 3/4
beats_per_bar: 4
tracks: {a: {input: 1}}
sections: [{id: x, bars: 1}]
";
    let err = errors(yaml);
    assert_eq!(err.issues.len(), 1);
    assert_eq!(err.issues[0].line, Some(3));
    assert!(err.issues[0].message.contains("widerspricht"), "{}", err);
}

// ---------------------------------------------------------------------------------------------
// Diagnostics: exact positions
// ---------------------------------------------------------------------------------------------

#[test]
fn an_unknown_track_in_a_section_suggests_the_right_name() {
    //                                                   1234567890
    let yaml = "\
bpm: 120
tracks:
  bass: {input: 1}
sections:
  - id: bridge
    bars: 4
    tracks:
      bss: play
";
    let err = errors(yaml);
    assert_eq!(err.issues.len(), 1);
    let issue = &err.issues[0];
    assert_eq!((issue.line, issue.column), (Some(8), Some(7)));
    assert_eq!(
        issue.message,
        "Sektion 'bridge' referenziert Track 'bss', den es nicht gibt."
    );
    assert_eq!(issue.suggestion.as_deref(), Some("meintest du 'bass'?"));
}

#[test]
fn an_unknown_track_without_a_close_match_lists_the_known_ones() {
    let yaml = "\
bpm: 120
tracks: {bass: {input: 1}, gitarre: {input: 2}}
sections:
  - {id: a, bars: 4, tracks: {schlagzeug: play}}
";
    let err = errors(yaml);
    assert_eq!(err.issues[0].suggestion.as_deref(), Some("Bekannte Tracks: bass, gitarre."));
}

#[test]
fn an_unknown_track_state_is_reported_at_the_value() {
    let yaml = "\
bpm: 120
tracks: {bass: {input: 1}}
sections:
  - id: a
    bars: 4
    tracks:
      bass: recrd
";
    let err = errors(yaml);
    assert_eq!(err.issues.len(), 1);
    assert_eq!(err.issues[0].line, Some(7));
    assert_eq!(err.issues[0].column, Some(13), "zeigt auf den Wert, nicht auf den Schluessel");
    assert!(err.issues[0].message.contains("Unbekannter Zustand 'recrd'"), "{}", err);
    assert_eq!(err.issues[0].suggestion.as_deref(), Some("meintest du 'record'?"));
}

#[test]
fn an_unknown_repeat_target_suggests_a_section() {
    let yaml = "\
bpm: 120
tracks: {bass: {input: 1}}
sections:
  - {id: verse, bars: 4}
  - {id: outro, repeat: vrse}
";
    let err = errors(yaml);
    assert_eq!(err.issues.len(), 1);
    assert_eq!(err.issues[0].line, Some(5));
    assert!(err.issues[0].message.contains("unbekannte Sektion 'vrse'"), "{}", err);
    assert_eq!(err.issues[0].suggestion.as_deref(), Some("meintest du 'verse'?"));
}

/// The one that must not hang: `a` repeats `b`, `b` repeats `a`.
#[test]
fn a_repeat_cycle_is_reported_and_does_not_hang() {
    let yaml = "\
bpm: 120
tracks: {bass: {input: 1}}
sections:
  - {id: a, repeat: b}
  - {id: b, repeat: a}
";
    let err = errors(yaml);
    assert_eq!(err.issues.len(), 2, "beide Enden des Zyklus werden genannt: {err}");
    assert_eq!(err.issues[0].line, Some(4));
    assert_eq!(err.issues[1].line, Some(5));
    assert!(err.issues[0].message.contains("zirkulaer"), "{}", err);
}

#[test]
fn a_section_repeating_itself_is_reported() {
    let yaml = "bpm: 120\ntracks: {b: {input: 1}}\nsections:\n  - {id: a, repeat: a}\n";
    let err = errors(yaml);
    assert_eq!(err.issues.len(), 1);
    assert_eq!(err.issues[0].line, Some(4));
    assert!(err.issues[0].message.contains("zirkulaer"), "{}", err);
}

#[test]
fn a_longer_cycle_is_reported_too() {
    let yaml = "\
bpm: 120
tracks: {b: {input: 1}}
sections:
  - {id: a, repeat: b}
  - {id: b, repeat: c}
  - {id: c, repeat: a}
";
    let err = errors(yaml);
    assert_eq!(err.issues.len(), 3);
    assert!(err.issues.iter().all(|i| i.message.contains("zirkulaer")), "{}", err);
}

#[test]
fn an_unknown_field_suggests_the_right_one() {
    let yaml = "\
bpm: 120
tracks: {bass: {input: 1}}
sections:
  - id: a
    barz: 4
";
    let err = errors(yaml);
    let unknown = err
        .issues
        .iter()
        .find(|i| i.message.contains("Unbekanntes Feld"))
        .unwrap_or_else(|| panic!("kein Feld-Fehler in {err}"));
    assert_eq!((unknown.line, unknown.column), (Some(5), Some(5)));
    assert_eq!(unknown.suggestion.as_deref(), Some("meintest du 'bars'?"));
}

#[test]
fn an_unknown_root_field_lists_the_allowed_ones() {
    let yaml = "bpm: 120\ntempo_map: 3\ntracks: {b: {input: 1}}\nsections: [{id: a, bars: 1}]\n";
    let err = errors(yaml);
    assert_eq!(err.issues[0].line, Some(2));
    assert!(err.issues[0].message.contains("Unbekanntes Feld 'tempo_map'"), "{}", err);
    assert!(err.issues[0].suggestion.as_deref().unwrap().starts_with("Erlaubt: title, bpm"));
}

#[test]
fn the_retired_ableton_fields_say_what_replaced_them() {
    let yaml = "\
bpm: 120
tracks:
  gitarre: {ableton_track: 0}
  voice: {group: voice, layers: 3, reserve: 1}
sections: [{id: a, bars: 1}]
";
    let err = errors(yaml);
    let lines: Vec<_> = err.issues.iter().map(|i| i.line).collect();
    assert_eq!(lines, vec![Some(3), Some(4), Some(4), Some(4)]);
    assert!(err.issues[0].message.contains("'ableton_track' gibt es nicht mehr"), "{}", err);
    assert!(err.issues[0].suggestion.as_deref().unwrap().contains("input"));
    // No follow-up "Track 'gitarre' hat kein 'input'": one mistake, one message.
    assert!(!err.to_string().contains("hat kein 'input'"), "{}", err);
}

#[test]
fn a_missing_required_field_is_named() {
    let err = errors("tracks: {b: {input: 1}}\nsections: [{id: a, bars: 1}]\n");
    assert_eq!(err.issues.len(), 1);
    assert!(err.issues[0].message.contains("Pflichtfeld 'bpm' fehlt"), "{}", err);
    assert_eq!(err.issues[0].suggestion.as_deref(), Some("z. B. 'bpm: 120'."));
}

#[test]
fn a_track_without_an_input_is_told_what_to_write() {
    let yaml = "bpm: 120\ntracks:\n  gitarre: {monitor: true}\nsections: [{id: a, bars: 1}]\n";
    let err = errors(yaml);
    assert_eq!(err.issues.len(), 1);
    assert_eq!(err.issues[0].line, Some(3));
    assert!(err.issues[0].message.contains("hat kein 'input'"), "{}", err);
}

#[test]
fn values_out_of_range_name_the_range() {
    let cases = [
        ("bpm: 12\ntracks: {a: {input: 1}}\nsections: [{id: x, bars: 1}]\n", 1, "'bpm'"),
        ("bpm: 120\ntracks: {a: {input: 0}}\nsections: [{id: x, bars: 1}]\n", 2, "'input'"),
        ("bpm: 120\ntracks: {a: {input: 1}}\nsections: [{id: x, bars: 0}]\n", 3, "'bars'"),
    ];
    for (yaml, line, needle) in cases {
        let err = errors(yaml);
        assert_eq!(err.issues[0].line, Some(line), "{yaml}\n{err}");
        assert!(err.issues[0].message.contains(needle), "{err}");
        assert!(err.issues[0].message.contains("zwischen"), "{err}");
    }
}

#[test]
fn a_wrong_type_says_what_was_expected_and_what_was_found() {
    let yaml = "\
bpm: 120
tracks: {a: {input: 1}}
sections:
  - id: x
    bars: 4
    autorelease: yes
";
    let err = errors(yaml);
    assert_eq!(err.issues[0].line, Some(6));
    assert!(err.issues[0].message.contains("muss true oder false sein"), "{}", err);
    assert!(err.issues[0].message.contains("'yes'"), "{}", err);
    assert!(err.issues[0].suggestion.as_deref().unwrap().contains("true oder false"));
}

#[test]
fn a_quoted_number_is_a_type_error_rather_than_a_silent_success() {
    let err = errors("bpm: \"120\"\ntracks: {a: {input: 1}}\nsections: [{id: x, bars: 1}]\n");
    assert_eq!(err.issues[0].line, Some(1));
    assert!(err.issues[0].message.contains("muss eine Zahl sein"), "{}", err);
}

#[test]
fn a_section_without_bars_or_repeat_is_refused() {
    let err = errors("bpm: 120\ntracks: {a: {input: 1}}\nsections:\n  - id: leer\n");
    assert_eq!(err.issues.len(), 1);
    assert_eq!(err.issues[0].line, Some(4));
    assert!(err.issues[0].message.contains("weder 'bars' noch 'repeat'"), "{}", err);
}

#[test]
fn duplicate_ids_and_duplicate_keys_are_caught() {
    let yaml = "\
bpm: 120
tracks: {a: {input: 1}}
sections:
  - {id: intro, bars: 4}
  - {id: intro, bars: 8}
";
    let err = errors(yaml);
    assert_eq!(err.issues[0].line, Some(5));
    assert!(err.issues[0].message.contains("doppelt vergeben"), "{}", err);

    let yaml = "\
bpm: 120
tracks: {a: {input: 1}}
sections:
  - id: intro
    bars: 4
    bars: 8
";
    let err = errors(yaml);
    assert_eq!(err.issues[0].line, Some(6));
    assert!(err.issues[0].message.contains("mehrfach vor"), "{}", err);
}

#[test]
fn a_midi_binding_needs_exactly_one_of_note_and_cc() {
    let base = "bpm: 120\ntracks: {a: {input: 1}}\nsections: [{id: x, bars: 1}]\n";
    let err = errors(&format!("{base}midi:\n  next_section: {{}}\n"));
    assert!(err.issues[0].message.contains("weder 'note' noch 'cc'"), "{}", err);

    let err = errors(&format!("{base}midi:\n  next_section: {{note: 36, cc: 64}}\n"));
    assert!(err.issues[0].message.contains("gleichzeitig"), "{}", err);

    let err = errors(&format!("{base}midi:\n  next_section: {{cc: 200}}\n"));
    assert!(err.issues[0].message.contains("zwischen 0 und 127"), "{}", err);
}

#[test]
fn a_midi_channel_shows_up_in_the_id() {
    let yaml = "\
bpm: 120
tracks: {a: {input: 1}}
midi:
  next_section: {note: 36, channel: 10}
sections: [{id: x, bars: 1}]
";
    assert_eq!(compiled(yaml).midi.get("next_section").unwrap().id, "ch10.note36");
}

#[test]
fn more_tracks_than_the_engine_carries_are_refused() {
    let tracks: String = (1..=9).map(|i| format!("  t{i}: {{input: 1}}\n")).collect();
    let err = errors(&format!("bpm: 120\ntracks:\n{tracks}sections: [{{id: x, bars: 1}}]\n"));
    assert!(err.issues[0].message.contains("hoechstens 8"), "{}", err);
}

// ---------------------------------------------------------------------------------------------
// Several errors at once - the property that makes the compiler usable
// ---------------------------------------------------------------------------------------------

#[test]
fn every_mistake_in_a_file_is_reported_in_one_pass() {
    let yaml = "\
bpm: 120
tracks:
  gitarre: {input: 2}
  voice: {input: 1}
sections:
  - id: intro
    barz: 4
    tracks:
      gitare: record
  - id: verse
    bars: 4
    tracks:
      voice: recrd
  - id: outro
    repeat: vrse
";
    let err = errors(yaml);
    let lines: Vec<_> = err.issues.iter().map(|i| i.line).collect();
    assert_eq!(
        lines,
        vec![Some(7), Some(9), Some(13), Some(15)],
        "vier unabhaengige Fehler, sortiert nach Position:\n{err}"
    );
    let suggestions: Vec<_> = err.issues.iter().map(|i| i.suggestion.as_deref()).collect();
    assert_eq!(
        suggestions,
        vec![
            Some("meintest du 'bars'?"),
            Some("meintest du 'gitarre'?"),
            Some("meintest du 'record'?"),
            Some("meintest du 'verse'?"),
        ]
    );
}

#[test]
fn several_broken_tracks_are_all_reported() {
    let yaml = "\
bpm: 120
tracks:
  a: {input: 0}
  b: {inpt: 1}
  c: {input: 1, monitor: 3}
sections: [{id: x, bars: 1}]
";
    let err = errors(yaml);
    assert_eq!(err.issues.iter().map(|i| i.line).collect::<Vec<_>>(), vec![Some(3), Some(4), Some(5)]);
    assert_eq!(err.issues[1].suggestion.as_deref(), Some("meintest du 'input'?"));
}

// ---------------------------------------------------------------------------------------------
// Syntax level
// ---------------------------------------------------------------------------------------------

#[test]
fn a_yaml_syntax_error_reports_a_line_and_a_hint() {
    let err = errors("bpm: 120\ntracks:\n\tgitarre: {input: 1}\nsections: []\n");
    assert_eq!(err.issues.len(), 1, "{err}");
    assert!(err.issues[0].message.contains("YAML-Syntaxfehler"), "{}", err);
    assert_eq!(err.issues[0].line, Some(3));
}

#[test]
fn a_score_that_is_not_a_mapping_says_so() {
    let err = errors("- eine\n- Liste\n");
    assert!(err.issues[0].message.contains("muss ein Mapping"), "{}", err);
}

// ---------------------------------------------------------------------------------------------
// Stereo tracks and the pan
// ---------------------------------------------------------------------------------------------

/// The two shapes `input` may have, side by side in one score: a number is a mono track, a pair of
/// numbers is a stereo one. The channel count follows from that and from nothing else.
#[test]
fn a_pair_of_inputs_makes_a_stereo_track() {
    let score = compiled(
        "bpm: 120\n\
         tracks:\n\
        \x20 stimme:  {input: 1}\n\
        \x20 klavier: {input: [3, 4]}\n\
        \x20 flaeche: {input: [7, 2], pan: -0.5}\n\
         sections: [{id: a, bars: 1}]\n",
    );
    let stimme = score.track("stimme").unwrap();
    assert_eq!((stimme.input, stimme.input_channel), (1, 0));
    assert_eq!(stimme.input_right, None);
    assert_eq!(stimme.channels, 1);
    assert_eq!(stimme.pan, 0.0, "ohne Angabe sitzt ein Track in der Mitte");
    assert_eq!(stimme.track_input(), TrackInput::Mono(0));

    let klavier = score.track("klavier").unwrap();
    assert_eq!((klavier.input, klavier.input_channel), (3, 2));
    assert_eq!((klavier.input_right, klavier.input_channel_right), (Some(4), Some(3)));
    assert_eq!(klavier.channels, 2);
    assert_eq!(
        klavier.track_input(),
        TrackInput::Stereo { left: 2, right: 3 }
    );

    // The two halves need not be adjacent, and they need not be in order - a router puts them
    // where it likes.
    let flaeche = score.track("flaeche").unwrap();
    assert_eq!(
        flaeche.track_input(),
        TrackInput::Stereo { left: 6, right: 1 }
    );
    assert_eq!(flaeche.pan, -0.5);
}

/// Every way of writing the input wrong gets its own German sentence with a position, and none of
/// them silently produces half a stereo track.
#[test]
fn a_malformed_input_or_pan_is_refused_in_german() {
    for (yaml, needle) in [
        ("tracks: {a: {input: [3]}}", "genau zwei Eingaenge"),
        ("tracks: {a: {input: [3, 4, 5]}}", "genau zwei Eingaenge"),
        ("tracks: {a: {input: [3, 3]}}", "zweimal Eingang 3"),
        ("tracks: {a: {input: [3, links]}}", "ganze Zahl"),
        ("tracks: {a: {input: [0, 4]}}", "zwischen 1 und"),
        ("tracks: {a: {input: 1, pan: 2}}", "zwischen -1 und 1"),
        ("tracks: {a: {input: 1, pan: links}}", "muss eine Zahl sein"),
    ] {
        let yaml = format!("bpm: 120\n{yaml}\nsections: [{{id: x, bars: 1}}]\n");
        let err = compile_score(&yaml).expect_err(&yaml);
        assert!(
            err.to_string().contains(needle),
            "\"{needle}\" fehlt in: {err}"
        );
        assert!(err.issues[0].line.is_some(), "ohne Zeilennummer: {err}");
    }
}

/// A track with two mistakes reports both, which is the rule the whole reader is built on.
#[test]
fn a_track_with_a_bad_input_and_a_bad_pan_reports_both() {
    let err = compile_score("bpm: 120\ntracks: {a: {input: [1], pan: 9}}\nsections: [{id: x, bars: 1}]\n")
        .expect_err("beides falsch");
    assert_eq!(err.issues.len(), 2, "{err}");
}

// ---------------------------------------------------------------------------------------------
// Latency per track
// ---------------------------------------------------------------------------------------------

/// A track may bring its own latency compensation, in the two halves the engine keeps apart: the
/// measured number and the manual surcharge. A track that says nothing carries neither, which is
/// how it ends up on the engine's global default.
#[test]
fn a_track_can_bring_its_own_latency_and_its_own_surcharge() {
    let score = compiled(
        "bpm: 120\n\
         tracks:\n\
        \x20 stimme:    {input: 1}\n\
        \x20 cantabile: {input: [5, 6], latency: 512, latency_trim: 96}\n\
        \x20 router:    {input: 7, latency_trim: -40}\n\
         sections: [{id: a, bars: 1}]\n",
    );

    let stimme = score.track("stimme").unwrap();
    assert_eq!(stimme.latency, None, "ohne Angabe gilt die Vorgabe der Engine");
    assert_eq!(stimme.latency_trim, 0);
    assert!(stimme.track_latency().inherits());
    assert_eq!(stimme.track_latency().resolve(827), 827);

    let cantabile = score.track("cantabile").unwrap();
    assert_eq!(cantabile.latency, Some(512));
    assert_eq!(cantabile.latency_trim, 96);
    assert_eq!(cantabile.track_latency().resolve(827), 608);

    // Only a surcharge: the base stays the engine's default, which is the case a source nobody can
    // measure needs.
    let router = score.track("router").unwrap();
    assert_eq!(router.latency, None);
    assert!(router.track_latency().inherits());
    assert_eq!(router.track_latency().resolve(827), 787);
}

/// Both fields are checked, and the surcharge is the one value in the format that may be negative -
/// a digital return can arrive earlier than the converter path the default was measured on.
#[test]
fn a_malformed_latency_is_refused_in_german() {
    for (yaml, needle) in [
        ("tracks: {a: {input: 1, latency: -5}}", "zwischen 0 und"),
        ("tracks: {a: {input: 1, latency: 999999}}", "zwischen 0 und"),
        ("tracks: {a: {input: 1, latency: viel}}", "ganze Zahl"),
        ("tracks: {a: {input: 1, latency: 512.5}}", "ganze Zahl"),
        ("tracks: {a: {input: 1, latency_trim: 999999}}", "zwischen -96000 und"),
        ("tracks: {a: {input: 1, latency_trim: viel}}", "ganze Zahl"),
    ] {
        let yaml = format!("bpm: 120\n{yaml}\nsections: [{{id: x, bars: 1}}]\n");
        let err = compile_score(&yaml).expect_err(&yaml);
        assert!(
            err.to_string().contains(needle),
            "\"{needle}\" fehlt in: {err}"
        );
        assert!(err.issues[0].line.is_some(), "ohne Zeilennummer: {err}");
    }

    // A negative surcharge is not a mistake.
    let score = compiled(
        "bpm: 120\ntracks: {a: {input: 1, latency_trim: -300}}\nsections: [{id: x, bars: 1}]\n",
    );
    assert_eq!(score.track("a").unwrap().latency_trim, -300);
}

/// `pan` is a known field now, so a typo next to it suggests it rather than listing the world.
#[test]
fn a_misspelt_pan_suggests_the_real_field() {
    let err = compile_score("bpm: 120\ntracks: {a: {input: 1, pann: 0.5}}\nsections: [{id: x, bars: 1}]\n")
        .expect_err("Tippfehler");
    let suggestion = err.issues[0].suggestion.as_deref().unwrap_or("");
    assert!(suggestion.contains("pan"), "{suggestion}");
}

// ---------------------------------------------------------------------------------------------
// Serialisation contract
// ---------------------------------------------------------------------------------------------

#[test]
fn the_compiled_score_serialises_to_the_documented_shape() {
    let score = compiled(EXAMPLE_SONG);
    let json = serde_json::to_value(&score).unwrap();
    assert_eq!(json["title"], "Beispiel-Song");
    assert_eq!(json["bpm"], 100.0);
    assert_eq!(json["beats_per_bar"], 4);
    assert_eq!(json["beat_unit"], 4);
    assert_eq!(json["time_signature"], "4/4");
    assert_eq!(json["tracks"][0]["name"], "gitarre");
    assert_eq!(json["tracks"][0]["input"], 2);
    assert_eq!(json["tracks"][0]["input_channel"], 1);
    assert_eq!(json["midi"]["next_section"]["id"], "ch1.cc64");
    let outro = &json["sections"][2];
    assert_eq!(outro["id"], "outro");
    assert_eq!(outro["index"], 2);
    assert_eq!(outro["bars"], 4);
    assert_eq!(outro["autorelease"], false);
    assert_eq!(outro["quantize"], "loop");
    assert_eq!(outro["tracks"]["gitarre"], "overdub");
    assert_eq!(outro["tracks"]["voice"], "play");
    assert!(outro["source_line"].is_number());

    // And back again, unchanged.
    let round_trip: super::CompiledScore = serde_json::from_value(json).unwrap();
    assert_eq!(round_trip, score);
}

#[test]
fn the_source_score_round_trips_through_serde() {
    let source = read::read_score(EXAMPLE_SONG).unwrap();
    let json = serde_json::to_string(&source).unwrap();
    let back: super::ScoreSource = serde_json::from_str(&json).unwrap();
    assert_eq!(back, source);
}

// ---------------------------------------------------------------------------------------------
// The output buses
// ---------------------------------------------------------------------------------------------

/// A score can say where a track is heard: in the room, on the headphones, or both. The two fields
/// are separate because they answer separate questions - a guide figure belongs on the headphones
/// while the singer over it belongs everywhere.
#[test]
fn a_track_can_say_which_output_buses_it_is_heard_on() {
    use crate::engine::bus::BusSend;

    let score = compiled(
        "bpm: 120\n\
         tracks:\n\
        \x20 stimme:  {input: 1}\n\
        \x20 klick_gtr: {input: 2, bus: monitor}\n\
        \x20 ansage:  {input: 3, bus: main, monitor_bus: main+monitor}\n\
        \x20 spaeter: {input: 4, bus: none}\n\
         sections: [{id: a, bars: 1}]\n",
    );

    // Nothing said: heard everywhere, and the live input in the headphones only.
    let stimme = score.track("stimme").unwrap();
    assert_eq!(stimme.loop_send(), BusSend::BOTH);
    assert_eq!(stimme.monitor_send(), BusSend::MONITOR);

    // A cue track: the musician gets it, the room never does.
    assert_eq!(score.track("klick_gtr").unwrap().loop_send(), BusSend::MONITOR);

    let ansage = score.track("ansage").unwrap();
    assert_eq!(ansage.loop_send(), BusSend::MAIN);
    assert_eq!(ansage.monitor_send(), BusSend::BOTH);

    assert_eq!(score.track("spaeter").unwrap().loop_send(), BusSend::NONE);
}

/// A bus that does not exist is a positioned German sentence, not a track that is quietly silent.
#[test]
fn an_unknown_bus_in_a_score_is_refused_in_german() {
    let err = compile_score("bpm: 120\ntracks: {a: {input: 1, bus: saal_hinten}}\nsections: [{id: x, bars: 1}]\n")
        .expect_err("muss scheitern");
    assert!(err.issues[0].message.contains("saal_hinten"), "{err}");
    let suggestion = err.issues[0].suggestion.clone().unwrap_or_default();
    assert!(suggestion.contains("monitor"), "{suggestion}");
    assert!(suggestion.contains("Klick"), "der Grund steht dabei: {suggestion}");
}
