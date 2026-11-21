//! Plugin inserts for the offline render: the score's plugin settings file, and loading a track's
//! inserts the way the app does.
//!
//! The file (`<score>.plugins.json`, copied into a recording as `score.plugins.json`) is written by
//! the app (`app/src-tauri/src/scorestate.rs`); this is a reader of the same format, version 1:
//!
//! ```json
//! { "version": 1,
//!   "tracks": { "guitar": { "inserts": [ {"plugin_id": "vst3:…", "name": "IRDX Studio", "slot": 0,
//!                                         "before": "octaver", "state": "<base64>"} ] } } }
//! ```
//!
//! What is loaded, and in which state, follows the app when it loads a score
//! (`plugins.rs`, `apply_score`): the `inserts:` line of the score's `mixer:` says **which** plugin
//! sits where and whether it is bypassed; the file gives each one its state - in order first, then
//! by id, never the state of another plugin ([`match_states`], the same rule as the app's). The
//! plugin is loaded, gets its state, and only then is started (`activate`), so it never plays a
//! block in its default sound; its reported latency then goes into the read-ahead exactly as the
//! host's poll puts it there.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::Deserialize;

use crate::engine::insert::{Insert, InsertPosition, InsertShared, MAX_BLOCK};
use crate::plugin::{self, PluginInfo, PluginInstance};
use crate::score::mixer::InsertLine;

/// The settings file, as far as the render needs it.
#[derive(Clone, Debug, Default, Deserialize)]
pub struct Sidecar {
    pub version: u32,
    #[serde(default)]
    pub tracks: BTreeMap<String, SideTrack>,
}

#[derive(Clone, Debug, Default, Deserialize)]
pub struct SideTrack {
    #[serde(default)]
    pub instrument: Option<SidePlugin>,
    #[serde(default)]
    pub inserts: Vec<SidePlugin>,
}

#[derive(Clone, Debug, Deserialize)]
pub struct SidePlugin {
    pub plugin_id: String,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub slot: Option<usize>,
    #[serde(default)]
    pub bypass: bool,
    #[serde(default)]
    pub before: Option<String>,
    #[serde(default)]
    pub state: Option<String>,
}

impl SidePlugin {
    fn bytes(&self) -> Option<Vec<u8>> {
        self.state.as_deref().and_then(base64_decode)
    }
}

/// Read a settings file. A file of another version is refused rather than half-read.
pub fn read_sidecar(path: &Path) -> Result<Sidecar, String> {
    let text = std::fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let side: Sidecar = serde_json::from_str(&text)
        .map_err(|e| format!("{} is not a plugin settings file: {e}", path.display()))?;
    if side.version != 1 {
        return Err(format!(
            "{} has version {}; this program reads version 1.",
            path.display(),
            side.version
        ));
    }
    Ok(side)
}

/// For each wanted plugin id, in order, the state the file has for it: the entry at the same
/// position if it is that plugin, else the first unused entry with that id, else none. Each entry
/// is handed out once. The app's `scorestate::match_states`, the same rule.
pub fn match_states(wanted: &[&str], side: &[SidePlugin]) -> Vec<Option<Vec<u8>>> {
    let mut used = vec![false; side.len()];
    wanted
        .iter()
        .enumerate()
        .map(|(k, id)| {
            let at = if side.get(k).is_some_and(|s| s.plugin_id == *id) && !used[k] {
                Some(k)
            } else {
                (0..side.len()).find(|i| !used[*i] && side[*i].plugin_id == *id)
            }?;
            used[at] = true;
            side[at].bytes()
        })
        .collect()
}

/// Standard base64 (with or without padding), as the app writes states.
pub fn base64_decode(text: &str) -> Option<Vec<u8>> {
    const B64: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = Vec::with_capacity(text.len() / 4 * 3);
    let mut acc = 0u32;
    let mut bits = 0;
    for c in text.bytes() {
        if c == b'=' || c.is_ascii_whitespace() {
            continue;
        }
        let v = B64.iter().position(|&b| b == c)? as u32;
        acc = (acc << 6) | v;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((acc >> bits) as u8);
            acc &= (1 << bits) - 1;
        }
    }
    Some(out)
}

/// Where a plugin id is found: bundles named on the command line first, then the app's scan cache
/// (read only; nothing is scanned or written).
pub struct PluginFinder {
    extra: Vec<PluginInfo>,
    cached: Vec<crate::plugin::scan::Scanned>,
}

impl PluginFinder {
    /// `bundles` are `.vst3` bundles to look in besides the scan cache.
    pub fn new(bundles: &[PathBuf]) -> Result<Self, String> {
        let mut extra = Vec::new();
        for bundle in bundles {
            extra.extend(enumerate_bundle(bundle)?);
        }
        Ok(Self {
            extra,
            cached: crate::plugin::scan::cached(),
        })
    }

    /// Only the given plugins, no cache - for tests.
    pub fn only(infos: Vec<PluginInfo>) -> Self {
        Self {
            extra: infos,
            cached: Vec::new(),
        }
    }

