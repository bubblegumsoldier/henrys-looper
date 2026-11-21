//! `looper-engine render`: a finished recording mixed again, offline, from its dry stems - through
//! the live engine's own channel strip.
//!
//! ```text
//! looper-engine render "<recording folder>" --out <folder> [--score edited.yaml] [--no-plugins]
//! ```
//!
//! Per track: the dry stem (`NN name (dry).wav`) goes through the strip the score's `mixer:` entry
//! describes - built-in effects in their fixed order, plugin inserts at their `before:` places,
//! fader, pan - and comes out as `NN name.wav`; the stems are summed into `Mix.wav`, the way the
//! Main bus sums them. With the score the recording was made with and the same plugin states the
//! loop playback comes out as it was heard; with an edited copy (`--score`, e.g. less reverb) it
//! comes out as it would have been heard.
//!
//! # What is the live code, and what is not
//!
//! The strip is not a copy of the engine: it **is** the engine ([`strip::OfflineStrip`] runs an
//! [`crate::engine::process::EngineCore`] with one track), put into its state by the same commands
//! a loaded score sends ([`crate::score::mixer::track_commands`]), with plugins loaded, given their
//! state and started the way the app starts them. What this module adds is only the plumbing around
//! it: reading the folder, feeding the dry file in as the track's input, the read-ahead, the sum.
//!
//! # What cannot come out the same
//!
//! * **Live passes.** While a track recorded, overdubbed or heard through, the room heard its input
//!   through the effects one roundtrip late, plus the chain's own latency (or, where the input did
//!   not reach Main, the stem got it without effects - the "live complement"). The dry stem holds
//!   that input latency-aligned, so the render puts it, with effects, on the grid - where the loop
//!   playback of the same pass sits. That is the better take, and it is not what was heard;
//!   `--compare` marks those sections `live`. (Measured on a real take: the bass's live pass is in
//!   the stem 740 frames after the render, 596 roundtrip plus 144 octaver.)
//! * **The 24-bit dry file.** The live chain heard floats; the file holds them rounded to 24 bit.
//!   Most of the music is on that grid anyway, but fades are not, and a compressor or a long reverb
//!   carries the rounding on: around -84 dB under the stem on the voice chain of a real take.
//! * **The octaver's history.** Its splice search runs on a 4:1 decimated copy whose phase is the
//!   number of samples it has processed since it was switched on - so its output depends, a few
//!   milliseconds at a time, on everything it heard before, including live passes that reached it
//!   late. Where that count differs from live the stem differs by about -44 dB (short stretches
//!   around splices); where it agrees the render is exact to the last bit.
//! * **Main bus inserts** live in the app's plugin rack, not in the score; the score format has no
//!   place for them, so `Mix.wav` is the plain sum of the stems (`contents.mix` without bus inserts).
//! * **Instrument tracks** keep their recorded stem (the instrument is not loaded).
//! * Whatever was in a chain before bar 1 (a count-in heard through a reverb) is not in the dry
//!   files; the render starts from a chain that waited in silence. Plugins are told the same tempo
//!   and time signature as live, but their transport position counts from the render's own start.

pub mod plugins;
pub mod recording;
pub mod strip;

#[cfg(test)]
mod tests;

use std::io::Write;
use std::path::{Path, PathBuf};

use crate::capture::wav::{WavWriter, to_i24};
use crate::engine::fx::FxSlot;
use crate::engine::fx::render::chain_for;
use crate::engine::frame::Channels;
use crate::engine::timeline::TimeSignature;
use crate::score::mixer::{TrackMixSource, gain_to_db, preset_name};
use crate::score::{CompiledScore, TrackState, compile_score};

use plugins::{InsertPlan, PluginFinder, Sidecar, plan_inserts, read_sidecar};
use recording::Recording;
use strip::{OfflineStrip, StripSpec};

/// Frames per process call, as the app runs by default (architecture section 18).
pub const DEFAULT_BLOCK: usize = 128;

/// Options of `render`.
#[derive(Clone, Debug, Default)]
pub struct RenderOpts {
    pub recording: PathBuf,
    pub out: PathBuf,
    /// An edited copy of the score; its `mixer:` section (and the tracks' `pan`) is what is
    /// rendered. Default: the score copy in the recording.
    pub score: Option<PathBuf>,
    /// The plugin settings file. Default: the copy in the recording.
    pub plugins: Option<PathBuf>,
    /// Tracks whose recorded stem is taken as it is.
    pub stems_from_recording: Vec<String>,
    /// Only these tracks (no `Mix.wav` then).
    pub only: Vec<String>,
    /// Leave every plugin insert out (one warning per insert).
    pub no_plugins: bool,
    /// `.vst3` bundles to find plugins in, before the scan cache.
    pub plugin_bundles: Vec<PathBuf>,
    /// Compare every rendered stem with the recorded one.
    pub compare: bool,
    /// Frames per process call; 0 = [`DEFAULT_BLOCK`].
    pub block: usize,
}

