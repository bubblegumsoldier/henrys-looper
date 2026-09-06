//! Offline proofs for the whole MIDI layer.
//!
//! **Not one of these needs a controller.** That is the reason the byte decoding, the address tree,
//! the mapping and the resolution live apart from [`super::input`]: what can be proved at a desk
//! with no hardware gets proved there, and what is left over is opening a port.

use std::path::PathBuf;

use super::*;
use crate::engine::command::Command;
use crate::engine::fx::{FxParam, FxPreset, FxSlot};
use crate::engine::runner::TRANSPORT_BELONGS_TO_SCORE;
use crate::score::compile_score;

// ---------------------------------------------------------------------------------------------
// 1. Bytes to events
// ---------------------------------------------------------------------------------------------

#[test]
fn note_on_note_off_and_control_change_come_out_of_the_bytes() {
    let mut decoder = MidiDecoder::new();
    let events = decoder.decode(&[0x90, 36, 100, 0x80, 36, 64, 0xB0, 64, 127]);
    assert_eq!(
        events,
        vec![
            MidiEvent::NoteOn {
                channel: 1,
                note: 36,
                velocity: 100
            },
            MidiEvent::NoteOff {
                channel: 1,
                note: 36,
                velocity: 64
            },
            MidiEvent::ControlChange {
                channel: 1,
                controller: 64,
                value: 127
            },
        ]
    );
    // The channel nibble is 0-based on the wire and 1-based everywhere a human sees it.
    let events = decoder.decode(&[0x99, 40, 1]);
    assert_eq!(events[0].channel(), 10);
}

/// The one convention that decides whether pads work at all: almost no controller sends 0x8n, and
/// a looper that reads `90 24 00` as a press latches every pad down forever.
#[test]
fn a_note_on_with_velocity_zero_is_a_note_off() {
    let mut decoder = MidiDecoder::new();
    assert_eq!(
        decoder.decode(&[0x90, 36, 0]),
        vec![MidiEvent::NoteOff {
            channel: 1,
            note: 36,
            velocity: 0
        }]
    );
    // And it addresses the same control as the press, or a toggle could never be released.
    let on = MidiEvent::NoteOn {
        channel: 1,
        note: 36,
        velocity: 100,
    };
    assert_eq!(on.id(), decoder.decode(&[0x90, 36, 0])[0].id());
}

#[test]
fn running_status_repeats_the_last_status_byte() {
    let mut decoder = MidiDecoder::new();
    // One status byte, three note events - what a controller sends when it saves bandwidth.
    let events = decoder.decode(&[0x90, 36, 100, 37, 90, 36, 0]);
    assert_eq!(events.len(), 3);
    assert!(matches!(events[0], MidiEvent::NoteOn { note: 36, .. }));
    assert!(matches!(events[1], MidiEvent::NoteOn { note: 37, .. }));
    assert!(matches!(events[2], MidiEvent::NoteOff { note: 36, .. }));
}

#[test]
fn a_message_split_across_two_deliveries_survives() {
    let mut decoder = MidiDecoder::new();
    assert!(decoder.decode(&[0xB0, 7]).is_empty());
    assert_eq!(
        decoder.decode(&[64]),
        vec![MidiEvent::ControlChange {
            channel: 1,
            controller: 7,
            value: 64
        }]
    );
}

/// Clock and active sensing may sit *inside* another message and change nothing about it - not even
/// the running status. Getting this wrong corrupts every second message on a controller that sends
/// clock.
#[test]
fn real_time_bytes_do_not_disturb_the_message_around_them() {
    let mut decoder = MidiDecoder::new();
    let events = decoder.decode(&[0x90, 0xF8, 36, 0xFE, 100, 0xF8, 37, 90]);
    assert_eq!(events.len(), 2);
    assert!(matches!(events[0], MidiEvent::NoteOn { note: 36, velocity: 100, .. }));
    assert!(matches!(events[1], MidiEvent::NoteOn { note: 37, velocity: 90, .. }));
}

#[test]
fn sysex_and_program_changes_are_consumed_without_desynchronising_the_stream() {
    let mut decoder = MidiDecoder::new();
    let events = decoder.decode(&[
        0xF0, 0x7E, 0x00, 0x06, 0x01, 0xF7, // a SysEx enquiry
        0xC0, 5, // program change: one data byte, correctly consumed
        0x90, 36, 100, // and the note that follows still arrives
    ]);
    assert_eq!(
        events,
        vec![MidiEvent::NoteOn {
            channel: 1,
            note: 36,
            velocity: 100
        }]
    );
}

#[test]
fn pitch_bend_is_fourteen_bits_around_zero() {
    let mut decoder = MidiDecoder::new();
    assert_eq!(
        decoder.decode(&[0xE0, 0, 64]),
        vec![MidiEvent::PitchBend {
            channel: 1,
            value: 0
        }]
    );
    assert_eq!(decoder.decode(&[0xE0, 0, 0])[0].value7(), 0);
    assert_eq!(decoder.decode(&[0xE0, 127, 127])[0].value7(), 127);
}

#[test]
fn an_id_survives_being_written_down_and_read_back() {
    for text in ["ch1.note36", "ch16.cc127", "ch10.bend"] {
        let id = MidiId::parse(text).expect(text);
        assert_eq!(id.to_string(), text);
    }
    assert!(MidiId::parse("ch0.note1").is_err(), "Kanal 0 gibt es nicht");
    assert!(MidiId::parse("ch1.note128").is_err());
    assert!(MidiId::parse("note36").unwrap_err().contains("ch1.note36"));
}

