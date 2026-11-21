//! Proofs for the octaver: the octave is really one octave down, at a sane level, without clicks,
//! NaNs or denormals; attacks arrive after the latency it reports; and the engine reads the loop
//! early by exactly that latency, so a loop played through it stays on the grid.

use super::octaver::{Octaver, OctaverSettings};
use super::{Chain, FxParam, FxPreset, FxSlot};
use crate::engine::command::Command;
use crate::engine::frame::Frame;
use crate::engine::sim::{Sim, SimSpec, TrackSpec, mono};
use crate::engine::timeline::{TimeSignature, Timeline};

const RATE: u32 = 48_000;
const LOW_E: f32 = 82.41;

fn sine(hz: f32, seconds: f32, amp: f32) -> Vec<f32> {
    let n = (seconds * RATE as f32) as usize;
    (0..n)
        .map(|i| amp * (std::f32::consts::TAU * hz * i as f32 / RATE as f32).sin())
        .collect()
}

/// A plucked string: Karplus-Strong. Harmonically rich at the attack, darker and quieter as it
/// rings - which is what a pick on a steel string does. The noise burst is a fixed pseudo-random
/// sequence, so the test is deterministic. The loop is `len` samples plus the half sample of the
/// averaging filter, so the pitch is within a few cents of `hz`; the tests measure the input's
/// pitch anyway rather than trusting it.
fn pluck(hz: f32, seconds: f32, amp: f32, seed: u32) -> Vec<f32> {
    let len = (RATE as f32 / hz - 0.5).round().max(2.0) as usize;
    let mut state = seed.wrapping_mul(2_654_435_761).max(1);
    let mut line: Vec<f32> = (0..len)
        .map(|_| {
            state ^= state << 13;
            state ^= state >> 17;
            state ^= state << 5;
            (state as f32 / u32::MAX as f32) * 2.0 - 1.0
        })
        .collect();
    let n = (seconds * RATE as f32) as usize;
    let mut out = Vec::with_capacity(n);
    for i in 0..n {
        let a = line[i % len];
        let b = line[(i + 1) % len];
        out.push(a);
        line[i % len] = 0.996 * 0.5 * (a + b);
    }
    let peak = out.iter().fold(0.0f32, |m, v| m.max(v.abs())).max(1.0e-9);
    out.iter().map(|v| v * amp / peak).collect()
}

fn run(oct: &mut Octaver, input: &[f32]) -> Vec<f32> {
    input.iter().map(|&x| oct.process(Frame::mono(x)).l).collect()
}

fn octave_only() -> Octaver {
    let mut o = Octaver::new(RATE);
    o.set_settings(OctaverSettings {
        dry: 0.0,
        sub: 1.0,
        tone_hz: 8_000.0,
        drive: 0.0,
    });
    o.reset();
    o
}

fn rms(x: &[f32]) -> f32 {
    (x.iter().map(|v| v * v).sum::<f32>() / x.len().max(1) as f32).sqrt()
}

/// Magnitude of one frequency, Hann-windowed.
fn level_at(x: &[f32], hz: f32) -> f32 {
    let n = x.len() as f32;
    let (mut re, mut im) = (0.0f64, 0.0f64);
    for (i, &v) in x.iter().enumerate() {
        let w = 0.5 - 0.5 * (std::f32::consts::TAU * i as f32 / n).cos();
        let ph = std::f64::consts::TAU * hz as f64 * i as f64 / RATE as f64;
        re += (v * w) as f64 * ph.cos();
        im += (v * w) as f64 * ph.sin();
    }
    ((re * re + im * im).sqrt() / n as f64) as f32
}

/// Fundamental by the YIN difference function: the first dip of the normalised difference under
/// 0.15, refined by a parabola. Searched between 25 and 1000 Hz.
fn fundamental(x: &[f32]) -> f32 {
    let tmin = (RATE / 1000) as usize;
    let tmax = (RATE / 25) as usize;
    let w = x.len() - tmax - 2;
    let mut d = vec![0.0f64; tmax + 2];
    for (tau, slot) in d.iter_mut().enumerate().skip(1) {
        *slot = (0..w)
            .map(|i| {
                let e = (x[i] - x[i + tau]) as f64;
                e * e
            })
            .sum();
    }
    let mut cm = vec![1.0f64; tmax + 2];
    let mut run = 0.0;
    for tau in 1..tmax + 2 {
        run += d[tau];
        cm[tau] = if run > 0.0 { d[tau] * tau as f64 / run } else { 1.0 };
    }
    let mut tau = tmin;
    while tau < tmax && cm[tau] >= 0.15 {
        tau += 1;
    }
    while tau < tmax && cm[tau + 1] < cm[tau] {
        tau += 1;
    }
    let (a, b, c) = (cm[tau - 1], cm[tau], cm[tau + 1]);
    let den = a - 2.0 * b + c;
    let shift = if den.abs() > 1e-12 { 0.5 * (a - c) / den } else { 0.0 };
    RATE as f32 / (tau as f64 + shift) as f32
}

