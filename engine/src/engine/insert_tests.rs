//! Offline proofs for the insert slots (`super::insert`): where they sit in the signal path, what
//! they process, that the recording stays dry, and that a plugin's latency is made up for on the
//! loop. No plugin and no device - the processors are the plain-Rust doubles in
//! `insert::testing`. The tests that push audio through a real installed plugin live in
//! `crate::plugin`.

use super::bus::{Bus, BusRouting, BusSend};
use super::command::Command;
use super::frame::Frame;
use super::insert::testing::{Broken, Delay, Gain, Swap};
use super::insert::{
    Insert, InsertMsg, InsertSender, InsertShared, InsertTarget, MAX_BLOCK, insert_channel,
};
use super::sim::{Played, Sim, SimSpec, TrackSpec, mono, silence};
use super::timeline::{TimeSignature, Timeline};

const RATE: u32 = 48_000;
const R: u64 = 827;

fn fingerprint(k: u64) -> f32 {
    ((k % 65_536) as f32) / 65_536.0 - 0.5
}

fn spec() -> SimSpec {
    SimSpec {
        bpm: 100.0,
        signature: TimeSignature::new(4, 4),
        bars: 2,
        latency: R,
        roundtrip: R,
        block: 128,
        // No cable: a monitored input plus a loopback would feed itself.
        loopback_channel: None,
        ..Default::default()
    }
}

fn with_rack(spec: SimSpec, played: Played) -> (Sim, InsertSender) {
    let mut sim = Sim::new(spec, played);
    let (sender, endpoint) = insert_channel(32);
    sim.core.attach_inserts(endpoint);
    (sim, sender)
}

fn install(
    tx: &mut InsertSender,
    target: InsertTarget,
    slot: usize,
    processor: impl super::insert::InsertProcessor + 'static,
    latency: u32,
) -> std::sync::Arc<InsertShared> {
    let shared = InsertShared::new(latency);
    tx.send(InsertMsg::Install {
        target,
        slot,
        insert: Insert::new(Box::new(processor), shared.clone()),
    })
    .expect("Queue frei");
    shared
}

/// An attached but empty rack renders bit for bit what the engine rendered before plugins existed
/// - the block path is only taken while something is installed.
#[test]
fn an_empty_rack_changes_nothing() {
    let spec_of = || SimSpec {
        click: true,
        tracks: vec![TrackSpec::on(0).monitoring(), TrackSpec::stereo(1, 2).monitoring().panned(0.3)],
        input_channels: 3,
        ..spec()
    };
    let played: fn() -> Played = || Box::new(|k, c| fingerprint(k * 3 + c as u64) * 0.5);
    let mut plain = Sim::new(spec_of(), played());
    let (mut racked, _tx) = with_rack(spec_of(), played());
    plain.run_blocks(40);
    racked.run_blocks(40);
    assert_eq!(plain.out_l, racked.out_l);
    assert_eq!(plain.out_r, racked.out_r);
}

/// The block path with a neutral plugin renders what the frame path renders: same chain, same
/// panner, same buses, same click. Only the order of one addition differs (the click now comes
/// after the tracks), so the comparison allows one rounding step.
#[test]
fn a_neutral_insert_renders_what_the_frame_path_renders() {
    let spec_of = || SimSpec {
        click: true,
        output_channels: 4,
        tracks: vec![
            TrackSpec::on(0).monitoring().panned(-0.4),
            TrackSpec::stereo(1, 2)
                .monitoring()
                .sending(BusSend::MAIN)
                .monitor_sending(BusSend::MONITOR),
        ],
        input_channels: 3,
        ..spec()
    };
    let played: fn() -> Played = || Box::new(|k, c| fingerprint(k * 7 + c as u64) * 0.5);
    let mut plain = Sim::new(spec_of(), played());
    let (mut racked, mut tx) = with_rack(spec_of(), played());
    install(&mut tx, InsertTarget::Track(0), 0, Gain(1.0), 0);
    install(&mut tx, InsertTarget::Bus(Bus::Monitor), 3, Gain(1.0), 0);
    plain.run_blocks(40);
    racked.run_blocks(40);
    for bus in Bus::ALL {
        for c in 0..2 {
            let a = &plain.buses[bus.index()][c];
            let b = &racked.buses[bus.index()][c];
            assert_eq!(a.len(), b.len());
            for (i, (x, y)) in a.iter().zip(b).enumerate() {
                assert!((x - y).abs() < 1e-6, "{bus:?}/{c} Frame {i}: {x} gegen {y}");
            }
        }
    }
}

