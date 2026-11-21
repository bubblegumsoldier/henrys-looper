//! `looper-engine render`: a finished recording mixed again, offline - the performance played once
//! more through the live engine, from the dry stems.
//!
//! ```text
//! looper-engine render "<recording folder>" --out <folder> [--score edited.yaml] [--no-plugins]
//! ```
//!
//! Out come `NN name.wav` per track and `Mix.wav`, as the recorder writes them. With the score the
//! recording was made with and the same plugin states they are what was heard; with an edited copy
//! (`--score`, e.g. less reverb) what would have been heard.
//!
//! # What is the live code, and what is not
//!
//! Nothing between a dry stem and a stem is computed here. [`perform`] starts an
//! [`crate::engine::process::EngineCore`] with every track of the recording's score, loads it the
//! way the app loads a score (setup, the recorded compensation, the `mixer:`), loads the plugins
//! with their saved state, and lets the score's [`crate::engine::runner::Runner`] run the sections
//! on the recording's timeline - count-in, takes, loop playback, monitoring, `monitor_fade`, the
//! hand-off between tracks sharing an input. The input the engine gets is the musician's,
//! reconstructed from the dry stems (see `perform`); the stems and the mix come out of the recorder
//! (`crate::capture`) attached to that engine - the same tap as live. What this module adds is the
//! plumbing: reading the folder, two passes, writing files, comparing.
//!
//! # What cannot come out the same, and why
//!
//! * **Instrument tracks** keep their recorded stem: the instrument plugin is not loaded.
//! * **Two settings are not in a recording**: how many outputs the interface had (with two, a
//!   monitored input reaches Main; `--outputs`) and the level of the monitored inputs when the
//!   score does not set `mixer.buses.inputs` (`--monitor-db`). Both are assumed and printed.
//! * **The 24-bit dry file.** The live engine heard floats; the file holds them rounded. A
//!   compressor or a long reverb carries the rounding on (around -84 dB on a voice chain).
//! * **Before bar 1** nothing is in the files: what a chain heard during the count-in (a
//!   `monitor: always` guitar through its reverb) is missing at the start.
//! * **The octaver's history** (its splice search runs on a decimated copy whose phase is the count
//!   of samples it processed) and **commands that arrived late live** (a loop entry 10 samples late)
//!   are not in the recording; where they mattered the stem differs by around -44 dB.
//! * Main bus inserts are not in the score and are not applied.
//!
//! `--compare` measures all of it against the recorded files: per section, and per quarter second
//! in level - a window that differs by more than 3 dB is listed.

pub mod perform;
pub mod plugins;
pub mod recording;

#[cfg(test)]
mod tests;

use std::io::{BufReader, Read as _, Write};
use std::path::{Path, PathBuf};

use crate::capture::Layout;
use crate::capture::wav::{HEADER_BYTES, WavWriter, to_i24};
use crate::engine::fx::FxSlot;
use crate::score::mixer::{TrackMixSource, db_to_gain, gain_to_db, preset_name};
use crate::score::{CompiledScore, TrackState, compile_score};

use perform::{Pass, PerformSpec, perform};
use plugins::{InsertPlan, PluginFinder, Sidecar, TrackInserts, plan_inserts, read_sidecar};
use recording::Recording;

/// Frames per process call, as the app runs by default (architecture section 18).
pub const DEFAULT_BLOCK: usize = 128;

/// A window of the level check, seconds.
pub const LEVEL_WINDOW_SECS: f64 = 0.25;
/// Level difference that gets a window listed, dB.
pub const LEVEL_TOLERANCE_DB: f64 = 3.0;
/// Windows where both are quieter than this are not listed, dBFS.
pub const LEVEL_FLOOR_DBFS: f64 = -80.0;

