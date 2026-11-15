//! Henrys Looper - the desktop app.
//!
//! Everything of substance is in the library crate `looper-engine`; this binary is the window in
//! front of it. Three files carry it:
//!
//! * [`host`] - the OS thread that owns the audio engine, and the only place cpal is touched.
//! * [`proto`] - the wire format between here and the web view.
//! * [`schedule`] - turning a user action into timed engine commands, provable without a device.
//!
//! Every command below is `async` and does nothing but hand work to the host thread through
//! `spawn_blocking`. That is rule 3 from `docs/architektur.md` section 7: the Tauri main thread is
//! MAINSTA and drives the window's message pump, so blocking it freezes the window.
//!
//! Errors are `Result<_, String>` with whole sentences, because they end up in front of a
//! musician. Where the engine already has a sentence for a situation, that sentence is used.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod files;
mod devshot;
mod host;
mod logfile;
mod layout;
mod midi;
mod mixstore;
mod instruments;
mod pluginmix;
mod plugins;
mod proto;
mod recording;
mod schedule;

use tauri::{Emitter, Manager, State};

use host::{Action, EngineHandle, READY_EVENT, ScoreAction};
use logfile::log;
use midi::{BindingOptions, MidiPortView, MidiSaved, MidiView, ProfileAction};
use looper_engine::engine::bus::{Bus, TrackSource};
use looper_engine::engine::fx::{EQ_BANDS as MAX_EQ_BANDS, FxParam as EngineFxParam};
use looper_engine::engine::track::TrackLatency as EngineTrackLatency;
use looper_engine::score::ScoreDoc;
use looper_engine::score::mixer::MixerSource;
use looper_engine::score::doc::{ParseOutcome, RenderOutcome};
use proto::{
    AppInfo, BandKindName, BusOutConfig, CalibrateConfig, CalibrateOutcome, CompileOutcome,
    DelayNoteName,
    DeviceReport, EngineInfo, FxParamName, FxPresetName, FxSlotName, OverdubMode, QuantizeName, ScoreLoaded,
    StartConfig,
};

/// Run blocking work on Tauri's blocking pool and turn a lost worker into a sentence.
async fn offload<T, F>(work: F) -> Result<T, String>
where
    T: Send + 'static,
    F: FnOnce() -> Result<T, String> + Send + 'static,
{
    tauri::async_runtime::spawn_blocking(work)
        .await
        .map_err(|e| format!("The background work was cancelled: {e}"))?
}

/// Every per-track and per-layer command funnels through here.
async fn act(engine: State<'_, EngineHandle>, action: Action) -> Result<(), String> {
    let handle = engine.inner().clone();
    offload(move || handle.act(action)).await
}

// ---------------------------------------------------------------------------------------------
// Setup
// ---------------------------------------------------------------------------------------------

/// Version, log file path and the limits of this build. Also emitted once as `looper://ready`;
/// use this command when the frontend cannot be sure it was already listening.
#[tauri::command]
fn app_info() -> AppInfo {
    host::app_info()
}

/// What the running engine opened (device, rate, buffer), or `null` when it is stopped. A page
/// reloaded while the engine runs asks this instead of waiting for the next `engine_start`.
#[tauri::command]
async fn engine_info(engine: State<'_, EngineHandle>) -> Result<Option<EngineInfo>, String> {
    let handle = engine.inner().clone();
    offload(move || handle.info()).await
}

/// Every host, device and supported configuration. Safe at any time - it opens nothing.
#[tauri::command]
async fn list_devices(engine: State<'_, EngineHandle>) -> Result<DeviceReport, String> {
    let handle = engine.inner().clone();
    offload(move || handle.list_devices()).await
}

/// Open the device and start the engine. If one is already running it is stopped first, so a
/// changed configuration is one command, not two.
#[tauri::command(rename_all = "snake_case")]
async fn engine_start(
    config: StartConfig,
    engine: State<'_, EngineHandle>,
) -> Result<EngineInfo, String> {
    let handle = engine.inner().clone();
    offload(move || handle.start(config)).await
}

/// Stop the engine and give the device back. Every buffer returns to the control thread.
#[tauri::command]
async fn engine_stop(engine: State<'_, EngineHandle>) -> Result<(), String> {
    let handle = engine.inner().clone();
    offload(move || handle.stop()).await
}

// ---------------------------------------------------------------------------------------------
// Per track. `track` is the zero-based index into the status event's `tracks` array.
// ---------------------------------------------------------------------------------------------

/// New loop from the next bar, over the configured number of bars. Replaces existing layers.
#[tauri::command(rename_all = "snake_case")]
async fn track_record(track: usize, engine: State<'_, EngineHandle>) -> Result<(), String> {
    act(engine, Action::Record { track }).await
}

/// Further layer from the next bar, exactly one pass of the existing loop.
#[tauri::command(rename_all = "snake_case")]
async fn track_overdub(track: usize, engine: State<'_, EngineHandle>) -> Result<(), String> {
    act(engine, Action::Overdub { track }).await
}

/// Cancel a scheduled recording, end a running one on the next bar, or stop playback.
#[tauri::command(rename_all = "snake_case")]
async fn track_stop(track: usize, engine: State<'_, EngineHandle>) -> Result<(), String> {
    act(engine, Action::StopTrack { track }).await
}

/// Start playback from the next bar.
#[tauri::command(rename_all = "snake_case")]
async fn track_play(track: usize, engine: State<'_, EngineHandle>) -> Result<(), String> {
    act(engine, Action::Play { track }).await
}

