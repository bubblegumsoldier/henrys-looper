//! Offline proofs for plugin inserts placed **between** the built-in effects (`super::insert`,
//! "Where they sit"; `fx::Chain::process_segment`): the signal passes the devices in the listed
//! order, an identity plugin anywhere changes nothing, the latencies add up wherever the plugins
//! sit, and a move while audio runs dips instead of clicking. Plain-Rust test processors only -
//! no installed plugin is ever loaded here.

use super::bus::{Bus, BusRouting, BusSend};
use super::command::Command;
use super::frame::Frame;
use super::fx::{FxPreset, FxSlot, octaver};
use super::insert::testing::{Delay, Gain, Offset};
use super::insert::{
    Insert, InsertMsg, InsertPosition, InsertProcessor, InsertSender, InsertShared, InsertTarget,
    REORDER_FADE, insert_channel,
};
use super::sim::{Played, Sim, SimSpec, TrackSpec, mono};
use super::timeline::TimeSignature;

const RATE: u32 = 48_000;

fn spec() -> SimSpec {
    SimSpec {
        bpm: 100.0,
        signature: TimeSignature::new(4, 4),
        bars: 2,
        latency: 827,
        roundtrip: 827,
        block: 128,
        loopback_channel: None,
        tracks: vec![TrackSpec::on(0).monitoring()],
        ..Default::default()
    }
}

fn fingerprint(k: u64) -> f32 {
    ((k.wrapping_mul(2_654_435_761) % 65_536) as f32) / 65_536.0 - 0.5
}

fn with_rack(spec: SimSpec, played: Played) -> (Sim, InsertSender) {
    let mut sim = Sim::new(spec, played);
    let (sender, endpoint) = insert_channel(32);
    sim.core.attach_inserts(endpoint);
    (sim, sender)
}

fn install_at(
    tx: &mut InsertSender,
    slot: usize,
    processor: impl InsertProcessor + 'static,
    latency: u32,
    position: InsertPosition,
) -> std::sync::Arc<InsertShared> {
    let shared = InsertShared::new(latency);
    tx.send(InsertMsg::Install {
        target: InsertTarget::Track(0),
        slot,
        insert: Insert::new(Box::new(processor), shared.clone()).at(position),
    })
    .expect("Queue frei");
    shared
}

fn high_pass_on(sim: &mut Sim) {
    sim.send(Command::SetFxBypass { track: 0, on: false });
    sim.send(Command::SetFxEnabled {
        track: 0,
        slot: FxSlot::HighPass,
        on: true,
    });
}

/// Every place a plugin can sit: in front of each built-in effect, and the end.
fn positions() -> Vec<InsertPosition> {
    let mut all: Vec<InsertPosition> = FxSlot::all().into_iter().map(InsertPosition::before).collect();
    all.push(InsertPosition::END);
    all
}

#[test]
fn positions_are_spelled_as_the_score_writes_them() {
    assert_eq!(InsertPosition::parse(None), Some(InsertPosition::END));
    assert_eq!(InsertPosition::parse(Some("eq")), Some(InsertPosition::before(FxSlot::Eq)));
    assert_eq!(InsertPosition::parse(Some("reverb")).unwrap().name(), Some("reverb"));
    assert_eq!(InsertPosition::parse(Some("chorus")), None);
    assert_eq!(InsertPosition::END.name(), None);
    assert_eq!(InsertPosition::default(), InsertPosition::END);
    assert!(InsertPosition::before(FxSlot::Octaver) < InsertPosition::before(FxSlot::Reverb));
    assert!(InsertPosition::before(FxSlot::Reverb) < InsertPosition::END);
}

/// A DC offset added by a plugin survives when the plugin sits after the high-pass and is removed
/// when it sits in front of it - the signal really passes the devices in the listed order.
#[test]
fn a_plugin_in_front_of_the_high_pass_is_filtered_by_it() {
    let settled = |position: InsertPosition| {
        // Silence in; the only signal is what the plugin adds.
        let (mut sim, mut tx) = with_rack(spec(), mono(|_| 0.0));
        high_pass_on(&mut sim);
        install_at(&mut tx, 0, Offset(0.25), 0, position);
        sim.run_blocks(400); // about a second: the high-pass has long settled
        sim.out(sim.pos() - 1).l
    };
    for slot in [FxSlot::Octaver, FxSlot::HighPass] {
        let out = settled(InsertPosition::before(slot));
        assert!(out.abs() < 1e-3, "vor {slot:?}: der Hochpass nimmt den Gleichanteil, {out}");
    }
    for position in [
        InsertPosition::before(FxSlot::Eq),
        InsertPosition::before(FxSlot::Reverb),
        InsertPosition::END,
    ] {
        let out = settled(position);
        assert!((out - 0.25).abs() < 1e-6, "{position:?}: hinter dem Hochpass bleibt er, {out}");
    }
}

/// The position decides the order, the slot only among plugins at the same position: a plugin in
/// slot 2 placed in front of the chain runs before one in slot 1 at the end.
#[test]
fn position_comes_before_slot_order() {
    let (mut sim, mut tx) = with_rack(spec(), mono(|_| 0.5));
    install_at(&mut tx, 0, Offset(0.25), 0, InsertPosition::END);
    install_at(&mut tx, 1, Gain(0.0), 0, InsertPosition::before(FxSlot::Octaver));
    sim.run_blocks(4);
    // Gain 0 first (silences the input), then the chain, then the offset: 0.25. In slot order it
    // would have been (0.5 + 0.25) * 0 = 0.
    assert_eq!(sim.out(sim.pos() - 1), Frame::mono(0.25));
}