// ---------------------------------------------------------------------------------------------
// 2. The address tree
// ---------------------------------------------------------------------------------------------

/// Every address the catalogue offers has to survive being written into a file and read back, or a
/// learn mode would save something it cannot load.
#[test]
fn every_address_of_the_catalogue_round_trips() {
    let all = catalogue(3, 2, 4);
    assert!(all.len() > 100, "der Baum hat {} Adressen", all.len());
    for target in all {
        let address = target.to_string();
        let back = Target::parse(&address).unwrap_or_else(|e| panic!("{address}: {e}"));
        assert_eq!(back, target, "{address}");
        assert!(!target.label().is_empty(), "{address} hat keine Beschriftung");
    }
}

#[test]
fn a_track_is_addressed_by_number_or_by_name() {
    assert_eq!(
        Target::parse("track.1.record").unwrap(),
        Target::Record(TrackRef::Index(0))
    );
    assert_eq!(
        Target::parse("track.stimme.record").unwrap(),
        Target::Record(TrackRef::Name("stimme".to_string()))
    );
    // 1-based outside, 0-based inside - at every level.
    assert_eq!(
        Target::parse("track.2.layer.3.gain").unwrap(),
        Target::LayerGain(TrackRef::Index(1), 2)
    );
    assert_eq!(
        Target::parse("track.1.fx.eq.2.gain").unwrap(),
        Target::FxKnob(TrackRef::Index(0), FxKnob::BandGainDb(1))
    );
    assert!(Target::parse("track.0.record").is_err(), "ab 1 gezaehlt");
    assert!(Target::parse("track.1.fx.eq.4.gain").is_err(), "drei Baender");
}

/// The old score format had exactly two bindings. They stay valid addresses, so a file that
/// compiled before this existed compiles now.
#[test]
fn the_two_old_names_are_still_addresses() {
    assert_eq!(Target::parse("next_section").unwrap(), Target::ScoreNext);
    assert_eq!(Target::parse("stop_all").unwrap(), Target::ScoreStopAll);
    assert_eq!(Target::parse("transport.next").unwrap(), Target::ScoreNext);
}

#[test]
fn an_unknown_address_says_where_the_list_is() {
    let error = Target::parse("track.1.reverb").unwrap_err();
    assert!(error.contains("midi targets"), "{error}");
}

// ---------------------------------------------------------------------------------------------
// 3. Controller value to parameter value
// ---------------------------------------------------------------------------------------------

/// The ends have to be exact. A reverb mix that stops at 0.9921 when the knob is at its stop is a
/// knob that lies about where it is.
#[test]
fn a_knob_hits_both_ends_of_its_range_exactly() {
    for target in [
        Target::Pan(TrackRef::Index(0)),
        Target::LayerGain(TrackRef::Index(0), 0),
        Target::Tempo,
        Target::FxKnob(TrackRef::Index(0), FxKnob::ReverbMix),
        Target::FxKnob(TrackRef::Index(0), FxKnob::BandHz(0)),
        Target::FxKnob(TrackRef::Index(0), FxKnob::CompRatio),
        Target::FxKnob(TrackRef::Index(0), FxKnob::DelayFeedback),
    ] {
        let Control::Range(range) = target.control() else {
            panic!("{target} ist kein Wert");
        };
        assert_eq!(range.value_of(0), range.min, "{target} bei CC 0");
        assert_eq!(range.value_of(127), range.max, "{target} bei CC 127");
    }
}

/// A range that straddles zero has a musical centre, and a controller has a detent at 64. "Put the
/// pan back in the middle" has to be exactly the middle.
#[test]
fn a_bipolar_range_is_exactly_centred_at_sixty_four() {
    let Control::Range(pan) = Target::Pan(TrackRef::Index(0)).control() else {
        unreachable!()
    };
    assert_eq!(pan.value_of(64), 0.0);
    assert_eq!(pan.value_of(0), -1.0);
    assert_eq!(pan.value_of(127), 1.0);
    assert_eq!(pan.value_of(32), -0.5);

    let Control::Range(gain) = Target::FxKnob(TrackRef::Index(0), FxKnob::BandGainDb(0)).control()
    else {
        unreachable!()
    };
    assert_eq!(gain.value_of(64), 0.0, "0 dB ist die Mitte des EQ-Bandes");
}

/// A frequency knob is logarithmic, or the whole bass register sits in the first two of 127 steps.
#[test]
fn a_frequency_knob_is_logarithmic_and_a_gain_knob_is_not() {
    let hz = FxKnob::BandHz(0).range();
    assert_eq!(hz.value_of(0), 20.0);
    assert_eq!(hz.value_of(127), 20_000.0);
    // Halfway up the knob is the geometric mean, about 632 Hz - not the arithmetic 10 kHz.
    let middle = hz.value_of(64);
    assert!((middle - 636.0).abs() < 20.0, "Mitte des Frequenzreglers: {middle}");

    let db = FxKnob::CompThresholdDb.range();
    assert_eq!(db.value_of(0), -60.0);
    assert_eq!(db.value_of(127), 0.0);
    assert!((db.value_of(64) + 29.76).abs() < 0.1, "{}", db.value_of(64));
}