/// Throw this track's layers away.
#[tauri::command(rename_all = "snake_case")]
async fn track_clear(track: usize, engine: State<'_, EngineHandle>) -> Result<(), String> {
    act(engine, Action::ClearTrack { track }).await
}

/// Input monitoring, independent of whether the track is playing.
#[tauri::command(rename_all = "snake_case")]
async fn track_monitor(
    track: usize,
    on: bool,
    engine: State<'_, EngineHandle>,
) -> Result<(), String> {
    act(engine, Action::SetMonitor { track, on }).await
}

/// Where this track sits between the speakers: -1.0 hard left, 0.0 centre, +1.0 hard right.
///
/// On a mono track this places the source in the stereo field; on a stereo one it is a balance
/// between the two recorded channels. Either way the centre passes both sides at unity - see
/// `looper_engine::engine::frame` for the pan law and why it is that one.
#[tauri::command(rename_all = "snake_case")]
async fn track_pan(track: usize, pan: f32, engine: State<'_, EngineHandle>) -> Result<(), String> {
    act(engine, Action::SetPan { track, pan }).await
}

/// What a take of an instrument track stores: `midi` - the notes, played back through the
/// instrument - or `audio`, its sound. Only while the track is empty.
#[tauri::command(rename_all = "snake_case")]
async fn track_loop_kind(
    track: usize,
    kind: String,
    engine: State<'_, EngineHandle>,
) -> Result<(), String> {
    let midi = match kind.as_str() {
        "midi" => true,
        "audio" => false,
        other => return Err(format!("The loop is 'audio' or 'midi', not '{other}'.")),
    };
    act(engine, Action::SetLoopKind { track, midi }).await
}

/// The note grid a MIDI loop's layers are played on: `none`, `1/4`, `1/8`, `1/16`, `1/8t`,
/// `1/16t`. Acts on what is already recorded too, and never changes the recording itself.
#[tauri::command(rename_all = "snake_case")]
async fn track_grid(track: usize, grid: String, engine: State<'_, EngineHandle>) -> Result<(), String> {
    let grid = looper_engine::engine::midiloop::NoteGrid::parse(&grid).ok_or_else(|| {
        format!("'{grid}' is not a note grid (none, 1/4, 1/8, 1/16, 1/8t, 1/16t).")
    })?;
    act(engine, Action::SetNoteGrid { track, grid }).await
}

/// The channel fader of one track, linear: 0.0 is silence, 1.0 is 0 dB, the top is +12 dB
/// (`MAX_TRACK_GAIN`). After the effect chain and the plugin inserts, before the panner; smoothed
/// in the engine, so a jump is a short ramp. Never reaches a recording.
#[tauri::command(rename_all = "snake_case")]
async fn track_gain(track: usize, gain: f32, engine: State<'_, EngineHandle>) -> Result<(), String> {
    act(engine, Action::SetTrackGain { track, gain }).await
}

/// Channel mute: the track is silent on both buses. Listening only - it still records.
#[tauri::command(rename_all = "snake_case")]
async fn track_mute(track: usize, on: bool, engine: State<'_, EngineHandle>) -> Result<(), String> {
    act(engine, Action::SetTrackMute { track, on }).await
}

/// Channel solo: while any track is soloed, only soloed tracks are heard. Listening only - a
/// track that is soloed away records exactly what it would have recorded. The click stays.
#[tauri::command(rename_all = "snake_case")]
async fn track_solo(track: usize, on: bool, engine: State<'_, EngineHandle>) -> Result<(), String> {
    act(engine, Action::SetTrackSolo { track, on }).await
}

/// Which output buses one source of a track is heard on.
///
/// `source` is `loop` (the recorded layers) or `monitor` (the live input while monitoring is on);
/// `bus` is `main` (to the PA) or `monitor` (to the headphones). Two switches per source rather
/// than one word, because that is how they are operated: "geht dieser Track in den Saal" has a yes
/// and a no.
///
/// A bus assignment is an **output** decision - it cannot reach a layer buffer - which is why it
/// stays available while a score is running and while a take is being recorded.
#[tauri::command(rename_all = "snake_case")]
async fn track_bus(
    track: usize,
    source: String,
    bus: String,
    on: bool,
    engine: State<'_, EngineHandle>,
) -> Result<(), String> {
    let source = TrackSource::parse(&source)
        .ok_or_else(|| format!("\"{source}\" is not a source. There are loop and monitor."))?;
    let bus = Bus::parse(&bus)
        .ok_or_else(|| format!("\"{bus}\" is not a bus. There are main and monitor."))?;
    // One bit of a set. The set is read back on the host thread, where the newest engine snapshot
    // is - doing it here would need a round trip, and switching Main off without reading it first
    // would take the track off the headphones as well.
    act(
        engine,
        Action::SetTrackBus {
            track,
            source,
            bus,
            on,
        },
    )
    .await
}

/// Volume of one output bus, linear, 0.0 to 4.0.
///
/// This is half the reason there are two buses: the headphones can be turned down without the room
/// changing.
#[tauri::command(rename_all = "snake_case")]
async fn bus_gain(bus: String, gain: f32, engine: State<'_, EngineHandle>) -> Result<(), String> {
    let bus = Bus::parse(&bus)
        .ok_or_else(|| format!("\"{bus}\" is not a bus. There are main and monitor."))?;
    act(engine, Action::SetBusGain { bus, gain }).await
}

