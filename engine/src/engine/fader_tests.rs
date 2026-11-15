//! Offline proofs for the channel strip: fader, mute and solo (`Track::fader_next`, `Track::strip`).
//!
//! Where they sit (after the chain and the inserts, before the panner), that a change is a ramp and
//! not a step, that solo silences the others including their monitored input, and that none of it
//! reaches a recording. No device - the simulated engine of `super::sim`.

use super::command::Command;
use super::frame::Frame;
use super::insert::testing::Gain;
use super::insert::{Insert, InsertMsg, InsertShared, InsertTarget, insert_channel};
use super::sim::{Sim, SimSpec, TrackSpec, mono};
use super::timeline::{TimeSignature, Timeline};
use super::track::MAX_TRACK_GAIN;

const RATE: u32 = 48_000;
const R: u64 = 827;

fn fingerprint(k: u64) -> f32 {
    ((k % 65_536) as f32) / 65_536.0 - 0.5
}

/// No cable: a monitored input plus a loopback would feed itself.
fn spec(tracks: Vec<TrackSpec>, input_channels: usize) -> SimSpec {
    SimSpec {
        bars: 2,
        latency: R,
        roundtrip: R,
        loopback_channel: None,
        tracks,
        input_channels,
        ..Default::default()
    }
}

/// Two monitored mono tracks, a constant 0.5 on input 1 and 0.25 on input 2. With two outputs both
/// buses land on one pair, so the device output is simply the sum of the two strips.
fn two_tracks() -> Sim {
    Sim::new(
        spec(vec![TrackSpec::on(0).monitoring(), TrackSpec::on(1).monitoring()], 2),
        Box::new(|_, c| if c == 0 { 0.5 } else { 0.25 }),
    )
}

fn last(sim: &Sim) -> Frame {
    sim.out(sim.pos() - 1)
}

/// The ramp has a 10 ms time constant and snaps once the rest is under 1e-5 - about 12 time
/// constants for a full jump. 80 blocks of 128 frames are 213 ms, well past that.
const SETTLE_BLOCKS: u64 = 80;

#[test]
fn minus_six_db_halves_the_strip_after_settling() {
    let mut sim = two_tracks();
    sim.run_blocks(4);
    assert_eq!(last(&sim), Frame::mono(0.75));
    sim.send(Command::SetTrackGain { track: 0, gain: 0.5 });
    sim.run_blocks(SETTLE_BLOCKS);
    assert_eq!(last(&sim), Frame::mono(0.5), "0.5 * 0.5 + 0.25");
}

#[test]
fn the_fader_ceiling_is_plus_twelve_db_and_nonsense_is_ignored() {
    let mut sim = two_tracks();
    sim.send(Command::SetTrackGain { track: 1, gain: 100.0 });
    sim.run_blocks(SETTLE_BLOCKS);
    assert_eq!(sim.core.track(1).gain(), MAX_TRACK_GAIN);
    assert!((20.0 * MAX_TRACK_GAIN.log10() - 12.0).abs() < 1e-4);
    sim.send(Command::SetTrackGain { track: 1, gain: f32::NAN });
    sim.run_blocks(2);
    assert_eq!(sim.core.track(1).gain(), MAX_TRACK_GAIN, "NaN aendert nichts");
    let o = last(&sim);
    assert!(o.l.is_finite());
}

/// A jump from unity to silence is a ramp: no sample-to-sample step anywhere near the size of the
/// jump. On a constant input the output can then only move by the ramp's own slope.
#[test]
fn a_fader_jump_is_a_ramp_not_a_step() {
    let mut sim = Sim::new(spec(vec![TrackSpec::on(0).monitoring()], 1), mono(|_| 0.8));
    sim.run_blocks(4);
    let from = sim.pos();
    sim.send(Command::SetTrackGain { track: 0, gain: 0.0 });
    sim.run_blocks(SETTLE_BLOCKS);
    let mut max_step = 0.0f32;
    for p in from..sim.pos() {
        max_step = max_step.max((sim.out(p).l - sim.out(p - 1).l).abs());
    }
    // A 10 ms one-pole covers at most 1/480 of the distance per sample: 0.8/480 ≈ 0.0017.
    assert!(max_step < 0.003, "Sprung von {max_step} - das waere ein Klick");
    assert_eq!(last(&sim), Frame::SILENT, "die Rampe kommt exakt an");
}

