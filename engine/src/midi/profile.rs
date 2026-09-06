//! The controller profile: what *this box* does, saved once and valid everywhere.
//!
//! # Why the mapping is not simply part of the score
//!
//! Because a mapping belongs to the controller, not to the song. Bound inside a score, every new
//! piece would start with dead pads and the same twenty minutes of learning them again - and the
//! twentieth score would hold the twentieth copy of the same list, with nineteen of them slowly
//! going out of date. So there are two stages, and they compose:
//!
//! | Stufe | Datei | Gilt fuer | Beispiel |
//! |---|---|---|---|
//! | Controller-Profil | `<Anwendungsdaten>/henrys-looper/midi/<geraet>.yaml` | jedes Stueck | Pad 1 = Aufnahme Track 1 |
//! | Partitur-Ergaenzung | der `midi:`-Block der Partitur | dieses Stueck | Pad 5 = Sprung zum Refrain |
//!
//! The score is laid over the profile ([`super::binding::MidiMap::overlay`]), so a piece can take a
//! pad over without the profile knowing about that piece.
//!
//! # Where the file lives, and why not next to the score
//!
//! In the per-user application data directory - on Windows `%APPDATA%\henrys-looper\midi\`. The
//! argument is the same one that decided the two stages: the profile describes a piece of hardware
//! that is plugged into *this computer*, so it belongs to the computer and not to a project folder
//! that gets copied, mailed and version-controlled. A profile beside the score would travel with
//! the score to a machine where the MPD218 is not, and be wrong there.
//!
//! `HENRYS_LOOPER_CONFIG_DIR` moves the whole directory. That is what the tests use, and it is the
//! escape hatch for a musician who keeps his settings on a stick.
//!
//! # The file
//!
//! ```yaml
//! device: MPD218
//! bindings:
//!   ch1.note36: {target: track.1.record}
//!   ch1.note37: {target: track.1.overdub}
//!   ch1.note38: {target: track.1.monitor, mode: momentary}
//!   ch1.cc3:    {target: track.1.fx.reverb.mix, max: 0.4}
//!   ch1.cc9:    {target: track.1.pan, takeover: jump}
//! ```
//!
//! Keyed by the **control**, unlike the score's `midi:` block, which is keyed by the target. Both
//! read the way their file reads: a profile is a list of what the pads do, a score is a list of
//! what the piece needs. Keying the profile by control also makes a conflict structurally
//! impossible - one pad, one entry - and the reader reports a repeated key rather than letting the
//! second silently win.

use std::path::{Path, PathBuf};

use crate::score::yaml::{Node, NodeKind, parse_document};

use super::binding::{Binding, ButtonMode, MidiMap, Takeover, trim_number};
use super::event::MidiId;
use super::target::{Control, Target};

/// Directory name under the application data directory.
const APP_DIR: &str = "henrys-looper";
/// Subdirectory holding one file per controller.
const MIDI_DIR: &str = "midi";
/// Overrides the whole location. Used by the tests, and by anybody who keeps settings on a stick.
const DIR_ENV: &str = "HENRYS_LOOPER_CONFIG_DIR";

/// One controller's mapping.
#[derive(Clone, Debug, PartialEq)]
pub struct Profile {
    /// The device this belongs to, as the port announces itself. Matched loosely against the port
    /// name, because Windows decorates it: an MPD218 shows up as "MPD218" on one machine and as
    /// "2- MPD218" on the next, depending on how many MIDI devices were plugged in first.
    pub device: String,
    pub map: MidiMap,
}

impl Profile {
    pub fn new(device: impl Into<String>) -> Self {
        Self {
            device: device.into(),
            map: MidiMap::new(),
        }
    }

    /// Where this profile's file goes.
    pub fn path(&self) -> PathBuf {
        profile_path(&self.device)
    }