/// What became of one track.
#[derive(Clone, Debug, PartialEq)]
pub struct TrackReport {
    pub name: String,
    pub file: String,
    pub rendered: bool,
    /// Read-ahead the engine applied (octaver plus plugins), frames.
    pub read_ahead: u64,
    pub inserts_applied: Vec<String>,
    pub inserts_skipped: Vec<String>,
    pub peak: f32,
    /// With `--compare`: the rendered stem against the recorded one over the whole take.
    pub residual: Option<Residual>,
    /// With `--compare`, per section: (section, whether this track's input was live there).
    pub sections: Vec<(String, bool, Residual)>,
}

/// What the render did.
#[derive(Clone, Debug, Default)]
pub struct RenderReport {
    pub tracks: Vec<TrackReport>,
    pub mix: Option<PathBuf>,
    pub mix_residual: Option<Residual>,
}

/// `render` subcommand.
pub fn cmd_render(opts: &RenderOpts) -> Result<(), String> {
    let mut out = std::io::stdout();
    render(opts, None, &mut out).map(|_| ())
}

/// The whole render. `finder` replaces the plugin lookup (tests); `None` builds it from the
/// command line's bundles and the scan cache when a plugin has to be loaded.
pub fn render(
    opts: &RenderOpts,
    finder: Option<PluginFinder>,
    log: &mut dyn Write,
) -> Result<RenderReport, String> {
    let rec = Recording::open(&opts.recording)?;
    let session = &rec.session;
    let rate = session.sample_rate;
    let frames = session.frames;
    let block = if opts.block == 0 { DEFAULT_BLOCK } else { opts.block };

    // The output folder: never the recording itself.
    std::fs::create_dir_all(&opts.out)
        .map_err(|e| format!("{} cannot be created: {e}", opts.out.display()))?;
    let out_dir = opts.out.canonicalize().map_err(|e| e.to_string())?;
    let rec_dir = rec.dir.canonicalize().map_err(|e| e.to_string())?;
    if out_dir.starts_with(&rec_dir) {
        return Err(format!(
            "--out {} lies in the recording folder. A render never writes into a recording; \
             choose another folder.",
            opts.out.display()
        ));
    }

    // The score: the edited copy, or the one the recording was made with.
    let score_path = match &opts.score {
        Some(p) => p.clone(),
        None => rec.score_copy().ok_or_else(|| {
            format!(
                "{} has no score copy (score.yaml); name the score with --score.",
                rec.dir.display()
            )
        })?,
    };
    let score = read_score(&score_path)?;
    // Section states for `--compare` come from the score the recording was made with.
    let recorded_score = match (&opts.score, rec.score_copy()) {
        (None, _) => Some(score.clone()),
        (Some(_), Some(copy)) => read_score(&copy).ok(),
        (Some(_), None) => None,
    };
    let mixer = score.mixer.clone().unwrap_or_default();

    // The plugin settings: named, or the copy in the recording.
    let sidecar_path = opts.plugins.clone().or_else(|| rec.sidecar_copy());
    let sidecar: Option<Sidecar> = match &sidecar_path {
        Some(p) => Some(read_sidecar(p)?),
        None => None,
    };

    // Names on the command line must be tracks of the recording.
    let names: Vec<&str> = session.tracks.iter().map(|t| t.name.as_str()).collect();
    for name in opts.stems_from_recording.iter().chain(&opts.only) {
        if !names.contains(&name.as_str()) {
            return Err(format!(
                "There is no track \"{name}\" in this recording. Its tracks: {}.",
                names.join(", ")
            ));
        }
    }

    writeln!(
        log,
        "Rendering \"{}\" ({} tracks, {:.1} s at {} Hz, {} BPM)\n  score:   {}\n  plugins: {}\n  out:     {}",
        session.title,
        session.tracks.len(),
        frames as f64 / rate as f64,
        rate,
        session.bpm,
        score_path.display(),
        match (&sidecar_path, opts.no_plugins) {
            (_, true) => "none (--no-plugins)".to_string(),
            (Some(p), _) => p.display().to_string(),
            (None, _) => "no settings file - plugins start in their default state".to_string(),
        },
        opts.out.display()
    )
    .ok();

    let mut finder = finder;
    let signature = TimeSignature::new(session.beats_per_bar, session.beat_unit);
    let mut mix: Option<Vec<f32>> = opts.only.is_empty().then(|| vec![0.0f32; frames as usize * 2]);
    let mut report = RenderReport::default();

    for t in &session.tracks {
        if !opts.only.is_empty() && !opts.only.contains(&t.name) {
            continue;
        }
        let compiled = score.tracks.iter().find(|c| c.name == t.name);
        let entry = mixer.tracks.get(&t.name).cloned();
        let instrument = compiled.is_some_and(|c| c.is_instrument())
            || entry.as_ref().is_some_and(|e| e.instrument.is_some())
            || t.midi.is_some();
        let asked = opts.stems_from_recording.contains(&t.name);
        let label = format!("{:02} {}", t.number, t.name);

        let (stem, mut tr) = if asked || instrument {
            let stem = rec.read(&t.stem, 2)?;
            std::fs::copy(rec.dir.join(&t.stem), opts.out.join(&t.stem))
                .map_err(|e| format!("{}: {e}", opts.out.join(&t.stem).display()))?;
            let why = if asked {
                "--stems-from-recording".to_string()
            } else {
                "instrument track: its sound comes from its instrument plugin, which is not loaded"
                    .to_string()
            };
            writeln!(log, "{label}: recorded stem taken as it is ({why})").ok();
            let peak = peak_of(&stem);
            (
                stem,
                TrackReport {
                    name: t.name.clone(),
                    file: t.stem.clone(),
                    rendered: false,
                    read_ahead: 0,
                    inserts_applied: Vec::new(),
                    inserts_skipped: Vec::new(),
                    peak,
                    residual: None,
                    sections: Vec::new(),
                },
            )
        } else {
            let dry_name = t.dry.as_deref().ok_or_else(|| {
                format!(
                    "Track \"{}\" has no dry stem in this recording (dry stems were switched off). \
                     Take its recorded stem with --stems-from-recording {}.",
                    t.name, t.name
                )
            })?;
            let dry = rec.read(dry_name, t.input_channels)?;
            let source = entry.clone().unwrap_or_default();
            if entry.is_none() {
                writeln!(
                    log,
                    "{label}: note - the score's mixer: has no entry for this track; rendered as a \
                     fresh track (dry, 0 dB)"
                )
                .ok();
            }
            let pan = compiled.map_or(0.0, |c| c.pan);
            let lines = source.inserts.clone().unwrap_or_default();
            let side = sidecar.as_ref().and_then(|s| s.tracks.get(&t.name));
            let needs_finder = !opts.no_plugins && lines.iter().any(|l| !l.bypass.unwrap_or(false));
            if needs_finder && finder.is_none() {
                finder = Some(PluginFinder::new(&opts.plugin_bundles)?);
            }
            let mut inserts = plan_inserts(&lines, side, finder.as_ref(), !opts.no_plugins)
                .map_err(|e| {
                    format!(
                        "Track \"{}\": {e}. Render without plugins with --no-plugins, or take this \
                         track's recorded stem with --stems-from-recording {}.",
                        t.name, t.name
                    )
                })?;
            let spec = StripSpec {
                sample_rate: rate,
                bpm: session.bpm,
                signature,
                channels: if t.input_channels == 2 { Channels::Stereo } else { Channels::Mono },
                pan,
                mix: source.resolve(),
                block,
            };
            let result = render_track(&spec, &source, &dry, frames, &mut inserts);
            inserts.stop();
            let (stem, ahead) = result?;
            write_stem(&opts.out.join(&t.stem), &stem, rate)?;
            let mut applied = Vec::new();
            let mut skipped = Vec::new();
            for plan in &inserts.plans {
                match plan {
                    InsertPlan::Loaded {
                        name,
                        position,
                        state_restored,
                        ..
                    } => applied.push(format!(
                        "{name} before {}{}",
                        position.name().unwrap_or("the end"),
                        if *state_restored { "" } else { " (default state: none saved)" }
                    )),
                    InsertPlan::Skipped { name, why, bypassed } => {
                        if !bypassed {
                            writeln!(
                                log,
                                "{label}: WARNING - plugin insert {name} SKIPPED ({why}). This \
                                 stem is NOT what was heard."
                            )
                            .ok();
                        }
                        skipped.push(format!("{name} ({why})"));
                    }
                }
            }
            let peak = peak_of(&stem);
            writeln!(
                log,
                "{label}: {} | fader {} | pan {} | inserts: {} | read-ahead {ahead} | peak {}",
                describe_chain(&source),
                db_text(source.resolve().gain, source.resolve().mute),
                pan_text(pan),
                inserts_text(&applied, &skipped),
                dbfs(peak)
            )
            .ok();
            (
                stem,
                TrackReport {
                    name: t.name.clone(),
                    file: t.stem.clone(),
                    rendered: true,
                    read_ahead: ahead,
                    inserts_applied: applied,
                    inserts_skipped: skipped,
                    peak,
                    residual: None,
                    sections: Vec::new(),
                },
            )
        };

        if opts.compare && tr.rendered {
            let recorded = rec.read(&t.stem, 2)?;
            let whole = Residual::of(&stem, &recorded, 0, frames);
            writeln!(log, "    compare with the recorded stem, whole take: {}", whole.text()).ok();
            tr.residual = Some(whole);
            for (section, from, to) in rec.sections() {
                let live = recorded_score
                    .as_ref()
                    .is_some_and(|s| live_in(s, &section, &t.name));
                let r = Residual::of(&stem, &recorded, from, to);
                writeln!(
                    log,
                    "      {section:<16} {:<5} {}",
                    if live { "live" } else { "" },
                    r.text()
                )
                .ok();
                tr.sections.push((section, live, r));
            }
        }

        if let Some(mix) = mix.as_mut() {
            // Track by track, from silence, in track order - how the Main bus sums.
            for (m, s) in mix.iter_mut().zip(&stem) {
                *m += *s;
            }
        }
        report.tracks.push(tr);
    }

    match mix {
        Some(mix) => {
            let name = session.mix.clone().unwrap_or_else(|| "Mix.wav".to_string());
            let path = opts.out.join(&name);
            write_stem(&path, &mix, rate)?;
            writeln!(log, "{name}: sum of the stems | peak {}", dbfs(peak_of(&mix))).ok();
            if opts.compare
                && let Ok(recorded) = rec.read(&name, 2)
            {
                let r = Residual::of(&mix, &recorded, 0, frames);
                writeln!(log, "    compare with the recorded mix: {}", r.text()).ok();
                report.mix_residual = Some(r);
            }
            report.mix = Some(path);
        }
        None => {
            writeln!(log, "Mix.wav not written: --only renders single tracks.").ok();
        }
    }
    Ok(report)
}