/// An identity plugin changes nothing wherever it sits - with a full preset on, monitored input,
/// and on split buses (where Monitor gets the built-in chain without the plugins). The split
/// chain computes exactly what the whole chain computes.
#[test]
fn an_identity_plugin_anywhere_changes_nothing() {
    let spec_of = || SimSpec {
        output_channels: 4,
        routing: Some(BusRouting::default_for(4)),
        tracks: vec![
            TrackSpec::on(0)
                .monitoring()
                .sending(BusSend::BOTH)
                .monitor_sending(BusSend::BOTH),
            TrackSpec::on(1)
                .monitoring()
                .sending(BusSend::MAIN)
                .monitor_sending(BusSend::MONITOR),
        ],
        input_channels: 2,
        ..spec()
    };
    let played: fn() -> Played = || Box::new(|k, c| fingerprint(k * 3 + c as u64) * 0.5);
    let run = |position: InsertPosition| {
        let (mut sim, mut tx) = with_rack(spec_of(), played());
        for track in 0..2 {
            sim.send(Command::LoadFxPreset {
                track,
                preset: FxPreset::Voice,
            });
        }
        // Let the preset's crossfades arrive first: while the whole-chain bypass fades, a split
        // chain fades each segment on its own - click-free too, but not the same numbers.
        sim.run_blocks(200);
        for track in 0..2 {
            tx.send(InsertMsg::Install {
                target: InsertTarget::Track(track),
                slot: 0,
                insert: Insert::new(Box::new(Gain(1.0)), InsertShared::new(0)).at(position),
            })
            .unwrap();
        }
        sim.run_blocks(60);
        sim
    };
    let reference = run(InsertPosition::END);
    for position in positions() {
        let sim = run(position);
        for bus in Bus::ALL {
            for c in 0..2 {
                assert_eq!(
                    reference.buses[bus.index()][c],
                    sim.buses[bus.index()][c],
                    "{position:?}, {bus:?}/{c}"
                );
            }
        }
    }
}

/// Latency is the sum of everything heard, wherever it sits: a plugin with lookahead in front of
/// the octaver adds to the octaver's own latency, and the loop is read ahead by both.
#[test]
fn latencies_add_up_wherever_the_plugins_sit() {
    const L: u32 = 8;
    for position in positions() {
        let (mut sim, mut tx) = with_rack(spec(), mono(|_| 0.0));
        sim.send(Command::SetFxBypass { track: 0, on: false });
        sim.send(Command::SetFxEnabled {
            track: 0,
            slot: FxSlot::Octaver,
            on: true,
        });
        install_at(&mut tx, 0, Delay::new(L as usize), L, position);
        install_at(&mut tx, 1, Delay::new(3), 3, InsertPosition::END);
        sim.run_blocks(2);
        assert_eq!(
            sim.core.track(0).playback_advance(),
            u64::from(L + 3 + octaver::latency_frames(RATE)),
            "{position:?}"
        );
    }
}

/// Moving a plugin across the high-pass while a signal runs changes the sound - but under a dip
/// of the track, never with a step: no two neighbouring samples are further apart than the fade
/// allows, and afterwards the plugin is where it was put.
#[test]
fn moving_a_plugin_while_it_plays_dips_instead_of_clicking() {
    let (mut sim, mut tx) = with_rack(spec(), mono(|_| 0.0));
    high_pass_on(&mut sim);
    install_at(&mut tx, 0, Offset(0.25), 0, InsertPosition::END);
    sim.run_blocks(20);
    let from = sim.pos();
    assert_eq!(sim.out(from - 1), Frame::mono(0.25));
    tx.send(InsertMsg::Place {
        target: InsertTarget::Track(0),
        from: 0,
        to: 0,
        position: InsertPosition::before(FxSlot::HighPass),
    })
    .unwrap();
    sim.run_blocks(400);
    let step = 0.25 / REORDER_FADE as f32;
    let mut lowest = f32::MAX;
    for pos in from + 1..sim.pos() {
        let (a, b) = (sim.out(pos - 1).l, sim.out(pos).l);
        assert!((b - a).abs() <= step * 1.5, "Sprung bei {pos}: {a} -> {b}");
        lowest = lowest.min(b.abs());
    }
    assert_eq!(lowest, 0.0, "the track went through silence while the slots changed");
    let last = sim.out(sim.pos() - 1).l;
    assert!(last.abs() < 1e-3, "now in front of the high-pass, which takes the offset: {last}");
}

/// Two reorders in a row are both carried out, in order, each under its own dip.
#[test]
fn reorders_queue_up_and_keep_their_order() {
    let (mut sim, mut tx) = with_rack(spec(), mono(|_| 0.5));
    install_at(&mut tx, 0, Gain(0.0), 0, InsertPosition::END);
    install_at(&mut tx, 1, Offset(0.25), 0, InsertPosition::END);
    sim.run_blocks(2);
    assert_eq!(sim.out(sim.pos() - 1), Frame::mono(0.25), "gain 0, then the offset");
    // Offset to the front, then the gain in front of it again.
    for (from, to) in [(1, 0), (1, 0)] {
        tx.send(InsertMsg::Move {
            target: InsertTarget::Track(0),
            from,
            to,
        })
        .unwrap();
    }
    sim.run_blocks(20);
    assert_eq!(sim.out(sim.pos() - 1), Frame::mono(0.25), "two swaps: back where it was");
    tx.send(InsertMsg::Move {
        target: InsertTarget::Track(0),
        from: 1,
        to: 0,
    })
    .unwrap();
    sim.run_blocks(20);
    assert_eq!(sim.out(sim.pos() - 1), Frame::SILENT, "one swap: offset first, then gain 0");
}
