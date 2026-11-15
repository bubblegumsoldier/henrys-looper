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
use looper_engine::midi::cli::MidiOpts;
use looper_engine::plugin::cli::PluginOpts;
use looper_engine::{click, duplex, engine, latency, midi, plugin, soak};

#[derive(Parser, Debug)]
#[command(
    name = "looper-engine",
    version,
    about = "Henrys Looper engine and audio measuring tool (cpal; Windows and macOS)"
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand, Debug)]
enum Command {
    /// List all hosts and devices with their supported configurations
    List,

    /// Duplex pass-through: input to output, with level meter and xrun counter
    Thru {
        #[command(flatten)]
        dev: DeviceOpts,

        /// Gain of the passed-through signal
        #[arg(long, default_value_t = 1.0)]
        gain: f32,
    },

    /// Metronome with sample-accurate click position
    Click {
        #[command(flatten)]
        dev: DeviceOpts,

        #[command(flatten)]
        click: ClickOpts,
    },

    /// Measure roundtrip latency with a loopback cable (output 1 back into input 1)
    Latency {
        #[command(flatten)]
        dev: DeviceOpts,

        /// Number of measurement runs
        #[arg(long, default_value_t = 20)]
        runs: u32,
    },

    /// Soak test: pass-through and click at the same time, with xrun and callback statistics
    Soak {
        #[command(flatten)]
        dev: DeviceOpts,

        /// Duration in minutes
        #[arg(long, default_value_t = 10)]
        minutes: u64,

        /// Gain of the passed-through signal
        #[arg(long, default_value_t = 1.0)]
        gain: f32,
    },

    /// Looper (phase 2): several tracks with their own input channels, unlimited Overdub layers,
    /// Monitor per track, Record on the bar line, latency compensation, keyboard control
    Live {
        #[command(flatten)]
        dev: DeviceOpts,

        #[command(flatten)]
        live: LiveOpts,
    },

    /// Play a score (phase 3): tempo, time signature, tracks and sections come from the YAML
    /// file. The runner handles count-in, auto-release and armed switches; the only thing left
    /// to press by hand is Release.
    Score {
        #[command(flatten)]
        dev: DeviceOpts,

        #[command(flatten)]
        score: ScoreOpts,
    },

    /// Measure the CPU load of the effect chain: time per effect and for the whole chain, as a
    /// share of the callback budget. Opens no device and makes no sound - only meaningful in a
    /// release build.
    Fxbench {
        /// Sample rate to compute for
        #[arg(long, default_value_t = 48_000)]
        rate: u32,

        /// Seconds of audio processed per result line
        #[arg(long, default_value_t = 2.0)]
        seconds: f64,
    },

    /// MIDI control (phase 5): list devices, show incoming events, learn buttons onto targets
    /// and save them in the controller profile. Makes no sound and opens no audio device.
    Midi {
        #[command(flatten)]
        midi: MidiOpts,
    },

    /// Check the latency compensation: the engine records its own click through a loopback cable
    /// and measures how far it lands from the beat grid
    Calibrate {
        #[command(flatten)]
        dev: DeviceOpts,

        #[command(flatten)]
        calibrate: CalibrateOpts,
    },
    /// Plugins (VST3, Audio Units): scan, probe one offline, open its editor window
    Plugins {
        #[command(subcommand)]
        plugins: PluginOpts,
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
    // The plugin scan starts this executable as a child to load one plugin at a time; a plugin
    // that crashes takes the child down, not the scan. See `plugin::scan`.
    let args: Vec<String> = std::env::args().collect();
    if let Some(code) = plugin::scan::probe_main(&args) {
        return ExitCode::from(code as u8);
    }
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
        Command::Midi { midi } => midi::cli::cmd_midi(midi),
        Command::Calibrate { dev, calibrate } => engine::calibrate::cmd_calibrate(dev, calibrate),
        Command::Plugins { plugins } => plugin::cli::cmd_plugins(plugins),
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(msg) => {
            eprintln!("\nError: {msg}");
            ExitCode::FAILURE
        }
    }
}
