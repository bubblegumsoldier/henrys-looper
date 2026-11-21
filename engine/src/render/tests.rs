//! Offline proofs for `render`: synthetic recordings made by the live engine (`Sim` with the
//! recorder attached), rendered again from their dry stems, compared with what the live engine put
//! into the stems. Everything lives in the system's temporary directory; no installed plugin is
//! loaded - the plugin test uses the repository's own test VST3 (`tools/test-vst3-gain`).

use std::path::{Path, PathBuf};

use super::plugins::PluginFinder;
use super::strip::{OfflineStrip, StripSpec};
use super::*;
use crate::capture::run::{self, MIN_SECONDS, Mode, RunInfo, RunTrack};
use crate::capture::session::{dry_file, stem_file};
use crate::capture::wav::read_wav;
use crate::capture::capture_channel;
use crate::engine::command::{Command, TrackStatus};
use crate::engine::insert::testing::Delay;
use crate::engine::insert::{Insert, InsertPosition, InsertShared};
use crate::engine::sim::{Played, Sim, SimSpec, TrackSpec};
use crate::engine::timeline::Timeline;
use crate::engine::fx::render::parse_entry;
use crate::score::mixer::{TrackMix, track_commands};

const RATE: u32 = 48_000;
const BLOCK: usize = 128;
/// The simulated roundtrip, which is also what the tracks compensate.
const R: u64 = 827;

fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "henrys-looper-render-{name}-{}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn timeline() -> Timeline {
    Timeline::new(RATE, 120.0, TimeSignature::new(4, 4))
}

/// A phrase with notes and pauses - enough to move a compressor and fill a reverb - between
/// `from` and `to` (input indices), silence elsewhere.
fn phrase(from: u64, to: u64) -> impl Fn(u64) -> f32 {
    move |k| {
        if k < from || k >= to {
            return 0.0;
        }
        let t = (k - from) as f64 / RATE as f64;
        let note = (t * 4.0) as u64; // four notes a second
        let within = t * 4.0 - note as f64;
        let freq = [196.0, 247.0, 294.0, 330.0][(note % 4) as usize];
        let env = (-(within * 6.0)).exp() * if note % 3 == 2 { 0.2 } else { 0.6 };
        let x = (env * (2.0 * std::f64::consts::PI * freq * t).sin()) as f32;
        // On the 24-bit grid already: the dry file then holds exactly the floats the live chain
        // heard. (A real input is not on it, which costs a real take a residual around -84 dB on
        // a compressed chain - see the module comment of `render`.)
        to_i24(x) as f32 / 8_388_607.0
    }
}

/// The score both the live run and the render use. Only `mixer:` and `pan` matter to the render.
fn score_yaml(mixer_entry: &str, pan: f32) -> String {
    format!(
        "title: Render test\nbpm: 120\ntracks:\n  voice: {{input: 1, pan: {pan}}}\nsections:\n  - {{id: a, bars: 2}}\n  - {{id: b, bars: 4}}\n  - {{id: c, bars: 4}}\nmixer:\n  tracks:\n    voice: {mixer_entry}\n"
    )
}

struct Made {
    folder: PathBuf,
    /// Musical positions of the take and of the loop playback, file frames.
    take: (u64, u64),
    playback: (u64, u64),
}

/// Record a synthetic performance with the live engine: one mono track whose chain the score's
/// mixer entry sets, the phrase played and monitored through Main during bars 3-4 (taken as a
/// loop), then - after twelve seconds of silence - the loop played back for two passes. Monitoring
/// stays on (the input is silent after the take, and `x + 0.0` is `x`). Bar 1 is file frame 0.
fn make_recording(dir: &Path, mixer_entry: &str, pan: f32) -> Made {
    make_recording_at(dir, mixer_entry, pan, 10)
}

