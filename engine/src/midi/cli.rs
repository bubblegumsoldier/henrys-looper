//! The `midi` subcommand: enough to use all of this without a user interface.
//!
//! Four jobs, and the second is the one that actually gets used:
//!
//! * `list` - which MIDI inputs are there. Opens nothing, so it works even while Ableton holds the
//!   controller, and then it is the fastest way to see that the controller *is* there and taken.
//! * `monitor` - what does this pad send? A control surface's manual is usually wrong or missing,
//!   and the MPD218's pad banks send different notes per bank. This prints one line per event with
//!   the id to write into a file, and what the current mapping does with it.
//! * `learn` - bind the next control pressed to a target, and optionally write it into the profile.
//!   The same mechanic the user interface will use, driven from a terminal.
//! * `targets` - the whole address tree. This is the catalogue a binding is written against.

use std::io::Write;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use clap::{Args, Subcommand};

use super::binding::{ButtonMode, MidiMap, Takeover};
use super::input;
use super::learn::Learn;
use super::profile::{self, Profile};
use super::router::{Context, Resolution, Router, TrackLayout};
use super::target::{Target, catalogue};

/// How often the monitor loop looks for new events. MIDI is buffered in a ring buffer, so this is
/// a display rate, not a timing one.
const POLL: Duration = Duration::from_millis(5);

#[derive(Args, Debug)]
pub struct MidiOpts {
    #[command(subcommand)]
    pub command: MidiCommand,
}

#[derive(Subcommand, Debug)]
pub enum MidiCommand {
    /// List MIDI inputs. Opens nothing.
    List,

    /// Show incoming events: what does this pad or knob send?
    Monitor {
        /// Number or part of the name of the input. If omitted: the only one, if there is just one.
        #[arg(long, default_value = "")]
        device: String,

        /// Profile file to resolve against. If omitted, the profile for the device is looked
        /// up.
        #[arg(long)]
        profile: Option<PathBuf>,

        /// Stop after this many seconds. 0 means: until Ctrl+C.
        #[arg(long, default_value_t = 0)]
        seconds: u64,
    },

    /// Bind the next key pressed or knob moved to a target. With --save the result goes into
    /// the profile.
    Learn {
        /// Target address, e.g. track.1.record. 'midi targets' lists every address.
        #[arg(long)]
        target: String,

        #[arg(long, default_value = "")]
        device: String,

        /// Profile file. If omitted: the device's file in the application data folder.
        #[arg(long)]
        profile: Option<PathBuf>,

        /// Write the learned binding into the profile.
        #[arg(long)]
        save: bool,

        /// Button mode: press, release, toggle, momentary. If omitted: the target's default.
        #[arg(long)]
        mode: Option<String>,

        /// Takeover mode for a knob: pickup or jump.
        #[arg(long)]
        takeover: Option<String>,

        /// Give up after this many seconds.
        #[arg(long, default_value_t = 30)]
        seconds: u64,
    },

    /// Show the profile - path and contents.
    Profile {
        #[arg(long, default_value = "")]
        device: String,

        #[arg(long)]
        profile: Option<PathBuf>,
    },

    /// List all target addresses. Needs no device.
    Targets {
        /// How many tracks to build the list for.
        #[arg(long, default_value_t = 2)]
        tracks: usize,

        /// How many layers to list per track.
        #[arg(long, default_value_t = 2)]
        layers: usize,

        /// How many sections the score has (for transport.goto.N).
        #[arg(long, default_value_t = 4)]
        sections: usize,

        /// Only addresses containing this text.
        #[arg(long, default_value = "")]
        filter: String,
    },
}

pub fn cmd_midi(opts: &MidiOpts) -> Result<(), String> {
    match &opts.command {
        MidiCommand::List => list(),
        MidiCommand::Monitor {
            device,
            profile,
            seconds,
        } => monitor(device, profile.as_deref(), *seconds),
        MidiCommand::Learn {
            target,
            device,
            profile,
            save,
            mode,
            takeover,
            seconds,
        } => learn(
            target,
            device,
            profile.as_deref(),
            *save,
            mode.as_deref(),
            takeover.as_deref(),
            *seconds,
        ),
        MidiCommand::Profile { device, profile } => show_profile(device, profile.as_deref()),
        MidiCommand::Targets {
            tracks,
            layers,
            sections,
            filter,
        } => targets(*tracks, *layers, *sections, filter),
    }
}

// ---------------------------------------------------------------------------------------------