/// Options of `render`.
#[derive(Clone, Debug)]
pub struct RenderOpts {
    pub recording: PathBuf,
    pub out: PathBuf,
    /// An edited copy of the score; its `mixer:` section and the tracks' `pan` are rendered. The
    /// sections, tracks and timing always come from the score copy in the recording.
    pub score: Option<PathBuf>,
    /// The plugin settings file. Default: the copy in the recording.
    pub plugins: Option<PathBuf>,
    /// Tracks whose recorded stem is taken as it is.
    pub stems_from_recording: Vec<String>,
    /// Only these stems are written (no `Mix.wav` then).
    pub only: Vec<String>,
    /// Leave every plugin insert out (one warning per insert).
    pub no_plugins: bool,
    /// `.vst3` bundles to find plugins in, before the scan cache.
    pub plugin_bundles: Vec<PathBuf>,
    /// Compare every written stem and the mix with the recorded ones.
    pub compare: bool,
    /// Frames per process call; 0 = [`DEFAULT_BLOCK`].
    pub block: usize,
    /// Output channels of the interface the recording was made with (not stored in a recording).
    pub outputs: usize,
    /// Level of the monitored inputs in dB, when the score does not set `mixer.buses.inputs`.
    pub monitor_db: f64,
    /// Also write the re-performance's own dry stems, to see where the input reconstruction
    /// differs from the recording.
    pub write_dry: bool,
}

impl Default for RenderOpts {
    fn default() -> Self {
        Self {
            recording: PathBuf::new(),
            out: PathBuf::new(),
            score: None,
            plugins: None,
            stems_from_recording: Vec::new(),
            only: Vec::new(),
            no_plugins: false,
            plugin_bundles: Vec::new(),
            compare: false,
            block: DEFAULT_BLOCK,
            outputs: 2,
            monitor_db: 0.0,
            write_dry: false,
        }
    }
}

/// A listed window of the level check: start in seconds, render and recorded level in dBFS.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct LevelFlag {
    pub at: f64,
    pub render_dbfs: f64,
    pub recorded_dbfs: f64,
}

/// What became of one track.
#[derive(Clone, Debug, PartialEq)]
pub struct TrackReport {
    pub name: String,
    pub file: String,
    /// False when the recorded stem was taken.
    pub rendered: bool,
    pub inserts_applied: Vec<String>,
    pub inserts_skipped: Vec<String>,
    pub peak: f32,
    /// The re-performance's dry stem against the recorded one: how well the input was
    /// reconstructed. `None` for a track without one.
    pub dry_check: Option<Residual>,
    /// With `--compare`: the stem against the recorded one over the whole take.
    pub residual: Option<Residual>,
    /// With `--compare`, per section: (section, whether this track's input was live there).
    pub sections: Vec<(String, bool, Residual)>,
    /// With `--compare`: windows whose level differs by more than [`LEVEL_TOLERANCE_DB`].
    pub level_flags: Vec<LevelFlag>,
}

/// What the render did.
#[derive(Clone, Debug, Default)]
pub struct RenderReport {
    pub tracks: Vec<TrackReport>,
    pub mix: Option<PathBuf>,
    pub mix_residual: Option<Residual>,
    pub mix_level_flags: Vec<LevelFlag>,
    /// Frames whose recorder weight differed between the two passes (must be 0).
    pub weight_mismatches: u64,
}

/// `render` subcommand.
pub fn cmd_render(opts: &RenderOpts) -> Result<(), String> {
    let mut out = std::io::stdout();
    render(opts, None, &mut out).map(|_| ())
}

