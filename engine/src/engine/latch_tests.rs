//! Continuous overdub (`super::latch`), proved offline: every pass a layer of its own, no sample
//! lost or doubled at a seam, empty passes dropped, the ceiling ending it cleanly - and the one-pass
//! overdub exactly as before.

use super::command::{Command, Refusal};
use super::sim::{Played, Sim, SimSpec, mono};
use super::timeline::TimeSignature;
use super::track::MAX_LAYERS;

const BLOCK: usize = 128;
const R: u64 = 827;
/// Loop length of every test: short, and deliberately not a multiple of the block.
const L: u64 = 4_000;
/// First sample of the loop, mid-block as well.
const START: u64 = 4_000;

fn spec() -> SimSpec {
    SimSpec {
        bpm: 100.0,
        signature: TimeSignature::new(4, 4),
        bars: 8,
        latency: R,
        roundtrip: R,
        block: BLOCK,
        // No cable from the output back in: an overdub must record only what is played.
        loopback_channel: None,
        layer_frames: Some(L),
        ..Default::default()
    }
}

/// Unique per input frame and never quiet for long: every pass is "played".
fn fingerprint(k: u64) -> f32 {
    ((k % 65_536) as f32) / 65_536.0 - 0.5
}

/// Start of pass `n` (0 is the loop-defining take) in musical positions.
fn pass(n: u64) -> u64 {
    START + n * L
}

/// The input frame that lands on musical position `p`.
fn input_of(p: u64) -> u64 {
    p + R
}

fn record_loop(sim: &mut Sim) {
    sim.send(Command::StartRecord { track: 0, at: START });
    sim.send(Command::StopRecord {
        track: 0,
        at: START + L,
    });
    sim.send(Command::StartPlay {
        track: 0,
        at: START + L,
    });
}

/// Run until the take covering pass `n` has been closed (its last input frame arrived).
fn run_past_pass(sim: &mut Sim, n: u64) {
    sim.run_to(input_of(pass(n + 1)) + 2 * BLOCK as u64);
}

fn content(sim: &Sim, layer: usize) -> Vec<f32> {
    let t = sim.core.track(0);
    t.layer(layer).expect("Ebene existiert").content(L).to_vec()
}

fn latched(sim: &mut Sim) -> bool {
    sim.run_blocks(1);
    sim.latest_status().expect("Status").tracks[0].latched
}

/// The owner's case: "overdub next" pressed during the first recording, three passes played, then
/// switched off. Four layers, each holding exactly the input of its own pass - which proves both
/// "no gap" and "no sample twice" at every seam, the record -> overdub one included.
#[test]
fn three_latched_passes_after_the_recording_are_three_seamless_layers() {
    let mut sim = Sim::new(spec(), mono(fingerprint));
    record_loop(&mut sim);
    // Pressed halfway through the first take.
    sim.run_to(pass(0) + L / 2);
    sim.send(Command::LatchOverdub { track: 0, on: true });
    assert!(latched(&mut sim), "die Anzeige sieht \"Overdub next\"");

    // Three overdub passes, then switched off during the third: it still finishes.
    sim.run_to(pass(3) + L / 3);
    sim.send(Command::LatchOverdub { track: 0, on: false });
    run_past_pass(&mut sim, 3);
    sim.run_to(input_of(pass(5)) + 4 * BLOCK as u64);

    let t = sim.core.track(0);
    assert_eq!(t.layer_count(), 4, "Aufnahme + drei Durchlaeufe, kein vierter");
    assert!(t.take().is_none(), "nach dem Ausschalten laeuft nichts weiter");
    assert!(t.playing());
    for layer in 0..4u64 {
        let got = content(&sim, layer as usize);
        for (j, &v) in got.iter().enumerate() {
            assert_eq!(
                v,
                fingerprint(input_of(pass(layer)) + j as u64),
                "Ebene {layer}, Sample {j}"
            );
        }
    }
}

