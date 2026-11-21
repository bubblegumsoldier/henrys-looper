//! Offline proofs for `render`. The recordings are synthetic and made the way the app makes them:
//! the score's runner driving the live engine, a played input on the device channels, the recorder
//! attached (`perform` with [`Source::Played`]). The render then has only the folder - dry stems,
//! `session.json`, `score.yaml` - and must play the same performance again. Everything lives in the
//! system's temporary directory; no installed plugin is loaded (the plugin test uses the
//! repository's own test VST3, `tools/test-vst3-gain`).

use std::path::{Path, PathBuf};

use super::perform::{Pass, PerformSpec, Source, perform, start_positions};
use super::plugins::PluginFinder;
use super::*;
use crate::capture::Layout;
use crate::capture::session::{dry_file, stem_file};
use crate::capture::wav::WavWriter;
use crate::engine::frame::Channels;
use crate::engine::insert::{Insert, InsertPosition, InsertShared, MAX_BLOCK};
use crate::engine::timeline::{TimeSignature, Timeline};

const RATE: u32 = 48_000;
const R: u64 = 600;
const BLOCK: usize = 128;

fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "henrys-looper-render-{name}-{}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// Three tracks, two on one input: a `monitor: always` guitar that gives way to the bass take
/// (the hand-off), a voice heard through for one section, `monitor_fade` on everything.
fn score_yaml(mixer: &str) -> String {
    format!(
        "title: Render test\nbpm: 120\nmonitor_fade: {{in: 2}}\ntracks:\n  guitar: {{input: 1, monitor: always}}\n  bass: {{input: 1}}\n  voice: {{input: 2, pan: -0.3}}\nsections:\n  - {{id: a, bars: 2, autorelease: true, tracks: {{guitar: record}}}}\n  - {{id: b, bars: 2, autorelease: true, tracks: {{guitar: play, bass: record}}}}\n  - {{id: c, bars: 2, autorelease: true, tracks: {{guitar: play, bass: play, voice: hear_through}}}}\n  - {{id: d, bars: 2, autorelease: true, tracks: {{guitar: play, bass: play}}}}\n  - {{id: e, bars: 1, autorelease: false}}\n{mixer}"
    )
}

const MIXER: &str = "mixer:\n  tracks:\n    guitar: {gain: 2, preset: piezo_guitar}\n    bass: {preset: bass_from_guitar, comp: {on: true, makeup: 6}}\n    voice: {gain: -3, preset: voice}\n";

/// A phrase with notes and pauses, on the 24-bit grid (so a dry file holds exactly what the engine
/// heard), between `from` and `to`.
fn phrase(k: u64, from: u64, to: u64, base: f64) -> f32 {
    if k < from || k >= to {
        return 0.0;
    }
    let t = (k - from) as f64 / RATE as f64;
    let note = (t * 4.0) as u64;
    let within = t * 4.0 - note as f64;
    let freq = base * [1.0, 1.26, 1.5, 1.68][(note % 4) as usize];
    let env = (-(within * 5.0)).exp() * if note % 3 == 2 { 0.15 } else { 0.5 };
    let x = (env * (2.0 * std::f64::consts::PI * freq * t).sin()) as f32;
    to_i24(x) as f32 / 8_388_607.0
}

fn spec_for(yaml: &str, frames: u64) -> PerformSpec {
    let score = compile_score(yaml).unwrap();
    let n = score.tracks.len();
    PerformSpec {
        score,
        sample_rate: RATE,
        frames,
        first_abs: 58_935_464,
        count_in_bars: 2,
        latency: vec![R; n],
        block: BLOCK,
        outputs: 2,
        monitor_gain: 1.0,
    }
}

struct Made {
    folder: PathBuf,
    /// File frames where each section starts.
    sections: Vec<u64>,
}