/// Which device output channels one bus leaves on.
///
/// `channel` is one-based as printed on the interface, `width` is 2 for a stereo pair and 1 for a
/// single channel - the latter being the stopgap on a two-output interface: main on 1, monitor on
/// 2, split with a Y-cable.
#[tauri::command(rename_all = "snake_case")]
async fn bus_output(
    bus: String,
    channel: u32,
    width: u32,
    engine: State<'_, EngineHandle>,
) -> Result<(), String> {
    let bus = Bus::parse(&bus)
        .ok_or_else(|| format!("\"{bus}\" is not a bus. There are main and monitor."))?;
    let out = BusOutConfig { channel, width }.to_output(bus)?;
    act(engine, Action::SetBusOutput { bus, out }).await
}

/// What this track subtracts while recording, in **frames**, as the two numbers it is made of.
///
/// * `measured` - what a loopback measurement found for this input, or `null` to follow the
///   engine's global default. This is the half `calibrate` writes.
/// * `trim` - a manual surcharge on top, for what no measurement can see: the latency an external
///   plugin host has inside itself. A calibration never touches it, which is why it is a separate
///   number and not folded into the first.
///
/// Takes effect for input arriving from now on. Material that is already in a layer was stored
/// with the old value and stays where it was played - a take that moves because a number was
/// corrected would be worse than the wrong number.
#[tauri::command(rename_all = "snake_case")]
async fn track_latency(
    track: usize,
    measured: Option<u32>,
    trim: Option<i32>,
    engine: State<'_, EngineHandle>,
) -> Result<(), String> {
    act(
        engine,
        Action::SetTrackLatency {
            track,
            latency: EngineTrackLatency {
                measured,
                trim: trim.unwrap_or(0),
            },
        },
    )
    .await
}

// ---------------------------------------------------------------------------------------------
// Per layer. `layer` is the zero-based index into that track's `layers` array.
// ---------------------------------------------------------------------------------------------

#[tauri::command(rename_all = "snake_case")]
async fn layer_mute(
    track: usize,
    layer: usize,
    muted: bool,
    engine: State<'_, EngineHandle>,
) -> Result<(), String> {
    act(
        engine,
        Action::LayerMute {
            track,
            layer,
            muted,
        },
    )
    .await
}

/// Remove one layer; its buffer travels back to the control thread.
#[tauri::command(rename_all = "snake_case")]
async fn layer_remove(
    track: usize,
    layer: usize,
    engine: State<'_, EngineHandle>,
) -> Result<(), String> {
    act(engine, Action::LayerRemove { track, layer }).await
}

/// Layer volume, 0.0 to 4.0.
#[tauri::command(rename_all = "snake_case")]
async fn layer_gain(
    track: usize,
    layer: usize,
    gain: f32,
    engine: State<'_, EngineHandle>,
) -> Result<(), String> {
    act(engine, Action::LayerGain { track, layer, gain }).await
}

// ---------------------------------------------------------------------------------------------
// Global
// ---------------------------------------------------------------------------------------------

/// Reset every track to the state of a fresh start.
#[tauri::command]
async fn clear_all(engine: State<'_, EngineHandle>) -> Result<(), String> {
    act(engine, Action::ClearAll).await
}

/// Start (`true`), stop (`false`) or toggle (`null`) the clock of free looping. While it stands the
/// display sits at bar 1, the click is silent and the loops are kept but not heard; a start plays
/// them from bar 1. Refused while a score is loaded - the score has its own Start / Stop all.
#[tauri::command(rename_all = "snake_case")]
async fn transport(run: Option<bool>, engine: State<'_, EngineHandle>) -> Result<(), String> {
    act(engine, Action::Transport { run }).await
}

/// Metronome on or off. The click grid keeps running either way.
#[tauri::command(rename_all = "snake_case")]
async fn set_click(on: bool, engine: State<'_, EngineHandle>) -> Result<(), String> {
    act(engine, Action::SetClick { on }).await
}

/// Volume of the click, linear 0.0 to 4.0. The click is on the monitor bus only.
#[tauri::command(rename_all = "snake_case")]
async fn click_gain(gain: f32, engine: State<'_, EngineHandle>) -> Result<(), String> {
    act(engine, Action::SetClickGain { gain }).await
}

/// Volume of every monitored live input, linear 0.0 to 4.0. Output side only - a recording is
/// never affected.
#[tauri::command(rename_all = "snake_case")]
async fn monitor_gain(gain: f32, engine: State<'_, EngineHandle>) -> Result<(), String> {
    act(engine, Action::SetMonitorGain { gain }).await
}

/// New tempo and time signature. Only accepted while every track is empty.
#[tauri::command(rename_all = "snake_case")]
async fn set_tempo(
    bpm: f64,
    beats_per_bar: u32,
    beat_unit: u32,
    bars: Option<u32>,
    engine: State<'_, EngineHandle>,
) -> Result<(), String> {
    act(
        engine,
        Action::SetTempo {
            bpm,
            beats_per_bar,
            beat_unit,
            bars,
        },
    )
    .await
}

/// One beat of tap tempo. From the third tap on the tempo follows the median gap.
#[tauri::command(rename_all = "snake_case")]
async fn tap_tempo(engine: State<'_, EngineHandle>) -> Result<(), String> {
    act(engine, Action::TapTempo).await
}

// ---------------------------------------------------------------------------------------------
// Effects. `track` is the zero-based index into the status event's `tracks` array; the state of a
// chain comes back in that track's `fx` object.
//
// Effects act on playback and on monitoring, never on the recording - see
// `looper_engine::engine::fx`. Nothing below can change what lands in a layer buffer.
// ---------------------------------------------------------------------------------------------

