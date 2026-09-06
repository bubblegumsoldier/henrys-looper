//! `live` subcommand: the looper on real hardware. Phase 2 - several tracks, unlimited layers.
//!
//! Thread layout, following the plan:
//!
//! * **Input callback** - copies whole input frames (all device channels, interleaved) into a
//!   lock-free FIFO and counts. Nothing else.
//! * **Output callback** - takes the FIFO content and drives [`EngineCore`], which owns the whole
//!   musical state. This callback is the engine clock.
//! * **Main thread** - allocates, zeroes, schedules commands, prints. Never touches engine state
//!   directly, and is the only place a `Vec` is ever created or dropped.
//!
//! # Why the input goes through a FIFO, and why that does not disturb the compensation
//!
//! cpal has no duplex callback, so input and output are two separate streams even on the one ASIO
//! driver. The FIFO is how the input reaches the engine, and it does add a transit delay of its
//! own between the two callbacks. That transit does **not** enter the latency compensation, and
//! the reason is worth spelling out:
//!
//! * `EngineCore` starts counting `out_pos` at 0 on the first output callback, so `out_pos` is the
//!   output stream's own frame index.
//! * The FIFO never drops a sample, so the `k`-th frame the engine consumes is the input stream's
//!   own frame number `k`, no matter how long it sat in the FIFO. `in_index` is that number.
//! * Phase 0 measured the roundtrip between exactly these two counters: what the output writes at
//!   frame `P` is seen by the input at frame `P + R`, R = 827 at 128 frames / 48 kHz.
//!
//! So the compensation `m = k - R` works on stream frame numbers, not on arrival times, and the
//! FIFO transit cancels out. It only decides *when* the engine gets to write a sample, not *where*
//! - which is why a FIFO underrun (fewer frames available than the block needs) costs nothing but
//! a little delay: the engine simply consumes fewer frames this round and picks them up next time,
//! and `in_index` still counts input frames.
//!
//! Two conditions this rests on, both of them cheap to keep:
//!
//! 1. **Nothing may be dropped from the FIFO, and never half a frame.** An overrun loses input
//!    samples and shifts every later recording permanently, so overruns are counted and shown as a
//!    warning, and both sides move whole frames only. The FIFO holds 64 buffers and is drained on
//!    every output callback, so it cannot fill in normal operation.
//! 2. **The streams are started in the same order as in the phase 0 measurement**, input first,
//!    then output, so that both frame counters start on the same driver callback and share the
//!    origin the 827 samples were measured against.
//!
//! `--latency-samples` still deserves one check against the real path: record the click through a
//! loopback cable and look at where it lands (subcommand `calibrate`). That is the honest way to
//! confirm the number for a different device, sample rate or buffer size.

use std::io::Write;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{Receiver, RecvTimeoutError, channel};
use std::time::{Duration, Instant};

use clap::Args;
use cpal::traits::StreamTrait;
use cpal::{ErrorKind, InputCallbackInfo, OutputCallbackInfo};

use crate::audio::{self, DeviceOpts};
use crate::meter::fmt_dbfs;

use super::command::{
    Command, CommandSender, LayerPool, MAX_TRACKS, Refusal, Status, TrackStatus, buffer_channel,
    command_channel, status_channel,
};
use super::fx::{DelayNote, FxParam, FxPreset, FxSlot, FxStatus};
use super::process::{EngineConfig, EngineCore, check_loop, limits_line, loop_capacity};
use super::schedule::{Lead, Quantize, Scheduled, Scheduler};
use super::timeline::{TimeSignature, Timeline};
use super::track::{MAX_LAYERS, Track, TrackState};

/// Input FIFO size in units of the audio buffer size. Generous on purpose: it costs a few dozen kB
/// and it is the difference between a hiccup and a permanently misaligned recording.
const INPUT_FIFO_BUFFERS: u32 = 64;
/// Status snapshots per second the audio thread produces. The display consumes 10 of them.
const STATUS_HZ: u32 = 200;
/// Terminal refresh rate, as required: no more than ten updates per second.
const DISPLAY_INTERVAL: Duration = Duration::from_millis(100);
/// Prepared layer buffers the control thread keeps in the engine, so an overdub never waits for an
/// allocation. Three is one for the take that is starting, one for a second track starting at the
/// same bar, and one in reserve.
const SPARE_SLOTS: usize = 3;
/// Width the status block is padded to, so remains of a longer previous line are erased.
const LINE_WIDTH: usize = 186;

#[derive(Args, Debug, Clone)]
pub struct LiveOpts {
    /// Track als NAME:KANAL, mehrfach angebbar. KANAL ist die Eingangsnummer wie am Geraet
    /// beschriftet (1-basiert), z.B. --track stimme:1 --track gitarre:2
    #[arg(long = "track", value_name = "NAME:KANAL")]
    pub tracks: Vec<String>,

    /// Tempo in Schlaegen pro Minute (bezogen auf Viertel)
    #[arg(long, default_value_t = 100.0)]
    pub bpm: f64,

    /// Schlaege pro Takt (3, 4, 5, 7 ... alles erlaubt)
    #[arg(long, default_value_t = 4)]
    pub beats_per_bar: u32,

    /// Zaehlzeit: 4 = Viertel, 8 = Achtel
    #[arg(long, default_value_t = 4)]
    pub beat_unit: u32,

    /// Loop-Laenge in Takten
    #[arg(long, default_value_t = 8)]
    pub bars: u32,

    /// Raster, auf das Aufnahme und Overdub einrasten:
    /// loop = naechster Loop-Anfang (einmal frueh druecken reicht), bar = naechste Taktgrenze
    #[arg(long, value_enum, default_value_t = Quantize::Loop)]
    pub quantize: Quantize,

