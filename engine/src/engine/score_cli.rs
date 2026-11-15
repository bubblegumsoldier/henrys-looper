//! `score` subcommand: play a written score on real hardware.
//!
//! Everything musical is decided by the score - tempo, time signature, tracks, inputs, section
//! lengths, quantisation. The flags left on the command line are the ones the *machine* needs and
//! the score cannot know: which device, which buffer size, what the latency compensation is.
//!
//! The engine is built **from** the score, which is why nothing here has to reconfigure a running
//! one. [`super::runner::check_tracks`] is the rule for the case that will exist once a UI can load
//! a second score into a session that is already playing: it refuses rather than rewires.
//!
//! Only the terminal lives in this file. The state machine is [`super::runner::Runner`], and it has
//! no idea a terminal exists - which is what lets the whole of phase 3 be proven offline.

use std::path::PathBuf;
use std::sync::atomic::Ordering;
use std::sync::mpsc::RecvTimeoutError;
use std::time::{Duration, Instant};

use clap::Args;

use crate::audio::{self, DeviceOpts};
use crate::meter::fmt_dbfs;
use crate::score::{CompiledScore, ScoreError, TrackState as ScoreState, compile_score};

use super::bus::{Bus, routing_note};
use super::command::{Command, Refusal, Status, TrackStatus};
use super::live::{
    DISPLAY_INTERVAL, EngineSpec, RunningEngine, TrackDef, apply_track_latencies, bus_text, draw,
    fx_text, input_text, latency_origin, latency_text, layer_list, on_off, pan_label, resolve_bus_gains,
    resolve_routing, send_text, spawn_keyboard, start_engine, stats_report,
};
use super::process::{check_loop, loop_capacity, max_latency};
use super::runner::{DEFAULT_COUNT_IN_BARS, Phase, Runner, check_tracks};
use super::timeline::Timeline;
use super::track::TrackState;

#[derive(Args, Debug, Clone)]
pub struct ScoreOpts {
    /// Path to the score (YAML)
    #[arg(value_name = "FILE")]
    pub file: PathBuf,

    /// Round-trip latency in frames that is compensated when recording - the default for
    /// every track that has no value of its own in the score.
    #[arg(long = "latency-frames", alias = "latency-samples", default_value_t = audio::DEFAULT_LATENCY_FRAMES)]
    pub latency_frames: u64,

    /// Latency compensation of one track: NAME:FRAMES[+TRIM] or NAME:+TRIM.
    /// Overrides what the score says.
    #[arg(long = "track-latency", value_name = "NAME:FRAMES[+TRIM]")]
    pub track_latencies: Vec<String>,

    /// Count-in bars before the first section
    #[arg(long = "count-in", default_value_t = DEFAULT_COUNT_IN_BARS)]
    pub count_in: u32,

    /// Gain of the monitored input signal
    #[arg(long, default_value_t = 1.0)]
    pub monitor_gain: f32,

    /// Click level
    #[arg(long, default_value_t = 1.0)]
    pub click_gain: f32,

    /// Output pair of a bus: BUS:CHANNEL[-CHANNEL] with BUS = main or monitor,
    /// repeatable. The click is always and only on the Monitor bus.
    #[arg(long = "bus-out", value_name = "BUS:CHANNEL[-CHANNEL]")]
    pub bus_outs: Vec<String>,

    /// Level of a bus: BUS:VALUE, repeatable.
    #[arg(long = "bus-gain", value_name = "BUS:VALUE")]
    pub bus_gains: Vec<String>,

    /// Start without click (the count-in is then silent)
    #[arg(long)]
    pub no_click: bool,

    /// Only compile the score and print its layout; no device opened, no sound
    #[arg(long)]
    pub check: bool,

    /// Display without cursor control: every update is appended instead of overwritten
    #[arg(long)]
    pub simple_display: bool,
}

/// Read and compile a score, turning every compiler issue into a positioned German line.
///
/// The compiler already carries line, column, message and suggestion per issue and reports **all**
/// of them at once; this only prefixes the file name so the output can be pasted into an editor.
pub fn load_score(path: &PathBuf) -> Result<CompiledScore, String> {
    let text = std::fs::read_to_string(path)
        .map_err(|e| format!("Cannot read score \"{}\": {e}", path.display()))?;
    compile_score(&text).map_err(|error| render_errors(path, &error))
}