/// Pickup asks "has the knob passed the value", which is a question about controller positions -
/// so the mapping has to be invertible, on every curve.
#[test]
fn the_controller_position_of_a_value_is_the_inverse_of_the_value_of_a_position() {
    for range in [
        FxKnob::BandHz(0).range(),
        FxKnob::CompRatio.range(),
        FxKnob::ReverbMix.range(),
        Range::linear(-1.0, 1.0),
        Range::linear(-24.0, 24.0),
    ] {
        for cc in [0u8, 1, 32, 63, 64, 65, 100, 126, 127] {
            let value = range.value_of(cc);
            let back = range.cc_of(value);
            assert!(
                (back - f32::from(cc)).abs() < 0.75,
                "{range:?}: CC {cc} -> {value} -> CC {back}"
            );
        }
    }
}

/// A binding may narrow the range: a knob that only has to cover 0 to 40 % reverb should have all
/// 127 steps for those 40 %, not 51 of them.
#[test]
fn a_binding_can_narrow_the_range_it_covers() {
    let binding = Binding::new(Target::FxKnob(TrackRef::Index(0), FxKnob::ReverbMix))
        .with_bounds(None, Some(0.4));
    let range = binding.range().expect("Wertebereich");
    assert_eq!(range.value_of(0), 0.0);
    assert_eq!(range.value_of(127), 0.4);
}

// ---------------------------------------------------------------------------------------------
// 4. Buttons: trigger against toggle
// ---------------------------------------------------------------------------------------------

fn layout() -> TrackLayout {
    TrackLayout::new(["stimme", "gitarre"])
}

fn press(note: u8) -> MidiEvent {
    MidiEvent::NoteOn {
        channel: 1,
        note,
        velocity: 100,
    }
}

fn release(note: u8) -> MidiEvent {
    MidiEvent::NoteOff {
        channel: 1,
        note,
        velocity: 0,
    }
}

fn cc(number: u8, value: u8) -> MidiEvent {
    MidiEvent::ControlChange {
        channel: 1,
        controller: number,
        value,
    }
}

fn router_with(bindings: Vec<(&str, Binding)>) -> Router {
    let mut map = MidiMap::new();
    for (id, binding) in bindings {
        map.insert(MidiId::parse(id).expect(id), binding);
    }
    Router::new(map)
}

/// A trigger fires once per press and does nothing on release; a toggle flips once per press and
/// stays where it was put. Both are wrong for the other's target, which is why the mode is per
/// binding.
#[test]
fn a_trigger_fires_on_the_press_and_a_toggle_flips_on_it() {
    let mut router = router_with(vec![
        ("ch1.note36", Binding::new(Target::Record(TrackRef::Index(0)))),
        ("ch1.note37", Binding::new(Target::Monitor(TrackRef::Index(0)))),
    ]);
    let layout = layout();
    let ctx = Context::bare(&layout);

    // Record is a trigger by default: press acts, release does not.
    assert_eq!(
        router.resolve(&press(36), &ctx).action(),
        Some(MidiAction::Record { track: 0 })
    );
    assert!(
        matches!(router.resolve(&release(36), &ctx), Resolution::Absorbed(_)),
        "das Loslassen loest keine zweite Aufnahme aus"
    );
    assert_eq!(
        router.resolve(&press(36), &ctx).action(),
        Some(MidiAction::Record { track: 0 }),
        "der naechste Druck loest wieder aus"
    );

    // Monitoring is a switch, so it toggles - and it stays on when the pad is let go.
    assert_eq!(
        router.resolve(&press(37), &ctx).action(),
        Some(MidiAction::Monitor {
            track: 0,
            on: true
        })
    );
    assert!(matches!(
        router.resolve(&release(37), &ctx),
        Resolution::Absorbed(_)
    ));
    assert_eq!(
        router.resolve(&press(37), &ctx).action(),
        Some(MidiAction::Monitor {
            track: 0,
            on: false
        })
    );
}

/// Talkback: monitoring on while the pad is held, off when it is let go.
#[test]
fn a_momentary_switch_is_on_only_while_the_pad_is_held() {
    let mut router = router_with(vec![(
        "ch1.note40",
        Binding::new(Target::Monitor(TrackRef::Index(1))).with_button(ButtonMode::Momentary),
    )]);
    let layout = layout();
    let ctx = Context::bare(&layout);
    assert_eq!(
        router.resolve(&press(40), &ctx).action(),
        Some(MidiAction::Monitor {
            track: 1,
            on: true
        })
    );
    assert_eq!(
        router.resolve(&release(40), &ctx).action(),
        Some(MidiAction::Monitor {
            track: 1,
            on: false
        })
    );
}

/// The engine's own state wins over the router's memory: the mouse and the score change these too,
/// and a toggle with a private copy would be out of step from the first time anything else touched
/// it.
#[test]
fn a_toggle_flips_the_state_the_engine_reports_not_its_own_memory() {
    let target = Target::FxBypass(TrackRef::Index(0));
    let mut router = router_with(vec![("ch1.note50", Binding::new(target.clone()))]);
    let layout = layout();
    let state = StaticState::new().with_switch(&target, true);
    let ctx = Context {
        layout: &layout,
        state: &state,
        score_is_playing: false,
    };
    assert_eq!(
        router.resolve(&press(50), &ctx).action(),
        Some(MidiAction::FxBypass {
            track: 0,
            on: false
        }),
        "die Kette ist umgangen, der Druck schaltet sie ein"
    );
}

