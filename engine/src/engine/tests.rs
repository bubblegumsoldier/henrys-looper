//! Offline proof of sample accuracy. Nothing here opens an audio device.

use super::command::Command;
use super::metro::Metronome;
use super::sim::{Sim, SimSpec};
use super::timeline::{TimeSignature, Timeline};
use super::track::TrackState;

const RATE: u32 = 48_000;
const BLOCK: usize = 128;
/// Roundtrip measured in phase 0: 827 samples at 128 frames / 48 kHz, stddev 0 over 60 runs.
const R: u64 = 827;

fn silence() -> Box<dyn Fn(u64) -> f32> {
    Box::new(|_| 0.0)
}

/// A signal whose value identifies the sample it came from, bounded well inside the headroom so
/// the output clamp cannot alter it.
fn fingerprint(k: u64) -> f32 {
    ((k % 65_536) as f32) / 65_536.0 - 0.5
}

fn spec_4_4() -> SimSpec {
    SimSpec {
        bpm: 100.0,
        signature: TimeSignature::new(4, 4),
        bars: 8,
        latency: R,
        roundtrip: R,
        block: BLOCK,
        ..Default::default()
    }
}

// -------------------------------------------------------------------------------------------
// 1. A command timed for the middle of a buffer acts at that sample, not at a buffer edge
// -------------------------------------------------------------------------------------------

#[test]
fn start_record_acts_exactly_at_its_sample() {
    // Neither the command position nor the input sample it selects lands on a buffer boundary.
    let start = 5_000u64;
    let end = start + 4_000;
    assert_ne!(start as usize % BLOCK, 0, "Testaufbau: Start mitten im Puffer");
    assert_ne!(
        (start + R) as usize % BLOCK,
        0,
        "Testaufbau: auch das gesuchte Eingangssample liegt mitten im Puffer"
    );

    let mut sim = Sim::new(spec_4_4(), Box::new(fingerprint));
    sim.send(Command::StartRecord { at: start });
    sim.send(Command::StopRecord { at: end });
    sim.run_to(end + R + 4 * BLOCK as u64);

    let content = sim.core.track().content();
    assert_eq!(content.len(), 4_000, "Loop-Laenge");
    assert_eq!(
        content[0],
        fingerprint(start + R),
        "erstes aufgenommenes Sample"
    );
    assert_ne!(
        content[0],
        fingerprint(start + R - 1),
        "das Sample davor darf nicht im Loop stehen"
    );
    for (i, &got) in content.iter().enumerate() {
        assert_eq!(got, fingerprint(start + R + i as u64), "Sample {i}");
    }
}

#[test]
fn start_play_acts_exactly_at_its_sample() {
    let start = 4_096u64;
    let end = start + 20_000;
    let mut sim = Sim::new(spec_4_4(), Box::new(fingerprint));
    sim.send(Command::StartRecord { at: start });
    sim.send(Command::StopRecord { at: end });
    sim.run_to(end + R + 4 * BLOCK as u64);

    // Deliberately not on a buffer boundary.
    let play_at = end + R + 10_000 + 37;
    assert_ne!(play_at as usize % BLOCK, 0);
    sim.send(Command::StartPlay { at: play_at });
    sim.run_to(play_at + 4 * BLOCK as u64);

    let history = &sim.out_history;
    assert_eq!(
        history[(play_at - 1) as usize],
        0.0,
        "vor dem Startsample muss Stille sein"
    );
    let loop_len = sim.core.track().loop_len();
    let expected = sim.core.track().content()[((play_at - start) % loop_len) as usize];
    assert_ne!(expected, 0.0, "Testaufbau: erwartetes Sample ist nicht still");
    assert_eq!(
        history[play_at as usize], expected,
        "Wiedergabe startet exakt am geforderten Sample"
    );
}

// -------------------------------------------------------------------------------------------
// 2. Loop length
// -------------------------------------------------------------------------------------------

#[test]
fn eight_bars_give_exactly_the_expected_sample_count() {
    for (bpm, sig) in [
        (100.0, TimeSignature::new(4, 4)),
        (100.0, TimeSignature::new(3, 4)),
        (137.0, TimeSignature::new(7, 8)),
    ] {
        let timeline = Timeline::new(RATE, bpm, sig);
        let start = timeline.bar_start(4);
        let end = timeline.bar_start(12);
        let expected = end - start;

        let spec = SimSpec {
            bpm,
            signature: sig,
            bars: 8,
            latency: R,
            roundtrip: R,
            block: BLOCK,
            ..Default::default()
        };
        let mut sim = Sim::new(spec, Box::new(fingerprint));
        sim.send(Command::StartRecord { at: start });
        sim.send(Command::StopRecord { at: end });
        sim.run_to(end + R + 4 * BLOCK as u64);

        assert_eq!(
            sim.core.track().loop_len(),
            expected,
            "{bpm} BPM, {}/{}",
            sig.beats_per_bar,
            sig.beat_unit
        );
        // And the sample count really is eight bars' worth on this grid.
        assert_eq!(expected, timeline.span_bars(4, 8));
    }

    // The plain 4/4 case can be checked against a hand-computed number: 100 BPM at 48 kHz is
    // 28800 samples per quarter, 115200 per bar, 921600 for eight bars.
    let t = Timeline::new(RATE, 100.0, TimeSignature::new(4, 4));
    assert_eq!(t.span_bars(4, 8), 921_600);
}