fn render_errors(path: &PathBuf, error: &ScoreError) -> String {
    let mut out = format!(
        "{} error(s) in \"{}\":",
        error.issues.len(),
        path.display()
    );
    for issue in &error.issues {
        out.push_str("\n  ");
        match (issue.line, issue.column) {
            (Some(line), Some(column)) => {
                out.push_str(&format!("{}:{line}:{column}  ", path.display()))
            }
            (Some(line), None) => out.push_str(&format!("{}:{line}  ", path.display())),
            _ => {}
        }
        out.push_str(&issue.message);
        if let Some(suggestion) = &issue.suggestion {
            out.push_str(&format!("  -> {suggestion}"));
        }
    }
    out
}

/// The tracks of a score as the engine wants them.
pub fn track_defs(score: &CompiledScore) -> Vec<TrackDef> {
    score
        .tracks
        .iter()
        .map(|track| TrackDef {
            name: track.name.clone(),
            input: track.track_input(),
            pan: track.pan,
            latency: track.track_latency(),
            loop_send: track.loop_send(),
            monitor_send: track.monitor_send(),
        })
        .collect()
}

/// The header: which section, where inside it, and what is armed.
fn section_line(runner: &Runner, view: &super::runner::RunnerView, status: &Status) -> String {
    let head = match view.section {
        Some(index) => format!(
            "Section {}/{} \"{}\"  Bar {}/{}  Beat {}  Pass {}",
            index + 1,
            view.section_count,
            view.section_id,
            view.bar,
            view.bars_total,
            view.beat,
            view.pass
        ),
        None => format!("[{}]", view.phase.label()),
    };
    let armed = match &view.armed {
        Some(a) => a.text(),
        None if view.phase == Phase::Running => "no switch armed".to_string(),
        None => String::new(),
    };
    format!(
        "{head} | {armed} | {:.1} BPM {} | Click {} | {} | {}",
        status.bpm,
        runner.score().time_signature,
        on_off(status.click),
        bus_text(status, Bus::Main),
        bus_text(status, Bus::Monitor),
    )
}

/// One line per track: what the score asks of it, what the engine is actually doing, and the
/// numbers that say whether that is going well.
fn track_line(
    name: &str,
    def: &TrackDef,
    want: ScoreState,
    ts: &TrackStatus,
    lead: super::schedule::Lead,
    timeline: &Timeline,
) -> String {
    let engine_state = match lead.text() {
        Some(text) => format!("{} - {text}", ts.state.label()),
        None => ts.state.label().to_string(),
    };
    let loop_text = if ts.loop_len > 0 {
        format!("{:.2} s", timeline.samples_to_secs(ts.loop_len))
    } else if ts.state == TrackState::Recording {
        format!("running, {:.2} s", timeline.samples_to_secs(ts.filled))
    } else {
        "-".to_string()
    };
    let level = match ts.channels {
        2 => format!(
            "L {} R {}",
            fmt_dbfs(ts.input_peak[0]),
            fmt_dbfs(ts.input_peak[1])
        ),
        _ => format!("{}      ", fmt_dbfs(ts.input_peak[0])),
    };
    format!(
        "  {:<10} {} | Score {:<11} | Engine {:<32} | Loop {:>10} | Layers {:>2} {:<20} | Level {} | Pan {:<5} | Bus {:<5} | Lat {:<14} | Monitor {} | FX {:<16}",
        name,
        input_text(def.input),
        want.label(),
        engine_state,
        loop_text,
        ts.layers,
        layer_list(ts, &[]),
        level,
        pan_label(ts.pan),
        send_text(ts),
        latency_text(ts),
        on_off(ts.monitor),
        fx_text(&ts.fx),
    )
}

/// Print what the score says, before anything is opened. Also the whole of `--check`.
fn print_score(score: &CompiledScore, defs: &[TrackDef], default_latency: u64, rate: u32) {
    println!("Score:    \"{}\"", score.title);
    println!(
        "Tempo:    {:.1} BPM, {}, {} sections, {} bars in total",
        score.bpm,
        score.time_signature,
        score.sections.len(),
        score.total_bars()
    );
    println!("Tracks:");
    for (track, def) in score.tracks.iter().zip(defs) {
        let effective = def.latency.resolve(default_latency);
        println!(
            "  {} {:<10} input {} ({}), pan {}, Monitor {}, latency {} frames ({:.2} ms, {}), Loop -> {}, Monitor bus {}",
            track.index + 1,
            track.name,
            def.input.label(),
            def.input.channels().label(),
            pan_label(def.pan),
            if track.monitor { "allowed" } else { "off" },
            effective,
            effective as f64 * 1000.0 / rate.max(1) as f64,
            latency_origin(def.latency, default_latency),
            def.loop_send.label(),
            def.monitor_send.label(),
        );
    }
    println!("Sections:");
    for section in &score.sections {
        let states: Vec<String> = score
            .tracks
            .iter()
            .map(|t| {
                format!(
                    "{}={}",
                    t.name,
                    section
                        .tracks
                        .get(&t.name)
                        .copied()
                        .unwrap_or(ScoreState::Stop)
                        .as_str()
                )
            })
            .collect();
        println!(
            "  {:>2} {:<14} {:>2} bars  {:<24} {}",
            section.index + 1,
            section.id,
            section.bars,
            if section.autorelease {
                "autorelease".to_string()
            } else {
                format!("Release, grid {}", section.quantize.label())
            },
            states.join("  ")
        );
    }
}