#[test]
fn mute_silences_and_unmute_restores() {
    let mut sim = two_tracks();
    sim.send(Command::SetTrackMute { track: 0, on: true });
    sim.run_blocks(SETTLE_BLOCKS);
    assert_eq!(last(&sim), Frame::mono(0.25));
    assert!(sim.core.track(0).muted());
    sim.send(Command::SetTrackMute { track: 0, on: false });
    sim.run_blocks(SETTLE_BLOCKS);
    assert_eq!(last(&sim), Frame::mono(0.75), "bitgleich wie vorher");
}

/// Solo on one track holds every other one at silence - its monitored input included - and lifting
/// the last solo brings them all back.
#[test]
fn solo_silences_the_others_including_their_monitored_input() {
    let mut sim = two_tracks();
    sim.send(Command::SetTrackSolo { track: 0, on: true });
    sim.run_blocks(SETTLE_BLOCKS);
    assert_eq!(last(&sim), Frame::mono(0.5), "nur Track 1 (Mithoeren) bleibt");

    // A second solo adds its track back.
    sim.send(Command::SetTrackSolo { track: 1, on: true });
    sim.run_blocks(SETTLE_BLOCKS);
    assert_eq!(last(&sim), Frame::mono(0.75));

    sim.send(Command::SetTrackSolo { track: 0, on: false });
    sim.send(Command::SetTrackSolo { track: 1, on: false });
    sim.run_blocks(SETTLE_BLOCKS);
    assert_eq!(last(&sim), Frame::mono(0.75), "ohne Solo hoeren alle");

    // A muted soloed track stays muted: mute wins.
    sim.send(Command::SetTrackSolo { track: 0, on: true });
    sim.send(Command::SetTrackMute { track: 0, on: true });
    sim.run_blocks(SETTLE_BLOCKS);
    assert_eq!(last(&sim), Frame::SILENT);
}