/// Whole chain of one track in or out of the signal path. Out is a bit-identical pass-through,
/// which makes it the panic switch.
#[tauri::command(rename_all = "snake_case")]
async fn fx_bypass(track: usize, on: bool, engine: State<'_, EngineHandle>) -> Result<(), String> {
    act(engine, Action::FxBypass { track, on }).await
}

/// One effect on or off: `high_pass`, `eq`, `comp`, `delay`, `reverb`.
#[tauri::command(rename_all = "snake_case")]
async fn fx_enable(
    track: usize,
    effect: FxSlotName,
    on: bool,
    engine: State<'_, EngineHandle>,
) -> Result<(), String> {
    act(
        engine,
        Action::FxEnable {
            track,
            slot: effect.into(),
            on,
        },
    )
    .await
}

/// Load a ready-made chain: `dry`, `voice` or `piezo_guitar`. The one command that matters on
/// stage - nobody turns knobs during a song.
#[tauri::command(rename_all = "snake_case")]
async fn fx_preset(
    track: usize,
    preset: FxPresetName,
    engine: State<'_, EngineHandle>,
) -> Result<(), String> {
    act(
        engine,
        Action::FxPreset {
            track,
            preset: preset.into(),
        },
    )
    .await
}

/// One numeric knob, by name - `comp_ratio`, `reverb_mix`, `high_pass_hz` and so on. The three
/// band-scoped names (`band_hz`, `band_q`, `band_gain_db`) additionally need `band`, zero-based.
///
/// Values are clamped by the engine to a range that makes sense, so a slider cannot produce
/// something unusable; only a missing or out-of-range `band` is refused.
#[tauri::command(rename_all = "snake_case")]
async fn fx_set(
    track: usize,
    param: FxParamName,
    value: f64,
    band: Option<u32>,
    engine: State<'_, EngineHandle>,
) -> Result<(), String> {
    let param = param.to_param(value, band)?;
    act(engine, Action::FxParam { track, param }).await
}

/// What one EQ band does: `peak`, `low_shelf` or `high_shelf`. `band` is zero-based.
#[tauri::command(rename_all = "snake_case")]
async fn fx_band_kind(
    track: usize,
    band: u32,
    kind: BandKindName,
    engine: State<'_, EngineHandle>,
) -> Result<(), String> {
    if band as usize >= MAX_EQ_BANDS {
        return Err(format!(
            "There are EQ bands 0 to {}, not {band}.",
            MAX_EQ_BANDS - 1
        ));
    }
    act(
        engine,
        Action::FxParam {
            track,
            param: EngineFxParam::BandKind {
                band: band as usize,
                kind: kind.into(),
            },
        },
    )
    .await
}

/// Note value of the tempo-synchronous delay: `quarter`, `dotted_eighth`, `eighth` or
/// `triplet_eighth`. There is no milliseconds setting - the time comes from the engine's timeline
/// and follows a tempo change on its own.
#[tauri::command(rename_all = "snake_case")]
async fn fx_delay_note(
    track: usize,
    note: DelayNoteName,
    engine: State<'_, EngineHandle>,
) -> Result<(), String> {
    act(
        engine,
        Action::FxParam {
            track,
            param: EngineFxParam::DelayNote(note.into()),
        },
    )
    .await
}

/// Which grid a recording and an overdub snap to: `"loop"` (next loop boundary, so one early press
/// is enough) or `"bar"` (next bar boundary). Takes that are already armed keep their position.
#[tauri::command(rename_all = "snake_case")]
async fn set_quantize(
    quantize: QuantizeName,
    engine: State<'_, EngineHandle>,
) -> Result<(), String> {
    act(
        engine,
        Action::SetQuantize {
            quantize: quantize.into(),
        },
    )
    .await
}

/// Bars of count-in before a take that starts the standing clock of free looping (0 = off).
#[tauri::command(rename_all = "snake_case")]
async fn set_count_in(bars: u32, engine: State<'_, EngineHandle>) -> Result<(), String> {
    act(engine, Action::SetCountIn { bars }).await
}

/// Undo on one track: its running take, or else its newest layer.
#[tauri::command(rename_all = "snake_case")]
async fn track_undo(track: usize, engine: State<'_, EngineHandle>) -> Result<(), String> {
    act(engine, Action::Undo { track }).await
}

/// Undo the newest take over all tracks.
#[tauri::command(rename_all = "snake_case")]
async fn undo_last(engine: State<'_, EngineHandle>) -> Result<(), String> {
    act(engine, Action::UndoLast).await
}

/// What `O` does in free looping: `"latch"` (stays on until pressed again, every pass a layer) or
/// `"once"` (one pass).
#[tauri::command(rename_all = "snake_case")]
async fn set_overdub_mode(
    mode: OverdubMode,
    engine: State<'_, EngineHandle>,
) -> Result<(), String> {
    act(engine, Action::SetOverdubMode { mode }).await
}

// ---------------------------------------------------------------------------------------------
// The score. Phase 3: the compiler makes a `CompiledScore` out of YAML, the runner plays it.
//
// Two things about this group are unlike the rest of the file, and both follow from the runner:
//
// * `score_compile` is the only command that reaches nothing at all. It is what the editor calls
//   while somebody types, so it must work with the engine stopped, with the engine running and
//   with a score playing - and it must hand back the compiler's issue list *positioned*, because
//   the editor turns each entry into a marker on a line.
// * The four transport commands answer with a **sentence instead of nothing**. The runner
//   does not fail: `score_goto` while a change is armed reports how far the armed change still is
//   and sends not one command. That sentence is the answer, so the frontend prints it rather than
//   guessing from a silent success.
// ---------------------------------------------------------------------------------------------

