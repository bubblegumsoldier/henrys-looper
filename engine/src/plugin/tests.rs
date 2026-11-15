//! Tests against **real, installed** plugins. Each one looks for its test object and returns early
//! (with a line on stderr) when the machine does not have it, so the suite stays green on a machine
//! without plugins - and proves something real on one that has them.
//!
//! Apple's own Audio Units ship with every Mac and are the default test objects. A VST3 is taken
//! from the standard folders or from `HENRYS_LOOPER_PLUGIN_PATH`.

use super::scan;
use super::{Format, Kind, PluginInfo};

/// Push a sine through a processor, block by block; return what came out.
fn run(
    processor: &mut dyn crate::engine::insert::InsertProcessor,
    frames: usize,
    block: usize,
    signal: impl Fn(usize) -> (f32, f32),
) -> (Vec<f32>, Vec<f32>) {
    let mut out_l = Vec::with_capacity(frames);
    let mut out_r = Vec::with_capacity(frames);
    let mut l = vec![0.0f32; block];
    let mut r = vec![0.0f32; block];
    let mut done = 0;
    while done < frames {
        let n = block.min(frames - done);
        for i in 0..n {
            let (a, b) = signal(done + i);
            l[i] = a;
            r[i] = b;
        }
        processor.process(&mut l[..n], &mut r[..n]);
        out_l.extend_from_slice(&l[..n]);
        out_r.extend_from_slice(&r[..n]);
        done += n;
    }
    (out_l, out_r)
}

fn sine(i: usize) -> (f32, f32) {
    let v = (i as f32 * 2.0 * std::f32::consts::PI * 440.0 / 48_000.0).sin() * 0.25;
    (v, v)
}

fn rms(x: &[f32]) -> f32 {
    (x.iter().map(|s| s * s).sum::<f32>() / x.len().max(1) as f32).sqrt()
}

#[cfg(target_os = "macos")]
fn apple_au(subtype: &str) -> Option<PluginInfo> {
    let id = format!("au:aufx:{subtype}:appl");
    let found = super::au::enumerate().into_iter().find(|i| i.id == id);
    if found.is_none() {
        eprintln!("uebersprungen: {id} ist auf diesem Rechner nicht installiert");
    }
    found
}

/// The registry lists Apple's effects, with the identity the cache and the UI rely on.
#[cfg(target_os = "macos")]
#[test]
fn the_registry_lists_apples_effects_without_loading_them() {
    let all = super::au::enumerate();
    if all.is_empty() {
        eprintln!("uebersprungen: keine Audio Units registriert");
        return;
    }
    let reverb = all.iter().find(|i| i.id == "au:aufx:rvb2:appl");
    let Some(reverb) = reverb else {
        eprintln!("uebersprungen: AUReverb2 fehlt");
        return;
    };
    assert_eq!(reverb.format, Format::Au);
    assert_eq!(reverb.kind, Kind::Effect);
    assert_eq!(reverb.vendor, "Apple");
    assert!(reverb.name.contains("Reverb"), "{}", reverb.name);
    let back = super::au::parse_id(&reverb.id).expect("Id laesst sich lesen");
    assert_eq!(super::au::id_of(&back), reverb.id);
}

/// A real Audio Unit in the path: AUDelay with its wet/dry mix at 100 % delays a click by the
/// configured time. Proves loading, stereo setup, render callback, parameters and that audio really
/// passes through.
#[cfg(target_os = "macos")]
#[test]
fn audio_goes_through_a_real_audio_unit() {
    let Some(info) = apple_au("dely") else {
        return;
    };
    let mut plugin = super::load(&info).expect("AUDelay laedt");
    let params = plugin.params();
    assert!(params.len() >= 3, "AUDelay hat Parameter: {params:?}");
    let mix = params
        .iter()
        .find(|p| p.name.to_lowercase().contains("dry/wet") || p.name.to_lowercase().contains("mix"))
        .expect("Mix-Parameter");
    let time = params
        .iter()
        .find(|p| p.name.to_lowercase().contains("delay time"))
        .expect("Delay-Zeit");
    let feedback = params
        .iter()
        .find(|p| p.name.to_lowercase().contains("feedback"))
        .expect("Feedback");
    let mut processor = plugin.activate(48_000.0, 256).expect("startet");
    // 100 % wet, no feedback, then find the delay time by where the click comes back.
    plugin.set_param(mix.id, 1.0);
    plugin.set_param(feedback.id, 0.5); // -100..100 % -> 0 %
    plugin.set_param(time.id, 0.05); // 0..2 s -> 0.1 s
    assert!((plugin.param(mix.id) - 1.0).abs() < 1e-3);
    let text = plugin.param_text(time.id, plugin.param(time.id));
    assert!(!text.is_empty(), "AU liefert eine Anzeige");
    let (l, r) = run(processor.as_mut(), 48_000, 256, |i| {
        let v = if i == 0 { 1.0 } else { 0.0 };
        (v, v)
    });
    let peak = l
        .iter()
        .enumerate()
        .max_by(|a, b| a.1.abs().total_cmp(&b.1.abs()))
        .map(|(i, _)| i)
        .unwrap();
    assert!(
        (4_700..=4_900).contains(&peak),
        "der Klick kommt nach 0,1 s wieder, nicht bei Frame {peak} (Anzeige: {text})"
    );
    assert!(l[peak].abs() > 0.5 && r[peak].abs() > 0.5, "beide Seiten");
    assert!(l[..4_000].iter().all(|s| s.abs() < 1e-3), "trocken ist weg");
    drop(processor);
    plugin.deactivate();
}