fn assert_clean(y: &[f32], what: &str) {
    for (i, v) in y.iter().enumerate() {
        assert!(v.is_finite(), "{what}: sample {i} is {v}");
        assert!(!v.is_subnormal(), "{what}: sample {i} is denormal ({v:e})");
    }
}

#[test]
fn a_sine_comes_out_one_octave_lower() {
    for hz in [LOW_E, 110.0, 196.0, 329.6, 659.3] {
        let mut o = octave_only();
        let y = run(&mut o, &sine(hz, 1.5, 0.5));
        assert_clean(&y, "Sinus");
        let tail = &y[RATE as usize / 2..];
        let f = fundamental(tail);
        assert!(
            (f / (hz / 2.0) - 1.0).abs() < 0.005,
            "{hz} Hz: Grundton {f:.2} Hz statt {:.2}",
            hz / 2.0
        );
        // The octave dominates: nothing of the original pitch and no splice products near it.
        let sub = level_at(tail, hz / 2.0);
        let orig = level_at(tail, hz);
        assert!(
            20.0 * (sub / orig.max(1e-9)).log10() > 30.0,
            "{hz} Hz: Oktave nur {:.1} dB ueber dem Original",
            20.0 * (sub / orig.max(1e-9)).log10()
        );
        // Half speed keeps the amplitude.
        let ratio = rms(tail) / (0.5 / std::f32::consts::SQRT_2);
        assert!((0.85..1.1).contains(&ratio), "{hz} Hz: Pegel {ratio:.3} vom Eingang");
    }
}

#[test]
fn a_plucked_string_comes_out_one_octave_lower() {
    for (i, hz) in [LOW_E, 98.0, 146.8, 220.0, 392.0].into_iter().enumerate() {
        let input = pluck(hz, 1.5, 0.6, i as u32 + 1);
        let mut o = octave_only();
        let y = run(&mut o, &input);
        assert_clean(&y, "Saite");
        // A window into the ringing note, well after the attack.
        let from = RATE as usize / 5;
        let to = from + RATE as usize / 2;
        let f_in = fundamental(&input[from..to]);
        let f = fundamental(&y[from..to]);
        assert!(
            (f / (f_in / 2.0) - 1.0).abs() < 0.01,
            "{hz} Hz: Grundton {f:.2} Hz statt {:.2}",
            f_in / 2.0
        );
        let lin = rms(&y[from..to]) / rms(&input[from..to]);
        assert!(
            (0.7..1.4).contains(&lin),
            "{hz} Hz: Pegel {:.1} dB gegen den Eingang",
            20.0 * lin.log10()
        );
    }
}

/// A bass line of four notes, the guitar's low E among them: every note is tracked, and the
/// change from one to the next does not leave the old pitch hanging.
#[test]
fn note_changes_are_followed() {
    let notes = [LOW_E, 110.0, 146.8, 98.0, LOW_E];
    let len = 0.4;
    let mut input = Vec::new();
    for (i, &hz) in notes.iter().enumerate() {
        input.extend(pluck(hz, len, 0.5, 10 + i as u32));
    }
    let mut o = octave_only();
    let y = run(&mut o, &input);
    assert_clean(&y, "Tonfolge");
    let note = (len * RATE as f32) as usize;
    for (i, &hz) in notes.iter().enumerate() {
        // From 60 ms after the attack (latency plus the attack crossfade, with room to spare) to
        // the end of the note.
        let from = i * note + RATE as usize * 6 / 100;
        let to = (i + 1) * note;
        let f_in = fundamental(&input[from..to]);
        let f = fundamental(&y[from..to]);
        assert!(
            (f / (f_in / 2.0) - 1.0).abs() < 0.01,
            "Note {i} ({hz} Hz): Grundton {f:.2} Hz statt {:.2}",
            f_in / 2.0
        );
    }
}