/// Perform the score with a played input - guitar on input 1 from bar 1 on, the voice only inside
/// section `c` - and write the folder the app's recorder would write.
fn make_recording(dir: &Path, yaml: &str, live_inserts: Vec<Vec<(usize, Insert)>>) -> Made {
    let tl = Timeline::new(RATE, 120.0, TimeSignature::new(4, 4));
    let bar = tl.bar_start(1) - tl.bar_start(0);
    let sections: Vec<u64> = [0, 2, 4, 6, 8].iter().map(|b| b * bar).collect();
    let frames = 9 * bar + 2 * u64::from(RATE);
    let spec = spec_for(yaml, frames);
    let (_, _, first) = start_positions(&spec).unwrap();
    let c = (sections[2], sections[3]);
    let played = move |k: u64, ch: usize| -> f32 {
        match ch {
            0 => phrase(k, first + R, u64::MAX, 110.0),
            1 => phrase(k, first + R + c.0 + 4_000, first + R + c.1 - 4_000, 220.0),
            _ => 0.0,
        }
    };
    let folder = dir.join("rec");
    std::fs::create_dir_all(&folder).unwrap();
    let names = ["guitar", "bass", "voice"];
    let layout = Layout {
        tracks: vec![Channels::Mono; 3],
        dry: true,
    };
    let mut stems: Vec<WavWriter> =
        (0..3).map(|t| WavWriter::create(&folder.join(stem_file(t, names[t])), 2, RATE).unwrap()).collect();
    let mut drys: Vec<WavWriter> =
        (0..3).map(|t| WavWriter::create(&folder.join(dry_file(t, names[t])), 1, RATE).unwrap()).collect();
    let mut mix = WavWriter::create(&folder.join("Mix.wav"), 2, RATE).unwrap();
    let mut sink = |_f: u64, sf: &[f32]| {
        mix.sample(sf[0]).unwrap();
        mix.sample(sf[1]).unwrap();
        for t in 0..3 {
            let so = layout.stem_offset(t);
            stems[t].sample(sf[so]).unwrap();
            stems[t].sample(sf[so + 1]).unwrap();
            drys[t].sample(sf[layout.dry_offset(t).unwrap()]).unwrap();
        }
    };
    let mut poll = || {};
    perform(
        &spec,
        Pass::Audio {
            source: Source::Played(&played),
            inserts: live_inserts,
            poll: &mut poll,
            sink: &mut sink,
        },
    )
    .unwrap();
    for w in stems.into_iter().chain(drys).chain([mix]) {
        w.finish().unwrap();
    }
    std::fs::write(folder.join("score.yaml"), yaml).unwrap();
    let tracks: Vec<serde_json::Value> = (0..3)
        .map(|t| {
            serde_json::json!({
                "number": t + 1, "name": names[t], "input_channels": 1,
                "stem": stem_file(t, names[t]), "dry": dry_file(t, names[t]), "midi": null,
                "latency_frames": R
            })
        })
        .collect();
    let markers: Vec<serde_json::Value> = ["a", "b", "c", "d", "e"]
        .iter()
        .zip(&sections)
        .map(|(id, f)| serde_json::json!({"frame": f, "section": id}))
        .collect();
    let session = serde_json::json!({
        "format": "henrys-looper-recording", "version": 1, "title": "Render test",
        "sample_rate": RATE, "bit_depth": 24, "bpm": 120.0, "beats_per_bar": 4, "beat_unit": 4,
        "frames": frames, "engine_position": first, "count_in_bars": 2, "mix": "Mix.wav",
        "tracks": tracks, "markers": markers, "score_copy": "score.yaml"
    });
    std::fs::write(folder.join("session.json"), serde_json::to_string_pretty(&session).unwrap())
        .unwrap();
    Made { folder, sections }
}

fn opts(rec: &Path, out: PathBuf) -> RenderOpts {
    RenderOpts {
        recording: rec.to_path_buf(),
        out,
        compare: true,
        ..Default::default()
    }
}

#[test]
fn the_performance_played_again_gives_the_same_files() {
    let dir = scratch("same");
    let made = make_recording(&dir, &score_yaml(MIXER), Vec::new());
    let mut log = Vec::new();
    let report = render(&opts(&made.folder, dir.join("out")), None, &mut log).unwrap();
    let text = String::from_utf8_lossy(&log).to_string();
    assert_eq!(report.weight_mismatches, 0, "{text}");
    for tr in &report.tracks {
        // The input was found again: the dry stems played again are the recorded ones, to the
        // last step of the file. (One step where a weight below 1 divides the rounding of the
        // file - inside the fades.)
        let dry = tr.dry_check.unwrap();
        assert!(dry.max_lsb <= 1, "{}: dry {}\n{text}", tr.name, dry.text());
        // Everything after it - takes, loops, monitor fades, the hand-off, live passes one
        // roundtrip late - comes out as recorded. Bit-exact where nothing amplifies that one
        // step; behind a compressor and a reverb it stays under -80 dB.
        let r = tr.residual.unwrap();
        assert!(r.stem_dbfs > -60.0, "{}: something to compare", tr.name);
        assert!(
            r.bit_exact() || r.rel_db.unwrap() < -80.0,
            "{}: {}\n{text}",
            tr.name,
            r.text()
        );
        assert!(tr.level_flags.is_empty(), "{}: {:?}", tr.name, tr.level_flags);
    }
    let mix = report.mix_residual.unwrap();
    assert!(mix.bit_exact() || mix.rel_db.unwrap() < -80.0, "{}", mix.text());
    assert!(report.mix_level_flags.is_empty());
}