pub fn cmd_score(dev: &DeviceOpts, opts: &ScoreOpts) -> Result<(), String> {
    let score = load_score(&opts.file)?;
    let mut defs = track_defs(&score);
    apply_track_latencies(&mut defs, &opts.track_latencies)?;

    if opts.check {
        print_score(&score, &defs, opts.latency_frames, 48_000);
        println!("\nScore compiled, no errors. No device opened, no sound.");
        return Ok(());
    }

    let setup = audio::open_duplex(dev)?;
    let rate = setup.sample_rate();
    if setup.out_plan.config.sample_rate != rate {
        return Err(format!(
            "Input and output run at different sample rates ({} and {}). \
             The engine's timeline needs a shared clock.",
            rate, setup.out_plan.config.sample_rate
        ));
    }
    let signature = score.signature();
    Timeline::validate(rate, score.bpm, signature)?;
    let timeline = Timeline::new(rate, score.bpm, signature);

    let in_channels = setup.in_plan.config.channels as usize;
    for def in &defs {
        if def.input.highest() >= in_channels {
            return Err(format!(
                "Track \"{}\" listens to input {} according to the score, but the device only provides {} \
                 input channels.\n\
                 \x20 - enter a different input in the score, or\n\
                 \x20 - open more channels: --in-channels {}",
                def.name,
                def.input.highest() + 1,
                in_channels,
                def.input.highest() + 1
            ));
        }
    }
    // The engine is built from the score, so this can only fail if the two ever drift apart - which
    // is exactly the check a UI will need when it loads a score into a running session.
    check_tracks(&score, &defs)?;

    // Layer buffers are allocated at the longest section of the score, because that is the longest
    // loop any take in it can define.
    let max_bars = score.sections.iter().map(|s| s.bars).max().unwrap_or(1).max(1);
    let worst_latency = max_latency(opts.latency_frames, defs.iter().map(|d| d.latency));
    check_loop(&timeline, max_bars, worst_latency)?;

    audio::print_setup(&setup);
    print_score(&score, &defs, opts.latency_frames, rate);
    let capacity = loop_capacity(&timeline, max_bars);
    println!(
        "Layers:   buffers for the longest section ({max_bars} bars = {capacity} frames = {:.2} s)",
        timeline.samples_to_secs(capacity)
    );

    let out_channels = setup.out_plan.config.channels as usize;
    let routing = resolve_routing(&opts.bus_outs, out_channels)?;
    let bus_gain = resolve_bus_gains(&opts.bus_gains)?;
    println!(
        "Buses:    Main -> output {} (x{:.2}), Monitor -> output {} (x{:.2}), of {} channels. \
         The click is on Monitor and never on Main.",
        routing.get(Bus::Main).label(),
        bus_gain[Bus::Main.index()],
        routing.get(Bus::Monitor).label(),
        bus_gain[Bus::Monitor.index()],
        out_channels
    );
    if let Some(note) = routing_note(&routing, out_channels) {
        println!("WARNING:  {note}");
    }

    let RunningEngine {
        input,
        output,
        mut cmd,
        status: mut status_rx,
        mut pool,
        stats,
        buffer_frames,
    } = start_engine(EngineSpec {
        setup: &setup,
        timeline,
        bars: max_bars,
        defs: &defs,
        latency_frames: opts.latency_frames,
        // Monitoring is the runner's business: it belongs to `hear_through` and to the takes, and
        // the score says which tracks may have it at all.
        monitor: false,
        monitor_gain: opts.monitor_gain,
        click: !opts.no_click,
        click_gain: opts.click_gain,
        bus_gain,
        routing,
    })?;

    // The score's `mixer:` section: faders, mutes, chains and bus levels, by track name.
    if let Some(mixer) = &score.mixer {
        let names: Vec<String> = score.tracks.iter().map(|t| t.name.clone()).collect();
        for command in crate::score::mixer::mixer_commands(mixer, &names, &[]) {
            if let Err(e) = cmd.send(command) {
                println!("WARNING:  the score's mix was not applied completely: {e}");
                break;
            }
        }
    }

    let mut runner = Runner::new(score, timeline, buffer_frames)?.with_count_in(opts.count_in);

    println!(
        "\nKeys (confirm each with Enter):\n\
         \x20 <empty> or n   trigger the next section (the Release button)\n\
         \x20 g <no>         jump to section <no> (only while no switch is armed)\n\
         \x20 s              Stop all tracks\n\
         \x20 k              Click on/off\n\
         \x20 q              quit\n\
         The count-in starts as soon as the first status report arrives.\n"
    );

    let keys = spawn_keyboard();
    let mut last: Option<(Status, Instant)> = None;
    let mut last_print = Instant::now() - DISPLAY_INTERVAL;
    let mut printed_lines = 0usize;
    let mut last_refusal = (0u64, Refusal::None);
    let mut running = true;
    let mut started = false;
    let mut message = String::new();

    while running {
        if let Some(s) = status_rx.latest() {
            if s.stopped {
                running = false;
            }
            if s.ignored_commands > last_refusal.0 || s.refusal != last_refusal.1 {
                if let Some(text) = s.refusal.message() {
                    message = text.to_string();
                }
                last_refusal = (s.ignored_commands, s.refusal);
            }
            last = Some((s, Instant::now()));
        }
        // The only place buffers are allocated, zeroed and dropped.
        pool.service(last.as_ref().map(|(s, _)| s));

        let est = runner.estimated_pos(last.map(|(s, seen)| (s.pos, seen.elapsed())));
        if !started && last.is_some() {
            started = true;
            message = runner.start(est);
        }
        if let Some((s, _)) = last {
            runner.tick(s.pos);
            if let Some(notice) = runner.take_notices().pop() {
                message = notice;
            }
        }

        match keys.recv_timeout(Duration::from_millis(20)) {
            Ok(line) => {
                let lowered = line.trim().to_lowercase();
                let mut parts = lowered.split_whitespace();
                match parts.next().unwrap_or("") {
                    "" | "n" => message = runner.next(est),
                    "g" => match parts.next().and_then(|n| n.parse::<usize>().ok()) {
                        Some(n) if n >= 1 => message = runner.goto(n - 1, est),
                        _ => message = "Usage: g <no>  (section number, from 1)".to_string(),
                    },
                    "s" => message = runner.stop_all(est),
                    "k" => {
                        let on = !last.map(|(s, _)| s.click).unwrap_or(true);
                        let _ = cmd.send(Command::SetClick { on });
                        message = format!("Click {}.", if on { "on" } else { "off" });
                    }
                    "q" => {
                        let _ = cmd.send(Command::Stop);
                        running = false;
                        message = "Quitting.".to_string();
                    }
                    other => {
                        message =
                            format!("Unknown input \"{other}\". Keys: <empty>/n g <no> s k q")
                    }
                }
            }
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => break,
        }

        for command in runner.take_commands() {
            if let Err(e) = cmd.send(command) {
                message = e;
                running = false;
            }
        }

        if last_print.elapsed() >= DISPLAY_INTERVAL {
            last_print = Instant::now();
            if let Some((s, _)) = last {
                let view = runner.view(s.pos);
                let mut lines = vec![section_line(&runner, &view, &s)];
                for (i, def) in defs.iter().enumerate() {
                    let ts = s.tracks().get(i).copied().unwrap_or_default();
                    let want = view.tracks.get(i).copied().unwrap_or(ScoreState::Stop);
                    lines.push(track_line(
                        &def.name,
                        def,
                        want,
                        &ts,
                        runner.scheduler().lead(i, s.pos),
                        &timeline,
                    ));
                }
                lines.push(format!("  {message}"));
                draw(&lines, printed_lines, opts.simple_display);
                printed_lines = lines.len();
            }
        }
    }

    drop(input);
    drop(output);
    pool.drain();
    println!("\n");
    println!("{}", stats_report(&stats));
    println!("Xruns total:         {}", stats.xruns.load(Ordering::Relaxed));
    Ok(())
}