fn read_score(path: &Path) -> Result<CompiledScore, String> {
    let text = std::fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?;
    compile_score(&text).map_err(|e| format!("{}: {e}", path.display()))
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

    // The score the recording was made with: its sections and timing are what is performed.
    let recorded_path = rec.score_copy().ok_or_else(|| {
        format!(
            "{} has no score copy (score.yaml). The render plays the score again and needs the one \
             the recording was made with.",
            rec.dir.display()
        )
    })?;
    let recorded = read_score(&recorded_path)?;
    let names: Vec<&str> = session.tracks.iter().map(|t| t.name.as_str()).collect();
    let score_names: Vec<&str> = recorded.tracks.iter().map(|t| t.name.as_str()).collect();
    if names != score_names {
        return Err(format!(
            "session.json lists the tracks {} but its score.yaml {}; the folder is inconsistent.",
            names.join(", "),
            score_names.join(", ")
        ));
    }
    for name in opts.stems_from_recording.iter().chain(&opts.only) {
        if !names.contains(&name.as_str()) {
            return Err(format!(
                "There is no track \"{name}\" in this recording. Its tracks: {}.",
                names.join(", ")
            ));
        }
    }
    // What is rendered: the recorded score with the edited one's mixer and pans.
    let mut score = recorded.clone();
    if let Some(path) = &opts.score {
        let edited = read_score(path)?;
        score.mixer = edited.mixer.clone();
        for t in score.tracks.iter_mut() {
            match edited.tracks.iter().find(|e| e.name == t.name) {
                Some(e) => t.pan = e.pan,
                None => writeln!(log, "note: {} has no track \"{}\"; its pan is kept", path.display(), t.name)
                    .unwrap_or(()),
            }
        }
    }
    let mixer = score.mixer.clone().unwrap_or_default();
    let inputs_from_score = mixer.buses.as_ref().and_then(|b| b.inputs);

    let sidecar_path = opts.plugins.clone().or_else(|| rec.sidecar_copy());
    let sidecar: Option<Sidecar> = match &sidecar_path {
        Some(p) => Some(read_sidecar(p)?),
        None => None,
    };

    writeln!(
        log,
        "Rendering \"{}\" ({} tracks, {:.1} s at {} Hz, {} BPM) - the score played again\n  score:   {}{}\n  plugins: {}\n  out:     {}\n  assumed: {} output channels{}; monitored inputs at {} (neither is stored in a recording)",
        session.title,
        session.tracks.len(),
        frames as f64 / rate as f64,
        rate,
        session.bpm,
        recorded_path.display(),
        opts.score.as_ref().map(|p| format!(" (mixer and pan from {})", p.display())).unwrap_or_default(),
        match (&sidecar_path, opts.no_plugins) {
            (_, true) => "none (--no-plugins)".to_string(),
            (Some(p), _) => p.display().to_string(),
            (None, _) => "no settings file - plugins start in their default state".to_string(),
        },
        opts.out.display(),
        opts.outputs.max(2),
        if opts.outputs <= 2 { " (one mix: a monitored input reaches Main)" } else { "" },
        match inputs_from_score {
            Some(db) => format!("{db:+.1} dB (score)"),
            None => format!("{:+.1} dB (--monitor-db)", opts.monitor_db),
        },
    )
    .ok();

    // Per track: plugins, the dry stem, whether the recorded stem is taken instead.
    let mut finder = finder;
    let n = session.tracks.len();
    let mut plans: Vec<TrackInserts> = Vec::with_capacity(n);
    let mut dry: Vec<Vec<f32>> = Vec::with_capacity(n);
    let mut taken: Vec<Option<Vec<f32>>> = Vec::with_capacity(n);
    for (t, st) in session.tracks.iter().enumerate() {
        let ct = &recorded.tracks[t];
        let entry = mixer.tracks.get(&st.name).cloned();
        let instrument = ct.is_instrument();
        let asked = opts.stems_from_recording.contains(&st.name);
        let lines = if instrument {
            Vec::new()
        } else {
            entry.as_ref().and_then(|e| e.inserts.clone()).unwrap_or_default()
        };
        let side = sidecar.as_ref().and_then(|s| s.tracks.get(&st.name));
        // With --no-plugins the scan cache is still read (never written): for the latencies.
        let needs = lines.iter().any(|l| !l.bypass.unwrap_or(false));
        if needs && finder.is_none() {
            finder = Some(PluginFinder::new(&opts.plugin_bundles)?);
        }
        let plan = plan_inserts(&lines, side, finder.as_ref(), !opts.no_plugins).map_err(|e| {
            format!(
                "Track \"{}\": {e}. Render without plugins with --no-plugins, or take this \
                 track's recorded stem with --stems-from-recording {}.",
                st.name, st.name
            )
        })?;
        plans.push(plan);
        if instrument {
            dry.push(Vec::new());
        } else {
            let name = st.dry.as_deref().ok_or_else(|| {
                format!(
                    "Track \"{}\" has no dry stem in this recording (dry stems were switched off); \
                     the performance cannot be played again without it.",
                    st.name
                )
            })?;
            dry.push(rec.read(name, st.input_channels)?);
        }
        taken.push(if instrument || asked { Some(rec.read(&st.stem, 2)?) } else { None });
    }

    let spec = PerformSpec {
        score: score.clone(),
        sample_rate: rate,
        frames,
        first_abs: session.engine_position,
        count_in_bars: session.count_in_bars.max(1),
        latency: session.tracks.iter().map(|t| t.latency_frames).collect(),
        block,
        outputs: opts.outputs.max(2),
        monitor_gain: db_to_gain(opts.monitor_db, crate::engine::command::MAX_LEVEL_GAIN),
    };

    // First pass: the recorder's weights, from silence.
    let weights = perform(&spec, Pass::Weights)?.weights;

    // Second pass: the reconstructed input, the plugins, the recorder.
    let mut installs = Vec::with_capacity(n);
    for plan in plans.iter_mut() {
        installs.push(plan.start(f64::from(rate))?);
    }
    let layout = Layout {
        tracks: recorded.tracks.iter().map(|t| t.track_input().channels()).collect(),
        dry: true,
    };
    let write = |t: usize| opts.only.is_empty() || opts.only.contains(&session.tracks[t].name);
    let mut writers: Vec<Option<WavWriter>> = Vec::with_capacity(n);
    for (t, st) in session.tracks.iter().enumerate() {
        writers.push(if write(t) && taken[t].is_none() {
            let path = opts.out.join(&st.stem);
            Some(WavWriter::create(&path, 2, rate).map_err(|e| format!("{}: {e}", path.display()))?)
        } else {
            None
        });
    }
    let mut dry_writers: Vec<Option<WavWriter>> = Vec::with_capacity(n);
    for (t, st) in session.tracks.iter().enumerate() {
        dry_writers.push(match (&st.dry, opts.write_dry && write(t) && !dry[t].is_empty()) {
            (Some(name), true) => {
                let path = opts.out.join(name);
                Some(
                    WavWriter::create(&path, st.input_channels as u16, rate)
                        .map_err(|e| format!("{}: {e}", path.display()))?,
                )
            }
            _ => None,
        });
    }
    let mix_name = session.mix.clone().unwrap_or_else(|| "Mix.wav".to_string());
    let mut mix_writer = if opts.only.is_empty() {
        let path = opts.out.join(&mix_name);
        Some(WavWriter::create(&path, 2, rate).map_err(|e| format!("{}: {e}", path.display()))?)
    } else {
        None
    };
    let window = ((rate as f64 * LEVEL_WINDOW_SECS) as u64).max(1);
    let sections = rec.sections();
    let mut compare: Vec<Option<Compare>> = Vec::with_capacity(n);
    for (t, st) in session.tracks.iter().enumerate() {
        compare.push(if opts.compare && writers[t].is_some() {
            Some(Compare::new(&rec.dir.join(&st.stem), window, &sections)?)
        } else {
            None
        });
    }
    let mut mix_compare = if opts.compare && mix_writer.is_some() {
        Compare::new(&rec.dir.join(&mix_name), window, &sections).ok()
    } else {
        None
    };
    let mut dry_check: Vec<ResidualAcc> = vec![ResidualAcc::default(); n];
    let mut peaks = vec![0.0f32; n];
    let mut failure: Option<String> = None;
    let dry_ref = &dry;
    let taken_ref = &taken;
    let mut sink = |f: u64, sf: &[f32]| {
        let fi = f as usize;
        let mut mix = [sf[0], sf[1]];
        for t in 0..n {
            let so = layout.stem_offset(t);
            let mut stem = [sf[so], sf[so + 1]];
            if let Some(rec_stem) = &taken_ref[t] {
                // The recorded stem replaces this track's, in the mix as well.
                let r = [rec_stem[fi * 2], rec_stem[fi * 2 + 1]];
                mix[0] += r[0] - stem[0];
                mix[1] += r[1] - stem[1];
                stem = r;
            }
            if let Some(d) = layout.dry_offset(t)
                && !dry_ref[t].is_empty()
            {
                let ch = layout.tracks[t].count();
                for c in 0..ch {
                    dry_check[t].add(sf[d + c], dry_ref[t][fi * ch + c]);
                    if let Some(w) = dry_writers[t].as_mut()
                        && let Err(e) = w.sample(sf[d + c])
                    {
                        failure.get_or_insert(e.to_string());
                    }
                }
            }
            if let Some(w) = writers[t].as_mut() {
                peaks[t] = peaks[t].max(stem[0].abs()).max(stem[1].abs());
                if let Err(e) = w.sample(stem[0]).and_then(|_| w.sample(stem[1])) {
                    failure.get_or_insert(e.to_string());
                }
                if let Some(c) = compare[t].as_mut() {
                    c.add(f, stem);
                }
            }
        }
        if let Some(w) = mix_writer.as_mut()
            && let Err(e) = w.sample(mix[0]).and_then(|_| w.sample(mix[1]))
        {
            failure.get_or_insert(e.to_string());
        }
        if let Some(c) = mix_compare.as_mut() {
            c.add(f, mix);
        }
    };
    let mut poll = {
        let plans = &mut plans;
        move || {
            for p in plans.iter_mut() {
                p.poll_latency();
            }
        }
    };
    let second = perform(
        &spec,
        Pass::Audio {
            source: perform::Source::Recording {
                dry: &dry,
                weights: &weights,
            },
            inserts: installs,
            poll: &mut poll,
            sink: &mut sink,
        },
    );
    drop(poll);
    for plan in plans.iter_mut() {
        plan.stop();
    }
    let second = second?;
    for w in dry_writers.into_iter().flatten() {
        w.finish().map_err(|e| e.to_string())?;
    }
    if let Some(e) = failure {
        return Err(format!("writing the stems: {e}"));
    }

    let mut report = RenderReport {
        weight_mismatches: second.weight_mismatches,
        ..Default::default()
    };
    for (t, st) in session.tracks.iter().enumerate() {
        let label = format!("{:02} {}", st.number, st.name);
        if !write(t) {
            continue;
        }
        if taken[t].is_some() {
            std::fs::copy(rec.dir.join(&st.stem), opts.out.join(&st.stem))
                .map_err(|e| format!("{}: {e}", opts.out.join(&st.stem).display()))?;
            let why = if recorded.tracks[t].is_instrument() {
                "instrument track: its sound comes from its instrument plugin, which is not loaded"
            } else {
                "--stems-from-recording"
            };
            writeln!(log, "{label}: recorded stem taken as it is ({why})").ok();
            report.tracks.push(TrackReport {
                name: st.name.clone(),
                file: st.stem.clone(),
                rendered: false,
                inserts_applied: Vec::new(),
                inserts_skipped: Vec::new(),
                peak: 0.0,
                dry_check: None,
                residual: None,
                sections: Vec::new(),
                level_flags: Vec::new(),
            });
            continue;
        }
        if let Some(w) = writers[t].take() {
            w.finish().map_err(|e| format!("{}: {e}", st.stem))?;
        }
        let source = mixer.tracks.get(&st.name).cloned().unwrap_or_default();
        let (applied, skipped) = describe_inserts(&plans[t], &label, log);
        let dry_res = (!dry[t].is_empty()).then(|| dry_check[t].finish());
        writeln!(
            log,
            "{label}: {} | fader {} | pan {} | inserts: {} | peak {}",
            describe_chain(&source),
            db_text(source.resolve().gain, source.resolve().mute),
            pan_text(score.tracks[t].pan),
            inserts_text(&applied, &skipped),
            dbfs(peaks[t]),
        )
        .ok();
        if let Some(r) = &dry_res {
            writeln!(log, "    input reconstruction (dry stem played again vs recorded): {}", r.text()).ok();
        }
        let mut tr = TrackReport {
            name: st.name.clone(),
            file: st.stem.clone(),
            rendered: true,
            inserts_applied: applied,
            inserts_skipped: skipped,
            peak: peaks[t],
            dry_check: dry_res,
            residual: None,
            sections: Vec::new(),
            level_flags: Vec::new(),
        };
        if let Some(c) = compare[t].take() {
            let done = c.finish();
            print_compare(log, "stem", &done, |s| live_in(&recorded, s, &st.name));
            tr.residual = Some(done.whole);
            tr.sections = done.sections;
            tr.level_flags = done.flags;
        }
        report.tracks.push(tr);
    }
    if let Some(w) = mix_writer.take() {
        w.finish().map_err(|e| format!("{mix_name}: {e}"))?;
        let path = opts.out.join(&mix_name);
        writeln!(log, "{mix_name}: the Main bus as the recorder takes it").ok();
        if let Some(c) = mix_compare.take() {
            let done = c.finish();
            print_compare(log, "mix", &done, |_| false);
            report.mix_residual = Some(done.whole);
            report.mix_level_flags = done.flags;
        }
        report.mix = Some(path);
    } else {
        writeln!(log, "Mix.wav not written: --only writes single stems.").ok();
    }
    if report.weight_mismatches > 0 {
        writeln!(
            log,
            "WARNING: the recorder's weights differed between the two passes on {} frames; the \
             input there is not reconstructed exactly.",
            report.weight_mismatches
        )
        .ok();
    }
    Ok(report)
}

