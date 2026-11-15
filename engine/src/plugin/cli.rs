//! `looper-engine plugins …` - the plugin host without the app: scan, probe one plugin offline,
//! open one editor window. A measuring tool like the rest of the CLI; no device is opened.

use clap::Subcommand;

use super::scan::{self, Scanned};

#[derive(Subcommand, Debug)]
pub enum PluginOpts {
    /// Find all plugins (VST3 and Audio Units), each probed in a child process; uses the cache
    Scan {
        /// Ignore the cache and probe everything again
        #[arg(long)]
        full: bool,
    },
    /// Load one plugin here, list its parameters and push a second of sine through it
    Probe {
        /// Id (`au:aufx:rvb2:appl`, `vst3:…`) or part of the name
        plugin: String,
    },
    /// Open one plugin's own editor window for a few seconds (macOS)
    Edit {
        /// Id or part of the name
        plugin: String,
        /// How long the window stays open
        #[arg(long, default_value_t = 5.0)]
        seconds: f64,
        /// Halfway through, resize the window as a user would, to WIDTHxHEIGHT points
        #[arg(long)]
        resize: Option<String>,
        /// Draw the editor into this PNG file at the end (no screen capture)
        #[arg(long)]
        png: Option<std::path::PathBuf>,
        /// Close and open the window this many times before the timed part
        #[arg(long, default_value_t = 0)]
        reopen: u32,
    },
    /// Check the editor window's sizing rules with stand-in views (macOS, opens windows briefly)
    EditorSelftest,
}

pub fn cmd_plugins(opts: &PluginOpts) -> Result<(), String> {
    match opts {
        PluginOpts::Scan { full } => {
            let exe = std::env::current_exe().map_err(|e| e.to_string())?;
            let list = scan::scan(&exe, *full, |label| eprintln!("  probing {label} ..."));
            for p in &list {
                println!(
                    "{:<10} {:<4} {:<34} {:<22} {:>3} params  {:>5} lat  {}",
                    format!("{:?}", p.status).to_lowercase(),
                    p.info.format.label(),
                    p.info.name,
                    p.info.vendor,
                    p.params,
                    p.latency,
                    p.info.id
                );
                if let Some(m) = &p.message
                    && p.status != scan::Status::Ok
                {
                    println!("           {m}");
                }
            }
            println!("{} plugins, cache: {}", list.len(), scan::cache_path().display());
            Ok(())
        }
        PluginOpts::Probe { plugin } => probe(&find(plugin)?),
        PluginOpts::EditorSelftest => editor_selftest(),
        PluginOpts::Edit {
            plugin,
            seconds,
            resize,
            png,
            reopen,
        } => edit(
            &find(plugin)?,
            &EditOpts {
                seconds: *seconds,
                resize: resize.clone(),
                png: png.clone(),
                reopen: *reopen,
            },
        ),
    }
}

fn find(query: &str) -> Result<Scanned, String> {
    let mut list = scan::cached();
    if list.is_empty() {
        let exe = std::env::current_exe().map_err(|e| e.to_string())?;
        list = scan::scan(&exe, false, |_| {});
    }
    let q = query.to_lowercase();
    list.iter()
        .find(|p| p.info.id == query)
        .or_else(|| list.iter().find(|p| p.info.name.to_lowercase() == q))
        .or_else(|| list.iter().find(|p| p.info.name.to_lowercase().contains(&q)))
        .cloned()
        .ok_or_else(|| format!("No plugin matches '{query}'. Run `plugins scan` first."))
}

fn probe(found: &Scanned) -> Result<(), String> {
    let mut plugin = super::load(&found.info)?;
    println!("{} ({}, {})", found.info.name, found.info.vendor, found.info.format.label());
    for p in plugin.params() {
        let v = plugin.param(p.id);
        println!(
            "  {:>6}  {:<32} {:>6.3}  {:<16} {}{}",
            p.id,
            p.name,
            v,
            plugin.param_text(p.id, v),
            if p.steps > 0 { format!("{} steps ", p.steps) } else { String::new() },
            if p.read_only { "read-only" } else { "" }
        );
    }
    let mut processor = plugin.activate(48_000.0, 512)?;
    println!("  latency: {} frames", plugin.latency());
    let mut l = vec![0.0f32; 480];
    let mut r = vec![0.0f32; 480];
    let (mut sum_in, mut sum_out, mut n) = (0.0f64, 0.0f64, 0usize);
    for block in 0..100 {
        for (i, (a, b)) in l.iter_mut().zip(r.iter_mut()).enumerate() {
            let t = (block * 480 + i) as f32 / 48_000.0;
            *a = (t * 440.0 * std::f32::consts::TAU).sin() * 0.25;
            *b = *a;
            sum_in += (*a as f64).powi(2) * 2.0;
        }
        processor.process(&mut l, &mut r);
        for (a, b) in l.iter().zip(r.iter()) {
            sum_out += (*a as f64).powi(2) + (*b as f64).powi(2);
        }
        n += 960;
    }
    drop(processor);
    plugin.deactivate();
    let db = |s: f64| 10.0 * (s / n as f64).max(1e-12).log10();
    println!(
        "  1 s sine 440 Hz: in {:.1} dBFS RMS, out {:.1} dBFS RMS",
        db(sum_in),
        db(sum_out)
    );
    let state = plugin.save_state()?;
    println!("  state: {} bytes", state.len());
    Ok(())
}

