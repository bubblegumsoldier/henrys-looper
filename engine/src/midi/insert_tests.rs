//! The plugin-insert addresses: `track.<ref>.insert.<n>.bypass`, `track.<ref>.insert.<n>.param.<id>`
//! and the same under `global.bus.<main|monitor>`. Offline, like the rest of the MIDI layer.

use super::*;
use crate::engine::bus::Bus;
use crate::engine::insert::{InsertTarget, MAX_INSERTS};

fn router_with(bindings: Vec<(&str, Binding)>) -> Router {
    let mut map = MidiMap::new();
    for (id, binding) in bindings {
        map.insert(MidiId::parse(id).expect(id), binding);
    }
    Router::new(map)
}

fn press(note: u8) -> MidiEvent {
    MidiEvent::NoteOn {
        channel: 1,
        note,
        velocity: 100,
    }
}

fn cc(number: u8, value: u8) -> MidiEvent {
    MidiEvent::ControlChange {
        channel: 1,
        controller: number,
        value,
    }
}

#[test]
fn insert_addresses_read_and_write_back_the_same() {
    for (address, target) in [
        (
            "track.1.insert.2.bypass",
            Target::InsertBypass(InsertOwner::Track(TrackRef::Index(0)), 1),
        ),
        (
            "track.stimme.insert.1.param.17",
            Target::InsertParam(InsertOwner::Track(TrackRef::Name("stimme".into())), 0, 17),
        ),
        (
            "global.bus.monitor.insert.8.bypass",
            Target::InsertBypass(InsertOwner::Bus(Bus::Monitor), 7),
        ),
        (
            "global.bus.main.insert.3.param.0",
            Target::InsertParam(InsertOwner::Bus(Bus::Main), 2, 0),
        ),
    ] {
        assert_eq!(Target::parse(address), Ok(target.clone()), "{address}");
        assert_eq!(target.to_string(), address);
    }
    assert!(Target::parse("track.1.insert.9.bypass").is_err(), "nur {MAX_INSERTS} Slots");
    assert!(Target::parse("track.1.insert.0.bypass").is_err(), "ab 1 gezaehlt");
    assert!(Target::parse("track.1.insert.1.param.x").is_err());
    assert!(Target::parse("global.bus.nowhere.insert.1.bypass").is_err());
}

#[test]
fn insert_bypass_is_a_switch_and_a_parameter_a_range_of_zero_to_one() {
    let bypass = Target::InsertBypass(InsertOwner::Track(TrackRef::Index(0)), 0);
    assert_eq!(bypass.control(), Control::Switch);
    assert_eq!(bypass.track(), Some(&TrackRef::Index(0)));
    let param = Target::InsertParam(InsertOwner::Bus(Bus::Main), 0, 5);
    assert_eq!(param.track(), None);
    let Control::Range(range) = param.control() else {
        panic!("ein Wert");
    };
    assert_eq!((range.min, range.max), (0.0, 1.0));
    assert!(!bypass.moves_transport() && !param.moves_transport(), "Mix, nicht Transport");
    // Every slot's bypass is in the catalogue, for every track and both buses.
    let all = catalogue(2, 0, 0);
    let count = all
        .iter()
        .filter(|t| matches!(t, Target::InsertBypass(..)))
        .count();
    assert_eq!(count, (2 + 2) * MAX_INSERTS);
}

#[test]
fn a_pad_toggles_a_slot_bypass_on_the_named_track() {
    let mut router = router_with(vec![(
        "ch1.note40",
        Binding::new(Target::InsertBypass(
            InsertOwner::Track(TrackRef::Name("gitarre".into())),
            2,
        )),
    )]);
    let layout = TrackLayout::new(["stimme", "gitarre"]);
    let ctx = Context::bare(&layout);
    let action = router.resolve(&press(40), &ctx).action().expect("Absicht");
    assert_eq!(
        action,
        MidiAction::InsertBypass {
            target: InsertTarget::Track(1),
            slot: 2,
            on: true
        }
    );
    assert_eq!(action.command(), None, "kein Engine-Kommando: das macht der Plugin-Manager");
}

#[test]
fn a_knob_drives_a_plugin_parameter_on_a_bus() {
    let mut router = router_with(vec![(
        "ch1.cc7",
        Binding::new(Target::InsertParam(InsertOwner::Bus(Bus::Monitor), 0, 42))
            .with_takeover(Takeover::Jump),
    )]);
    let layout = TrackLayout::new(["stimme"]);
    let ctx = Context::bare(&layout);
    let action = router.resolve(&cc(7, 127), &ctx).action().expect("Absicht");
    assert_eq!(
        action,
        MidiAction::InsertParam {
            target: InsertTarget::Bus(Bus::Monitor),
            slot: 0,
            param: 42,
            value: 1.0
        }
    );
    assert!(!action.moves_transport());
}
