//! Offline proofs for the recorder, driven through the real `EngineCore` in `Sim`.
//!
//! Everything is written into the system's temporary directory - never into a user's documents.

use std::path::{Path, PathBuf};

use super::run::{MIN_SECONDS, Mode, Outcome, RunInfo, RunTrack, SectionEvent, SectionWatch};
use super::session::{MIX_FILE, SESSION_FILE, dry_file, stem_file};
use super::wav::{WavWriter, parse_header, read_wav};
use super::*;
use crate::engine::command::Command;
use crate::engine::frame::Channels;
use crate::engine::process::loop_capacity;
use crate::engine::runner::Runner;
use crate::engine::sim::{Played, Sim, SimSpec, TrackSpec, mono};
use crate::engine::timeline::{TimeSignature, Timeline};
use crate::score::compile_score;

const RATE: u32 = 48_000;
const BLOCK: usize = 128;
const R: u64 = 827;

fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "henrys-looper-capture-{name}-{}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn info(dir: &Path, names: &[&str], channels: &[Channels], start: u64, timeline: &Timeline) -> RunInfo {
    RunInfo {
        title: "Test run".into(),
        mode: Mode::Free,
        folder: dir.join("run"),
        started_at: "2026-11-15 20:00:00".into(),
        sample_rate: RATE,
        bpm: timeline.bpm(),
        beats_per_bar: timeline.signature().beats_per_bar,
        beat_unit: timeline.signature().beat_unit,
        count_in_bars: 0,
        start,
        start_bar: timeline.bar_index_at(start),
        tracks: names
            .iter()
            .zip(channels)
            .map(|(n, c)| RunTrack {
                name: n.to_string(),
                channels: *c,
                latency_frames: R,
            })
            .collect(),
        dry: true,
        max_block: BLOCK,
        score_yaml: None,
        score_path: None,
        min_seconds: MIN_SECONDS,
        // The simulation runs many times faster than real time; the writer must not fall behind.
        queue_seconds: 60.0,
    }
}

/// Step the simulation until the recording's writer is done, at most `limit` frames further.
fn finish(sim: &mut Sim, rec: &mut run::Recording, limit: u64) -> Outcome {
    let end = sim.pos() + limit;
    while sim.pos() < end {
        sim.step();
        if rec.poll().is_some() {
            break;
        }
    }
    rec.wait().cloned().expect("the writer finished")
}

fn sine(freq: f64, amp: f32) -> impl Fn(u64) -> f32 {
    move |k| amp * (2.0 * std::f64::consts::PI * freq * k as f64 / RATE as f64).sin() as f32
}

fn spec(tracks: Vec<TrackSpec>, inputs: usize) -> SimSpec {
    SimSpec {
        bpm: 120.0,
        bars: 2,
        block: BLOCK,
        tracks,
        input_channels: inputs,
        loopback_channel: None,
        ..Default::default()
    }
}

fn timeline() -> Timeline {
    Timeline::new(RATE, 120.0, TimeSignature::new(4, 4))
}