/// The same, with the loop played from bar `play + 1` on.
fn make_recording_at(dir: &Path, mixer_entry: &str, pan: f32, play: u64) -> Made {
    let tl = timeline();
    // Zero-based bar indices: bar(0) is bar 1, file frame 0.
    let bar = |n: u64| tl.bar_start(n);
    let yaml = score_yaml(mixer_entry, pan);
    let score = compile_score(&yaml).unwrap();
    let entry = score.mixer.as_ref().unwrap().tracks.get("voice").unwrap().clone();
    let mix: TrackMix = entry.resolve();
    // Played only well inside the take, so the recorder's fades at its edges act on silence and
    // the dry stem holds exactly what the live chain heard, R frames earlier.
    let (take_from, take_to) = (bar(2), bar(4));
    let p = phrase(take_from + R + 4_000, take_to + R - 4_000);
    let played: Played = Box::new(move |k, ch| if ch == 0 { p(k) } else { 0.0 });
    let mut sim = Sim::new(
        SimSpec {
            bpm: 120.0,
            bars: 2,
            block: BLOCK,
            latency: R,
            roundtrip: R,
            tracks: vec![TrackSpec::on(0).monitoring().panned(pan)],
            input_channels: 1,
            loopback_channel: None,
            ..Default::default()
        },
        played,
    );
    for cmd in track_commands(0, &mix, &TrackStatus::default()) {
        sim.send(cmd);
    }
    let (mut port, endpoint) = capture_channel();
    sim.core.attach_capture(endpoint);
    let play_from = bar(play);
    let play_to = bar(play + 4);
    for cmd in [
        Command::StartRecord { track: 0, at: take_from },
        Command::StopRecord { track: 0, at: take_to },
        Command::StartPlay { track: 0, at: play_from },
        Command::StopPlay { track: 0, at: play_to },
    ] {
        sim.send(cmd);
    }
    let info = RunInfo {
        title: "Render test".into(),
        mode: Mode::Score,
        folder: dir.join("rec"),
        started_at: "2026-11-21 20:00:00".into(),
        sample_rate: RATE,
        bpm: 120.0,
        beats_per_bar: 4,
        beat_unit: 4,
        count_in_bars: 0,
        start: bar(0),
        start_bar: 0,
        tracks: vec![RunTrack {
            name: "voice".into(),
            channels: Channels::Mono,
            latency_frames: R,
        }],
        dry: true,
        max_block: BLOCK,
        score_yaml: Some(yaml),
        score_path: None,
        min_seconds: MIN_SECONDS,
        queue_seconds: 120.0,
    };
    let mut rec = run::Recording::start(info, &mut port).unwrap();
    rec.note_take();
    sim.run_to(bar(play + 7));
    rec.stop(bar(play + 7));
    let end = sim.pos() + 30 * RATE as u64;
    while sim.pos() < end {
        sim.step();
        if rec.poll().is_some() {
            break;
        }
    }
    let outcome = rec.wait().cloned().expect("the writer finished");
    assert!(outcome.kept, "{outcome:?}");
    Made {
        folder: outcome.folder.clone(),
        take: (take_from, take_to),
        playback: (play_from, play_to),
    }
}

fn read(path: &Path) -> Vec<f32> {
    read_wav(path).unwrap().1
}

fn quiet() -> Vec<u8> {
    Vec::new()
}

/// The voice chain with a tempo delay on top: high-pass, EQ, compressor, delay, reverb - every
/// stateful built-in effect except the octaver, which has its own test.
const VOICE: &str = "{gain: -2, preset: voice, delay: {on: true, mix: 0.3}}";

#[test]
fn a_rendered_dry_stem_is_the_live_stem_wherever_the_loop_played() {
    let dir = scratch("loop");
    let made = make_recording(&dir, VOICE, 0.3);
    let out = dir.join("out");
    let mut log = quiet();
    let report = render(
        &RenderOpts {
            recording: made.folder.clone(),
            out: out.clone(),
            compare: true,
            ..Default::default()
        },
        None,
        &mut log,
    )
    .unwrap();
    let text = String::from_utf8_lossy(&log).to_string();
    assert!(report.tracks[0].rendered, "{text}");

    let rendered = read(&out.join(stem_file(0, "voice")));
    let recorded = read(&made.folder.join(stem_file(0, "voice")));
    assert_eq!(rendered.len(), recorded.len());
    // The loop playback, from its first faded-in frame to the end of its last tail: the same file
    // contents, bit for bit.
    let (from, to) = (made.playback.0 - 2_000, made.playback.1 + 4 * RATE as u64);
    let loop_part = Residual::of(&rendered, &recorded, from, to);
    assert!(loop_part.stem_dbfs > -40.0, "the loop is audible: {loop_part:?}");
    assert!(loop_part.bit_exact(), "loop playback: {}\n{text}", loop_part.text());

    // The live pass: heard through Main R frames after it was played; the render puts it on the
    // grid. Shifted by R, it is the same signal.
    let (a, b) = made.take;
    let mut worst = 0.0f32;
    for i in (a as usize)..(b as usize + 2 * RATE as usize) {
        for c in 0..2 {
            let live = recorded[(i + R as usize) * 2 + c];
            let grid = rendered[i * 2 + c];
            worst = worst.max((live - grid).abs());
        }
    }
    assert!(worst <= 1.5 / 8_388_607.0, "live pass, shifted by R: worst {worst}");
    assert!(
        Residual::of(&rendered, &recorded, a, b).rel_db.unwrap() > -20.0,
        "unshifted, the live pass does not null - and must not be reported as if it did"
    );

    // Mix.wav is the sum of the stems, and with one track that is the stem.
    let mix = read(&out.join("Mix.wav"));
    assert_eq!(mix, rendered);
}