/// A pad-shaped controller that sends 0 and 127 on a CC still works, and it fires once per press
/// rather than once per message.
#[test]
fn a_controller_that_sends_zero_and_one_two_seven_behaves_like_a_pad() {
    let mut router = router_with(vec![(
        "ch1.cc64",
        Binding::new(Target::ScoreNext),
    )]);
    let layout = layout();
    let ctx = Context::bare(&layout);
    assert_eq!(
        router.resolve(&cc(64, 127), &ctx).action(),
        Some(MidiAction::ScoreNext)
    );
    assert!(
        router.resolve(&cc(64, 100), &ctx).action().is_none(),
        "der Wert bleibt oben - das ist kein zweiter Druck"
    );
    assert!(router.resolve(&cc(64, 0), &ctx).action().is_none());
    assert_eq!(
        router.resolve(&cc(64, 127), &ctx).action(),
        Some(MidiAction::ScoreNext)
    );
}

/// "Aufnahme, aber umgeschaltet" has no second state to switch to. Refusing it when the file is
/// read beats treating it as a press and leaving a musician with a file that says one thing and
/// does another.
#[test]
fn a_trigger_cannot_be_a_toggle() {
    let binding =
        Binding::new(Target::Record(TrackRef::Index(0))).with_button(ButtonMode::Toggle);
    let error = binding.check().unwrap_err();
    assert!(error.contains("press"), "{error}");
    assert!(
        Binding::new(Target::Monitor(TrackRef::Index(0)))
            .with_button(ButtonMode::Toggle)
            .check()
            .is_ok()
    );
}

// ---------------------------------------------------------------------------------------------
// 5. Events become commands
// ---------------------------------------------------------------------------------------------

#[test]
fn an_event_becomes_the_right_command_with_the_right_target_and_value() {
    let target = Target::FxKnob(TrackRef::Index(1), FxKnob::ReverbMix);
    let mut router = router_with(vec![(
        "ch1.cc22",
        Binding::new(target).with_takeover(Takeover::Jump),
    )]);
    let layout = layout();
    let ctx = Context::bare(&layout);
    let action = router
        .resolve(&cc(22, 127), &ctx)
        .action()
        .expect("eine Absicht");
    assert_eq!(
        action,
        MidiAction::FxParam {
            track: 1,
            param: FxParam::ReverbMix(1.0)
        }
    );
    assert_eq!(
        action.command(),
        Some(Command::SetFxParam {
            track: 1,
            param: FxParam::ReverbMix(1.0)
        })
    );
}

#[test]
fn a_pad_can_load_a_preset_and_a_knob_can_set_the_pan() {
    let mut router = router_with(vec![
        (
            "ch1.note60",
            Binding::new(Target::FxPreset(
                TrackRef::Name("gitarre".to_string()),
                FxPreset::PiezoGuitar,
            )),
        ),
        (
            "ch1.cc9",
            Binding::new(Target::Pan(TrackRef::Name("gitarre".to_string())))
                .with_takeover(Takeover::Jump),
        ),
    ]);
    let layout = layout();
    let ctx = Context::bare(&layout);
    assert_eq!(
        router.resolve(&press(60), &ctx).action().unwrap().command(),
        Some(Command::LoadFxPreset {
            track: 1,
            preset: FxPreset::PiezoGuitar
        })
    );
    assert_eq!(
        router.resolve(&cc(9, 0), &ctx).action().unwrap().command(),
        Some(Command::SetPan {
            track: 1,
            pan: -1.0
        })
    );
}

/// A binding that names a track this session does not have is refused with a sentence, never
/// guessed at: guessing sends the next take to the wrong microphone.
#[test]
fn a_binding_on_a_track_that_is_not_there_is_refused_in_german() {
    let mut router = router_with(vec![(
        "ch1.note36",
        Binding::new(Target::Record(TrackRef::Name("klavier".to_string()))),
    )]);
    let layout = layout();
    let ctx = Context::bare(&layout);
    match router.resolve(&press(36), &ctx) {
        Resolution::Refused(message) => {
            assert!(message.contains("klavier"), "{message}");
            assert!(message.contains("stimme"), "die Meldung zaehlt auf, was es gibt: {message}");
        }
        other => panic!("abgelehnt erwartet, bekommen: {other:?}"),
    }
}

#[test]
fn an_unbound_control_is_reported_as_unbound_and_not_as_an_error() {
    let mut router = router_with(vec![]);
    let layout = layout();
    assert_eq!(
        router.resolve(&press(36), &Context::bare(&layout)),
        Resolution::Unbound
    );
}

// ---------------------------------------------------------------------------------------------
// 6. The value jump
// ---------------------------------------------------------------------------------------------