    /// Roundtrip-Latenz in Samples, die beim Aufnehmen herausgerechnet wird.
    /// Standard ist der Messwert aus Phase 0 (128 Frames, 48 kHz, Scarlett 2i2 an ASIO).
    #[arg(long, default_value_t = 827)]
    pub latency_samples: u64,

    /// Verstaerkung des mitgehoerten Eingangssignals
    #[arg(long, default_value_t = 1.0)]
    pub monitor_gain: f32,

    /// Lautstaerke des Klicks
    #[arg(long, default_value_t = 1.0)]
    pub click_gain: f32,

    /// Mit eingeschaltetem Mithoeren starten (fuer alle Tracks)
    #[arg(long)]
    pub monitor: bool,

    /// Ohne Klick starten
    #[arg(long)]
    pub no_click: bool,

    /// Anzeige ohne Cursor-Steuerung: jede Aktualisierung wird angehaengt statt ueberschrieben.
    /// Noetig nur in Terminals, die ANSI-Steuerzeichen nicht koennen.
    #[arg(long)]
    pub simple_display: bool,
}

/// One track as the user asked for it on the command line.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TrackDef {
    pub name: String,
    /// Zero-based channel index inside an input frame.
    pub channel: usize,
}

/// Parse one `--track name:channel` argument. The channel is 1-based here, as printed on the
/// interface, and zero-based everywhere inside the engine.
pub fn parse_track_arg(spec: &str) -> Result<TrackDef, String> {
    let spec = spec.trim();
    let Some((name, channel)) = spec.rsplit_once(':') else {
        return Err(format!(
            "--track \"{spec}\" ist unvollstaendig. Erwartet wird NAME:KANAL, z.B. --track stimme:1"
        ));
    };
    let name = name.trim();
    if name.is_empty() {
        return Err(format!("--track \"{spec}\": vor dem Doppelpunkt fehlt der Name."));
    }
    let channel: usize = channel.trim().parse().map_err(|_| {
        format!(
            "--track \"{spec}\": \"{}\" ist keine Kanalnummer. Erwartet wird eine ganze Zahl ab 1.",
            channel.trim()
        )
    })?;
    if channel == 0 {
        return Err(format!(
            "--track \"{spec}\": Kanaele werden ab 1 gezaehlt, wie am Geraet beschriftet."
        ));
    }
    Ok(TrackDef {
        name: name.to_string(),
        channel: channel - 1,
    })
}

/// Turn the `--track` arguments into track definitions, or produce a German error.
///
/// Without any argument the setup this looper was built for is assumed: voice on input 1, guitar on
/// input 2 - reduced to what the device can actually deliver.
pub fn resolve_tracks(specs: &[String], in_channels: usize) -> Result<Vec<TrackDef>, String> {
    if in_channels == 0 {
        return Err("Das Geraet meldet keinen einzigen Eingangskanal.".to_string());
    }
    let defs: Vec<TrackDef> = if specs.is_empty() {
        ["stimme", "gitarre"]
            .iter()
            .take(in_channels.min(2))
            .enumerate()
            .map(|(i, name)| TrackDef {
                name: name.to_string(),
                channel: i,
            })
            .collect()
    } else {
        specs
            .iter()
            .map(|s| parse_track_arg(s))
            .collect::<Result<Vec<_>, _>>()?
    };

    if defs.len() > MAX_TRACKS {
        return Err(format!(
            "{} Tracks angefragt, moeglich sind hoechstens {MAX_TRACKS}.",
            defs.len()
        ));
    }
    for def in &defs {
        if def.channel >= in_channels {
            return Err(format!(
                "Track \"{}\" soll auf Eingang {} hoeren, das Geraet liefert aber nur {} Eingangskanaele \
                 (also Eingang 1 bis {}).\n\
                 \x20 - anderen Kanal waehlen: --track {}:1\n\
                 \x20 - oder mehr Kanaele oeffnen: --in-channels {}",
                def.name,
                def.channel + 1,
                in_channels,
                in_channels,
                def.name,
                def.channel + 1
            ));
        }
    }
    for (i, def) in defs.iter().enumerate() {
        if let Some(other) = defs[..i].iter().find(|d| d.name == def.name) {
            return Err(format!(
                "Der Trackname \"{}\" kommt zweimal vor. Namen muessen eindeutig sein.",
                other.name
            ));
        }
    }
    Ok(defs)
}

#[derive(Default)]
struct LiveStats {
    xruns: AtomicU64,
    other_errors: AtomicU64,
    /// Output callbacks that found the input FIFO short.
    underruns: AtomicU64,
    /// Input callbacks that could not push - frames lost, alignment gone.
    overruns: AtomicU64,
    cb_nanos_max: AtomicU64,
}

impl LiveStats {
    #[inline]
    fn record_callback(&self, started: Instant) {
        self.cb_nanos_max
            .fetch_max(started.elapsed().as_nanos() as u64, Ordering::Relaxed);
    }

    fn count_error(&self, err: cpal::Error) {
        if err.kind() == ErrorKind::Xrun {
            self.xruns.fetch_add(1, Ordering::Relaxed);
        } else {
            self.other_errors.fetch_add(1, Ordering::Relaxed);
        }
    }
}

/// The control thread's view of one track: what the user calls it, and the layer gains it set.
///
/// The engine is the authority on how many layers exist; the gains are mirrored here because they
/// are the one piece of layer state the status snapshot does not carry.
struct TrackUi {
    name: String,
    channel: usize,
    gains: Vec<f32>,
}