/// The attack of a note after silence arrives [`Octaver::latency`] frames late, within a
/// millisecond - that is the number the engine compensates for.
#[test]
fn the_attack_arrives_after_the_reported_latency() {
    for hz in [LOW_E, 196.0] {
        let mut input = vec![0.0f32; RATE as usize / 4];
        let onset = input.len();
        input.extend(pluck(hz, 0.5, 0.5, 3));
        // Octave and dry share the latency: check both.
        for (dry, sub) in [(0.0, 1.0), (1.0, 0.0)] {
            let mut o = Octaver::new(RATE);
            o.set_settings(OctaverSettings {
                dry,
                sub,
                tone_hz: 8_000.0,
                drive: 0.0,
            });
            o.reset();
            let latency = o.latency() as usize;
            let y = run(&mut o, &input);
            let peak = y.iter().fold(0.0f32, |m, v| m.max(v.abs()));
            let first = y.iter().position(|v| v.abs() > 0.05 * peak).expect("kein Ausgang");
            let arrived = first as i64 - onset as i64;
            assert!(
                (arrived - latency as i64).abs() <= (RATE / 1000) as i64,
                "{hz} Hz, dry {dry}: Einsatz nach {arrived} Frames, gemeldet {latency}"
            );
        }
    }
}

#[test]
fn silence_stays_silent_and_a_tail_dies_to_exact_zero() {
    let mut o = octave_only();
    let y = run(&mut o, &vec![0.0; RATE as usize]);
    assert!(y.iter().all(|v| *v == 0.0), "Stille erzeugt Ausgang");

    let mut input = pluck(LOW_E, 0.5, 0.5, 7);
    input.extend(vec![0.0; 2 * RATE as usize]);
    let mut o = octave_only();
    o.set_settings(OctaverSettings {
        drive: 0.5,
        ..o.settings()
    });
    let y = run(&mut o, &input);
    assert_clean(&y, "Ausklang");
    let end = &y[y.len() - RATE as usize / 2..];
    assert!(end.iter().all(|v| v.abs() < 1.0e-6), "Ausklang endet nicht");
}

/// No discontinuity anywhere: the steepest step of the output is no steeper than the steepest
/// step a clean sine at the output pitch has. A splice that lands off the waveform, or a fade
/// that is too short, would show up as a step many times that size.
#[test]
fn no_clicks_on_a_held_note() {
    for hz in [LOW_E, 123.5, 220.0, 440.0] {
        let amp = 0.5;
        let mut o = octave_only();
        let y = run(&mut o, &sine(hz, 2.0, amp));
        let steady = &y[RATE as usize / 4..];
        let max_step = steady
            .windows(2)
            .map(|w| (w[1] - w[0]).abs())
            .fold(0.0f32, f32::max);
        let sine_step = amp * std::f32::consts::TAU * (hz / 2.0) / RATE as f32;
        assert!(
            max_step < 1.3 * sine_step,
            "{hz} Hz: groesster Sprung {max_step:.5}, ein Sinus macht {sine_step:.5}"
        );
    }
}

/// The chain processes frame by frame, so any block size must give the same samples; this proves
/// it for the octaver inside a whole chain, with sizes that do not divide each other.
#[test]
fn block_boundaries_do_not_matter() {
    let input = {
        let mut v = pluck(LOW_E, 0.6, 0.5, 5);
        v.extend(pluck(130.8, 0.6, 0.5, 6));
        v
    };
    let render = |block: usize| {
        let mut chain = Chain::new(RATE);
        chain.load_preset(FxPreset::BassFromGuitar);
        let mut out = Vec::with_capacity(input.len());
        for chunk in input.chunks(block) {
            for &x in chunk {
                out.push(chain.process(Frame::mono(x)));
            }
        }
        out
    };
    let a = render(64);
    let b = render(441);
    assert_eq!(a, b);
    let l: Vec<f32> = a.iter().map(|f| f.l).collect();
    assert_clean(&l, "Kette");
}

