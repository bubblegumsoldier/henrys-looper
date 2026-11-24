//! Proofs for instrument tracks in a score (`input: instrument`, `midi:`, `mixer.<track>.instrument`
//! and `inserts`) and for the reserved fields `loop` and `grid`: they compile, they say where a
//! mistake is, they round-trip through the document with their comments, and the reserved ones are
//! notes rather than errors.

use super::doc::{DocTrack, parse_doc, parse_doc_outcome, render_doc, template_with};
use super::mixer::InsertLine;
use super::model::{InstrumentSource, LoopKind};
use super::{ScoreError, compile_score, score_notes};
use crate::engine::bus::BusSend;
use crate::engine::command::Command;
use crate::engine::frame::TrackInput;
use crate::engine::runner::setup_commands;

const KEYS: &str = "\
# Klavier und Pads aus dem Rechner, Stimme vom Mikro
title: Keys
bpm: 96
time_signature: 4/4

tracks:
  keys: {input: instrument, midi: {device: \"KeyLab 49\", channel: 1}}   # das Klavier
  # Pads auf Kanal 10, nur die unteren Tasten
  drums: {input: instrument, midi: {device: MPD218, channel: 10, notes: 36-51}, monitor: false}
  voice: {input: 1, pan: -0.2}

sections:
  - id: a
    bars: 4
    tracks:
      keys: record
      drums: play
      voice: hear_through

mixer:
  tracks:
    # Klavier: DLS, ein Delay dahinter
    keys:
      gain: -3
      instrument: \"au:aumu:dls :appl\"   # Apple DLS
      inserts: [{plugin: \"au:aufx:dely:appl\", bypass: false}, {plugin: \"vst3:0123ABCD\"}]
    drums: {instrument: \"au:aumu:dls :appl\"}
";

/// The compiled score without source lines, which a re-rendered layout is allowed to move.
fn compiled(text: &str) -> super::CompiledScore {
    let mut score = compile_score(text).expect("compiles");
    for section in &mut score.sections {
        section.source_line = None;
    }
    score
}

fn issues(text: &str) -> Vec<(Option<u32>, String)> {
    match compile_score(text) {
        Ok(_) => Vec::new(),
        Err(ScoreError { issues }) => issues.into_iter().map(|i| (i.line, i.message)).collect(),
    }
}

/// The one issue of `text`, with its line.
fn one_issue(text: &str) -> (u32, String) {
    let all = issues(text);
    assert_eq!(all.len(), 1, "genau ein Fehler erwartet: {all:?}");
    (all[0].0.expect("mit Zeile"), all[0].1.clone())
}

fn with_track(line: &str) -> String {
    format!("bpm: 100\ntracks:\n  {line}\nsections:\n  - id: a\n    bars: 1\n")
}

#[test]
fn an_instrument_track_compiles_with_its_midi_routing() {
    let score = compile_score(KEYS).expect("compiles");
    let keys = score.track("keys").unwrap();
    assert_eq!(keys.track_input(), TrackInput::Instrument);
    assert_eq!(keys.channels, 2, "an instrument is always stereo");
    assert_eq!((keys.input, keys.input_channel), (0, 0));
    assert_eq!(
        keys.instrument,
        Some(InstrumentSource {
            device: Some("KeyLab 49".into()),
            channel: Some(1),
            note_low: None,
            note_high: None,
            transpose: None,
        })
    );
    // An instrument is heard in the room as well, unless the score says otherwise.
    assert_eq!(keys.monitor_send(), BusSend::BOTH);
    let drums = score.track("drums").unwrap();
    assert_eq!(drums.instrument.as_ref().unwrap().note_low, Some(36));
    assert_eq!(drums.instrument.as_ref().unwrap().note_high, Some(51));
    assert!(!drums.monitor);
    let voice = score.track("voice").unwrap();
    assert!(voice.instrument.is_none());
    assert_eq!(voice.monitor_send(), BusSend::MONITOR);

    let mixer = score.mixer.as_ref().unwrap();
    let keys_mix = mixer.tracks.get("keys").unwrap();
    assert_eq!(keys_mix.instrument.as_deref(), Some("au:aumu:dls :appl"));
    assert_eq!(
        keys_mix.inserts.as_deref(),
        Some(
            &[
                InsertLine {
                    plugin: "au:aufx:dely:appl".into(),
                    bypass: Some(false),
                    before: None,
                },
                InsertLine {
                    plugin: "vst3:0123ABCD".into(),
                    bypass: None,
                    before: None,
                },
            ][..]
        )
    );
}

#[test]
fn an_instrument_track_without_midi_listens_to_everything() {
    let score = compile_score(&with_track("piano: {input: instrument}")).unwrap();
    assert_eq!(score.tracks[0].instrument, Some(InstrumentSource::default()));
    let score = compile_score(&with_track("piano: {input: instrument, midi: {channel: all}}")).unwrap();
    assert_eq!(score.tracks[0].instrument, Some(InstrumentSource::default()));
}

#[test]
fn mistakes_around_instruments_are_reported_on_their_line() {
    let (line, message) = one_issue(&with_track("voice: {input: 1, midi: {channel: 1}}"));
    assert_eq!(line, 3);
    assert!(message.contains("has 'midi', but it records an audio input"), "{message}");

    let (line, message) = one_issue(&with_track("keys: {input: instrument, midi: {channel: 17}}"));
    assert_eq!(line, 3);
    assert!(message.contains("between 1 and 16"), "{message}");

    let (_, message) = one_issue(&with_track("keys: {input: instrument, midi: {notes: 51-36}}"));
    assert!(message.contains("goes downwards"), "{message}");
    let (_, message) = one_issue(&with_track("keys: {input: instrument, midi: {notes: 36}}"));
    assert!(message.contains("must be a range"), "{message}");
    let (_, message) = one_issue(&with_track("keys: {input: instrument, midi: {notes: 0-200}}"));
    assert!(message.contains("not a range of MIDI notes"), "{message}");
    let (_, message) = one_issue(&with_track("keys: {input: instrument, midi: {port: x}}"));
    assert!(message.contains("Unknown field 'port'"), "{message}");

    let (_, message) = one_issue(&with_track("keys: {input: instrument, latency: 300}"));
    assert!(message.contains("has no measured 'latency'"), "{message}");
    let (_, message) = one_issue(&with_track("keys: {input: keyboard}"));
    assert!(message.contains("'keyboard' is not an input"), "{message}");

    let (_, message) = one_issue(&with_track("voice: {input: 1, loop: midi}"));
    assert!(message.contains("a MIDI loop needs an instrument track"), "{message}");
    let (_, message) = one_issue(&with_track("keys: {input: instrument, loop: notes}"));
    assert!(message.contains("not a loop kind"), "{message}");
    let (_, message) = one_issue(&with_track("keys: {input: instrument, grid: 1/32}"));
    assert!(message.contains("not a note grid"), "{message}");

    let text = "bpm: 100\ntracks:\n  voice: {input: 1}\nsections:\n  - id: a\n    bars: 1\n\
                mixer:\n  tracks:\n    voice: {instrument: \"au:aumu:dls :appl\"}\n";
    let (line, message) = one_issue(text);
    assert_eq!(line, 9);
    assert!(message.contains("records an audio input"), "{message}");

    let nine = (0..9).map(|_| "{plugin: x}").collect::<Vec<_>>().join(", ");
    let text = format!(
        "bpm: 100\ntracks:\n  voice: {{input: 1}}\nsections:\n  - id: a\n    bars: 1\n\
         mixer:\n  tracks:\n    voice: {{inserts: [{nine}]}}\n"
    );
    let (_, message) = one_issue(&text);
    assert!(message.contains("a track has 8 slots"), "{message}");
    let text = "bpm: 100\ntracks:\n  voice: {input: 1}\nsections:\n  - id: a\n    bars: 1\n\
                mixer:\n  tracks:\n    voice: {inserts: [{bypass: true}]}\n";
    let (_, message) = one_issue(text);
    assert!(message.contains("has no 'plugin'"), "{message}");
}

/// `loop: midi` and `grid:` compile - for the track, and per clip in a section - and the engine has
/// them, so they are no notes any more.
#[test]
fn loop_and_grid_compile_without_notes() {
    let text = "bpm: 100\ntracks:\n  keys: {input: instrument, loop: midi, grid: 1/16}\n\
                sections:\n  - id: a\n    bars: 1\n    tracks:\n      keys: {state: record, grid: 1/8t}\n";
    let score = compile_score(text).expect("reserved fields are no errors");
    assert_eq!(score.tracks[0].loop_kind, LoopKind::Midi);
    assert_eq!(score.tracks[0].grid.as_deref(), Some("1/16"));
    assert_eq!(
        score.sections[0].tracks.get("keys"),
        Some(&super::TrackState::Record)
    );
    assert_eq!(
        score.sections[0].grids.get("keys").map(String::as_str),
        Some("1/8t"),
        "die Raster-Ueberschreibung des Clips"
    );
    assert!(score_notes(text).is_empty());
    let outcome = parse_doc_outcome(text);
    assert!(outcome.ok);
    assert!(outcome.notes.is_empty());
    // A score without them has none.
    assert!(score_notes(KEYS).is_empty());
    // `quantize:` of a section is something else entirely and stays what it was.
    let (_, message) = one_issue(
        "bpm: 100\ntracks:\n  v: {input: 1}\nsections:\n  - id: a\n    bars: 1\n    quantize: 1/16\n",
    );
    assert!(message.contains("Unknown quantize value"), "{message}");
}

#[test]
fn a_clip_with_settings_still_needs_its_state() {
    let text = "bpm: 100\ntracks:\n  keys: {input: instrument}\nsections:\n  - id: a\n    bars: 1\n    \
                tracks:\n      keys: {grid: 1/8}\n";
    let (line, message) = one_issue(text);
    assert_eq!(line, 8);
    assert!(message.contains("has no 'state'"), "{message}");
}

#[test]
fn instruments_round_trip_through_the_document_with_their_comments() {
    let doc = parse_doc(KEYS).expect("parses");
    let keys = &doc.tracks[0];
    assert!(keys.instrument);
    assert_eq!(keys.midi.as_ref().unwrap().device.as_deref(), Some("KeyLab 49"));
    assert_eq!(keys.comments.after.as_deref(), Some("das Klavier"));
    let mix = &doc.mixer.as_ref().unwrap().tracks[0];
    assert_eq!(mix.mix.instrument.as_deref(), Some("au:aumu:dls :appl"));
    assert_eq!(mix.mix.inserts.as_ref().unwrap().len(), 2);

    let text = render_doc(&doc);
    // Nothing the compiler sees changes, every comment survives, a second pass is identical.
    assert_eq!(compiled(&text), compiled(KEYS));
    for comment in [
        "Klavier und Pads aus dem Rechner",
        "das Klavier",
        "Pads auf Kanal 10",
        "Apple DLS",
        "ein Delay dahinter",
    ] {
        assert!(text.contains(comment), "lost '{comment}':\n{text}");
    }
    assert_eq!(render_doc(&parse_doc(&text).unwrap()), text);
    assert!(text.contains(
        "  keys: {input: instrument, midi: {device: KeyLab 49, channel: 1}}  # das Klavier\n"
    ), "{text}");
    assert!(text.contains(
        "  drums: {input: instrument, midi: {device: MPD218, channel: 10, notes: 36-51}, monitor: false}\n"
    ), "{text}");
    assert!(text.contains("      instrument: au:aumu:dls :appl  # Apple DLS\n"), "{text}");
    assert!(text.contains(
        "      inserts: [{plugin: au:aufx:dely:appl, bypass: false}, {plugin: vst3:0123ABCD}]\n"
    ), "{text}");
}

#[test]
fn reserved_fields_round_trip_as_written() {
    let text = "bpm: 100\n\ntracks:\n  keys: {input: instrument, loop: midi, grid: 1/16}\n\n\
                sections:\n  - id: a\n    bars: 1\n    tracks:\n      keys: {state: record, grid: 1/8t}  # dichter\n";
    let doc = parse_doc(text).unwrap();
    assert_eq!(doc.tracks[0].loop_kind.as_deref(), Some("midi"));
    assert_eq!(doc.sections[0].tracks[0].grid.as_deref(), Some("1/8t"));
    assert_eq!(render_doc(&doc), text);
}

#[test]
fn an_old_score_reads_and_renders_unchanged() {
    for text in [
        include_str!("../../../examples/henry-3-4.rust.yaml"),
        include_str!("../../../examples/solo-loop-4-4.yaml"),
        include_str!("../../../examples/waltz-three-tracks-3-4.yaml"),
    ] {
        let doc = parse_doc(text).unwrap();
        assert!(doc.tracks.iter().all(|t| !t.instrument && t.midi.is_none()));
        let again = render_doc(&doc);
        assert_eq!(compiled(&again), compiled(text));
        assert!(score_notes(text).is_empty());
    }
}

#[test]
fn a_new_score_takes_the_instrument_tracks_of_the_setup() {
    let text = template_with(
        vec![
            DocTrack::new("voice", 1),
            DocTrack::instrument(
                "keys",
                Some(InstrumentSource {
                    device: Some("Testkeys".into()),
                    channel: Some(2),
                    ..InstrumentSource::default()
                }),
            ),
        ],
        vec![("keys".into(), "au:aumu:dls :appl".into())],
    );
    let score = compile_score(&text).expect("template compiles");
    let keys = score.track("keys").unwrap();
    assert_eq!(keys.track_input(), TrackInput::Instrument);
    assert_eq!(keys.instrument.as_ref().unwrap().channel, Some(2));
    assert_eq!(
        score.mixer.unwrap().tracks.get("keys").unwrap().instrument.as_deref(),
        Some("au:aumu:dls :appl")
    );
    assert!(text.contains("keys: {input: instrument, midi: {device: Testkeys, channel: 2}}"), "{text}");
}

/// Monitoring of an instrument track is not the sections' business: the score's setup starts it
/// as `monitor` says - the keys heard, the pads with `monitor: false` not - and from then on only
/// the musician's switch changes it.
#[test]
fn the_score_setup_starts_an_instrument_tracks_monitoring_as_the_score_says() {
    let score = compile_score(KEYS).unwrap();
    let monitors: Vec<(usize, bool)> = setup_commands(&score)
        .into_iter()
        .filter_map(|c| match c {
            Command::SetMonitor { track, on } => Some((track, on)),
            _ => None,
        })
        .collect();
    assert_eq!(monitors, vec![(0, true), (1, false), (2, false)]);
    for section in 0..score.sections.len() {
        let plan = crate::engine::runner::monitor_plan(&score, Some(section));
        assert!(!plan[0] && !plan[1], "the sections never switch an instrument track");
    }
}

/// The command line cannot play an instrument; it says so before opening a device.
#[test]
fn the_command_line_refuses_an_instrument_score_in_words() {
    let message = crate::engine::score_cli::refuse_instruments(&compile_score(KEYS).unwrap())
        .unwrap_err();
    assert!(message.contains("keys, drums"), "{message}");
    assert!(message.contains("Play this score in the app"), "{message}");
    let audio = compile_score(include_str!("../../../examples/solo-loop-4-4.yaml")).unwrap();
    assert!(crate::engine::score_cli::refuse_instruments(&audio).is_ok());
}

/// The shipped example with an instrument track reads, round-trips and names the DLS piano.
#[test]
fn the_piano_example_round_trips() {
    let text = include_str!("../../../examples/piano-loop-4-4.yaml");
    let score = compile_score(text).unwrap();
    assert!(score.track("piano").unwrap().is_instrument());
    assert!(!score.track("voice").unwrap().is_instrument());
    let piano = score.mixer.as_ref().unwrap().tracks.get("piano").unwrap();
    assert_eq!(piano.instrument.as_deref(), Some("au:aumu:dls :appl"));
    let again = render_doc(&parse_doc(text).unwrap());
    assert_eq!(compiled(&again), compiled(text));
    assert_eq!(render_doc(&parse_doc(&again).unwrap()), again);
    assert!(score_notes(text).is_empty());
}

/// `midi: {transpose: 12}`: parsed, range-checked, and written back by the document renderer.
#[test]
fn transpose_is_part_of_the_midi_field() {
    let text = "bpm: 100\ntracks:\n  drums: {input: instrument, midi: {channel: 10, notes: 36-51, transpose: 12}}\n\
                sections:\n  - id: a\n    bars: 1\n";
    let score = compile_score(text).expect("transpose ist erlaubt");
    let midi = score.tracks[0].instrument.clone().unwrap();
    assert_eq!(midi.transpose, Some(12));
    assert_eq!((midi.note_low, midi.note_high), (Some(36), Some(51)));
    let wrong = text.replace("transpose: 12", "transpose: 60");
    assert!(compile_score(&wrong).is_err(), "mehr als 48 Halbtoene");
    let outcome = parse_doc_outcome(text);
    assert!(outcome.ok);
    let doc = outcome.doc.expect("Dokument");
    assert_eq!(doc.tracks[0].midi.as_ref().unwrap().transpose, Some(12));
    let rendered = crate::score::doc::render_doc(&doc);
    assert!(rendered.contains("transpose: 12"), "{rendered}");
}

// ---- plugins inside the built-in chain (`before:`) ------------------------------------------

const PLACED: &str = "bpm: 100\ntracks:\n  voice: {input: 1}\nsections:\n  - id: a\n    bars: 1\n\n\
    mixer:\n  tracks:\n    voice:\n      inserts: [{plugin: vst3:IRDX, before: octaver}, \
    {plugin: vst3:DEESS, bypass: true, before: comp}, {plugin: au:aufx:dely:appl}]\n";

/// `before:` names the built-in effect a plugin sits in front of; absent is the end, as in every
/// score written before positions existed.
#[test]
fn inserts_can_sit_in_front_of_a_built_in_effect() {
    use crate::engine::fx::FxSlot;
    use crate::engine::insert::InsertPosition;
    let score = compile_score(PLACED).expect("compiles");
    let inserts = score.mixer.as_ref().unwrap().tracks.get("voice").unwrap().inserts.clone().unwrap();
    let positions: Vec<InsertPosition> = inserts.iter().map(InsertLine::position).collect();
    assert_eq!(
        positions,
        vec![
            InsertPosition::before(FxSlot::Octaver),
            InsertPosition::before(FxSlot::Comp),
            InsertPosition::END
        ]
    );
    assert_eq!(inserts[1].bypass, Some(true));

    // An old score: no `before`, everything at the end.
    let old = compile_score(KEYS).unwrap();
    let keys = old.mixer.as_ref().unwrap().tracks.get("keys").unwrap();
    assert!(keys.inserts.as_ref().unwrap().iter().all(|i| i.position() == InsertPosition::END));
}

#[test]
fn a_placed_insert_round_trips_through_the_document_and_json() {
    let doc = parse_doc(PLACED).expect("parses");
    let text = render_doc(&doc);
    assert!(
        text.contains(
            "      inserts: [{plugin: vst3:IRDX, before: octaver}, {plugin: vst3:DEESS, bypass: true, \
             before: comp}, {plugin: au:aufx:dely:appl}]\n"
        ),
        "{text}"
    );
    assert_eq!(compiled(&text), compiled(PLACED));
    assert_eq!(render_doc(&parse_doc(&text).unwrap()), text);

    // The wire format (`mixer_take`, `mixer_apply`): `before` only when set, an old entry reads.
    let line = InsertLine {
        plugin: "vst3:IRDX".into(),
        bypass: None,
        before: Some("eq".into()),
    };
    let json = serde_json::to_string(&line).unwrap();
    assert_eq!(json, r#"{"plugin":"vst3:IRDX","before":"eq"}"#);
    assert_eq!(serde_json::from_str::<InsertLine>(&json).unwrap(), line);
    let old: InsertLine = serde_json::from_str(r#"{"plugin":"x","bypass":true}"#).unwrap();
    assert_eq!(old.before, None);
}

/// A name that is no built-in effect, and a list out of signal order, are errors with a line.
#[test]
fn a_wrong_position_is_an_error_with_its_line() {
    let wrong = PLACED.replace("before: comp", "before: chorus");
    let (line, message) = one_issue(&wrong);
    assert_eq!(line, 11);
    assert!(message.contains("'chorus' is not one of the allowed words"), "{message}");

    let swapped = PLACED.replace(
        "[{plugin: vst3:IRDX, before: octaver}, {plugin: vst3:DEESS, bypass: true, before: comp}",
        "[{plugin: vst3:DEESS, bypass: true, before: comp}, {plugin: vst3:IRDX, before: octaver}",
    );
    let (_, message) = one_issue(&swapped);
    assert!(message.contains("plugin 2 of") && message.contains("in signal order"), "{message}");
    let after_end = PLACED.replace(
        "{plugin: au:aufx:dely:appl}]",
        "{plugin: au:aufx:dely:appl}, {plugin: x, before: eq}]",
    );
    assert_eq!(issues(&after_end).len(), 1, "nothing may follow the end inside the chain");
}