#[test]
fn an_edited_score_changes_only_what_it_changes() {
    let dir = scratch("edited");
    let made = make_recording(&dir, VOICE, 0.0);
    // The same mix with the reverb turned down.
    let edited = dir.join("edited.yaml");
    std::fs::write(
        &edited,
        score_yaml("{gain: -2, preset: voice, delay: {on: true, mix: 0.3}, reverb: {mix: 0.05}}", 0.0),
    )
    .unwrap();
    let mut log = quiet();
    render(
        &RenderOpts {
            recording: made.folder.clone(),
            out: dir.join("same"),
            ..Default::default()
        },
        None,
        &mut log,
    )
    .unwrap();
    render(
        &RenderOpts {
            recording: made.folder.clone(),
            out: dir.join("less"),
            score: Some(edited),
            ..Default::default()
        },
        None,
        &mut log,
    )
    .unwrap();
    let same = read(&dir.join("same").join(stem_file(0, "voice")));
    let less = read(&dir.join("less").join(stem_file(0, "voice")));
    let recorded = read(&made.folder.join(stem_file(0, "voice")));
    let (from, to) = (made.playback.0, made.playback.1);
    assert!(Residual::of(&same, &recorded, from, to).bit_exact());
    let changed = Residual::of(&less, &recorded, from, to);
    assert!(
        changed.rel_db.unwrap() > -40.0,
        "less reverb is audibly different: {}",
        changed.text()
    );
    // Before the first note both are silent.
    assert!(less[..(made.take.0 as usize * 2)].iter().all(|s| *s == 0.0));
}

#[test]
fn the_octaver_and_plugin_latency_are_read_ahead_as_live() {
    // An octaver that only delays (dry 1, sub 0) and a plugin that only delays: what comes out
    // must sit exactly where it went in, as the loop does live
    // (`the_octavers_latency_is_made_up_for_on_the_loop`).
    let entry = parse_entry("{octaver: {on: true, dry: 1, sub: 0}}").unwrap();
    let spec = StripSpec {
        sample_rate: RATE,
        bpm: 120.0,
        signature: TimeSignature::new(4, 4),
        channels: Channels::Mono,
        pan: 0.0,
        mix: entry.resolve(),
        block: BLOCK,
    };
    let mut strip = OfflineStrip::new(spec);
    let shared = InsertShared::new(64);
    strip
        .install(vec![(
            0,
            Insert::new(Box::new(Delay::new(64)), shared).at(InsertPosition::before(FxSlot::Comp)),
        )])
        .unwrap();
    strip.settle(RATE as u64);
    let ahead = strip.read_ahead();
    assert_eq!(ahead, 144 + 64, "octaver plus plugin");
    let mut dry = vec![0.0f32; 4_000];
    dry[1_000] = 0.5;
    let out = strip.render(&dry, 4_000, ahead);
    let peak = (0..4_000).max_by(|a, b| out[a * 2].abs().total_cmp(&out[b * 2].abs())).unwrap();
    assert_eq!(peak, 1_000, "on the grid");
    assert!((out[peak * 2] - 0.5).abs() < 1e-6 && (out[peak * 2 + 1] - 0.5).abs() < 1e-6);
}