/// Translate a score without loading it. Returns the compiled score, or **every** problem the
/// compiler found - each with line, column, message and often a suggestion.
///
/// Opens nothing and touches no engine, so it is safe at any time.
#[tauri::command(rename_all = "snake_case")]
async fn score_compile(yaml: String) -> Result<CompileOutcome, String> {
    offload(move || Ok(host::compile(&yaml))).await
}

/// Text -> structured document, for an editor that arranges the score graphically.
///
/// When the text compiles: the whole document - every track, binding and section as written,
/// each section with what it states itself and what it inherits through `repeat:` (`resolved`),
/// every comment attached to its element, and the source line of every element. When it does not:
/// no document, and the compiler's issues, each additionally naming its element (`at`).
///
/// Pure, like `score_compile`: no engine, no device, safe while a score plays.
#[tauri::command(rename_all = "snake_case")]
async fn score_parse(yaml: String) -> Result<ParseOutcome, String> {
    offload(move || Ok(looper_engine::score::doc::parse_doc_outcome(&yaml))).await
}

/// Structured document -> canonical text, validated.
///
/// Always answers with the rendered text. `ok` says whether it compiles; `errors` are the
/// compiler's own issues, each with the element of the document it belongs to (`at`). The rule for
/// comments and layout is in `looper_engine::score::doc::render_doc`.
#[tauri::command(rename_all = "snake_case")]
async fn score_render(doc: ScoreDoc) -> Result<RenderOutcome, String> {
    offload(move || Ok(looper_engine::score::doc::render_doc_outcome(&doc))).await
}

// ---------------------------------------------------------------------------------------------
// Score files. The pickers are the dialog plugin on the web side; reading and writing are these
// commands - see `files.rs` for why not the FS plugin.
// ---------------------------------------------------------------------------------------------

fn config_dir(app: &tauri::AppHandle) -> std::path::PathBuf {
    // The same override the MIDI profiles and the plugin cache honour, so a test run with its own
    // directory leaves the musician's recent-files list alone too.
    if let Some(dir) = std::env::var_os("HENRYS_LOOPER_CONFIG_DIR") {
        return std::path::PathBuf::from(dir);
    }
    app.path()
        .app_config_dir()
        .unwrap_or_else(|_| std::env::temp_dir().join("henrys-looper"))
}

/// The folder "Save as" proposes (`~/Documents/Henrys Looper`, created on demand) and the recent
/// files, newest first.
#[tauri::command]
async fn score_files_info(app: tauri::AppHandle) -> Result<files::FilesInfo, String> {
    offload(move || {
        Ok(files::FilesInfo {
            default_dir: files::default_dir(app.path().document_dir().ok())
                .map(|d| d.display().to_string()),
            recent: files::load_recent(&config_dir(&app)),
        })
    })
    .await
}

/// Read a score file (`.yaml`/`.yml`, at most 1 MB, UTF-8) and put it on top of the recent list.
#[tauri::command(rename_all = "snake_case")]
async fn score_file_open(path: String, app: tauri::AppHandle) -> Result<files::OpenedFile, String> {
    offload(move || {
        let p = std::path::PathBuf::from(&path);
        let yaml = files::read_score_file(&p)?;
        let recent = files::remember(&config_dir(&app), &p);
        Ok(files::OpenedFile {
            name: p.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default(),
            path,
            yaml,
            recent,
        })
    })
    .await
}

/// Write a score file atomically. A name without extension gets `.yaml`; the answer carries the
/// path really written.
#[tauri::command(rename_all = "snake_case")]
async fn score_file_save(
    path: String,
    yaml: String,
    app: tauri::AppHandle,
) -> Result<files::SavedFile, String> {
    offload(move || {
        let written = files::write_score_file(std::path::Path::new(&path), &yaml)?;
        let recent = files::remember(&config_dir(&app), &written);
        Ok(files::SavedFile {
            name: written
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default(),
            path: written.display().to_string(),
            recent,
        })
    })
    .await
}

/// Drop one entry from the recent list (a file that has gone).
#[tauri::command(rename_all = "snake_case")]
async fn score_file_forget(
    path: String,
    app: tauri::AppHandle,
) -> Result<Vec<files::RecentFile>, String> {
    offload(move || Ok(files::forget(&config_dir(&app), &path))).await
}

/// One track for the "New" template: name and 1-based input(s), or what plays it when it is an
/// instrument track (as the setup has it).
#[derive(serde::Deserialize)]
struct TemplateTrack {
    name: String,
    input: u32,
    #[serde(default)]
    input_right: Option<u32>,
    #[serde(default)]
    instrument: Option<proto::InstrumentInput>,
}

