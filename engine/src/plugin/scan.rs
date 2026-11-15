//! Finding plugins, without letting a broken one take the program down.
//!
//! # The two halves
//!
//! * **Candidates** are cheap and safe to find in this process: VST3 bundles are directories on
//!   disk, Audio Units are entries in the system registry. Neither requires running plugin code.
//! * **Probing** a candidate means running its code - loading the bundle, creating an instance,
//!   pushing a few blocks of audio through it. That happens in a **child process**: this program's
//!   own executable, started with `--plugin-probe`. A plugin that crashes or hangs there kills the
//!   child, not the program; the parent records `crashed` or `timeout` and never loads it.
//!
//! # The cache
//!
//! Probing every plugin on every start would cost seconds per plugin. The result of each probe is
//! kept in `plugins.json` next to the MIDI profiles, keyed by what identifies the build of the
//! plugin: path and modification time for a bundle, id and version for an Audio Unit. An updated
//! plugin has a new key and is probed again; a deleted one simply drops out. A plugin that crashed
//! stays marked until it changes or the user asks for a full rescan.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use super::{Format, Kind, PluginInfo};

/// The flag that turns this program's executable into a probe.
pub const PROBE_FLAG: &str = "--plugin-probe";
/// Prefix of the one line of the child's output the parent reads. Plugins print all sorts of
/// things to stdout; this line is how ours is found among them.
const REPORT_PREFIX: &str = "LOOPER-PROBE ";
/// A probe that takes longer than this is killed and the plugin marked as hanging. Generous: some
/// plugins check a licence or unpack resources on their first start.
const PROBE_TIMEOUT: Duration = Duration::from_secs(45);
/// Extra places to look for VST3 bundles, separated by `:`. Directories or bundles.
pub const PATH_ENV: &str = "HENRYS_LOOPER_PLUGIN_PATH";
/// Cache format; a different number throws the old cache away.
const CACHE_VERSION: u32 = 1;

/// How the probe of a plugin went.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Status {
    /// Loaded, started, processed audio.
    Ok,
    /// Refused to load or to start, with a reason.
    Failed,
    /// Took the probe process down.
    Crashed,
    /// Did not finish in time.
    Timeout,
    /// An instrument that could not be played by the version that scanned it. Only found in old
    /// caches; such entries are probed again. Playable instruments are `ok`.
    Instrument,
}

/// One plugin as the program shows it: what it is, and whether it may be loaded.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Scanned {
    #[serde(flatten)]
    pub info: PluginInfo,
    pub status: Status,
    /// Why, when the status is not `ok`.
    pub message: Option<String>,
    /// Latency reported at 48 kHz during the probe, in frames.
    pub latency: u32,
    /// Number of parameters.
    pub params: usize,
}

impl Scanned {
    pub fn loadable(&self) -> bool {
        self.status == Status::Ok
    }
}

/// What a child process reports.
#[derive(Debug, Serialize, Deserialize)]
struct Report {
    plugins: Vec<Scanned>,
    error: Option<String>,
}

#[derive(Default, Serialize, Deserialize)]
struct Cache {
    version: u32,
    entries: HashMap<String, Vec<Scanned>>,
}

/// One thing to probe.
#[derive(Clone, Debug)]
enum Candidate {
    Bundle(PathBuf),
    Au(PluginInfo),
}

impl Candidate {
    fn key(&self) -> String {
        match self {
            Candidate::Bundle(path) => {
                let stamp = std::fs::metadata(path.join("Contents"))
                    .or_else(|_| std::fs::metadata(path))
                    .and_then(|m| m.modified())
                    .ok()
                    .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
                    .map(|d| d.as_secs())
                    .unwrap_or(0);
                format!("vst3:{}@{stamp}", path.display())
            }
            Candidate::Au(info) => format!("{}@{}", info.id, info.version),
        }
    }

    fn args(&self) -> Vec<String> {
        match self {
            Candidate::Bundle(path) => vec!["vst3".into(), path.display().to_string()],
            Candidate::Au(info) => vec!["au".into(), info.id.clone()],
        }
    }

    fn label(&self) -> String {
        match self {
            Candidate::Bundle(path) => path
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default(),
            Candidate::Au(info) => format!("{} ({})", info.name, info.vendor),
        }
    }