fn read_score(path: &Path) -> Result<CompiledScore, String> {
    let text =
        std::fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?;
    compile_score(&text).map_err(|e| format!("{}: {e}", path.display()))
}

/// One track through its strip: the engine built from `spec`, the plugins started and installed,
/// silence until everything is at rest, the read-ahead as the engine has it, then the dry file.
fn render_track(
    spec: &StripSpec,
    source: &TrackMixSource,
    dry: &[f32],
    frames: u64,
    inserts: &mut plugins::TrackInserts,
) -> Result<(Vec<f32>, u64), String> {
    let mut strip = OfflineStrip::new(*spec);
    strip.install(inserts.start(f64::from(spec.sample_rate))?)?;
    // Longer than the chain's longest tail, so a chain that waited in silence is idle (as one that
    // waited through a count-in is), and at least a second for the fader and switch fades.
    let (chain, _) = chain_for(source, spec.sample_rate);
    let settle = u64::from(chain.tail_samples()) + u64::from(spec.sample_rate);
    strip.settle(settle);
    // The host polls the plugins' latency; one more block lets the engine take it over.
    inserts.poll_latency();
    strip.settle(spec.block as u64);
    let ahead = strip.read_ahead();
    let stem = strip.render(dry, frames, ahead);
    drop(strip);
    Ok((stem, ahead))
}

