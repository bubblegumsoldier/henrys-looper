//! What actually leaves the device: buses that share a socket, the safety limiter on every device
//! channel, and the meters that report the sum on the socket rather than a bus.
//!
//! The question behind these tests came from a real evening: the headphones distorted while the
//! loops ran, the recorded files were clean. The recording is the Main bus before its volume and
//! without the click; the socket is everything that lands on it.

use super::bus::{BusOutput, BusRouting, LIMIT_THRESHOLD, OutputLimiter};
use super::command::{Command, Status};
use super::metro::Metronome;
use super::sim::{Sim, SimSpec, mono};
use super::timeline::{TimeSignature, Timeline};

const RATE: u32 = 48_000;
const BLOCK: usize = 128;
const R: u64 = 827;

/// -3 dBFS, the level the loop is recorded at in the first test.
const MINUS_3_DB: f32 = 0.707_945_8;
/// -1.4 dBFS, the peak of the mix that was recorded on the evening in question.
const MINUS_1_4_DB: f32 = 0.851_138;

const START: u64 = 4_096;
const LEN: u64 = 48_000;
const PLAY_AT: u64 = START + LEN + R + 8_000;

fn sine(k: u64) -> f32 {
    MINUS_3_DB * (std::f32::consts::TAU * 220.0 * (k % 48_000) as f32 / RATE as f32).sin()
}

/// A square wave at -1.4 dBFS: as loud as that mix, and loud at every instant, so the click is
/// guaranteed to land on top of a peak rather than between two.
fn square(k: u64) -> f32 {
    if (k / 480) % 2 == 0 {
        MINUS_1_4_DB
    } else {
        -MINUS_1_4_DB
    }
}

/// Quiet enough that loop plus click stays below the limiter threshold everywhere.
fn quiet(k: u64) -> f32 {
    0.25 * (std::f32::consts::TAU * 330.0 * (k % 48_000) as f32 / RATE as f32).sin()
}

fn spec() -> SimSpec {
    SimSpec {
        bpm: 100.0,
        signature: TimeSignature::new(4, 4),
        bars: 8,
        latency: R,
        roundtrip: R,
        block: BLOCK,
        loopback_channel: None,
        ..Default::default()
    }
}

/// Record [`LEN`] frames on track 0 and start playing them at [`PLAY_AT`]; returns the loop as it
/// is heard - the content with its seam crossfade (`Track::play_frame`).
fn record_and_play(sim: &mut Sim) -> Vec<f32> {
    sim.send(Command::StartRecord {
        track: 0,
        at: START,
    });
    sim.send(Command::StopRecord {
        track: 0,
        at: START + LEN,
    });
    sim.run_to(START + LEN + R + 4 * BLOCK as u64);
    sim.send(Command::StartPlay {
        track: 0,
        at: PLAY_AT,
    });
    let t = sim.core.track(0);
    (0..t.loop_len()).map(|i| t.steady_layer(0, i).l).collect()
}

fn loop_at(content: &[f32], pos: u64) -> f32 {
    content[((pos - START) % LEN) as usize]
}

/// Run one second of playback block by block and keep the loudest value every meter reported.
fn run_and_meter(sim: &mut Sim) -> Status {
    sim.run_to(PLAY_AT);
    let mut held: Option<Status> = None;
    while sim.pos() < PLAY_AT + 48_000 {
        sim.run_blocks(1);
        let s = sim.latest_status().expect("Status");
        held = Some(match held {
            Some(earlier) => {
                let mut s = s;
                s.hold_meters(&earlier);
                s
            }
            None => s,
        });
    }
    sim.run_blocks(2);
    held.expect("Status")
}

fn peak(values: &[f32]) -> f32 {
    values.iter().fold(0.0f32, |m, v| m.max(v.abs()))
}

fn window(values: &[f32]) -> &[f32] {
    &values[PLAY_AT as usize..(PLAY_AT + 48_000) as usize]
}