/// An impulse exactly on a loop boundary belongs to the pass that *begins* there: at index 0 of
/// that layer, and nowhere in the layer before it.
#[test]
fn an_impulse_on_the_boundary_lands_in_exactly_one_layer_at_index_zero() {
    let boundary = pass(2);
    // Steady quiet-but-played content so no pass counts as empty, plus one big spike.
    let played: Played = mono(move |k| if k == input_of(boundary) { 1.0 } else { 0.1 });
    let mut sim = Sim::new(spec(), played);
    record_loop(&mut sim);
    sim.send(Command::LatchOverdub { track: 0, on: true });
    sim.run_to(input_of(pass(2)) + 2 * BLOCK as u64);
    sim.send(Command::LatchOverdub { track: 0, on: false });
    run_past_pass(&mut sim, 2);

    assert_eq!(sim.core.track(0).layer_count(), 3);
    let spikes: Vec<(usize, usize)> = (0..3)
        .flat_map(|layer| {
            content(&sim, layer)
                .into_iter()
                .enumerate()
                .filter(|(_, v)| *v > 0.5)
                .map(move |(j, _)| (layer, j))
        })
        .collect();
    assert_eq!(spikes, vec![(2, 0)], "genau eine Ebene, genau Index 0");
}

/// A pass in which nothing was played is not kept; the passes around it are, at their places.
#[test]
fn an_empty_pass_is_dropped_and_the_next_one_takes_its_place() {
    let quiet = input_of(pass(2))..input_of(pass(3));
    let played: Played = mono(move |k| {
        if quiet.contains(&k) {
            // Interface noise well below the threshold.
            if k % 2 == 0 { 0.003 } else { -0.003 }
        } else {
            fingerprint(k)
        }
    });
    let mut sim = Sim::new(spec(), played);
    record_loop(&mut sim);
    sim.send(Command::LatchOverdub { track: 0, on: true });
    sim.run_to(input_of(pass(3)) + 2 * BLOCK as u64);
    sim.send(Command::LatchOverdub { track: 0, on: false });
    run_past_pass(&mut sim, 3);

    let t = sim.core.track(0);
    assert_eq!(t.layer_count(), 3, "Aufnahme, Durchlauf 1, Durchlauf 3");
    let third = content(&sim, 2);
    assert_eq!(third[0], fingerprint(input_of(pass(3))));
    assert_eq!(third[L as usize - 1], fingerprint(input_of(pass(3)) + L - 1));
}

/// The last pass, left running one loop too long and switched off, is judged as well.
#[test]
fn the_pass_left_running_too_long_is_dropped_too() {
    let played: Played = mono(move |k| if k < input_of(pass(2)) { fingerprint(k) } else { 0.0 });
    let mut sim = Sim::new(spec(), played);
    record_loop(&mut sim);
    sim.send(Command::LatchOverdub { track: 0, on: true });
    sim.run_to(pass(2) + L / 2);
    sim.send(Command::LatchOverdub { track: 0, on: false });
    run_past_pass(&mut sim, 2);
    assert_eq!(sim.core.track(0).layer_count(), 2);
}

/// One accent above the threshold keeps a pass otherwise silent.
#[test]
fn a_single_accent_keeps_the_pass() {
    let accent = input_of(pass(1)) + 1_234;
    let played: Played = mono(move |k| {
        if k < input_of(pass(1)) {
            fingerprint(k)
        } else if k == accent {
            0.05
        } else {
            0.0
        }
    });
    let mut sim = Sim::new(spec(), played);
    record_loop(&mut sim);
    sim.send(Command::LatchOverdub { track: 0, on: true });
    sim.run_to(pass(1) + L / 2);
    sim.send(Command::LatchOverdub { track: 0, on: false });
    run_past_pass(&mut sim, 1);
    assert_eq!(sim.core.track(0).layer_count(), 2);
    assert_eq!(content(&sim, 1)[1_234], 0.05);
}

/// At the ceiling the latch stops by itself, says why, and nothing half-made is left.
#[test]
fn the_layer_ceiling_ends_the_overdub_with_a_reason() {
    let mut sim = Sim::new(spec(), mono(fingerprint));
    record_loop(&mut sim);
    sim.send(Command::LatchOverdub { track: 0, on: true });
    run_past_pass(&mut sim, MAX_LAYERS as u64 + 2);

    let status = sim.latest_status().expect("Status");
    let t = sim.core.track(0);
    assert_eq!(t.layer_count(), MAX_LAYERS);
    assert!(t.take().is_none());
    assert!(!status.tracks[0].latched);
    assert_eq!(status.refusal, Refusal::OverdubFull);
    assert!(Refusal::OverdubFull.message().unwrap().contains("16 layers"));
    // The last layer is a complete, correct pass.
    let last = content(&sim, MAX_LAYERS - 1);
    assert_eq!(last[0], fingerprint(input_of(pass(MAX_LAYERS as u64 - 1))));
}