fn write_stem(path: &Path, samples: &[f32], rate: u32) -> Result<(), String> {
    let err = |e: std::io::Error| format!("{}: {e}", path.display());
    let mut w = WavWriter::create(path, 2, rate).map_err(err)?;
    for &s in samples {
        w.sample(s).map_err(err)?;
    }
    w.finish().map_err(err)?;
    Ok(())
}

fn peak_of(samples: &[f32]) -> f32 {
    samples.iter().fold(0.0f32, |p, s| p.max(s.abs()))
}


/// A rendered stereo signal against a recorded one over frames `[from, to)`. The rendered one is
/// quantised to 24 bit first, as the recorder would have written it, so "bit-exact" means the same
/// file contents.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Residual {
    /// RMS of the difference relative to the recorded signal, dB; `None` when that is silent.
    pub rel_db: Option<f64>,
    /// RMS of the difference, dBFS (`-inf` when identical).
    pub residual_dbfs: f64,
    /// RMS of the recorded signal, dBFS.
    pub stem_dbfs: f64,
    /// Largest difference of one sample, in steps of the 24-bit file.
    pub max_lsb: u32,
}

impl Residual {
    pub fn of(a: &[f32], b: &[f32], from: u64, to: u64) -> Self {
        let lo = from as usize * 2;
        let hi = (to as usize * 2).min(a.len()).min(b.len()).max(lo);
        let mut diff = 0.0f64;
        let mut reference = 0.0f64;
        let mut max = 0i32;
        for i in lo..hi {
            let (qa, qb) = (to_i24(a[i]), to_i24(b[i]));
            max = max.max((qa - qb).abs());
            let d = f64::from(qa - qb) / 8_388_607.0;
            let r = f64::from(qb) / 8_388_607.0;
            diff += d * d;
            reference += r * r;
        }
        let n = (hi - lo).max(1) as f64;
        let dbfs = |energy: f64| {
            if energy == 0.0 { f64::NEG_INFINITY } else { 10.0 * (energy / n).log10() }
        };
        Self {
            rel_db: (reference > 0.0).then(|| {
                if diff == 0.0 { f64::NEG_INFINITY } else { 10.0 * (diff / reference).log10() }
            }),
            residual_dbfs: dbfs(diff),
            stem_dbfs: dbfs(reference),
            max_lsb: max as u32,
        }
    }

