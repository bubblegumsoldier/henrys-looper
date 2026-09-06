//! `looper-engine` - the command line measuring tool.
//!
//! Everything of substance lives in the library (`src/lib.rs`); this file is only the argument
//! parser in front of it, so the CLI and the desktop app share one loop core.

use std::process::ExitCode;

use clap::{Parser, Subcommand};

use looper_engine::audio::{self, DeviceOpts};
use looper_engine::click::ClickOpts;
use looper_engine::engine::calibrate::CalibrateOpts;
use looper_engine::engine::live::LiveOpts;
use looper_engine::engine::score_cli::ScoreOpts;
use looper_engine::{click, duplex, engine, latency, soak};

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

    /// Looper (Phase 2): mehrere Tracks mit eigenen Eingangskanaelen, unbegrenzte Overdub-Ebenen,
    /// Mithoeren je Track, Aufnahme auf Taktgrenze, Latenzkompensation, Tastatursteuerung
    Live {
        #[command(flatten)]
        dev: DeviceOpts,

        #[command(flatten)]
        live: LiveOpts,
    },

    /// Partitur abspielen (Phase 3): Tempo, Taktart, Tracks und Sektionen kommen aus der
    /// YAML-Datei. Einzaehler, Autorelease und armierte Wechsel uebernimmt der Runner; von Hand
    /// wird nur noch der Release-Knopf bedient.
    Score {
        #[command(flatten)]
        dev: DeviceOpts,

        #[command(flatten)]
        score: ScoreOpts,
    },

    /// Rechenlast der Effektkette messen: Zeit je Effekt und fuer die ganze Kette, als Anteil am
    /// Callback-Budget. Oeffnet kein Geraet und macht keinen Ton - aussagekraeftig nur im
    /// Release-Build.
    Fxbench {
        /// Samplerate, fuer die gerechnet wird
        #[arg(long, default_value_t = 48_000)]
        rate: u32,

        /// Wie viele Sekunden Audio je Messzeile durchgerechnet werden
        #[arg(long, default_value_t = 2.0)]
        seconds: f64,
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
        Command::Score { dev, score } => engine::score_cli::cmd_score(dev, score),
        Command::Fxbench { rate, seconds } => engine::fx::bench::cmd_fxbench(*rate, *seconds),
        Command::Calibrate { dev, calibrate } => engine::calibrate::cmd_calibrate(dev, calibrate),
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(msg) => {
            eprintln!("\nFehler: {msg}");
            ExitCode::FAILURE
        }
    }
}