fn list() -> Result<(), String> {
    let ports = input::list_ports()?;
    if ports.is_empty() {
        if cfg!(windows) {
            println!(
                "No MIDI input found.\n\
                 Is the controller connected? Windows shows a MIDI device only once it is \
                 plugged in and its driver is loaded."
            );
        } else {
            println!(
                "No MIDI input found.\n\
                 Is the controller connected? On macOS, the \"MIDI Studio\" window of \
                 \"Audio MIDI Setup\" shows which devices the system sees."
            );
        }
        return Ok(());
    }
    println!("MIDI inputs:");
    for port in &ports {
        let profile = profile::profile_for_port(&port.name);
        let note = match &profile {
            Some((path, profile)) => format!(
                "   Profile \"{}\" ({} bindings): {}",
                profile.device,
                profile.map.len(),
                path.display()
            ),
            None => "   no profile".to_string(),
        };
        println!("  {}  {}\n{note}", port.index, port.name);
    }
    if cfg!(windows) {
        println!(
            "\nNote: Windows gives a MIDI device to only one program at a time. \
             If Ableton is running with the controller as a control surface, it cannot be opened here."
        );
    }
    Ok(())
}

/// Load the profile the user asked for, or the one belonging to this port.
fn load_profile(port_name: &str, explicit: Option<&std::path::Path>) -> Result<Option<Profile>, String> {
    match explicit {
        Some(path) => Profile::load(path).map(Some),
        None => Ok(profile::profile_for_port(port_name).map(|(_, profile)| profile)),
    }
}

fn monitor(device: &str, profile_path: Option<&std::path::Path>, seconds: u64) -> Result<(), String> {
    let mut midi = input::open(device)?;
    let name = midi.name().to_string();
    let profile = load_profile(&name, profile_path)?;
    let map = profile
        .as_ref()
        .map(|p| p.map.clone())
        .unwrap_or_else(MidiMap::new);
    println!("MIDI input \"{name}\" open.");
    match &profile {
        Some(profile) => println!(
            "Resolving with profile \"{}\" ({} bindings).",
            profile.device,
            profile.map.len()
        ),
        None => println!("No profile - showing incoming events only."),
    }
    println!("Hit pads, turn knobs. Ctrl+C to quit.\n");

    // The layout is synthetic: the monitor has no engine, so a binding on `track.1.record` resolves
    // against a made-up list of eight tracks. It says *which* target a control is on, which is what
    // this command is for; whether that track exists is a question for the session.
    let layout = TrackLayout::new((1..=8).map(|n| format!("track{n}")));
    let mut router = Router::new(map);
    let started = Instant::now();
    let mut count = 0u64;

    loop {
        while let Some(timed) = midi.try_recv() {
            count += 1;
            let id = timed.event.id();
            let line = format!("{:>10}  {}", id.to_string(), timed.event.describe());
            let ctx = Context::bare(&layout);
            let verdict = match router.resolve(&timed.event, &ctx) {
                Resolution::Unbound => "unbound".to_string(),
                Resolution::Absorbed(reason) => format!("- {reason}"),
                Resolution::Refused(reason) => format!("refused: {reason}"),
                Resolution::Action(action) => format!("=> {action:?}"),
            };
            println!("{line}   {verdict}");
            let _ = std::io::stdout().flush();
        }
        if midi.dropped() > 0 {
            println!("Warning: {} events were lost.", midi.dropped());
        }
        if seconds > 0 && started.elapsed() >= Duration::from_secs(seconds) {
            break;
        }
        std::thread::sleep(POLL);
    }
    println!("\n{count} events.");
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn learn(
    target: &str,
    device: &str,
    profile_path: Option<&std::path::Path>,
    save: bool,
    mode: Option<&str>,
    takeover: Option<&str>,
    seconds: u64,
) -> Result<(), String> {
    let target = Target::parse(target)?;
    let mut midi = input::open(device)?;
    let name = midi.name().to_string();
    let path = match profile_path {
        Some(path) => path.to_path_buf(),
        None => profile::profile_for_port(&name)
            .map(|(path, _)| path)
            .unwrap_or_else(|| profile::profile_path(&name)),
    };
    let mut profile = if path.exists() {
        Profile::load(&path)?
    } else {
        Profile::new(&name)
    };

    let mut learn = Learn::new();
    println!("{}", learn.arm(target));
    let started = Instant::now();
    let learned = 'wait: loop {
        while let Some(timed) = midi.try_recv() {
            let previous = profile.map.get(timed.event.id()).cloned();
            if let Some(learned) = learn.feed(&timed.event, previous.as_ref()) {
                break 'wait learned;
            }
        }
        if started.elapsed() >= Duration::from_secs(seconds.max(1)) {
            return Err(format!(
                "Nothing arrived in {seconds} seconds. Is the controller sending on this input? \
                 'looper-engine midi monitor' shows it."
            ));
        }
        std::thread::sleep(POLL);
    };

    let mut binding = learned.binding.clone();
    if let Some(mode) = mode {
        binding.button = ButtonMode::parse(mode).ok_or_else(|| {
            format!(
                "'{mode}' is not a button mode. Allowed: {}.",
                ButtonMode::names().join(", ")
            )
        })?;
    }
    if let Some(takeover) = takeover {
        binding.takeover = Takeover::parse(takeover).ok_or_else(|| {
            format!(
                "'{takeover}' is not a takeover mode. Allowed: {}.",
                Takeover::names().join(", ")
            )
        })?;
    }
    binding.check()?;

    println!("{}", learned.message);
    if !save {
        println!(
            "Not saved. With --save the binding goes into {}.",
            path.display()
        );
        println!("For the score: {}: {{{}}}", binding.target, id_yaml(learned.id));
        return Ok(());
    }
    profile.map.insert(learned.id, binding);
    profile.save(&path)?;
    println!("Saved to {}.", path.display());
    Ok(())
}