#[test]
fn a_track_that_was_silent_for_longer_than_its_tail_comes_back_as_live() {
    // A long reverb, and a silence longer than its tail between the take and the playback: the
    // chain goes idle - and freezes what is left of the tail, about 60 dB down - in the render at
    // the same point of the signal as live. A render whose chain idled differently would bring
    // that frozen tail back differently, some 60 dB under the music.
    let dir = scratch("silent");
    let entry = "{preset: voice, reverb: {size: 0.9, mix: 0.4}}";
    let (chain, _) = chain_for(&parse_entry(entry).unwrap(), RATE);
    let gap_bars = u64::from(chain.tail_samples()) / u64::from(2 * RATE) + 2;
    let made = make_recording_at(&dir, entry, 0.0, 4 + gap_bars);
    let mut log = quiet();
    render(
        &RenderOpts {
            recording: made.folder.clone(),
            out: dir.join("out"),
            ..Default::default()
        },
        None,
        &mut log,
    )
    .unwrap();
    let rendered = read(&dir.join("out").join(stem_file(0, "voice")));
    let recorded = read(&made.folder.join(stem_file(0, "voice")));
    // The live pass and the whole silence after it, R frames apart: identical, idle point included.
    let mut worst = 0;
    for i in (made.take.0 as usize * 2)..((made.playback.0 as usize - 4_000) * 2) {
        worst = worst.max((to_i24(recorded[i + 2 * R as usize]) - to_i24(rendered[i])).abs());
    }
    assert_eq!(worst, 0, "live pass and silence");
    // The playback. Not bit-exact here, and not because of the engine: the loop fades in, the
    // fade's samples lie between the steps of a 24-bit file, the live chain heard them unrounded,
    // and a reverb this long carries the rounding on. What remains is far below a frozen tail.
    let r = Residual::of(&rendered, &recorded, made.playback.0 - 2_000, recorded.len() as u64 / 2);
    assert!(r.stem_dbfs > -40.0, "{}", r.text());
    assert!(r.rel_db.unwrap() < -80.0, "{}", r.text());
}

#[test]
fn a_broken_folder_is_refused_with_one_clear_sentence() {
    let dir = scratch("broken");
    let mut log = quiet();
    let opts = |rec: PathBuf| RenderOpts {
        recording: rec,
        out: dir.join("out"),
        ..Default::default()
    };
    let err = render(&opts(dir.join("nothing")), None, &mut log).err().unwrap();
    assert!(err.contains("is not a folder"), "{err}");
    std::fs::create_dir_all(dir.join("empty")).unwrap();
    let err = render(&opts(dir.join("empty")), None, &mut log).err().unwrap();
    assert!(err.contains("session.json"), "{err}");

    let made = make_recording(&dir, VOICE, 0.0);
    // Writing into the recording is refused.
    let mut inside = opts(made.folder.clone());
    inside.out = made.folder.join("render");
    let err = render(&inside, None, &mut log).err().unwrap();
    assert!(err.contains("never writes into a recording"), "{err}");
    // A dry stem cut short.
    let dry = made.folder.join(dry_file(0, "voice"));
    let bytes = std::fs::read(&dry).unwrap();
    std::fs::write(&dry, &bytes[..bytes.len() - 3_000]).unwrap();
    let err = render(&opts(made.folder.clone()), None, &mut log).err().unwrap();
    assert!(err.contains("cut short"), "{err}");
    // An unknown track name.
    let mut wrong = opts(made.folder.clone());
    wrong.stems_from_recording = vec!["vocals".into()];
    let err = render(&wrong, None, &mut log).err().unwrap();
    assert!(err.contains("no track \"vocals\""), "{err}");
}

#[test]
fn stems_from_the_recording_are_copied_and_summed() {
    let dir = scratch("copy");
    let made = make_recording(&dir, VOICE, 0.0);
    let mut log = quiet();
    let report = render(
        &RenderOpts {
            recording: made.folder.clone(),
            out: dir.join("out"),
            stems_from_recording: vec!["voice".into()],
            ..Default::default()
        },
        None,
        &mut log,
    )
    .unwrap();
    assert!(!report.tracks[0].rendered);
    let name = stem_file(0, "voice");
    assert_eq!(
        std::fs::read(dir.join("out").join(&name)).unwrap(),
        std::fs::read(made.folder.join(&name)).unwrap()
    );
}

// ---- plugins ---------------------------------------------------------------------------------

fn base64(data: &[u8]) -> String {
    const B64: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::new();
    for chunk in data.chunks(3) {
        let b = [chunk[0], *chunk.get(1).unwrap_or(&0), *chunk.get(2).unwrap_or(&0)];
        let n = (u32::from(b[0]) << 16) | (u32::from(b[1]) << 8) | u32::from(b[2]);
        for i in 0..4 {
            if i <= chunk.len() {
                out.push(B64[((n >> (18 - 6 * i)) & 63) as usize] as char);
            } else {
                out.push('=');
            }
        }
    }
    out
}

