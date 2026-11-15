//! Hosting foreign audio plugins: VST3 and Audio Units, on macOS.
//!
//! # Which formats, and why these two
//!
//! | Format | Rust side | Licence | On this Mac | Verdict |
//! |---|---|---|---|---|
//! | **VST3** | `vst3` 0.3 (bindings generated from the Steinberg headers) | crate MIT/Apache, SDK MIT since 3.8 | what Henry asked for; every commercial vendor ships it | **built** |
//! | **Audio Unit v2** (AUv3 through the same API) | the plain AudioToolbox C API | system framework | Apple ships 20+ effects (AUReverb2, AUDelay, …); most Mac vendors ship AU next to VST3 | **built** - almost free next to VST3, and the only format with test objects on every Mac |
//! | CLAP | `clack-host` 0.2 | MIT/Apache | no CLAP plugin installed; the big libraries (Kontakt, Superior Drummer) do not ship it | not now |
//!
//! # Threads
//!
//! A [`PluginInstance`] is the control side of one loaded plugin: parameters, state, latency,
//! editor. It is **not** `Send` and lives on one thread - in the app that is the main thread,
//! because both VST3 and Cocoa insist on it for views and most plugins assume it for everything
//! else. [`PluginInstance::activate`] hands out the audio side as an
//! [`InsertProcessor`](crate::engine::insert::InsertProcessor), which travels into the engine
//! through the insert queue and back. Between `activate` and `deactivate` the two halves run
//! concurrently; that is the contract of both plugin APIs and the reason parameter changes cross
//! over through lock-free queues or thread-safe calls only.
//!
//! # Crashes
//!
//! A plugin runs inside this process. A plugin that crashes takes the program with it - the
//! architecture document says so (section 8) and nothing here pretends otherwise. What *is*
//! isolated is the **scan** ([`scan`]): every plugin is first loaded in a child process, and one
//! that crashes or hangs there is marked and never loaded into the program.

use serde::{Deserialize, Serialize};

pub mod cli;
pub mod scan;

#[cfg(target_os = "macos")]
mod au;
#[cfg(target_os = "macos")]
mod cf;
#[cfg(target_os = "macos")]
pub mod editor;
#[cfg(target_os = "macos")]
mod vst3;

#[cfg(test)]
mod tests;

use crate::engine::insert::InsertProcessor;
use crate::engine::source::SourceProcessor;

/// The plugin formats this host speaks.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Format {
    Vst3,
    Au,
}

impl Format {
    pub fn label(self) -> &'static str {
        match self {
            Format::Vst3 => "VST3",
            Format::Au => "AU",
        }
    }
}

/// What a plugin is for. Effects go into insert slots, instruments into a track's source slot.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Kind {
    Effect,
    Instrument,
    Other,
}

/// One plugin as the scan found it. Serialised into the cache and sent to the UI as is.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct PluginInfo {
    /// Stable identity across scans and machines: `vst3:<class id in hex>` or
    /// `au:<type>:<subtype>:<manufacturer>`.
    pub id: String,
    pub format: Format,
    pub name: String,
    pub vendor: String,
    pub version: String,
    /// Free-form category: VST3 sub-categories (`Fx|Reverb`), or the AU type.
    pub category: String,
    pub kind: Kind,
    /// Bundle on disk (VST3). Audio Units are found through the system registry instead.
    pub path: Option<String>,
}

/// One parameter of a loaded plugin. Values travel **normalised** (0..1) everywhere, which is what
/// VST3 speaks natively, what a MIDI knob maps onto without knowing the plugin, and what an AU's
/// min/max is turned into.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct ParamInfo {
    pub id: u32,
    pub name: String,
    pub unit: String,
    /// Normalised default.
    pub default: f64,
    /// Number of steps for a stepped parameter (a switch has 1), 0 for a continuous one.
    pub steps: u32,
    pub automatable: bool,
    pub read_only: bool,
    /// The plugin's own bypass parameter, if it has one. The host has its own bypass per slot.
    pub is_bypass: bool,
}

/// The control side of one loaded plugin. Lives on one thread (see the module comment).
pub trait PluginInstance {
    fn info(&self) -> &PluginInfo;
    fn params(&self) -> Vec<ParamInfo>;
    /// Current normalised value.
    fn param(&self, id: u32) -> f64;
    /// How the plugin itself displays `value` (normalised) of a parameter.
    fn param_text(&self, id: u32, value: f64) -> String;
    /// Set a normalised value. Reaches the audio side without a lock.
    fn set_param(&mut self, id: u32, value: f64);
    /// Latency the plugin reports, in frames at the activated rate.
    fn latency(&self) -> u32;
    /// The plugin's complete state as one opaque chunk.
    fn save_state(&self) -> Result<Vec<u8>, String>;
    fn load_state(&mut self, data: &[u8]) -> Result<(), String>;
    /// Prepare for audio and hand out the processor that runs on the audio thread.
    fn activate(
        &mut self,
        sample_rate: f64,
        max_block: usize,
    ) -> Result<Box<dyn InsertProcessor>, String>;
    /// Prepare an instrument for audio and hand out the processor that plays it on the audio
    /// thread (`crate::engine::source`). Only instruments implement it.
    fn activate_instrument(
        &mut self,
        sample_rate: f64,
        max_block: usize,
    ) -> Result<Box<dyn SourceProcessor>, String> {
        let _ = (sample_rate, max_block);
        Err(format!("\"{}\" is not an instrument.", self.info().name))
    }
    /// Stop audio. Only to be called once the processor from [`Self::activate`] (or
    /// [`Self::activate_instrument`]) is dropped.
    fn deactivate(&mut self);
    fn has_editor(&self) -> bool;
    /// Open the plugin's own window, or bring it to the front. Main thread only.
    fn open_editor(&mut self, title: &str) -> Result<(), String>;
    fn close_editor(&mut self);
    fn editor_open(&self) -> bool;
    /// Size of the editor's view in points, while it exists.
    fn editor_size(&self) -> Option<(f64, f64)> {
        None
    }
    /// Parameters the plugin changed by itself (in its editor) since the last call.
    fn take_changed(&mut self) -> Vec<u32>;
}

/// Load a plugin into this process. On macOS only; elsewhere every call explains why not.
pub fn load(info: &PluginInfo) -> Result<Box<dyn PluginInstance>, String> {
    #[cfg(target_os = "macos")]
    {
        match info.format {
            Format::Vst3 => vst3::load(info),
            Format::Au => au::load(info),
        }
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = info;
        Err(UNSUPPORTED.to_string())
    }
}

/// Take down the plugin libraries no instance uses any more - VST3 modules get their factory
/// released and `bundleExit`; the code itself is never unmapped. Call it on the way out, **after**
/// every instance is dropped (editors closed, audio stopped). Returns how many modules are still
/// in use and were left alone.
pub fn shutdown() -> usize {
    #[cfg(target_os = "macos")]
    {
        vst3::shutdown()
    }
    #[cfg(not(target_os = "macos"))]
    {
        0
    }
}

/// Said by every plugin call on a platform without a plugin host.
pub const UNSUPPORTED: &str = "Plugin hosting is available on macOS only for now.";

/// Whether this build can host plugins at all.
pub const SUPPORTED: bool = cfg!(target_os = "macos");

/// Short display text for a normalised value when the plugin has none.
pub fn plain_text(value: f64) -> String {
    format!("{:.0} %", value * 100.0)
}