/// Everything the main thread needs to schedule commands.
///
/// All the arithmetic - which grid, how much head start, how long a take runs - lives in
/// [`Scheduler`], the same one the desktop app uses. This struct is only the keyboard in front of
/// it.
struct Control {
    cmd: CommandSender,
    pool: LayerPool,
    sched: Scheduler,
    latency: u64,
    tracks: Vec<TrackUi>,
    active: usize,
    message: String,
    running: bool,
}

impl Control {
    fn send(&mut self, cmd: Command) {
        if let Err(e) = self.cmd.send(cmd) {
            self.message = e;
            self.running = false;
        }
    }

    /// Push a whole plan into the queue and keep its German sentence.
    fn send_all(&mut self, plan: Scheduled) {
        for cmd in plan.commands {
            self.send(cmd);
        }
        self.message = plan.message;
    }

    fn status_of(&self, last: Option<(Status, Instant)>, track: usize) -> TrackStatus {
        last.and_then(|(s, _)| s.tracks().get(track).copied())
            .unwrap_or_default()
    }

    fn handle(&mut self, line: &str, last: Option<(Status, Instant)>) {
        let lowered = line.trim().to_lowercase();
        let mut parts = lowered.split_whitespace();
        let key = parts.next().unwrap_or("");
        let est = self.estimated_pos(last);
        let track = self.active;
        let ts = self.status_of(last, track);

        match key {
            "" => {}
            d if d.len() == 1 && d.chars().all(|c| c.is_ascii_digit()) => {
                let n: usize = d.parse().unwrap_or(0);
                if n >= 1 && n <= self.tracks.len() {
                    self.active = n - 1;
                    self.message = format!("Track {} \"{}\" gewaehlt.", n, self.tracks[n - 1].name);
                } else {
                    self.message = format!(
                        "Track {n} gibt es nicht. Vorhanden: 1 bis {}.",
                        self.tracks.len()
                    );
                }
            }
            "r" => {
                let name = self.tracks[track].name.clone();
                let plan = self.sched.record(track, &name, est);
                self.tracks[track].gains.clear();
                self.send_all(plan);
            }
            "o" => {
                let name = self.tracks[track].name.clone();
                let plan =
                    self.sched
                        .overdub(track, &name, est, ts.origin, ts.loop_len, ts.layers);
                self.send_all(plan);
            }
            "s" => {
                let name = self.tracks[track].name.clone();
                let plan = self.sched.stop(track, &name, est, ts.state);
                self.send_all(plan);
            }
            "p" => {
                let name = self.tracks[track].name.clone();
                let plan = self.sched.play(track, &name, est);
                self.send_all(plan);
            }
            "c" => {
                let name = self.tracks[track].name.clone();
                let plan = self.sched.clear_track(track, &name, est);
                self.tracks[track].gains.clear();
                self.send_all(plan);
            }
            "a" => {
                let plan = self.sched.clear_all(est);
                for t in self.tracks.iter_mut() {
                    t.gains.clear();
                }
                self.active = 0;
                self.send_all(plan);
            }
            "m" => {
                let on = !ts.monitor;
                self.send(Command::SetMonitor { track, on });
                self.message = format!(
                    "\"{}\": Mithoeren {}.",
                    self.tracks[track].name,
                    if on { "an" } else { "aus" }
                );
            }
            "k" => {
                let on = !last.map(|(s, _)| s.click).unwrap_or(true);
                self.send(Command::SetClick { on });
                self.message = format!("Klick {}.", if on { "an" } else { "aus" });
            }
            "f" => self.fx_bypass(ts),
            "x" => self.fx_slot(parts.next(), ts),
            "v" => self.fx_preset(parts.next()),
            "d" => self.fx_delay_note(parts.next()),
            "e" => self.layer_mute(parts.next(), ts),
            "w" => self.layer_remove(parts.next(), ts),
            "l" => self.layer_gain(parts.next(), parts.next(), ts),
            "t" => self.set_tempo(parts.next(), parts.next(), parts.next(), last),
            "q" => {
                self.send(Command::Stop);
                self.running = false;
                self.message = "Beende.".to_string();
            }
            other => {
                self.message = format!(
                    "Unbekannte Eingabe \"{other}\". Tasten: 1-{} r o s p c a m k e w l t f x v d q",
                    self.tracks.len()
                );
            }
        }
    }

    // ---- effects -----------------------------------------------------------------------------
    // Four keys, and no more: on stage the musician switches the chain, switches one effect, or
    // loads a preset. Everything finer than that belongs in the window, not under a guitar.

    /// `f` - whole chain of this track in or out. The panic switch: out is bit-identical.
    fn fx_bypass(&mut self, ts: TrackStatus) {
        let track = self.active;
        let on = !ts.fx.bypass;
        self.send(Command::SetFxBypass { track, on });
        self.message = format!(
            "\"{}\": Effektkette {}.",
            self.tracks[track].name,
            if on { "umgangen" } else { "aktiv" }
        );
    }

    /// `x <1-5>` - one effect on or off.
    fn fx_slot(&mut self, arg: Option<&str>, ts: TrackStatus) {
        let usage = "x <1-5>  (1 Hochpass, 2 EQ, 3 Kompressor, 4 Delay, 5 Hall)";
        let Some(slot) = arg
            .and_then(|a| a.parse::<usize>().ok())
            .and_then(FxSlot::from_number)
        else {
            self.message = format!("Aufruf: {usage}");
            return;
        };
        let track = self.active;
        let on = !ts.fx.settings.enabled[slot.index()];
        self.send(Command::SetFxEnabled { track, slot, on });
        let hint = if ts.fx.bypass {
            " (die ganze Kette ist noch umgangen - f druecken)"
        } else {
            ""
        };
        self.message = format!(
            "\"{}\": {} {}{hint}.",
            self.tracks[track].name,
            slot.label(),
            if on { "an" } else { "aus" }
        );
    }