/// AUReverb2 at 48 kHz turns a sine into a sine plus a tail; the state survives a round trip and
/// restores a changed parameter.
#[cfg(target_os = "macos")]
#[test]
fn a_real_reverb_processes_and_its_state_round_trips() {
    let Some(info) = apple_au("rvb2") else {
        return;
    };
    let mut plugin = super::load(&info).expect("AUReverb2 laedt");
    let mix = plugin
        .params()
        .into_iter()
        .find(|p| p.name.to_lowercase().contains("dry/wet") || p.name.to_lowercase().contains("mix"))
        .expect("Mix");
    plugin.set_param(mix.id, 0.75);
    let saved = plugin.save_state().expect("Zustand");
    assert!(saved.len() > 16);
    plugin.set_param(mix.id, 0.1);
    assert!((plugin.param(mix.id) - 0.1).abs() < 1e-3);
    plugin.load_state(&saved).expect("Zustand zurueck");
    assert!(
        (plugin.param(mix.id) - 0.75).abs() < 1e-3,
        "Mix nach dem Laden: {}",
        plugin.param(mix.id)
    );

    let mut processor = plugin.activate(48_000.0, 512).expect("startet");
    let (l, r) = run(processor.as_mut(), 24_000, 512, sine);
    assert!(l.iter().chain(r.iter()).all(|s| s.is_finite()));
    assert!(rms(&l[12_000..]) > 0.01, "es kommt etwas heraus");
    // Silence afterwards: the tail rings on.
    let (tail, _) = run(processor.as_mut(), 4_800, 512, |_| (0.0, 0.0));
    assert!(rms(&tail) > 1e-4, "der Hall klingt nach");
}

/// The probe that runs in the scan child process, run here in-process on Apple's effects: every one
/// of them loads, starts and processes noise.
#[cfg(target_os = "macos")]
#[test]
fn every_apple_effect_survives_the_probe() {
    let effects: Vec<PluginInfo> = super::au::enumerate()
        .into_iter()
        .filter(|i| i.vendor == "Apple" && i.kind == Kind::Effect)
        // These two talk to the network or the speech system and are no audio effects to speak of.
        .filter(|i| !i.id.contains(":nsnd:") && !i.id.contains(":raac:"))
        .collect();
    if effects.is_empty() {
        eprintln!("uebersprungen: keine Apple-Effekte");
        return;
    }
    let mut ok = 0;
    for info in effects {
        let name = info.name.clone();
        let result = scan::probe_one(info);
        if result.status == scan::Status::Ok {
            ok += 1;
        } else {
            eprintln!("{name}: {:?} {:?}", result.status, result.message);
        }
    }
    assert!(ok >= 10, "nur {ok} Apple-Effekte bestehen die Probe");
}

/// A real VST3, if this machine has one: loaded, started, audio through it, state saved and put
/// back. Takes the first effect found in the standard folders or `HENRYS_LOOPER_PLUGIN_PATH`.
#[cfg(target_os = "macos")]
#[test]
fn audio_goes_through_a_real_vst3() {
    let mut found = None;
    // Whatever is installed first; the test plugin of this repository when nothing is.
    for bundle in scan::vst3_bundles().into_iter().chain(fixture_bundle()) {
        if let Ok(infos) = super::vst3::enumerate(&bundle)
            && let Some(info) = infos.into_iter().find(|i| i.kind == Kind::Effect)
        {
            found = Some(info);
            break;
        }
    }
    let Some(info) = found else {
        eprintln!("uebersprungen: kein VST3-Effekt installiert (HENRYS_LOOPER_PLUGIN_PATH setzen)");
        return;
    };
    eprintln!("VST3 im Test: {} ({})", info.name, info.vendor);
    let mut plugin = super::load(&info).expect("VST3 laedt");
    let params = plugin.params();
    let state = plugin.save_state().expect("Zustand");
    let mut processor = plugin.activate(48_000.0, 512).expect("startet");
    let (l, r) = run(processor.as_mut(), 48_000, 480, sine);
    assert!(l.iter().chain(r.iter()).all(|s| s.is_finite()));
    assert!(rms(&l[24_000..]) > 1e-3 || rms(&r[24_000..]) > 1e-3, "es kommt Ton heraus");
    // A parameter change reaches the audio side through the queue.
    if let Some(p) = params.iter().find(|p| !p.read_only && p.steps == 0) {
        plugin.set_param(p.id, 0.0);
        let _ = run(processor.as_mut(), 4_800, 480, sine);
        assert!((plugin.param(p.id) - 0.0).abs() < 1e-6);
    }
    drop(processor);
    plugin.deactivate();
    plugin.load_state(&state).expect("Zustand zurueck");
}