/// Step 1 of the investigation, kept as a test. On a two-output device both buses share the pair,
/// and a loop assigned to Main *and* Monitor at -3 dBFS comes out at -3 dBFS - once, not 6 dB
/// louder. What does push the socket over full scale is the click on top of the loop, which the
/// recording never contains: a -1.4 dBFS mix plus the click at its default level reaches +1.8
/// dBFS. Before the output stage existed that was clamped hard to 1.0 - digital clipping on every
/// beat - and no meter could say by how much, because the bus meter measured after the clamp.
#[test]
fn on_two_outputs_a_loop_on_both_buses_is_heard_once_and_the_click_is_what_overloads() {
    let mut sim = Sim::new(spec(), mono(sine));
    assert!(sim.routing().collapsed(), "zwei Ausgaenge: beide Busse auf 1-2");
    record_and_play(&mut sim);
    run_and_meter(&mut sim);
    let dev = peak(window(&sim.dev[0]));
    assert!(
        (dev - MINUS_3_DB).abs() < 1e-3,
        "einmal, nicht doppelt: {dev} = {:.2} dBFS",
        20.0 * dev.log10()
    );

    let mut sim = Sim::new(
        SimSpec {
            click: true,
            ..spec()
        },
        mono(square),
    );
    let content = record_and_play(&mut sim);
    let status = run_and_meter(&mut sim);
    let timeline = Timeline::new(RATE, 100.0, TimeSignature::new(4, 4));
    let metro = Metronome::new(RATE);
    let sum = (PLAY_AT..PLAY_AT + 48_000)
        .map(|p| (loop_at(&content, p) + metro.sample_at(&timeline, p)).abs())
        .fold(0.0f32, f32::max);
    assert!(sum > 1.2, "Loop -1,4 dBFS plus Klick: {:.2} dBFS", 20.0 * sum.log10());

    // The meter at the socket says so, and says how much the limiter had to take off.
    assert!(
        (status.out_peak[0] - sum).abs() < 1e-6,
        "Ausgangsmeter {} statt {sum}",
        status.out_peak[0]
    );
    assert!(status.out_reduction_db[0] < -1.5, "{}", status.out_reduction_db[0]);
    // The Monitor bus meter sees nothing: with one mix, everything is on Main.
    assert_eq!(status.bus_peak[1], [0.0, 0.0]);
    // And what leaves is never above full scale.
    assert!(peak(window(&sim.dev[0])) <= 1.0);
    assert!(peak(window(&sim.dev[1])) <= 1.0);
}

/// Shared buses are one mix: what a two-output device puts on its pair is exactly what the Monitor
/// pair of a four-output device gets - loop and click, each once.
#[test]
fn shared_buses_carry_the_loop_and_the_click_exactly_once() {
    let timeline = Timeline::new(RATE, 100.0, TimeSignature::new(4, 4));
    let metro = Metronome::new(RATE);
    let mut two = Sim::new(
        SimSpec {
            click: true,
            ..spec()
        },
        mono(quiet),
    );
    let mut four = Sim::new(
        SimSpec {
            click: true,
            output_channels: 4,
            ..spec()
        },
        mono(quiet),
    );
    let content = record_and_play(&mut two);
    record_and_play(&mut four);
    run_and_meter(&mut two);
    run_and_meter(&mut four);
    let mut both = 0;
    for pos in PLAY_AT..PLAY_AT + 48_000 {
        let l = loop_at(&content, pos);
        let c = metro.sample_at(&timeline, pos);
        assert!((l + c).abs() < LIMIT_THRESHOLD, "Testaufbau: unter der Schwelle");
        for ch in 0..2 {
            assert_eq!(two.device(ch, pos), l + c, "2 Ausgaenge, Kanal {ch}, Position {pos}");
            assert_eq!(four.device(ch, pos), l, "4 Ausgaenge Main, Position {pos}");
            assert_eq!(four.device(ch + 2, pos), l + c, "4 Ausgaenge Monitor, Position {pos}");
        }
        if l != 0.0 && c != 0.0 {
            both += 1;
        }
    }
    assert!(both > 100, "Testaufbau: Loop und Klick klingen gleichzeitig");
}

/// A partial overlap, checked per channel: Main on 1-2, Monitor on 2-3. Channel 2 belongs to both
/// and carries the one mix once; channel 3 belongs to Monitor alone and carries it as well.
#[test]
fn a_partial_overlap_is_one_mix_on_every_channel_and_nothing_twice() {
    let timeline = Timeline::new(RATE, 100.0, TimeSignature::new(4, 4));
    let metro = Metronome::new(RATE);
    let mut sim = Sim::new(
        SimSpec {
            click: true,
            output_channels: 3,
            routing: Some(BusRouting::new(BusOutput::pair(0), BusOutput::pair(1))),
            ..spec()
        },
        mono(quiet),
    );
    assert!(sim.routing().collapsed() && !sim.routing().identical());
    let content = record_and_play(&mut sim);
    run_and_meter(&mut sim);
    for pos in PLAY_AT..PLAY_AT + 48_000 {
        let want = loop_at(&content, pos) + metro.sample_at(&timeline, pos);
        for ch in 0..3 {
            assert_eq!(sim.device(ch, pos), want, "Kanal {} bei {pos}", ch + 1);
        }
    }
}

