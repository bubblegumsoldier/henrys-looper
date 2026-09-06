//! Full-duplex monitoring: input callback -> lock-free ring buffer -> output callback.
//!
//! Used by `thru` and (with the metronome mixed in) by `soak`.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use cpal::traits::StreamTrait;
use cpal::{ErrorKind, InputCallbackInfo, OutputCallbackInfo, Stream};

use crate::audio::{self, DeviceOpts, DuplexSetup};
use crate::click::{ClickGen, ClickSpec};
use crate::meter::{PeakMeter, fmt_dbfs};

/// Ring buffer size in units of the audio buffer size.
///
/// Every sample sitting in the ring is added latency, so this is deliberately tight: two buffers
/// would leave no slack for the input/output callbacks drifting against each other, four gives
/// one buffer of headroom on each side. At 128 frames / 48 kHz that is 4 * 2.67 ms = 10.7 ms of
/// worst-case ring latency, and typically far less because the ring runs near empty.
const RING_BUFFERS: u32 = 4;

#[derive(Default)]
pub struct DuplexStats {
    /// Xruns reported by cpal itself (`ErrorKind::Xrun`).
    pub xruns: AtomicU64,
    /// Any other error delivered to a stream error callback.
    pub other_errors: AtomicU64,
    /// Output callbacks that found the ring empty (counted once per callback).
    pub underruns: AtomicU64,
    /// Input callbacks that found the ring full (counted once per callback).
    pub overruns: AtomicU64,
    pub in_callbacks: AtomicU64,
    pub out_callbacks: AtomicU64,
    pub cb_nanos_total: AtomicU64,
    pub cb_nanos_max: AtomicU64,
    pub input_peak: PeakMeter,
}

impl DuplexStats {
    fn record_callback(&self, started: Instant, is_input: bool) {
        let nanos = started.elapsed().as_nanos() as u64;
        self.cb_nanos_total.fetch_add(nanos, Ordering::Relaxed);
        self.cb_nanos_max.fetch_max(nanos, Ordering::Relaxed);
        if is_input {
            self.in_callbacks.fetch_add(1, Ordering::Relaxed);
        } else {
            self.out_callbacks.fetch_add(1, Ordering::Relaxed);
        }
    }

    pub fn callbacks(&self) -> u64 {
        self.in_callbacks.load(Ordering::Relaxed) + self.out_callbacks.load(Ordering::Relaxed)
    }

    pub fn max_callback_ms(&self) -> f64 {
        self.cb_nanos_max.load(Ordering::Relaxed) as f64 / 1e6
    }

    pub fn avg_callback_ms(&self) -> f64 {
        let n = self.callbacks();
        if n == 0 {
            0.0
        } else {
            self.cb_nanos_total.load(Ordering::Relaxed) as f64 / n as f64 / 1e6
        }
    }
}

/// Keeps both streams alive; dropping this stops the audio.
pub struct DuplexRunner {
    _input: Stream,
    _output: Stream,
    pub stats: Arc<DuplexStats>,
    pub ring_capacity: usize,
}

