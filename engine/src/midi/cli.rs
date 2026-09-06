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
    /// MIDI-Eingaenge auflisten. Oeffnet nichts.
    List,

    /// Eingehende Ereignisse anzeigen: was sendet dieses Pad, dieser Regler?
    Monitor {
        /// Nummer oder Namensteil des Eingangs. Ohne Angabe: der einzige, falls es nur einen gibt.
        #[arg(long, default_value = "")]
        device: String,

        /// Profil-Datei, gegen die aufgeloest wird. Ohne Angabe wird das Profil zum Geraet
        /// gesucht.
        #[arg(long)]
        profile: Option<PathBuf>,

        /// Nach so vielen Sekunden beenden. 0 heisst: bis Strg+C.
        #[arg(long, default_value_t = 0)]
        seconds: u64,
    },

    /// Die naechste gedrueckte Taste beziehungsweise der naechste bewegte Regler wird auf ein Ziel
    /// gelegt. Mit --save landet das Ergebnis im Profil.
    Learn {
        /// Zieladresse, z. B. track.1.record. Alle Adressen zeigt 'midi targets'.
        #[arg(long)]
        target: String,

        #[arg(long, default_value = "")]
        device: String,

        /// Profil-Datei. Ohne Angabe die Datei zum Geraet im Anwendungsdatenverzeichnis.
        #[arg(long)]
        profile: Option<PathBuf>,

        /// Das Gelernte ins Profil schreiben.
        #[arg(long)]
        save: bool,

        /// Tastenverhalten: press, release, toggle, momentary. Ohne Angabe die Vorgabe des Ziels.
        #[arg(long)]
        mode: Option<String>,

        /// Wertuebernahme fuer einen Regler: pickup oder jump.
        #[arg(long)]
        takeover: Option<String>,

        /// Nach so vielen Sekunden aufgeben.
        #[arg(long, default_value_t = 30)]
        seconds: u64,
    },

    /// Das Profil anzeigen - Pfad und Inhalt.
    Profile {
        #[arg(long, default_value = "")]
        device: String,

        #[arg(long)]
        profile: Option<PathBuf>,
    },

    /// Alle Zieladressen auflisten. Braucht kein Geraet.
    Targets {
        /// Fuer wie viele Tracks die Liste aufgebaut wird.
        #[arg(long, default_value_t = 2)]
        tracks: usize,

        /// Wie viele Ebenen je Track aufgefuehrt werden.
        #[arg(long, default_value_t = 2)]
        layers: usize,

        /// Wie viele Sektionen die Partitur hat (fuer transport.goto.N).
        #[arg(long, default_value_t = 4)]
        sections: usize,

        /// Nur Adressen, die diesen Text enthalten.
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
        println!(
            "Kein MIDI-Eingang gefunden.\n\
             Haengt der Controller am Rechner? Windows zeigt ein MIDI-Geraet erst, wenn es \
             angesteckt und der Treiber geladen ist."
        );
        return Ok(());
    }
    println!("MIDI-Eingaenge:");
    for port in &ports {
        let profile = profile::profile_for_port(&port.name);
        let note = match &profile {
            Some((path, profile)) => format!(
                "   Profil \"{}\" ({} Bindungen): {}",
                profile.device,
                profile.map.len(),
                path.display()
            ),
            None => "   kein Profil".to_string(),
        };
        println!("  {}  {}\n{note}", port.index, port.name);
    }
    println!(
        "\nHinweis: Windows gibt ein MIDI-Geraet immer nur an ein Programm gleichzeitig heraus. \
         Laeuft Ableton mit dem Controller als Control Surface, laesst er sich hier nicht oeffnen."
    );
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
    println!("MIDI-Eingang \"{name}\" offen.");
    match &profile {
        Some(profile) => println!(
            "Profil \"{}\" mit {} Bindungen wird zum Aufloesen benutzt.",
            profile.device,
            profile.map.len()
        ),
        None => println!("Kein Profil - es werden nur die eingehenden Ereignisse angezeigt."),
    }
    println!("Pads druecken, Regler drehen. Beenden mit Strg+C.\n");

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
                Resolution::Unbound => "nicht belegt".to_string(),
                Resolution::Absorbed(reason) => format!("- {reason}"),
                Resolution::Refused(reason) => format!("abgelehnt: {reason}"),
                Resolution::Action(action) => format!("=> {action:?}"),
            };
            println!("{line}   {verdict}");
            let _ = std::io::stdout().flush();
        }
        if midi.dropped() > 0 {
            println!("Achtung: {} Ereignisse verloren gegangen.", midi.dropped());
        }
        if seconds > 0 && started.elapsed() >= Duration::from_secs(seconds) {
            break;
        }
        std::thread::sleep(POLL);
    }
    println!("\n{count} Ereignisse.");
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
                "In {seconds} Sekunden kam nichts an. Sendet der Controller auf diesem Eingang? \
                 'looper-engine midi monitor' zeigt es."
            ));
        }
        std::thread::sleep(POLL);
    };

    let mut binding = learned.binding.clone();
    if let Some(mode) = mode {
        binding.button = ButtonMode::parse(mode).ok_or_else(|| {
            format!(
                "'{mode}' ist kein Tastenverhalten. Erlaubt: {}.",
                ButtonMode::names().join(", ")
            )
        })?;
    }
    if let Some(takeover) = takeover {
        binding.takeover = Takeover::parse(takeover).ok_or_else(|| {
            format!(
                "'{takeover}' ist keine Wertuebernahme. Erlaubt: {}.",
                Takeover::names().join(", ")
            )
        })?;
    }
    binding.check()?;

    println!("{}", learned.message);
    if !save {
        println!(
            "Nicht gespeichert. Mit --save landet die Bindung in {}.",
            path.display()
        );
        println!("Fuer die Partitur: {}: {{{}}}", binding.target, id_yaml(learned.id));
        return Ok(());
    }
    profile.map.insert(learned.id, binding);
    profile.save(&path)?;
    println!("Gespeichert in {}.", path.display());
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
                    println!("Profilverzeichnis: {}", profile::profile_dir().display());
                    if port.is_empty() {
                        println!(
                            "Es ist kein MIDI-Geraet angeschlossen und keine Datei mit --profile \
                             genannt, also ist auch kein Profil zu zeigen."
                        );
                    } else {
                        println!(
                            "Fuer \"{port}\" gibt es noch kein Profil. \
                             'looper-engine midi learn --target ... --save' legt eines an."
                        );
                    }
                    return Ok(());
                }
            }
        }
    };
    println!("Profil: {}", path.display());
    println!("Geraet: {}", profile.device);
    println!("{} Bindungen:\n", profile.map.len());
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
        "Zieladressen fuer {tracks} Tracks, {layers} Ebenen je Track, {sections} Sektionen.\n\
         Ein Track wird mit seiner Nummer (im Profil) oder mit seinem Namen (in der Partitur) \
         angesprochen.\n"
    );
    for target in &all {
        let address = target.to_string();
        if !filter.is_empty() && !address.to_lowercase().contains(&filter) {
            continue;
        }
        shown += 1;
        let kind = match target.control() {
            super::target::Control::Trigger => "Taster ".to_string(),
            super::target::Control::Switch => "Schalter".to_string(),
            super::target::Control::Range(range) => format!(
                "Wert    {} .. {}",
                super::binding::trim_number(range.min),
                super::binding::trim_number(range.max)
            ),
        };
        println!("  {address:<38} {kind:<22} {}", target.label());
    }
    println!("\n{shown} von {} Adressen.", all.len());
    Ok(())
}
