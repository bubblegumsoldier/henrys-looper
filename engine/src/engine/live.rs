//! `live` subcommand: the phase 1 looper on real hardware.
//!
//! Thread layout, following the plan:
//!
//! * **Input callback** - copies channel 1 into a lock-free FIFO and counts. Nothing else.
//! * **Output callback** - takes the FIFO content and drives [`EngineCore`], which owns the whole
//!   musical state. This callback is the engine clock.
//! * **Main thread** - allocates, schedules commands, prints. Never touches engine state directly.
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
//! * The FIFO never drops a sample, so the `k`-th sample the engine consumes is the input stream's
//!   own frame number `k`, no matter how long it sat in the FIFO. `in_index` is that number.
//! * Phase 0 measured the roundtrip between exactly these two counters: what the output writes at
//!   frame `P` is seen by the input at frame `P + R`, R = 827 at 128 frames / 48 kHz.
//!
//! So the compensation `m = k - R` works on stream frame numbers, not on arrival times, and the
//! FIFO transit cancels out. It only decides *when* the engine gets to write a sample, not *where*
//! - which is why a FIFO underrun (fewer samples available than the block needs) costs nothing but
//! a little delay: the engine simply consumes fewer samples this round and picks them up next time,
//! and `in_index` still counts input frames.
//!
//! Two conditions this rests on, both of them cheap to keep:
//!
//! 1. **Nothing may be dropped from the FIFO.** An overrun loses input samples and shifts every
//!    later recording permanently, so overruns are counted and shown as a warning. The FIFO holds
//!    64 buffers and is drained on every output callback, so it cannot fill in normal operation.
//! 2. **The streams are started in the same order as in the phase 0 measurement**, input first,
//!    then output, so that both frame counters start on the same driver callback and share the
//!    origin the 827 samples were measured against.
//!
//! `--latency-samples` still deserves one check against the real path: record the click through a
//! loopback cable and look at where it lands. That is the honest way to confirm the number for a
//! different device, sample rate or buffer size.

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
    BufferChannel, Command, CommandSender, Status, buffer_channel, command_channel, status_channel,
};
use super::process::{EngineConfig, EngineCore, check_loop, loop_capacity};
use super::timeline::{TimeSignature, Timeline};
use super::track::TrackState;

/// Input FIFO size in units of the audio buffer size. Generous on purpose: it costs 32 kB and it
/// is the difference between a hiccup and a permanently misaligned recording.
const INPUT_FIFO_BUFFERS: u32 = 64;
/// Status snapshots per second the audio thread produces. The display consumes 10 of them.
const STATUS_HZ: u32 = 200;
/// Terminal refresh rate, as required: no more than ten updates per second.
const DISPLAY_INTERVAL: Duration = Duration::from_millis(100);

#[derive(Args, Debug, Clone)]
pub struct LiveOpts {
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

    /// Mit eingeschaltetem Mithoeren starten
    #[arg(long)]
    pub monitor: bool,

    /// Ohne Klick starten
    #[arg(long)]
    pub no_click: bool,
}

#[derive(Default)]
struct LiveStats {
    xruns: AtomicU64,
    other_errors: AtomicU64,
    /// Output callbacks that found the input FIFO short.
    underruns: AtomicU64,
    /// Input callbacks that could not push - samples lost, alignment gone.
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

/// Everything the main thread needs to schedule commands.
struct Control {
    cmd: CommandSender,
    buffers: BufferChannel,
    timeline: Timeline,
    bars: u32,
    latency: u64,
    /// Head start every scheduled command gets, so it can never arrive after its own time.
    guard: u64,
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

    /// Next bar boundary that is far enough ahead for the audio thread to still see the command.
    fn next_bar(&self, est: u64) -> u64 {
        self.timeline.bar_start_at_or_after(est + self.guard)
    }

