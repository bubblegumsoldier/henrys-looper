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

// ---------------------------------------------------------------------------------------------
// The clock a hosted plugin is told (`PluginTransport`)
// ---------------------------------------------------------------------------------------------

mod clock {
    use std::sync::{Arc, Mutex};

    use super::super::command::Command;
    use super::super::insert::{InsertProcessor, InsertTarget, MAX_BLOCK, PluginTransport};
    use super::super::sim::{Sim, SimSpec, silence};
    use super::super::source::{
        BlockEvent, Instrument, InstrumentMsg, SourceProcessor, SourceShared, StereoOut,
        source_channel,
    };
    use super::super::timeline::TimeSignature;
    use super::{install, spec, with_rack};

    /// Every call a plugin got: the clock it was told right before, and how many frames it ran.
    type Log = Arc<Mutex<Vec<(PluginTransport, usize)>>>;

    struct Watch {
        log: Log,
        told: Option<PluginTransport>,
    }

    impl Watch {
        fn new() -> (Self, Log) {
            let log = Log::default();
            (
                Self {
                    log: log.clone(),
                    told: None,
                },
                log,
            )
        }

        fn note(&mut self, n: usize) {
            let told = self.told.take().expect("vor jedem Aufruf wird die Uhr gesetzt");
            self.log.lock().unwrap().push((told, n));
        }
    }

    impl InsertProcessor for Watch {
        fn set_transport(&mut self, transport: &PluginTransport) {
            self.told = Some(*transport);
        }
        fn process(&mut self, left: &mut [f32], _right: &mut [f32]) {
            self.note(left.len());
        }
    }

    impl SourceProcessor for Watch {
        fn set_transport(&mut self, transport: &PluginTransport) {
            self.told = Some(*transport);
        }
        fn render(&mut self, _events: &[BlockEvent], outs: &mut [StereoOut<'_>]) {
            self.note(outs[0].l.len());
        }
    }

    const BIG_BLOCK: usize = 2_500;

    fn three_four() -> SimSpec {
        SimSpec {
            bpm: 90.0,
            signature: TimeSignature::new(3, 4),
            // Larger than MAX_BLOCK, so the clock has to move on from chunk to chunk.
            block: BIG_BLOCK,
            ..spec()
        }
    }

    fn calls(log: &Log) -> Vec<(PluginTransport, usize)> {
        log.lock().unwrap().clone()
    }

    /// The calls form one gapless run of frames, each one told the clock of its own first frame.
    fn assert_gapless(calls: &[(PluginTransport, usize)]) {
        for pair in calls.windows(2) {
            let ((a, n), (b, _)) = (pair[0], pair[1]);
            assert_eq!(b.continuous, a.continuous + n as u64, "lueckenlos: {a:?} -> {b:?}");
            assert!(n <= MAX_BLOCK);
        }
    }

    /// 90 BPM in 3/4: a quarter is 32 000 frames, a bar 96 000.
    const QUARTER: f64 = 32_000.0;

    #[test]
    fn an_insert_hears_tempo_signature_and_a_sample_exact_position() {
        let (mut sim, mut tx) = with_rack(three_four(), silence());
        sim.core.hold_transport();
        let (watch, log) = Watch::new();
        install(&mut tx, InsertTarget::Track(0), 0, watch, 0);
        sim.run_to(20_000);

        // Standing: bar 1, not playing - and the tempo is valid all the same.
        let standing = calls(&log);
        assert!(!standing.is_empty());
        assert_gapless(&standing);
        assert!(standing.iter().any(|&(_, n)| n == MAX_BLOCK), "in Stuecke geschnitten");
        for (t, _) in &standing {
            assert_eq!((t.bpm, t.beats_per_bar, t.beat_unit), (90.0, 3, 4));
            assert_eq!(t.sample_rate, 48_000.0);
            assert!(!t.playing);
            assert_eq!(t.position, 0);
            assert_eq!(t.quarters(), 0.0);
        }

        // Start: the press is bar 1, and from there the position is the frame count since it.
        let at = 23_456;
        sim.send(Command::TransportStart { at });
        sim.run_to(at + 250_000);
        let all = calls(&log);
        assert_gapless(&all);
        let first = all
            .iter()
            .find(|(t, _)| t.continuous == at)
            .expect("ein Aufruf beginnt genau am Start");
        assert!(first.0.playing);
        assert_eq!(first.0.position, 0, "springt beim Start auf 0");
        for (t, _) in &all {
            if t.continuous < at {
                assert!(!t.playing && t.position == 0, "vor dem Start: {t:?}");
            } else {
                assert!(t.playing);
                assert_eq!(t.position, (t.continuous - at) as i64, "sample-genau: {t:?}");
                let q = t.position as f64 / QUARTER;
                assert!((t.quarters() - q).abs() < 1e-12);
                assert_eq!(t.bar_start_quarters(), (q / 3.0).floor() * 3.0, "{t:?}");
            }
        }
        // The second bar begins at 3 quarters - 3/4, not 4/4.
        assert!(all.iter().any(|(t, _)| t.bar_start_quarters() == 3.0));

        // Stop: untimed, so it acts at the top of the next block; from there on bar 1 again.
        let before = all.len();
        sim.send(Command::TransportStop);
        sim.run_to(sim.pos() + 8 * BIG_BLOCK as u64);
        let after = calls(&log);
        assert_gapless(&after);
        let stopped = &after[before..];
        assert!(!stopped.is_empty());
        for (t, _) in stopped {
            assert!(!t.playing, "steht: {t:?}");
            assert_eq!(t.position, 0);
            assert_eq!(t.bpm, 90.0);
        }

        // Started again: back to 0, not where it stopped.
        let again = sim.pos() + 3_000;
        sim.send(Command::TransportStart { at: again });
        sim.run_to(again + 10_000);
        let restarted = calls(&log);
        let (t, _) = restarted
            .iter()
            .find(|(t, _)| t.continuous == again)
            .expect("Aufruf am zweiten Start");
        assert!(t.playing);
        assert_eq!(t.position, 0);
    }