#[test]
fn base64_reads_what_the_app_writes() {
    for data in [&b""[..], b"a", b"ab", b"abc", b"\x00\xff\x10\x80hello"] {
        assert_eq!(plugins::base64_decode(&base64(data)).unwrap(), data);
    }
}

#[test]
fn states_are_matched_in_order_then_by_id_and_never_twice() {
    let side = |id: &str, state: &[u8]| plugins::SidePlugin {
        plugin_id: id.into(),
        name: String::new(),
        slot: None,
        bypass: false,
        before: None,
        state: Some(base64(state)),
    };
    let file = vec![side("b", b"B"), side("a", b"A1"), side("a", b"A2")];
    let got = plugins::match_states(&["a", "b", "a", "a", "c"], &file);
    assert_eq!(
        got,
        vec![Some(b"A1".to_vec()), Some(b"B".to_vec()), Some(b"A2".to_vec()), None, None]
    );
}

/// The repository's test VST3: gain parameter, 64 frames of latency, state = the gain. Loaded
/// offline, given the state from the settings file, run at its place in the chain with its latency
/// made up - and the result is exactly half of the render without it. Proves the state restore
/// (a default state would give gain 1) without any third-party plugin.
#[cfg(target_os = "macos")]
#[test]
fn a_plugin_insert_is_loaded_offline_with_its_saved_state() {
    let Some(bundle) = crate::plugin::tests::fixture_bundle() else {
        return;
    };
    let info = crate::plugin::enumerate_vst3(&bundle).unwrap().remove(0);
    // The state as the app would have saved it: gain 0.5.
    let mut plugin = crate::plugin::load(&info).unwrap();
    plugin.set_param(0, 0.5);
    let state = plugin.save_state().unwrap();
    drop(plugin);

    let dir = scratch("vst3");
    let entry = format!(
        "{{gain: -2, inserts: [{{plugin: \"{}\", before: octaver}}]}}",
        info.id
    );
    let made = make_recording(&dir, &entry, 0.0);
    let sidecar = dir.join("my score.plugins.json");
    std::fs::write(
        &sidecar,
        format!(
            "{{\"version\": 1, \"tracks\": {{\"voice\": {{\"inserts\": [{{\"plugin_id\": \"{}\", \
             \"name\": \"Looper Test Gain\", \"slot\": 0, \"before\": \"octaver\", \"state\": \"{}\"}}]}}}}}}",
            info.id,
            base64(&state)
        ),
    )
    .unwrap();
    let mut log = quiet();
    let with = render(
        &RenderOpts {
            recording: made.folder.clone(),
            out: dir.join("with"),
            plugins: Some(sidecar),
            ..Default::default()
        },
        Some(PluginFinder::only(vec![info.clone()])),
        &mut log,
    )
    .unwrap();
    let without = render(
        &RenderOpts {
            recording: made.folder.clone(),
            out: dir.join("without"),
            no_plugins: true,
            ..Default::default()
        },
        None,
        &mut log,
    )
    .unwrap();
    let text = String::from_utf8_lossy(&log).to_string();
    assert_eq!(with.tracks[0].read_ahead, 64, "{text}");
    assert_eq!(with.tracks[0].inserts_applied.len(), 1, "{text}");
    assert!(!with.tracks[0].inserts_applied[0].contains("default state"), "{text}");
    assert_eq!(without.tracks[0].inserts_skipped.len(), 1);
    assert!(text.contains("SKIPPED (--no-plugins)"), "{text}");

    // Through a chain that is bypassed (fader only), the plugin is a gain of 0.5 behind a delay of
    // 64 that the read-ahead takes back: exactly half, on the same frames.
    let a = read(&dir.join("with").join(stem_file(0, "voice")));
    let b = read(&dir.join("without").join(stem_file(0, "voice")));
    let mut worst = 0.0f32;
    for (x, y) in a.iter().zip(&b) {
        worst = worst.max((x - 0.5 * y).abs());
    }
    assert!(worst <= 1.0 / 8_388_607.0, "half, on the grid: worst {worst}");
    assert!(b.iter().any(|s| s.abs() > 0.01), "there is something to halve");
}