    /// `v <name>` - load a ready-made chain.
    fn fx_preset(&mut self, arg: Option<&str>) {
        let names: Vec<&str> = FxPreset::loadable().iter().map(|p| p.label()).collect();
        let Some(preset) = arg.and_then(FxPreset::parse) else {
            self.message = format!("Aufruf: v <{}>", names.join("|"));
            return;
        };
        let track = self.active;
        self.send(Command::LoadFxPreset { track, preset });
        self.message = format!(
            "\"{}\": Preset \"{}\" geladen.",
            self.tracks[track].name,
            preset.label()
        );
    }

    /// `d <notenwert>` - the delay is tempo-synchronous, so what it takes is a note value.
    fn fx_delay_note(&mut self, arg: Option<&str>) {
        let names: Vec<&str> = DelayNote::all().iter().map(|n| n.label()).collect();
        let Some(note) = arg.and_then(DelayNote::parse) else {
            self.message = format!("Aufruf: d <{}>", names.join("|"));
            return;
        };
        let track = self.active;
        self.send(Command::SetFxParam {
            track,
            param: FxParam::DelayNote(note),
        });
        self.message = format!(
            "\"{}\": Delay auf {} (aus dem Tempo gerechnet).",
            self.tracks[track].name,
            note.label()
        );
    }

    /// Parse a 1-based layer number against what the engine says exists.
    fn layer_index(&mut self, arg: Option<&str>, ts: TrackStatus, usage: &str) -> Option<usize> {
        let Some(Ok(n)) = arg.map(str::parse::<usize>) else {
            self.message = format!("Aufruf: {usage}");
            return None;
        };
        if n == 0 || n > ts.layers as usize {
            self.message = if ts.layers == 0 {
                format!("\"{}\" hat keine Ebenen.", self.tracks[self.active].name)
            } else {
                format!(
                    "\"{}\" hat die Ebenen 1 bis {}.",
                    self.tracks[self.active].name, ts.layers
                )
            };
            return None;
        }
        Some(n - 1)
    }

    fn layer_mute(&mut self, arg: Option<&str>, ts: TrackStatus) {
        let Some(index) = self.layer_index(arg, ts, "e <nr>  (Ebene stumm/laut)") else {
            return;
        };
        let muted = ts.muted_mask & (1 << index) == 0;
        let track = self.active;
        self.send(Command::SetLayerMute {
            track,
            layer: index,
            muted,
        });
        self.message = format!(
            "\"{}\": Ebene {} {}.",
            self.tracks[track].name,
            index + 1,
            if muted { "stumm" } else { "wieder hoerbar" }
        );
    }

    fn layer_remove(&mut self, arg: Option<&str>, ts: TrackStatus) {
        let Some(index) = self.layer_index(arg, ts, "w <nr>  (Ebene weg)") else {
            return;
        };
        let track = self.active;
        self.send(Command::RemoveLayer {
            track,
            layer: index,
        });
        if index < self.tracks[track].gains.len() {
            self.tracks[track].gains.remove(index);
        }
        self.message = format!(
            "\"{}\": Ebene {} entfernt.",
            self.tracks[track].name,
            index + 1
        );
    }

    fn layer_gain(&mut self, nr: Option<&str>, value: Option<&str>, ts: TrackStatus) {
        let usage = "l <nr> <wert>  (Lautstaerke einer Ebene, 0.0 bis 4.0)";
        let Some(index) = self.layer_index(nr, ts, usage) else {
            return;
        };
        let Some(Ok(gain)) = value.map(str::parse::<f32>) else {
            self.message = format!("Aufruf: {usage}");
            return;
        };
        if !(0.0..=4.0).contains(&gain) {
            self.message = "Die Lautstaerke muss zwischen 0.0 und 4.0 liegen.".to_string();
            return;
        }
        let track = self.active;
        self.send(Command::SetLayerGain {
            track,
            layer: index,
            gain,
        });
        let gains = &mut self.tracks[track].gains;
        while gains.len() <= index {
            gains.push(1.0);
        }
        gains[index] = gain;
        self.message = format!(
            "\"{}\": Ebene {} auf {:.2}.",
            self.tracks[track].name,
            index + 1,
            gain
        );
    }

    /// `t <bpm> [schlaege_pro_takt] [zaehlzeit]` - only while nothing is recorded, because a new
    /// tempo redefines what every sample position means.
    fn set_tempo(
        &mut self,
        bpm: Option<&str>,
        beats: Option<&str>,
        unit: Option<&str>,
        last: Option<(Status, Instant)>,
    ) {
        let busy = last
            .map(|(s, _)| s.tracks().iter().any(|t| t.state != TrackState::Empty))
            .unwrap_or(false);
        if busy {
            self.message =
                "Tempo laesst sich nur aendern, wenn alle Tracks leer sind (erst a).".to_string();
            return;
        }
        let Some(Ok(bpm)) = bpm.map(str::parse::<f64>) else {
            self.message = "Aufruf: t <bpm> [schlaege_pro_takt] [zaehlzeit]".to_string();
            return;
        };
        let old = self.sched.timeline.signature();
        let beats_per_bar = match beats.map(str::parse::<u32>) {
            Some(Ok(v)) => v,
            None => old.beats_per_bar,
            Some(Err(_)) => {
                self.message = "Schlaege pro Takt muss eine ganze Zahl sein.".to_string();
                return;
            }
        };
        let beat_unit = match unit.map(str::parse::<u32>) {
            Some(Ok(v)) => v,
            None => old.beat_unit,
            Some(Err(_)) => {
                self.message = "Zaehlzeit muss eine ganze Zahl sein.".to_string();
                return;
            }
        };
        let signature = TimeSignature::new(beats_per_bar, beat_unit);
        let rate = self.sched.timeline.sample_rate();
        if let Err(e) = Timeline::validate(rate, bpm, signature) {
            self.message = e;
            return;
        }
        let timeline = Timeline::new(rate, bpm, signature);
        if let Err(e) = check_loop(&timeline, self.sched.bars, self.latency) {
            self.message = e;
            return;
        }
        let layer_capacity = loop_capacity(&timeline, self.sched.bars);
        self.send(Command::SetTempo {
            bpm,
            signature,
            layer_capacity,
        });
        // Every allocation of a new layer length happens here, in the control thread; the old stock
        // is dropped and the engine hands back what it still holds.
        self.pool.set_layer_len(layer_capacity as usize);
        self.sched.timeline = timeline;
        self.message = format!("Tempo {bpm} BPM, {beats_per_bar}/{beat_unit}.");
    }