/// Ending with S or P: the latch goes off and the take is stopped on a bar line inside the pass.
/// The partial layer stays; nothing follows it.
#[test]
fn a_stop_inside_a_latched_pass_ends_it_there() {
    let mut sim = Sim::new(spec(), mono(fingerprint));
    record_loop(&mut sim);
    sim.send(Command::LatchOverdub { track: 0, on: true });
    sim.run_to(input_of(pass(2)) + 2 * BLOCK as u64);
    let cut = pass(2) + 2_000;
    sim.send(Command::LatchOverdub { track: 0, on: false });
    sim.send(Command::StopRecord { track: 0, at: cut });
    sim.send(Command::StartPlay { track: 0, at: cut });
    sim.run_to(input_of(pass(4)));

    let t = sim.core.track(0);
    assert_eq!(t.layer_count(), 3);
    assert!(t.take().is_none());
    let partial = content(&sim, 2);
    assert_eq!(partial[1_999], fingerprint(input_of(cut - 1)));
    assert_eq!(partial[2_000], 0.0, "nach dem Stopp wird nichts mehr geschrieben");
}

/// Clearing the track or stopping the transport switches the latch off.
#[test]
fn clear_and_transport_stop_switch_the_latch_off() {
    let mut sim = Sim::new(spec(), mono(fingerprint));
    record_loop(&mut sim);
    sim.send(Command::LatchOverdub { track: 0, on: true });
    sim.run_to(pass(1) + 100);
    sim.send(Command::TransportStop);
    assert!(!latched(&mut sim));
    sim.run_to(input_of(pass(3)));
    assert!(sim.core.track(0).take().is_none());

    sim.send(Command::LatchOverdub { track: 0, on: true });
    assert!(latched(&mut sim));
    let at = sim.pos() + 1_000;
    sim.send(Command::ClearTrack { track: 0, at });
    sim.run_to(at + BLOCK as u64);
    assert!(!latched(&mut sim));
}

/// Without the latch an overdub is what it always was: one pass, kept even when silent, followed
/// by nothing.
#[test]
fn the_one_pass_overdub_is_unchanged() {
    let played: Played = mono(move |k| if k < input_of(pass(1)) { fingerprint(k) } else { 0.0 });
    let mut sim = Sim::new(spec(), played);
    record_loop(&mut sim);
    sim.send(Command::StartOverdub {
        track: 0,
        at: pass(1),
    });
    sim.send(Command::StopRecord {
        track: 0,
        at: pass(2),
    });
    run_past_pass(&mut sim, 3);
    let t = sim.core.track(0);
    assert_eq!(t.layer_count(), 2, "die stille Ebene bleibt, es folgt keine weitere");
    assert!(t.take().is_none());
}

/// Overdub pressed on an empty track with the latch on: the first take defines the loop and the
/// overdub carries straight on - "record and keep layering" with one press.
#[test]
fn a_latched_overdub_on_an_empty_track_records_first_and_then_layers() {
    let mut sim = Sim::new(spec(), mono(fingerprint));
    sim.send(Command::LatchOverdub { track: 0, on: true });
    sim.send(Command::StartOverdub {
        track: 0,
        at: START,
    });
    sim.send(Command::StopRecord {
        track: 0,
        at: START + L,
    });
    sim.run_to(input_of(pass(2)) + 2 * BLOCK as u64);
    sim.send(Command::LatchOverdub { track: 0, on: false });
    run_past_pass(&mut sim, 2);
    let t = sim.core.track(0);
    assert_eq!(t.loop_len(), L);
    assert_eq!(t.layer_count(), 3);
    assert_eq!(content(&sim, 2)[0], fingerprint(input_of(pass(2))));
}