    fn handle(&mut self, line: &str, last: Option<(Status, Instant)>) {
        let lowered = line.trim().to_lowercase();
        let mut parts = lowered.split_whitespace();
        let key = parts.next().unwrap_or("");
        let est = self.estimated_pos(last);
        let state = last.map(|(s, _)| s.track).unwrap_or_default();

        match key {
            "" => {}
            "r" => {
                let start = self.next_bar(est);
                let bar = self.timeline.bar_index_at(start);
                let end = self.timeline.position_of(bar + self.bars as u64, 0);
                self.send(Command::StartRecord { at: start });
                self.send(Command::StopRecord { at: end });
                self.send(Command::StartPlay { at: end });
                self.message = format!(
                    "Aufnahme ab Takt {} ueber {} Takte, danach laeuft der Loop.",
                    bar + 1,
                    self.bars
                );
            }
            "s" => match state {
                TrackState::Armed => {
                    self.send(Command::ClearTrack { at: est + self.guard });
                    self.message = "Geplante Aufnahme abgebrochen.".to_string();
                }
                TrackState::Recording => {
                    let end = self.next_bar(est);
                    self.send(Command::StopRecord { at: end });
                    self.send(Command::StartPlay { at: end });
                    let bar = self.timeline.bar_index_at(end);
                    self.message = format!("Aufnahme endet mit Takt {}.", bar);
                }
                _ => {
                    self.send(Command::StopPlay { at: est + self.guard });
                    self.message = "Wiedergabe gestoppt.".to_string();
                }
            },
            "p" => {
                let at = self.next_bar(est);
                self.send(Command::StartPlay { at });
                self.message = format!(
                    "Wiedergabe ab Takt {}.",
                    self.timeline.bar_index_at(at) + 1
                );
            }
            "c" => {
                self.send(Command::ClearTrack { at: est + self.guard });
                self.message = "Loop geleert.".to_string();
            }
            "m" => {
                let on = !last.map(|(s, _)| s.monitor).unwrap_or(false);
                self.send(Command::SetMonitor { on });
                self.message = format!("Mithoeren {}.", if on { "an" } else { "aus" });
            }
            "k" => {
                let on = !last.map(|(s, _)| s.click).unwrap_or(true);
                self.send(Command::SetClick { on });
                self.message = format!("Klick {}.", if on { "an" } else { "aus" });
            }
            "t" => self.set_tempo(parts.next(), parts.next(), parts.next(), state),
            "q" => {
                self.send(Command::Stop);
                self.running = false;
                self.message = "Beende.".to_string();
            }
            other => {
                self.message = format!("Unbekannte Eingabe \"{other}\". Tasten: r s p c m k t q");
            }
        }
    }

    /// `t <bpm> [schlaege_pro_takt] [zaehlzeit]` - only while nothing is recorded, because a new
    /// tempo redefines what every sample position means.
    fn set_tempo(
        &mut self,
        bpm: Option<&str>,
        beats: Option<&str>,
        unit: Option<&str>,
        state: TrackState,
    ) {
        if state != TrackState::Empty {
            self.message =
                "Tempo laesst sich nur bei leerem Loop aendern (erst mit c leeren).".to_string();
            return;
        }
        let Some(Ok(bpm)) = bpm.map(str::parse::<f64>) else {
            self.message = "Aufruf: t <bpm> [schlaege_pro_takt] [zaehlzeit]".to_string();
            return;
        };
        let old = self.timeline.signature();
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
        let rate = self.timeline.sample_rate();
        if let Err(e) = Timeline::validate(rate, bpm, signature) {
            self.message = e;
            return;
        }
        let timeline = Timeline::new(rate, bpm, signature);
        if let Err(e) = check_loop(&timeline, self.bars, self.latency) {
            self.message = e;
            return;
        }
        // Allocation happens here, in the control thread, and the finished buffer is handed over.
        let buffer = vec![0.0f32; loop_capacity(&timeline, self.bars) as usize];
        if let Err(e) = self.buffers.install(buffer) {
            self.message = e;
            return;
        }
        self.send(Command::SetTempo { bpm, signature });
        self.timeline = timeline;
        self.message = format!("Tempo {bpm} BPM, {beats_per_bar}/{beat_unit}.");
    }