    /// What to list when the probe gave nothing back.
    fn fallback(&self, status: Status, message: String) -> Vec<Scanned> {
        let info = match self {
            Candidate::Au(info) => info.clone(),
            Candidate::Bundle(path) => PluginInfo {
                id: format!("vst3-bundle:{}", path.display()),
                format: Format::Vst3,
                name: path
                    .file_stem()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_default(),
                vendor: String::new(),
                version: String::new(),
                category: String::new(),
                kind: Kind::Other,
                path: Some(path.display().to_string()),
            },
        };
        vec![Scanned {
            info,
            status,
            message: Some(message),
            latency: 0,
            params: 0,
        }]
    }
}

/// The directories VST3 bundles are installed into, plus whatever [`PATH_ENV`] adds.
pub fn vst3_dirs() -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    if let Ok(extra) = std::env::var(PATH_ENV) {
        dirs.extend(extra.split(':').filter(|s| !s.is_empty()).map(PathBuf::from));
    }
    if cfg!(target_os = "macos") {
        if let Ok(home) = std::env::var("HOME") {
            dirs.push(PathBuf::from(home).join("Library/Audio/Plug-Ins/VST3"));
        }
        dirs.push(PathBuf::from("/Library/Audio/Plug-Ins/VST3"));
    }
    dirs
}

/// Every `.vst3` bundle in those directories, one level of sub-folders deep (vendors like to group
/// their plugins in a folder of their own).
pub fn vst3_bundles() -> Vec<PathBuf> {
    let mut out = Vec::new();
    for dir in vst3_dirs() {
        collect_bundles(&dir, 2, &mut out);
    }
    out.sort();
    out.dedup();
    out
}

fn collect_bundles(path: &Path, depth: u32, out: &mut Vec<PathBuf>) {
    if path.extension().is_some_and(|e| e.eq_ignore_ascii_case("vst3")) {
        if path.is_dir() {
            out.push(path.to_path_buf());
        }
        return;
    }
    if depth == 0 {
        return;
    }
    let Ok(entries) = std::fs::read_dir(path) else {
        return;
    };
    for entry in entries.flatten() {
        collect_bundles(&entry.path(), depth - 1, out);
    }
}

fn candidates() -> Vec<Candidate> {
    let mut out: Vec<Candidate> = vst3_bundles().into_iter().map(Candidate::Bundle).collect();
    #[cfg(target_os = "macos")]
    out.extend(super::au::enumerate().into_iter().map(Candidate::Au));
    out
}

/// Where the cache lives: next to the MIDI profiles.
pub fn cache_path() -> PathBuf {
    let midi = crate::midi::profile::profile_dir();
    midi.parent()
        .map(Path::to_path_buf)
        .unwrap_or(midi)
        .join("plugins.json")
}

fn read_cache(path: &Path) -> Cache {
    std::fs::read_to_string(path)
        .ok()
        .and_then(|text| serde_json::from_str::<Cache>(&text).ok())
        .filter(|c| c.version == CACHE_VERSION)
        .unwrap_or_default()
}

fn write_cache(path: &Path, cache: &Cache) {
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    if let Ok(text) = serde_json::to_string_pretty(cache) {
        let _ = std::fs::write(path, text);
    }
}

/// Find and probe every plugin, using the cache for those already known.
///
/// `exe` is the executable that understands [`PROBE_FLAG`] - this program. `full` ignores the
/// cache. `progress` hears the label of each plugin as it is probed.
pub fn scan(exe: &Path, full: bool, mut progress: impl FnMut(&str)) -> Vec<Scanned> {
    let path = cache_path();
    let old = if full { Cache::default() } else { read_cache(&path) };
    let mut cache = Cache {
        version: CACHE_VERSION,
        entries: HashMap::new(),
    };
    let mut out = Vec::new();
    for candidate in candidates() {
        let key = candidate.key();
        let found = match old.entries.get(&key) {
            // Instruments were listed as "not yet" before they could be played (Audio Units, then
            // VST3); probe them again instead of trusting that verdict.
            Some(known) if !known.iter().any(was_unplayable) => known.clone(),
            _ => {
                progress(&candidate.label());
                probe_in_child(exe, &candidate)
            }
        };
        out.extend(found.iter().cloned());
        cache.entries.insert(key, found);
    }
    write_cache(&path, &cache);
    out.sort_by(|a, b| {
        (a.info.kind != Kind::Effect, a.info.name.to_lowercase(), a.info.format.label())
            .cmp(&(b.info.kind != Kind::Effect, b.info.name.to_lowercase(), b.info.format.label()))
    });
    out
}