#[cfg(target_os = "macos")]
fn editor_selftest() -> Result<(), String> {
    use objc2::MainThreadMarker;
    use objc2_app_kit::NSApplication;
    let mtm = MainThreadMarker::new().ok_or("not on the main thread")?;
    NSApplication::sharedApplication(mtm).finishLaunching();
    match super::editor::self_test() {
        Ok(lines) => {
            lines.iter().for_each(|l| println!("{l}"));
            println!("editor window: all checks passed");
            Ok(())
        }
        Err(e) => Err(format!("editor window check failed: {e}")),
    }
}

#[cfg(not(target_os = "macos"))]
fn editor_selftest() -> Result<(), String> {
    Err(super::UNSUPPORTED.to_string())
}

struct EditOpts {
    seconds: f64,
    resize: Option<String>,
    png: Option<std::path::PathBuf>,
    reopen: u32,
}

#[cfg(target_os = "macos")]
fn edit(found: &Scanned, opts: &EditOpts) -> Result<(), String> {
    use objc2::MainThreadMarker;
    use objc2_app_kit::{NSApplication, NSApplicationActivationPolicy};

    let mtm = MainThreadMarker::new().ok_or("not on the main thread")?;
    let app = NSApplication::sharedApplication(mtm);
    app.setActivationPolicy(NSApplicationActivationPolicy::Regular);
    app.finishLaunching();
    let resize = match &opts.resize {
        Some(text) => {
            let (w, h) = text
                .split_once('x')
                .and_then(|(w, h)| Some((w.parse::<f64>().ok()?, h.parse::<f64>().ok()?)))
                .ok_or_else(|| format!("--resize wants WIDTHxHEIGHT, not '{text}'"))?;
            Some((w, h))
        }
        None => None,
    };
    let started = std::time::Instant::now();
    let mut plugin = super::load(&found.info)?;
    println!("loaded in {:?}", started.elapsed());
    let instrument = found.info.kind == super::Kind::Instrument;
    let mut effect = None;
    let mut synth = None;
    if instrument {
        synth = Some(plugin.activate_instrument(48_000.0, 512)?);
    } else {
        effect = Some(plugin.activate(48_000.0, 512)?);
    }
    let report = |plugin: &dyn super::PluginInstance, when: &str| {
        println!(
            "[{when}] open = {}, view {:?}: {}",
            plugin.editor_open(),
            plugin.editor_size(),
            plugin.editor_report().unwrap_or_default()
        );
    };
    let opened = std::time::Instant::now();
    plugin.open_editor(&found.info.name, "cli")?;
    println!("editor opened in {:?}", opened.elapsed());
    report(plugin.as_ref(), "open");
    for i in 0..opts.reopen {
        super::editor::pump_events(0.2);
        plugin.close_editor();
        report(plugin.as_ref(), &format!("closed {}", i + 1));
        super::editor::pump_events(0.2);
        plugin.open_editor(&found.info.name, "cli")?;
        report(plugin.as_ref(), &format!("open again {}", i + 1));
    }
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs_f64(opts.seconds);
    let half = std::time::Instant::now() + std::time::Duration::from_secs_f64(opts.seconds / 2.0);
    let mut resized = resize.is_none();
    let mut reported = false;
    while std::time::Instant::now() < deadline {
        super::editor::pump_events(0.05);
        if !reported && std::time::Instant::now() + std::time::Duration::from_secs(1) > half {
            report(plugin.as_ref(), "settled");
            reported = true;
        }
        if !resized && std::time::Instant::now() > half {
            let (w, h) = resize.unwrap();
            plugin.editor_resize_as_user(w, h);
            report(plugin.as_ref(), &format!("user resize to {w}x{h}"));
            super::editor::pump_events(0.5);
            report(plugin.as_ref(), "after resize");
            resized = true;
        }
        let changed = plugin.take_changed();
        if !changed.is_empty() {
            println!("  changed in the editor: {} parameters", changed.len());
        }
    }
    report(plugin.as_ref(), "end");
    if let Some(path) = &opts.png {
        match plugin.editor_snapshot_png() {
            Some(png) => {
                std::fs::write(path, &png).map_err(|e| e.to_string())?;
                println!("editor drawn into {} ({} bytes)", path.display(), png.len());
            }
            None => println!("the editor could not be drawn into a picture"),
        }
    }
    plugin.close_editor();
    println!("closed: editor window open = {}", plugin.editor_open());
    drop(effect);
    drop(synth);
    plugin.deactivate();
    drop(plugin);
    super::shutdown();
    Ok(())
}

#[cfg(not(target_os = "macos"))]
fn edit(_found: &Scanned, _opts: &EditOpts) -> Result<(), String> {
    Err(super::UNSUPPORTED.to_string())
}
