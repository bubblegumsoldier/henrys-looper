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
//! `--latency-frames` still deserves one check against the real path: record the click through a
//! loopback cable and look at where it lands (subcommand `calibrate`). That is the honest way to
//! confirm the number for a different device, sample rate or buffer size.
//!
//! Since a plugin host on an ASIO router does not travel the same way in as a microphone, that
//! number is a **default**: `--track-latency NAME:WERT[+ZUSCHLAG]` gives one track its own, and
//! `calibrate --for-track` measures it. See [`super::track::TrackLatency`].

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
    Command, CommandSender, LayerPool, MAX_TRACKS, Refusal, Status, StatusReceiver, TrackStatus,
    buffer_channel, command_channel, status_channel,
};
use super::bus::{
    BUS_COUNT, BUS_SAMPLES, Bus, BusOutput, BusRouting, BusSend, MAX_BUS_GAIN, TrackSource,
    place_buses, routing_note,
};
use super::frame::{Channels, TrackInput};
use super::fx::{DelayNote, FxParam, FxPreset, FxSlot, FxStatus};
use super::process::{
    EngineConfig, EngineCore, check_loop, limits_line, loop_capacity, max_latency, spare_channels,
    spare_slots_for, total_channels,
};
use super::schedule::{Lead, Quantize, Scheduled, Scheduler};
use super::timeline::{TimeSignature, Timeline};
use super::track::{MAX_LAYERS, Track, TrackLatency, TrackState};

/// Input FIFO size in units of the audio buffer size. Generous on purpose: it costs a few dozen kB
/// and it is the difference between a hiccup and a permanently misaligned recording.
const INPUT_FIFO_BUFFERS: u32 = 64;
/// Status snapshots per second the audio thread produces. The display consumes 10 of them.
const STATUS_HZ: u32 = 200;
/// Terminal refresh rate, as required: no more than ten updates per second.
pub(crate) const DISPLAY_INTERVAL: Duration = Duration::from_millis(100);
/// Prepared layer buffers the control thread keeps in the engine, so an overdub never waits for an
/// allocation. Three is one for the take that is starting, one for a second track starting at the
/// same bar, and one in reserve.
pub(crate) const SPARE_SLOTS: usize = 3;
/// Width the status block is padded to, so remains of a longer previous line are erased.
const LINE_WIDTH: usize = 208;

#[derive(Args, Debug, Clone)]
pub struct LiveOpts {
    /// Track als NAME:KANAL (mono) oder NAME:KANAL-KANAL (stereo), mehrfach angebbar. KANAL ist
    /// die Eingangsnummer wie am Geraet beschriftet (1-basiert), z.B.
    /// --track stimme:1 --track gitarre:2 --track klavier:3-4.
    /// Optional dahinter das Panorama, -1 links bis 1 rechts: --track stimme:1@-0.3
    #[arg(long = "track", value_name = "NAME:KANAL[-KANAL][@PANORAMA]")]
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

    /// Roundtrip-Latenz in Frames, die beim Aufnehmen herausgerechnet wird - die Vorgabe fuer
    /// jeden Track, der nichts eigenes sagt. Ein Frame ist ein Zeitpunkt: ein Sample bei einer
    /// Mono-Quelle, zwei bei einer Stereo-Quelle.
    /// Standard ist der Messwert aus Phase 0 (128 Frames, 48 kHz, Scarlett 2i2 an ASIO).
    #[arg(long = "latency-frames", alias = "latency-samples", default_value_t = 827)]
    pub latency_frames: u64,

    /// Eigene Latenzkompensation fuer einen Track, mehrfach angebbar:
    /// NAME:FRAMES fuer den gemessenen Wert, NAME:FRAMES+ZUSCHLAG mit manuellem Aufschlag,
    /// NAME:+ZUSCHLAG nur Aufschlag auf die globale Vorgabe.
    /// Beispiel: --track-latency cantabile:512+96
    #[arg(long = "track-latency", value_name = "NAME:FRAMES[+ZUSCHLAG]")]
    pub track_latencies: Vec<String>,

    /// Ausgangspaar eines Busses, mehrfach angebbar: BUS:KANAL[-KANAL] mit BUS = main oder
    /// monitor. Der Klick liegt immer und nur auf dem Monitor-Bus.
    /// Voreinstellung: ab vier Ausgaengen main:1-2 und monitor:3-4, sonst beide auf 1-2 (dann
    /// summiert, der Klick geht mit in den Saal).
    /// Notbehelf mit zwei Ausgaengen: --bus-out main:1 --bus-out monitor:2 (Mix links, Klick
    /// rechts, mit einem Y-Kabel aufzutrennen).
    #[arg(long = "bus-out", value_name = "BUS:KANAL[-KANAL]")]
    pub bus_outs: Vec<String>,

    /// Lautstaerke eines Busses, mehrfach angebbar: BUS:WERT, z.B. --bus-gain monitor:0.8.
    /// Damit laesst sich der Kopfhoerer regeln, ohne den Saal zu veraendern.
    #[arg(long = "bus-gain", value_name = "BUS:WERT")]
    pub bus_gains: Vec<String>,

    /// Auf welche Busse die Loop-Wiedergabe eines Tracks geht, mehrfach angebbar:
    /// NAME:main | monitor | main+monitor | none. Voreinstellung main+monitor.
    #[arg(long = "track-bus", value_name = "NAME:BUS")]
    pub track_buses: Vec<String>,

    /// Auf welche Busse das Mithoer-Signal eines Tracks geht, unabhaengig von der Wiedergabe.
    /// Voreinstellung monitor - der Saal hoert den Musiker in der Regel ueber die PA.
    #[arg(long = "track-monitor-bus", value_name = "NAME:BUS")]
    pub track_monitor_buses: Vec<String>,

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
#[derive(Clone, Debug, PartialEq)]
pub struct TrackDef {
    pub name: String,
    /// Which device input channel or channel pair this track records, zero-based.
    pub input: TrackInput,
    /// Position in the stereo field, -1.0 to +1.0. Centre unless the argument said otherwise.
    pub pan: f32,
    /// Latency compensation of this track. Follows the global default unless `--track-latency`
    /// (or the app, or the score) said otherwise.
    pub latency: TrackLatency,
    /// Which output buses this track's loop playback feeds. Both by default: a loop belongs in the
    /// room *and* in the headphones.
    pub loop_send: BusSend,
    /// Which output buses this track's monitored live input feeds. The headphones by default; see
    /// [`super::bus::TrackSource`].
    pub monitor_send: BusSend,
}

impl TrackDef {
    pub fn channels(&self) -> Channels {
        self.input.channels()
    }