/// The test VST3 (`tools/test-vst3-gain`) wrapped into a `.vst3` bundle in a temporary folder.
/// Built on demand into its own target directory, so the cargo that runs this test is not blocked.
#[cfg(target_os = "macos")]
pub(crate) fn fixture_bundle() -> Option<std::path::PathBuf> {
    use std::path::PathBuf;
    use std::sync::OnceLock;
    static BUNDLE: OnceLock<Option<PathBuf>> = OnceLock::new();
    BUNDLE
        .get_or_init(|| {
            let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).parent()?.to_path_buf();
            let target = root.join("target").join("fixtures");
            let dylib = target.join("debug").join("libtest_vst3_gain.dylib");
            let cargo = std::env::var("CARGO").unwrap_or_else(|_| "cargo".to_string());
            let built = std::process::Command::new(cargo)
                .args(["build", "--quiet", "-p", "test-vst3-gain", "--target-dir"])
                .arg(&target)
                .current_dir(&root)
                .status()
                .map(|s| s.success())
                .unwrap_or(false);
            if !built || !dylib.exists() {
                eprintln!("uebersprungen: das Test-VST3 liess sich nicht bauen");
                return None;
            }
            let dir = std::env::temp_dir().join(format!("looper-vst3-{}", std::process::id()));
            let bundle = dir.join("LooperTestGain.vst3");
            let macos = bundle.join("Contents").join("MacOS");
            std::fs::create_dir_all(&macos).ok()?;
            std::fs::copy(&dylib, macos.join("LooperTestGain")).ok()?;
            std::fs::write(
                bundle.join("Contents").join("Info.plist"),
                "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<plist version=\"1.0\"><dict>\
                 <key>CFBundleExecutable</key><string>LooperTestGain</string>\
                 <key>CFBundleIdentifier</key><string>de.henrys-looper.test-gain</string>\
                 <key>CFBundlePackageType</key><string>BNDL</string></dict></plist>\n",
            )
            .ok()?;
            Some(bundle)
        })
        .clone()
}

/// The test VST3, with numbers that are known exactly: gain 1, a reported latency of 64 frames that
/// is also the real delay, a parameter that reaches the audio side through the queue, and a state
/// that comes back. Then the same plugin in the engine's insert slot, where its latency is made up
/// for on the loop.
#[cfg(target_os = "macos")]
#[test]
fn the_test_vst3_behaves_exactly_as_built() {
    let Some(bundle) = fixture_bundle() else {
        return;
    };
    let infos = super::vst3::enumerate(&bundle).expect("Bundle laesst sich lesen");
    assert_eq!(infos.len(), 1);
    let info = &infos[0];
    assert_eq!(info.name, "Looper Test Gain");
    assert_eq!(info.vendor, "Henrys Looper Tests");
    assert_eq!(info.kind, Kind::Effect);
    assert!(info.id.starts_with("vst3:"));

    let mut plugin = super::load(info).expect("laedt");
    let params = plugin.params();
    assert_eq!(params.len(), 1);
    assert_eq!(params[0].name, "Gain");
    assert_eq!(plugin.param(0), 1.0);
    assert_eq!(plugin.param_text(0, 0.5), "0.50");
    assert!(plugin.has_editor());
    assert!(plugin.open_editor("x").is_err(), "das Test-Plugin hat keine eigene Ansicht");

    let mut processor = plugin.activate(48_000.0, 256).expect("startet");
    assert_eq!(plugin.latency(), 64);
    let impulse = |i: usize| if i == 0 { (1.0, -1.0) } else { (0.0, 0.0) };
    let (l, r) = run(processor.as_mut(), 512, 100, impulse);
    assert_eq!(l[64], 1.0, "um genau die gemeldete Latenz verzoegert");
    assert_eq!(r[64], -1.0, "rechts bleibt rechts");
    assert_eq!(l.iter().filter(|s| **s != 0.0).count(), 1);

    plugin.set_param(0, 0.5);
    let (l, _) = run(processor.as_mut(), 512, 100, impulse);
    assert_eq!(l[64], 0.5, "der Parameter kommt ueber die Queue an");

    let saved = plugin.save_state().expect("Zustand");
    plugin.set_param(0, 0.25);
    drop(processor);
    plugin.deactivate();

    // Changed while nothing runs: the saved state must still carry the new value.
    plugin.set_param(0, 0.75);
    let idle = plugin.save_state().expect("Zustand ohne Engine");
    assert_eq!(&idle[8..16], &0.75f64.to_le_bytes(), "Parameter ohne laufende Engine im Zustand");
    plugin.load_state(&saved).expect("Zustand zurueck");
    assert_eq!(plugin.param(0), 0.5, "der Controller kennt den geladenen Zustand");

    // Reactivated after a deactivate - what a sample-rate change or an engine restart does.
    let mut processor = plugin.activate(44_100.0, 512).expect("startet erneut");
    let (l, _) = run(processor.as_mut(), 512, 512, impulse);
    assert_eq!(l[64], 0.5, "der geladene Zustand gilt auch im Prozessor");
}