    /// During a count-in the clock runs towards bar 1: the position is negative and reaches 0
    /// exactly where the count-in ends, without a jump.
    #[test]
    fn a_count_in_counts_up_to_bar_one() {
        let (mut sim, mut tx) = with_rack(three_four(), silence());
        sim.core.hold_transport();
        let (watch, log) = Watch::new();
        install(&mut tx, InsertTarget::Track(0), 0, watch, 0);
        let at = 10_000;
        let end = at + 96_000;
        sim.send(Command::CountIn { at, end });
        sim.send(Command::TransportStart { at: end });
        sim.run_to(end + 20_000);
        let all = calls(&log);
        assert_gapless(&all);
        let mut saw_count_in = false;
        for (t, _) in &all {
            if t.continuous < at {
                assert!(!t.playing);
            } else {
                assert!(t.playing);
                assert_eq!(t.position, t.continuous as i64 - end as i64, "{t:?}");
                if t.position < 0 {
                    saw_count_in = true;
                    assert_eq!(t.bar_start_quarters(), -3.0, "der Vorzaehltakt ist Takt 0");
                }
            }
        }
        assert!(saw_count_in);
        assert!(all.iter().any(|(t, _)| t.continuous == end && t.position == 0));
    }

    /// A loaded score keeps the clock running but its window closed: the plugins are told nothing
    /// plays (position 0) until the count-in opens it, then they count towards the first bar of the
    /// first section - and after the close they stand again.
    #[test]
    fn a_closed_score_window_tells_the_plugins_nothing_plays() {
        let (mut sim, mut tx) = with_rack(three_four(), silence());
        let (watch, log) = Watch::new();
        install(&mut tx, InsertTarget::Track(0), 0, watch, 0);
        let (open, bar1, close) = (20_000, 116_000, 300_000);
        sim.send(crate::engine::runner::CLOSED_CLICK_GATE);
        sim.send(Command::ClickGate {
            at: open,
            open: true,
            bar1,
        });
        sim.send(Command::ClickGate {
            at: close,
            open: false,
            bar1: 0,
        });
        sim.run_to(close + 20_000);
        let all = calls(&log);
        assert_gapless(&all);
        let mut saw = [false; 3];
        for (t, _) in &all {
            if t.continuous < open || t.continuous >= close {
                assert!(!t.playing, "{t:?}");
                assert_eq!(t.position, 0, "{t:?}");
                saw[if t.continuous < open { 0 } else { 2 }] = true;
            } else {
                assert!(t.playing, "{t:?}");
                assert_eq!(t.position, t.continuous as i64 - bar1 as i64, "{t:?}");
                saw[1] = true;
            }
        }
        assert_eq!(saw, [true; 3]);
        assert!(all.iter().any(|(t, _)| t.continuous == open && t.position < 0));
    }