/// The text behind "New": the given tracks (normally the running engine's, so the new score loads
/// straight away) or a voice and a guitar, and three sections - record, layer, play out.
#[tauri::command(rename_all = "snake_case")]
async fn score_template(tracks: Option<Vec<TemplateTrack>>) -> Result<String, String> {
    use looper_engine::score::doc::DocTrack;
    use looper_engine::score::model::InstrumentSource;
    offload(move || {
        let tracks: Vec<DocTrack> = tracks
            .unwrap_or_default()
            .into_iter()
            .map(|t| match t.instrument {
                Some(i) => {
                    let mut track = DocTrack::instrument(
                        t.name,
                        Some(InstrumentSource {
                            device: i.device,
                            channel: i.channel,
                            note_low: i.note_low.filter(|_| i.note_high.is_some()),
                            note_high: i.note_high.filter(|_| i.note_low.is_some()),
                        }),
                    );
                    if !i.monitor {
                        track.monitor = Some(false);
                    }
                    track
                }
                None => {
                    let mut track = DocTrack::new(t.name, t.input);
                    track.input_right = t.input_right;
                    track
                }
            })
            .collect();
        // The instrument each instrument track plays now goes into `mixer:` - by id only.
        let names: Vec<String> = tracks
            .iter()
            .filter(|t| t.instrument)
            .map(|t| t.name.clone())
            .collect();
        let instruments = pluginmix::instruments_of(&names);
        Ok(looper_engine::score::doc::template_with(tracks, instruments))
    })
    .await
}

/// Compile a score and hand it to the runner.
///
/// Needs a running engine whose track layout matches the score (same names, same inputs - see
/// `looper_engine::engine::runner::check_tracks`) and every track empty. Tempo, time signature and
/// the length every layer buffer is allocated at then come from the score, and so does every
/// track's pan, bus assignment (`bus`, `monitor_bus`) and latency compensation (`latency`,
/// `latency_trim`).
///
/// `count_in` is the number of bars of click before the first section; absent means one bar, which
/// is the shortest lead that is still a bar of the metre about to be played.
/// The mix as a score's `mixer:` section would hold it: every track of the engine and the buses.
#[tauri::command]
async fn mixer_state(engine: State<'_, EngineHandle>) -> Result<MixerSource, String> {
    let handle = engine.inner().clone();
    offload(move || handle.mixer_state()).await
}

/// The mixer entries a human changed since the last call - see `StatusEvent::mixer_rev`.
#[tauri::command]
async fn mixer_take(engine: State<'_, EngineHandle>) -> Result<host::MixerTake, String> {
    let handle = engine.inner().clone();
    offload(move || handle.mixer_take()).await
}

/// Put the engine into a mix, tracks by name (an undo in the arrangement), plus the pans the
/// score's `tracks:` give.
#[tauri::command(rename_all = "snake_case")]
async fn mixer_apply(
    mixer: MixerSource,
    pan: Option<looper_engine::score::OrderedMap<f64>>,
    engine: State<'_, EngineHandle>,
) -> Result<(), String> {
    let handle = engine.inner().clone();
    offload(move || handle.mixer_apply(mixer, pan.unwrap_or_default())).await
}

#[tauri::command(rename_all = "snake_case")]
async fn score_load(
    yaml: String,
    count_in: Option<u32>,
    engine: State<'_, EngineHandle>,
) -> Result<ScoreLoaded, String> {
    let handle = engine.inner().clone();
    offload(move || handle.score_load(yaml, count_in)).await
}

/// Start the count-in and arm the first section.
#[tauri::command]
async fn score_start(engine: State<'_, EngineHandle>) -> Result<String, String> {
    score_act(engine, ScoreAction::Start).await
}

/// The release button: arm the change to the next section. Pressing it twice skips nothing - the
/// second press only reports how far the armed change still is.
#[tauri::command]
async fn score_next(engine: State<'_, EngineHandle>) -> Result<String, String> {
    score_act(engine, ScoreAction::Next).await
}

/// Jump to a section instead of taking the next one. `section` is zero-based.
///
/// Refused while a change is armed, and the answer says so: the armed section's commands are
/// already in the audio thread's waiting room with their timestamps on them and cannot be recalled.
#[tauri::command(rename_all = "snake_case")]
async fn score_goto(section: usize, engine: State<'_, EngineHandle>) -> Result<String, String> {
    score_act(engine, ScoreAction::Goto(section)).await
}

/// Stop every track and end the run. An armed change is neutralised where it lands.
#[tauri::command]
async fn score_stop_all(engine: State<'_, EngineHandle>) -> Result<String, String> {
    score_act(engine, ScoreAction::StopAll).await
}

/// Back to the start: clear every track and put the score before its first section, so "Start"
/// plays it again from the top. Refused while the score plays ("Stop all" first).
///
/// The clearing is not optional: the runner plans every overdub against loops it recorded itself,
/// and a loop left over from the previous run would be invisible to it. The score's track setup
/// (pan, buses, latency) is restored right after the clear.
#[tauri::command]
async fn score_rewind(engine: State<'_, EngineHandle>) -> Result<String, String> {
    score_act(engine, ScoreAction::Rewind).await
}

/// Bars of count-in for the next start of the loaded score (1 to 8; 0 becomes 1, and the answer
/// says so). Only before the score starts. Shown in the status as `score.count_in`.
#[tauri::command(rename_all = "snake_case")]
async fn score_count_in(bars: u32, engine: State<'_, EngineHandle>) -> Result<String, String> {
    score_act(engine, ScoreAction::CountIn(bars)).await
}

/// Every score transport press funnels through here.
async fn score_act(
    engine: State<'_, EngineHandle>,
    action: ScoreAction,
) -> Result<String, String> {
    let handle = engine.inner().clone();
    offload(move || handle.score_act(action)).await
}

/// Check the latency compensation against the hardware, through a loopback cable.
///
/// **This makes sound** and takes over the device for the whole measurement, so the engine has to
/// be stopped. The full report lands in the log file.
#[tauri::command(rename_all = "snake_case")]
async fn calibrate(
    config: CalibrateConfig,
    engine: State<'_, EngineHandle>,
) -> Result<CalibrateOutcome, String> {
    let handle = engine.inner().clone();
    offload(move || handle.calibrate(config)).await
}