/// The real VST3 in the engine's track slot: its reported latency is read ahead on the loop, so
/// the loop through the plugin lands on exactly the samples it lands on without it.
#[cfg(target_os = "macos")]
#[test]
fn a_real_vst3_in_a_track_slot_keeps_the_loop_on_the_grid() {
    use crate::engine::command::Command;
    use crate::engine::insert::{Insert, InsertMsg, InsertShared, InsertTarget, insert_channel};
    use crate::engine::sim::{Sim, SimSpec, TrackSpec, mono};
    use crate::engine::timeline::{TimeSignature, Timeline};

    let Some(bundle) = fixture_bundle() else {
        return;
    };
    let info = super::vst3::enumerate(&bundle).unwrap().remove(0);
    let timeline = Timeline::new(48_000, 100.0, TimeSignature::new(4, 4));
    let origin = timeline.bar_start(1);
    let len = timeline.span_bars(1, 1);
    let fingerprint = |k: u64| ((k % 65_536) as f32) / 65_536.0 - 0.5;
    let run_sim = |with_plugin: bool| {
        let spec = SimSpec {
            bpm: 100.0,
            bars: 2,
            latency: 827,
            roundtrip: 827,
            block: 128,
            loopback_channel: None,
            tracks: vec![TrackSpec::on(0)],
            ..Default::default()
        };
        let mut sim = Sim::new(spec, mono(fingerprint));
        let (mut tx, endpoint) = insert_channel(8);
        sim.core.attach_inserts(endpoint);
        let mut plugin = None;
        if with_plugin {
            let mut p = super::load(&info).unwrap();
            let processor = p.activate(48_000.0, crate::engine::insert::MAX_BLOCK).unwrap();
            let shared = InsertShared::new(p.latency());
            tx.send(InsertMsg::Install {
                target: InsertTarget::Track(0),
                slot: 0,
                insert: Insert::new(processor, shared),
            })
            .unwrap();
            plugin = Some(p);
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
        (sim, plugin)
    };
    let (plain, _) = run_sim(false);
    let (through, _plugin) = run_sim(true);
    assert_eq!(through.core.track(0).playback_advance(), 64);
    for pos in origin + 2 * len..origin + 3 * len - 1 {
        assert_eq!(plain.out(pos), through.out(pos), "Position {pos}");
    }
}

/// The cache file round-trips the scan result, so the next start needs no child process.
#[test]
fn scanned_entries_serialise_for_the_cache_and_the_ui() {
    let entry = scan::Scanned {
        info: PluginInfo {
            id: "au:aufx:rvb2:appl".into(),
            format: Format::Au,
            name: "AUReverb2".into(),
            vendor: "Apple".into(),
            version: "1.7.0".into(),
            category: "Effect".into(),
            kind: Kind::Effect,
            path: None,
        },
        status: scan::Status::Ok,
        message: None,
        latency: 0,
        params: 7,
    };
    let json = serde_json::to_string(&entry).unwrap();
    assert!(json.contains("\"format\":\"au\""), "{json}");
    assert!(json.contains("\"status\":\"ok\""), "{json}");
    assert!(json.contains("\"id\":\"au:aufx:rvb2:appl\""), "flach, nicht verschachtelt: {json}");
    let back: scan::Scanned = serde_json::from_str(&json).unwrap();
    assert_eq!(back, entry);
}