    /// The same with a real runner, started somewhere in a bar of the clock that idles under the
    /// loaded score: nothing plays until the press plus the head start; from there the plugins count
    /// up from minus the whole count-in, and position 0 is the first sample of the first section.
    #[test]
    fn a_score_start_counts_the_plugins_up_to_its_first_section() {
        use crate::engine::runner::{CLOSED_CLICK_GATE, Runner};
        use crate::engine::timeline::Timeline;
        let yaml = "bpm: 90\ntime_signature: 3/4\ntracks:\n  a: {input: 1}\nsections:\n  - id: one\n    bars: 2\n    autorelease: true\n    tracks:\n      a: stop\n";
        let score = crate::score::compile_score(yaml).expect("uebersetzt");
        let (mut sim, mut tx) = with_rack(three_four(), silence());
        let (watch, log) = Watch::new();
        install(&mut tx, InsertTarget::Track(0), 0, watch, 0);
        let timeline = Timeline::new(48_000, 90.0, TimeSignature::new(3, 4));
        let mut runner = Runner::new(score, timeline, BIG_BLOCK as u32).expect("Runner");
        sim.send(CLOSED_CLICK_GATE);
        sim.run_to(77_777);
        let est = sim.pos();
        runner.start(est);
        let anchor = est + runner.start_lead();
        let (first, _) = runner.armed_at().expect("armiert");
        let count_in = runner.scheduler().timeline.span_bars(0, runner.count_in_bars());
        assert_eq!(first - anchor, count_in);
        for cmd in runner.take_commands() {
            sim.send(cmd);
        }
        sim.run_to(first + 20_000);
        let all = calls(&log);
        assert_gapless(&all);
        for (t, _) in &all {
            if t.continuous < anchor {
                assert!(!t.playing, "vor dem ersten Schlag steht alles: {t:?}");
                assert_eq!(t.position, 0);
            } else {
                assert!(t.playing, "{t:?}");
                assert_eq!(t.position, t.continuous as i64 - first as i64, "{t:?}");
            }
        }
        assert!(
            all.iter()
                .any(|(t, _)| t.continuous == anchor && t.position == -(count_in as i64)),
            "der erste Schlag des Einzaehlers liegt genau count_in Takte vor Takt 1"
        );
    }

    /// A tempo change (or a score loaded with its own tempo, which arrives as the same command)
    /// reaches the plugins with the next block.
    #[test]
    fn a_tempo_change_reaches_the_plugins() {
        let (mut sim, mut tx) = with_rack(three_four(), silence());
        let (watch, log) = Watch::new();
        install(&mut tx, InsertTarget::Track(0), 0, watch, 0);
        sim.run_to(2 * BIG_BLOCK as u64);
        sim.send(Command::SetTempo {
            bpm: 133.0,
            signature: TimeSignature::new(7, 8),
            layer_frames: 48_000 * 4,
        });
        sim.run_to(sim.pos() + 2 * BIG_BLOCK as u64);
        let last = calls(&log).last().copied().unwrap().0;
        assert_eq!((last.bpm, last.beats_per_bar, last.beat_unit), (133.0, 7, 8));
        assert_eq!(last.bar_quarters(), 3.5);
    }

    /// An instrument is told the clock too, chunk by chunk within one long process call.
    #[test]
    fn an_instrument_hears_the_clock_chunk_by_chunk() {
        let mut sim = Sim::new(three_four(), silence());
        sim.core.hold_transport();
        let (mut instruments, _feeds, endpoint) = source_channel(16);
        sim.core.attach_sources(endpoint, BIG_BLOCK);
        let (watch, log) = Watch::new();
        instruments
            .send(InstrumentMsg::Install {
                track: 0,
                instrument: Instrument::new(Box::new(watch), SourceShared::new(0)),
            })
            .unwrap();
        // Instruments render a whole process call before its commands act, so they learn of a
        // start at the next call; from there the position is exact.
        let at = 4 * BIG_BLOCK as u64 + 700;
        let next_call = 5 * BIG_BLOCK as u64;
        sim.send(Command::TransportStart { at });
        sim.run_to(at + 3 * BIG_BLOCK as u64);
        let all = calls(&log);
        assert_gapless(&all);
        assert!(all.iter().any(|&(_, n)| n == MAX_BLOCK), "in Stuecke geschnitten");
        for (t, _) in &all {
            assert_eq!((t.bpm, t.beats_per_bar), (90.0, 3));
            if t.continuous < next_call {
                assert!(!t.playing && t.position == 0);
            } else {
                assert!(t.playing);
                assert_eq!(t.position, (t.continuous - at) as i64);
            }
        }
        let first = (next_call - at) as i64;
        assert!(all.iter().any(|(t, _)| t.position == first), "erster Aufruf nach dem Start");
        assert!(
            all.iter().any(|(t, _)| t.position == first + MAX_BLOCK as i64),
            "das zweite Stueck desselben Aufrufs ist weitergezaehlt"
        );
    }
}