// ---------------------------------------------------------------------------------------------
// MIDI. Phase 5: a pad or a knob operates whatever a mouse could operate.
//
// Everything about the mapping itself is in `looper_engine::midi` and needs no device. The commands
// below are the app's half: which port is open, what the learn mode is waiting for, and where the
// profile is written.
//
// Incoming events do **not** come back through these commands. They arrive on `looper://midi`,
// which is a queue rather than the twenty-per-second sampling the status event is - see the module
// comment of `midi.rs`.
// ---------------------------------------------------------------------------------------------

/// Every MIDI input the system has, with the profile that belongs to it. Opens nothing, so it works
/// even while another program holds the controller.
#[tauri::command]
async fn midi_ports(engine: State<'_, EngineHandle>) -> Result<Vec<MidiPortView>, String> {
    let handle = engine.inner().clone();
    offload(move || handle.midi_ports()).await
}

/// Open a port by number or by part of its name, and load its profile.
///
/// Windows hands a MIDI device out to one program at a time; if Ableton holds the controller as a
/// control surface, the refusal says so by name.
#[tauri::command(rename_all = "snake_case")]
async fn midi_open(device: String, engine: State<'_, EngineHandle>) -> Result<MidiView, String> {
    let handle = engine.inner().clone();
    offload(move || handle.midi_open(device)).await
}

/// Disconnect by hand: one device (`device` = port name or the profile's `device:`), or all of them
/// when `device` is missing. A disconnected device is not reconnected automatically until it is
/// unplugged or connected by hand again.
#[tauri::command(rename_all = "snake_case")]
async fn midi_close(
    device: Option<String>,
    engine: State<'_, EngineHandle>,
) -> Result<MidiView, String> {
    let handle = engine.inner().clone();
    offload(move || handle.midi_close(device)).await
}

/// Arm the learn mode for one address, or cancel it with `null`.
///
/// `address` is the dotted path of the clicked control - `track.1.record`, `global.click`,
/// `track.2.fx.reverb.mix`. It is parsed on the Rust side, so a wrong one is a sentence
/// rather than a binding on nothing. The next control that arrives becomes that address and the
/// mode disarms itself; the result travels on `looper://midi`.
#[tauri::command(rename_all = "snake_case")]
async fn midi_learn(
    address: Option<String>,
    engine: State<'_, EngineHandle>,
) -> Result<MidiView, String> {
    let handle = engine.inner().clone();
    offload(move || handle.midi_learn(address)).await
}

/// Take a control's job away and save the profile. `id` is what the overview shows (`ch1.note36`);
/// `device` (the binding's `device`) is only needed when several profiles bind the same id.
#[tauri::command(rename_all = "snake_case")]
async fn midi_unbind(
    id: String,
    device: Option<String>,
    engine: State<'_, EngineHandle>,
) -> Result<MidiView, String> {
    let handle = engine.inner().clone();
    offload(move || handle.midi_unbind(id, device)).await
}

/// Change a binding's options and save the profile. `options` replaces all four at once:
/// `{mode, takeover, min, max}`, each `null` for the target's default.
#[tauri::command(rename_all = "snake_case")]
async fn midi_edit(
    id: String,
    device: Option<String>,
    options: BindingOptions,
    engine: State<'_, EngineHandle>,
) -> Result<MidiView, String> {
    let handle = engine.inner().clone();
    offload(move || handle.midi_edit(id, device, options)).await
}

/// Write every profile whose last automatic save failed - the retry. Normally there is nothing to
/// do: every learned, unbound or edited binding is written at once to
/// `%APPDATA%\henrys-looper\midi\<geraet>.yaml` (macOS: `~/Library/Application Support/...`).
#[tauri::command]
async fn midi_save(engine: State<'_, EngineHandle>) -> Result<MidiSaved, String> {
    let handle = engine.inner().clone();
    offload(move || handle.midi_save()).await
}

/// Move every binding of one device to another, rename a device, or delete its profile:
/// `{action: "move", device, to}`, `{action: "rename", device, name}`, `{action: "delete", device}`.
#[tauri::command]
async fn midi_profile(
    action: ProfileAction,
    engine: State<'_, EngineHandle>,
) -> Result<MidiView, String> {
    let handle = engine.inner().clone();
    offload(move || handle.midi_profile(action)).await
}

/// The connection, the mapping and the learn mode, for a frontend that has just started and missed
/// the events so far.
#[tauri::command]
async fn midi_state(engine: State<'_, EngineHandle>) -> Result<MidiView, String> {
    let handle = engine.inner().clone();
    offload(move || handle.midi_state()).await
}

/// The window's page as a PNG at `path` (macOS, WebKit's own snapshot). For comparing the UI with
/// the design mockups from a script; see `devshot.rs`.
#[tauri::command]
async fn dev_snapshot(window: tauri::WebviewWindow, path: String) -> Result<String, String> {
    devshot::snapshot(&window, path)
}

/// Development aid for the autopilot (`ui/src/dev/autopilot.ts`, step `dump`): write what the page
/// shows as text, so a script can read the UI without a screenshot. Only into the temp directory.
#[tauri::command]
async fn dev_dump(path: String, text: String) -> Result<(), String> {
    let p = std::path::Path::new(&path);
    let tmp = std::env::temp_dir();
    if !(p.starts_with(&tmp) || p.starts_with("/private/tmp") || p.starts_with("/tmp")) {
        return Err("dev_dump writes into the temp directory only.".into());
    }
    std::fs::write(p, text).map_err(|e| e.to_string())
}

