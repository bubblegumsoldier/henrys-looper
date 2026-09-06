//! Long-run stability test: duplex monitoring plus metronome, so the callbacks carry realistic
//! load, with per-minute status output from the main thread.

use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

use crate::audio::{self, DeviceOpts};
use crate::click::ClickSpec;
use crate::duplex::{self, print_summary};
use crate::meter::fmt_dbfs;

pub fn cmd_soak(dev: &DeviceOpts, minutes: u64, gain: f32) -> Result<(), String> {
    if minutes == 0 {
        return Err("--minutes muss mindestens 1 sein.".to_string());
    }
    let setup = audio::open_duplex(dev)?;
    audio::print_setup(&setup);

    let spec = ClickSpec {
        bpm: 100.0,
        beats_per_bar: 4,
        beat_unit: 4,
        bars: 0,
    };
    spec.validate()?;
    let runner = duplex::start_duplex(&setup, gain, Some(spec))?;
    println!(
        "Dauerlauf: {minutes} Minuten, Durchhoeren + Klick ({} BPM, {}/{}) gleichzeitig.",
        spec.bpm, spec.beats_per_bar, spec.beat_unit
    );
    println!("Enter bricht vorzeitig ab.\n");

    let enter = crate::wait_for_enter();
    let started = Instant::now();
    let total = Duration::from_secs(minutes * 60);
    let mut next_report = Duration::from_secs(60);
    let stats = &runner.stats;

    loop {
        if enter.recv_timeout(Duration::from_millis(250)).is_ok() {
            println!("Abbruch durch Benutzer.");
            break;
        }
        let elapsed = started.elapsed();
        if elapsed >= next_report {
            let peak = stats.input_peak.take();
            println!(
                "[{:>4} min] Callbacks: {:<10} Xruns: {:<5} Ring leer: {:<5} Ring voll: {:<5} Fehler: {:<5} Callback max {:.3} ms / Mittel {:.3} ms | Eingang {}",
                next_report.as_secs() / 60,
                stats.callbacks(),
                stats.xruns.load(Ordering::Relaxed),
                stats.underruns.load(Ordering::Relaxed),
                stats.overruns.load(Ordering::Relaxed),
                stats.other_errors.load(Ordering::Relaxed),
                stats.max_callback_ms(),
                stats.avg_callback_ms(),
                fmt_dbfs(peak)
            );
            next_report += Duration::from_secs(60);
        }
        if elapsed >= total {
            break;
        }
    }

    let elapsed = started.elapsed();
    println!("\n===== Zusammenfassung nach {:.1} Minuten =====", elapsed.as_secs_f64() / 60.0);
    print_summary(&runner.stats);
    let cbs = runner.stats.callbacks();
    if cbs > 0 {
        println!(
            "Callbacks pro Sekunde: {:.1} (erwartet ca. {:.1} bei {} Frames)",
            cbs as f64 / elapsed.as_secs_f64(),
            2.0 * setup.sample_rate() as f64 / setup.buffer_frames().max(1) as f64,
            setup.buffer_frames()
        );
    }
    Ok(())
}