// -------------------------------------------------------------------------------------------
// 3. Latency compensation - the core test
// -------------------------------------------------------------------------------------------

/// The musician plays an impulse on every beat *as he hears it*, i.e. the impulse reaches the
/// input R samples after the beat it belongs to. The impulses must end up exactly on the beat
/// grid inside the loop buffer. Tolerance: zero samples.
#[test]
fn latency_compensation_puts_impulses_exactly_on_the_beat() {
    let timeline = Timeline::new(RATE, 100.0, TimeSignature::new(4, 4));
    let start = timeline.bar_start(4);
    let end = timeline.bar_start(12);

    let played = Box::new(move |k: u64| {
        if k < R {
            return 0.0;
        }
        let musical = k - R;
        let beat = timeline.beat_index_at(musical);
        if timeline.beat_start(beat) == musical { 1.0 } else { 0.0 }
    });

    let mut sim = Sim::new(spec_4_4(), played);
    sim.send(Command::StartRecord { at: start });
    sim.send(Command::StopRecord { at: end });
    sim.run_to(end + R + 4 * BLOCK as u64);

    let content = sim.core.track().content();
    assert_eq!(content.len() as u64, end - start);

    let mut impulses = 0usize;
    for (i, &got) in content.iter().enumerate() {
        let musical = start + i as u64;
        let beat = timeline.beat_index_at(musical);
        let on_beat = timeline.beat_start(beat) == musical;
        if on_beat {
            impulses += 1;
            assert_eq!(got, 1.0, "Impuls fehlt auf Schlaggrenze bei Loop-Index {i}");
        } else {
            assert_eq!(got, 0.0, "Impuls an falscher Stelle: Loop-Index {i}");
        }
    }
    assert_eq!(impulses, 8 * 4, "acht Takte zu vier Schlaegen");
}

/// The same thing without any injected signal: the engine's own click travels through the
/// simulated cable and is recorded. The recorded loop has to be bit-identical to the click
/// rendered directly at those positions.
#[test]
fn recorded_click_loopback_lands_bit_identical_on_the_grid() {
    let timeline = Timeline::new(RATE, 137.0, TimeSignature::new(7, 8));
    let metro = Metronome::new(RATE);
    let start = timeline.bar_start(4);
    let end = timeline.bar_start(12);

    let spec = SimSpec {
        bpm: 137.0,
        signature: TimeSignature::new(7, 8),
        bars: 8,
        latency: R,
        roundtrip: R,
        block: BLOCK,
        click: true,
        ..Default::default()
    };
    let mut sim = Sim::new(spec, silence());
    sim.send(Command::StartRecord { at: start });
    sim.send(Command::StopRecord { at: end });
    sim.run_to(end + R + 4 * BLOCK as u64);

    let content = sim.core.track().content();
    assert_eq!(content.len() as u64, end - start);
    for (i, &got) in content.iter().enumerate() {
        let expected = metro.sample_at(&timeline, start + i as u64);
        assert_eq!(got, expected, "Loop-Index {i}");
    }

    // The test discriminates: shifting the comparison by the roundtrip - which is what a wrong
    // sign would produce - must fail loudly.
    let mut shifted_mismatches = 0usize;
    for (i, &got) in content.iter().enumerate() {
        if got != metro.sample_at(&timeline, start + i as u64 + 2 * R) {
            shifted_mismatches += 1;
        }
    }
    assert!(
        shifted_mismatches > 1_000,
        "Der Test wuerde ein falsches Vorzeichen nicht bemerken"
    );
}