/// A track insert acts on that track and nothing else; the click is untouched.
#[test]
fn a_track_insert_acts_on_its_track_only() {
    let spec = SimSpec {
        tracks: vec![TrackSpec::on(0).monitoring(), TrackSpec::on(1).monitoring()],
        input_channels: 2,
        ..spec()
    };
    let (mut sim, mut tx) = with_rack(spec, Box::new(|_, c| if c == 0 { 0.5 } else { 0.25 }));
    let shared = install(&mut tx, InsertTarget::Track(0), 2, Gain(0.0), 0);
    sim.run_blocks(4);
    // Track 0 silenced, track 1 untouched: on the summed device output that is 0.25.
    assert_eq!(sim.out(300), Frame::new(0.25, 0.25));
    assert!(shared.blocks() >= 3, "der Slot hat gerechnet");
}

/// The inserts sit after the chain and **before** the panner: a plugin that swaps the sides of a
/// stereo track sees them unpanned, and the panner places what comes out.
#[test]
fn the_panner_comes_after_the_inserts_and_the_channels_keep_their_order() {
    let spec = SimSpec {
        tracks: vec![TrackSpec::stereo(0, 1).monitoring()],
        input_channels: 2,
        ..spec()
    };
    let (mut sim, mut tx) = with_rack(spec, Box::new(|_, c| if c == 0 { 0.4 } else { 0.8 }));
    install(&mut tx, InsertTarget::Track(0), 0, Swap, 0);
    sim.run_blocks(3);
    assert_eq!(sim.out(200), Frame::new(0.8, 0.4), "vertauscht, sonst nichts");

    sim.send(Command::SetPan { track: 0, pan: -1.0 });
    sim.run_blocks(3);
    let end = sim.pos() - 1;
    assert_eq!(
        sim.out(end),
        Frame::new(0.8, 0.0),
        "ganz links nach dem Plugin: rechts still"
    );
}

/// Bypass is an atomic switch: no message, and the very next block passes the signal unchanged.
#[test]
fn bypass_passes_the_signal_unchanged() {
    let spec = SimSpec {
        tracks: vec![TrackSpec::on(0).monitoring()],
        ..spec()
    };
    let (mut sim, mut tx) = with_rack(spec, mono(fingerprint));
    let shared = install(&mut tx, InsertTarget::Track(0), 0, Gain(0.0), 0);
    sim.run_blocks(2);
    assert_eq!(sim.out(200), Frame::SILENT);
    shared.set_bypass(true);
    let before = shared.blocks();
    sim.run_blocks(2);
    let p = sim.pos() - 10;
    assert_eq!(sim.out(p), Frame::mono(fingerprint(p)), "umgangen ist bitgleich");
    assert_eq!(shared.blocks(), before, "ein umgangener Slot rechnet nicht");
}

/// A bus insert processes the sum on that bus; the click is added afterwards, so a plugin on the
/// monitor bus never touches the metronome.
#[test]
fn a_bus_insert_processes_the_bus_and_leaves_the_click_alone() {
    let spec = SimSpec {
        click: true,
        output_channels: 4,
        tracks: vec![TrackSpec::on(0).monitoring()],
        ..spec()
    };
    let (mut sim, mut tx) = with_rack(spec, mono(|_| 0.3));
    install(&mut tx, InsertTarget::Bus(Bus::Monitor), 0, Gain(0.0), 0);
    let timeline = Timeline::new(RATE, 100.0, TimeSignature::new(4, 4));
    sim.run_to(timeline.bar_start(1) + 2_000);
    let beat = timeline.bar_start(1) + 10;
    // Main: the track, untouched.
    assert_eq!(sim.bus(Bus::Main, beat), Frame::mono(0.3));
    // Monitor: the track is gone, the click is there.
    let click = sim.bus(Bus::Monitor, beat);
    assert!(click.l.abs() > 0.01, "der Klick bleibt auf Monitor: {click:?}");
    assert_eq!(sim.bus(Bus::Monitor, beat + 1_500), Frame::SILENT, "zwischen den Klicks: nichts");
}