    /// Where the audio thread is right now, as well as the main thread can know: the position of
    /// the newest status snapshot plus the time that has passed since it was received. Both
    /// streams run on the interface clock, so the extrapolation is good to a few samples - and the
    /// remaining error is absorbed by `guard` and by quantising to a bar boundary.
    fn estimated_pos(&self, last: Option<(Status, Instant)>) -> u64 {
        match last {
            Some((s, seen)) => {
                let rate = self.timeline.sample_rate() as f64;
                s.pos + (seen.elapsed().as_secs_f64() * rate) as u64
            }
            None => 0,
        }
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

fn status_line(s: &Status, tl: &Timeline, stats: &LiveStats) -> String {
    let beats_per_bar = tl.signature().beats_per_bar;
    let progress = if s.samples_per_beat > 0.0 {
        (s.beat_offset as f64 / s.samples_per_beat * 8.0) as usize
    } else {
        0
    };
    let bar_gfx: String = (0..8)
        .map(|i| if i <= progress { '#' } else { '.' })
        .collect();

    let loop_text = if s.loop_len > 0 {
        format!(
            "{} Samples / {:.2} s",
            s.loop_len,
            tl.samples_to_secs(s.loop_len)
        )
    } else if s.track == TrackState::Recording {
        format!("laeuft, {:.2} s", tl.samples_to_secs(s.filled))
    } else {
        "-".to_string()
    };

    let mut line = format!(
        "Takt {:>4} Schlag {}/{} [{}] | {:<10} | Loop {:<24} | Ein {} | Aus {} | Mithoeren {} | Klick {} | {:.1} BPM",
        s.bar + 1,
        s.beat + 1,
        beats_per_bar,
        bar_gfx,
        s.track.label(),
        loop_text,
        fmt_dbfs(s.input_peak),
        fmt_dbfs(s.output_peak),
        on_off(s.monitor),
        on_off(s.click),
        s.bpm,
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
    if s.ignored_commands > 0 {
        line.push_str(&format!(" | {} Kommandos abgelehnt", s.ignored_commands));
    }
    line
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
        "Latenz:   {} Samples ({:.2} ms) werden beim Aufnehmen herausgerechnet",
        opts.latency_samples,
        opts.latency_samples as f64 * 1000.0 / rate as f64
    );
    println!(
        "          Der Wert stammt aus der Loopback-Messung. Nach jeder Aenderung von Geraet,\n\
         \x20         Samplerate oder Puffergroesse neu messen (Subcommand latency)."
    );

    let in_channels = setup.in_plan.config.channels as usize;
    let out_channels = setup.out_plan.config.channels as usize;
    let buffer_frames = setup.buffer_frames().max(1);
    // Same reasoning as in audio.rs: room for a driver block far larger than requested, so the
    // callback never has to allocate even if the driver ignores the request.
    let scratch_frames = (buffer_frames * 16) as usize;

    let stats = Arc::new(LiveStats::default());
    let (mut producer, mut consumer) =
        rtrb::RingBuffer::<f32>::new((buffer_frames * INPUT_FIFO_BUFFERS) as usize);
    let (cmd_tx, cmd_rx) = command_channel(256);
    let (status_tx, mut status_rx) = status_channel(1024);
    let (buffers, buffer_endpoint) = buffer_channel(4);

    // The one and only loop-buffer allocation of a normal session, done here in the control
    // thread before any stream exists.
    let core = EngineCore::new(EngineConfig {
        timeline,
        latency_samples: opts.latency_samples,
        buffer: vec![0.0f32; loop_capacity(&timeline, opts.bars) as usize],
        commands: cmd_rx,
        status: status_tx,
        buffers: buffer_endpoint,
        monitor: opts.monitor,
        monitor_gain: opts.monitor_gain,
        click: !opts.no_click,
        click_gain: opts.click_gain,
        status_interval: (rate / STATUS_HZ).max(1) as u64,
    });

    // ---- input stream: copy channel 1 into the FIFO, nothing else ------------------------
    let stats_in = Arc::clone(&stats);
    let stats_in_err = Arc::clone(&stats);
    let input = audio::build_input(
        &setup.input,
        &setup.in_plan,
        move |data: &[f32], _: &InputCallbackInfo, _offset: usize| {
            let t0 = Instant::now();
            let mut overrun = false;
            for frame in data.chunks_exact(in_channels) {
                if producer.push(frame[0]).is_err() {
                    overrun = true;
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
    let mut in_scratch = vec![0.0f32; scratch_frames];
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
                while got < n {
                    match consumer.pop() {
                        Ok(v) => {
                            in_scratch[got] = v;
                            got += 1;
                        }
                        Err(_) => break,
                    }
                }
                if got < n {
                    stats_out.underruns.fetch_add(1, Ordering::Relaxed);
                }
                core.process(&in_scratch[..got], &mut out_scratch[..n]);
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

    println!(
        "\nTasten (jeweils mit Enter bestaetigen):\n\
         \x20 r  Aufnahme ab der naechsten Taktgrenze, {} Takte, danach laeuft der Loop\n\
         \x20 s  Stopp: laufende Aufnahme auf der naechsten Taktgrenze beenden, sonst Wiedergabe aus\n\
         \x20 p  Wiedergabe ab der naechsten Taktgrenze\n\
         \x20 c  Loop leeren\n\
         \x20 m  Mithoeren an/aus\n\
         \x20 k  Klick an/aus\n\
         \x20 t  Tempo aendern, z.B. \"t 120\" oder \"t 120 7 8\" (nur bei leerem Loop)\n\
         \x20 q  Beenden\n",
        opts.bars
    );

    let keys = spawn_keyboard();
    let mut control = Control {
        cmd: cmd_tx,
        buffers,
        timeline,
        bars: opts.bars,
        latency: opts.latency_samples,
        // Four buffers plus 20 ms: comfortably more than one round through the callbacks, so a
        // scheduled command always reaches the audio thread before its own timestamp.
        guard: (buffer_frames * 4 + rate / 50) as u64,
        message: String::new(),
        running: true,
    };
    let mut last: Option<(Status, Instant)> = None;
    let mut last_print = Instant::now() - DISPLAY_INTERVAL;

    while control.running {
        if let Some(s) = status_rx.latest() {
            if s.stopped {
                control.running = false;
            }
            last = Some((s, Instant::now()));
        }
        control.buffers.drain_retired();

        match keys.recv_timeout(Duration::from_millis(20)) {
            Ok(line) => control.handle(&line, last),
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => break,
        }

        if last_print.elapsed() >= DISPLAY_INTERVAL {
            last_print = Instant::now();
            if let Some((s, _)) = last {
                let mut line = status_line(&s, &control.timeline, &stats);
                if !control.message.is_empty() {
                    line.push_str(" | ");
                    line.push_str(&control.message);
                }
                // Pad so the remains of a longer previous line are erased.
                print!("\r{line:<200}");
                let _ = std::io::stdout().flush();
            }
        }
    }

    drop(input);
    drop(output);
    control.buffers.drain_retired();

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