/// The `note:`/`cc:` half of a score's `midi:` entry, for copy and paste.
fn id_yaml(id: super::event::MidiId) -> String {
    let channel = if id.channel == 1 {
        String::new()
    } else {
        format!(", channel: {}", id.channel)
    };
    match id.kind {
        super::event::MidiIdKind::Note => format!("note: {}{channel}", id.number),
        super::event::MidiIdKind::Cc => format!("cc: {}{channel}", id.number),
        super::event::MidiIdKind::Bend => "cc: 0".to_string(),
    }
}

fn show_profile(device: &str, profile_path: Option<&std::path::Path>) -> Result<(), String> {
    let (path, profile) = match profile_path {
        Some(path) => (path.to_path_buf(), Profile::load(path)?),
        None => {
            let ports = input::list_ports()?;
            let port = if device.is_empty() {
                ports.first().map(|p| p.name.clone())
            } else {
                ports
                    .iter()
                    .find(|p| p.name.to_lowercase().contains(&device.to_lowercase()))
                    .map(|p| p.name.clone())
            };
            let port = port.unwrap_or_else(|| device.to_string());
            match profile::profile_for_port(&port) {
                Some(found) => found,
                None => {
                    println!("Profile folder: {}", profile::profile_dir().display());
                    if port.is_empty() {
                        println!(
                            "No MIDI device is connected and no file was given with --profile, \
                             so there is no profile to show."
                        );
                    } else {
                        println!(
                            "There is no profile for \"{port}\" yet. \
                             'looper-engine midi learn --target ... --save' creates one."
                        );
                    }
                    return Ok(());
                }
            }
        }
    };
    println!("Profile: {}", path.display());
    println!("Device: {}", profile.device);
    println!("{} bindings:\n", profile.map.len());
    for (id, binding) in profile.map.sorted() {
        println!("  {:>10}  {}", id.to_string(), binding.describe());
    }
    Ok(())
}

fn targets(tracks: usize, layers: usize, sections: usize, filter: &str) -> Result<(), String> {
    let filter = filter.to_lowercase();
    let all = catalogue(tracks, layers, sections);
    let mut shown = 0usize;
    println!(
        "Target addresses for {tracks} tracks, {layers} layers per track, {sections} sections.\n\
         A track is addressed by its number (in a profile) or by its name (in a score).\n"
    );
    for target in &all {
        let address = target.to_string();
        if !filter.is_empty() && !address.to_lowercase().contains(&filter) {
            continue;
        }
        shown += 1;
        let kind = match target.control() {
            super::target::Control::Trigger => "Trigger".to_string(),
            super::target::Control::Switch => "Switch".to_string(),
            super::target::Control::Range(range) => format!(
                "Value   {} .. {}",
                super::binding::trim_number(range.min),
                super::binding::trim_number(range.max)
            ),
        };
        println!("  {address:<38} {kind:<22} {}", target.label());
    }
    println!("\n{shown} of {} addresses.", all.len());
    Ok(())
}