    /// Whether this profile is meant for a port of that name.
    pub fn matches(&self, port_name: &str) -> bool {
        let a = normalise(&self.device);
        let b = normalise(port_name);
        !a.is_empty() && (a == b || b.contains(&a) || a.contains(&b))
    }

    // ---- reading ---------------------------------------------------------------------------

    /// Parse a profile. Reports **every** problem at once, the way the score compiler does: a
    /// mapping file is edited by hand, and one message per pass is one pass per typo.
    pub fn parse(text: &str) -> Result<Profile, Vec<String>> {
        let document = parse_document(text).map_err(|error| {
            error
                .issues
                .iter()
                .map(|issue| issue.to_string())
                .collect::<Vec<_>>()
        })?;
        let mut issues: Vec<String> = Vec::new();
        let NodeKind::Map(entries) = &document.kind else {
            return Err(vec![
                "Ein Controller-Profil ist ein Mapping mit 'device' und 'bindings'.".to_string(),
            ]);
        };

        let mut device = String::new();
        let mut bindings: Option<&Node<'_>> = None;
        for entry in entries {
            match entry.key.as_str() {
                Some("device") => match entry.value.as_str() {
                    Some(name) => device = name.to_string(),
                    None => issues.push(format!(
                        "{}: 'device' muss der Name des Geraets sein (Text).",
                        entry.key.pos
                    )),
                },
                Some("bindings") => bindings = Some(&entry.value),
                Some(other) => issues.push(format!(
                    "{}: unbekanntes Feld '{other}'. Ein Profil hat 'device' und 'bindings'.",
                    entry.key.pos
                )),
                None => issues.push(format!("{}: Schluessel muessen Text sein.", entry.key.pos)),
            }
        }
        if device.is_empty() {
            issues.push(
                "Das Feld 'device' fehlt. Es traegt den Namen, unter dem sich der Controller \
                 meldet - 'looper-engine midi list' zeigt ihn."
                    .to_string(),
            );
        }

        let mut map = MidiMap::new();
        match bindings {
            None => issues.push("Das Feld 'bindings' fehlt.".to_string()),
            Some(node) => match &node.kind {
                NodeKind::Map(entries) => {
                    for entry in entries {
                        let Some(key) = entry.key.as_str() else {
                            issues.push(format!(
                                "{}: ein Schluessel unter 'bindings' ist eine MIDI-Adresse wie \
                                 ch1.note36.",
                                entry.key.pos
                            ));
                            continue;
                        };
                        let id = match MidiId::parse(key) {
                            Ok(id) => id,
                            Err(message) => {
                                issues.push(format!("{}: {message}", entry.key.pos));
                                continue;
                            }
                        };
                        match read_binding(&entry.value) {
                            Ok(binding) => {
                                if map.insert(id, binding).is_some() {
                                    issues.push(format!(
                                        "{}: '{id}' kommt mehrfach vor. Eine Taste kann nur eine \
                                         Sache tun; der zweite Eintrag wuerde den ersten still \
                                         ueberschreiben.",
                                        entry.key.pos
                                    ));
                                }
                            }
                            Err(message) => {
                                issues.push(format!("{}: {message}", entry.value.pos));
                            }
                        }
                    }
                }
                NodeKind::Null => {}
                _ => issues.push(format!(
                    "{}: 'bindings' muss ein Mapping von MIDI-Adresse auf Ziel sein.",
                    node.pos
                )),
            },
        }

        if issues.is_empty() {
            Ok(Profile { device, map })
        } else {
            Err(issues)
        }
    }

    pub fn load(path: &Path) -> Result<Profile, String> {
        let text = std::fs::read_to_string(path)
            .map_err(|e| format!("Profil {} laesst sich nicht lesen: {e}", path.display()))?;
        Profile::parse(&text).map_err(|issues| {
            let mut out = format!(
                "Das Profil {} laesst sich nicht lesen ({} Fehler):",
                path.display(),
                issues.len()
            );
            for issue in issues {
                out.push_str("\n  ");
                out.push_str(&issue);
            }
            out
        })
    }

