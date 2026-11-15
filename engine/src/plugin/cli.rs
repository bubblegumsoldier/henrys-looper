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
    },
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
        PluginOpts::Edit { plugin, seconds } => edit(&find(plugin)?, *seconds),
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
fn edit(found: &Scanned, seconds: f64) -> Result<(), String> {
    use objc2::MainThreadMarker;
    use objc2_app_kit::{NSApplication, NSApplicationActivationPolicy};

    let mtm = MainThreadMarker::new().ok_or("not on the main thread")?;
    let app = NSApplication::sharedApplication(mtm);
    app.setActivationPolicy(NSApplicationActivationPolicy::Regular);
    app.finishLaunching();
    let mut plugin = super::load(&found.info)?;
    let processor = plugin.activate(48_000.0, 512)?;
    plugin.open_editor(&found.info.name)?;
    println!(
        "{}: editor window open = {}, view {:?} points",
        found.info.name,
        plugin.editor_open(),
        plugin.editor_size()
    );
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs_f64(seconds);
    while std::time::Instant::now() < deadline {
        super::editor::pump_events(0.05);
        let changed = plugin.take_changed();
        if !changed.is_empty() {
            println!("  changed in the editor: {changed:?}");
        }
    }
    plugin.close_editor();
    println!("closed: editor window open = {}", plugin.editor_open());
    drop(processor);
    plugin.deactivate();
    Ok(())
}

#[cfg(not(target_os = "macos"))]
fn edit(_found: &Scanned, _seconds: f64) -> Result<(), String> {
    Err(super::UNSUPPORTED.to_string())
}