fn describe_inserts(plan: &TrackInserts, label: &str, log: &mut dyn Write) -> (Vec<String>, Vec<String>) {
    let mut applied = Vec::new();
    let mut skipped = Vec::new();
    for p in &plan.plans {
        match p {
            InsertPlan::Loaded {
                name,
                position,
                state_restored,
                shared,
                ..
            } => applied.push(format!(
                "{name} before {}, latency {}{}",
                position.name().unwrap_or("the end"),
                shared.latency(),
                if *state_restored { "" } else { " (default state: none saved)" }
            )),
            InsertPlan::Skipped { name, why, bypassed, .. } => {
                if !bypassed {
                    writeln!(
                        log,
                        "{label}: WARNING - plugin insert {name} SKIPPED ({why}). This stem is NOT \
                         what was heard."
                    )
                    .ok();
                }
                skipped.push(format!("{name} ({why})"));
            }
        }
    }
    (applied, skipped)
}

fn print_compare(log: &mut dyn Write, what: &str, c: &Compared, live: impl Fn(&str) -> bool) {
    writeln!(log, "    compare with the recorded {what}, whole take: {}", c.whole.text()).ok();
    for (section, _, r) in &c.sections {
        writeln!(
            log,
            "      {section:<16} {:<5} {}",
            if live(section) { "live" } else { "" },
            r.text()
        )
        .ok();
    }
    if c.flags.is_empty() {
        writeln!(
            log,
            "    level check: no {LEVEL_WINDOW_SECS} s window differs by more than {LEVEL_TOLERANCE_DB} dB"
        )
        .ok();
    } else {
        writeln!(
            log,
            "    level check: {} window(s) of {LEVEL_WINDOW_SECS} s differ by more than {LEVEL_TOLERANCE_DB} dB:",
            c.flags.len()
        )
        .ok();
        for f in c.flags.iter().take(200) {
            writeln!(
                log,
                "      {:8.2} s  render {:7.1} dBFS  recorded {:7.1} dBFS  ({:+.1} dB)",
                f.at,
                f.render_dbfs,
                f.recorded_dbfs,
                f.render_dbfs - f.recorded_dbfs
            )
            .ok();
        }
        if c.flags.len() > 200 {
            writeln!(log, "      ... and {} more", c.flags.len() - 200).ok();
        }
    }
}