#[test]
fn a_missing_monitor_fade_is_caught_by_the_level_check() {
    // The class of error a render that ignores the live gains makes: the monitored inputs 20 dB
    // off. The stems still look alike as a whole - the windows say where they are not.
    let dir = scratch("level");
    let made = make_recording(&dir, &score_yaml(MIXER), Vec::new());
    let mut o = opts(&made.folder, dir.join("out"));
    o.monitor_db = -20.0;
    let mut log = Vec::new();
    let report = render(&o, None, &mut log).unwrap();
    let voice = report.tracks.iter().find(|t| t.name == "voice").unwrap();
    assert!(!voice.level_flags.is_empty(), "{}", String::from_utf8_lossy(&log));
    let c = made.sections[2] as f64 / RATE as f64;
    assert!(voice.level_flags.iter().all(|f| f.render_dbfs < f.recorded_dbfs));
    assert!(voice.level_flags.iter().any(|f| f.at >= c));
    assert!(String::from_utf8_lossy(&log).contains("differ by more than 3 dB"));
    assert!(level_flagged(-40.0, -66.0) && !level_flagged(-40.0, -42.0) && !level_flagged(-95.0, -120.0));
}

#[test]
fn an_edited_score_changes_only_what_it_changes() {
    let dir = scratch("edited");
    let made = make_recording(&dir, &score_yaml(MIXER), Vec::new());
    let edited = dir.join("edited.yaml");
    let less = MIXER.replace("voice: {gain: -3, preset: voice}", "voice: {gain: -3, preset: voice, reverb: {mix: 0.02}}");
    std::fs::write(&edited, score_yaml(&less)).unwrap();
    let mut o = opts(&made.folder, dir.join("out"));
    o.score = Some(edited);
    let mut log = Vec::new();
    let report = render(&o, None, &mut log).unwrap();
    for tr in &report.tracks {
        let r = tr.residual.unwrap();
        if tr.name == "voice" {
            assert!(r.rel_db.unwrap() > -40.0, "less reverb is audible: {}", r.text());
        } else {
            assert!(r.bit_exact() || r.rel_db.unwrap() < -80.0, "{}: {}", tr.name, r.text());
        }
    }
}

#[test]
fn stems_from_the_recording_are_copied_and_summed() {
    let dir = scratch("copy");
    let made = make_recording(&dir, &score_yaml(MIXER), Vec::new());
    let mut o = opts(&made.folder, dir.join("out"));
    o.stems_from_recording = vec!["bass".into()];
    let mut log = Vec::new();
    let report = render(&o, None, &mut log).unwrap();
    assert!(!report.tracks[1].rendered);
    let name = stem_file(1, "bass");
    assert_eq!(
        std::fs::read(dir.join("out").join(&name)).unwrap(),
        std::fs::read(made.folder.join(&name)).unwrap()
    );
    // The mix with the recorded stem in it: the same, up to the order of the additions and the
    // one-step rounding of the other tracks.
    let r = report.mix_residual.unwrap();
    assert!(r.rel_db.unwrap() < -80.0, "{}", r.text());
}

