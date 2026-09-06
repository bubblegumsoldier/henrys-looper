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
//! Errors are `Result<_, String>` with German sentences, because they end up in front of a
//! musician. Where the engine already has a sentence for a situation, that sentence is used.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod host;
mod logfile;
mod proto;
mod schedule;

use tauri::{Emitter, Manager, State};

use host::{Action, EngineHandle, READY_EVENT};
use logfile::log;
use looper_engine::engine::fx::{EQ_BANDS as MAX_EQ_BANDS, FxParam as EngineFxParam};
use proto::{
    AppInfo, BandKindName, CalibrateConfig, CalibrateOutcome, DelayNoteName, DeviceReport,
    EngineInfo, FxParamName, FxPresetName, FxSlotName, QuantizeName, StartConfig,
};

/// Run blocking work on Tauri's blocking pool and translate a lost worker into German.
async fn offload<T, F>(work: F) -> Result<T, String>
where
    T: Send + 'static,
    F: FnOnce() -> Result<T, String> + Send + 'static,
{
    tauri::async_runtime::spawn_blocking(work)
        .await
        .map_err(|e| format!("Die Arbeit im Hintergrund wurde abgebrochen: {e}"))?
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

/// Metronome on or off. The click grid keeps running either way.
#[tauri::command(rename_all = "snake_case")]
async fn set_click(on: bool, engine: State<'_, EngineHandle>) -> Result<(), String> {
    act(engine, Action::SetClick { on }).await
}

/// New tempo and time signature. Only accepted while every track is empty.
#[tauri::command(rename_all = "snake_case")]
async fn set_tempo(
    bpm: f64,
    beats_per_bar: u32,
    beat_unit: u32,
    engine: State<'_, EngineHandle>,
) -> Result<(), String> {
    act(
        engine,
        Action::SetTempo {
            bpm,
            beats_per_bar,
            beat_unit,
        },
    )
    .await
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
            "Es gibt die EQ-Baender 0 bis {}, nicht {band}.",
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

fn main() {
    tauri::Builder::default()
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

            let info = host::app_info();
            log!(
                "ASIO eingebaut: {}. Hoechstens {} Tracks, {} Ebenen je Track.",
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
            engine_start,
            engine_stop,
            track_record,
            track_overdub,
            track_stop,
            track_play,
            track_clear,
            track_monitor,
            track_pan,
            layer_mute,
            layer_remove,
            layer_gain,
            clear_all,
            set_click,
            set_tempo,
            set_quantize,
            fx_bypass,
            fx_enable,
            fx_preset,
            fx_set,
            fx_band_kind,
            fx_delay_note,
            calibrate
        ])
        .run(tauri::generate_context!())
        .expect("Tauri-Anwendung liess sich nicht starten");
}
