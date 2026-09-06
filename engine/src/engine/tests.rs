//! Offline proof of sample accuracy. Nothing here opens an audio device.

use super::command::Command;
use super::frame::{Channels, Frame};
use super::metro::Metronome;
use super::process::is_fresh;
use super::sim::{Played, Sim, SimSpec, TrackSpec, mono, silence};
use super::timeline::{TimeSignature, Timeline};
use super::track::TrackState;

const RATE: u32 = 48_000;
const BLOCK: usize = 128;
/// Roundtrip measured in phase 0: 827 frames at 128 frames / 48 kHz, stddev 0 over 60 runs.
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
        sim.out_l[(play_at - 1) as usize],
        0.0,
        "vor dem Startsample muss Stille sein"
    );
    let loop_len = sim.core.track(0).loop_len();
    let content = layer_content(&sim, 0, 0);
    let expected = content[((play_at - start) % loop_len) as usize];
    assert_ne!(expected, 0.0, "Testaufbau: erwartetes Sample ist nicht still");
    assert_eq!(
        sim.out_l[play_at as usize], expected,
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

    let history = &sim.out_l;
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
    assert!(s.tracks()[0].input_peak_max() > 0.0);
}

#[test]
fn clear_stop_and_monitor_do_what_they_say() {
    let timeline = Timeline::new(RATE, 100.0, TimeSignature::new(4, 4));
    let start = timeline.bar_start(1);
    let end = timeline.bar_start(3);

    let spec = SimSpec {
        tracks: vec![TrackSpec::on(0).monitoring()],
        ..spec_4_4()
    };
    let mut sim = Sim::new(spec, mono(fingerprint));
    sim.run_to(4 * BLOCK as u64);
    // Monitoring passes the consumed input straight through.
    assert_eq!(sim.out_l[BLOCK], fingerprint(BLOCK as u64));

    sim.send(Command::SetMonitor {
        track: 0,
        on: false,
    });
    sim.run_to(8 * BLOCK as u64);
    assert_eq!(sim.out_l[7 * BLOCK], 0.0);

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
        layer_frames: 921_664,
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
        layer_frames: 921_664,
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
        layer_frames: Some(4_096),
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
            sim.out_l[(from + i) as usize],
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
            sim.out_l[(from + i) as usize],
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
        sim.out_l[from as usize],
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
            sim.out_l[(from + i) as usize],
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
            sim.out_l[(from + i) as usize],
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
            sim.out_l[pos as usize],
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
        assert_eq!(a.input_channels, f.input_channels);
        assert_eq!(a.channels, f.channels);
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

// -------------------------------------------------------------------------------------------
// 12. The loop grid, end to end: what the scheduler plans really does land on the layer below
// -------------------------------------------------------------------------------------------

/// The whole point of quantising an overdub to the track's own grid, proven through the engine
/// rather than on paper: a further layer planned by [`Scheduler`] in [`Quantize::Loop`] mode has to
/// end up sample-identical over the first one.
///
/// The probe is deliberately *not* a signal locked to the beat. Bar boundaries are rounded
/// individually while a loop is a fixed number of samples, so the ideal beat grid slides against
/// the loop index by up to a sample per pass (`docs/architektur.md`, section 6). What the musician
/// hears as "the same moment in the loop" is a distance from the loop's top, and that is what is
/// played here: the same waveform, measured from the start of each take.
///
/// Odd time signatures are the interesting ones - 3/4 and 7/8 both have a bar length that is not a
/// whole number of samples, which is exactly where a grid derived from bar boundaries instead of
/// from `origin` and `loop_len` would miss.
#[test]
fn an_overdub_planned_on_the_loop_grid_lands_sample_identical_on_the_first_layer() {
    use super::schedule::{Quantize, Scheduler};

    for (bpm, beats_per_bar, beat_unit, bars) in [(100.0, 3u32, 4u32, 4u32), (137.0, 7, 8, 5)] {
        let signature = TimeSignature::new(beats_per_bar, beat_unit);
        let timeline = Timeline::new(RATE, bpm, signature);
        let mut sched = Scheduler::new(timeline, bars, BLOCK as u32, Quantize::Loop);

        // The musician presses "record" early, inside bar 2 of the very first loop. With the loop
        // grid that arms the take for the start of the next loop - the whole point of the change.
        let press = timeline.bar_start(1) + 137;
        let record = sched.record(0, "t", press);
        let start = record.commands[0].at().expect("Aufnahme ist getimt");
        let end = record.commands[1].at().expect("Stopp ist getimt");
        let len = end - start;
        assert_eq!(
            start,
            timeline.bar_start(bars as u64),
            "{beats_per_bar}/{beat_unit}: aus Takt 2 wird der Beginn des naechsten Loops"
        );

        // Then "overdub", pressed somewhere in the middle of the second pass.
        let press_again = start + len + len / 3;
        let over = sched.overdub(0, "t", press_again, start, len, 1);
        let start2 = over.commands[0].at().expect("Overdub ist getimt");
        let end2 = over.commands[1].at().expect("Stopp ist getimt");
        assert_eq!(
            (start2 - start) % len,
            0,
            "{beats_per_bar}/{beat_unit}: die Ebene setzt auf dem Loop-Raster an"
        );
        assert_eq!(end2 - start2, len, "und dauert genau einen Durchlauf");

        // The same waveform on both takes, at half the level the second time, measured from the
        // start of whichever take is running.
        let played = mono(move |k: u64| {
            if k < R {
                return 0.0;
            }
            let musical = k - R;
            if (start..end).contains(&musical) {
                0.5 * fingerprint(musical - start)
            } else if (start2..end2).contains(&musical) {
                0.25 * fingerprint(musical - start2)
            } else {
                0.0
            }
        });

        let spec = SimSpec {
            bpm,
            signature,
            bars,
            loopback_channel: None,
            ..spec_4_4()
        };
        let mut sim = Sim::new(spec, played);
        for cmd in record.commands.iter().chain(over.commands.iter()) {
            sim.send(*cmd);
        }
        sim.run_to(end2 + R + 4 * BLOCK as u64);

        assert_eq!(sim.core.track(0).layer_count(), 2);
        assert_eq!(sim.core.track(0).loop_len(), len);
        let first = layer_content(&sim, 0, 0);
        let second = layer_content(&sim, 0, 1);
        assert_eq!(first.len(), len as usize);
        assert_eq!(second.len(), len as usize);

        // Zero tolerance: every index of the loop carries the same moment of the performance in
        // both layers, at exactly half the amplitude. One sample of offset breaks this everywhere.
        let mut written = 0usize;
        for i in 0..len as usize {
            assert_eq!(
                second[i] * 2.0,
                first[i],
                "{beats_per_bar}/{beat_unit}: Loop-Index {i} liegt nicht uebereinander"
            );
            if first[i] != 0.0 {
                written += 1;
            }
        }
        assert!(
            written as u64 > len - 64,
            "{beats_per_bar}/{beat_unit}: der Loop ist fast vollstaendig beschrieben, nicht nur \
             an ein paar Stellen ({written} von {len})"
        );
    }
}

// -------------------------------------------------------------------------------------------
// 11. Phase 6: effects act on playback and monitoring, never on the recording
// -------------------------------------------------------------------------------------------

/// **The one test this whole phase stands on.** With every effect switched on, at settings that
/// change the signal beyond recognition, what lands in the loop buffer is still the input sample,
/// latency-compensated and otherwise untouched.
///
/// If this ever fails, a take is unrecoverable: a compressor or a reverb baked into a recording
/// cannot be taken out again. The plan says "aufgenommen wird trocken", and this is that sentence
/// as an assertion.
///
/// The track also monitors while it records, because that is the situation the mistake would hide
/// in: monitoring runs through the chain, so a wrongly wired engine would record what it plays.
#[test]
fn the_recording_stays_dry_with_the_whole_chain_turned_up() {
    use super::fx::{FxParam, FxPreset, FxSlot};

    let timeline = Timeline::new(RATE, 100.0, TimeSignature::new(4, 4));
    let bars = 2u32;
    let origin = timeline.bar_start(2);
    let len = timeline.span_bars(2, bars);

    let spec = SimSpec {
        bars,
        // Monitoring on from the start: the live signal goes through the chain and out, while the
        // recording has to stay clean.
        tracks: vec![TrackSpec::on(0).monitoring()],
        // No cable - the chain plus a loopback would be a feedback loop, and what is under test
        // is the engine's wiring, not the room.
        loopback_channel: None,
        ..spec_4_4()
    };
    let mut sim = Sim::new(spec, mono(fingerprint));

    // Everything on, and everything as violent as the parameters allow.
    sim.send(Command::LoadFxPreset {
        track: 0,
        preset: FxPreset::Voice,
    });
    for slot in FxSlot::all() {
        sim.send(Command::SetFxEnabled {
            track: 0,
            slot,
            on: true,
        });
    }
    for param in [
        FxParam::CompThresholdDb(-40.0),
        FxParam::CompRatio(20.0),
        FxParam::CompMakeupDb(12.0),
        FxParam::DelayFeedback(0.8),
        FxParam::DelayMix(1.0),
        FxParam::ReverbSize(0.9),
        FxParam::ReverbMix(1.0),
        FxParam::BandGainDb { band: 1, db: 18.0 },
    ] {
        sim.send(Command::SetFxParam { track: 0, param });
    }
    sim.run_blocks(4);

    sim.send(Command::StartRecord { track: 0, at: origin });
    sim.send(Command::StopRecord {
        track: 0,
        at: origin + len,
    });
    sim.run_to(origin + len + R + 4 * BLOCK as u64);

    let recorded = layer_content(&sim, 0, 0);
    assert_eq!(recorded.len(), len as usize);
    for (i, &got) in recorded.iter().enumerate() {
        assert_eq!(
            got,
            fingerprint(origin + R + i as u64),
            "Sample {i} der Aufnahme traegt eine Spur der Effektkette"
        );
    }

    // And the counter-check, so the test cannot pass because the chain is not connected at all:
    // what left the output during the take is *not* what went in.
    let mut different = 0usize;
    for i in 0..len {
        let pos = (origin + i) as usize;
        if sim.out_l[pos] != fingerprint(origin + i) {
            different += 1;
        }
    }
    assert!(
        different as u64 > len / 2,
        "die Kette hat am Ausgang nichts veraendert - dann beweist der Test oben nichts \
         ({different} von {len} Samples)"
    );
}

/// The other half of the arrangement: the chain really is in the playback path, and the bypass
/// really takes it back out - bit for bit, so it works as a panic switch on stage.
#[test]
fn the_chain_colours_the_playback_and_the_bypass_takes_it_back_exactly() {
    use super::fx::FxPreset;

    let timeline = Timeline::new(RATE, 100.0, TimeSignature::new(4, 4));
    let bars = 1u32;
    let origin = timeline.bar_start(2);
    let len = timeline.span_bars(2, bars);

    let spec = SimSpec {
        bars,
        loopback_channel: None,
        ..spec_4_4()
    };
    let mut sim = Sim::new(spec, mono(fingerprint));
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
    let loop_content = layer_content(&sim, 0, 0);

    // Chain on: the played-back loop is coloured.
    sim.send(Command::LoadFxPreset {
        track: 0,
        preset: FxPreset::Voice,
    });
    let coloured_from = origin + 2 * len;
    sim.run_to(coloured_from + len);
    let mut different = 0usize;
    for i in 0..len {
        let pos = coloured_from + i;
        let dry = loop_content[((pos - origin) % len) as usize];
        if sim.out_l[pos as usize] != dry {
            different += 1;
        }
    }
    assert!(
        different as u64 > len / 2,
        "das Preset aendert die Wiedergabe nicht ({different} von {len})"
    );

    // Bypass: back to the bare loop, sample for sample. One second of settling, because the
    // crossfade only becomes an exact zero once it is under -100 dB.
    sim.send(Command::SetFxBypass {
        track: 0,
        on: true,
    });
    let bypassed_from = coloured_from + 2 * len + RATE as u64;
    sim.run_to(bypassed_from + len);
    for i in 0..len {
        let pos = bypassed_from + i;
        let dry = loop_content[((pos - origin) % len) as usize];
        assert_eq!(
            sim.out_l[pos as usize], dry,
            "Position {pos} ist im Bypass nicht bitgleich mit dem Loop"
        );
    }

    // And the status carries the whole thing back to the control thread.
    let status = sim.latest_status().expect("Status");
    let fx = status.tracks()[0].fx;
    assert!(fx.bypass);
    assert_eq!(fx.preset, FxPreset::Voice);
    assert_eq!(fx.letters(), "HEK-R");
}

/// The delay is tempo-synchronous, and the engine is where the tempo actually lives: a quarter
/// note at 120 BPM has to arrive as exactly 24 000 samples in the status, without anybody
/// converting milliseconds anywhere.
#[test]
fn the_delay_time_comes_from_the_engine_timeline() {
    use super::fx::{DelayNote, FxParam, FxPreset, FxSlot};

    let spec = SimSpec {
        bpm: 120.0,
        bars: 1,
        loopback_channel: None,
        ..spec_4_4()
    };
    let mut sim = Sim::new(spec, silence());
    sim.send(Command::LoadFxPreset {
        track: 0,
        preset: FxPreset::Voice,
    });
    sim.send(Command::SetFxEnabled {
        track: 0,
        slot: FxSlot::Delay,
        on: true,
    });
    sim.send(Command::SetFxParam {
        track: 0,
        param: FxParam::DelayNote(DelayNote::Quarter),
    });
    sim.run_blocks(8);
    let status = sim.latest_status().expect("Status");
    assert_eq!(status.tracks()[0].fx.delay_samples, 24_000);
    assert_eq!(status.tracks()[0].fx.letters(), "HEKDR");

    // A tempo change moves it, because the delay asks the timeline rather than a millisecond
    // value somebody typed in.
    sim.send(Command::SetTempo {
        bpm: 90.0,
        signature: TimeSignature::new(4, 4),
        layer_frames: 200_000,
    });
    sim.run_blocks(200);
    let status = sim.latest_status().expect("Status");
    assert_eq!(status.tracks()[0].fx.delay_samples, 32_000);
}

// -------------------------------------------------------------------------------------------
// 13. Stereo: two channels in, two channels out, and nothing swapped on the way
// -------------------------------------------------------------------------------------------

/// A second fingerprint, unmistakably different from the first. Their only common value is zero,
/// so a channel mix-up cannot pass a comparison by accident.
fn other_fingerprint(k: u64) -> f32 {
    -0.5 * fingerprint(k)
}

/// The content of one channel of one layer, de-interleaved.
fn layer_channel(sim: &Sim, track: usize, layer: usize, channel: usize) -> Vec<f32> {
    let t = sim.core.track(track);
    t.layer(layer)
        .expect("Ebene existiert")
        .channel(channel, t.loop_len())
}

/// Two input channels in, two channels out, and they stay apart the whole way.
///
/// The pair is deliberately **not** adjacent - inputs 1 and 3 - because an ASIO router puts the two
/// halves of a stereo return wherever it likes, and the decoy on the channel in between must never
/// appear in the recording.
#[test]
fn a_stereo_track_records_two_inputs_and_plays_them_back_on_their_own_side() {
    let timeline = Timeline::new(RATE, 100.0, TimeSignature::new(4, 4));
    let bars = 2u32;
    let origin = timeline.bar_start(2);
    let len = timeline.span_bars(2, bars);

    let played: Played = Box::new(|k: u64, ch: usize| match ch {
        0 => fingerprint(k),
        2 => other_fingerprint(k),
        // The channel between the two: full scale, and it may never turn up anywhere.
        _ => 0.99,
    });

    let spec = SimSpec {
        bars,
        tracks: vec![TrackSpec::stereo(0, 2)],
        input_channels: 3,
        loopback_channel: None,
        ..spec_4_4()
    };
    let mut sim = Sim::new(spec, played);
    sim.send(Command::StartRecord {
        track: 0,
        at: origin,
    });
    sim.send(Command::StopRecord {
        track: 0,
        at: origin + len,
    });
    sim.send(Command::StartPlay {
        track: 0,
        at: origin + len,
    });
    sim.run_to(origin + len + R + 4 * BLOCK as u64);

    assert_eq!(sim.core.track(0).channels(), Channels::Stereo);
    assert_eq!(sim.core.track(0).loop_len(), len, "Loop-Laenge zaehlt Frames");
    let left = layer_channel(&sim, 0, 0, 0);
    let right = layer_channel(&sim, 0, 0, 1);
    assert_eq!(left.len() as u64, len);
    assert_eq!(right.len() as u64, len);

    let mut differences = 0usize;
    for i in 0..len as usize {
        let k = origin + R + i as u64;
        assert_eq!(left[i], fingerprint(k), "linker Kanal, Frame {i}");
        assert_eq!(right[i], other_fingerprint(k), "rechter Kanal, Frame {i}");
        assert_ne!(left[i], 0.99, "der Koeder von Eingang 2 ist im Loop gelandet");
        if left[i] != right[i] {
            differences += 1;
        }
    }
    assert!(
        differences as u64 > len - 8,
        "die beiden Kanaele muessen nachweislich Verschiedenes enthalten, verschieden sind aber \
         nur {differences} von {len} Frames"
    );

    // ... and the playback puts each of them on its own bus channel, sample for sample. The track
    // is centred, so both pan gains are exactly 1.0 and this is a bit-exact comparison.
    let from = origin + 2 * len;
    sim.run_to(from + len + BLOCK as u64);
    for i in 0..len {
        let idx = ((from + i - origin) % len) as usize;
        assert_eq!(
            sim.out(from + i),
            Frame::new(left[idx], right[idx]),
            "Ausgabe bei Loop-Index {idx}"
        );
    }
}

/// The central alignment test of phase 2, on stereo: three layers recorded in completely different
/// bars, each carrying an impulse on every beat, and the two channels carrying *different*
/// amplitudes. Every layer has to put its impulses on exactly the same loop indices, on both
/// channels, with no tolerance.
///
/// An offset of one *sample* instead of one frame - the classic stereo mistake - shows up here as
/// the two channels having swapped, and a per-layer offset shows up as a missing impulse.
#[test]
fn stereo_layers_recorded_in_different_bars_stay_sample_aligned() {
    let timeline = Timeline::new(RATE, 100.0, TimeSignature::new(4, 4));
    let bars = 4u32;
    let origin = timeline.bar_start(4);
    let len = timeline.span_bars(4, bars);

    // The same three takes as in the mono test: bar 5, bar 14, and bar 23 plus half a bar, so the
    // later two wrap around the loop end while they are being recorded.
    let starts = [
        origin,
        timeline.bar_start(13),
        timeline.bar_start(22) + timeline.samples_per_beat() as u64 * 2,
    ];
    let amps = [0.5f32, 0.25, 0.125];
    // The right channel is a quarter of the left one on every take, so a swap is unmistakable.
    let right_factor = 0.25f32;

    let played: Played = Box::new(move |k: u64, ch: usize| {
        if k < R {
            return 0.0;
        }
        let musical = k - R;
        let beat = timeline.beat_index_at(musical);
        if timeline.beat_start(beat) != musical {
            return 0.0;
        }
        for (i, &s) in starts.iter().enumerate() {
            if musical >= s && musical < s + len {
                return if ch == 1 {
                    amps[i] * right_factor
                } else {
                    amps[i]
                };
            }
        }
        0.0
    });

    let spec = SimSpec {
        bars,
        tracks: vec![TrackSpec::stereo(0, 1)],
        input_channels: 2,
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
    let layers: Vec<(Vec<f32>, Vec<f32>)> = (0..3)
        .map(|i| (layer_channel(&sim, 0, i, 0), layer_channel(&sim, 0, i, 1)))
        .collect();

    let mut impulses = 0usize;
    for i in 0..len as usize {
        let musical = origin + i as u64;
        let on_beat = timeline.beat_start(timeline.beat_index_at(musical)) == musical;
        if on_beat {
            impulses += 1;
        }
        for (n, (left, right)) in layers.iter().enumerate() {
            let want = if on_beat { amps[n] } else { 0.0 };
            assert_eq!(
                left[i],
                want,
                "Ebene {} links bei Loop-Index {i} nicht ausgerichtet (Takt {})",
                n + 1,
                timeline.bar_index_at(musical) + 1
            );
            assert_eq!(
                right[i],
                want * right_factor,
                "Ebene {} rechts bei Loop-Index {i} nicht ausgerichtet",
                n + 1
            );
        }
    }
    assert_eq!(impulses, bars as usize * 4);

    // And the sum on the bus carries all three at the same instant, on both sides.
    let from = origin + (sim.pos() - origin).div_ceil(len) * len;
    sim.run_to(from + len + BLOCK as u64);
    let total: f32 = amps.iter().sum();
    for i in 0..len {
        let idx = ((from + i - origin) % len) as usize;
        let musical = origin + idx as u64;
        let on_beat = timeline.beat_start(timeline.beat_index_at(musical)) == musical;
        let want = if on_beat { total } else { 0.0 };
        assert_eq!(
            sim.out(from + i),
            Frame::new(want, want * right_factor),
            "Ausgabe bei Loop-Index {idx}"
        );
    }
}

/// The latency compensation on a stereo track, proven exactly as in the mono case and with the
/// same tolerance: zero.
///
/// The musician plays an impulse on every beat *as he hears it*, so it reaches the input `R` frames
/// after the beat it belongs to - on both channels at the same instant, which is what a stereo
/// source does. Both channels must land on the beat grid, and both must land on the *same* frame.
#[test]
fn latency_compensation_puts_a_stereo_take_exactly_on_the_beat() {
    let timeline = Timeline::new(RATE, 100.0, TimeSignature::new(4, 4));
    let start = timeline.bar_start(4);
    let end = timeline.bar_start(12);

    let played: Played = Box::new(move |k: u64, ch: usize| {
        if k < R {
            return 0.0;
        }
        let musical = k - R;
        let beat = timeline.beat_index_at(musical);
        if timeline.beat_start(beat) != musical {
            return 0.0;
        }
        // Different levels per side: an offset by one sample would exchange the two.
        if ch == 1 { 0.5 } else { 1.0 }
    });

    let spec = SimSpec {
        tracks: vec![TrackSpec::stereo(0, 1)],
        input_channels: 2,
        loopback_channel: None,
        ..spec_4_4()
    };
    let mut sim = Sim::new(spec, played);
    sim.send(Command::StartRecord { track: 0, at: start });
    sim.send(Command::StopRecord { track: 0, at: end });
    sim.run_to(end + R + 4 * BLOCK as u64);

    let left = layer_channel(&sim, 0, 0, 0);
    let right = layer_channel(&sim, 0, 0, 1);
    assert_eq!(left.len() as u64, end - start);
    assert_eq!(right.len() as u64, end - start);

    let mut impulses = 0usize;
    for i in 0..left.len() {
        let musical = start + i as u64;
        let on_beat = timeline.beat_start(timeline.beat_index_at(musical)) == musical;
        let (want_l, want_r) = if on_beat { (1.0, 0.5) } else { (0.0, 0.0) };
        if on_beat {
            impulses += 1;
        }
        assert_eq!(left[i], want_l, "links bei Loop-Index {i}");
        assert_eq!(right[i], want_r, "rechts bei Loop-Index {i}");
    }
    assert_eq!(impulses, 8 * 4, "acht Takte zu vier Schlaegen");

    // The counter-example that makes the test above mean something: with the compensation set to
    // zero the same performance lands `R` frames late, on both channels alike.
    let spec = SimSpec {
        latency: 0,
        roundtrip: R,
        tracks: vec![TrackSpec::stereo(0, 1)],
        input_channels: 2,
        loopback_channel: None,
        ..spec_4_4()
    };
    let played: Played = Box::new(move |k: u64, ch: usize| {
        if k < R {
            return 0.0;
        }
        let musical = k - R;
        let beat = timeline.beat_index_at(musical);
        if timeline.beat_start(beat) != musical {
            return 0.0;
        }
        if ch == 1 { 0.5 } else { 1.0 }
    });
    let mut wrong = Sim::new(spec, played);
    wrong.send(Command::StartRecord { track: 0, at: start });
    wrong.send(Command::StopRecord { track: 0, at: end });
    wrong.run_to(end + 2 * R + 4 * BLOCK as u64);
    let left = layer_channel(&wrong, 0, 0, 0);
    let on_grid = left
        .iter()
        .enumerate()
        .filter(|(i, v)| {
            let musical = start + *i as u64;
            **v != 0.0 && timeline.beat_start(timeline.beat_index_at(musical)) == musical
        })
        .count();
    assert_eq!(
        on_grid, 0,
        "ohne Kompensation darf kein Impuls auf einer Schlaggrenze liegen"
    );
}

/// A mono and a stereo track side by side on the same device, recorded at the same time. Each has
/// to end up with its own material, in its own channel count, and the buffer pool has to have
/// handed out the right kind to each.
#[test]
fn a_mono_and_a_stereo_track_run_side_by_side() {
    let start = 4_000u64;
    let end = start + 6_000;
    // Channel 0 for the microphone, channels 1 and 2 for the stereo instrument.
    let played: Played = Box::new(|k: u64, ch: usize| match ch {
        0 => fingerprint(k),
        1 => other_fingerprint(k),
        _ => 0.4 - other_fingerprint(k),
    });

    let spec = SimSpec {
        tracks: vec![TrackSpec::on(0), TrackSpec::stereo(1, 2)],
        input_channels: 3,
        loopback_channel: None,
        ..spec_4_4()
    };
    let mut sim = Sim::new(spec, played);
    for track in 0..2 {
        sim.send(Command::StartRecord { track, at: start });
        sim.send(Command::StopRecord { track, at: end });
    }
    sim.run_to(end + R + 4 * BLOCK as u64);

    assert_eq!(sim.core.track(0).channels(), Channels::Mono);
    assert_eq!(sim.core.track(1).channels(), Channels::Stereo);
    let voice = layer_content(&sim, 0, 0);
    assert_eq!(voice.len(), 6_000, "ein Mono-Puffer haelt ein Sample je Frame");
    let piano_l = layer_channel(&sim, 1, 0, 0);
    let piano_r = layer_channel(&sim, 1, 0, 1);
    assert_eq!(piano_l.len(), 6_000);
    for i in 0..6_000usize {
        let k = start + R + i as u64;
        assert_eq!(voice[i], fingerprint(k), "Stimme, Frame {i}");
        assert_eq!(piano_l[i], other_fingerprint(k), "Klavier links, Frame {i}");
        assert_eq!(
            piano_r[i],
            0.4 - other_fingerprint(k),
            "Klavier rechts, Frame {i}"
        );
    }

    // One buffer of each kind is in use, and the pool has handed out exactly those kinds.
    assert!(sim.pushed_of(Channels::Mono) >= 1);
    assert!(sim.pushed_of(Channels::Stereo) >= 1);

    // Wiping everything gives every buffer back, each to its own stock, and refills both.
    let clear_at = sim.pos() + BLOCK as u64;
    sim.send(Command::ClearAll { at: clear_at });
    sim.run_to(clear_at + 8 * BLOCK as u64);
    assert_eq!(
        sim.reclaimed_of(Channels::Mono),
        1,
        "der Mono-Puffer muss zurueckkommen"
    );
    assert_eq!(
        sim.reclaimed_of(Channels::Stereo),
        1,
        "der Stereo-Puffer muss zurueckkommen"
    );
    assert!(sim.core.spare_count_of(Channels::Mono) > 0);
    assert!(sim.core.spare_count_of(Channels::Stereo) > 0);
    let status = sim.latest_status().expect("Status");
    assert!(is_fresh(&status), "nach dem Loeschen wie frisch gestartet");
    assert_eq!(status.tracks()[0].channels, 1);
    assert_eq!(status.tracks()[1].channels, 2);
    assert_eq!(status.tracks()[1].input_channels, [1, 2]);
}

/// The panner, through the whole engine: a mono track in the centre is equally loud on both bus
/// channels; panned hard left the right one is digitally silent.
#[test]
fn a_centred_mono_track_is_equally_loud_on_both_sides_and_hard_left_silences_the_right() {
    let timeline = Timeline::new(RATE, 100.0, TimeSignature::new(4, 4));
    let bars = 1u32;
    let origin = timeline.bar_start(2);
    let len = timeline.span_bars(2, bars);

    let spec = SimSpec {
        bars,
        loopback_channel: None,
        ..spec_4_4()
    };
    let mut sim = Sim::new(spec, mono(fingerprint));
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
    let content = layer_content(&sim, 0, 0);

    // Centre: both sides carry the loop, bit for bit.
    let centre_from = origin + 2 * len;
    sim.run_to(centre_from + len + BLOCK as u64);
    let mut loud = 0usize;
    for i in 0..len {
        let want = content[((centre_from + i - origin) % len) as usize];
        let got = sim.out(centre_from + i);
        assert_eq!(got.l, want, "links bei Position {}", centre_from + i);
        assert_eq!(got.r, want, "rechts bei Position {}", centre_from + i);
        if want != 0.0 {
            loud += 1;
        }
    }
    assert!(
        loud as u64 > len / 2,
        "Testaufbau: der Loop muss ueberhaupt klingen"
    );

    // Hard left: the left side is unchanged, the right one is exactly zero.
    sim.send(Command::SetPan {
        track: 0,
        pan: -1.0,
    });
    sim.run_blocks(2);
    let left_from = origin + (sim.pos() - origin).div_ceil(len) * len;
    sim.run_to(left_from + len + BLOCK as u64);
    for i in 0..len {
        let want = content[((left_from + i - origin) % len) as usize];
        let got = sim.out(left_from + i);
        assert_eq!(got.l, want, "links bei Position {}", left_from + i);
        assert_eq!(got.r, 0.0, "rechts muss still sein, Position {}", left_from + i);
    }
    let status = sim.latest_status().expect("Status");
    assert_eq!(status.tracks()[0].pan, -1.0);
    assert_eq!(status.output_peak[1], 0.0, "die rechte Summe bleibt stumm");
    assert!(status.output_peak[0] > 0.0, "die linke Summe klingt");
}

/// **The dryness test, in stereo.** With every effect switched on at settings that change the
/// signal beyond recognition, and monitoring on so the chain really is in the signal path, what
/// lands in a stereo loop buffer is still the two raw input channels, latency-compensated and
/// otherwise untouched.
#[test]
fn a_stereo_recording_stays_dry_with_the_whole_chain_turned_up() {
    use super::fx::{FxParam, FxPreset, FxSlot};

    let timeline = Timeline::new(RATE, 100.0, TimeSignature::new(4, 4));
    let bars = 2u32;
    let origin = timeline.bar_start(2);
    let len = timeline.span_bars(2, bars);

    let played: Played = Box::new(|k: u64, ch: usize| {
        if ch == 1 {
            other_fingerprint(k)
        } else {
            fingerprint(k)
        }
    });

    let spec = SimSpec {
        bars,
        tracks: vec![TrackSpec::stereo(0, 1).monitoring().panned(-0.4)],
        input_channels: 2,
        loopback_channel: None,
        ..spec_4_4()
    };
    let mut sim = Sim::new(spec, played);

    sim.send(Command::LoadFxPreset {
        track: 0,
        preset: FxPreset::Voice,
    });
    for slot in FxSlot::all() {
        sim.send(Command::SetFxEnabled {
            track: 0,
            slot,
            on: true,
        });
    }
    for param in [
        FxParam::CompThresholdDb(-40.0),
        FxParam::CompRatio(20.0),
        FxParam::CompMakeupDb(12.0),
        FxParam::DelayFeedback(0.8),
        FxParam::DelayMix(1.0),
        FxParam::ReverbSize(0.9),
        FxParam::ReverbMix(1.0),
        FxParam::BandGainDb { band: 1, db: 18.0 },
    ] {
        sim.send(Command::SetFxParam { track: 0, param });
    }
    sim.run_blocks(4);

    sim.send(Command::StartRecord { track: 0, at: origin });
    sim.send(Command::StopRecord {
        track: 0,
        at: origin + len,
    });
    sim.run_to(origin + len + R + 4 * BLOCK as u64);

    let left = layer_channel(&sim, 0, 0, 0);
    let right = layer_channel(&sim, 0, 0, 1);
    assert_eq!(left.len() as u64, len);
    for i in 0..len as usize {
        let k = origin + R + i as u64;
        assert_eq!(left[i], fingerprint(k), "links, Frame {i}: Spur der Effektkette");
        assert_eq!(
            right[i],
            other_fingerprint(k),
            "rechts, Frame {i}: Spur der Effektkette"
        );
    }

    // And the counter-check, so the test cannot pass because the chain is not connected: what left
    // the bus during the take is *not* what went in, on either side.
    let mut different = 0usize;
    for i in 0..len {
        let got = sim.out(origin + i);
        if got.l != fingerprint(origin + i) || got.r != other_fingerprint(origin + i) {
            different += 1;
        }
    }
    assert!(
        different as u64 > len / 2,
        "die Kette hat am Ausgang nichts veraendert - dann beweist der Test oben nichts \
         ({different} von {len} Frames)"
    );
}

/// A tempo change invalidates every prepared buffer, of both lengths at once. Afterwards both
/// stocks have to be refilled at the new length, and a stereo take has to find a stereo buffer -
/// the case where an accounting slip between the two threads would show up as "kein vorbereiteter
/// Puffer frei" on the first overdub after a tempo change and nowhere else.
#[test]
fn both_buffer_stocks_survive_a_tempo_change() {
    use super::command::Refusal;

    let spec = SimSpec {
        bars: 1,
        tracks: vec![TrackSpec::on(0), TrackSpec::stereo(0, 1)],
        input_channels: 2,
        loopback_channel: None,
        ..spec_4_4()
    };
    let mut sim = Sim::new(spec, mono(fingerprint));
    sim.run_blocks(4);

    let signature = TimeSignature::new(4, 4);
    let after = Timeline::new(RATE, 137.0, signature);
    let layer_frames = super::process::loop_capacity(&after, 1);
    sim.send(Command::SetTempo {
        bpm: 137.0,
        signature,
        layer_frames,
    });
    // The control thread learns the new length the same way `live.rs` and `host.rs` do.
    sim.set_layer_frames(layer_frames as usize);
    sim.run_blocks(20);

    let status = sim.latest_status().expect("Status");
    assert_eq!(status.bpm, 137.0, "leere Tracks: der Tempowechsel greift");
    assert!(
        sim.core.spare_count_of(Channels::Mono) > 0,
        "der Mono-Vorrat muss wieder gefuellt sein"
    );
    assert!(
        sim.core.spare_count_of(Channels::Stereo) > 0,
        "der Stereo-Vorrat muss wieder gefuellt sein"
    );

    // And both really are usable: a take on each track finds a buffer of its own length.
    let len = after.span_bars(0, 1);
    let start = after.bar_start_at_or_after(sim.pos() + BLOCK as u64);
    for track in 0..2 {
        sim.send(Command::StartRecord { track, at: start });
        sim.send(Command::StopRecord {
            track,
            at: start + len,
        });
    }
    sim.run_to(start + len + R + 8 * BLOCK as u64);
    let status = sim.latest_status().expect("Status");
    assert_eq!(status.refusal, Refusal::None, "kein Kommando wurde abgelehnt");
    assert_eq!(sim.core.track(0).loop_len(), len, "mono nimmt auf");
    assert_eq!(sim.core.track(1).loop_len(), len, "stereo nimmt auch auf");
    assert_eq!(
        sim.core.track(1).layer(0).expect("Ebene").content(len).len() as u64,
        len * 2,
        "und zwar in einen Puffer der doppelten Laenge"
    );
}

/// A stereo track really does cost twice the memory of a mono one, and the pool hands out the
/// right length for each - a mono buffer given to a stereo track would be half a loop long.
#[test]
fn a_stereo_track_gets_buffers_of_twice_the_length() {
    let spec = SimSpec {
        bars: 1,
        layer_frames: Some(4_096),
        tracks: vec![TrackSpec::on(0), TrackSpec::stereo(0, 1)],
        input_channels: 2,
        loopback_channel: None,
        ..spec_4_4()
    };
    let mut sim = Sim::new(spec, mono(fingerprint));
    sim.run_blocks(4);

    let start = sim.pos() + BLOCK as u64;
    for track in 0..2 {
        sim.send(Command::StartRecord { track, at: start });
    }
    // No stop: both takes run into the end of their buffer, which is the frame count in both cases.
    sim.run_to(start + R + 12_000);
    assert_eq!(sim.core.track(0).loop_len(), 4_096, "mono: 4096 Frames");
    assert_eq!(sim.core.track(1).loop_len(), 4_096, "stereo: auch 4096 Frames");
    assert_eq!(
        sim.core.track(0).layer(0).expect("Ebene").content(4_096).len(),
        4_096,
        "mono: 4096 Samples"
    );
    assert_eq!(
        sim.core.track(1).layer(0).expect("Ebene").content(4_096).len(),
        8_192,
        "stereo: dieselben 4096 Frames sind 8192 Samples"
    );
}