    /// Where the audio thread is right now, as well as the main thread can know. The extrapolation
    /// itself lives in [`Scheduler::estimated_pos`].
    fn estimated_pos(&self, last: Option<(Status, Instant)>) -> u64 {
        self.sched
            .estimated_pos(last.map(|(s, seen)| (s.pos, seen.elapsed())))
    }
}

/// Non-blocking keyboard: a helper thread owns stdin, the main loop reads lines from a channel.
///
/// Line based rather than raw single keys - raw terminal mode would need another dependency, and
/// every action is quantised to a bar boundary anyway, so the extra Enter costs no accuracy.
fn spawn_keyboard() -> Receiver<String> {
    let (tx, rx) = channel();
    std::thread::spawn(move || {
        let stdin = std::io::stdin();
        loop {
            let mut line = String::new();
            match stdin.read_line(&mut line) {
                Ok(0) => break,
                Ok(_) => {
                    if tx.send(line).is_err() {
                        break;
                    }
                }
                Err(_) => break,
            }
        }
    });
    rx
}

fn on_off(v: bool) -> &'static str {
    if v { "an " } else { "aus" }
}

/// `[1 2* 3]`, with `*` for muted and the gain appended where it is not 1.
fn layer_list(ts: &TrackStatus, gains: &[f32]) -> String {
    if ts.layers == 0 {
        return "-".to_string();
    }
    let mut parts = Vec::with_capacity(ts.layers as usize);
    for i in 0..ts.layers as usize {
        let muted = ts.muted_mask & (1 << i) != 0;
        let gain = gains.get(i).copied().unwrap_or(1.0);
        let mut part = format!("{}", i + 1);
        if muted {
            part.push('*');
        }
        if (gain - 1.0).abs() > 0.001 {
            part.push_str(&format!("({gain:.2})"));
        }
        parts.push(part);
    }
    format!("[{}]", parts.join(" "))
}

/// The effect column of a track line: `stimme HEK-R 1/8.` - which preset, which effects are on,
/// and, when the delay is one of them, the note value it is locked to.
fn fx_text(fx: &FxStatus) -> String {
    if fx.bypass {
        return "aus".to_string();
    }
    let mut text = format!("{} {}", fx.preset.label(), fx.letters());
    if fx.settings.enabled[FxSlot::Delay.index()] {
        text.push(' ');
        text.push_str(fx.settings.delay_note.label());
    }
    text
}

/// The global line: bar, beat, loop length, tempo, click, and which grid takes snap to.
fn global_line(s: &Status, sched: &Scheduler, stats: &LiveStats) -> String {
    let tl = &sched.timeline;
    let bars = sched.bars;
    let beats_per_bar = tl.signature().beats_per_bar;
    let progress = if s.samples_per_beat > 0.0 {
        (s.beat_offset as f64 / s.samples_per_beat * 8.0) as usize
    } else {
        0
    };
    let bar_gfx: String = (0..8)
        .map(|i| if i <= progress { '#' } else { '.' })
        .collect();
    let loop_len = tl.span_bars(0, bars);

    let mut line = format!(
        "Takt {:>4} Schlag {}/{} [{}] | Loop {} Takte = {} Samples = {:.2} s | {:.1} BPM | Raster {} | Klick {} | Aus {}",
        s.bar + 1,
        s.beat + 1,
        beats_per_bar,
        bar_gfx,
        bars,
        loop_len,
        tl.samples_to_secs(loop_len),
        s.bpm,
        sched.quantize.label(),
        on_off(s.click),
        fmt_dbfs(s.output_peak),
    );
    let xruns = stats.xruns.load(Ordering::Relaxed);
    let underruns = stats.underruns.load(Ordering::Relaxed);
    let overruns = stats.overruns.load(Ordering::Relaxed);
    if xruns > 0 || underruns > 0 {
        line.push_str(&format!(" | Xruns {xruns}, FIFO leer {underruns}"));
    }
    if overruns > 0 {
        line.push_str(&format!(
            " | ACHTUNG {overruns} FIFO-Ueberlaeufe: Eingangssamples verloren, Aufnahme verschoben"
        ));
    }
    line
}

/// One line per track: name, input channel, state, layers, level, monitoring - and, while something
/// is armed, how much of the count-in is left. That last part is the difference between "scharf"
/// and knowing whether to pick the instrument up now or in five bars.
fn track_line(ts: &TrackStatus, ui: &TrackUi, tl: &Timeline, lead: Lead, active: bool) -> String {
    let loop_text = if ts.loop_len > 0 {
        format!("{:.2} s", tl.samples_to_secs(ts.loop_len))
    } else if ts.state == TrackState::Recording {
        format!("laeuft, {:.2} s", tl.samples_to_secs(ts.filled))
    } else {
        "-".to_string()
    };
    let state_text = match lead.text() {
        Some(text) => format!("{} - {text}", ts.state.label()),
        None => ts.state.label().to_string(),
    };
    format!(
        "{} {:<10} Ein {} | {:<34} | Loop {:>10} | Ebenen {:>2} {:<28} | Pegel {} | Mithoeren {} | FX {:<22}",
        if active { '>' } else { ' ' },
        ui.name,
        ui.channel + 1,
        state_text,
        loop_text,
        ts.layers,
        layer_list(ts, &ui.gains),
        fmt_dbfs(ts.input_peak),
        on_off(ts.monitor),
        fx_text(&ts.fx),
    )
}