// ---- comparing -------------------------------------------------------------------------------

/// A recorded 24-bit stereo WAV read frame by frame.
struct WavStream {
    reader: BufReader<std::fs::File>,
    buf: [u8; 6],
}

impl WavStream {
    fn open(path: &Path) -> Result<Self, String> {
        let mut file =
            std::fs::File::open(path).map_err(|e| format!("{}: {e}", path.display()))?;
        let mut head = [0u8; HEADER_BYTES as usize];
        file.read_exact(&mut head).map_err(|e| format!("{}: {e}", path.display()))?;
        Ok(Self {
            reader: BufReader::with_capacity(1 << 18, file),
            buf: [0; 6],
        })
    }

    fn next(&mut self) -> [f32; 2] {
        if self.reader.read_exact(&mut self.buf).is_err() {
            return [0.0; 2];
        }
        let s = |b: &[u8]| (i32::from_le_bytes([0, b[0], b[1], b[2]]) >> 8) as f32 / 8_388_607.0;
        [s(&self.buf[0..3]), s(&self.buf[3..6])]
    }
}

/// Residual of a rendered signal against a recorded one, accumulated sample by sample. The
/// rendered one is quantised to 24 bit first, as it is written, so "bit-exact" means the same file
/// contents.
#[derive(Clone, Copy, Debug, Default)]
pub struct ResidualAcc {
    diff: f64,
    reference: f64,
    max: i32,
    n: u64,
}