fn was_unplayable(s: &Scanned) -> bool {
    s.status == Status::Instrument
}

/// What the cache knows, without probing anything. For a fast start.
pub fn cached() -> Vec<Scanned> {
    let cache = read_cache(&cache_path());
    let mut out: Vec<Scanned> = cache.entries.into_values().flatten().collect();
    out.sort_by(|a, b| a.info.name.to_lowercase().cmp(&b.info.name.to_lowercase()));
    out
}

/// Probe one VST3 bundle in a child process - what [`scan`] does for every new bundle. Public for
/// the integration test that proves a crashing plugin does not take the scan with it.
pub fn probe_bundle_in_child(exe: &Path, bundle: &Path) -> Vec<Scanned> {
    probe_in_child(exe, &Candidate::Bundle(bundle.to_path_buf()))
}

fn probe_in_child(exe: &Path, candidate: &Candidate) -> Vec<Scanned> {
    let mut child = match Command::new(exe)
        .arg(PROBE_FLAG)
        .args(candidate.args())
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
    {
        Ok(child) => child,
        Err(e) => {
            return candidate.fallback(Status::Failed, format!("The probe did not start: {e}"));
        }
    };
    // Read stdout on a thread, so a chatty plugin cannot fill the pipe and stall the child.
    let mut stdout = child.stdout.take();
    let reader = std::thread::spawn(move || {
        let mut text = String::new();
        if let Some(out) = stdout.as_mut() {
            use std::io::Read;
            let _ = out.read_to_string(&mut text);
        }
        text
    });
    let started = Instant::now();
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break Some(status),
            Ok(None) if started.elapsed() > PROBE_TIMEOUT => {
                let _ = child.kill();
                let _ = child.wait();
                break None;
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(20)),
            Err(_) => break None,
        }
    };
    let text = reader.join().unwrap_or_default();
    let report = text
        .lines()
        .rev()
        .find_map(|l| l.strip_prefix(REPORT_PREFIX))
        .and_then(|json| serde_json::from_str::<Report>(json).ok());
    match (status, report) {
        (None, _) => candidate.fallback(
            Status::Timeout,
            format!(
                "Did not finish loading within {} seconds and was not loaded.",
                PROBE_TIMEOUT.as_secs()
            ),
        ),
        (Some(_), Some(report)) if !report.plugins.is_empty() => report.plugins,
        (Some(_), Some(report)) => candidate.fallback(
            Status::Failed,
            report
                .error
                .unwrap_or_else(|| "The bundle contains no audio plugin.".to_string()),
        ),
        (Some(status), None) => candidate.fallback(
            Status::Crashed,
            format!("Crashed while loading ({status}) and was not loaded."),
        ),
    }
}

/// Entry point of the probe child. Call first thing in `main`: returns `Some(exit code)` when this
/// process was started as a probe, `None` otherwise.
pub fn probe_main(args: &[String]) -> Option<i32> {
    if args.get(1).map(String::as_str) != Some(PROBE_FLAG) {
        return None;
    }
    let report = match (args.get(2).map(String::as_str), args.get(3)) {
        (Some(kind), Some(target)) => probe(kind, target),
        _ => Report {
            plugins: Vec::new(),
            error: Some("usage: --plugin-probe <vst3|au> <path|id>".to_string()),
        },
    };
    let code = if report.error.is_some() { 1 } else { 0 };
    println!(
        "{REPORT_PREFIX}{}",
        serde_json::to_string(&report).unwrap_or_default()
    );
    Some(code)
}

/// Load, start and run every plugin of one candidate, in this process.
fn probe(kind: &str, target: &str) -> Report {
    #[cfg(target_os = "macos")]
    {
        let infos = match kind {
            "vst3" => super::vst3::enumerate(Path::new(target)),
            "au" => super::au::enumerate()
                .into_iter()
                .find(|i| i.id == target)
                .map(|i| vec![i])
                .ok_or_else(|| format!("No Audio Unit with the id {target}.")),
            other => Err(format!("Unknown plugin format '{other}'.")),
        };
        let infos = match infos {
            Ok(infos) => infos,
            Err(e) => {
                return Report {
                    plugins: Vec::new(),
                    error: Some(e),
                };
            }
        };
        let plugins = infos.into_iter().map(probe_one).collect();
        Report {
            plugins,
            error: None,
        }
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = (kind, target);
        Report {
            plugins: Vec::new(),
            error: Some(super::UNSUPPORTED.to_string()),
        }
    }
}

