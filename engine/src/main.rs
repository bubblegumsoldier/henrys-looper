//! looper-engine - measuring tool answering whether Windows audio in Rust is good enough for a
//! live looper. Throwaway instrument, not production code: it measures, prints and exits.
//!
//! Real-time rule for everything below: no allocation, no locking, no logging, no formatting and
//! no file access inside an audio callback. Callbacks only touch atomics, pre-allocated buffers
//! and a lock-free ring buffer.

mod audio;
mod click;
mod duplex;
mod engine;
mod latency;
mod meter;
mod soak;

use std::process::ExitCode;
use std::sync::mpsc::{Receiver, channel};

use clap::{Parser, Subcommand};

use audio::DeviceOpts;
use click::ClickOpts;
use engine::calibrate::CalibrateOpts;
use engine::live::LiveOpts;

#[derive(Parser, Debug)]
#[command(
    name = "looper-engine",
    version,
    about = "Audio-Messwerkzeug fuer den Looper-Prototyp (cpal / Windows)"
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand, Debug)]
enum Command {
    /// Alle Hosts und Geraete mit ihren unterstuetzten Konfigurationen auflisten
    List,

    /// Duplex-Durchhoeren: Eingang auf Ausgang, mit Pegelanzeige und Xrun-Zaehler
    Thru {
        #[command(flatten)]
        dev: DeviceOpts,

        /// Verstaerkung des durchgehoerten Signals
        #[arg(long, default_value_t = 1.0)]
        gain: f32,
    },

    /// Metronom mit sample-genauer Klickposition
    Click {
        #[command(flatten)]
        dev: DeviceOpts,

        #[command(flatten)]
        click: ClickOpts,
    },

    /// Roundtrip-Latenz per Loopback-Kabel messen (Ausgang 1 zurueck in Eingang 1)
    Latency {
        #[command(flatten)]
        dev: DeviceOpts,

        /// Anzahl der Messdurchlaeufe
        #[arg(long, default_value_t = 20)]
        runs: u32,
    },

    /// Dauerlauf: Durchhoeren und Klick gleichzeitig, mit Xrun- und Callback-Statistik
    Soak {
        #[command(flatten)]
        dev: DeviceOpts,

        /// Laufzeit in Minuten
        #[arg(long, default_value_t = 10)]
        minutes: u64,

        /// Verstaerkung des durchgehoerten Signals
        #[arg(long, default_value_t = 1.0)]
        gain: f32,
    },

    /// Looper (Phase 1): ein Track, Aufnahme auf Taktgrenze, Latenzkompensation, Tastatursteuerung
    Live {
        #[command(flatten)]
        dev: DeviceOpts,

        #[command(flatten)]
        live: LiveOpts,
    },

    /// Latenzkompensation pruefen: die Engine nimmt per Loopback-Kabel ihren eigenen Klick auf
    /// und misst, wie weit er vom Schlagraster abweicht
    Calibrate {
        #[command(flatten)]
        dev: DeviceOpts,

        #[command(flatten)]
        calibrate: CalibrateOpts,
    },
}

/// Non-blocking "press Enter to stop": a helper thread owns stdin, the main loop polls the
/// channel. Never touched from an audio callback.
pub fn wait_for_enter() -> Receiver<()> {
    let (tx, rx) = channel();
    std::thread::spawn(move || {
        let mut line = String::new();
        let _ = std::io::stdin().read_line(&mut line);
        let _ = tx.send(());
    });
    rx
}

#[cfg(test)]
mod cli_tests {
    use super::Cli;
    use clap::CommandFactory;

    /// clap builds the command at runtime, so flag collisions between flattened option structs
    /// only surface when the program starts. This turns that into a compile-and-test check.
    #[test]
    fn cli_is_well_formed() {
        Cli::command().debug_assert();
    }
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    let result = match &cli.command {
        Command::List => audio::cmd_list(),
        Command::Thru { dev, gain } => duplex::cmd_thru(dev, *gain),
        Command::Click { dev, click } => click::cmd_click(dev, click),
        Command::Latency { dev, runs } => latency::cmd_latency(dev, *runs),
        Command::Soak { dev, minutes, gain } => soak::cmd_soak(dev, *minutes, *gain),
        Command::Live { dev, live } => engine::live::cmd_live(dev, live),
        Command::Calibrate { dev, calibrate } => {
            engine::calibrate::cmd_calibrate(dev, calibrate)
        }
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(msg) => {
            eprintln!("\nFehler: {msg}");
            ExitCode::FAILURE
        }
    }
}