/// Solo, mute and fader are listening only: a track that is soloed away, muted and at silence
/// records exactly the raw input, sample for sample.
#[test]
fn the_strip_never_reaches_a_recording() {
    let timeline = Timeline::new(RATE, 100.0, TimeSignature::new(4, 4));
    let origin = timeline.bar_start(1);
    let len = timeline.span_bars(1, 1);
    let mut sim = Sim::new(
        spec(vec![TrackSpec::on(0).monitoring(), TrackSpec::on(1)], 2),
        Box::new(|k, c| if c == 0 { fingerprint(k) } else { 0.1 }),
    );
    sim.send(Command::SetTrackSolo { track: 1, on: true });
    sim.send(Command::SetTrackMute { track: 0, on: true });
    sim.send(Command::SetTrackGain { track: 0, gain: 0.0 });
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

/// At unity and unmuted the strip multiplies by exactly 1.0: a session that never touches a fader
/// renders bit for bit what it rendered before faders existed - checked against a round trip
/// through -6 dB and back, which has to land exactly where it started.
#[test]
fn unity_is_bit_identical() {
    let make = || {
        Sim::new(
            spec(
                vec![TrackSpec::on(0).monitoring().panned(-0.3), TrackSpec::stereo(1, 2).monitoring()],
                3,
            ),
            Box::new(|k, c| fingerprint(k * 3 + c as u64) * 0.5),
        )
    };
    let mut plain = make();
    let mut ridden = make();
    ridden.send(Command::SetTrackGain { track: 0, gain: 0.5 });
    ridden.run_blocks(SETTLE_BLOCKS);
    ridden.send(Command::SetTrackGain { track: 0, gain: 1.0 });
    ridden.run_blocks(SETTLE_BLOCKS);
    plain.run_blocks(3 * SETTLE_BLOCKS);
    ridden.run_blocks(SETTLE_BLOCKS);
    let tail = (2 * SETTLE_BLOCKS * 128) as usize;
    assert_eq!(plain.out_l[tail..], ridden.out_l[tail..]);
    assert_eq!(plain.out_r[tail..], ridden.out_r[tail..]);
}

/// The fader comes after the plugin inserts: on the block path a gain plugin and the fader
/// multiply, and the panner still comes last.
#[test]
fn the_fader_works_on_the_block_path_after_the_inserts() {
    let mut sim = Sim::new(
        spec(vec![TrackSpec::on(0).monitoring().panned(-1.0)], 1),
        mono(|_| 0.5),
    );
    let (mut tx, endpoint) = insert_channel(8);
    sim.core.attach_inserts(endpoint);
    tx.send(InsertMsg::Install {
        target: InsertTarget::Track(0),
        slot: 0,
        insert: Insert::new(Box::new(Gain(0.5)), InsertShared::new(0)),
    })
    .expect("Queue frei");
    sim.send(Command::SetTrackGain { track: 0, gain: 0.5 });
    sim.run_blocks(SETTLE_BLOCKS);
    assert_eq!(last(&sim), Frame::new(0.125, 0.0), "0.5 * Plugin 0.5 * Fader 0.5, ganz links");
    sim.send(Command::SetTrackMute { track: 0, on: true });
    sim.run_blocks(SETTLE_BLOCKS);
    assert_eq!(last(&sim), Frame::SILENT);
}

/// The snapshot reports the strip: fader, mute, solo and a post-fader, post-panner meter.
#[test]
fn the_status_reports_the_strip() {
    let mut sim = Sim::new(
        spec(vec![TrackSpec::on(0).monitoring().panned(1.0), TrackSpec::on(1)], 2),
        mono(|_| 0.5),
    );
    sim.send(Command::SetTrackGain { track: 0, gain: 0.5 });
    sim.send(Command::SetTrackMute { track: 1, on: true });
    sim.send(Command::SetTrackSolo { track: 0, on: true });
    sim.run_blocks(SETTLE_BLOCKS);
    let status = sim.latest_status().expect("Status");
    let a = status.tracks()[0];
    let b = status.tracks()[1];
    assert_eq!(a.gain, 0.5);
    assert!(a.soloed && !a.muted);
    assert!(b.muted && !b.soloed);
    assert_eq!(a.strip_peak, [0.0, 0.25], "nach dem Fader, ganz rechts");
    assert_eq!(b.strip_peak, [0.0, 0.0]);
    // The take meter is unchanged by the fader: it still says how loud the take is (no take here).
    assert_eq!(a.output_peak, [0.0, 0.0]);
}

/// "Clear all" empties the loops and leaves the balance alone.
#[test]
fn clear_all_keeps_the_balance() {
    let mut sim = two_tracks();
    sim.send(Command::SetTrackGain { track: 0, gain: 0.25 });
    sim.send(Command::SetTrackMute { track: 1, on: true });
    sim.send(Command::ClearAll { at: 0 });
    sim.run_blocks(SETTLE_BLOCKS);
    assert_eq!(sim.core.track(0).gain(), 0.25);
    assert!(sim.core.track(1).muted());
}

#[test]
fn the_strip_commands_are_untimed_and_name_their_track() {
    for cmd in [
        Command::SetTrackGain { track: 3, gain: 1.0 },
        Command::SetTrackMute { track: 3, on: true },
        Command::SetTrackSolo { track: 3, on: true },
    ] {
        assert_eq!(cmd.at(), None);
        assert_eq!(cmd.track(), Some(3));
    }
}