/// The classic trap: the knob is at 0, the parameter at 0.8, and the first millimetre of movement
/// makes the reverb jump. With pickup the knob does nothing until it reaches the value.
#[test]
fn pickup_keeps_a_knob_quiet_until_it_reaches_the_value() {
    let target = Target::FxKnob(TrackRef::Index(0), FxKnob::ReverbMix);
    let mut router = router_with(vec![("ch1.cc3", Binding::new(target.clone()))]);
    let layout = layout();
    // The parameter sits at 0.8, i.e. at controller position 102.
    let state = StaticState::new().with_value(&target, 0.8);
    let ctx = Context {
        layout: &layout,
        state: &state,
        score_is_playing: false,
    };

    for value in [0u8, 10, 40, 80] {
        match router.resolve(&cc(3, value), &ctx) {
            Resolution::Absorbed(reason) => assert!(reason.contains("drehen"), "{reason}"),
            other => panic!("bei {value} haette nichts passieren duerfen: {other:?}"),
        }
    }
    // Now it arrives, and from here on it works.
    assert!(
        router.resolve(&cc(3, 102), &ctx).action().is_some(),
        "beim Erreichen des Wertes uebernimmt der Regler"
    );
    assert_eq!(
        router.resolve(&cc(3, 60), &ctx).action(),
        Some(MidiAction::FxParam {
            track: 0,
            param: FxParam::ReverbMix(FxKnob::ReverbMix.range().value_of(60))
        })
    );
}

/// A knob swept quickly skips steps, so "exactly the value" is not enough - crossing it has to
/// count too.
#[test]
fn pickup_also_catches_a_knob_that_sweeps_past_the_value() {
    let target = Target::Pan(TrackRef::Index(0));
    let mut router = router_with(vec![("ch1.cc9", Binding::new(target.clone()))]);
    let layout = layout();
    let state = StaticState::new().with_value(&target, 0.0); // Mitte, also CC 64
    let ctx = Context {
        layout: &layout,
        state: &state,
        score_is_playing: false,
    };
    assert!(matches!(
        router.resolve(&cc(9, 10), &ctx),
        Resolution::Absorbed(_)
    ));
    assert!(
        router.resolve(&cc(9, 120), &ctx).action().is_some(),
        "der Regler ist ueber den Wert hinweggefahren"
    );
}

/// Switchable, because "the first movement does nothing" is confusing if you do not know it is on -
/// and after a preset, jumping is the faster way back to a known state.
#[test]
fn pickup_can_be_switched_off_per_binding() {
    let target = Target::FxKnob(TrackRef::Index(0), FxKnob::ReverbMix);
    let mut router = router_with(vec![(
        "ch1.cc3",
        Binding::new(target.clone()).with_takeover(Takeover::Jump),
    )]);
    let layout = layout();
    let state = StaticState::new().with_value(&target, 0.8);
    let ctx = Context {
        layout: &layout,
        state: &state,
        score_is_playing: false,
    };
    assert_eq!(
        router.resolve(&cc(3, 0), &ctx).action(),
        Some(MidiAction::FxParam {
            track: 0,
            param: FxParam::ReverbMix(0.0)
        }),
        "ohne Schutz wirkt der Regler sofort"
    );
}

/// Before the first status snapshot there is nothing to protect. A knob that stayed dead until one
/// arrived would look exactly like a broken cable.
#[test]
fn a_knob_takes_over_at_once_when_the_current_value_is_unknown() {
    let mut router = router_with(vec![(
        "ch1.cc3",
        Binding::new(Target::FxKnob(TrackRef::Index(0), FxKnob::ReverbMix)),
    )]);
    let layout = layout();
    let ctx = Context::bare(&layout);
    assert!(router.resolve(&cc(3, 0), &ctx).action().is_some());
}

/// After a preset every knob is somewhere else than its parameter. Re-arming is what a preset load
/// is supposed to do, and without it pickup protects the first touch after startup and nothing
/// afterwards.
#[test]
fn rearming_makes_a_caught_knob_catch_again() {
    let target = Target::FxKnob(TrackRef::Index(0), FxKnob::ReverbMix);
    let mut router = router_with(vec![("ch1.cc3", Binding::new(target.clone()))]);
    let layout = layout();
    let state = StaticState::new().with_value(&target, 0.0);
    let ctx = Context {
        layout: &layout,
        state: &state,
        score_is_playing: false,
    };
    assert!(router.resolve(&cc(3, 0), &ctx).action().is_some(), "faengt sofort");

    router.rearm();
    let state = StaticState::new().with_value(&target, 0.9);
    let ctx = Context {
        layout: &layout,
        state: &state,
        score_is_playing: false,
    };
    assert!(
        matches!(router.resolve(&cc(3, 5), &ctx), Resolution::Absorbed(_)),
        "nach dem Preset muss der Regler den neuen Wert erst wieder erreichen"
    );
}

// ---------------------------------------------------------------------------------------------
// 7. The controller profile
// ---------------------------------------------------------------------------------------------

fn scratch(name: &str) -> PathBuf {
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let dir = std::env::temp_dir().join(format!("henrys-looper-midi-{name}-{stamp}"));
    std::fs::create_dir_all(&dir).expect("Testverzeichnis");
    dir
}

/// Written and read back has to be the same mapping. Everything else about a profile - that it
/// survives a restart, that a musician can edit it - rests on this one property.
#[test]
fn a_profile_survives_being_written_and_read_back() {
    let mut profile = Profile::new("MPD218");
    profile.map.insert(
        MidiId::note(1, 36),
        Binding::new(Target::Record(TrackRef::Index(0))),
    );
    profile.map.insert(
        MidiId::note(1, 37),
        Binding::new(Target::Monitor(TrackRef::Index(0))).with_button(ButtonMode::Momentary),
    );
    profile.map.insert(
        MidiId::cc(1, 3),
        Binding::new(Target::FxKnob(TrackRef::Index(0), FxKnob::ReverbMix))
            .with_takeover(Takeover::Jump)
            .with_bounds(None, Some(0.4)),
    );
    profile.map.insert(
        MidiId::cc(10, 9),
        Binding::new(Target::Pan(TrackRef::Name("stimme".to_string()))),
    );

    let dir = scratch("roundtrip");
    let path = dir.join("mpd218.yaml");
    profile.save(&path).expect("schreiben");
    let back = Profile::load(&path).expect("lesen");
    assert_eq!(back.device, "MPD218");
    assert_eq!(back.map, profile.map);
    let _ = std::fs::remove_dir_all(&dir);
}