    /// A mono track on one input channel, centred, on the global compensation - the shape the
    /// default setup and most tests want.
    pub fn mono(name: &str, channel: usize) -> Self {
        Self {
            name: name.to_string(),
            input: TrackInput::Mono(channel),
            pan: 0.0,
            latency: TrackLatency::INHERITED,
            loop_send: BusSend::BOTH,
            monitor_send: BusSend::MONITOR,
        }
    }
}

/// Highest latency any track may be given, in frames: two seconds at 48 kHz.
///
/// Not a technical limit but a typo catch. A roundtrip beyond this is not a setup, it is a missing
/// digit - and the error it would produce (a loop that cannot be shorter than the compensation)
/// would point at the loop length instead of at the number that is wrong.
pub const MAX_LATENCY_FRAMES: i64 = 96_000;

/// Parse one `--track-latency` argument into a track name and its compensation.
///
/// ```text
/// cantabile:512        gemessener Wert 512 Frames, kein Zuschlag
/// cantabile:512+96     gemessen 512, plus 96 Frames von Hand
/// cantabile:512-40     gemessen 512, minus 40
/// cantabile:+96        globale Vorgabe plus 96 - fuer eine Quelle, die nie gemessen wurde
/// ```
///
/// The two halves stay separate all the way through the program: a calibration writes the first
/// one and never the second. See [`TrackLatency`].
pub fn parse_track_latency_arg(spec: &str) -> Result<(String, TrackLatency), String> {
    let spec = spec.trim();
    let Some((name, value)) = spec.rsplit_once(':') else {
        return Err(format!(
            "--track-latency \"{spec}\" ist unvollstaendig. Erwartet wird NAME:FRAMES, \
             NAME:FRAMES+ZUSCHLAG oder NAME:+ZUSCHLAG, z.B. --track-latency cantabile:512+96"
        ));
    };
    let name = name.trim();
    if name.is_empty() {
        return Err(format!(
            "--track-latency \"{spec}\": vor dem Doppelpunkt fehlt der Trackname."
        ));
    }
    let value = value.trim();
    if value.is_empty() {
        return Err(format!(
            "--track-latency \"{spec}\": hinter dem Doppelpunkt fehlt die Zahl."
        ));
    }

    let number = |text: &str, what: &str| -> Result<i64, String> {
        text.trim().parse::<i64>().map_err(|_| {
            format!(
                "--track-latency \"{spec}\": \"{}\" ist keine {what} in Frames.",
                text.trim()
            )
        })
    };
    let in_range = |value: i64, what: &str| -> Result<(), String> {
        if value.abs() > MAX_LATENCY_FRAMES {
            return Err(format!(
                "--track-latency \"{spec}\": {what} liegt mit {value} Frames ausserhalb von \
                 {MAX_LATENCY_FRAMES} Frames (zwei Sekunden bei 48 kHz). Das ist keine Latenz, \
                 sondern ein Tippfehler."
            ));
        }
        Ok(())
    };

    // A leading sign means "only a surcharge"; anything else starts with the measured value, and a
    // sign further along splits the two. `char_indices` rather than byte indexing, so a stray
    // umlaut produces the German parse error and not a panic on a char boundary.
    let mut chars = value.char_indices();
    let first = chars.next().expect("nicht leer").1;
    let (measured, trim) = if first == '+' || first == '-' {
        (None, number(value, "Zuschlagszahl")?)
    } else {
        match chars.find(|(_, c)| *c == '+' || *c == '-').map(|(i, _)| i) {
            Some(cut) => (
                Some(number(&value[..cut], "Frame-Zahl")?),
                number(&value[cut..], "Zuschlagszahl")?,
            ),
            None => (Some(number(value, "Frame-Zahl")?), 0),
        }
    };
    in_range(trim, "der Zuschlag")?;
    if let Some(measured) = measured {
        in_range(measured, "der gemessene Wert")?;
    }

    Ok((
        name.to_string(),
        TrackLatency {
            measured: measured.map(|m| m as u32),
            trim: trim as i32,
        },
    ))
}

/// Apply every `--track-latency` argument to the tracks it names.
///
/// An argument naming a track that does not exist is an error rather than a silent no-op: a typo
/// there would leave the source it was meant for on the wrong number, and that is exactly the kind
/// of mistake nobody hears until two takes are laid on top of each other.
pub fn apply_track_latencies(defs: &mut [TrackDef], specs: &[String]) -> Result<(), String> {
    for spec in specs {
        let (name, latency) = parse_track_latency_arg(spec)?;
        match defs.iter_mut().find(|d| d.name == name) {
            Some(def) => def.latency = latency,
            None => {
                let known: Vec<&str> = defs.iter().map(|d| d.name.as_str()).collect();
                return Err(format!(
                    "--track-latency \"{spec}\": den Track \"{name}\" gibt es nicht. \
                     Vorhanden: {}.",
                    known.join(", ")
                ));
            }
        }
    }
    Ok(())
}

/// Parse one `--track` argument.
///
/// The shapes, all with 1-based channel numbers as they are printed on the interface:
///
/// ```text
/// stimme:1            mono auf Eingang 1
/// klavier:3-4         stereo auf den Eingaengen 3 und 4
/// gitarre:2@-0.4      mono, etwas nach links
/// ```
///
/// The pair is written with a dash because that is how an interface labels a stereo return
/// ("3-4"), and it is the one separator that cannot be confused with the colon in front of it.
pub fn parse_track_arg(spec: &str) -> Result<TrackDef, String> {
    let spec = spec.trim();
    // The pan is split off first, so a name may not contain '@' but a channel spec cannot be
    // confused with one either.
    let (head, pan) = match spec.rsplit_once('@') {
        Some((head, pan)) => {
            let value: f32 = pan.trim().parse().map_err(|_| {
                format!(
                    "--track \"{spec}\": \"{}\" ist keine Panorama-Zahl. Erwartet wird -1 (ganz \
                     links) bis 1 (ganz rechts), z.B. --track stimme:1@-0.5",
                    pan.trim()
                )
            })?;
            if !(-1.0..=1.0).contains(&value) {
                return Err(format!(
                    "--track \"{spec}\": das Panorama muss zwischen -1 (ganz links) und 1 (ganz \
                     rechts) liegen, gefunden {value}."
                ));
            }
            (head, value)
        }
        None => (spec, 0.0),
    };

    let Some((name, channels)) = head.rsplit_once(':') else {
        return Err(format!(
            "--track \"{spec}\" ist unvollstaendig. Erwartet wird NAME:KANAL fuer mono oder \
             NAME:KANAL-KANAL fuer stereo, z.B. --track stimme:1 oder --track klavier:3-4"
        ));
    };
    let name = name.trim();
    if name.is_empty() {
        return Err(format!("--track \"{spec}\": vor dem Doppelpunkt fehlt der Name."));
    }

    let number = |text: &str| -> Result<usize, String> {
        let text = text.trim();
        let value: usize = text.parse().map_err(|_| {
            format!(
                "--track \"{spec}\": \"{text}\" ist keine Kanalnummer. Erwartet wird eine ganze \
                 Zahl ab 1."
            )
        })?;
        if value == 0 {
            return Err(format!(
                "--track \"{spec}\": Kanaele werden ab 1 gezaehlt, wie am Geraet beschriftet."
            ));
        }
        Ok(value - 1)
    };

    let input = match channels.trim().split_once('-') {
        Some((left, right)) => {
            let left = number(left)?;
            let right = number(right)?;
            if left == right {
                return Err(format!(
                    "--track \"{spec}\": ein Stereo-Track braucht zwei verschiedene Eingaenge. \
                     Fuer einen Mono-Track reicht --track {name}:{}",
                    left + 1
                ));
            }
            TrackInput::Stereo { left, right }
        }
        None => TrackInput::Mono(number(channels)?),
    };

    Ok(TrackDef {
        name: name.to_string(),
        input,
        pan,
        // The compensation is a separate argument: it is set once per interface and cabling, while
        // the channel and the pan are set per song.
        latency: TrackLatency::INHERITED,
        // The bus routing is a separate argument for the same reason, and because cramming a third
        // suffix onto `NAME:KANAL@PAN` would make the one argument nobody could read.
        loop_send: BusSend::BOTH,
        monitor_send: BusSend::MONITOR,
    })
}

/// Parse one `--track-bus` / `--track-monitor-bus` argument into a track name and its routing.
///
/// ```text
/// stimme:main            nur in den Saal
/// stimme:monitor         nur auf den Kopfhoerer
/// klick_track:main+monitor
/// alt:none               vorerst nirgends
/// ```
pub fn parse_track_send_arg(flag: &str, spec: &str) -> Result<(String, BusSend), String> {
    let spec = spec.trim();
    let Some((name, value)) = spec.rsplit_once(':') else {
        return Err(format!(
            "{flag} \"{spec}\" ist unvollstaendig. Erwartet wird NAME:BUS, z.B. \
             {flag} stimme:main+monitor"
        ));
    };
    let name = name.trim();
    if name.is_empty() {
        return Err(format!(
            "{flag} \"{spec}\": vor dem Doppelpunkt fehlt der Trackname."
        ));
    }
    let Some(send) = BusSend::parse(value) else {
        return Err(format!(
            "{flag} \"{spec}\": \"{}\" ist keine Bus-Angabe. Erlaubt sind main, monitor, \
             main+monitor und none.",
            value.trim()
        ));
    };
    Ok((name.to_string(), send))
}

/// Apply every `--track-bus` / `--track-monitor-bus` argument to the track it names.
///
/// An unknown name is an error for the same reason `--track-latency` refuses one: a typo would
/// leave the track it was meant for on a bus the musician thinks he changed, and he would find out
/// on stage.
pub fn apply_track_sends(
    defs: &mut [TrackDef],
    source: TrackSource,
    specs: &[String],
) -> Result<(), String> {
    let flag = match source {
        TrackSource::Loop => "--track-bus",
        TrackSource::Monitor => "--track-monitor-bus",
    };
    for spec in specs {
        let (name, send) = parse_track_send_arg(flag, spec)?;
        match defs.iter_mut().find(|d| d.name == name) {
            Some(def) => match source {
                TrackSource::Loop => def.loop_send = send,
                TrackSource::Monitor => def.monitor_send = send,
            },
            None => {
                let known: Vec<&str> = defs.iter().map(|d| d.name.as_str()).collect();
                return Err(format!(
                    "{flag} \"{spec}\": den Track \"{name}\" gibt es nicht. Vorhanden: {}.",
                    known.join(", ")
                ));
            }
        }
    }
    Ok(())
}

/// Parse one `--bus-out` argument: `main:1-2`, `monitor:3-4`, `monitor:2`.
///
/// A single channel is not a mistake, it is the stopgap for a two-output interface: `--bus-out
/// main:1 --bus-out monitor:2` puts the mix on the left and the click on the right.
pub fn parse_bus_out_arg(spec: &str) -> Result<(Bus, BusOutput), String> {
    let spec = spec.trim();
    let Some((bus, channels)) = spec.rsplit_once(':') else {
        return Err(format!(
            "--bus-out \"{spec}\" ist unvollstaendig. Erwartet wird BUS:KANAL[-KANAL], z.B. \
             --bus-out monitor:3-4"
        ));
    };
    let Some(bus) = Bus::parse(bus) else {
        return Err(format!(
            "--bus-out \"{spec}\": \"{}\" ist kein Bus. Es gibt main und monitor.",
            bus.trim()
        ));
    };
    let number = |text: &str| -> Result<usize, String> {
        let text = text.trim();
        let value: usize = text.parse().map_err(|_| {
            format!("--bus-out \"{spec}\": \"{text}\" ist keine Kanalnummer (ganze Zahl ab 1).")
        })?;
        if value == 0 {
            return Err(format!(
                "--bus-out \"{spec}\": Kanaele werden ab 1 gezaehlt, wie am Geraet beschriftet."
            ));
        }
        Ok(value - 1)
    };
    let out = match channels.trim().split_once('-') {
        Some((left, right)) => {
            let left = number(left)?;
            let right = number(right)?;
            if right != left + 1 {
                return Err(format!(
                    "--bus-out \"{spec}\": ein Bus liegt auf einem benachbarten Kanalpaar, also \
                     z.B. {}-{}. Fuer einen einzelnen Kanal reicht {}:{}.",
                    left + 1,
                    left + 2,
                    bus.name(),
                    left + 1
                ));
            }
            BusOutput::pair(left)
        }
        None => BusOutput::mono(number(channels)?),
    };
    Ok((bus, out))
}

/// Parse one `--bus-gain` argument: `monitor:0.8`.
pub fn parse_bus_gain_arg(spec: &str) -> Result<(Bus, f32), String> {
    let spec = spec.trim();
    let Some((bus, value)) = spec.rsplit_once(':') else {
        return Err(format!(
            "--bus-gain \"{spec}\" ist unvollstaendig. Erwartet wird BUS:WERT, z.B. \
             --bus-gain monitor:0.8"
        ));
    };
    let Some(bus) = Bus::parse(bus) else {
        return Err(format!(
            "--bus-gain \"{spec}\": \"{}\" ist kein Bus. Es gibt main und monitor.",
            bus.trim()
        ));
    };
    let gain: f32 = value.trim().parse().map_err(|_| {
        format!(
            "--bus-gain \"{spec}\": \"{}\" ist keine Zahl.",
            value.trim()
        )
    })?;
    if !(0.0..=MAX_BUS_GAIN).contains(&gain) {
        return Err(format!(
            "--bus-gain \"{spec}\": die Lautstaerke muss zwischen 0.0 und {MAX_BUS_GAIN} liegen."
        ));
    }
    Ok((bus, gain))
}

/// Turn the `--bus-out` arguments into a routing, starting from the default for this device.
pub fn resolve_routing(specs: &[String], out_channels: usize) -> Result<BusRouting, String> {
    let mut routing = BusRouting::default_for(out_channels);
    for spec in specs {
        let (bus, out) = parse_bus_out_arg(spec)?;
        if out.first + out.width > out_channels {
            return Err(format!(
                "--bus-out \"{}\": das Geraet hat {} Ausgangskanaele, Kanal {} gibt es nicht.\n\
                 \x20 - anderes Paar waehlen: --bus-out {}:1-2\n\
                 \x20 - oder mehr Kanaele oeffnen: --out-channels {}",
                spec.trim(),
                out_channels,
                out.first + out.width,
                bus.name(),
                out.first + out.width
            ));
        }
        routing.set(bus, out);
    }
    Ok(routing.clamped(out_channels))
}

/// Turn the `--bus-gain` arguments into a volume per bus.
pub fn resolve_bus_gains(specs: &[String]) -> Result<[f32; BUS_COUNT], String> {
    let mut gains = [1.0f32; BUS_COUNT];
    for spec in specs {
        let (bus, gain) = parse_bus_gain_arg(spec)?;
        gains[bus.index()] = gain;
    }
    Ok(gains)
}

/// Turn the `--track` arguments into track definitions, or produce a German error.
///
/// Without any argument the setup this looper was built for is assumed: voice on input 1, guitar on
/// input 2, both mono and both centred - reduced to what the device can actually deliver. A stereo
/// track is never guessed: which two inputs belong together is a wiring decision nobody can read
/// off the device.
pub fn resolve_tracks(specs: &[String], in_channels: usize) -> Result<Vec<TrackDef>, String> {
    if in_channels == 0 {
        return Err("Das Geraet meldet keinen einzigen Eingangskanal.".to_string());
    }
    let defs: Vec<TrackDef> = if specs.is_empty() {
        ["stimme", "gitarre"]
            .iter()
            .take(in_channels.min(2))
            .enumerate()
            .map(|(i, name)| TrackDef::mono(name, i))
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
        let highest = def.input.highest();
        if highest >= in_channels {
            return Err(format!(
                "Track \"{}\" soll auf Eingang {} hoeren, das Geraet liefert aber nur {} Eingangskanaele \
                 (also Eingang 1 bis {}).\n\
                 \x20 - anderen Kanal waehlen: --track {}:1\n\
                 \x20 - oder mehr Kanaele oeffnen: --in-channels {}",
                def.name,
                highest + 1,
                in_channels,
                in_channels,
                def.name,
                highest + 1
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

/// Counters both front ends fill from the audio callbacks and print afterwards.
#[derive(Default)]
pub struct LiveStats {
    pub xruns: AtomicU64,
    pub other_errors: AtomicU64,
    /// Output callbacks that found the input FIFO short.
    pub underruns: AtomicU64,
    /// Input callbacks that could not push - frames lost, alignment gone.
    pub overruns: AtomicU64,
    pub cb_nanos_max: AtomicU64,
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
    input: TrackInput,
    gains: Vec<f32>,
    /// This track's compensation as the control thread last set it. Mirrored here - like the
    /// gains - because a tempo change has to be checked against the *largest* one before the
    /// engine sees the command, and the newest status snapshot may be a few milliseconds old.
    latency: TrackLatency,
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
    /// The global default, i.e. what a track without a measured value of its own compensates.
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
            "n" => self.set_pan(parts.next()),
            "b" => self.set_track_send(parts.next(), parts.next()),
            "g" => self.set_bus_gain(parts.next(), parts.next(), last),
            "u" => self.set_bus_out(parts.next(), parts.next()),
            "i" => self.set_latency(parts.next(), parts.next(), ts),
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
                    "Unbekannte Eingabe \"{other}\". Tasten: 1-{} r o s p c a m n b g u i k e w l t f x v d q",
                    self.tracks.len()
                );
            }
        }
    }

    /// `n <wert>` - where this track sits between the speakers, -1 to 1.
    fn set_pan(&mut self, arg: Option<&str>) {
        let usage = "n <wert>  (Panorama, -1 ganz links bis 1 ganz rechts, 0 Mitte)";
        let Some(Ok(pan)) = arg.map(str::parse::<f32>) else {
            self.message = format!("Aufruf: {usage}");
            return;
        };
        if !(-1.0..=1.0).contains(&pan) {
            self.message = "Das Panorama muss zwischen -1 (links) und 1 (rechts) liegen.".to_string();
            return;
        }
        let track = self.active;
        self.send(Command::SetPan { track, pan });
        self.message = format!(
            "\"{}\": Panorama {}.",
            self.tracks[track].name,
            pan_label(pan)
        );
    }

    /// `b <loop|mon> <busse>` - which buses this track's loop or its monitor signal goes to.
    ///
    /// Two sources under one key: they are the same kind of decision and belong next to each other,
    /// and a second letter for the second half would be a letter spent on nothing.
    fn set_track_send(&mut self, source: Option<&str>, value: Option<&str>) {
        let usage =
            "b <loop|mon> <main|monitor|main+monitor|none>  (auf welche Busse dieser Track geht)";
        let source = match source {
            Some("loop") | Some("l") => TrackSource::Loop,
            Some("mon") | Some("monitor") | Some("m") => TrackSource::Monitor,
            _ => {
                self.message = format!("Aufruf: {usage}");
                return;
            }
        };
        let Some(send) = value.and_then(BusSend::parse) else {
            self.message = format!("Aufruf: {usage}");
            return;
        };
        let track = self.active;
        self.send(Command::SetTrackSend {
            track,
            source,
            send,
        });
        self.message = format!(
            "\"{}\": {} -> {}.",
            self.tracks[track].name,
            source.label(),
            send.label()
        );
    }

    /// `g <main|monitor> <wert>` - the volume of one bus, and half the reason there are two of
    /// them: the headphones move and the room stays where it was.
    fn set_bus_gain(
        &mut self,
        bus: Option<&str>,
        value: Option<&str>,
        last: Option<(Status, Instant)>,
    ) {
        let usage = "g <main|monitor> <0.0-4.0>  (Lautstaerke eines Busses)";
        let Some(bus) = bus.and_then(Bus::parse) else {
            self.message = format!("Aufruf: {usage}");
            return;
        };
        let Some(Ok(gain)) = value.map(str::parse::<f32>) else {
            self.message = format!("Aufruf: {usage}");
            return;
        };
        if !(0.0..=MAX_BUS_GAIN).contains(&gain) {
            self.message =
                format!("Die Bus-Lautstaerke muss zwischen 0.0 und {MAX_BUS_GAIN} liegen.");
            return;
        }
        self.send(Command::SetBusGain { bus, gain });
        let other = match bus {
            Bus::Main => Bus::Monitor,
            Bus::Monitor => Bus::Main,
        };
        let untouched = last.map(|(s, _)| s.bus_gain(other)).unwrap_or(1.0);
        self.message = format!(
            "{} auf {gain:.2}. {} bleibt bei {untouched:.2}.",
            bus.label(),
            other.label()
        );
    }

    /// `u <main|monitor> <kanal[-kanal]>` - which socket a bus leaves on.
    fn set_bus_out(&mut self, bus: Option<&str>, value: Option<&str>) {
        let usage = "u <main|monitor> <kanal[-kanal]>  (Ausgangspaar eines Busses, 1-basiert)";
        let (Some(bus), Some(value)) = (bus.and_then(Bus::parse), value) else {
            self.message = format!("Aufruf: {usage}");
            return;
        };
        match parse_bus_out_arg(&format!("{}:{value}", bus.name())) {
            Ok((bus, out)) => {
                self.send(Command::SetBusOutput { bus, out });
                self.message = format!(
                    "{} liegt auf Ausgang {}. Was das Geraet nicht hat, wird auf das naechste \
                     vorhandene Paar gelegt.",
                    bus.label(),
                    out.label()
                );
            }
            Err(e) => self.message = e.replace("--bus-out", "u"),
        }
    }

    /// `i <frames|-> [zuschlag]` - what this track subtracts while recording.
    ///
    /// `-` puts the track back on the global default; leaving the surcharge out keeps whatever it
    /// had, because the two numbers are set by different people at different times - a
    /// measurement writes the first, an ear the second.
    fn set_latency(&mut self, base: Option<&str>, trim: Option<&str>, ts: TrackStatus) {
        let usage = "i <frames|-> [zuschlag]  (Latenzkompensation dieses Tracks; - nimmt die globale Vorgabe)";
        let Some(base) = base else {
            self.message = format!("Aufruf: {usage}");
            return;
        };
        let measured = if base == "-" {
            None
        } else {
            match base.parse::<i64>() {
                Ok(value) if (0..=MAX_LATENCY_FRAMES).contains(&value) => Some(value as u32),
                _ => {
                    self.message = format!(
                        "Die Latenz muss eine ganze Zahl von 0 bis {MAX_LATENCY_FRAMES} Frames \
                         sein, oder \"-\" fuer die globale Vorgabe."
                    );
                    return;
                }
            }
        };
        let trim = match trim {
            None => ts.latency.trim,
            Some(text) => match text.parse::<i64>() {
                Ok(value) if value.abs() <= MAX_LATENCY_FRAMES => value as i32,
                _ => {
                    self.message = format!(
                        "Der Zuschlag muss eine ganze Zahl zwischen -{MAX_LATENCY_FRAMES} und \
                         {MAX_LATENCY_FRAMES} Frames sein."
                    );
                    return;
                }
            },
        };
        let latency = TrackLatency { measured, trim };
        let track = self.active;
        self.tracks[track].latency = latency;
        self.send(Command::SetTrackLatency { track, latency });
        let rate = self.sched.timeline.sample_rate().max(1) as f64;
        let effective = latency.resolve(self.latency);
        self.message = format!(
            "\"{}\": Latenzkompensation {} Frames ({:.2} ms) - {}. Wirkt auf kommende Aufnahmen, \
             nicht auf schon Aufgenommenes.",
            self.tracks[track].name,
            effective,
            effective as f64 * 1000.0 / rate,
            latency_origin(latency, self.latency),
        );
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
        let worst = max_latency(self.latency, self.tracks.iter().map(|t| t.latency));
        if let Err(e) = check_loop(&timeline, self.sched.bars, worst) {
            self.message = e;
            return;
        }
        let layer_frames = loop_capacity(&timeline, self.sched.bars);
        self.send(Command::SetTempo {
            bpm,
            signature,
            layer_frames,
        });
        // Every allocation of a new layer length happens here, in the control thread; the old stock
        // is dropped and the engine hands back what it still holds.
        self.pool.set_layer_frames(layer_frames as usize);
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

/// What a front end needs to build the engine and start both streams.
pub struct EngineSpec<'a> {
    pub setup: &'a audio::DuplexSetup,
    pub timeline: Timeline,
    /// Longest loop the session can record, in bars - the layer buffer length.
    pub bars: u32,
    pub defs: &'a [TrackDef],
    pub latency_frames: u64,
    /// Whether every track starts with monitoring on.
    pub monitor: bool,
    pub monitor_gain: f32,
    pub click: bool,
    pub click_gain: f32,
    /// Volume of each output bus, linear.
    pub bus_gain: [f32; BUS_COUNT],
    /// Which device output pair each bus leaves on. Clamped by the engine against what the device
    /// really has.
    pub routing: BusRouting,
}

/// A running engine: two live cpal streams plus the control thread's ends of every channel.
///
/// Every field is public and the struct is meant to be **destructured**: `cpal::Stream` is `!Send`
/// and dropping one stops it, so the two streams have to stay bound in the control loop's own
/// scope. Taking them out of a struct by pattern is the way to do that without a borrow that
/// outlives the loop.
pub struct RunningEngine {
    pub input: cpal::Stream,
    pub output: cpal::Stream,
    pub cmd: CommandSender,
    pub status: StatusReceiver,
    pub pool: LayerPool,
    pub stats: Arc<LiveStats>,
    /// Buffer size the driver actually granted, which is what the head start is computed from.
    pub buffer_frames: u32,
}

/// German summary of what the callbacks saw, for the end of a session.
pub fn stats_report(stats: &LiveStats) -> String {
    format!(
        "Xruns (cpal):        {}\nFIFO leer:           {}\nFIFO uebergelaufen:  {}\n\
         Sonstige Fehler:     {}\nCallback-Dauer max:  {:.3} ms",
        stats.xruns.load(Ordering::Relaxed),
        stats.underruns.load(Ordering::Relaxed),
        stats.overruns.load(Ordering::Relaxed),
        stats.other_errors.load(Ordering::Relaxed),
        stats.cb_nanos_max.load(Ordering::Relaxed) as f64 / 1e6,
    )
}

/// Build the engine, wire both callbacks and start the streams.
///
/// One place rather than one per subcommand: the two conditions the latency compensation rests on -
/// whole frames through the FIFO and **input stream started before the output stream** - are stated
/// in the module comment and would otherwise have to be re-established every time a front end is
/// added.
pub fn start_engine(spec: EngineSpec) -> Result<RunningEngine, String> {
    let setup = spec.setup;
    let rate = setup.sample_rate();
    let in_channels = setup.in_plan.config.channels as usize;
    let out_channels = setup.out_plan.config.channels as usize;
    let buffer_frames = setup.buffer_frames().max(1);
    // Same reasoning as in audio.rs: room for a driver block far larger than requested, so the
    // callback never has to allocate even if the driver ignores the request.
    let scratch_frames = (buffer_frames * 16) as usize;

    let capacity = loop_capacity(&spec.timeline, spec.bars);
    let kinds: Vec<Channels> = spec.defs.iter().map(TrackDef::channels).collect();
    let slots = spare_slots_for(&kinds, SPARE_SLOTS);

    let stats = Arc::new(LiveStats::default());
    let (mut producer, mut consumer) =
        rtrb::RingBuffer::<f32>::new((buffer_frames * INPUT_FIFO_BUFFERS) as usize * in_channels);
    let (cmd_tx, cmd_rx) = command_channel(256);
    let (status_tx, status_rx) = status_channel(1024);
    // The return queue has room for every buffer that can be inside the engine at once, so the
    // audio thread can always hand one back instead of leaking it.
    let (channel, buffer_endpoint) = buffer_channel(
        slots[0] + slots[1] + 8,
        spec.defs.len() * MAX_LAYERS + slots[0] + slots[1] + 8,
    );
    let mut pool = LayerPool::new(channel, capacity as usize, slots);
    // Every layer buffer of the session is born here, in the control thread, before any stream
    // exists; from now on the pool only recycles.
    pool.service(None);

    let tracks: Vec<Track> = spec
        .defs
        .iter()
        .map(|d| {
            Track::new(d.input, spec.monitor, d.pan, rate)
                .with_latency(d.latency)
                .with_sends(d.loop_send, d.monitor_send)
        })
        .collect();
    let mut core = EngineCore::new(EngineConfig {
        timeline: spec.timeline,
        latency_frames: spec.latency_frames,
        input_channels: in_channels,
        tracks,
        spares: [Vec::with_capacity(slots[0]), Vec::with_capacity(slots[1])],
        layer_frames: capacity,
        commands: cmd_rx,
        status: status_tx,
        buffers: buffer_endpoint,
        monitor_gain: spec.monitor_gain,
        click: spec.click,
        click_gain: spec.click_gain,
        bus_gain: spec.bus_gain,
        routing: spec.routing,
        output_channels: out_channels,
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
    let mut in_scratch = vec![0.0f32; scratch_frames * in_channels];
    let mut out_scratch = vec![0.0f32; scratch_frames * BUS_SAMPLES];
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
                core.process(
                    &in_scratch[..got * in_channels],
                    &mut out_scratch[..n * BUS_SAMPLES],
                );
                // The one place a bus becomes a socket. The routing is read from the engine, which
                // owns it, so a `SetBusOutput` takes effect on the very next block.
                let routing = core.routing();
                let base = done * out_channels;
                for (i, frame) in data[base..base + n * out_channels]
                    .chunks_exact_mut(out_channels)
                    .enumerate()
                {
                    let buses = &out_scratch[i * BUS_SAMPLES..(i + 1) * BUS_SAMPLES];
                    place_buses(buses, frame, &routing);
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

    Ok(RunningEngine {
        input,
        output,
        cmd: cmd_tx,
        status: status_rx,
        pool,
        stats,
        buffer_frames,
    })
}

/// Non-blocking keyboard: a helper thread owns stdin, the main loop reads lines from a channel.
///
/// Line based rather than raw single keys - raw terminal mode would need another dependency, and
/// every action is quantised to a bar boundary anyway, so the extra Enter costs no accuracy.
pub(crate) fn spawn_keyboard() -> Receiver<String> {
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

pub(crate) fn on_off(v: bool) -> &'static str {
    if v { "an " } else { "aus" }
}

/// `Mitte`, `L50`, `R100` - short enough for a track line, unambiguous enough to read at a glance.
pub(crate) fn pan_label(pan: f32) -> String {
    let percent = (pan.abs() * 100.0).round() as i32;
    if percent == 0 {
        "Mitte".to_string()
    } else if pan < 0.0 {
        format!("L{percent}")
    } else {
        format!("R{percent}")
    }
}

/// `Ein 1 mono` for a microphone, `Ein 3+4 stereo` for a pair. Which inputs and how many channels
/// end up in the loop buffer is the one setup fact worth repeating on every line.
pub(crate) fn input_text(input: TrackInput) -> String {
    format!("Ein {:<4} {:<6}", input.label(), input.channels().label())
}

/// Where a track's compensation comes from, as a sentence fragment: `gemessen 512 + 96 Zuschlag`,
/// `Vorgabe 827`. Never just a number - a value that looks like a setting but is inherited is
/// exactly the thing this is here to prevent.
pub(crate) fn latency_origin(latency: TrackLatency, default: u64) -> String {
    let base = match latency.measured {
        Some(frames) => format!("gemessen {frames}"),
        None => format!("Vorgabe {default}"),
    };
    match latency.trim {
        0 => base,
        trim => format!("{base} {} {} Zuschlag", if trim < 0 { '-' } else { '+' }, trim.abs()),
    }
}

/// The latency column of a track line: the effective number, and whether it is the track's own.
pub(crate) fn latency_text(ts: &TrackStatus) -> String {
    let mark = if ts.latency.inherits() { "geerbt" } else { "eigen " };
    match ts.latency.trim {
        0 => format!("{:>5} {mark}", ts.latency_frames),
        trim => format!("{:>5} {mark}{trim:+}", ts.latency_frames),
    }
}

/// `[1 2* 3]`, with `*` for muted and the gain appended where it is not 1.
pub(crate) fn layer_list(ts: &TrackStatus, gains: &[f32]) -> String {
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
pub(crate) fn fx_text(fx: &FxStatus) -> String {
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

/// `Main 1-2 L -12.3 R -12.1 x1.00` - where a bus goes, what leaves on it, and how loud.
pub(crate) fn bus_text(s: &Status, bus: Bus) -> String {
    let peak = s.bus_peak(bus);
    format!(
        "{} {} L {} R {} x{:.2}",
        match bus {
            Bus::Main => "Main",
            Bus::Monitor => "Mon ",
        },
        s.bus_out.get(bus).label(),
        fmt_dbfs(peak[0]),
        fmt_dbfs(peak[1]),
        s.bus_gain(bus),
    )
}

/// `MK/-K` - which buses the loop goes to, and which the monitor signal goes to.
pub(crate) fn send_text(ts: &TrackStatus) -> String {
    format!("{}/{}", ts.loop_send.short(), ts.monitor_send.short())
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
        "Takt {:>4} Schlag {}/{} [{}] | Loop {} Takte = {} Frames = {:.2} s | {:.1} BPM | Raster {} | Klick {} | {} | {}",
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
        // One block per bus, each with both channels: a mix that clips only on one side, or only
        // in the headphones, has to say where.
        bus_text(s, Bus::Main),
        bus_text(s, Bus::Monitor),
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
    // One meter per recorded channel: a stereo source with a dead cable on one side is otherwise
    // invisible until the take is played back.
    let level = match ts.channels {
        2 => format!(
            "L {} R {}",
            fmt_dbfs(ts.input_peak[0]),
            fmt_dbfs(ts.input_peak[1])
        ),
        _ => format!("{}      ", fmt_dbfs(ts.input_peak[0])),
    };
    format!(
        "{} {:<10} {} | {:<34} | Loop {:>10} | Ebenen {:>2} {:<28} | Pegel {} | Pan {:<5} | Bus {:<5} | Lat {:<14} | Mithoeren {} | FX {:<22}",
        if active { '>' } else { ' ' },
        ui.name,
        input_text(ui.input),
        state_text,
        loop_text,
        ts.layers,
        layer_list(ts, &ui.gains),
        level,
        pan_label(ts.pan),
        send_text(ts),
        latency_text(ts),
        on_off(ts.monitor),
        fx_text(&ts.fx),
    )
}

/// Draw the block in place. Without cursor control every update is simply appended.
pub(crate) fn draw(lines: &[String], previous_lines: usize, simple: bool) {
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

    let in_channels = setup.in_plan.config.channels as usize;
    let out_channels = setup.out_plan.config.channels as usize;
    let mut defs = resolve_tracks(&opts.tracks, in_channels)?;
    apply_track_latencies(&mut defs, &opts.track_latencies)?;
    apply_track_sends(&mut defs, TrackSource::Loop, &opts.track_buses)?;
    apply_track_sends(&mut defs, TrackSource::Monitor, &opts.track_monitor_buses)?;
    let routing = resolve_routing(&opts.bus_outs, out_channels)?;
    let bus_gain = resolve_bus_gains(&opts.bus_gains)?;
    // The loop has to be longer than the *largest* compensation any track uses, not than the
    // default: a track whose value is bigger is the one that would write into a loop it has
    // already played past.
    let worst_latency = max_latency(
        opts.latency_frames,
        defs.iter().map(|d| d.latency),
    );
    check_loop(&timeline, opts.bars, worst_latency)?;

    audio::print_setup(&setup);
    let loop_len = timeline.span_bars(0, opts.bars);
    println!(
        "Takt:     {:.1} BPM, {}/{}, {:.3} Frames pro Schlag",
        opts.bpm,
        opts.beats_per_bar,
        opts.beat_unit,
        timeline.samples_per_beat()
    );
    println!(
        "Loop:     {} Takte = {} Frames = {:.2} s",
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
    let kinds: Vec<Channels> = defs.iter().map(TrackDef::channels).collect();
    let slots = spare_slots_for(&kinds, SPARE_SLOTS);
    println!(
        "{}",
        limits_line(
            capacity,
            defs.len(),
            total_channels(&kinds),
            spare_channels(slots)
        )
    );
    println!("Tracks:");
    for (i, def) in defs.iter().enumerate() {
        let effective = def.latency.resolve(opts.latency_frames);
        println!(
            "  {} {:<10} Eingang {} ({}, von {} Kanaelen), Panorama {}, Latenz {} Frames ({:.2} ms, {}), Loop -> {}, Mithoeren -> {}",
            i + 1,
            def.name,
            def.input.label(),
            def.input.channels().label(),
            in_channels,
            pan_label(def.pan),
            effective,
            effective as f64 * 1000.0 / rate as f64,
            latency_origin(def.latency, opts.latency_frames),
            def.loop_send.label(),
            def.monitor_send.label(),
        );
    }
    println!(
        "Busse:    Main -> Ausgang {} (Lautstaerke {:.2}), Monitor -> Ausgang {} (Lautstaerke {:.2}), 
                   von {} Ausgangskanaelen. Der Klick liegt auf Monitor und nie auf Main.",
        routing.get(Bus::Main).label(),
        bus_gain[Bus::Main.index()],
        routing.get(Bus::Monitor).label(),
        bus_gain[Bus::Monitor.index()],
        out_channels
    );
    if let Some(note) = routing_note(&routing, out_channels) {
        println!("ACHTUNG:  {note}");
    }
    println!(
        "Latenz:   Vorgabe {} Frames ({:.2} ms) - sie gilt fuer jeden Track, der nichts eigenes sagt",
        opts.latency_frames,
        opts.latency_frames as f64 * 1000.0 / rate as f64
    );
    println!(
        "          Der Wert stammt aus der Loopback-Messung. Nach jeder Aenderung von Geraet,\n\
         \x20         Samplerate oder Puffergroesse neu messen (Subcommand calibrate). Eine Quelle,\n\
         \x20         die ueber einen Plugin-Host hereinkommt, hat einen eigenen Weg und braucht\n\
         \x20         einen eigenen Wert: calibrate --for-track, dann --track-latency NAME:FRAMES."
    );

    let RunningEngine {
        input,
        output,
        cmd: cmd_tx,
        status: mut status_rx,
        pool,
        stats,
        buffer_frames,
    } = start_engine(EngineSpec {
        setup: &setup,
        timeline,
        bars: opts.bars,
        defs: &defs,
        latency_frames: opts.latency_frames,
        monitor: opts.monitor,
        monitor_gain: opts.monitor_gain,
        click: !opts.no_click,
        click_gain: opts.click_gain,
        bus_gain,
        routing,
    })?;

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
         \x20 n <wert>      Panorama: -1 ganz links, 0 Mitte, 1 ganz rechts\n\
         \x20 i <frames|-> [zuschlag]  Latenzkompensation dieses Tracks; - nimmt die globale Vorgabe.\n\
         \x20               Wirkt auf kommende Aufnahmen, nicht auf schon Aufgenommenes.\n\
         Busse - Main geht in den Saal, Monitor auf den Kopfhoerer, der Klick nur auf Monitor:\n\
         \x20 b <loop|mon> <main|monitor|main+monitor|none>  Bus-Zuordnung dieses Tracks\n\
         \x20 g <main|monitor> <wert>  Lautstaerke eines Busses (der andere bleibt, wie er ist)\n\
         \x20 u <main|monitor> <kanal[-kanal]>  Ausgangspaar eines Busses\n\
         \x20 k   Klick an/aus (er liegt auf dem Monitor-Bus)\n\
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
        latency: opts.latency_frames,
        tracks: defs
            .iter()
            .map(|d| TrackUi {
                name: d.name.clone(),
                input: d.input,
                gains: Vec::new(),
                latency: d.latency,
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
    println!("{}", stats_report(&stats));
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
        assert_eq!(parse_track_arg("stimme:1").unwrap(), TrackDef::mono("stimme", 0));
        assert_eq!(
            parse_track_arg(" gitarre : 2 ").unwrap().input,
            TrackInput::Mono(1)
        );
        for bad in ["stimme", "stimme:", ":1", "stimme:0", "stimme:x"] {
            let err = parse_track_arg(bad).expect_err(bad);
            assert!(err.contains("--track"), "deutsche Meldung fehlt: {err}");
        }
    }

    /// The new half of the argument: a channel pair makes a stereo track, and an optional pan
    /// places it. Both spellings have to survive whitespace, because a shell often adds some.
    #[test]
    fn a_channel_pair_makes_a_stereo_track_and_at_sets_the_pan() {
        let klavier = parse_track_arg("klavier:3-4").unwrap();
        assert_eq!(klavier.input, TrackInput::Stereo { left: 2, right: 3 });
        assert_eq!(klavier.channels(), Channels::Stereo);
        assert_eq!(klavier.pan, 0.0);
        assert_eq!(klavier.input.label(), "3+4");

        let panned = parse_track_arg("stimme:1@-0.5").unwrap();
        assert_eq!(panned.input, TrackInput::Mono(0));
        assert_eq!(panned.pan, -0.5);
        assert_eq!(panned.channels(), Channels::Mono);

        let both = parse_track_arg(" flaeche : 5 - 6 @ 0.25 ").unwrap();
        assert_eq!(both.input, TrackInput::Stereo { left: 4, right: 5 });
        assert_eq!(both.pan, 0.25);

        // Every mistake gets its own German sentence rather than a silently wrong track.
        for (bad, needle) in [
            ("klavier:3-0", "ab 1"),
            ("klavier:3-x", "Kanalnummer"),
            ("klavier:3-3", "zwei verschiedene"),
            ("stimme:1@links", "Panorama-Zahl"),
            ("stimme:1@2", "zwischen -1"),
        ] {
            let err = parse_track_arg(bad).expect_err(bad);
            assert!(err.contains(needle), "\"{bad}\" meldet: {err}");
        }
    }

    #[test]
    fn the_default_setup_is_voice_and_guitar_on_the_first_two_inputs() {
        let two = resolve_tracks(&[], 2).unwrap();
        assert_eq!(two.len(), 2);
        assert_eq!(two[0].name, "stimme");
        assert_eq!(two[0].input, TrackInput::Mono(0));
        assert_eq!(two[1].name, "gitarre");
        assert_eq!(two[1].input, TrackInput::Mono(1));
        // Never guessed: a stereo pairing is a wiring decision, so the default stays mono even on
        // a two-input device.
        assert!(two.iter().all(|t| t.channels() == Channels::Mono));
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

        // The same for the second half of a stereo pair, which is the one a device runs out of.
        let err = resolve_tracks(&specs(&["klavier:2-3"]), 2).expect_err("Kanal 3 gibt es nicht");
        assert!(err.contains("Eingang 3"), "{err}");

        let err = resolve_tracks(&specs(&["a:1", "a:2"]), 4).expect_err("doppelter Name");
        assert!(err.contains("eindeutig"), "{err}");

        let many: Vec<String> = (1..=MAX_TRACKS + 1).map(|i| format!("t{i}:1")).collect();
        assert!(resolve_tracks(&many, 8).is_err(), "Obergrenze greift");
    }

    /// The argument that makes the compensation per track. Every spelling, including the one that
    /// only adds a surcharge to the global default - which is what a source nobody can measure
    /// (a plugin host's own buffer) needs.
    #[test]
    fn a_track_latency_argument_carries_a_measured_value_a_trim_or_both() {
        assert_eq!(
            parse_track_latency_arg("cantabile:512").unwrap(),
            ("cantabile".to_string(), TrackLatency::measured(512))
        );
        assert_eq!(
            parse_track_latency_arg("cantabile:512+96").unwrap(),
            ("cantabile".to_string(), TrackLatency::measured(512).with_trim(96))
        );
        assert_eq!(
            parse_track_latency_arg(" cantabile : 512-40 ").unwrap(),
            ("cantabile".to_string(), TrackLatency::measured(512).with_trim(-40))
        );
        let (name, only_trim) = parse_track_latency_arg("stimme:+96").unwrap();
        assert_eq!(name, "stimme");
        assert!(only_trim.inherits(), "ohne Zahl davor bleibt die Vorgabe stehen");
        assert_eq!(only_trim.trim, 96);
        assert_eq!(only_trim.resolve(827), 923);

        for (bad, needle) in [
            ("cantabile", "unvollstaendig"),
            (":512", "Trackname"),
            ("cantabile:", "fehlt die Zahl"),
            ("cantabile:viel", "Frame-Zahl"),
            ("cantabile:512+viel", "Zuschlagszahl"),
            ("cantabile:999999", "Tippfehler"),
            ("cantabile:+999999", "Tippfehler"),
            // A non-ASCII first character must produce the German message, not a panic on a char
            // boundary.
            ("cantabile:ue512", "Frame-Zahl"),
        ] {
            let err = parse_track_latency_arg(bad).expect_err(bad);
            assert!(err.contains(needle), "\"{bad}\" meldet: {err}");
        }
    }

    /// The arguments are matched to tracks by name, and a name that does not exist is refused:
    /// silently ignoring it would leave the source it was meant for on the wrong number.
    #[test]
    fn track_latencies_are_applied_by_name_and_an_unknown_name_is_refused() {
        let mut defs = resolve_tracks(&specs(&["stimme:1", "cantabile:3-4"]), 4).unwrap();
        assert!(defs.iter().all(|d| d.latency.inherits()), "ohne Angabe: die Vorgabe");

        apply_track_latencies(&mut defs, &specs(&["cantabile:512+96"])).unwrap();
        assert!(defs[0].latency.inherits(), "die Stimme bleibt auf der Vorgabe");
        assert_eq!(defs[0].latency.resolve(827), 827);
        assert_eq!(defs[1].latency.resolve(827), 608);

        let err = apply_track_latencies(&mut defs, &specs(&["klavier:512"])).expect_err("kein Track");
        assert!(err.contains("klavier"), "{err}");
        assert!(err.contains("stimme"), "die Meldung zaehlt auf, was es gibt: {err}");
    }

    /// The loop must be longer than the *worst* compensation, not than the default - otherwise a
    /// track with a bigger value would write into a loop it has already played past.
    #[test]
    fn the_loop_check_uses_the_largest_latency_of_all_tracks() {
        use super::super::process::max_latency;
        let default = 827u64;
        assert_eq!(max_latency(default, []), 827);
        assert_eq!(
            max_latency(default, [TrackLatency::measured(400), TrackLatency::INHERITED]),
            827,
            "die Vorgabe zaehlt mit, auch wenn ein Track darunter liegt"
        );
        assert_eq!(
            max_latency(default, [TrackLatency::measured(400), TrackLatency::measured(2_000)]),
            2_000
        );
        assert_eq!(
            max_latency(default, [TrackLatency::INHERITED.with_trim(500)]),
            1_327
        );
    }

    #[test]
    fn the_latency_column_says_where_the_number_comes_from() {
        let inherited = TrackStatus {
            latency: TrackLatency::INHERITED,
            latency_frames: 827,
            ..Default::default()
        };
        assert_eq!(latency_text(&inherited).trim(), "827 geerbt");
        assert_eq!(latency_origin(inherited.latency, 827), "Vorgabe 827");

        let own = TrackStatus {
            latency: TrackLatency::measured(512).with_trim(96),
            latency_frames: 608,
            ..Default::default()
        };
        assert_eq!(latency_text(&own).trim(), "608 eigen +96");
        assert_eq!(latency_origin(own.latency, 827), "gemessen 512 + 96 Zuschlag");
        assert_eq!(
            latency_origin(TrackLatency::INHERITED.with_trim(-40), 827),
            "Vorgabe 827 - 40 Zuschlag"
        );
    }

    #[test]
    fn the_pan_is_written_as_a_side_and_a_percentage() {
        assert_eq!(pan_label(0.0), "Mitte");
        assert_eq!(pan_label(-1.0), "L100");
        assert_eq!(pan_label(1.0), "R100");
        assert_eq!(pan_label(-0.5), "L50");
        assert_eq!(pan_label(0.25), "R25");
    }

    #[test]
    fn the_input_column_names_the_channels_and_the_channel_count() {
        assert_eq!(input_text(TrackInput::Mono(0)).trim(), "Ein 1    mono");
        assert_eq!(
            input_text(TrackInput::Stereo { left: 2, right: 3 }).trim(),
            "Ein 3+4  stereo"
        );
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

    /// The routing arguments, in every shape - including the single channel that makes "Mix links,
    /// Klick rechts" possible without a mode of its own.
    #[test]
    fn a_bus_out_argument_names_a_pair_or_a_single_channel() {
        assert_eq!(
            parse_bus_out_arg("main:1-2").unwrap(),
            (Bus::Main, BusOutput::pair(0))
        );
        assert_eq!(
            parse_bus_out_arg(" monitor : 3 - 4 ").unwrap(),
            (Bus::Monitor, BusOutput::pair(2))
        );
        assert_eq!(
            parse_bus_out_arg("monitor:2").unwrap(),
            (Bus::Monitor, BusOutput::mono(1))
        );

        for (bad, needle) in [
            ("main", "unvollstaendig"),
            ("buehne:1-2", "kein Bus"),
            ("main:0", "ab 1"),
            ("main:x", "Kanalnummer"),
            // A pair is adjacent; anything else would be a routing nobody could wire.
            ("main:1-3", "benachbarten"),
        ] {
            let err = parse_bus_out_arg(bad).expect_err(bad);
            assert!(err.contains(needle), "\"{bad}\" meldet: {err}");
        }
    }

    /// The default routing follows the device, an explicit one overrides it, and one that names a
    /// socket the device has not got is refused with a way out rather than going silent.
    #[test]
    fn the_routing_follows_the_device_unless_the_command_line_says_otherwise() {
        let four = resolve_routing(&[], 4).unwrap();
        assert_eq!(four.get(Bus::Monitor), BusOutput::pair(2));
        assert!(!four.overlaps());

        let two = resolve_routing(&[], 2).unwrap();
        assert!(two.collapsed(), "zwei Ausgaenge sind ein Weg hinaus");
        let note = routing_note(&two, 2).expect("das wird erklaert");
        assert!(note.contains("Klick"), "{note}");

        let split = resolve_routing(&specs(&["main:1", "monitor:2"]), 2).unwrap();
        assert_eq!(split.get(Bus::Main), BusOutput::mono(0));
        assert_eq!(split.get(Bus::Monitor), BusOutput::mono(1));
        assert!(!split.overlaps(), "Mix links, Klick rechts");
        assert!(routing_note(&split, 2).is_none());

        let err = resolve_routing(&specs(&["monitor:3-4"]), 2).expect_err("Kanal 4 gibt es nicht");
        assert!(err.contains("Ausgangskanaele"), "{err}");
        assert!(err.contains("--out-channels"), "der Ausweg fehlt: {err}");
    }

    #[test]
    fn a_bus_gain_argument_is_a_bus_and_a_number_in_range() {
        assert_eq!(parse_bus_gain_arg("monitor:0.8").unwrap(), (Bus::Monitor, 0.8));
        assert_eq!(
            resolve_bus_gains(&specs(&["monitor:0.5"])).unwrap(),
            [1.0, 0.5],
            "was nicht genannt wird, bleibt auf 1.0"
        );
        for (bad, needle) in [
            ("monitor", "unvollstaendig"),
            ("buehne:1", "kein Bus"),
            ("monitor:laut", "keine Zahl"),
            ("monitor:9", "zwischen"),
        ] {
            let err = parse_bus_gain_arg(bad).expect_err(bad);
            assert!(err.contains(needle), "\"{bad}\" meldet: {err}");
        }
    }

    /// Per-track routing is matched by name, and an unknown name is refused for the same reason
    /// `--track-latency` refuses one: the track it was meant for would silently stay where it was.
    #[test]
    fn track_bus_arguments_are_applied_by_name() {
        let mut defs = resolve_tracks(&specs(&["stimme:1", "klick_gtr:2"]), 2).unwrap();
        assert!(defs.iter().all(|d| d.loop_send == BusSend::BOTH));
        assert!(defs.iter().all(|d| d.monitor_send == BusSend::MONITOR));

        apply_track_sends(&mut defs, TrackSource::Loop, &specs(&["klick_gtr:monitor"])).unwrap();
        apply_track_sends(
            &mut defs,
            TrackSource::Monitor,
            &specs(&["stimme:main+monitor"]),
        )
        .unwrap();
        assert_eq!(defs[0].monitor_send, BusSend::BOTH);
        assert_eq!(defs[0].loop_send, BusSend::BOTH, "unangetastet");
        assert_eq!(defs[1].loop_send, BusSend::MONITOR);
        assert_eq!(defs[1].monitor_send, BusSend::MONITOR, "unangetastet");

        let err = apply_track_sends(&mut defs, TrackSource::Loop, &specs(&["klavier:main"]))
            .expect_err("kein Track");
        assert!(err.contains("klavier"), "{err}");
        assert!(err.contains("stimme"), "die Meldung zaehlt auf, was es gibt: {err}");

        let err = apply_track_sends(&mut defs, TrackSource::Loop, &specs(&["stimme:buehne"]))
            .expect_err("kein Bus");
        assert!(err.contains("main+monitor"), "{err}");
    }

    /// The track line says where a track goes in five characters, because a stage screen has no
    /// room for a sentence per track.
    #[test]
    fn the_bus_column_is_the_loop_and_the_monitor_signal_side_by_side() {
        let ts = TrackStatus {
            loop_send: BusSend::BOTH,
            monitor_send: BusSend::MONITOR,
            ..Default::default()
        };
        assert_eq!(send_text(&ts), "MK/-K");

        let cue = TrackStatus {
            loop_send: BusSend::MONITOR,
            monitor_send: BusSend::NONE,
            ..Default::default()
        };
        assert_eq!(send_text(&cue), "-K/--");
    }
}