/// A compensation value that does not match the hardware misaligns the recording by exactly the
/// difference - the counter-example that makes the test above meaningful.
#[test]
fn wrong_compensation_value_shifts_the_recording() {
    let timeline = Timeline::new(RATE, 100.0, TimeSignature::new(4, 4));
    let start = timeline.bar_start(4);
    let end = start + 10_000;

    let spec = SimSpec {
        latency: 0, // engine believes there is no latency ...
        roundtrip: R, // ... but the cable still delays by 827 samples
        ..spec_4_4()
    };
    let mut sim = Sim::new(spec, Box::new(fingerprint));
    sim.send(Command::StartRecord { at: start });
    sim.send(Command::StopRecord { at: end });
    sim.run_to(end + 2 * R + 4 * BLOCK as u64);

    let content = sim.core.track().content();
    // Without compensation the loop holds what arrived, i.e. the material of position start,
    // shifted by the full roundtrip.
    assert_eq!(content[0], fingerprint(start));
    assert_ne!(content[0], fingerprint(start + R));
}

// -------------------------------------------------------------------------------------------
// 4. The seam
// -------------------------------------------------------------------------------------------

/// Record a strictly rising ramp, play it three times and check every single sample of the
/// output: no sample may be swallowed, repeated or reordered at the loop wrap.
#[test]
fn loop_seam_loses_and_repeats_nothing() {
    let timeline = Timeline::new(RATE, 100.0, TimeSignature::new(4, 4));
    let start = timeline.bar_start(2);
    let end = timeline.bar_start(6);
    let len = end - start;

    // Ramp indexed by loop position: the musician's signal arrives R samples late, so his sample
    // for musical position m has to be produced at input index m + R.
    let played = Box::new(move |k: u64| {
        if k < R {
            return 0.0;
        }
        let musical = k - R;
        if musical < start || musical >= end {
            return 0.0;
        }
        (musical - start) as f32 / len as f32 - 0.5
    });

    let spec = SimSpec {
        bars: 4,
        ..spec_4_4()
    };
    let mut sim = Sim::new(spec, played);
    sim.send(Command::StartRecord { at: start });
    sim.send(Command::StopRecord { at: end });
    sim.send(Command::StartPlay { at: end });
    sim.run_to(end + 3 * len + BLOCK as u64);

    assert_eq!(sim.core.track().loop_len(), len);
    let content: Vec<f32> = sim.core.track().content().to_vec();
    assert_eq!(content.len() as u64, len);

    let history = &sim.out_history;
    for j in 0..3 * len {
        let got = history[(end + j) as usize];
        let want = content[(j % len) as usize];
        assert_eq!(got, want, "Ausgabe bei Loop-Durchlauf {} Index {}", j / len, j % len);
    }
    // Independent of the buffer contents: the signal rises everywhere except at the seam, and the
    // seam occurs exactly every `len` samples. A swallowed or doubled sample breaks one of the two.
    for j in 1..3 * len {
        let prev = history[(end + j - 1) as usize];
        let got = history[(end + j) as usize];
        if j % len == 0 {
            assert!(got < prev, "Nahtstelle nicht bei Index {j}");
        } else {
            assert!(got > prev, "Sample verschluckt oder doppelt bei Index {j}");
        }
    }
}

// -------------------------------------------------------------------------------------------
// 5. State, status and the rest of the command set
// -------------------------------------------------------------------------------------------

#[test]
fn status_reports_bar_beat_and_track_state() {
    let timeline = Timeline::new(RATE, 100.0, TimeSignature::new(4, 4));
    let start = timeline.bar_start(2);
    let end = timeline.bar_start(4);

    let mut sim = Sim::new(spec_4_4(), Box::new(fingerprint));
    sim.run_to(BLOCK as u64 * 4);
    let s = sim.latest_status().expect("Status kommt an");
    assert_eq!(s.track, TrackState::Empty);
    assert_eq!(s.bpm, 100.0);
    assert_eq!(s.samples_per_beat, 28_800.0);

    sim.send(Command::StartRecord { at: start });
    sim.run_to(start + R / 2);
    let s = sim.latest_status().expect("Status");
    assert_eq!(s.track, TrackState::Armed, "geplant, aber Eingang noch nicht da");

    sim.run_to(start + R + 4 * BLOCK as u64);
    let s = sim.latest_status().expect("Status");
    assert_eq!(s.track, TrackState::Recording);
    assert!(s.filled > 0);

    sim.send(Command::StopRecord { at: end });
    sim.send(Command::StartPlay { at: end });
    sim.run_to(end + R + 4 * BLOCK as u64);
    let s = sim.latest_status().expect("Status");
    assert_eq!(s.track, TrackState::Playing);
    assert_eq!(s.loop_len, end - start);
    let here = timeline.locate(s.pos);
    assert_eq!((s.bar, s.beat, s.beat_offset), (here.bar, here.beat, here.offset));
    assert!(s.input_peak > 0.0);
}