/// The template that ships with the program, read from disk so it can never rot: every address in
/// it has to exist, and every combination in it has to be legal.
#[test]
fn the_shipped_controller_profile_parses() {
    const SHIPPED: &str = include_str!("../../../examples/midi-mpd218.yaml");
    let profile = Profile::parse(SHIPPED).unwrap_or_else(|issues| panic!("{issues:#?}"));
    assert_eq!(profile.device, "MPD218");
    assert_eq!(profile.map.len(), 22);
    assert_eq!(
        profile
            .map
            .get(MidiId::note(10, 45))
            .map(|b| b.target.clone()),
        Some(Target::ScoreNext),
        "Pad 10 ist der Release-Knopf"
    );
    assert_eq!(
        profile.map.get(MidiId::note(10, 48)).map(|b| b.button),
        Some(ButtonMode::Momentary)
    );
    assert_eq!(profile.map.get(MidiId::cc(1, 3)).and_then(|b| b.max), Some(0.5));
    // And the template survives being written back out - a musician who learns one more pad on top
    // of it must not lose the rest.
    let again = Profile::parse(&profile.to_yaml()).expect("erneut lesbar");
    assert_eq!(again.map, profile.map);
}

#[test]
fn a_profile_reports_every_problem_at_once_and_with_a_line() {
    let issues = Profile::parse(
        "device: MPD218\nbindings:\n  ch1.note36: {target: track.1.gibtsnicht}\n  quatsch: {target: track.1.play}\n",
    )
    .unwrap_err();
    assert_eq!(issues.len(), 2, "{issues:?}");
    assert!(issues.iter().all(|i| i.starts_with("Zeile")), "{issues:?}");
}

/// One pad can only do one thing. A repeated key is a file that does something other than it says.
#[test]
fn a_control_bound_twice_in_a_profile_is_reported() {
    let issues = Profile::parse(
        "device: X\nbindings:\n  ch1.note36: {target: track.1.record}\n  ch1.note36: {target: track.1.play}\n",
    )
    .unwrap_err();
    assert!(issues.iter().any(|i| i.contains("mehrfach")), "{issues:?}");
}

#[test]
fn the_short_form_of_a_binding_is_just_the_address() {
    let profile = Profile::parse("device: X\nbindings:\n  ch1.note36: track.1.record\n").unwrap();
    assert_eq!(
        profile.map.get(MidiId::note(1, 36)).map(|b| b.target.clone()),
        Some(Target::Record(TrackRef::Index(0)))
    );
}

/// Windows decorates a port name with a number when more than one MIDI device has been plugged in,
/// so the device is matched loosely rather than by an exact file name.
#[test]
fn a_profile_finds_its_port_through_the_prefix_windows_adds() {
    let profile = Profile::new("MPD218");
    assert!(profile.matches("MPD218"));
    assert!(profile.matches("2- MPD218"));
    assert!(profile.matches("MPD218 0"));
    assert!(!profile.matches("Scarlett 2i2 USB"));
}

#[test]
fn a_device_name_becomes_a_usable_file_name() {
    assert_eq!(profile::slug("MPD218"), "mpd218");
    assert_eq!(profile::slug("2- MPD218"), "2-mpd218");
    assert_eq!(profile::slug("///"), "controller");
    assert!(profile_path("MPD218").ends_with("mpd218.yaml"));
    assert!(profile_dir().ends_with("midi"));
}

// ---------------------------------------------------------------------------------------------
// 8. The score's supplement
// ---------------------------------------------------------------------------------------------

const SCORE: &str = "\
title: Test
bpm: 100
tracks:
  stimme: {input: 1}
  gitarre: {input: 2}
sections:
  - id: intro
    bars: 4
    tracks: {stimme: record}
  - id: refrain
    bars: 8
    tracks: {stimme: play}
";

fn score_with(midi: &str) -> String {
    format!("{SCORE}midi:\n{midi}")
}

/// The old two-binding block still compiles, still produces the same ids, and now also says which
/// address it means.
#[test]
fn the_old_midi_block_still_compiles() {
    let score = compile_score(&score_with("  next_section: {cc: 64}\n  stop_all: {note: 37}\n"))
        .expect("uebersetzt");
    assert_eq!(score.midi.get("next_section").unwrap().id, "ch1.cc64");
    assert_eq!(score.midi.get("next_section").unwrap().target, "transport.next");
    assert_eq!(score.midi.get("stop_all").unwrap().id, "ch1.note37");
    assert_eq!(score.midi.get("stop_all").unwrap().target, "transport.stop_all");
}