    pub fn find(&self, id: &str) -> Result<PluginInfo, String> {
        if let Some(info) = self.extra.iter().find(|i| i.id == id) {
            return Ok(info.clone());
        }
        match self.cached.iter().find(|s| s.info.id == id) {
            Some(s) if s.loadable() => Ok(s.info.clone()),
            Some(s) => Err(format!(
                "{} did not pass the plugin scan ({}) and is not loaded",
                s.info.name,
                s.message.clone().unwrap_or_else(|| format!("{:?}", s.status).to_lowercase())
            )),
            None => Err(format!(
                "{id} is not in the plugin scan cache ({}); scan in the app or with \
                 `looper-engine plugins scan`, or name the bundle with --plugin-bundle",
                crate::plugin::scan::cache_path().display()
            )),
        }
    }
}

#[cfg(target_os = "macos")]
fn enumerate_bundle(bundle: &Path) -> Result<Vec<PluginInfo>, String> {
    plugin::enumerate_vst3(bundle)
}

#[cfg(not(target_os = "macos"))]
fn enumerate_bundle(_bundle: &Path) -> Result<Vec<PluginInfo>, String> {
    Err(plugin::UNSUPPORTED.to_string())
}

/// One insert as the render treats it.
pub enum InsertPlan {
    /// Loaded, state restored, started: goes into `slot` at `position`.
    Loaded {
        slot: usize,
        name: String,
        position: InsertPosition,
        instance: Box<dyn PluginInstance>,
        shared: std::sync::Arc<InsertShared>,
        state_restored: bool,
    },
    /// Not in the signal path, and why - said once per insert.
    Skipped { name: String, why: String, bypassed: bool },
}

/// A track's loaded inserts. Keeps the control side of every plugin alive while its processor runs
/// in the strip, and stops them in the right order afterwards.
#[derive(Default)]
pub struct TrackInserts {
    pub plans: Vec<InsertPlan>,
}

impl TrackInserts {
    /// The processors for the strip, slot and insert. Hands each processor out once.
    pub fn start(&mut self, sample_rate: f64) -> Result<Vec<(usize, Insert)>, String> {
        let mut out = Vec::new();
        for plan in self.plans.iter_mut() {
            if let InsertPlan::Loaded {
                slot,
                name,
                position,
                instance,
                shared,
                ..
            } = plan
            {
                let processor = instance
                    .activate(sample_rate, MAX_BLOCK)
                    .map_err(|e| format!("{name} does not start: {e}"))?;
                shared.set_latency(instance.latency());
                out.push((*slot, Insert::new(processor, shared.clone()).at(*position)));
            }
        }
        Ok(out)
    }

    /// What the host's poll does: a latency the plugin changed after it started (often after its
    /// state settled) reaches the read-ahead. Returns the plugin latency sum.
    pub fn poll_latency(&mut self) -> u64 {
        let mut sum = 0;
        for plan in self.plans.iter_mut() {
            if let InsertPlan::Loaded { instance, shared, .. } = plan {
                let latency = instance.latency();
                if latency != shared.latency() {
                    shared.set_latency(latency);
                }
                sum += u64::from(latency);
            }
        }
        sum
    }

    /// Stop every plugin. Only after the strip that ran their processors is gone.
    pub fn stop(&mut self) {
        for plan in self.plans.iter_mut() {
            if let InsertPlan::Loaded { instance, .. } = plan {
                instance.deactivate();
            }
        }
    }
}

/// Which inserts a track gets, from its `inserts:` line and the settings file. `load` is false for
/// `--no-plugins`; then nothing is loaded and every insert that would be heard is skipped.
pub fn plan_inserts(
    lines: &[InsertLine],
    side: Option<&SideTrack>,
    finder: Option<&PluginFinder>,
    load: bool,
) -> Result<TrackInserts, String> {
    let ids: Vec<&str> = lines.iter().map(|l| l.plugin.as_str()).collect();
    let empty = Vec::new();
    let states = match_states(&ids, side.map_or(&empty, |s| &s.inserts));
    let mut plans = Vec::new();
    for (slot, (line, state)) in lines.iter().zip(states).enumerate() {
        let name = side
            .and_then(|s| s.inserts.iter().find(|p| p.plugin_id == line.plugin))
            .map(|p| p.name.clone())
            .filter(|n| !n.is_empty())
            .unwrap_or_else(|| line.plugin.clone());
        // The score's bypass is what the app applies (`apply_score`); a bypassed insert is not
        // run and its latency is not counted - leaving it out changes nothing.
        if line.bypass.unwrap_or(false) {
            plans.push(InsertPlan::Skipped {
                name,
                why: "bypassed in the score, not in the signal path (as live)".to_string(),
                bypassed: true,
            });
            continue;
        }
        if !load {
            plans.push(InsertPlan::Skipped {
                name,
                why: "--no-plugins".to_string(),
                bypassed: false,
            });
            continue;
        }
        let finder = finder.ok_or("no plugin finder")?;
        let info = finder.find(&line.plugin).map_err(|e| format!("insert {name}: {e}"))?;
        let mut instance =
            plugin::load(&info).map_err(|e| format!("insert {name} does not load: {e}"))?;
        let state_restored = match &state {
            Some(bytes) => {
                instance
                    .load_state(bytes)
                    .map_err(|e| format!("insert {name}: its saved state does not load: {e}"))?;
                true
            }
            None => false,
        };
        plans.push(InsertPlan::Loaded {
            slot,
            name: if info.name.is_empty() { name } else { info.name.clone() },
            position: line.position(),
            instance,
            shared: InsertShared::new(0),
            state_restored,
        });
    }
    Ok(TrackInserts { plans })
}