impl ResidualAcc {
    pub fn add(&mut self, rendered: f32, recorded: f32) {
        let (qa, qb) = (to_i24(rendered), to_i24(recorded));
        self.max = self.max.max((qa - qb).abs());
        let d = f64::from(qa - qb) / 8_388_607.0;
        let r = f64::from(qb) / 8_388_607.0;
        self.diff += d * d;
        self.reference += r * r;
        self.n += 1;
    }

    pub fn finish(&self) -> Residual {
        let n = self.n.max(1) as f64;
        let dbfs = |e: f64| if e == 0.0 { f64::NEG_INFINITY } else { 10.0 * (e / n).log10() };
        Residual {
            rel_db: (self.reference > 0.0).then(|| {
                if self.diff == 0.0 {
                    f64::NEG_INFINITY
                } else {
                    10.0 * (self.diff / self.reference).log10()
                }
            }),
            residual_dbfs: dbfs(self.diff),
            stem_dbfs: dbfs(self.reference),
            max_lsb: self.max as u32,
        }
    }
}

/// The result of comparing two signals.
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
    /// `a` rendered, `b` recorded, stereo interleaved, over frames `[from, to)`.
    pub fn of(a: &[f32], b: &[f32], from: u64, to: u64) -> Self {
        let mut acc = ResidualAcc::default();
        let lo = from as usize * 2;
        let hi = (to as usize * 2).min(a.len()).min(b.len()).max(lo);
        for i in lo..hi {
            acc.add(a[i], b[i]);
        }
        acc.finish()
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

/// Level of an energy sum over `n` samples, dBFS, floored at -120.
fn level_db(energy: f64, n: u64) -> f64 {
    if energy <= 0.0 || n == 0 {
        return -120.0;
    }
    (10.0 * (energy / n as f64).log10()).max(-120.0)
}

/// Whether two window levels differ enough to be listed.
pub fn level_flagged(render_dbfs: f64, recorded_dbfs: f64) -> bool {
    (render_dbfs - recorded_dbfs).abs() > LEVEL_TOLERANCE_DB
        && render_dbfs.max(recorded_dbfs) > LEVEL_FLOOR_DBFS
}

/// A stereo signal compared frame by frame with a recorded file: whole, per section, and per
/// window in level.
struct Compare {
    recorded: WavStream,
    whole: ResidualAcc,
    sections: Vec<(String, u64, u64, ResidualAcc)>,
    window: u64,
    win_a: f64,
    win_b: f64,
    win_n: u64,
    win_start: u64,
    rate: f64,
    flags: Vec<LevelFlag>,
}

struct Compared {
    whole: Residual,
    sections: Vec<(String, bool, Residual)>,
    flags: Vec<LevelFlag>,
}

impl Compare {
    fn new(path: &Path, window: u64, sections: &[(String, u64, u64)]) -> Result<Self, String> {
        Ok(Self {
            recorded: WavStream::open(path)?,
            whole: ResidualAcc::default(),
            sections: sections
                .iter()
                .map(|(s, a, b)| (s.clone(), *a, *b, ResidualAcc::default()))
                .collect(),
            window,
            win_a: 0.0,
            win_b: 0.0,
            win_n: 0,
            win_start: 0,
            rate: window as f64 / LEVEL_WINDOW_SECS,
            flags: Vec::new(),
        })
    }

    fn add(&mut self, f: u64, rendered: [f32; 2]) {
        let recorded = self.recorded.next();
        for c in 0..2 {
            self.whole.add(rendered[c], recorded[c]);
            for (_, a, b, acc) in self.sections.iter_mut() {
                if f >= *a && f < *b {
                    acc.add(rendered[c], recorded[c]);
                }
            }
            let qa = f64::from(to_i24(rendered[c])) / 8_388_607.0;
            self.win_a += qa * qa;
            self.win_b += f64::from(recorded[c]) * f64::from(recorded[c]);
        }
        self.win_n += 2;
        if self.win_n >= 2 * self.window {
            self.close_window();
            self.win_start = f + 1;
        }
    }

    fn close_window(&mut self) {
        if self.win_n == 0 {
            return;
        }
        let (a, b) = (level_db(self.win_a, self.win_n), level_db(self.win_b, self.win_n));
        if level_flagged(a, b) {
            self.flags.push(LevelFlag {
                at: self.win_start as f64 / self.rate,
                render_dbfs: a,
                recorded_dbfs: b,
            });
        }
        self.win_a = 0.0;
        self.win_b = 0.0;
        self.win_n = 0;
    }

    fn finish(mut self) -> Compared {
        self.close_window();
        Compared {
            whole: self.whole.finish(),
            sections: self
                .sections
                .iter()
                .map(|(s, _, _, acc)| (s.clone(), false, acc.finish()))
                .collect(),
            flags: self.flags,
        }
    }
}

/// Whether `track`'s live input was heard in `section`: a take, hear-through, `monitor: true`, or
/// `monitor: always`.
fn live_in(score: &CompiledScore, section: &str, track: &str) -> bool {
    let Some(s) = score.sections.iter().find(|s| s.id == section) else {
        return false;
    };
    let state = s.tracks.get(track).copied();
    let takes = matches!(
        state,
        Some(TrackState::Record | TrackState::Overdub | TrackState::Stack | TrackState::HearThrough)
    );
    let always = score.tracks.iter().any(|t| t.name == track && t.monitor_always);
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
        format!(
            "preset {preset}{}: {}",
            if source.has_effect_lines() { " (+ overrides)" } else { "" },
            on.join(", ")
        )
    }
}

fn db_text(gain: f32, mute: bool) -> String {
    if mute {
        return "muted".to_string();
    }
    format!("{:+.1} dB", gain_to_db(gain))
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