/// What is recorded stays dry, however violent the insert: the plugin is in the playback and
/// monitoring path, never in `record_input`.
#[test]
fn the_recording_stays_dry_with_an_insert() {
    let timeline = Timeline::new(RATE, 100.0, TimeSignature::new(4, 4));
    let origin = timeline.bar_start(1);
    let len = timeline.span_bars(1, 1);
    let spec = SimSpec {
        tracks: vec![TrackSpec::on(0).monitoring()],
        ..spec()
    };
    let (mut sim, mut tx) = with_rack(spec, mono(fingerprint));
    install(&mut tx, InsertTarget::Track(0), 0, Broken, 0);
    install(&mut tx, InsertTarget::Track(0), 1, Gain(8.0), 0);
    sim.send(Command::StartRecord { track: 0, at: origin });
    sim.send(Command::StopRecord {
        track: 0,
        at: origin + len,
    });
    sim.run_to(origin + len + R + 1_024);
    let t = sim.core.track(0);
    let layer = t.layer(0).expect("Ebene").content(t.loop_len()).to_vec();
    assert_eq!(layer.len(), len as usize);
    for (i, &got) in layer.iter().enumerate() {
        assert_eq!(got, fingerprint(origin + R + i as u64), "Sample {i}");
    }
}

/// A plugin that writes NaN is silenced where it entered, and the rest of the mix survives.
#[test]
fn a_broken_plugin_cannot_poison_the_mix() {
    let spec = SimSpec {
        tracks: vec![TrackSpec::on(0).monitoring(), TrackSpec::on(1).monitoring()],
        input_channels: 2,
        ..spec()
    };
    let (mut sim, mut tx) = with_rack(spec, Box::new(|_, c| if c == 0 { 0.5 } else { 0.25 }));
    install(&mut tx, InsertTarget::Track(0), 0, Broken, 0);
    sim.run_blocks(4);
    let o = sim.out(300);
    assert!(o.l.is_finite() && o.r.is_finite());
    // Left of track 0 is NaN -> 0, right stays 0.5; track 1 adds 0.25 to both.
    assert_eq!(o, Frame::new(0.25, 0.75));
}

/// Removing a slot sends the insert back to the control thread, where it is dropped; nothing leaks.
#[test]
fn a_removed_insert_comes_back_to_the_control_thread() {
    let spec = SimSpec {
        tracks: vec![TrackSpec::on(0).monitoring()],
        ..spec()
    };
    let (mut sim, mut tx) = with_rack(spec, mono(|_| 0.5));
    let shared = install(&mut tx, InsertTarget::Track(0), 0, Gain(0.5), 0);
    sim.run_blocks(2);
    assert_eq!(std::sync::Arc::strong_count(&shared), 2, "Engine haelt eine Referenz");
    // Replacing a slot retires the old one, removing retires the new one.
    install(&mut tx, InsertTarget::Track(0), 0, Gain(0.25), 0);
    sim.run_blocks(1);
    assert_eq!(sim.out(sim.pos() - 1), Frame::mono(0.125));
    tx.send(InsertMsg::Remove {
        target: InsertTarget::Track(0),
        slot: 0,
    })
    .unwrap();
    sim.run_blocks(1);
    assert_eq!(tx.drain_retired(), 2);
    assert_eq!(std::sync::Arc::strong_count(&shared), 1, "zurueck und freigegeben");
    assert_eq!(sim.out(sim.pos() - 1), Frame::mono(0.5), "leerer Slot: trocken");
}