/// Build and start the duplex path.
///
/// The mono monitoring path taps input channel 1 (index 0) and copies it to every output channel.
/// That keeps the level meter, the loopback measurement and what you hear on the same signal.
pub fn start_duplex(
    setup: &DuplexSetup,
    gain: f32,
    click: Option<ClickSpec>,
) -> Result<DuplexRunner, String> {
    let rate = setup.sample_rate();
    let in_channels = setup.in_plan.config.channels as usize;
    let out_channels = setup.out_plan.config.channels as usize;
    let ring_capacity = (setup.buffer_frames().max(1) * RING_BUFFERS) as usize;

    let stats = Arc::new(DuplexStats::default());
    let (mut producer, mut consumer) = rtrb::RingBuffer::<f32>::new(ring_capacity);

    let stats_in = Arc::clone(&stats);
    let stats_in_err = Arc::clone(&stats);
    let input = audio::build_input(
        &setup.input,
        &setup.in_plan,
        move |data: &[f32], _: &InputCallbackInfo, _offset: usize| {
            let t0 = Instant::now();
            let mut peak = 0.0f32;
            let mut overrun = false;
            for frame in data.chunks_exact(in_channels) {
                let s = frame[0];
                let a = s.abs();
                if a > peak {
                    peak = a;
                }
                if producer.push(s).is_err() {
                    overrun = true;
                }
            }
            stats_in.input_peak.push(peak);
            if overrun {
                stats_in.overruns.fetch_add(1, Ordering::Relaxed);
            }
            stats_in.record_callback(t0, true);
        },
        move |err| count_error(&stats_in_err, err),
    )?;

    let mut click_gen = click.map(|spec| ClickGen::new(spec, rate));
    let stats_out = Arc::clone(&stats);
    let stats_out_err = Arc::clone(&stats);
    let output = audio::build_output(
        &setup.output,
        &setup.out_plan,
        move |data: &mut [f32], _: &OutputCallbackInfo, _offset: usize| {
            let t0 = Instant::now();
            let mut underrun = false;
            for frame in data.chunks_exact_mut(out_channels) {
                let monitored = match consumer.pop() {
                    Ok(v) => v * gain,
                    Err(_) => {
                        underrun = true;
                        0.0
                    }
                };
                let clicked = match click_gen.as_mut() {
                    Some(g) => g.next_sample(),
                    None => 0.0,
                };
                let v = (monitored + clicked).clamp(-1.0, 1.0);
                for sample in frame.iter_mut() {
                    *sample = v;
                }
            }
            if underrun {
                stats_out.underruns.fetch_add(1, Ordering::Relaxed);
            }
            stats_out.record_callback(t0, false);
        },
        move |err| count_error(&stats_out_err, err),
    )?;

    if let Ok(frames) = input.buffer_size() {
        println!("Tatsaechliche Puffergroesse Eingang laut Treiber: {frames} Frames");
    }
    if let Ok(frames) = output.buffer_size() {
        println!("Tatsaechliche Puffergroesse Ausgang laut Treiber: {frames} Frames");
    }

    input
        .play()
        .map_err(|e| format!("Eingangsstream startet nicht: {e}"))?;
    output
        .play()
        .map_err(|e| format!("Ausgabestream startet nicht: {e}"))?;

    Ok(DuplexRunner {
        _input: input,
        _output: output,
        stats,
        ring_capacity,
    })
}

fn count_error(stats: &Arc<DuplexStats>, err: cpal::Error) {
    if err.kind() == ErrorKind::Xrun {
        stats.xruns.fetch_add(1, Ordering::Relaxed);
    } else {
        stats.other_errors.fetch_add(1, Ordering::Relaxed);
    }
}

pub fn cmd_thru(dev: &DeviceOpts, gain: f32) -> Result<(), String> {
    let setup = audio::open_duplex(dev)?;
    audio::print_setup(&setup);
    let runner = start_duplex(&setup, gain, None)?;
    println!(
        "Ringpuffer: {} Samples ({} x Puffergroesse, max. {:.2} ms Zusatzlatenz)",
        runner.ring_capacity,
        RING_BUFFERS,
        runner.ring_capacity as f64 * 1000.0 / setup.sample_rate() as f64
    );
    println!("Verstaerkung: {gain:.2}");
    println!("\nDurchhoeren laeuft. Enter beendet.\n");

    let enter = crate::wait_for_enter();
    let stats = Arc::clone(&runner.stats);
    loop {
        if enter.recv_timeout(Duration::from_millis(100)).is_ok() {
            break;
        }
        let peak = stats.input_peak.take();
        let xruns = stats.xruns.load(Ordering::Relaxed)
            + stats.underruns.load(Ordering::Relaxed)
            + stats.overruns.load(Ordering::Relaxed);
        print!(
            "\rEingang: {} | Xruns gesamt: {:<6} (cpal {}, Ring leer {}, Ring voll {})   ",
            fmt_dbfs(peak),
            xruns,
            stats.xruns.load(Ordering::Relaxed),
            stats.underruns.load(Ordering::Relaxed),
            stats.overruns.load(Ordering::Relaxed)
        );
        use std::io::Write;
        let _ = std::io::stdout().flush();
    }

    println!("\n");
    print_summary(&runner.stats);
    Ok(())
}

pub fn print_summary(stats: &DuplexStats) {
    println!("Xruns (cpal):        {}", stats.xruns.load(Ordering::Relaxed));
    println!(
        "Ring-Underruns:      {}",
        stats.underruns.load(Ordering::Relaxed)
    );
    println!(
        "Ring-Overruns:       {}",
        stats.overruns.load(Ordering::Relaxed)
    );
    println!(
        "Sonstige Fehler:     {}",
        stats.other_errors.load(Ordering::Relaxed)
    );
    println!(
        "Callback-Aufrufe:    {} (Eingang {}, Ausgang {})",
        stats.callbacks(),
        stats.in_callbacks.load(Ordering::Relaxed),
        stats.out_callbacks.load(Ordering::Relaxed)
    );
    println!(
        "Callback-Dauer:      max {:.3} ms, Mittel {:.3} ms",
        stats.max_callback_ms(),
        stats.avg_callback_ms()
    );
}
