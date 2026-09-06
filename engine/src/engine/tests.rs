//! Offline proof of sample accuracy. Nothing here opens an audio device.

use super::command::Command;
use super::metro::Metronome;
use super::process::is_fresh;
use super::sim::{Sim, SimSpec, TrackSpec, mono, silence};
use super::timeline::{TimeSignature, Timeline};
use super::track::TrackState;

const RATE: u32 = 48_000;
const BLOCK: usize = 128;
/// Roundtrip measured in phase 0: 827 samples at 128 frames / 48 kHz, stddev 0 over 60 runs.
const R: u64 = 827;

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

/// The recorded content of one layer, as long as the loop is.
fn layer_content(sim: &Sim, track: usize, layer: usize) -> Vec<f32> {
    let t = sim.core.track(track);
    t.layer(layer)
        .expect("Ebene existiert")
        .content(t.loop_len())
        .to_vec()
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

    let mut sim = Sim::new(spec_4_4(), mono(fingerprint));
    sim.send(Command::StartRecord { track: 0, at: start });
    sim.send(Command::StopRecord { track: 0, at: end });
    sim.run_to(end + R + 4 * BLOCK as u64);

    let content = layer_content(&sim, 0, 0);
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
    let mut sim = Sim::new(spec_4_4(), mono(fingerprint));
    sim.send(Command::StartRecord { track: 0, at: start });
    sim.send(Command::StopRecord { track: 0, at: end });
    sim.run_to(end + R + 4 * BLOCK as u64);

    // Deliberately not on a buffer boundary.
    let play_at = end + R + 10_000 + 37;
    assert_ne!(play_at as usize % BLOCK, 0);
    sim.send(Command::StartPlay {
        track: 0,
        at: play_at,
    });
    sim.run_to(play_at + 4 * BLOCK as u64);

    assert_eq!(
        sim.out_history[(play_at - 1) as usize],
        0.0,
        "vor dem Startsample muss Stille sein"
    );
    let loop_len = sim.core.track(0).loop_len();
    let content = layer_content(&sim, 0, 0);
    let expected = content[((play_at - start) % loop_len) as usize];
    assert_ne!(expected, 0.0, "Testaufbau: erwartetes Sample ist nicht still");
    assert_eq!(
        sim.out_history[play_at as usize], expected,
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
        let mut sim = Sim::new(spec, mono(fingerprint));
        sim.send(Command::StartRecord { track: 0, at: start });
        sim.send(Command::StopRecord { track: 0, at: end });
        sim.run_to(end + R + 4 * BLOCK as u64);

        assert_eq!(
            sim.core.track(0).loop_len(),
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

    let played = mono(move |k: u64| {
        if k < R {
            return 0.0;
        }
        let musical = k - R;
        let beat = timeline.beat_index_at(musical);
        if timeline.beat_start(beat) == musical { 1.0 } else { 0.0 }
    });

    let mut sim = Sim::new(spec_4_4(), played);
    sim.send(Command::StartRecord { track: 0, at: start });
    sim.send(Command::StopRecord { track: 0, at: end });
    sim.run_to(end + R + 4 * BLOCK as u64);

    let content = layer_content(&sim, 0, 0);
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
    sim.send(Command::StartRecord { track: 0, at: start });
    sim.send(Command::StopRecord { track: 0, at: end });
    sim.run_to(end + R + 4 * BLOCK as u64);

    let content = layer_content(&sim, 0, 0);
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
        latency: 0,   // engine believes there is no latency ...
        roundtrip: R, // ... but the cable still delays by 827 samples
        ..spec_4_4()
    };
    let mut sim = Sim::new(spec, mono(fingerprint));
    sim.send(Command::StartRecord { track: 0, at: start });
    sim.send(Command::StopRecord { track: 0, at: end });
    sim.run_to(end + 2 * R + 4 * BLOCK as u64);

    let content = layer_content(&sim, 0, 0);
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
    let played = mono(move |k: u64| {
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
    sim.send(Command::StartRecord { track: 0, at: start });
    sim.send(Command::StopRecord { track: 0, at: end });
    sim.send(Command::StartPlay { track: 0, at: end });
    sim.run_to(end + 3 * len + BLOCK as u64);

    assert_eq!(sim.core.track(0).loop_len(), len);
    let content = layer_content(&sim, 0, 0);
    assert_eq!(content.len() as u64, len);

    let history = &sim.out_history;
    for j in 0..3 * len {
        let got = history[(end + j) as usize];
        let want = content[(j % len) as usize];
        assert_eq!(
            got,
            want,
            "Ausgabe bei Loop-Durchlauf {} Index {}",
            j / len,
            j % len
        );
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

    let mut sim = Sim::new(spec_4_4(), mono(fingerprint));
    sim.run_to(BLOCK as u64 * 4);
    let s = sim.latest_status().expect("Status kommt an");
    assert_eq!(s.track_count, 1);
    assert_eq!(s.tracks()[0].state, TrackState::Empty);
    assert_eq!(s.bpm, 100.0);
    assert_eq!(s.samples_per_beat, 28_800.0);

    sim.send(Command::StartRecord { track: 0, at: start });
    sim.run_to(start + R / 2);
    let s = sim.latest_status().expect("Status");
    assert_eq!(
        s.tracks()[0].state,
        TrackState::Armed,
        "geplant, aber Eingang noch nicht da"
    );

    sim.run_to(start + R + 4 * BLOCK as u64);
    let s = sim.latest_status().expect("Status");
    assert_eq!(s.tracks()[0].state, TrackState::Recording);
    assert!(s.tracks()[0].filled > 0);

    sim.send(Command::StopRecord { track: 0, at: end });
    sim.send(Command::StartPlay { track: 0, at: end });
    sim.run_to(end + R + 4 * BLOCK as u64);
    let s = sim.latest_status().expect("Status");
    assert_eq!(s.tracks()[0].state, TrackState::Playing);
    assert_eq!(s.tracks()[0].loop_len, end - start);
    assert_eq!(s.tracks()[0].layers, 1);
    let here = timeline.locate(s.pos);
    assert_eq!(
        (s.bar, s.beat, s.beat_offset),
        (here.bar, here.beat, here.offset)
    );
    assert!(s.tracks()[0].input_peak > 0.0);
}

#[test]
fn clear_stop_and_monitor_do_what_they_say() {
    let timeline = Timeline::new(RATE, 100.0, TimeSignature::new(4, 4));
    let start = timeline.bar_start(1);
    let end = timeline.bar_start(3);

    let spec = SimSpec {
        tracks: vec![TrackSpec {
            channel: 0,
            monitor: true,
        }],
        ..spec_4_4()
    };
    let mut sim = Sim::new(spec, mono(fingerprint));
    sim.run_to(4 * BLOCK as u64);
    // Monitoring passes the consumed input straight through.
    assert_eq!(sim.out_history[BLOCK], fingerprint(BLOCK as u64));

    sim.send(Command::SetMonitor {
        track: 0,
        on: false,
    });
    sim.run_to(8 * BLOCK as u64);
    assert_eq!(sim.out_history[7 * BLOCK], 0.0);

    sim.send(Command::StartRecord { track: 0, at: start });
    sim.send(Command::StopRecord { track: 0, at: end });
    sim.send(Command::StartPlay { track: 0, at: end });
    sim.run_to(end + R + 4 * BLOCK as u64);
    assert!(sim.core.track(0).loop_len() > 0);

    let clear_at = sim.pos() + BLOCK as u64;
    sim.send(Command::ClearTrack {
        track: 0,
        at: clear_at,
    });
    sim.run_to(clear_at + 4 * BLOCK as u64);
    assert_eq!(sim.core.track(0).loop_len(), 0);
    assert_eq!(sim.core.track(0).layer_count(), 0);
    let s = sim.latest_status().expect("Status");
    assert_eq!(s.tracks()[0].state, TrackState::Empty);
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

    let mut sim = Sim::new(spec_4_4(), mono(fingerprint));
    sim.send(Command::SetTempo {
        bpm: 120.0,
        signature: TimeSignature::new(3, 4),
        layer_capacity: 921_664,
    });
    sim.run_to(4 * BLOCK as u64);
    let s = sim.latest_status().expect("Status");
    assert_eq!(s.bpm, 120.0, "leerer Track: Tempowechsel wird uebernommen");
    assert_eq!(s.ignored_commands, 0);

    sim.send(Command::StartRecord { track: 0, at: start });
    sim.send(Command::StopRecord { track: 0, at: end });
    sim.run_to(end + R + 4 * BLOCK as u64);
    sim.send(Command::SetTempo {
        bpm: 90.0,
        signature: TimeSignature::new(4, 4),
        layer_capacity: 921_664,
    });
    sim.run_to(sim.pos() + 4 * BLOCK as u64);
    let s = sim.latest_status().expect("Status");
    assert_eq!(s.bpm, 120.0, "belegter Track: Tempo bleibt");
    assert_eq!(s.ignored_commands, 1);
}

/// A take that is never stopped is closed by the length of its buffer instead of running away.
#[test]
fn the_layer_length_bounds_a_take_that_is_never_stopped() {
    let spec = SimSpec {
        layer_capacity: Some(4_096),
        ..spec_4_4()
    };
    let mut sim = Sim::new(spec, mono(fingerprint));
    sim.run_to(4 * BLOCK as u64);

    let start = sim.pos() + BLOCK as u64;
    sim.send(Command::StartRecord { track: 0, at: start });
    sim.run_to(start + R + 8_192);
    assert_eq!(sim.core.track(0).loop_len(), 4_096);
    assert_eq!(
        sim.core.track(0).layer(0).expect("Ebene").content(4_096).len(),
        4_096
    );
}

/// A second take scheduled for the very sample the running one ends at. The engine cannot close
/// the first take at that moment - its last `R` samples are still in the cable - so the new one is
/// queued behind it. Nothing may be lost at that seam, and a new loop really does replace the old.
#[test]
fn a_take_scheduled_at_the_end_of_the_running_one_starts_seamlessly() {
    let start = 4_000u64;
    let mid = start + 6_000;
    let end = mid + 5_000;
    let spec = SimSpec {
        loopback_channel: None,
        ..spec_4_4()
    };
    let mut sim = Sim::new(spec, mono(fingerprint));
    sim.send(Command::StartRecord { track: 0, at: start });
    sim.send(Command::StopRecord { track: 0, at: mid });
    sim.send(Command::StartRecord { track: 0, at: mid });
    sim.send(Command::StopRecord { track: 0, at: end });
    sim.run_to(end + R + 4 * BLOCK as u64);

    assert_eq!(
        sim.core.track(0).layer_count(),
        1,
        "der zweite Lauf ersetzt den ersten, er stapelt sich nicht darauf"
    );
    let content = layer_content(&sim, 0, 0);
    assert_eq!(content.len() as u64, end - mid);
    for (i, &got) in content.iter().enumerate() {
        assert_eq!(
            got,
            fingerprint(mid + R + i as u64),
            "Sample {i} des zweiten Loops"
        );
    }
}

/// Commands sent far ahead of time must not act early, and a stop stamped for a position that has
/// already gone by truncates the take there instead of being lost.
#[test]
fn early_and_late_commands_behave() {
    let timeline = Timeline::new(RATE, 100.0, TimeSignature::new(4, 4));
    let far = timeline.bar_start(6);

    let mut sim = Sim::new(spec_4_4(), mono(fingerprint));
    // Sent immediately, due six bars later.
    sim.send(Command::StartRecord { track: 0, at: far });
    sim.run_to(far - BLOCK as u64);
    assert_eq!(sim.core.track(0).filled(), 0, "darf nicht zu frueh anfangen");

    sim.run_to(far + R + 4 * BLOCK as u64);
    assert!(sim.core.track(0).filled() > 0, "muss dann aber anfangen");

    // A stop stamped for a position that is already behind the write pointer: the take is cut
    // there, it is not ignored and it does not run on forever.
    let late = far + 300;
    assert!(sim.core.track(0).filled() > 300);
    sim.send(Command::StopRecord {
        track: 0,
        at: late,
    });
    sim.run_to(sim.pos() + 4 * BLOCK as u64);
    assert_eq!(
        sim.core.track(0).loop_len(),
        300,
        "spaetes Stopp-Kommando greift"
    );
}

// -------------------------------------------------------------------------------------------
// 6. Phase 2: several tracks
// -------------------------------------------------------------------------------------------

/// Two tracks on two input channels, recorded at the same time, must end up with the material of
/// their own channel and nothing else. This is the "Stimme an Eingang 1, Gitarre an Eingang 2"
/// case, in the smallest form that can prove it.
#[test]
fn two_tracks_record_from_their_own_input_channel() {
    let start = 4_000u64;
    let end = start + 6_000;
    // Channel 0 carries the fingerprint, channel 1 carries its negative - two signals that agree
    // nowhere except at the single zero crossing.
    let played = Box::new(|k: u64, ch: usize| {
        if ch == 0 {
            fingerprint(k)
        } else {
            -fingerprint(k)
        }
    });

    let spec = SimSpec {
        tracks: vec![TrackSpec::on(0), TrackSpec::on(1)],
        input_channels: 2,
        // No cable: nothing must leak from the output back into the recording here.
        loopback_channel: None,
        ..spec_4_4()
    };
    let mut sim = Sim::new(spec, played);
    for track in 0..2 {
        sim.send(Command::StartRecord { track, at: start });
        sim.send(Command::StopRecord { track, at: end });
    }
    sim.run_to(end + R + 4 * BLOCK as u64);

    let voice = layer_content(&sim, 0, 0);
    let guitar = layer_content(&sim, 1, 0);
    assert_eq!(voice.len(), 6_000);
    assert_eq!(guitar.len(), 6_000);
    let mut differences = 0usize;
    for i in 0..6_000usize {
        let k = start + R + i as u64;
        assert_eq!(voice[i], fingerprint(k), "Stimme, Sample {i}");
        assert_eq!(guitar[i], -fingerprint(k), "Gitarre, Sample {i}");
        if voice[i] != guitar[i] {
            differences += 1;
        }
    }
    assert!(
        differences > 5_990,
        "die beiden Tracks muessen nachweislich Verschiedenes enthalten, \
         unterschiedlich sind aber nur {differences} Samples"
    );
}

// -------------------------------------------------------------------------------------------
// 7. Phase 2: layers
// -------------------------------------------------------------------------------------------

/// Record `count` layers of `len` samples each, one after the other, starting at `first_start`.
/// Layer `n` is recorded `gap` samples after the previous one ended, so the takes really do lie in
/// different bars. The signal is a constant per layer, so every layer is identifiable.
fn record_layers(bars: u32, count: usize, gap: u64) -> (Sim, u64, u64) {
    let timeline = Timeline::new(RATE, 100.0, TimeSignature::new(4, 4));
    let origin = timeline.bar_start(2);
    let len = timeline.span_bars(2, bars);

    // Amplitude changes with musical time, so each take records its own value: take `n` runs
    // while `musical` lies in its window.
    let played = mono(move |k: u64| {
        if k < R {
            return 0.0;
        }
        let musical = k - R;
        if musical < origin {
            return 0.0;
        }
        let window = (musical - origin) / (len + gap);
        0.1 * (window + 1) as f32
    });

    let spec = SimSpec {
        bars,
        loopback_channel: None,
        ..spec_4_4()
    };
    let mut sim = Sim::new(spec, played);
    // Everything is scheduled up front, back to back: take `n + 1` starts at the very sample take
    // `n` ends, which is how a loop is actually built and what the queued-take slot exists for.
    let mut last_end = origin;
    for n in 0..count as u64 {
        let start = origin + n * (len + gap);
        let cmd = if n == 0 {
            Command::StartRecord { track: 0, at: start }
        } else {
            Command::StartOverdub { track: 0, at: start }
        };
        sim.send(cmd);
        sim.send(Command::StopRecord {
            track: 0,
            at: start + len,
        });
        if n == 0 {
            sim.send(Command::StartPlay {
                track: 0,
                at: start + len,
            });
        }
        last_end = start + len;
    }
    sim.run_to(last_end + R + 4 * BLOCK as u64);
    (sim, origin, len)
}

/// Three layers on one track: the output is the sample-by-sample sum of the three buffers.
#[test]
fn three_layers_play_back_as_their_exact_sum() {
    let (mut sim, origin, len) = record_layers(4, 3, 0);
    assert_eq!(sim.core.track(0).layer_count(), 3);
    assert_eq!(sim.core.track(0).loop_len(), len);

    let layers: Vec<Vec<f32>> = (0..3).map(|i| layer_content(&sim, 0, i)).collect();
    // Each layer carries its own constant, so they are distinguishable and none is empty.
    let mid = len as usize / 2;
    for (n, layer) in layers.iter().enumerate() {
        assert!(
            (layer[mid] - 0.1 * (n + 1) as f32).abs() < 1e-6,
            "Ebene {} traegt nicht ihr eigenes Signal",
            n + 1
        );
    }
    assert_ne!(layers[0][mid], layers[1][mid]);
    assert_ne!(layers[1][mid], layers[2][mid]);

    // One full pass that lies entirely after the last take.
    let from = origin + (sim.pos() - origin).div_ceil(len) * len;
    sim.run_to(from + len + BLOCK as u64);
    for i in 0..len {
        let idx = ((from + i - origin) % len) as usize;
        let want = layers[0][idx] + layers[1][idx] + layers[2][idx];
        assert_eq!(
            sim.out_history[(from + i) as usize],
            want,
            "Summe der drei Ebenen bei Loop-Index {idx}"
        );
    }
}

/// Muting a layer removes exactly that layer from the sum and changes nothing else.
#[test]
fn a_muted_layer_drops_out_of_the_sum_exactly() {
    let (mut sim, origin, len) = record_layers(4, 3, 0);
    let layers: Vec<Vec<f32>> = (0..3).map(|i| layer_content(&sim, 0, i)).collect();

    sim.send(Command::SetLayerMute {
        track: 0,
        layer: 1,
        muted: true,
    });
    sim.run_blocks(2);

    let from = origin + (sim.pos() - origin).div_ceil(len) * len;
    sim.run_to(from + len + BLOCK as u64);
    for i in 0..len {
        let idx = ((from + i - origin) % len) as usize;
        let want = layers[0][idx] + layers[2][idx];
        assert_eq!(
            sim.out_history[(from + i) as usize],
            want,
            "stumme Ebene 2 darf bei Loop-Index {idx} nicht klingen"
        );
    }
    let s = sim.latest_status().expect("Status");
    assert_eq!(s.tracks()[0].muted_mask, 0b010);
    assert_eq!(s.tracks()[0].layers, 3, "stumm heisst nicht entfernt");

    // And unmuting brings it back, bit for bit.
    sim.send(Command::SetLayerMute {
        track: 0,
        layer: 1,
        muted: false,
    });
    sim.run_blocks(2);
    let from = origin + (sim.pos() - origin).div_ceil(len) * len;
    sim.run_to(from + BLOCK as u64);
    let idx = ((from - origin) % len) as usize;
    assert_eq!(
        sim.out_history[from as usize],
        layers[0][idx] + layers[1][idx] + layers[2][idx]
    );
}

/// Removing a layer hands its buffer back to the control thread - no leak - and leaves the others
/// untouched and aligned.
#[test]
fn a_removed_layer_returns_its_buffer_and_leaves_the_others_alone() {
    let (mut sim, origin, len) = record_layers(4, 3, 0);
    let before: Vec<Vec<f32>> = (0..3).map(|i| layer_content(&sim, 0, i)).collect();
    let reclaimed_before = sim.reclaimed();

    sim.send(Command::RemoveLayer { track: 0, layer: 1 });
    sim.run_blocks(4);

    assert_eq!(sim.core.track(0).layer_count(), 2);
    assert_eq!(
        sim.reclaimed(),
        reclaimed_before + 1,
        "der Puffer der entfernten Ebene muss beim Steuer-Thread ankommen"
    );

    // The survivors keep their content and their index arithmetic.
    let after: Vec<Vec<f32>> = (0..2).map(|i| layer_content(&sim, 0, i)).collect();
    assert_eq!(after[0], before[0]);
    assert_eq!(after[1], before[2], "Ebene 3 rueckt auf Platz 2");

    let from = origin + (sim.pos() - origin).div_ceil(len) * len;
    sim.run_to(from + len + BLOCK as u64);
    for i in 0..len {
        let idx = ((from + i - origin) % len) as usize;
        assert_eq!(
            sim.out_history[(from + i) as usize],
            before[0][idx] + before[2][idx],
            "Rest-Ebenen bei Loop-Index {idx}"
        );
    }
}

/// **The central test of phase 2.** Three layers recorded in completely different bars, with the
/// musician playing an impulse on every beat. Every layer has to carry its impulses on exactly the
/// same loop indices - to the sample, with no tolerance. A per-layer offset of a single sample, or
/// a missing modulo at the loop end, breaks this.
#[test]
fn layers_recorded_in_different_bars_stay_sample_aligned() {
    let timeline = Timeline::new(RATE, 100.0, TimeSignature::new(4, 4));
    let bars = 4u32;
    let origin = timeline.bar_start(4);
    let len = timeline.span_bars(4, bars);

    // Take windows, deliberately not multiples of the loop length apart: the second one starts one
    // bar into a later loop pass, the third one half a bar into an even later one. Both therefore
    // wrap around the loop end while being recorded.
    let starts = [
        origin,
        timeline.bar_start(13),
        timeline.bar_start(22) + timeline.samples_per_beat() as u64 * 2,
    ];
    // One amplitude per take, so the layers can be told apart. Their sum stays inside the
    // headroom, otherwise the output clamp would hide exactly what is being measured.
    let amps = [0.5f32, 0.25, 0.125];

    let played = mono(move |k: u64| {
        if k < R {
            return 0.0;
        }
        let musical = k - R;
        let beat = timeline.beat_index_at(musical);
        if timeline.beat_start(beat) != musical {
            return 0.0;
        }
        // Whichever take is running right now decides the amplitude.
        for (i, &s) in starts.iter().enumerate() {
            if musical >= s && musical < s + len {
                return amps[i];
            }
        }
        0.0
    });

    let spec = SimSpec {
        bars,
        loopback_channel: None,
        ..spec_4_4()
    };
    let mut sim = Sim::new(spec, played);
    for (i, &start) in starts.iter().enumerate() {
        let cmd = if i == 0 {
            Command::StartRecord { track: 0, at: start }
        } else {
            Command::StartOverdub { track: 0, at: start }
        };
        sim.send(cmd);
        sim.send(Command::StopRecord {
            track: 0,
            at: start + len,
        });
        if i == 0 {
            sim.send(Command::StartPlay {
                track: 0,
                at: start + len,
            });
        }
        sim.run_to(start + len + R + 4 * BLOCK as u64);
    }

    assert_eq!(sim.core.track(0).layer_count(), 3);
    assert_eq!(sim.core.track(0).loop_len(), len);
    let layers: Vec<Vec<f32>> = (0..3).map(|i| layer_content(&sim, 0, i)).collect();

    // Every layer: its amplitude on every beat boundary of the loop, silence everywhere else.
    let mut impulses = 0usize;
    for i in 0..len as usize {
        let musical = origin + i as u64;
        let beat = timeline.beat_index_at(musical);
        let on_beat = timeline.beat_start(beat) == musical;
        if on_beat {
            impulses += 1;
        }
        for (n, layer) in layers.iter().enumerate() {
            let want = if on_beat { amps[n] } else { 0.0 };
            assert_eq!(
                layer[i], want,
                "Ebene {} ist bei Loop-Index {i} nicht ausgerichtet (Takt {})",
                n + 1,
                timeline.bar_index_at(musical) + 1
            );
        }
    }
    assert_eq!(impulses, bars as usize * 4);

    // ... and the sum in the output carries all three at the same instant.
    let from = origin + (sim.pos() - origin).div_ceil(len) * len;
    sim.run_to(from + len + BLOCK as u64);
    let total: f32 = amps.iter().sum();
    for i in 0..len {
        let idx = ((from + i - origin) % len) as usize;
        let musical = origin + idx as u64;
        let on_beat = timeline.beat_start(timeline.beat_index_at(musical)) == musical;
        let want = if on_beat { total } else { 0.0 };
        assert_eq!(
            sim.out_history[(from + i) as usize],
            want,
            "Ausgabe bei Loop-Index {idx}"
        );
    }
}

/// The ceiling is a message, not a crash: the seventeenth layer is refused and the sixteen that
/// exist keep playing.
#[test]
fn the_layer_ceiling_is_refused_with_a_reason() {
    use super::command::Refusal;
    use super::track::MAX_LAYERS;

    let bars = 1u32;
    let (mut sim, _, len) = record_layers(bars, 1, 0);
    let mut at = sim.pos() + BLOCK as u64;
    for _ in 1..MAX_LAYERS {
        sim.send(Command::StartOverdub { track: 0, at });
        sim.send(Command::StopRecord {
            track: 0,
            at: at + len,
        });
        sim.run_to(at + len + R + 2 * BLOCK as u64);
        at = sim.pos() + BLOCK as u64;
    }
    assert_eq!(sim.core.track(0).layer_count(), MAX_LAYERS);

    sim.send(Command::StartOverdub { track: 0, at });
    sim.run_to(at + len + R + 2 * BLOCK as u64);
    assert_eq!(
        sim.core.track(0).layer_count(),
        MAX_LAYERS,
        "die Obergrenze haelt"
    );
    let s = sim.latest_status().expect("Status");
    assert_eq!(s.refusal, Refusal::LayerLimit);
    assert!(s.ignored_commands >= 1);
    assert!(
        Refusal::LayerLimit.message().is_some(),
        "und es gibt einen deutschen Satz dazu"
    );
}

// -------------------------------------------------------------------------------------------
// 8. Phase 2: monitoring next to playback
// -------------------------------------------------------------------------------------------

/// Hearing the loop and playing live over it at the same time, on the same track: the output
/// carries both, and what is recorded is the musician's signal alone.
#[test]
fn monitoring_and_playback_coexist_without_touching_the_recording() {
    let timeline = Timeline::new(RATE, 100.0, TimeSignature::new(4, 4));
    let bars = 2u32;
    let origin = timeline.bar_start(2);
    let len = timeline.span_bars(2, bars);

    // A signal that is different at every sample, so a mix-up cannot pass unnoticed.
    let played = mono(fingerprint);
    let spec = SimSpec {
        bars,
        input_channels: 2,
        // No cable in this test: monitoring plus a loopback would be an acoustic feedback loop,
        // and the point here is the engine's mix, not the room.
        loopback_channel: None,
        ..spec_4_4()
    };
    let mut sim = Sim::new(spec, played);

    sim.send(Command::StartRecord { track: 0, at: origin });
    sim.send(Command::StopRecord {
        track: 0,
        at: origin + len,
    });
    sim.send(Command::StartPlay {
        track: 0,
        at: origin + len,
    });
    sim.run_to(origin + len + R + 4 * BLOCK as u64);
    let first = layer_content(&sim, 0, 0);

    // Now: loop playing, monitoring on, and a second layer being recorded on top.
    sim.send(Command::SetMonitor {
        track: 0,
        on: true,
    });
    let over_start = origin + 2 * len;
    sim.send(Command::StartOverdub {
        track: 0,
        at: over_start,
    });
    sim.send(Command::StopRecord {
        track: 0,
        at: over_start + len,
    });
    sim.run_to(over_start + len + R + 4 * BLOCK as u64);

    // The output during the overdub is loop plus live input, sample for sample. The layer being
    // recorded contributes nothing yet - its samples are still in the cable.
    for i in 0..len {
        let pos = over_start + i;
        let idx = ((pos - origin) % len) as usize;
        assert_eq!(
            sim.out_history[pos as usize],
            first[idx] + fingerprint(pos),
            "Loop plus Mithoeren bei Position {pos}"
        );
    }

    // And the recording is untouched by the monitoring: the second layer holds exactly what was
    // played, latency-compensated, and nothing of what was heard.
    let second = layer_content(&sim, 0, 1);
    for i in 0..len {
        let idx = ((over_start + i - origin) % len) as usize;
        assert_eq!(
            second[idx],
            fingerprint(over_start + i + R),
            "Overdub-Ebene bei Loop-Index {idx}"
        );
    }
    let s = sim.latest_status().expect("Status");
    assert!(s.tracks()[0].monitor);
    assert!(s.tracks()[0].playing);
}

// -------------------------------------------------------------------------------------------
// 9. Phase 2: wiping everything
// -------------------------------------------------------------------------------------------

/// "Alles loeschen" has to leave a state that is indistinguishable from a fresh start, and every
/// single buffer has to be back with the control thread.
#[test]
fn clear_all_restores_a_fresh_start_and_returns_every_buffer() {
    let timeline = Timeline::new(RATE, 100.0, TimeSignature::new(4, 4));
    let bars = 1u32;
    let origin = timeline.bar_start(2);
    let len = timeline.span_bars(2, bars);

    let spec = SimSpec {
        bars,
        tracks: vec![TrackSpec::on(0), TrackSpec::on(1)],
        input_channels: 2,
        loopback_channel: None,
        ..spec_4_4()
    };
    let mut sim = Sim::new(spec, Box::new(|k, _| fingerprint(k)));

    // Reference: what a fresh engine reports.
    let mut fresh = Sim::new(
        SimSpec {
            bars,
            tracks: vec![TrackSpec::on(0), TrackSpec::on(1)],
            input_channels: 2,
            loopback_channel: None,
            ..spec_4_4()
        },
        silence(),
    );
    fresh.run_blocks(4);
    let fresh_status = fresh.latest_status().expect("Status");

    // Two tracks, four layers in total, one of them muted, both playing, one monitoring.
    let mut layers_made = 0u64;
    let mut at = origin;
    for round in 0..2 {
        for track in 0..2 {
            let cmd = if round == 0 {
                Command::StartRecord { track, at }
            } else {
                Command::StartOverdub { track, at }
            };
            sim.send(cmd);
            sim.send(Command::StopRecord {
                track,
                at: at + len,
            });
            layers_made += 1;
        }
        sim.send(Command::StartPlay {
            track: 0,
            at: at + len,
        });
        sim.send(Command::StartPlay {
            track: 1,
            at: at + len,
        });
        sim.run_to(at + len + R + 4 * BLOCK as u64);
        at = sim.pos() + BLOCK as u64;
    }
    sim.send(Command::SetLayerMute {
        track: 1,
        layer: 0,
        muted: true,
    });
    sim.send(Command::SetMonitor {
        track: 0,
        on: true,
    });
    sim.run_blocks(4);
    let busy = sim.latest_status().expect("Status");
    assert_eq!(busy.tracks()[0].layers, 2);
    assert_eq!(busy.tracks()[1].layers, 2);
    assert!(!is_fresh(&busy), "Testaufbau: jetzt ist etwas drin");

    // ... and now everything goes.
    let clear_at = sim.pos() + BLOCK as u64;
    sim.send(Command::ClearAll { at: clear_at });
    sim.run_to(clear_at + 8 * BLOCK as u64);

    let after = sim.latest_status().expect("Status");
    assert!(is_fresh(&after), "nach dem Loeschen wie frisch gestartet");
    for (i, (a, f)) in after.tracks().iter().zip(fresh_status.tracks()).enumerate() {
        assert_eq!(
            (a.state, a.layers, a.loop_len, a.filled, a.muted_mask, a.monitor, a.playing),
            (f.state, f.layers, f.loop_len, f.filled, f.muted_mask, f.monitor, f.playing),
            "Track {i} unterscheidet sich vom frischen Zustand"
        );
        assert_eq!(a.input_channel, f.input_channel);
    }
    assert_eq!(sim.core.track_count(), 2);
    assert_eq!(
        sim.reclaimed(),
        layers_made,
        "jeder Ebenen-Puffer muss zurueckgegeben worden sein"
    );
    assert!(
        sim.core.spare_count() > 0,
        "und der Vorrat leerer Puffer ist wieder aufgefuellt"
    );
    assert!(
        sim.pushed() >= sim.reclaimed(),
        "es kann nicht mehr zurueckkommen als hingegeben wurde"
    );

    // The engine is usable again straight away: recording after the wipe works and starts a new
    // loop from scratch.
    let again = sim.pos() + BLOCK as u64;
    sim.send(Command::StartRecord {
        track: 0,
        at: again,
    });
    sim.send(Command::StopRecord {
        track: 0,
        at: again + len,
    });
    sim.run_to(again + len + R + 4 * BLOCK as u64);
    assert_eq!(sim.core.track(0).layer_count(), 1);
    assert_eq!(sim.core.track(0).loop_len(), len);
}