#[test]
fn clear_stop_and_monitor_do_what_they_say() {
    let timeline = Timeline::new(RATE, 100.0, TimeSignature::new(4, 4));
    let start = timeline.bar_start(1);
    let end = timeline.bar_start(3);

    let spec = SimSpec {
        monitor: true,
        ..spec_4_4()
    };
    let mut sim = Sim::new(spec, Box::new(fingerprint));
    sim.run_to(4 * BLOCK as u64);
    // Monitoring passes the consumed input straight through.
    assert_eq!(sim.out_history[BLOCK], fingerprint(BLOCK as u64));

    sim.send(Command::SetMonitor { on: false });
    sim.run_to(8 * BLOCK as u64);
    assert_eq!(sim.out_history[7 * BLOCK], 0.0);

    sim.send(Command::StartRecord { at: start });
    sim.send(Command::StopRecord { at: end });
    sim.send(Command::StartPlay { at: end });
    sim.run_to(end + R + 4 * BLOCK as u64);
    assert!(sim.core.track().loop_len() > 0);

    let clear_at = sim.pos() + BLOCK as u64;
    sim.send(Command::ClearTrack { at: clear_at });
    sim.run_to(clear_at + 4 * BLOCK as u64);
    assert_eq!(sim.core.track().loop_len(), 0);
    let s = sim.latest_status().expect("Status");
    assert_eq!(s.track, TrackState::Empty);
    assert!(!s.stopped);

    sim.send(Command::Stop);
    sim.run_to(sim.pos() + 4 * BLOCK as u64);
    let s = sim.latest_status().expect("Status");
    assert!(s.stopped);
}

#[test]
fn tempo_change_is_refused_while_the_track_is_busy() {
    let timeline = Timeline::new(RATE, 100.0, TimeSignature::new(4, 4));
    let start = timeline.bar_start(1);
    let end = timeline.bar_start(3);

    let mut sim = Sim::new(spec_4_4(), Box::new(fingerprint));
    sim.send(Command::SetTempo {
        bpm: 120.0,
        signature: TimeSignature::new(3, 4),
    });
    sim.run_to(4 * BLOCK as u64);
    let s = sim.latest_status().expect("Status");
    assert_eq!(s.bpm, 120.0, "leerer Track: Tempowechsel wird uebernommen");
    assert_eq!(s.ignored_commands, 0);

    sim.send(Command::StartRecord { at: start });
    sim.send(Command::StopRecord { at: end });
    sim.run_to(end + R + 4 * BLOCK as u64);
    sim.send(Command::SetTempo {
        bpm: 90.0,
        signature: TimeSignature::new(4, 4),
    });
    sim.run_to(sim.pos() + 4 * BLOCK as u64);
    let s = sim.latest_status().expect("Status");
    assert_eq!(s.bpm, 120.0, "belegter Track: Tempo bleibt");
    assert_eq!(s.ignored_commands, 1);
}

#[test]
fn a_new_buffer_from_the_control_thread_replaces_the_old_one() {
    let mut sim = Sim::new(spec_4_4(), Box::new(fingerprint));
    sim.run_to(4 * BLOCK as u64);
    let before = sim.core.track().capacity();

    sim.install_buffer(4_096);
    sim.run_to(sim.pos() + 2 * BLOCK as u64);
    assert_ne!(sim.core.track().capacity(), before);
    assert_eq!(sim.core.track().capacity(), 4_096);
    sim.drain_retired();

    // The smaller buffer really bounds the take: recording without a stop closes at capacity.
    let start = sim.pos() + BLOCK as u64;
    sim.send(Command::StartRecord { at: start });
    sim.run_to(start + R + 8_192);
    assert_eq!(sim.core.track().loop_len(), 4_096);
}

/// Commands sent far ahead of time must not act early, and a stop stamped for a position that has
/// already gone by truncates the take there instead of being lost.
#[test]
fn early_and_late_commands_behave() {
    let timeline = Timeline::new(RATE, 100.0, TimeSignature::new(4, 4));
    let far = timeline.bar_start(6);

    let mut sim = Sim::new(spec_4_4(), Box::new(fingerprint));
    // Sent immediately, due six bars later.
    sim.send(Command::StartRecord { at: far });
    sim.run_to(far - BLOCK as u64);
    assert_eq!(sim.core.track().filled(), 0, "darf nicht zu frueh anfangen");

    sim.run_to(far + R + 4 * BLOCK as u64);
    assert!(sim.core.track().filled() > 0, "muss dann aber anfangen");

    // A stop stamped for a position that is already behind the write pointer: the take is cut
    // there, it is not ignored and it does not run on forever.
    let late = far + 300;
    assert!(sim.core.track().filled() > 300);
    sim.send(Command::StopRecord { at: late });
    sim.run_to(sim.pos() + 4 * BLOCK as u64);
    assert_eq!(sim.core.track().loop_len(), 300, "spaetes Stopp-Kommando greift");
}
