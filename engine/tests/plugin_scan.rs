//! The plugin scan runs every plugin in a child process (`looper-engine --plugin-probe …`). This
//! proves the point of that: a plugin that crashes while loading is reported as crashed, and the
//! process that scanned it is still standing.
//!
//! Uses the test VST3 of this repository (`tools/test-vst3-gain`), which aborts when
//! `LOOPER_TEST_VST3_CRASH` is set. macOS only, like the plugin host.

#![cfg(target_os = "macos")]

use std::path::PathBuf;
use std::process::Command;

use looper_engine::plugin::scan::{self, Status};

fn fixture_bundle() -> Option<PathBuf> {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).parent()?.to_path_buf();
    let target = root.join("target").join("fixtures");
    let cargo = std::env::var("CARGO").unwrap_or_else(|_| "cargo".to_string());
    let ok = Command::new(cargo)
        .args(["build", "--quiet", "-p", "test-vst3-gain", "--target-dir"])
        .arg(&target)
        .current_dir(&root)
        .status()
        .map(|s| s.success())
        .unwrap_or(false);
    let dylib = target.join("debug").join("libtest_vst3_gain.dylib");
    if !ok || !dylib.exists() {
        return None;
    }
    let bundle = std::env::temp_dir()
        .join(format!("looper-scan-{}", std::process::id()))
        .join("LooperTestGain.vst3");
    let macos = bundle.join("Contents").join("MacOS");
    std::fs::create_dir_all(&macos).ok()?;
    std::fs::copy(&dylib, macos.join("LooperTestGain")).ok()?;
    std::fs::write(
        bundle.join("Contents").join("Info.plist"),
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<plist version=\"1.0\"><dict>\
         <key>CFBundleExecutable</key><string>LooperTestGain</string>\
         <key>CFBundlePackageType</key><string>BNDL</string></dict></plist>\n",
    )
    .ok()?;
    Some(bundle)
}

#[test]
fn a_plugin_that_crashes_while_loading_does_not_take_the_scan_with_it() {
    let Some(bundle) = fixture_bundle() else {
        eprintln!("uebersprungen: Test-VST3 nicht baubar");
        return;
    };
    let exe = PathBuf::from(env!("CARGO_BIN_EXE_looper-engine"));

    // Healthy: loaded in the child, reported with its numbers.
    let found = scan::probe_bundle_in_child(&exe, &bundle);
    assert_eq!(found.len(), 1, "{found:?}");
    assert_eq!(found[0].status, Status::Ok, "{found:?}");
    assert_eq!(found[0].info.name, "Looper Test Gain");
    assert_eq!(found[0].latency, 64);
    assert_eq!(found[0].params, 1);

    // Broken: the child dies, this process does not.
    // SAFETY: this test is the only thread of this process touching the environment.
    unsafe { std::env::set_var("LOOPER_TEST_VST3_CRASH", "1") };
    let found = scan::probe_bundle_in_child(&exe, &bundle);
    unsafe { std::env::remove_var("LOOPER_TEST_VST3_CRASH") };
    assert_eq!(found.len(), 1);
    assert_eq!(found[0].status, Status::Crashed, "{found:?}");
    assert!(!found[0].loadable());
    let message = found[0].message.as_deref().unwrap_or_default();
    assert!(message.contains("Crashed"), "{message}");
}