/// Load one plugin, start it at 48 kHz and push a few blocks of noise through it.
pub fn probe_one(info: PluginInfo) -> Scanned {
    if info.kind == Kind::Instrument {
        return probe_instrument(info);
    }
    let result = (|| {
        let mut plugin = super::load(&info)?;
        let params = plugin.params().len();
        let mut processor = plugin.activate(48_000.0, 512)?;
        let mut seed = 1u32;
        let mut l = vec![0.0f32; 512];
        let mut r = vec![0.0f32; 512];
        for _ in 0..8 {
            for (a, b) in l.iter_mut().zip(r.iter_mut()) {
                seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                *a = ((seed >> 9) as f32 / 8_388_608.0 - 1.0) * 0.1;
                *b = *a;
            }
            processor.process(&mut l, &mut r);
        }
        if l.iter().chain(r.iter()).any(|s| !s.is_finite()) {
            return Err("Produced invalid numbers (NaN) from plain noise.".to_string());
        }
        let latency = plugin.latency();
        drop(processor);
        plugin.deactivate();
        Ok((params, latency))
    })();
    match result {
        Ok((params, latency)) => Scanned {
            info,
            status: Status::Ok,
            message: None,
            latency,
            params,
        },
        Err(e) => Scanned {
            info,
            status: Status::Failed,
            message: Some(e),
            latency: 0,
            params: 0,
        },
    }
}

/// Load an instrument, start it at 48 kHz and play a chord through it, with the sustain pedal and a
/// pitch bend on the way - what any keyboard sends in its first seconds.
pub fn probe_instrument(info: PluginInfo) -> Scanned {
    let result = (|| {
        use crate::engine::source::{BlockEvent, StereoOut};
        let mut plugin = super::load(&info)?;
        let params = plugin.params().len();
        let mut processor = plugin.activate_instrument(48_000.0, 512)?;
        let mut l = vec![0.0f32; 512];
        let mut r = vec![0.0f32; 512];
        let chord = [60u8, 64, 67];
        for block in 0..8 {
            let mut events = Vec::new();
            for (i, &note) in chord.iter().enumerate() {
                match block {
                    0 => events.push(BlockEvent {
                        offset: i as u32,
                        msg: [0x90, note, 90],
                    }),
                    5 => events.push(BlockEvent {
                        offset: 0,
                        msg: [0x80, note, 0],
                    }),
                    _ => {}
                }
            }
            match block {
                2 => events.extend([
                    BlockEvent {
                        offset: 10,
                        msg: [0xB0, 64, 127],
                    },
                    BlockEvent {
                        offset: 100,
                        msg: [0xE0, 0, 80],
                    },
                ]),
                6 => events.extend([
                    BlockEvent {
                        offset: 0,
                        msg: [0xB0, 64, 0],
                    },
                    BlockEvent {
                        offset: 0,
                        msg: [0xE0, 0, 64],
                    },
                ]),
                7 => events.push(BlockEvent {
                    offset: 0,
                    msg: [0xB0, 123, 0],
                }),
                _ => {}
            }
            l.fill(0.0);
            r.fill(0.0);
            processor.render(
                &events,
                &mut [StereoOut {
                    l: &mut l,
                    r: &mut r,
                }],
            );
            if l.iter().chain(r.iter()).any(|s| !s.is_finite()) {
                return Err("Produced invalid numbers (NaN) from a plain chord.".to_string());
            }
        }
        let latency = plugin.latency();
        drop(processor);
        plugin.deactivate();
        Ok((params, latency))
    })();
    match result {
        Ok((params, latency)) => Scanned {
            info,
            status: Status::Ok,
            message: None,
            latency,
            params,
        },
        Err(e) => Scanned {
            info,
            status: Status::Failed,
            message: Some(e),
            latency: 0,
            params: 0,
        },
    }
}