#[test]
fn the_stems_add_up_to_the_mix_and_every_file_has_the_same_length() {
    let dir = scratch("sum");
    let tl = timeline();
    let a = sine(220.0, 0.3);
    let b = sine(330.0, 0.2);
    let played: Played = Box::new(move |k, ch| if ch == 0 { a(k) } else { b(k) });
    // Monitoring on and routed to both buses: on the two-output device both buses are one mix,
    // the live input really is in it, so nothing is added to a stem that the mix lacks.
    let mut sim = Sim::new(
        spec(
            vec![
                TrackSpec::on(0).monitoring(),
                TrackSpec::on(1).monitoring().panned(-0.5),
            ],
            2,
        ),
        played,
    );
    let (mut port, endpoint) = capture_channel();
    sim.core.attach_capture(endpoint);
    let bar = |n| tl.bar_start(n);
    for cmd in [
        Command::StartRecord { track: 0, at: bar(1) },
        Command::StopRecord { track: 0, at: bar(3) },
        Command::StartPlay { track: 0, at: bar(3) },
        Command::StartRecord { track: 1, at: bar(2) },
        Command::StopRecord { track: 1, at: bar(4) },
        Command::StartPlay { track: 1, at: bar(4) },
    ] {
        sim.send(cmd);
    }
    let mut rec = run::Recording::start(
        info(&dir, &["voice", "guitar"], &[Channels::Mono, Channels::Mono], bar(1), &tl),
        &mut port,
    )
    .unwrap();
    rec.note_take();
    sim.run_to(bar(6));
    rec.stop(bar(6));
    let outcome = finish(&mut sim, &mut rec, 20 * RATE as u64);
    assert!(outcome.kept, "{outcome:?}");
    assert_eq!(outcome.error, None);

    let folder = &outcome.folder;
    let (mix_info, mix) = read_wav(&folder.join(MIX_FILE)).unwrap();
    let (s0_info, s0) = read_wav(&folder.join(stem_file(0, "voice"))).unwrap();
    let (s1_info, s1) = read_wav(&folder.join(stem_file(1, "guitar"))).unwrap();
    let (d0_info, d0) = read_wav(&folder.join(dry_file(0, "voice"))).unwrap();
    let (d1_info, _) = read_wav(&folder.join(dry_file(1, "guitar"))).unwrap();
    assert_eq!(mix_info.channels, 2);
    assert_eq!(s0_info.channels, 2);
    assert_eq!(d0_info.channels, 1, "a mono track has a mono dry stem");
    assert_eq!(mix_info.bits, 24);
    assert_eq!(mix_info.sample_rate, RATE);
    for other in [&s0_info, &s1_info, &d0_info, &d1_info] {
        assert_eq!(other.frames, mix_info.frames, "every file has the same length");
    }
    assert_eq!(mix_info.frames, outcome.frames);
    // Stopped at bar 6, then the tail: loops still play, so it runs to its maximum.
    let expected = bar(6) - bar(1) + (MAX_TAIL_SECS * RATE as f64) as u64;
    assert!(
        mix_info.frames.abs_diff(expected) <= 1,
        "{} frames, expected {expected}",
        mix_info.frames
    );

    let mut loud = 0;
    for i in 0..mix.len() {
        let sum = s0[i] + s1[i];
        assert!(
            (sum - mix[i]).abs() < 1.0e-6,
            "sample {i}: stems {sum} vs mix {}",
            mix[i]
        );
        if mix[i].abs() > 0.01 {
            loud += 1;
        }
    }
    assert!(loud > mix.len() / 2, "the mix is not silence");

    // The dry stem is the raw input while recording: frame 0 of the files is bar 1, where the
    // take started, and the input there was played at input index bar(1) + R.
    let a = sine(220.0, 0.3);
    for i in [10usize, 1000, 50_000] {
        let expect = a(bar(1) + i as u64 + R);
        assert!((d0[i] - expect).abs() < 1.0e-6, "dry {i}: {} vs {expect}", d0[i]);
    }

    let json: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(folder.join(SESSION_FILE)).unwrap()).unwrap();
    assert_eq!(json["frames"], mix_info.frames);
    assert_eq!(json["tracks"][0]["stem"], "01 voice.wav");
    assert_eq!(json["tracks"][1]["dry"], "02 guitar (dry).wav");
    assert!(json["tracks"][0]["midi"].is_null());
    assert_eq!(json["bar_one_frame"], 0);
    assert_eq!(json["dropouts"]["overflow_incidents"], 0);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_live_pass_and_its_loop_playback_sit_on_the_same_sample() {
    let dir = scratch("align");
    let tl = timeline();
    let bar = |n| tl.bar_start(n);
    // One click played exactly 1000 frames into bar 1: it reaches the engine one roundtrip later.
    let played_at = bar(1) + 1000;
    let mut sim = Sim::new(
        // Monitoring off: the stem only gets the live pass through the latency-aligned complement.
        spec(vec![TrackSpec::on(0)], 1),
        mono(move |k| if k == played_at + R { 0.5 } else { 0.0 }),
    );
    let (mut port, endpoint) = capture_channel();
    sim.core.attach_capture(endpoint);
    for cmd in [
        Command::StartRecord { track: 0, at: bar(1) },
        Command::StopRecord { track: 0, at: bar(2) },
        Command::StartPlay { track: 0, at: bar(2) },
        Command::StopPlay { track: 0, at: bar(5) },
    ] {
        sim.send(cmd);
    }
    let mut rec =
        run::Recording::start(info(&dir, &["voice"], &[Channels::Mono], bar(1), &tl), &mut port)
            .unwrap();
    rec.note_take();
    sim.run_to(bar(5));
    rec.stop(bar(5));
    let outcome = finish(&mut sim, &mut rec, 20 * RATE as u64);
    assert!(outcome.kept);
    let folder = &outcome.folder;
    let (_, dry) = read_wav(&folder.join(dry_file(0, "voice"))).unwrap();
    let (_, stem) = read_wav(&folder.join(stem_file(0, "voice"))).unwrap();
    let (_, mix) = read_wav(&folder.join(MIX_FILE)).unwrap();
    let loop_len = (bar(2) - bar(1)) as usize;

    let hits = |v: &[f32], stride: usize| -> Vec<usize> {
        v.iter()
            .enumerate()
            .filter(|(_, x)| x.abs() > 0.1)
            .map(|(i, _)| i / stride)
            .collect()
    };
    // The live pass, then three passes of the loop - every one exactly on frame 1000 of its bar.
    let expected: Vec<usize> = (0..4).map(|n| 1000 + n * loop_len).collect();
    assert_eq!(hits(&dry, 1), expected, "dry stem: live pass and playback aligned");
    let mut stem_hits = hits(&stem, 2);
    stem_hits.dedup();
    assert_eq!(stem_hits, expected, "stem: live pass and playback aligned");
    assert!((dry[1000] - 0.5).abs() < 1.0e-6);
    assert!((stem[2000] - 0.5).abs() < 1.0e-6, "centre pan is unity");
    // The mix has only what the room heard: no monitoring, so no live pass - the documented
    // difference between a stem and the mix.
    let mut mix_hits = hits(&mix, 2);
    mix_hits.dedup();
    assert_eq!(mix_hits, expected[1..].to_vec());
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_full_queue_is_filled_with_silence_and_every_file_stays_in_step() {
    const SLOW_RATE: u32 = 1_000;
    let layout = Layout {
        tracks: vec![Channels::Mono],
        dry: false,
    };
    let (mut session, mut reader) = capture_pair(&CaptureSpec {
        start: 500,
        layout,
        sample_rate: SLOW_RATE,
        max_block: 64,
        max_latency: 0,
        queue_seconds: QUEUE_SECONDS,
    });
    session.install(0);
    let value = |pos: u64| ((pos % 997) as f32 + 1.0) / 2_000.0;
    let mut out = Vec::new();
    let mut pos = 0u64;
    let mut blocks = 0;
    loop {
        for i in 0..64 {
            session.mix_frame(pos + i, Frame::new(value(pos + i), -value(pos + i)));
        }
        pos += 64;
        let done = session.end_block(pos);
        blocks += 1;
        // The writer sleeps through the first 300 blocks - far longer than the queue holds.
        if blocks > 300 {
            reader.read(&mut out, 10_000);
        }
        if blocks == 500 {
            session.shared().request_stop(pos);
        }
        if done {
            break;
        }
        assert!(blocks < 5_000, "the capture never ended");
    }
    while reader.read(&mut out, 10_000) != Read::Finished {}
    let shared = reader.shared();
    assert!(shared.overflows() >= 1, "the queue did run full");
    assert!(shared.dropped() > 0);
    let width = reader.width();
    let frames = out.len() / width;
    assert_eq!(frames as u64, shared.frames(), "the gap was filled, not skipped");
    let mut silent = 0u64;
    for (i, frame) in out.chunks_exact(width).enumerate() {
        let pos = 500 + i as u64;
        if frame[0] == 0.0 {
            silent += 1;
            assert_eq!(frame[1], 0.0);
        } else {
            assert_eq!(frame[0], value(pos), "frame {i} is out of step");
            assert_eq!(frame[1], -value(pos));
        }
    }
    assert_eq!(silent, shared.dropped(), "exactly the dropped frames are silent");
}

#[test]
fn section_markers_land_on_the_bar_lines_of_the_score() {
    let yaml = "title: Marker test\nbpm: 120\ntime_signature: 4/4\ntracks:\n  voice: {input: 1}\n\
                sections:\n  - id: intro\n    bars: 2\n    autorelease: true\n    tracks:\n      voice: record\n\
                \x20 - id: verse\n    bars: 3\n    autorelease: true\n    tracks:\n      voice: play\n";
    let score = compile_score(yaml).expect("the score compiles");
    let tl = Timeline::new(RATE, score.bpm, score.signature());
    let dir = scratch("markers");
    let mut sim = Sim::new(
        SimSpec {
            bpm: score.bpm,
            signature: score.signature(),
            bars: 3,
            block: BLOCK,
            tracks: vec![TrackSpec::on(0)],
            input_channels: 1,
            loopback_channel: None,
            layer_frames: Some(loop_capacity(&tl, 3)),
            ..Default::default()
        },
        mono(sine(440.0, 0.25)),
    );
    let (mut port, endpoint) = capture_channel();
    sim.core.attach_capture(endpoint);
    let mut runner = Runner::new(score, tl, BLOCK as u32).unwrap();
    runner.start(0);
    // The start made its own moment bar 1 of the run: the count-in begins there, the
    // first section - frame 0 of the files - begins `count_in` bars later.
    let tl = runner.scheduler().timeline;
    let (first, first_bar) = runner.armed_at().unwrap();
    assert_eq!((tl.zero(), first_bar), (runner.start_lead(), u64::from(runner.count_in_bars())));
    let mut run_info = info(&dir, &["voice"], &[Channels::Mono], first, &tl);
    run_info.mode = Mode::Score;
    run_info.count_in_bars = 1;
    run_info.score_yaml = Some(yaml.to_string());
    let mut rec = run::Recording::start(run_info, &mut port).unwrap();
    let mut watch = SectionWatch::default();
    let mut events = Vec::new();
    let end = tl.bar_start(first_bar + 6);
    while sim.pos() < end {
        for cmd in runner.take_commands() {
            sim.send(cmd);
        }
        sim.run_blocks(8);
        watch.before(&runner);
        runner.tick(sim.pos());
        match watch.after(&runner, sim.pos()) {
            Some(SectionEvent::Section { at, at_bar, index, id }) => {
                rec.note_take();
                rec.marker(at, at_bar, &id, index);
                events.push((index, at));
            }
            Some(SectionEvent::End { at }) => {
                rec.stop(at);
                events.push((99, at));
            }
            None => {}
        }
    }
    let outcome = finish(&mut sim, &mut rec, 20 * RATE as u64);
    assert!(outcome.kept, "{outcome:?}");
    assert_eq!(
        events,
        vec![
            (0, first),
            (1, tl.bar_start(first_bar + 2)),
            (99, tl.bar_start(first_bar + 5))
        ]
    );
    let json: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(outcome.folder.join(SESSION_FILE)).unwrap(),
    )
    .unwrap();
    let markers = json["markers"].as_array().unwrap();
    assert_eq!(markers.len(), 2);
    assert_eq!(markers[0]["frame"], 0);
    assert_eq!(markers[0]["bar"], 1);
    assert_eq!(markers[0]["section"], "intro");
    assert_eq!(markers[1]["frame"], tl.bar_start(first_bar + 2) - first);
    assert_eq!(markers[1]["bar"], 3);
    assert_eq!(markers[1]["section"], "verse");
    assert_eq!(json["mode"], "score");
    assert_eq!(json["score_copy"], "score.yaml");
    assert_eq!(
        std::fs::read_to_string(outcome.folder.join("score.yaml")).unwrap(),
        yaml
    );
    // Ended at bar 6 of the files (after 5 bars) plus a tail that stops on silence: the loop
    // stopped with the score, so the tail is short.
    let five_bars = tl.bar_start(first_bar + 5) - first;
    assert!(outcome.frames >= five_bars);
    assert!(outcome.frames < five_bars + (MAX_TAIL_SECS * RATE as f64) as u64);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_short_run_or_one_without_a_take_is_thrown_away() {
    let dir = scratch("short");
    let tl = timeline();
    let mut sim = Sim::new(spec(vec![TrackSpec::on(0)], 1), mono(sine(440.0, 0.3)));
    let (mut port, endpoint) = capture_channel();
    sim.core.attach_capture(endpoint);
    let mut rec =
        run::Recording::start(info(&dir, &["voice"], &[Channels::Mono], 0, &tl), &mut port)
            .unwrap();
    rec.note_take();
    sim.run_to(RATE as u64 * 2);
    rec.stop(RATE as u64 * 2);
    let outcome = finish(&mut sim, &mut rec, 20 * RATE as u64);
    assert!(!outcome.kept);
    assert!(outcome.reason.unwrap().contains("shorter"));
    assert!(!outcome.folder.exists(), "the folder is gone");

    // Long enough, but nothing recorded.
    let mut rec =
        run::Recording::start(info(&dir, &["voice"], &[Channels::Mono], sim.pos(), &tl), &mut port)
            .unwrap();
    let stop = sim.pos() + RATE as u64 * 7;
    sim.run_to(stop);
    rec.stop(stop);
    let outcome = finish(&mut sim, &mut rec, 20 * RATE as u64);
    assert!(!outcome.kept);
    assert_eq!(outcome.reason.as_deref(), Some("nothing was recorded"));
    assert!(!outcome.folder.exists());
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn stopping_during_the_count_in_writes_nothing() {
    let dir = scratch("countin");
    let tl = timeline();
    let mut sim = Sim::new(spec(vec![TrackSpec::on(0)], 1), mono(sine(440.0, 0.3)));
    let (mut port, endpoint) = capture_channel();
    sim.core.attach_capture(endpoint);
    let start = tl.bar_start(2);
    let mut rec =
        run::Recording::start(info(&dir, &["voice"], &[Channels::Mono], start, &tl), &mut port)
            .unwrap();
    sim.run_to(tl.bar_start(1));
    rec.stop(tl.bar_start(1));
    let outcome = finish(&mut sim, &mut rec, 4 * RATE as u64);
    assert_eq!(outcome.frames, 0);
    assert!(!outcome.kept);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_header_written_mid_run_leaves_a_readable_file_and_rf64_takes_over_past_4_gb() {
    let dir = scratch("wav");
    let path = dir.join("a.wav");
    let mut w = WavWriter::create(&path, 2, RATE).unwrap();
    for i in 0..1000 {
        w.sample(i as f32 / 1000.0).unwrap();
        w.sample(-(i as f32) / 1000.0).unwrap();
    }
    w.update_header().unwrap();
    // As if the program died here: the file on disk is already a valid WAV of 1000 frames.
    let bytes = std::fs::read(&path).unwrap();
    let head = parse_header(&bytes).unwrap();
    assert_eq!(head.frames, 1000);
    assert!(!head.rf64);
    let (_, samples) = read_wav(&path).unwrap();
    assert!((samples[2 * 500] - 0.5).abs() < 1.0e-6);
    assert!((samples[2 * 500 + 1] + 0.5).abs() < 1.0e-6);

    w.pretend_data_bytes(5_000_000_000);
    w.update_header().unwrap();
    let bytes = std::fs::read(&path).unwrap();
    let head = parse_header(&bytes).unwrap();
    assert!(head.rf64, "past 4 GB the header is RF64");
    assert_eq!(head.frames, 5_000_000_000 / 6);
    assert_eq!(&bytes[12..16], b"ds64");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn names_are_safe_for_the_file_system() {
    assert_eq!(session::safe_name("a/b:c"), "a-b-c");
    assert_eq!(session::safe_name("  ..hidden "), "hidden");
    assert_eq!(session::safe_name(""), "untitled");
    assert_eq!(stem_file(0, "voice"), "01 voice.wav");
    assert_eq!(dry_file(11, "keys"), "12 keys (dry).wav");
    assert_eq!(
        session::folder_name("2026-11-15 20.31.07", "Mein/Stueck"),
        "2026-11-15 20.31.07 Mein-Stueck"
    );
}

/// With plugin inserts the engine renders block by block; the taps there must say the same: a
/// track insert is in the stem (and the stems still add up to the mix), a Main bus insert is in
/// the mix and in no stem.
#[test]
fn inserts_are_in_the_stem_and_bus_inserts_only_in_the_mix() {
    use crate::engine::bus::Bus;
    use crate::engine::insert::testing::Gain;
    use crate::engine::insert::{Insert, InsertMsg, InsertShared, InsertTarget, insert_channel};

    let dir = scratch("inserts");
    let tl = timeline();
    let bar = |n| tl.bar_start(n);
    let a = sine(220.0, 0.3);
    let b = sine(330.0, 0.2);
    let played: Played = Box::new(move |k, ch| if ch == 0 { a(k) } else { b(k) });
    let mut sim = Sim::new(
        spec(
            // Track 0 without monitoring: after its take the dry stem is the loop alone.
            vec![TrackSpec::on(0), TrackSpec::on(1).monitoring().panned(0.4)],
            2,
        ),
        played,
    );
    let (mut tx, endpoint) = insert_channel(8);
    sim.core.attach_inserts(endpoint);
    let mut install = |target, gain| {
        tx.send(InsertMsg::Install {
            target,
            slot: 0,
            insert: Insert::new(Box::new(Gain(gain)), InsertShared::new(0)),
        })
        .unwrap();
    };
    install(InsertTarget::Track(0), 0.5);
    install(InsertTarget::Bus(Bus::Main), 2.0);
    let (mut port, endpoint) = capture_channel();
    sim.core.attach_capture(endpoint);
    for cmd in [
        Command::StartRecord { track: 0, at: bar(1) },
        Command::StopRecord { track: 0, at: bar(2) },
        Command::StartPlay { track: 0, at: bar(2) },
        Command::StartRecord { track: 1, at: bar(1) },
        Command::StopRecord { track: 1, at: bar(2) },
        Command::StartPlay { track: 1, at: bar(2) },
    ] {
        sim.send(cmd);
    }
    let mut rec = run::Recording::start(
        info(&dir, &["a", "b"], &[Channels::Mono, Channels::Mono], bar(1), &tl),
        &mut port,
    )
    .unwrap();
    rec.note_take();
    sim.run_to(bar(4));
    rec.stop(bar(4));
    let outcome = finish(&mut sim, &mut rec, 20 * RATE as u64);
    assert!(outcome.kept);
    let (_, mix) = read_wav(&outcome.folder.join(MIX_FILE)).unwrap();
    let (_, s0) = read_wav(&outcome.folder.join(stem_file(0, "a"))).unwrap();
    let (_, s1) = read_wav(&outcome.folder.join(stem_file(1, "b"))).unwrap();
    let (_, d0) = read_wav(&outcome.folder.join(dry_file(0, "a"))).unwrap();
    let played_loop = (bar(2) - bar(1)) as usize;
    // The track insert halves the loop playback in the stem ...
    for i in [played_loop + 100, played_loop + 30_000] {
        assert!((s0[2 * i] - 0.5 * d0[i]).abs() < 1.0e-5, "{} vs {}", s0[2 * i], d0[i]);
        assert!(d0[i].abs() > 0.01);
    }
    // ... and the Main bus insert doubles the mix, not the stems. (Before bar 2 track 0's stem
    // carries its live pass, which the mix does not have - monitoring is off.)
    let mut loud = 0;
    for j in 2 * played_loop..mix.len().min(s0.len()) {
        let sum = s0[j] + s1[j];
        if sum.abs() > 0.4 {
            continue; // the doubled mix would clip there
        }
        assert!((2.0 * sum - mix[j]).abs() < 1.0e-5, "sample {j}: 2 x {sum} vs {}", mix[j]);
        if sum.abs() > 0.05 {
            loud += 1;
        }
    }
    assert!(loud > 1000);
    let _ = std::fs::remove_dir_all(&dir);
}