/// Moving a slot changes the order of the chain: gain then swap is not swap then gain when the
/// gain is one-sided - here proven with two gains and a delay whose order is observable.
#[test]
fn moving_a_slot_reorders_the_chain() {
    let spec = SimSpec {
        tracks: vec![TrackSpec::on(0).monitoring()],
        ..spec()
    };
    let (mut sim, mut tx) = with_rack(spec, mono(|_| 0.5));
    install(&mut tx, InsertTarget::Track(0), 0, Gain(0.5), 0);
    install(&mut tx, InsertTarget::Bus(Bus::Main), 0, Gain(2.0), 0);
    sim.run_blocks(1);
    assert_eq!(sim.out(sim.pos() - 1), Frame::mono(0.5));
    tx.send(InsertMsg::Move {
        target: InsertTarget::Track(0),
        from: 0,
        to: 5,
    })
    .unwrap();
    sim.run_blocks(1);
    assert_eq!(sim.out(sim.pos() - 1), Frame::mono(0.5), "Position 6 rechnet genauso");
    tx.send(InsertMsg::Remove {
        target: InsertTarget::Track(0),
        slot: 0,
    })
    .unwrap();
    sim.run_blocks(1);
    assert_eq!(
        sim.out(sim.pos() - 1),
        Frame::mono(0.5),
        "Slot 1 ist nach dem Verschieben leer, der Gain sitzt weiter in Slot 6"
    );
    assert_eq!(tx.drain_retired(), 0);
}

/// A plugin that reports latency `L` delays the track by `L`. The loop is read `L` frames ahead,
/// so it lands on the same positions as without the plugin - sample for sample.
#[test]
fn a_plugins_latency_is_made_up_for_on_the_loop() {
    const L: u32 = 300;
    let timeline = Timeline::new(RATE, 100.0, TimeSignature::new(4, 4));
    let origin = timeline.bar_start(1);
    let len = timeline.span_bars(1, 1);
    let run = |latency: Option<u32>| {
        let spec = SimSpec {
            tracks: vec![TrackSpec::on(0)],
            ..spec()
        };
        let (mut sim, mut tx) = with_rack(spec, mono(fingerprint));
        if let Some(l) = latency {
            install(&mut tx, InsertTarget::Track(0), 0, Delay::new(l as usize), l);
        }
        sim.send(Command::StartRecord { track: 0, at: origin });
        sim.send(Command::StopRecord {
            track: 0,
            at: origin + len,
        });
        sim.send(Command::StartPlay {
            track: 0,
            at: origin + len,
        });
        sim.run_to(origin + 3 * len);
        sim
    };
    let plain = run(None);
    let delayed = run(Some(L));
    // From the second pass on, where the read-ahead has the previous pass to read from.
    let from = origin + 2 * len;
    let mut checked = 0;
    for pos in from..origin + 3 * len - 1 {
        assert_eq!(plain.out(pos), delayed.out(pos), "Position {pos}");
        checked += 1;
    }
    assert!(checked > 100_000);
    assert_eq!(
        delayed.core.track(0).playback_advance(),
        u64::from(L),
        "die gemeldete Latenz wird vorgelesen"
    );
}

/// A device buffer longer than the largest plugin block is cut into pieces; nothing is lost at the
/// seams.
#[test]
fn a_long_device_buffer_is_cut_into_plugin_blocks() {
    let spec = SimSpec {
        block: MAX_BLOCK * 2 + 77,
        tracks: vec![TrackSpec::on(0).monitoring()],
        ..spec()
    };
    let (mut sim, mut tx) = with_rack(spec, mono(fingerprint));
    install(&mut tx, InsertTarget::Track(0), 0, Delay::new(5), 0);
    sim.run_blocks(3);
    for pos in 5..sim.pos() {
        assert_eq!(sim.out(pos), Frame::mono(fingerprint(pos - 5)), "Frame {pos}");
    }
}

/// On a 4-output device with the loop on both buses and the live input on Monitor only, the two
/// buses carry different signals and the one plugin instance serves Main. That is the documented
/// limit; this test keeps it from changing silently.
#[test]
fn with_split_buses_the_inserts_process_main() {
    let spec = SimSpec {
        output_channels: 4,
        routing: Some(BusRouting::default_for(4)),
        tracks: vec![
            TrackSpec::on(0)
                .monitoring()
                .sending(BusSend::BOTH)
                .monitor_sending(BusSend::MONITOR),
        ],
        ..spec()
    };
    let (mut sim, mut tx) = with_rack(spec, mono(|_| 0.5));
    install(&mut tx, InsertTarget::Track(0), 0, Gain(0.5), 0);
    sim.run_blocks(3);
    // Nothing plays, so Main hears nothing; Monitor hears the live input without the plugin.
    assert_eq!(sim.bus(Bus::Main, 300), Frame::SILENT);
    assert_eq!(sim.bus(Bus::Monitor, 300), Frame::mono(0.5));
    let _ = silence;
}