/// Two strings at once and a full chord: not pretty, and not required to be - but finite and no
/// louder than what went in.
#[test]
fn chords_do_not_blow_up() {
    let chord = [LOW_E, 110.0, 146.8, 196.0, 246.9, 329.6];
    for strings in [&chord[..2], &chord[..]] {
        let mut input = vec![0.0f32; (1.5 * RATE as f32) as usize];
        for (i, &hz) in strings.iter().enumerate() {
            for (s, v) in input.iter_mut().zip(pluck(hz, 1.5, 0.3, 20 + i as u32)) {
                *s += v;
            }
        }
        let peak_in = input.iter().fold(0.0f32, |m, v| m.max(v.abs()));
        let mut chain = Chain::new(RATE);
        chain.load_preset(FxPreset::BassFromGuitar);
        let y: Vec<f32> = input.iter().map(|&x| chain.process(Frame::mono(x)).l).collect();
        assert_clean(&y, "Akkord");
        let peak_out = y.iter().fold(0.0f32, |m, v| m.max(v.abs()));
        assert!(
            peak_out < 2.5 * peak_in.max(0.25),
            "{} Saiten: Spitze {peak_out:.2} bei {peak_in:.2} am Eingang",
            strings.len()
        );
    }
}

/// Out-of-range settings, from a hand-edited score or a MIDI knob, land inside the ranges.
#[test]
fn levels_and_tone_are_clamped() {
    let s = OctaverSettings {
        dry: 3.0,
        sub: -1.0,
        tone_hz: 50_000.0,
        drive: f32::NAN,
    }
    .clamped();
    assert_eq!((s.dry, s.sub, s.tone_hz, s.drive), (1.0, 0.0, 8_000.0, 0.0));
}

#[test]
fn the_chain_reports_the_octavers_latency_only_while_it_is_in_the_path() {
    let mut chain = Chain::new(RATE);
    assert_eq!(chain.latency(), 0, "trocken");
    chain.load_preset(FxPreset::BassFromGuitar);
    let l = chain.latency();
    assert!(l > 0 && l < RATE / 100, "Latenz {l} Frames");
    assert_eq!(chain.status().latency_frames(RATE), l);
    assert_eq!(super::octaver::latency_frames(RATE), l);
    chain.set_bypass(true);
    assert_eq!(chain.latency(), 0, "Bypass");
    chain.set_bypass(false);
    chain.set_enabled(FxSlot::Octaver, false);
    assert_eq!(chain.latency(), 0, "Oktaver aus");
    chain.load_preset(FxPreset::PiezoGuitar);
    assert_eq!(chain.latency(), 0, "Gitarre");
}

/// The octaver with the octave turned off and the dry signal on is a pure delay of its latency.
/// The engine reads the loop that much early, so from the second pass on the output is the same,
/// sample for sample, as without the octaver - the loop stays on the grid. The live input is not
/// compensated (it cannot be read ahead) and is late by the latency, like behind a plugin.
#[test]
fn the_octavers_latency_is_made_up_for_on_the_loop() {
    let fingerprint = |k: u64| ((k % 65_536) as f32) / 65_536.0 - 0.5;
    let timeline = Timeline::new(RATE, 100.0, TimeSignature::new(4, 4));
    let origin = timeline.bar_start(1);
    let len = timeline.span_bars(1, 1);
    let run = |octaver: bool| {
        let spec = SimSpec {
            bpm: 100.0,
            bars: 2,
            latency: 827,
            roundtrip: 827,
            loopback_channel: None,
            tracks: vec![TrackSpec::on(0)],
            ..Default::default()
        };
        let mut sim = Sim::new(spec, mono(fingerprint));
        if octaver {
            for cmd in [
                Command::SetFxBypass { track: 0, on: false },
                Command::SetFxEnabled {
                    track: 0,
                    slot: FxSlot::Octaver,
                    on: true,
                },
                Command::SetFxParam {
                    track: 0,
                    param: FxParam::OctaverSub(0.0),
                },
                Command::SetFxParam {
                    track: 0,
                    param: FxParam::OctaverDry(1.0),
                },
            ] {
                sim.send(cmd);
            }
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
    let plain = run(false);
    let delayed = run(true);
    let latency = u64::from(Octaver::new(RATE).latency());
    assert_eq!(delayed.core.track(0).playback_advance(), latency);
    let from = origin + 2 * len;
    let mut checked = 0;
    for pos in from..origin + 3 * len - 1 {
        let (a, b) = (plain.out(pos), delayed.out(pos));
        assert!(
            (a.l - b.l).abs() < 1.0e-6 && (a.r - b.r).abs() < 1.0e-6,
            "Position {pos}: {a:?} gegen {b:?}"
        );
        checked += 1;
    }
    assert!(checked > 100_000);
}