    pub fn bit_exact(&self) -> bool {
        self.max_lsb == 0
    }

    pub fn text(&self) -> String {
        let db = |v: f64| if v == f64::NEG_INFINITY { "-inf".to_string() } else { format!("{v:.1}") };
        if self.bit_exact() {
            return format!("bit-exact (stem {} dBFS)", db(self.stem_dbfs));
        }
        let rel = match self.rel_db {
            Some(v) => format!("{} dB rel. to the stem", db(v)),
            None => "stem silent here".to_string(),
        };
        format!(
            "{rel} (residual {} dBFS, stem {} dBFS, max diff {} LSB)",
            db(self.residual_dbfs),
            db(self.stem_dbfs),
            self.max_lsb
        )
    }
}

/// Whether `track`'s live input was heard in `section`: a take, hear-through, `monitor: true`, or
/// `monitor: always` while nothing else on its input records.
fn live_in(score: &CompiledScore, section: &str, track: &str) -> bool {
    let Some(s) = score.sections.iter().find(|s| s.id == section) else {
        return false;
    };
    let state = s.tracks.get(track).copied();
    let takes = matches!(
        state,
        Some(TrackState::Record | TrackState::Overdub | TrackState::Stack | TrackState::HearThrough)
    );
    let always = score
        .tracks
        .iter()
        .any(|t| t.name == track && t.monitor_always);
    takes || s.clip(track).monitor || always
}

fn describe_chain(source: &TrackMixSource) -> String {
    let mix = source.resolve();
    let on: Vec<&str> = FxSlot::all()
        .into_iter()
        .filter(|s| mix.settings.enabled[s.index()])
        .map(FxSlot::label)
        .collect();
    let preset = preset_name(mix.base);
    if mix.bypass || on.is_empty() {
        format!("preset {preset}: no built-in effects")
    } else {
        format!("preset {preset}{}: {}", if source.has_effect_lines() { " (+ overrides)" } else { "" }, on.join(", "))
    }
}

fn db_text(gain: f32, mute: bool) -> String {
    if mute {
        return "muted".to_string();
    }
    let db = gain_to_db(gain);
    format!("{db:+.1} dB")
}

fn pan_text(pan: f32) -> String {
    if pan == 0.0 {
        "C".to_string()
    } else if pan < 0.0 {
        format!("L{:.0}", -pan * 100.0)
    } else {
        format!("R{:.0}", pan * 100.0)
    }
}

fn inserts_text(applied: &[String], skipped: &[String]) -> String {
    let mut parts: Vec<String> = applied.iter().map(|a| format!("{a} applied")).collect();
    parts.extend(skipped.iter().map(|s| format!("{s} skipped")));
    if parts.is_empty() { "none".to_string() } else { parts.join("; ") }
}

fn dbfs(peak: f32) -> String {
    if peak <= 0.0 {
        "silent".to_string()
    } else {
        format!("{:.1} dBFS", 20.0 * peak.log10())
    }
}