#[test]
fn a_score_can_bind_anything_the_tree_offers() {
    let score = compile_score(&score_with(
        "  track.stimme.record: {note: 36}\n  \
           track.2.fx.reverb.mix: {cc: 3, max: 0.4}\n  \
           track.stimme.monitor: {note: 40, mode: momentary}\n  \
           transport.goto.2: {note: 45}\n",
    ))
    .expect("uebersetzt");
    assert_eq!(score.midi.len(), 4);
    assert_eq!(score.midi.get("track.2.fx.reverb.mix").unwrap().max, Some(0.4));
    assert_eq!(
        score.midi.get("track.stimme.monitor").unwrap().mode,
        Some(ButtonMode::Momentary)
    );

    let map = score_map(&score).expect("Mapping");
    assert_eq!(map.len(), 4);
    assert_eq!(
        map.get(MidiId::note(1, 45)).map(|b| b.target.clone()),
        Some(Target::ScoreGoto(1))
    );
}

#[test]
fn a_score_binding_is_checked_against_the_score_itself() {
    let error = compile_score(&score_with("  track.klavier.record: {note: 36}\n")).unwrap_err();
    assert!(error.issues[0].message.contains("klavier"), "{:?}", error.issues);
    assert!(
        error.issues[0]
            .suggestion
            .as_deref()
            .is_some_and(|s| s.contains("gitarre") || s.contains("stimme")),
        "{:?}",
        error.issues
    );

    let error = compile_score(&score_with("  transport.goto.9: {note: 36}\n")).unwrap_err();
    assert!(error.issues[0].message.contains("Sektion 9"), "{:?}", error.issues);

    let error = compile_score(&score_with("  track.3.record: {note: 36}\n")).unwrap_err();
    assert!(error.issues[0].message.contains("Track 3"), "{:?}", error.issues);
}

/// Two targets on one pad is a file that does something other than it says. It is reported rather
/// than letting the second entry win silently.
#[test]
fn one_control_bound_to_two_targets_in_a_score_is_reported() {
    let error = compile_score(&score_with(
        "  track.stimme.record: {note: 36}\n  track.gitarre.play: {note: 36}\n",
    ))
    .unwrap_err();
    assert!(
        error.issues.iter().any(|i| i.message.contains("ch1.note36")),
        "{:?}",
        error.issues
    );
}

/// The two stages compose: the profile is the controller, the score is the piece, and the piece
/// wins - loudly.
#[test]
fn the_score_overrides_the_profile_and_says_so() {
    let mut profile = Profile::new("MPD218");
    profile.map.insert(
        MidiId::note(1, 36),
        Binding::new(Target::Record(TrackRef::Index(0))),
    );
    profile.map.insert(
        MidiId::note(1, 37),
        Binding::new(Target::Overdub(TrackRef::Index(0))),
    );

    let score = compile_score(&score_with("  transport.goto.2: {note: 36}\n")).expect("uebersetzt");
    let (map, notes) = session_map(Some(&profile), Some(&score)).expect("Mapping");

    assert_eq!(
        map.get(MidiId::note(1, 36)).map(|b| b.target.clone()),
        Some(Target::ScoreGoto(1)),
        "die Partitur gewinnt"
    );
    assert_eq!(
        map.get(MidiId::note(1, 37)).map(|b| b.target.clone()),
        Some(Target::Overdub(TrackRef::Index(0))),
        "alles andere bleibt, wie das Profil es sagt"
    );
    assert_eq!(notes.len(), 1, "{notes:?}");
    assert!(notes[0].contains("ch1.note36"), "{}", notes[0]);
    assert!(notes[0].contains("transport.goto.2"), "{}", notes[0]);
}

// ---------------------------------------------------------------------------------------------
// 9. The runner's boundary
// ---------------------------------------------------------------------------------------------

/// **The same line a mouse click runs into.** While a score plays, the runner owns the transport;
/// a take it did not schedule makes its predicted loop geometry wrong from that moment on. The
/// sentence is the same one, from the same constant, because two wordings would suggest two rules.
#[test]
fn a_transport_pad_is_refused_while_the_score_runs_exactly_as_a_mouse_click_is() {
    let mut router = router_with(vec![
        ("ch1.note36", Binding::new(Target::Record(TrackRef::Index(0)))),
        ("ch1.note37", Binding::new(Target::ScoreNext)),
        ("ch1.note38", Binding::new(Target::Monitor(TrackRef::Index(0)))),
        ("ch1.cc9", Binding::new(Target::Pan(TrackRef::Index(0))).with_takeover(Takeover::Jump)),
    ]);
    let layout = layout();
    let running = Context {
        layout: &layout,
        state: &(),
        score_is_playing: true,
    };

    match router.resolve(&press(36), &running) {
        Resolution::Refused(message) => assert_eq!(message, TRANSPORT_BELONGS_TO_SCORE),
        other => panic!("Aufnahme haette abgelehnt werden muessen: {other:?}"),
    }
    // The release button is the runner's own - refusing it would refuse the one thing the musician
    // is holding the controller for.
    assert_eq!(
        router.resolve(&press(37), &running).action(),
        Some(MidiAction::ScoreNext)
    );
    // The mix belongs to the human throughout.
    assert!(router.resolve(&press(38), &running).action().is_some());
    assert!(router.resolve(&cc(9, 100), &running).action().is_some());

    // With no score running, the same pad works.
    let idle = Context::bare(&layout);
    assert_eq!(
        router.resolve(&press(36), &idle).action(),
        Some(MidiAction::Record { track: 0 })
    );
}