/// Separate buses are untouched by all of this: each bus on its own pair, at its own volume, and
/// the output meter of each channel is simply that bus.
#[test]
fn separate_buses_keep_their_own_volumes_and_their_own_meters() {
    let timeline = Timeline::new(RATE, 100.0, TimeSignature::new(4, 4));
    let metro = Metronome::new(RATE);
    let mut sim = Sim::new(
        SimSpec {
            click: true,
            output_channels: 4,
            bus_gain: [0.5, 0.25],
            ..spec()
        },
        mono(quiet),
    );
    assert!(!sim.routing().collapsed());
    let content = record_and_play(&mut sim);
    let status = run_and_meter(&mut sim);
    for pos in PLAY_AT..PLAY_AT + 48_000 {
        let l = loop_at(&content, pos);
        let c = metro.sample_at(&timeline, pos);
        assert_eq!(sim.device(0, pos), l * 0.5);
        assert_eq!(sim.device(2, pos), (l + c) * 0.25);
    }
    assert_eq!(status.out_peak[0], status.bus_peak[0][0]);
    assert_eq!(status.out_peak[2], status.bus_peak[1][0]);
    assert_eq!(status.out_reduction_db, [0.0; 8], "nichts begrenzt");
}

/// The limiter, end to end: until the first over the socket gets the mix bit for bit; from then on
/// it is exactly the limiter's answer to the sum, never above full scale, and the overs are turned
/// down rather than cut off - after the attack the peaks sit at the threshold with the waveform
/// intact, instead of a run of samples pinned at 1.0.
#[test]
fn the_limiter_is_bit_identical_below_and_rides_the_gain_above() {
    let mut sim = Sim::new(
        SimSpec {
            click: true,
            ..spec()
        },
        mono(square),
    );
    record_and_play(&mut sim);
    run_and_meter(&mut sim);
    let end = sim.dev[0].len();
    let mut reference = OutputLimiter::new(RATE, 2);
    let (mut untouched, mut over, mut clipped) = (0usize, 0usize, 0usize);
    let mut seen_over = false;
    for pos in 0..end {
        for ch in 0..2 {
            let raw = sim.raw[ch][pos];
            let got = sim.dev[ch][pos];
            assert_eq!(got.to_bits(), reference.process(ch, raw).to_bits(), "Kanal {ch} bei {pos}");
            assert!(got.abs() <= 1.0);
            if raw.abs() > LIMIT_THRESHOLD {
                seen_over = true;
                over += 1;
                if got.abs() > LIMIT_THRESHOLD + 1e-4 {
                    clipped += 1;
                }
            } else if !seen_over {
                assert_eq!(got.to_bits(), raw.to_bits(), "bitgleich vor dem ersten Ueberschuss");
                untouched += 1;
            }
        }
    }
    assert!(untouched > 100_000 && over > 100, "Testaufbau: {untouched} / {over}");
    // A clamp would have pinned every one of the `over` samples; the limiter leaves only the few in
    // its first half millisecond to the soft clipper.
    assert!(
        clipped * 4 < over,
        "{clipped} von {over} Ueberschuss-Samples noch im Soft-Clipper"
    );
}

/// The recording is taken before the output stage: a mix that overloads the socket is recorded as
/// it is, unlimited, and the bus volume does not reach it either.
#[test]
fn the_output_stage_never_reaches_the_recording() {
    let mut sim = Sim::new(
        SimSpec {
            click: true,
            bus_gain: [2.0, 1.0],
            ..spec()
        },
        mono(square),
    );
    record_and_play(&mut sim);
    run_and_meter(&mut sim);
    let t = sim.core.track(0);
    let content = t.layer(0).expect("Ebene").content(t.loop_len()).to_vec();
    assert_eq!(peak(&content), MINUS_1_4_DB, "die Ebene ist der Eingang, unbegrenzt");
    // The Main bus itself is not clamped any more either: its meter shows the true +7.5 dBFS.
    let main = peak(window(&sim.buses[0][0]));
    assert!(main > 2.0, "{main}");
    assert!(peak(window(&sim.dev[0])) <= 1.0);
}