    // ---- writing ---------------------------------------------------------------------------

    /// The file's text. Hand-written rather than serialised: there is no YAML emitter in this
    /// workspace (`saphyr-parser` only parses), the shape is four fields wide, and a file a human
    /// is expected to edit deserves a header line saying what it is.
    pub fn to_yaml(&self) -> String {
        let mut out = String::new();
        out.push_str("# Controller-Profil fuer Henrys Looper.\n");
        out.push_str("# 'looper-engine midi targets' listet alle Ziele, 'midi monitor' zeigt,\n");
        out.push_str("# welche Adresse ein Pad oder Regler sendet.\n");
        out.push_str(&format!("device: {}\n", quote(&self.device)));
        out.push_str("bindings:\n");
        if self.map.is_empty() {
            out.push_str("  {}\n");
            return out;
        }
        for (id, binding) in self.map.sorted() {
            let mut fields = vec![format!("target: {}", quote(&binding.target.to_string()))];
            match binding.target.control() {
                Control::Range(range) => {
                    if binding.takeover != Takeover::default() {
                        fields.push(format!("takeover: {}", binding.takeover.name()));
                    }
                    if let Some(min) = binding.min {
                        fields.push(format!("min: {}", trim_number(min)));
                    }
                    if let Some(max) = binding.max {
                        fields.push(format!("max: {}", trim_number(max)));
                    }
                    let _ = range;
                }
                _ => {
                    if binding.button != Binding::new(binding.target.clone()).button {
                        fields.push(format!("mode: {}", binding.button.name()));
                    }
                }
            }
            out.push_str(&format!("  {id}: {{{}}}\n", fields.join(", ")));
        }
        out
    }

    pub fn save(&self, path: &Path) -> Result<(), String> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| {
                format!("Verzeichnis {} laesst sich nicht anlegen: {e}", parent.display())
            })?;
        }
        std::fs::write(path, self.to_yaml())
            .map_err(|e| format!("Profil {} laesst sich nicht schreiben: {e}", path.display()))
    }
}

/// One `{target: ..., mode: ..., takeover: ..., min: ..., max: ...}` block.
fn read_binding(node: &Node<'_>) -> Result<Binding, String> {
    // The short form: just the address, for a file written by hand in a hurry.
    if let Some(address) = node.as_str() {
        let binding = Binding::new(Target::parse(address)?);
        binding.check()?;
        return Ok(binding);
    }
    let NodeKind::Map(entries) = &node.kind else {
        return Err(
            "Eine Bindung ist entweder die Zieladresse oder ein Mapping mit 'target'.".to_string(),
        );
    };

    let mut target: Option<Target> = None;
    let mut button: Option<ButtonMode> = None;
    let mut takeover: Option<Takeover> = None;
    let mut min: Option<f32> = None;
    let mut max: Option<f32> = None;

    for entry in entries {
        let key = entry.key.as_str().unwrap_or_default();
        match key {
            "target" => {
                let address = entry
                    .value
                    .as_str()
                    .ok_or("'target' muss eine Adresse sein, z. B. track.1.record.")?;
                target = Some(Target::parse(address)?);
            }
            "mode" => {
                let text = entry.value.as_str().unwrap_or_default();
                button = Some(ButtonMode::parse(text).ok_or_else(|| {
                    format!(
                        "'{text}' ist kein Tastenverhalten. Erlaubt: {}.",
                        ButtonMode::names().join(", ")
                    )
                })?);
            }
            "takeover" => {
                let text = entry.value.as_str().unwrap_or_default();
                takeover = Some(Takeover::parse(text).ok_or_else(|| {
                    format!(
                        "'{text}' ist keine Wertuebernahme. Erlaubt: {}.",
                        Takeover::names().join(", ")
                    )
                })?);
            }
            "min" => min = Some(number(&entry.value, "min")?),
            "max" => max = Some(number(&entry.value, "max")?),
            other => {
                return Err(format!(
                    "Unbekanntes Feld '{other}' in einer Bindung. Es gibt 'target', 'mode', \
                     'takeover', 'min' und 'max'."
                ));
            }
        }
    }

    let target = target.ok_or("Der Bindung fehlt 'target'.")?;
    let mut binding = Binding::new(target);
    if let Some(button) = button {
        binding.button = button;
    }
    if let Some(takeover) = takeover {
        binding.takeover = takeover;
    }
    binding.min = min;
    binding.max = max;
    binding.check()?;
    Ok(binding)
}