#[test]
fn a_broken_folder_is_refused_with_one_clear_sentence() {
    let dir = scratch("broken");
    let mut log = Vec::new();
    let err = render(&opts(&dir.join("nothing"), dir.join("out")), None, &mut log).err().unwrap();
    assert!(err.contains("is not a folder"), "{err}");
    std::fs::create_dir_all(dir.join("empty")).unwrap();
    let err = render(&opts(&dir.join("empty"), dir.join("out")), None, &mut log).err().unwrap();
    assert!(err.contains("session.json"), "{err}");

    let made = make_recording(&dir, &score_yaml(MIXER), Vec::new());
    let err = render(&opts(&made.folder, made.folder.join("render")), None, &mut log).err().unwrap();
    assert!(err.contains("never writes into a recording"), "{err}");
    let mut wrong = opts(&made.folder, dir.join("out"));
    wrong.only = vec!["vocals".into()];
    let err = render(&wrong, None, &mut log).err().unwrap();
    assert!(err.contains("no track \"vocals\""), "{err}");
    // A dry stem cut short.
    let dry = made.folder.join(dry_file(2, "voice"));
    let bytes = std::fs::read(&dry).unwrap();
    std::fs::write(&dry, &bytes[..bytes.len() - 3_000]).unwrap();
    let err = render(&opts(&made.folder, dir.join("out")), None, &mut log).err().unwrap();
    assert!(err.contains("cut short"), "{err}");
    // No score copy: the performance cannot be played again.
    std::fs::remove_file(made.folder.join("score.yaml")).unwrap();
    let err = render(&opts(&made.folder, dir.join("out")), None, &mut log).err().unwrap();
    assert!(err.contains("no score copy"), "{err}");
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

#[test]
fn a_skipped_plugin_keeps_its_latency_as_a_plain_delay() {
    let mut d = plugins::StandIn::new(3);
    use crate::engine::insert::InsertProcessor;
    let (mut l, mut r) = (vec![1.0, 2.0, 3.0, 4.0, 5.0], vec![-1.0, -2.0, -3.0, -4.0, -5.0]);
    d.process(&mut l, &mut r);
    assert_eq!(l, vec![0.0, 0.0, 0.0, 1.0, 2.0]);
    assert_eq!(r, vec![0.0, 0.0, 0.0, -1.0, -2.0]);
}

/// The repository's test VST3 (gain, 64 frames of latency, state = the gain) on the voice, live
/// at gain 0.5. The render loads it offline, gives it the state from the settings file, runs it
/// at its place with its latency - and the files are the same. With the default state (gain 1)
/// they are not: the state really is restored.
#[cfg(target_os = "macos")]
#[test]
fn a_plugin_insert_is_loaded_offline_with_its_saved_state() {
    let Some(bundle) = crate::plugin::tests::fixture_bundle() else {
        return;
    };
    let info = crate::plugin::enumerate_vst3(&bundle).unwrap().remove(0);
    let mut live = crate::plugin::load(&info).unwrap();
    live.set_param(0, 0.5);
    let state = live.save_state().unwrap();
    let processor = live.activate(f64::from(RATE), MAX_BLOCK).unwrap();
    let shared = InsertShared::new(live.latency());
    let insert = Insert::new(processor, shared).at(InsertPosition::before(FxSlot::Comp));

    let dir = scratch("vst3");
    let mixer = MIXER.replace(
        "voice: {gain: -3, preset: voice}",
        &format!("voice: {{gain: -3, preset: voice, inserts: [{{plugin: \"{}\", before: comp}}]}}", info.id),
    );
    let made = make_recording(&dir, &score_yaml(&mixer), vec![Vec::new(), Vec::new(), vec![(0, insert)]]);
    live.deactivate();
    drop(live);
    let sidecar = |state: &[u8]| {
        format!(
            "{{\"version\": 1, \"tracks\": {{\"voice\": {{\"inserts\": [{{\"plugin_id\": \"{}\", \
             \"name\": \"Looper Test Gain\", \"slot\": 0, \"before\": \"comp\", \"state\": \"{}\"}}]}}}}}}",
            info.id,
            base64(state)
        )
    };
    std::fs::write(made.folder.join("score.plugins.json"), sidecar(&state)).unwrap();
    let mut log = Vec::new();
    let report = render(
        &opts(&made.folder, dir.join("with")),
        Some(PluginFinder::only(vec![info.clone()])),
        &mut log,
    )
    .unwrap();
    let text = String::from_utf8_lossy(&log).to_string();
    let voice = &report.tracks[2];
    assert_eq!(voice.inserts_applied.len(), 1, "{text}");
    assert!(voice.inserts_applied[0].contains("latency 64"), "{text}");
    assert!(voice.residual.unwrap().bit_exact(), "{}\n{text}", voice.residual.unwrap().text());

    // The plugin's default state instead of the saved one: gain 1, audibly different.
    let other = dir.join("default.plugins.json");
    std::fs::write(&other, "{\"version\": 1, \"tracks\": {}}").unwrap();
    let mut o = opts(&made.folder, dir.join("default"));
    o.plugins = Some(other);
    let report = render(&o, Some(PluginFinder::only(vec![info])), &mut log).unwrap();
    let voice = &report.tracks[2];
    assert!(voice.inserts_applied[0].contains("default state"));
    assert!(voice.residual.unwrap().rel_db.unwrap() > -20.0);
}