// ---------------------------------------------------------------------------------------------
// Recording every run to disk (`recording.rs`)
// ---------------------------------------------------------------------------------------------

/// The recording settings, the folder in use and the free space there.
#[tauri::command]
async fn recording_settings() -> Result<recording::SettingsView, String> {
    offload(|| Ok(recording::view())).await
}

#[tauri::command(rename_all = "snake_case")]
async fn recording_set(
    enabled: bool,
    dry: bool,
    folder: Option<String>,
) -> Result<recording::SettingsView, String> {
    offload(move || recording::set(recording::Settings { enabled, dry, folder })).await
}

/// The newest recordings, newest first.
#[tauri::command]
async fn recording_list() -> Result<Vec<recording::RecordingEntry>, String> {
    offload(|| Ok(recording::list())).await
}

/// Show one recording - or, without a path, the recordings folder - in the Finder.
#[tauri::command]
async fn recording_reveal(path: Option<String>) -> Result<(), String> {
    offload(move || recording::reveal(path.as_deref())).await
}

/// Delete one recording folder. The web view asks first.
#[tauri::command]
async fn recording_delete(path: String) -> Result<Vec<recording::RecordingEntry>, String> {
    offload(move || recording::delete(&path)).await
}

/// End the recording that is running now.
#[tauri::command]
async fn recording_stop(engine: State<'_, EngineHandle>) -> Result<(), String> {
    let engine = engine.inner().clone();
    offload(move || engine.recording_stop()).await
}

fn main() {
    // The plugin scan starts this executable as a child to load one plugin at a time, so a plugin
    // that crashes takes the child down and not the program (`looper_engine::plugin::scan`).
    let args: Vec<String> = std::env::args().collect();
    if let Some(code) = looper_engine::plugin::scan::probe_main(&args) {
        std::process::exit(code);
    }
    tauri::Builder::default()
        // Native open/save dialogs and the "are you sure" questions. File contents never go
        // through the plugin: reading and writing a score are commands of our own (`files.rs`).
        .plugin(tauri_plugin_dialog::init())
        .setup(|app| {
            // The log file first: from here on every println! in the process - ours and the
            // engine library's - ends up in it.
            let dir = app
                .path()
                .app_log_dir()
                .unwrap_or_else(|_| std::env::temp_dir().join("henrys-looper"));
            let path = logfile::init(&dir);

            // Spawn the audio thread from the Tauri main thread, after WebView2 has initialised
            // COM here. Apartments are per thread; the spike verified they do not collide.
            app.manage(EngineHandle::spawn(app.handle().clone()));
            // Plugin slots live on this (the main) thread; see `plugins.rs`.
            plugins::init(app.handle());
            instruments::init(app.handle());
            // Recording to disk: settings from the config folder, recordings under Documents.
            recording::init(config_dir(app.handle()), app.path().document_dir().ok());

            let info = host::app_info();
            log!(
                "ASIO built in: {}. At most {} tracks, {} layers per track.",
                info.asio_built,
                info.max_tracks,
                info.max_layers
            );
            // Emitted for a frontend that is already listening; everyone else asks `app_info`.
            let _ = app.emit(READY_EVENT, &info);
            let _ = path;
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            app_info,
            list_devices,
            engine_info,
            engine_start,
            engine_stop,
            track_record,
            track_overdub,
            track_stop,
            track_play,
            track_clear,
            track_monitor,
            track_pan,
            track_loop_kind,
            track_grid,
            track_gain,
            track_mute,
            track_solo,
            track_bus,
            bus_gain,
            bus_output,
            track_latency,
            layer_mute,
            layer_remove,
            layer_gain,
            clear_all,
            transport,
            set_click,
            set_tempo,
            click_gain,
            monitor_gain,
            set_quantize,
            set_overdub_mode,
            tap_tempo,
            track_undo,
            undo_last,
            set_count_in,
            fx_bypass,
            fx_enable,
            fx_preset,
            fx_set,
            fx_band_kind,
            fx_delay_note,
            score_compile,
            mixer_state,
            mixer_take,
            mixer_apply,
            score_parse,
            score_render,
            score_files_info,
            score_file_open,
            score_file_save,
            score_file_forget,
            score_template,
            score_load,
            score_start,
            score_next,
            score_goto,
            score_stop_all,
            score_rewind,
            score_count_in,
            calibrate,
            midi_ports,
            midi_open,
            midi_close,
            midi_learn,
            midi_unbind,
            midi_edit,
            midi_save,
            midi_state,
            midi_profile,
            plugins::plugin_catalog,
            plugins::plugin_scan,
            plugins::plugin_rack,
            plugins::plugin_load,
            plugins::plugin_remove,
            plugins::plugin_move,
            plugins::plugin_bypass,
            plugins::plugin_params,
            plugins::plugin_set_param,
            plugins::plugin_editor,
            plugins::plugin_state_get,
            plugins::plugin_state_set,
            instruments::instrument_rack,
            instruments::instrument_set,
            instruments::instrument_editor,
            instruments::instrument_panic,
            recording_settings,
            recording_set,
            recording_list,
            recording_reveal,
            recording_delete,
            recording_stop,
            dev_snapshot,
            dev_dump
        ])
        .run(tauri::generate_context!())
        .expect("the Tauri application did not start");
}