fn number(node: &Node<'_>, what: &str) -> Result<f32, String> {
    match node.kind {
        NodeKind::Int(value) => Ok(value as f32),
        NodeKind::Float(value) => Ok(value as f32),
        _ => Err(format!("'{what}' muss eine Zahl sein.")),
    }
}

/// Quote only where YAML needs it, so a plain name stays a plain name.
fn quote(text: &str) -> String {
    let plain = !text.is_empty()
        && text
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.' | '/' | ' '))
        && !text.starts_with(' ')
        && !text.ends_with(' ');
    if plain {
        text.to_string()
    } else {
        format!("\"{}\"", text.replace('\\', "\\\\").replace('"', "\\\""))
    }
}

// ---------------------------------------------------------------------------------------------
// Where the files are
// ---------------------------------------------------------------------------------------------

/// The directory holding one profile per controller.
pub fn profile_dir() -> PathBuf {
    if let Ok(dir) = std::env::var(DIR_ENV)
        && !dir.trim().is_empty()
    {
        return PathBuf::from(dir).join(MIDI_DIR);
    }
    let base = if cfg!(windows) {
        std::env::var("APPDATA").ok().map(PathBuf::from)
    } else {
        std::env::var("XDG_CONFIG_HOME")
            .ok()
            .map(PathBuf::from)
            .or_else(|| std::env::var("HOME").ok().map(|h| PathBuf::from(h).join(".config")))
    };
    base.unwrap_or_else(std::env::temp_dir)
        .join(APP_DIR)
        .join(MIDI_DIR)
}

/// The file a device's profile is written to.
pub fn profile_path(device: &str) -> PathBuf {
    profile_dir().join(format!("{}.yaml", slug(device)))
}

/// The profile belonging to a port, if there is one.
///
/// The directory is scanned rather than a file name being computed, because the name Windows
/// reports for a port is not stable enough to be a file name: the same MPD218 is "MPD218" or
/// "2- MPD218" depending on what else was plugged in first. The `device:` field inside each file
/// is matched against the port name instead, and the longest match wins.
pub fn profile_for_port(port_name: &str) -> Option<(PathBuf, Profile)> {
    let mut best: Option<(PathBuf, Profile)> = None;
    let entries = std::fs::read_dir(profile_dir()).ok()?;
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("yaml") {
            continue;
        }
        let Ok(profile) = Profile::load(&path) else {
            continue;
        };
        if !profile.matches(port_name) {
            continue;
        }
        let better = best
            .as_ref()
            .is_none_or(|(_, current)| current.device.len() < profile.device.len());
        if better {
            best = Some((path, profile));
        }
    }
    best
}

/// A device name turned into a file name: lower case, everything else a hyphen.
pub fn slug(device: &str) -> String {
    let mut out = String::new();
    let mut last_dash = true;
    for c in device.chars() {
        if c.is_ascii_alphanumeric() {
            out.push(c.to_ascii_lowercase());
            last_dash = false;
        } else if !last_dash {
            out.push('-');
            last_dash = true;
        }
    }
    let trimmed = out.trim_matches('-').to_string();
    if trimmed.is_empty() {
        "controller".to_string()
    } else {
        trimmed
    }
}

/// Lower case, only letters and digits - what two device names are compared as.
fn normalise(text: &str) -> String {
    text.chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .map(|c| c.to_ascii_lowercase())
        .collect()
}