/// Draw the block in place. Without cursor control every update is simply appended.
fn draw(lines: &[String], previous_lines: usize, simple: bool) {
    let mut out = String::new();
    if !simple && previous_lines > 0 {
        out.push_str(&format!("\x1b[{previous_lines}A"));
    }
    for line in lines {
        out.push('\r');
        out.push_str(&format!("{line:<LINE_WIDTH$}"));
        out.push('\n');
    }
    print!("{out}");
    let _ = std::io::stdout().flush();
}

pub fn cmd_live(dev: &DeviceOpts, opts: &LiveOpts) -> Result<(), String> {
    let signature = TimeSignature::new(opts.beats_per_bar, opts.beat_unit);
    let setup = audio::open_duplex(dev)?;
    let rate = setup.sample_rate();
    if setup.out_plan.config.sample_rate != rate {
        return Err(format!(
            "Ein- und Ausgang laufen auf verschiedenen Sampleraten ({} und {}). \
             Die Zeitachse der Engine setzt eine gemeinsame Clock voraus.",
            rate, setup.out_plan.config.sample_rate
        ));
    }
    Timeline::validate(rate, opts.bpm, signature)?;
    let timeline = Timeline::new(rate, opts.bpm, signature);
    check_loop(&timeline, opts.bars, opts.latency_samples)?;

    let in_channels = setup.in_plan.config.channels as usize;
    let out_channels = setup.out_plan.config.channels as usize;
    let defs = resolve_tracks(&opts.tracks, in_channels)?;

    audio::print_setup(&setup);
    let loop_len = timeline.span_bars(0, opts.bars);
    println!(
        "Takt:     {:.1} BPM, {}/{}, {:.3} Samples pro Schlag",
        opts.bpm,
        opts.beats_per_bar,
        opts.beat_unit,
        timeline.samples_per_beat()
    );
    println!(
        "Loop:     {} Takte = {} Samples = {:.2} s",
        opts.bars,
        loop_len,
        timeline.samples_to_secs(loop_len)
    );
    println!(
        "Raster:   {} - {}",
        opts.quantize.label(),
        opts.quantize.explanation()
    );
    let capacity = loop_capacity(&timeline, opts.bars);
    println!("{}", limits_line(capacity, defs.len(), SPARE_SLOTS));
    println!("Tracks:");
    for (i, def) in defs.iter().enumerate() {
        println!(
            "  {} {:<10} Eingang {} (Kanal {} von {})",
            i + 1,
            def.name,
            def.channel + 1,
            def.channel + 1,
            in_channels
        );
    }
    println!(
        "Latenz:   {} Samples ({:.2} ms) werden beim Aufnehmen herausgerechnet",
        opts.latency_samples,
        opts.latency_samples as f64 * 1000.0 / rate as f64
    );
    println!(
        "          Der Wert stammt aus der Loopback-Messung. Nach jeder Aenderung von Geraet,\n\
         \x20         Samplerate oder Puffergroesse neu messen (Subcommand calibrate)."
    );

    let buffer_frames = setup.buffer_frames().max(1);
    // Same reasoning as in audio.rs: room for a driver block far larger than requested, so the
    // callback never has to allocate even if the driver ignores the request.
    let scratch_frames = (buffer_frames * 16) as usize;

    let stats = Arc::new(LiveStats::default());
    let (mut producer, mut consumer) =
        rtrb::RingBuffer::<f32>::new((buffer_frames * INPUT_FIFO_BUFFERS) as usize * in_channels);
    let (cmd_tx, cmd_rx) = command_channel(256);
    let (status_tx, mut status_rx) = status_channel(1024);
    // The return queue has room for every buffer that can be inside the engine at once, so the
    // audio thread can always hand one back instead of leaking it.
    let (channel, buffer_endpoint) = buffer_channel(
        SPARE_SLOTS + 4,
        defs.len() * MAX_LAYERS + SPARE_SLOTS + 8,
    );
    let mut pool = LayerPool::new(channel, capacity as usize, SPARE_SLOTS);
    // Every layer buffer of the session is born here, in the control thread, before any stream
    // exists; from now on the pool only recycles.
    pool.service(None);

    let tracks: Vec<Track> = defs
        .iter()
        .map(|d| Track::new(d.channel, opts.monitor, rate))
        .collect();
    let core = EngineCore::new(EngineConfig {
        timeline,
        latency_samples: opts.latency_samples,
        input_channels: in_channels,
        tracks,
        spares: Vec::with_capacity(SPARE_SLOTS),
        layer_capacity: capacity,
        commands: cmd_rx,
        status: status_tx,
        buffers: buffer_endpoint,
        monitor_gain: opts.monitor_gain,
        click: !opts.no_click,
        click_gain: opts.click_gain,
        status_interval: (rate / STATUS_HZ).max(1) as u64,
    });

    // ---- input stream: whole frames into the FIFO, nothing else --------------------------
    let stats_in = Arc::clone(&stats);
    let stats_in_err = Arc::clone(&stats);
    let input = audio::build_input(
        &setup.input,
        &setup.in_plan,
        move |data: &[f32], _: &InputCallbackInfo, _offset: usize| {
            let t0 = Instant::now();
            let mut overrun = false;
            for frame in data.chunks_exact(in_channels) {
                // Whole frames only: half a frame in the FIFO would shift every channel of every
                // later recording against each other.
                if producer.slots() < in_channels {
                    overrun = true;
                    break;
                }
                for &sample in frame {
                    let _ = producer.push(sample);
                }
            }
            if overrun {
                stats_in.overruns.fetch_add(1, Ordering::Relaxed);
            }
            stats_in.record_callback(t0);
        },
        move |err| stats_in_err.count_error(err),
    )?;

    // ---- output stream: drives the engine ------------------------------------------------
    let stats_out = Arc::clone(&stats);
    let stats_out_err = Arc::clone(&stats);
    let mut core = core;
    let mut in_scratch = vec![0.0f32; scratch_frames * in_channels];
    let mut out_scratch = vec![0.0f32; scratch_frames];
    let output = audio::build_output(
        &setup.output,
        &setup.out_plan,
        move |data: &mut [f32], _: &OutputCallbackInfo, _offset: usize| {
            let t0 = Instant::now();
            let frames = data.len() / out_channels;
            let mut done = 0usize;
            while done < frames {
                let n = (frames - done).min(scratch_frames);
                let mut got = 0usize;
                while got < n && consumer.slots() >= in_channels {
                    for c in 0..in_channels {
                        if let Ok(v) = consumer.pop() {
                            in_scratch[got * in_channels + c] = v;
                        }
                    }
                    got += 1;
                }
                if got < n {
                    stats_out.underruns.fetch_add(1, Ordering::Relaxed);
                }
                core.process(&in_scratch[..got * in_channels], &mut out_scratch[..n]);
                let base = done * out_channels;
                for (i, frame) in data[base..base + n * out_channels]
                    .chunks_exact_mut(out_channels)
                    .enumerate()
                {
                    let v = out_scratch[i];
                    for sample in frame.iter_mut() {
                        *sample = v;
                    }
                }
                done += n;
            }
            stats_out.record_callback(t0);
        },
        move |err| stats_out_err.count_error(err),
    )?;

    if let Ok(frames) = input.buffer_size() {
        println!("Puffer laut Treiber: Eingang {frames} Frames");
    }
    if let Ok(frames) = output.buffer_size() {
        println!("Puffer laut Treiber: Ausgang {frames} Frames");
    }

    // Order matters: input first, then output, exactly as in the phase 0 latency measurement. Both
    // frame counters start on their stream's first callback, and only the same start order gives
    // them the same origin the 827 samples were measured against.
    input
        .play()
        .map_err(|e| format!("Eingangsstream startet nicht: {e}"))?;
    output
        .play()
        .map_err(|e| format!("Ausgabestream startet nicht: {e}"))?;

    let grid = match opts.quantize {
        Quantize::Bar => "der naechsten Taktgrenze",
        Quantize::Loop => "dem naechsten Loop-Anfang",
    };
    println!(
        "\nTasten (jeweils mit Enter bestaetigen). Alles ausser 1-{} wirkt auf den gewaehlten Track:\n\
         \x20 1-{}  Track waehlen\n\
         \x20 r   neuer Loop ab {grid}, {} Takte (ersetzt vorhandene Ebenen)\n\
         \x20 o   Overdub: weitere Ebene ab {grid}, eine Loop-Laenge lang\n\
         \x20 s   Stopp: laufende Aufnahme auf der naechsten Taktgrenze beenden, sonst Wiedergabe aus\n\
         \x20 p   Wiedergabe ab der naechsten Taktgrenze\n\
         \x20 c   diesen Track leeren\n\
         \x20 a   alles leeren (alle Tracks, Zustand wie frisch gestartet)\n\
         \x20 m   Mithoeren an/aus (unabhaengig von der Wiedergabe)\n\
         \x20 e <nr>        Ebene stumm/laut\n\
         \x20 w <nr>        Ebene weg\n\
         \x20 l <nr> <wert> Lautstaerke einer Ebene (0.0 bis 4.0)\n\
         \x20 k   Klick an/aus\n\
         \x20 t   Tempo aendern, z.B. \"t 120\" oder \"t 120 7 8\" (nur wenn alles leer ist)\n\
         Effekte - wirken auf Wiedergabe und Mithoeren, die Aufnahme bleibt immer trocken:\n\
         \x20 v <name>      Preset laden: stimme | gitarre | trocken\n\
         \x20 f             ganze Effektkette an/aus (aus = bitgleich durchgereicht)\n\
         \x20 x <1-5>       einzeln: 1 Hochpass, 2 EQ, 3 Kompressor, 4 Delay, 5 Hall\n\
         \x20 d <wert>      Delay-Notenwert: 1/4 | 1/8. | 1/8 | 1/8T (aus dem Tempo gerechnet)\n\
         \x20 q   Beenden\n",
        defs.len(),
        defs.len(),
        opts.bars
    );

    let keys = spawn_keyboard();
    let mut control = Control {
        cmd: cmd_tx,
        pool,
        // The head start is four buffers plus 20 ms; the Scheduler works it out and owns every
        // other rule about when a command may take effect.
        sched: Scheduler::new(timeline, opts.bars, buffer_frames, opts.quantize),
        latency: opts.latency_samples,
        tracks: defs
            .iter()
            .map(|d| TrackUi {
                name: d.name.clone(),
                channel: d.channel,
                gains: Vec::new(),
            })
            .collect(),
        active: 0,
        message: String::new(),
        running: true,
    };
    let mut last: Option<(Status, Instant)> = None;
    let mut last_print = Instant::now() - DISPLAY_INTERVAL;
    let mut printed_lines = 0usize;
    let mut last_refusal = (0u64, Refusal::None);

    while control.running {
        if let Some(s) = status_rx.latest() {
            if s.stopped {
                control.running = false;
            }
            if s.ignored_commands > last_refusal.0 || s.refusal != last_refusal.1 {
                if let Some(text) = s.refusal.message() {
                    control.message = text.to_string();
                }
                last_refusal = (s.ignored_commands, s.refusal);
            }
            last = Some((s, Instant::now()));
        }
        // The only place buffers are allocated, zeroed and dropped.
        control.pool.service(last.as_ref().map(|(s, _)| s));

        match keys.recv_timeout(Duration::from_millis(20)) {
            Ok(line) => control.handle(&line, last),
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => break,
        }

        if last_print.elapsed() >= DISPLAY_INTERVAL {
            last_print = Instant::now();
            if let Some((s, _)) = last {
                let sched = control.sched;
                let mut lines = vec![global_line(&s, &sched, &stats)];
                for (i, ui) in control.tracks.iter_mut().enumerate() {
                    let ts = s.tracks().get(i).copied().unwrap_or_default();
                    // Keep the gain mirror the same length the engine reports.
                    while ui.gains.len() < ts.layers as usize {
                        ui.gains.push(1.0);
                    }
                    ui.gains.truncate(ts.layers as usize);
                    let lead = sched.lead(i, s.pos);
                    lines.push(track_line(&ts, ui, &sched.timeline, lead, i == control.active));
                }
                lines.push(format!("  {}", control.message));
                draw(&lines, printed_lines, opts.simple_display);
                printed_lines = lines.len();
            }
        }
    }

    drop(input);
    drop(output);
    control.pool.drain();

    println!("\n");
    println!("Xruns (cpal):        {}", stats.xruns.load(Ordering::Relaxed));
    println!(
        "FIFO leer:           {}",
        stats.underruns.load(Ordering::Relaxed)
    );
    println!(
        "FIFO uebergelaufen:  {}",
        stats.overruns.load(Ordering::Relaxed)
    );
    println!(
        "Sonstige Fehler:     {}",
        stats.other_errors.load(Ordering::Relaxed)
    );
    println!(
        "Callback-Dauer max:  {:.3} ms",
        stats.cb_nanos_max.load(Ordering::Relaxed) as f64 / 1e6
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn specs(list: &[&str]) -> Vec<String> {
        list.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn track_arguments_are_parsed_one_based() {
        assert_eq!(
            parse_track_arg("stimme:1").unwrap(),
            TrackDef {
                name: "stimme".to_string(),
                channel: 0
            }
        );
        assert_eq!(parse_track_arg(" gitarre : 2 ").unwrap().channel, 1);
        for bad in ["stimme", "stimme:", ":1", "stimme:0", "stimme:x"] {
            let err = parse_track_arg(bad).expect_err(bad);
            assert!(err.contains("--track"), "deutsche Meldung fehlt: {err}");
        }
    }

    #[test]
    fn the_default_setup_is_voice_and_guitar_on_the_first_two_inputs() {
        let two = resolve_tracks(&[], 2).unwrap();
        assert_eq!(two.len(), 2);
        assert_eq!(two[0].name, "stimme");
        assert_eq!(two[0].channel, 0);
        assert_eq!(two[1].name, "gitarre");
        assert_eq!(two[1].channel, 1);
        // A mono device gets one track instead of an error.
        assert_eq!(resolve_tracks(&[], 1).unwrap().len(), 1);
        assert!(resolve_tracks(&[], 0).is_err());
    }

    #[test]
    fn a_channel_the_device_does_not_have_is_refused_in_german() {
        let err = resolve_tracks(&specs(&["stimme:1", "gitarre:4"]), 2).expect_err("muss scheitern");
        assert!(err.contains("gitarre"), "{err}");
        assert!(err.contains("Eingang 4"), "{err}");
        assert!(err.contains("--in-channels"), "Hinweis fehlt: {err}");

        let err = resolve_tracks(&specs(&["a:1", "a:2"]), 4).expect_err("doppelter Name");
        assert!(err.contains("eindeutig"), "{err}");

        let many: Vec<String> = (1..=MAX_TRACKS + 1).map(|i| format!("t{i}:1")).collect();
        assert!(resolve_tracks(&many, 8).is_err(), "Obergrenze greift");
    }

    /// The one thing the musician reads off the screen while playing: is the chain doing anything,
    /// which preset is it, and what is on.
    #[test]
    fn the_effect_column_says_what_is_switched_on() {
        let mut chain = super::super::fx::Chain::new(48_000);
        chain.load_preset(FxPreset::Voice);
        let ts = TrackStatus {
            fx: chain.status(),
            ..Default::default()
        };
        assert_eq!(fx_text(&ts.fx), "stimme HEK-R");

        chain.set_enabled(FxSlot::Delay, true);
        chain.set_param(FxParam::DelayNote(DelayNote::Quarter));
        let ts = TrackStatus {
            fx: chain.status(),
            ..Default::default()
        };
        assert_eq!(fx_text(&ts.fx), "eigen HEKDR 1/4");

        chain.set_bypass(true);
        let ts = TrackStatus {
            fx: chain.status(),
            ..Default::default()
        };
        assert_eq!(fx_text(&ts.fx), "aus");
        // A track nobody has touched shows nothing either, because it starts bypassed.
        assert_eq!(fx_text(&TrackStatus::default().fx), "aus");
    }

    #[test]
    fn the_layer_list_shows_mute_and_gain() {
        let ts = TrackStatus {
            layers: 3,
            muted_mask: 0b010,
            ..Default::default()
        };
        assert_eq!(layer_list(&ts, &[1.0, 1.0, 0.5]), "[1 2* 3(0.50)]");
        assert_eq!(layer_list(&TrackStatus::default(), &[]), "-");
    }
}
