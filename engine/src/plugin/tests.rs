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

/// A real VST3: loaded, started, audio through it, state saved and put back. The test plugin of
/// this repository; the installed ones only when `HENRYS_LOOPER_TEST_INSTALLED_VST3` is set.
///
/// Not the installed ones by default: enumerating a bundle loads it into the test process, and a
/// big commercial one (Kontakt 8) tears down its own Qt application in `exit()` and crashes there -
/// after every test has passed, with a macOS crash dialog for each run.
#[cfg(target_os = "macos")]
#[test]
fn audio_goes_through_a_real_vst3() {
    let mut found = None;
    let installed = if std::env::var_os("HENRYS_LOOPER_TEST_INSTALLED_VST3").is_some() {
        scan::vst3_bundles()
    } else {
        Vec::new()
    };
    for bundle in fixture_bundle().into_iter().chain(installed) {
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
        .get_or_init(|| build_fixture("test-vst3-gain", "LooperTestGain"))
        .clone()
}

/// The test instrument (`tools/test-vst3-sine`), bundled the same way.
#[cfg(target_os = "macos")]
pub(crate) fn sine_bundle() -> Option<std::path::PathBuf> {
    use std::path::PathBuf;
    use std::sync::OnceLock;
    static BUNDLE: OnceLock<Option<PathBuf>> = OnceLock::new();
    BUNDLE
        .get_or_init(|| build_fixture("test-vst3-sine", "LooperTestSine"))
        .clone()
}

#[cfg(target_os = "macos")]
fn build_fixture(package: &str, bundle_name: &str) -> Option<std::path::PathBuf> {
    use std::path::PathBuf;
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).parent()?.to_path_buf();
    let target = root.join("target").join("fixtures");
    let dylib = target
        .join("debug")
        .join(format!("lib{}.dylib", package.replace('-', "_")));
    let cargo = std::env::var("CARGO").unwrap_or_else(|_| "cargo".to_string());
    let built = std::process::Command::new(cargo)
        .args(["build", "--quiet", "-p", package, "--target-dir"])
        .arg(&target)
        .current_dir(&root)
        .status()
        .map(|s| s.success())
        .unwrap_or(false);
    if !built || !dylib.exists() {
        eprintln!("uebersprungen: das Test-VST3 {package} liess sich nicht bauen");
        return None;
    }
    let dir = std::env::temp_dir().join(format!("looper-vst3-{}", std::process::id()));
    let bundle = dir.join(format!("{bundle_name}.vst3"));
    let macos = bundle.join("Contents").join("MacOS");
    std::fs::create_dir_all(&macos).ok()?;
    std::fs::copy(&dylib, macos.join(bundle_name)).ok()?;
    std::fs::write(
        bundle.join("Contents").join("Info.plist"),
        format!(
            "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<plist version=\"1.0\"><dict>\
             <key>CFBundleExecutable</key><string>{bundle_name}</string>\
             <key>CFBundleIdentifier</key><string>de.henrys-looper.{package}</string>\
             <key>CFBundlePackageType</key><string>BNDL</string></dict></plist>\n"
        ),
    )
    .ok()?;
    Some(bundle)
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
    assert!(plugin.open_editor("x", "test").is_err(), "das Test-Plugin hat keine eigene Ansicht");

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

/// What the test VST3 read from its `ProcessContext` in one block, with its probe switched on
/// (see `tools/test-vst3-gain`): field `k` as an `f64`, split over `l[k]` and `r[k]`.
#[cfg(target_os = "macos")]
fn probe(
    processor: &mut dyn crate::engine::insert::InsertProcessor,
    transport: Option<crate::engine::insert::PluginTransport>,
    frames: usize,
) -> Probed {
    if let Some(t) = transport {
        processor.set_transport(&t);
    }
    let mut l = vec![0.0f32; frames];
    let mut r = vec![0.0f32; frames];
    processor.process(&mut l, &mut r);
    let field =
        |k: usize| f64::from_bits(u64::from(l[k].to_bits()) | u64::from(r[k].to_bits()) << 32);
    Probed {
        tempo: field(0),
        numerator: field(1),
        denominator: field(2),
        quarters: field(3),
        bar: field(4),
        samples: field(5),
        playing: field(6) == 1.0,
        valid: field(7) == 1.0,
        sample_rate: field(8),
        continuous: field(9),
        cycling: field(10) == 1.0,
        context: field(11) == 1.0,
    }
}

#[cfg(target_os = "macos")]
#[derive(Debug)]
struct Probed {
    tempo: f64,
    numerator: f64,
    denominator: f64,
    quarters: f64,
    bar: f64,
    samples: f64,
    playing: bool,
    valid: bool,
    sample_rate: f64,
    continuous: f64,
    cycling: bool,
    context: bool,
}

/// The test VST3 as a probe: what the engine's clock says is exactly what the plugin reads in its
/// `ProcessContext` - tempo, a 3/4 signature, the position in samples and quarters, the bar start,
/// playing or not - and between two calls that are not told anything the position moves on by
/// exactly the frames processed.
#[cfg(target_os = "macos")]
#[test]
fn the_test_vst3_reads_the_engines_clock_in_its_process_context() {
    use crate::engine::insert::PluginTransport;
    const PROBE_PARAM: u32 = 1;

    let Some(bundle) = fixture_bundle() else {
        return;
    };
    let info = super::vst3::enumerate(&bundle).unwrap().remove(0);
    let mut plugin = super::load(&info).expect("laedt");
    assert!(
        plugin.params().iter().all(|p| p.id != PROBE_PARAM),
        "der Messschalter ist verborgen"
    );
    plugin.set_param(PROBE_PARAM, 1.0);
    let mut processor = plugin.activate(48_000.0, 256).expect("startet");

    // Nobody told it anything yet (the scan's probe): a standing clock at 120, flagged valid.
    let p = probe(processor.as_mut(), None, 64);
    assert!(p.context, "es gibt einen ProcessContext");
    assert_eq!((p.tempo, p.samples, p.playing, p.valid), (120.0, 0.0, false, true));

    // 90 BPM in 3/4, standing: bar 1, not playing, the tempo valid all the same.
    let standing = PluginTransport {
        sample_rate: 48_000.0,
        bpm: 90.0,
        beats_per_bar: 3,
        beat_unit: 4,
        playing: false,
        position: 0,
        continuous: 1_000,
    };
    let p = probe(processor.as_mut(), Some(standing), 64);
    assert_eq!((p.tempo, p.numerator, p.denominator), (90.0, 3.0, 4.0));
    assert_eq!((p.quarters, p.bar, p.samples), (0.0, 0.0, 0.0));
    assert!(!p.playing && p.valid && !p.cycling);
    assert_eq!((p.sample_rate, p.continuous), (48_000.0, 1_000.0));
    // Standing, the next call stays where it is; only the continuous clock moves.
    let p = probe(processor.as_mut(), None, 64);
    assert_eq!((p.samples, p.continuous), (0.0, 1_064.0));

    // Started: position 0, playing; then it grows by exactly the frames processed.
    let started = PluginTransport {
        playing: true,
        position: 0,
        continuous: 5_000,
        ..standing
    };
    let p = probe(processor.as_mut(), Some(started), 100);
    assert!(p.playing);
    assert_eq!((p.samples, p.quarters), (0.0, 0.0));
    let p = probe(processor.as_mut(), None, 64);
    assert_eq!((p.samples, p.continuous), (100.0, 5_100.0), "sample-genau weitergezaehlt");
    assert!((p.quarters - 100.0 / 32_000.0).abs() < 1e-12);

    // In the second bar of 3/4: 3.125 quarters, the bar began at 3.
    let p = probe(
        processor.as_mut(),
        Some(PluginTransport {
            position: 100_000,
            continuous: 105_000,
            ..started
        }),
        64,
    );
    assert_eq!((p.samples, p.quarters, p.bar), (100_000.0, 3.125, 3.0));

    // A count-in: one bar before bar 1.
    let p = probe(
        processor.as_mut(),
        Some(PluginTransport {
            position: -32_000,
            ..started
        }),
        64,
    );
    assert_eq!((p.quarters, p.bar), (-1.0, -3.0));

    // Stopped again: bar 1, not playing, tempo still there.
    let p = probe(processor.as_mut(), Some(standing), 64);
    assert!(!p.playing);
    assert_eq!((p.samples, p.tempo), (0.0, 90.0));
}

/// Audio Units ask for the clock through host callbacks. Apple's units do not show what they read,
/// so this proves the part that can be proven: the unit took the callbacks (it hands them back),
/// and called the way a unit calls them, they answer with the clock the engine set for the block.
#[cfg(target_os = "macos")]
#[test]
fn an_audio_unit_gets_host_callbacks_that_answer_with_the_engines_clock() {
    use super::PluginInstance;
    use crate::engine::insert::PluginTransport;
    let Some(info) = apple_au("dely") else {
        return;
    };
    let mut plugin = super::au::open(&info).expect("AUDelay laedt");
    let mut processor = plugin.activate(48_000.0, 256).expect("startet");
    let cb = plugin.host_callbacks().expect("die Unit gibt die Callbacks zurueck");
    let user = cb.host_user_data;
    assert!(!user.is_null());
    let (beat_and_tempo, location, state, state2) = (
        cb.beat_and_tempo.expect("Beat und Tempo"),
        cb.musical_time_location.expect("Taktposition"),
        cb.transport_state.expect("Transport"),
        cb.transport_state2.expect("Transport 2"),
    );

    let t = PluginTransport {
        sample_rate: 48_000.0,
        bpm: 90.0,
        beats_per_bar: 3,
        beat_unit: 4,
        playing: true,
        position: 100_000,
        continuous: 105_000,
    };
    processor.set_transport(&t);
    let _ = run(processor.as_mut(), 256, 256, |_| (0.0, 0.0));

    let (mut beat, mut tempo) = (0.0, 0.0);
    assert_eq!(unsafe { beat_and_tempo(user, &mut beat, &mut tempo) }, 0);
    assert_eq!((beat, tempo), (3.125, 90.0), "Stand am Anfang des Blocks");

    let (mut next, mut num, mut den, mut downbeat) = (0u32, 0f32, 0u32, 0.0);
    assert_eq!(unsafe { location(user, &mut next, &mut num, &mut den, &mut downbeat) }, 0);
    assert_eq!((num, den, downbeat), (3.0, 4, 3.0));
    assert_eq!(next, 28_000, "bis zur naechsten Viertel: 4 * 32 000 - 100 000");

    let (mut playing, mut changed, mut sample, mut cycling) = (0u8, 0u8, 0.0, 1u8);
    let (mut cs, mut ce) = (1.0, 1.0);
    let st = unsafe {
        state(user, &mut playing, &mut changed, &mut sample, &mut cycling, &mut cs, &mut ce)
    };
    assert_eq!(st, 0);
    assert_eq!((playing, sample, cycling), (1, 100_000.0, 0));
    assert_eq!(changed, 1, "der Start ist eine Transport-Aenderung");
    let mut recording = 1u8;
    let st = unsafe {
        state2(
            user,
            &mut playing,
            &mut recording,
            &mut changed,
            &mut sample,
            &mut cycling,
            &mut cs,
            &mut ce,
        )
    };
    assert_eq!(st, 0);
    assert_eq!((recording, changed), (0, 0), "einmal gemeldet, dann nicht mehr");
    // Every out pointer may be null.
    let (null, none) = (std::ptr::null_mut::<u8>(), std::ptr::null_mut::<f64>());
    assert_eq!(unsafe { state2(user, null, null, null, none, null, none, none) }, 0);

    // The transport stops: position 0, not playing, the tempo still valid.
    processor.set_transport(&PluginTransport {
        playing: false,
        position: 0,
        ..t
    });
    let _ = run(processor.as_mut(), 256, 256, |_| (0.0, 0.0));
    assert_eq!(unsafe { beat_and_tempo(user, &mut beat, &mut tempo) }, 0);
    assert_eq!((beat, tempo), (0.0, 90.0));
    unsafe { state(user, &mut playing, &mut changed, &mut sample, null, none, none) };
    assert_eq!((playing, changed, sample), (0, 1, 0.0));
    unsafe { location(user, &mut next, &mut num, &mut den, &mut downbeat) };
    assert_eq!((next, downbeat), (0, 0.0));

    drop(processor);
    plugin.deactivate();
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

/// Apple's General MIDI synthesizer, the reference instrument: found as an instrument, it passes the
/// scan's probe, and a note played into it at an offset comes out as sound from that offset on -
/// silence before it, and silence before any note is played at all.
#[cfg(target_os = "macos")]
#[test]
fn apples_dls_synth_plays_a_note_at_its_offset() {
    use crate::engine::source::{BlockEvent, StereoOut};
    let Some(info) = super::au::enumerate()
        .into_iter()
        .find(|i| i.id == "au:aumu:dls :appl")
    else {
        eprintln!("uebersprungen: DLSMusicDevice ist nicht installiert");
        return;
    };
    assert_eq!(info.kind, Kind::Instrument);
    let probed = scan::probe_one(info.clone());
    assert_eq!(probed.status, scan::Status::Ok, "{:?}", probed.message);

    let mut plugin = super::load(&info).expect("laedt");
    let mut synth = plugin.activate_instrument(48_000.0, 512).expect("startet");
    let mut l = vec![0.0f32; 256];
    let mut r = vec![0.0f32; 256];
    let mut render = |events: &[BlockEvent], l: &mut [f32], r: &mut [f32]| {
        l.fill(0.0);
        r.fill(0.0);
        synth.render(events, &mut [StereoOut { l, r }]);
    };
    render(&[], &mut l, &mut r);
    assert_eq!(rms(&l), 0.0, "ohne Note still");
    render(
        &[BlockEvent {
            offset: 128,
            msg: [0x90, 60, 110],
        }],
        &mut l,
        &mut r,
    );
    assert!(l[..120].iter().all(|s| *s == 0.0), "vor dem Offset still");
    let mut energy = rms(&l[128..]);
    for _ in 0..8 {
        render(&[], &mut l, &mut r);
        energy = energy.max(rms(&l));
    }
    assert!(energy > 0.001, "die Note klingt: {energy}");
    assert!(l.iter().chain(r.iter()).all(|s| s.is_finite()));
    render(
        &[BlockEvent {
            offset: 0,
            msg: [0xB0, 123, 0],
        }],
        &mut l,
        &mut r,
    );
    drop(synth);
    plugin.deactivate();
}

/// The test instrument (`tools/test-vst3-sine`) with one note source, rendered block by block.
#[cfg(target_os = "macos")]
struct SineRig {
    plugin: Box<dyn super::PluginInstance>,
    synth: Option<Box<dyn crate::engine::source::SourceProcessor>>,
    l: Vec<f32>,
    r: Vec<f32>,
}

#[cfg(target_os = "macos")]
impl SineRig {
    fn new(name: &str, block: usize) -> Option<Self> {
        let bundle = sine_bundle()?;
        let info = super::vst3::enumerate(&bundle)
            .expect("Bundle laesst sich lesen")
            .into_iter()
            .find(|i| i.name == name)
            .expect("Klasse vorhanden");
        let mut plugin = super::load(&info).expect("laedt");
        let synth = plugin.activate_instrument(48_000.0, 512).expect("startet");
        Some(Self {
            plugin,
            synth: Some(synth),
            l: vec![0.0; block],
            r: vec![0.0; block],
        })
    }

    fn render(&mut self, events: &[(u32, [u8; 3])]) -> &[f32] {
        use crate::engine::source::{BlockEvent, StereoOut};
        let events: Vec<BlockEvent> = events
            .iter()
            .map(|&(offset, msg)| BlockEvent { offset, msg })
            .collect();
        self.l.fill(0.0);
        self.r.fill(0.0);
        self.synth.as_mut().unwrap().render(
            &events,
            &mut [StereoOut {
                l: &mut self.l,
                r: &mut self.r,
            }],
        );
        assert_eq!(self.l, self.r, "mono Synth, beide Seiten gleich");
        &self.l
    }
}

#[cfg(target_os = "macos")]
fn first_sound(x: &[f32]) -> Option<usize> {
    x.iter().position(|s| *s != 0.0)
}

#[cfg(target_os = "macos")]
fn peak(x: &[f32]) -> f32 {
    x.iter().fold(0.0f32, |m, s| m.max(s.abs()))
}

/// A VST3 instrument is found as one, passes the probe, and plays exactly: a note on at an offset
/// sounds from that sample (velocity scales it, the pitch is right), a note off at an offset ends it
/// on that sample, nothing sounds before any note.
#[cfg(target_os = "macos")]
#[test]
fn a_vst3_instrument_plays_notes_on_the_sample() {
    let Some(bundle) = sine_bundle() else {
        return;
    };
    let infos = super::vst3::enumerate(&bundle).expect("Bundle laesst sich lesen");
    assert_eq!(infos.len(), 2);
    assert!(infos.iter().all(|i| i.kind == Kind::Instrument), "{infos:?}");
    let probed = scan::probe_one(infos[0].clone());
    assert_eq!(probed.status, scan::Status::Ok, "{:?}", probed.message);
    assert_eq!(probed.params, 2);

    let Some(mut rig) = SineRig::new("Looper Test Sine", 256) else {
        return;
    };
    assert!(
        rig.plugin.activate(48_000.0, 256).is_err(),
        "ein Instrument ist kein Insert"
    );
    assert_eq!(rig.plugin.latency(), 0);
    assert!(first_sound(rig.render(&[])).is_none(), "ohne Note still");

    // A at 440 Hz, full velocity, at offset 100: sample 100 is sin(0) = 0, sample 101 the first
    // step of the sine.
    let out = rig.render(&[(100, [0x90, 69, 127])]).to_vec();
    assert_eq!(first_sound(&out), Some(101), "genau ab dem Offset");
    let step = std::f64::consts::TAU * 440.0 / 48_000.0;
    let expect = |k: usize| (0.25 * (step * k as f64).sin()) as f32;
    for k in 0..156 {
        assert!((out[100 + k] - expect(k)).abs() < 1e-5, "Sinus bei {k}: {}", out[100 + k]);
    }

    // Off at offset 50 of the next block: sound up to 49, silence from 50.
    let out = rig.render(&[(50, [0x80, 69, 0])]).to_vec();
    assert!(out[..50].iter().any(|s| *s != 0.0));
    assert!(out[50..].iter().all(|s| *s == 0.0), "Note-off auf dem Sample");
    assert!(first_sound(rig.render(&[])).is_none());

    // Half the velocity, half the level; a note on with velocity 0 is a note off.
    let out = rig.render(&[(0, [0x91, 69, 64])]).to_vec();
    assert!((peak(&out) - 0.25 * 64.0 / 127.0).abs() < 1e-3, "Velocity: {}", peak(&out));
    let out = rig.render(&[(10, [0x91, 69, 0])]).to_vec();
    assert!(out[10..].iter().all(|s| *s == 0.0), "Note-on mit Velocity 0");
}

/// The sustain pedal reaches a VST3 instrument as the parameter the plugin mapped CC 64 to
/// (`IMidiMapping`), at its sample offset: a note released while the pedal is down keeps sounding,
/// and stops on the sample the pedal comes up.
#[cfg(target_os = "macos")]
#[test]
fn a_vst3_instruments_sustain_pedal_holds_notes() {
    let Some(mut rig) = SineRig::new("Looper Test Sine", 256) else {
        return;
    };
    rig.render(&[(0, [0x90, 60, 100]), (5, [0xB0, 64, 127]), (20, [0x80, 60, 0])]);
    for _ in 0..4 {
        let out = rig.render(&[]);
        assert!(out.iter().any(|s| *s != 0.0), "das Pedal haelt die Note");
    }
    let out = rig.render(&[(30, [0xB0, 64, 0])]).to_vec();
    assert!(out[..30].iter().any(|s| *s != 0.0));
    assert!(out[30..].iter().all(|s| *s == 0.0), "Pedal hoch, Note aus - auf dem Sample");

    // Pedal on another channel: the plugin maps CC 64 of every channel.
    rig.render(&[(0, [0xB3, 64, 127]), (1, [0x93, 72, 90]), (2, [0x83, 72, 0])]);
    assert!(rig.render(&[]).iter().any(|s| *s != 0.0));
    rig.render(&[(0, [0xB3, 64, 0])]);
    assert!(first_sound(rig.render(&[])).is_none());

    // Controllers the plugin did not map change nothing and break nothing.
    rig.render(&[(0, [0xB0, 1, 127]), (0, [0xE0, 0, 127]), (0, [0xD0, 50, 0])]);
    assert!(first_sound(rig.render(&[])).is_none());
}

/// Panic on a VST3 instrument: what the track's note tracker sends (`NoteTracker::release`) ends
/// everything - and "all notes off" (CC 123) alone, which VST3 itself does not know, also ends every
/// note the host sent on that channel.
#[cfg(target_os = "macos")]
#[test]
fn panic_silences_a_vst3_instrument() {
    use crate::engine::source::NoteTracker;
    let Some(mut rig) = SineRig::new("Looper Test Sine", 128) else {
        return;
    };
    let mut tracker = NoteTracker::default();
    let played = [[0x90, 60, 100], [0xB0, 64, 127], [0x90, 64, 100], [0x80, 60, 0]];
    for m in played {
        tracker.note(m);
    }
    let events: Vec<(u32, [u8; 3])> = played.iter().map(|&m| (0, m)).collect();
    rig.render(&events);
    assert!(rig.render(&[]).iter().any(|s| *s != 0.0));
    let mut release = Vec::new();
    tracker.release(|m| release.push((7u32, m)));
    let out = rig.render(&release).to_vec();
    assert!(out[7..].iter().all(|s| *s == 0.0), "Panic aus dem Notentracker");

    // Only CC 123, nothing else: the host's own "all notes off" for VST3.
    rig.render(&[(0, [0x92, 50, 100]), (0, [0x92, 55, 100]), (0, [0x90, 57, 100])]);
    let out = rig.render(&[(40, [0xB2, 123, 0])]).to_vec();
    assert!(out[40..].iter().any(|s| *s != 0.0), "Kanal 1 klingt weiter");
    let out = rig.render(&[(3, [0xB0, 123, 0])]).to_vec();
    assert!(out[3..].iter().all(|s| *s == 0.0), "CC 123 beendet alles auf seinem Kanal");
}

/// A VST3 instrument's latency is reported (and is its real delay), its state round-trips, and it
/// starts again after a deactivate.
#[cfg(target_os = "macos")]
#[test]
fn a_vst3_instruments_latency_and_state() {
    let Some(mut rig) = SineRig::new("Looper Test Sine Latency", 256) else {
        return;
    };
    assert_eq!(rig.plugin.latency(), 32, "gemeldete Latenz");
    let out = rig.render(&[(0, [0x90, 69, 127])]).to_vec();
    assert_eq!(first_sound(&out), Some(33), "um genau die gemeldete Latenz verzoegert");

    // Volume (parameter 1) through the queue, then saved and restored into a fresh instance.
    rig.plugin.set_param(1, 0.5);
    rig.render(&[]);
    let out = rig.render(&[]).to_vec();
    assert!((peak(&out) - 0.125).abs() < 1e-3, "Lautstaerke halb: {}", peak(&out));
    let state = rig.plugin.save_state().expect("Zustand");
    drop(rig.synth.take());
    rig.plugin.deactivate();

    let Some(mut fresh) = SineRig::new("Looper Test Sine Latency", 256) else {
        return;
    };
    drop(fresh.synth.take());
    fresh.plugin.deactivate();
    fresh.plugin.load_state(&state).expect("Zustand zurueck");
    assert_eq!(fresh.plugin.param(1), 0.5, "der Controller kennt den Zustand");
    fresh.synth = Some(
        fresh
            .plugin
            .activate_instrument(44_100.0, 512)
            .expect("startet erneut"),
    );
    fresh.render(&[(0, [0x90, 69, 127])]);
    let out = fresh.render(&[]).to_vec();
    assert!((peak(&out) - 0.125).abs() < 1e-3, "Zustand gilt im Prozessor: {}", peak(&out));
}

/// Kontakt 8, if installed: loaded, started, a few blocks with a note, state saved and back, torn
/// down - and the **process has to end cleanly** afterwards. Only on request (`--ignored`): it
/// loads a big commercial plugin into the test process. Skips itself when Kontakt is missing.
#[cfg(target_os = "macos")]
fn kontakt_round_trip(info: PluginInfo) {
    use crate::engine::source::{BlockEvent, StereoOut};
    let started = std::time::Instant::now();
    let mut plugin = super::load(&info).expect("Kontakt laedt");
    eprintln!(
        "{}: geladen in {:?}, {} Parameter",
        info.id,
        started.elapsed(),
        plugin.params().len()
    );
    let state = plugin.save_state().expect("Zustand");
    eprintln!("Zustand: {} Bytes", state.len());
    // What `plugin_params` does for the UI: every parameter with value and text.
    let listed = std::time::Instant::now();
    let params = plugin.params();
    let bytes: usize = params
        .iter()
        .map(|p| p.name.len() + plugin.param_text(p.id, plugin.param(p.id)).len() + 60)
        .sum();
    eprintln!(
        "Parameterliste: {} Eintraege in {:?}, etwa {} kB als JSON",
        params.len(),
        listed.elapsed(),
        bytes / 1024
    );
    let mut synth = plugin.activate_instrument(48_000.0, 512).expect("startet");
    let mut l = vec![0.0f32; 512];
    let mut r = vec![0.0f32; 512];
    for i in 0..50 {
        let on = [BlockEvent {
            offset: 0,
            msg: [0x90, 60, 100],
        }];
        let events: &[BlockEvent] = if i == 0 { &on } else { &[] };
        synth.render(events, &mut [StereoOut { l: &mut l, r: &mut r }]);
        assert!(l.iter().chain(r.iter()).all(|s| s.is_finite()));
    }
    drop(synth);
    plugin.deactivate();
    plugin.load_state(&state).expect("Zustand zurueck");
    drop(plugin);
    let left = super::shutdown();
    eprintln!("{}: abgebaut nach {:?}, {left} Module noch in Gebrauch", info.id, started.elapsed());
}

#[cfg(target_os = "macos")]
const KONTAKT_VST3: &str = "/Library/Audio/Plug-Ins/VST3/Kontakt 8.vst3";

#[cfg(target_os = "macos")]
#[test]
#[ignore = "laedt Kontakt 8 (installiertes Fremd-Plugin); cargo test -- --ignored kontakt"]
fn kontakt_vst3_loads_plays_and_the_process_ends_cleanly() {
    let path = std::path::Path::new(KONTAKT_VST3);
    if !path.exists() {
        eprintln!("uebersprungen: Kontakt 8 (VST3) ist nicht installiert");
        return;
    }
    let info = super::vst3::enumerate(path)
        .expect("Kontakt laesst sich aufzaehlen")
        .into_iter()
        .find(|i| i.kind == Kind::Instrument)
        .expect("Kontakt ist ein Instrument");
    kontakt_round_trip(info.clone());
    // After bundleExit the module comes back with a fresh bundleEntry.
    kontakt_round_trip(info);
}

#[cfg(target_os = "macos")]
#[test]
#[ignore = "laedt Kontakt 8 (installiertes Fremd-Plugin); cargo test -- --ignored kontakt"]
fn kontakt_au_loads_plays_and_the_process_ends_cleanly() {
    let Some(info) = super::au::enumerate()
        .into_iter()
        .find(|i| i.id == "au:aumu:NiK8:-NI-")
    else {
        eprintln!("uebersprungen: Kontakt 8 (AU) ist nicht installiert");
        return;
    };
    kontakt_round_trip(info);
}
