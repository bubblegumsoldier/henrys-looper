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

    /// Render a recorded WAV through one track's effect chain into a new WAV file, offline. Opens
    /// no device and makes no sound.
    Fxrender {
        /// WAV file as the looper records it (24-bit, mono or stereo)
        input: std::path::PathBuf,

        /// WAV file to write (24-bit stereo)
        output: std::path::PathBuf,

        /// The track's mixer entry as written under `mixer.tracks` in a score, e.g.
        /// "{preset: bass_from_guitar, octaver: {tone: 1200}}"
        #[arg(long, default_value = "{preset: dry}")]
        mix: String,

        /// Tempo for the tempo-synchronous delay
        #[arg(long, default_value_t = 120.0)]
        bpm: f64,

        /// Start in the input, in seconds
        #[arg(long, default_value_t = 0.0)]
        start: f64,

        /// Seconds to render (default: to the end)
        #[arg(long)]
        seconds: Option<f64>,
    },

    /// Mix a finished recording again from its dry stems, offline, through the live engine's own
    /// channel strips (built-in effects, plugin inserts, fader, pan): one stem per track and
    /// Mix.wav. Opens no device and makes no sound; never writes into the recording.
    Render {
        /// The recording folder (the one with session.json)
        recording: std::path::PathBuf,

        /// Folder to write the stems and Mix.wav into (created if missing)
        #[arg(long)]
        out: std::path::PathBuf,

        /// An edited copy of the score: its mixer: section and the tracks' pan are rendered.
        /// Default: the score.yaml in the recording
        #[arg(long)]
        score: Option<std::path::PathBuf>,

        /// The plugin settings file (<score>.plugins.json). Default: score.plugins.json in the
        /// recording
        #[arg(long)]
        plugins: Option<std::path::PathBuf>,

        /// Tracks whose recorded stem is taken as it is, comma-separated. Instrument tracks always
        /// are
        #[arg(long, value_delimiter = ',')]
        stems_from_recording: Vec<String>,

        /// Write only these stems, comma-separated (no Mix.wav then)
        #[arg(long, value_delimiter = ',')]
        only: Vec<String>,

        /// Leave every plugin insert out, with a warning per insert
        #[arg(long)]
        no_plugins: bool,

        /// A .vst3 bundle to find plugins in before the scan cache (repeatable)
        #[arg(long)]
        plugin_bundle: Vec<std::path::PathBuf>,

        /// Compare every rendered stem (and the mix) with the recorded one: residual per section
        #[arg(long)]
        compare: bool,

        /// Frames per process call, as the audio device delivered them live
        #[arg(long, default_value_t = looper_engine::render::DEFAULT_BLOCK)]
        block: usize,

        /// Output channels of the interface the recording was made with (2: Main and Monitor are
        /// one mix, so a monitored input reached Main). Not stored in a recording
        #[arg(long, default_value_t = 2)]
        outputs: usize,

        /// Level of the monitored inputs in dB, when the score's mixer: does not set
        /// buses.inputs. Not stored in a recording
        #[arg(long, default_value_t = 0.0, allow_negative_numbers = true)]
        monitor_db: f64,

        /// Also write the re-performance's own dry stems (to see where the input reconstruction
        /// differs from the recording)
        #[arg(long)]
        write_dry: bool,
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
        Command::Fxrender {
            input,
            output,
            mix,
            bpm,
            start,
            seconds,
        } => engine::fx::render::cmd_fxrender(
            input,
            output,
            &engine::fx::render::RenderOpts {
                mix: mix.clone(),
                bpm: *bpm,
                start: *start,
                seconds: *seconds,
            },
        ),
        Command::Render {
            recording,
            out,
            score,
            plugins,
            stems_from_recording,
            only,
            no_plugins,
            plugin_bundle,
            compare,
            block,
            outputs,
            monitor_db,
            write_dry,
        } => looper_engine::render::cmd_render(&looper_engine::render::RenderOpts {
            recording: recording.clone(),
            out: out.clone(),
            score: score.clone(),
            plugins: plugins.clone(),
            stems_from_recording: stems_from_recording.clone(),
            only: only.clone(),
            no_plugins: *no_plugins,
            plugin_bundles: plugin_bundle.clone(),
            compare: *compare,
            block: *block,
            outputs: *outputs,
            monitor_db: *monitor_db,
            write_dry: *write_dry,
        }),
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