/// The list of what moves the transport has to be the same question whichever end it is asked
/// from - the address or the resolved action.
#[test]
fn the_address_and_the_action_agree_about_what_moves_the_transport() {
    let layout = TrackLayout::new(["a"]);
    let ctx = Context::bare(&layout);
    for target in catalogue(1, 1, 2) {
        let mut router = router_with(vec![("ch1.note36", Binding::new(target.clone()))]);
        // Not every target produces an action from one press (a knob needs a value), so only the
        // ones that do can be compared - which is every trigger and every switch.
        let Some(action) = router.resolve(&press(36), &ctx).action() else {
            continue;
        };
        assert_eq!(
            target.moves_transport(),
            action.moves_transport(),
            "{target} und {action:?} sind sich uneinig"
        );
    }
}

// ---------------------------------------------------------------------------------------------
// 10. Learn
// ---------------------------------------------------------------------------------------------

#[test]
fn learn_takes_the_next_press_and_then_disarms_itself() {
    let mut learn = Learn::new();
    assert!(!learn.is_armed());
    let message = learn.arm(Target::Record(TrackRef::Index(0)));
    assert!(message.contains("Aufnahme"), "{message}");
    assert!(learn.is_armed());

    // A note off is not a press - learning on it would produce the same binding twice.
    assert!(learn.feed(&release(36), None).is_none());
    assert!(learn.is_armed());

    let learned = learn.feed(&press(36), None).expect("gelernt");
    assert_eq!(learned.id, MidiId::note(1, 36));
    assert_eq!(learned.binding.target, Target::Record(TrackRef::Index(0)));
    assert!(!learn.is_armed(), "danach ist der Modus aus");
    assert!(learn.feed(&press(37), None).is_none());
}

#[test]
fn learn_says_what_the_control_did_before() {
    let mut learn = Learn::new();
    learn.arm(Target::Play(TrackRef::Index(0)));
    let previous = Binding::new(Target::Record(TrackRef::Index(0)));
    let learned = learn.feed(&press(36), Some(&previous)).expect("gelernt");
    assert!(learned.message.contains("vorher"), "{}", learned.message);
    assert_eq!(learned.replaced, Some(Target::Record(TrackRef::Index(0))));
}

#[test]
fn learn_can_be_cancelled() {
    let mut learn = Learn::new();
    learn.arm(Target::ClearAll);
    let message = learn.cancel().expect("Meldung");
    assert!(message.contains("abgebrochen"), "{message}");
    assert!(!learn.is_armed());
    assert!(learn.cancel().is_none());
}

/// A pad on a value target is allowed - velocity becomes the value - and says so, because that is
/// almost never what somebody meant.
#[test]
fn learning_a_pad_onto_a_value_says_what_will_happen() {
    let mut learn = Learn::new();
    learn.arm(Target::FxKnob(TrackRef::Index(0), FxKnob::ReverbMix));
    let learned = learn.feed(&press(36), None).expect("gelernt");
    assert!(
        learned.message.contains("Anschlagstaerke"),
        "{}",
        learned.message
    );
}

// ---------------------------------------------------------------------------------------------
// 11. Everything that is bindable, is bindable
// ---------------------------------------------------------------------------------------------

/// The brief lists what has to be reachable. This is that list, checked against the tree rather
/// than against a memory of it.
#[test]
fn every_operable_thing_has_an_address() {
    for address in [
        "transport.start",
        "transport.next",
        "transport.stop_all",
        "transport.goto.1",
        "global.click",
        "global.clear_all",
        "global.tempo",
        "global.quantize.bar",
        "track.1.record",
        "track.1.overdub",
        "track.1.play",
        "track.1.stop",
        "track.1.clear",
        "track.1.monitor",
        "track.1.pan",
        "track.1.latency_trim",
        "track.1.layer.1.mute",
        "track.1.layer.1.remove",
        "track.1.layer.1.gain",
        "track.1.fx.bypass",
        "track.1.fx.high_pass.on",
        "track.1.fx.eq.on",
        "track.1.fx.comp.on",
        "track.1.fx.delay.on",
        "track.1.fx.reverb.on",
        "track.1.fx.preset.stimme",
        "track.1.fx.high_pass.hz",
        "track.1.fx.eq.1.hz",
        "track.1.fx.eq.1.q",
        "track.1.fx.eq.1.gain",
        "track.1.fx.comp.threshold",
        "track.1.fx.comp.ratio",
        "track.1.fx.comp.attack",
        "track.1.fx.comp.release",
        "track.1.fx.comp.knee",
        "track.1.fx.comp.makeup",
        "track.1.fx.delay.note.dotted_eighth",
        "track.1.fx.delay.feedback",
        "track.1.fx.delay.mix",
        "track.1.fx.reverb.size",
        "track.1.fx.reverb.damping",
        "track.1.fx.reverb.mix",
    ] {
        let target = Target::parse(address).unwrap_or_else(|e| panic!("{address}: {e}"));
        assert_eq!(target.to_string(), address);
    }
}

/// Every one of the five effects can be switched, by the name the app's commands already use.
#[test]
fn each_of_the_five_effects_can_be_switched_by_name() {
    for slot in FxSlot::all() {
        let address = format!("track.1.fx.{}.on", slot.name());
        assert_eq!(
            Target::parse(&address).unwrap(),
            Target::FxEnabled(TrackRef::Index(0), slot)
        );
    }
}
