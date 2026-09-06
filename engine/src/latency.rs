//! Roundtrip latency measurement via a loopback cable (output -> input).
//!
//! Two independent measurements are taken for every run and both are always reported:
//!
//! 1. Timestamp method: cpal hands the output callback `timestamp().playback` (when the buffer
//!    will reach the DAC) and the input callback `timestamp().capture` (when the buffer was read
//!    from the ADC). On WASAPI both come from `QueryPerformanceCounter`, so they share a clock.
//!    A `StreamInstant` cannot be stored in an atomic directly, so the callbacks split it into
//!    the whole seconds and sub-second nanoseconds that `StreamInstant::new` takes back; the
//!    difference itself is always formed with `StreamInstant::checked_duration_since`. When that
//!    returns `None` the capture instant lies before the playback instant, which cannot describe
//!    a roundtrip - the run is reported as invalid for method 1 rather than saturated to zero.
//! 2. Frame-counter method: free-running frame counters in both callbacks, difference of the
//!    absolute frame indices. Both counters start at zero on their stream's first callback, so
//!    this number carries an unknown constant offset (the two streams do not start at the same
//!    instant). Its run-to-run spread is meaningful, its absolute value only in comparison with
//!    method 1 - and a large gap between the two is itself a result worth seeing.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::time::{Duration, Instant};

use cpal::traits::StreamTrait;
use cpal::{ErrorKind, InputCallbackInfo, OutputCallbackInfo, StreamInstant};

use crate::audio::{self, DeviceOpts};
use crate::meter::{dbfs, fmt_dbfs, print_stats};

/// Steep edge: 2 samples up to 0.9, 2 samples back to zero. The steeper the edge, the less the
/// detection threshold shifts the measured onset.
const PULSE: [f32; 4] = [0.45, 0.9, 0.45, 0.0];
/// Noise floor above this level means the loopback is way too hot or feeding back.
const MAX_NOISE_PEAK: f32 = 0.1; // -20 dBFS
const MIN_THRESHOLD: f32 = 0.02;
const MAX_THRESHOLD: f32 = 0.5;
const NOISE_SECONDS: f64 = 0.5;
/// Gap between runs so reflections and the tail of the previous pulse have died away.
const GAP: Duration = Duration::from_millis(200);
/// Give up on a run after this long without a detection.
const RUN_TIMEOUT: Duration = Duration::from_millis(1000);
/// Fixed-point scale for accumulating a sum of squares in an atomic integer.
const SQ_SCALE: f64 = 1_048_576.0; // 2^20

#[derive(Default)]
struct Shared {
    measure_noise: AtomicBool,
    noise_peak: AtomicU32,
    noise_sq_sum: AtomicU64,
    noise_frames: AtomicU64,
    threshold: AtomicU32,
    /// Main thread arms a run; the output callback consumes the flag and emits the pulse.
    armed: AtomicBool,
    pulse_emitted: AtomicBool,
    pulse_frame: AtomicU64,
    /// `timestamp().playback` of the buffer carrying the pulse, split into the two halves of a
    /// `StreamInstant` so nothing is lost on the way through the atomics.
    pulse_secs: AtomicU64,
    pulse_subsec_nanos: AtomicU32,
    /// Frame offset of the pulse inside that driver buffer.
    pulse_offset: AtomicU64,
    detected: AtomicBool,
    detect_frame: AtomicU64,
    /// `timestamp().capture` of the buffer the pulse was found in, split the same way.
    detect_secs: AtomicU64,
    detect_subsec_nanos: AtomicU32,
    /// Frame offset of the detected sample inside that driver buffer.
    detect_offset: AtomicU64,
    xruns: AtomicU64,
    errors: AtomicU64,
}

/// Split a `StreamInstant` into whole seconds and sub-second nanoseconds.
///
/// cpal exposes no field getters, but `as_nanos()` is exact (`secs * 1e9 + nanos`, both fields
/// unsigned), so the division recovers the original `secs` without loss and the remainder is the
/// original sub-second part. `StreamInstant::new` puts them back together. Callback-safe: pure
/// integer arithmetic, no allocation, no lock.
#[inline]
fn split_instant(t: StreamInstant) -> (u64, u32) {
    let total = t.as_nanos();
    ((total / 1_000_000_000) as u64, (total % 1_000_000_000) as u32)
}

/// Rebuild an instant stored by a callback and shift it by the sample offset inside its buffer.
///
/// Returns `None` if the shifted instant is not representable, which keeps every step of the
/// timestamp path inside cpal's own checked API.
fn shifted_instant(
    secs: u64,
    subsec_nanos: u32,
    offset_frames: u64,
    rate: u32,
) -> Option<StreamInstant> {
    let offset = Duration::from_nanos(offset_frames * 1_000_000_000 / rate as u64);
    StreamInstant::new(secs, subsec_nanos).checked_add(offset)
}

pub fn cmd_latency(dev: &DeviceOpts, runs: u32) -> Result<(), String> {
    if runs == 0 {
        return Err("--runs muss mindestens 1 sein.".to_string());
    }
    let setup = audio::open_duplex(dev)?;
    audio::print_setup(&setup);
    println!("Laeufe:   {runs}");
    println!(
        "\nAufbau: Kabel von Ausgang 1 zurueck in Eingang 1. Monitor-/Direct-Monitor am Interface aus.\n"
    );

    let rate = setup.sample_rate();
    let rate_f = rate as f64;
    let in_channels = setup.in_plan.config.channels as usize;
    let out_channels = setup.out_plan.config.channels as usize;

    let shared = Arc::new(Shared::default());
    shared.measure_noise.store(true, Ordering::Relaxed);
    shared
        .threshold
        .store(MAX_THRESHOLD.to_bits(), Ordering::Relaxed);

    // ---- input stream -------------------------------------------------------------------
    let sh = Arc::clone(&shared);
    let sh_err = Arc::clone(&shared);
    let mut in_frame: u64 = 0;
    let input = audio::build_input(
        &setup.input,
        &setup.in_plan,
        move |data: &[f32], info: &InputCallbackInfo, chunk_offset: usize| {
            let frames = data.len() / in_channels;
            if sh.measure_noise.load(Ordering::Relaxed) {
                let mut peak = 0.0f32;
                let mut sq = 0.0f64;
                for frame in data.chunks_exact(in_channels) {
                    let s = frame[0];
                    let a = s.abs();
                    if a > peak {
                        peak = a;
                    }
                    sq += (s as f64) * (s as f64);
                }
                sh.noise_peak.fetch_max(peak.to_bits(), Ordering::Relaxed);
                sh.noise_sq_sum
                    .fetch_add((sq * SQ_SCALE) as u64, Ordering::Relaxed);
                sh.noise_frames.fetch_add(frames as u64, Ordering::Relaxed);
            }
            // Only look for the pulse after the output callback has actually written it;
            // that rules out triggering on noise that happened before the run started.
            if sh.pulse_emitted.load(Ordering::Acquire) && !sh.detected.load(Ordering::Relaxed) {
                let threshold = f32::from_bits(sh.threshold.load(Ordering::Relaxed));
                let (capture_secs, capture_nanos) = split_instant(info.timestamp().capture);
                for (i, frame) in data.chunks_exact(in_channels).enumerate() {
                    if frame[0].abs() > threshold {
                        // `capture` refers to the start of the driver callback, so the offset
                        // counts from there, not from the start of this conversion pass. The
                        // main thread turns it into a `Duration` and adds it to the instant.
                        sh.detect_frame.store(in_frame + i as u64, Ordering::Relaxed);
                        sh.detect_secs.store(capture_secs, Ordering::Relaxed);
                        sh.detect_subsec_nanos
                            .store(capture_nanos, Ordering::Relaxed);
                        sh.detect_offset
                            .store((chunk_offset + i) as u64, Ordering::Relaxed);
                        sh.detected.store(true, Ordering::Release);
                        break;
                    }
                }
            }
            in_frame += frames as u64;
        },
        move |err| count_error(&sh_err, err),
    )?;

    // ---- output stream ------------------------------------------------------------------
    let sh = Arc::clone(&shared);
    let sh_err = Arc::clone(&shared);
    let mut out_frame: u64 = 0;
    let output = audio::build_output(
        &setup.output,
        &setup.out_plan,
        move |data: &mut [f32], info: &OutputCallbackInfo, chunk_offset: usize| {
            let frames = data.len() / out_channels;
            for sample in data.iter_mut() {
                *sample = 0.0;
            }
            if sh.armed.load(Ordering::Relaxed) && frames >= PULSE.len() {
                sh.armed.store(false, Ordering::Relaxed);
                // The pulse starts at the first frame of this buffer, on every output channel,
                // so either side of a stereo loopback cable works.
                for (i, v) in PULSE.iter().enumerate() {
                    let base = i * out_channels;
                    for c in 0..out_channels {
                        data[base + c] = *v;
                    }
                }
                // `playback` refers to the start of the driver callback; the pulse sits
                // `chunk_offset` frames later. Note the measured onset is the first pulse
                // sample while detection triggers on the first sample above the threshold,
                // which leaves a residual bias of at most two samples.
                let (playback_secs, playback_nanos) = split_instant(info.timestamp().playback);
                sh.pulse_frame.store(out_frame, Ordering::Relaxed);
                sh.pulse_secs.store(playback_secs, Ordering::Relaxed);
                sh.pulse_subsec_nanos
                    .store(playback_nanos, Ordering::Relaxed);
                sh.pulse_offset.store(chunk_offset as u64, Ordering::Relaxed);
                sh.pulse_emitted.store(true, Ordering::Release);
            }
            // Buffer too short for the pulse: stay armed and try again next callback.
            out_frame += frames as u64;
        },
        move |err| count_error(&sh_err, err),
    )?;

    // What the driver actually settled on, per direction. Read before `play()` like the other
    // subcommands do, printed further down so it sits right in front of the runs - a measurement
    // series only means something together with the buffer size it was taken at.
    let in_buffer = input.buffer_size().ok();
    let out_buffer = output.buffer_size().ok();

    input
        .play()
        .map_err(|e| format!("Eingangsstream startet nicht: {e}"))?;
    output
        .play()
        .map_err(|e| format!("Ausgabestream startet nicht: {e}"))?;

    // ---- a) noise floor -----------------------------------------------------------------
    println!("Messe Grundrauschen ({NOISE_SECONDS:.1} s) ...");
    std::thread::sleep(Duration::from_secs_f64(NOISE_SECONDS));
    shared.measure_noise.store(false, Ordering::Relaxed);
    let noise_peak = f32::from_bits(shared.noise_peak.load(Ordering::Relaxed));
    let noise_frames = shared.noise_frames.load(Ordering::Relaxed);
    if noise_frames == 0 {
        return Err(
            "Der Eingangsstream hat in 0,5 s keinen einzigen Callback geliefert - Geraet oder Treiber pruefen."
                .to_string(),
        );
    }
    let noise_rms =
        ((shared.noise_sq_sum.load(Ordering::Relaxed) as f64 / SQ_SCALE) / noise_frames as f64)
            .sqrt() as f32;
    println!(
        "Grundrauschen: RMS {} , Peak {}",
        fmt_dbfs(noise_rms),
        fmt_dbfs(noise_peak)
    );

    if noise_peak > MAX_NOISE_PEAK {
        return Err(format!(
            "Eingang zu laut oder Rueckkopplung - Gain pruefen (Peak {:.1} dBFS, Grenze {:.1} dBFS).",
            dbfs(noise_peak),
            dbfs(MAX_NOISE_PEAK)
        ));
    }

    // ---- b) detection threshold ---------------------------------------------------------
    let threshold = (20.0 * noise_peak).max(MIN_THRESHOLD);
    if threshold > MAX_THRESHOLD {
        return Err(format!(
            "Grundrauschen zu hoch: die Detektionsschwelle laege bei {:.3} ({:.1} dBFS) und damit ueber der Impulsamplitude. Gain herunterdrehen oder Kabel pruefen.",
            threshold,
            dbfs(threshold)
        ));
    }
    shared.threshold.store(threshold.to_bits(), Ordering::Relaxed);
    println!(
        "Detektionsschwelle: {:.4} ({})\n",
        threshold,
        fmt_dbfs(threshold)
    );

    // ---- c/d) the runs ------------------------------------------------------------------
    match in_buffer {
        Some(frames) => println!(
            "Tatsaechliche Puffergroesse Eingang laut Treiber: {frames} Frames ({:.2} ms)",
            frames as f64 * 1000.0 / rate_f
        ),
        None => println!("Tatsaechliche Puffergroesse Eingang laut Treiber: nicht ermittelbar"),
    }
    match out_buffer {
        Some(frames) => println!(
            "Tatsaechliche Puffergroesse Ausgang laut Treiber: {frames} Frames ({:.2} ms)",
            frames as f64 * 1000.0 / rate_f
        ),
        None => println!("Tatsaechliche Puffergroesse Ausgang laut Treiber: nicht ermittelbar"),
    }
    println!();

    let mut ts_samples: Vec<f64> = Vec::with_capacity(runs as usize);
    let mut frame_samples: Vec<f64> = Vec::with_capacity(runs as usize);
    let mut misses = 0u32;
    // Runs where the pulse was detected but the timestamps did not yield a usable difference.
    let mut ts_invalid = 0u32;

    for run in 1..=runs {
        shared.detected.store(false, Ordering::Relaxed);
        shared.pulse_emitted.store(false, Ordering::Relaxed);
        shared.armed.store(true, Ordering::Release);

        let started = Instant::now();
        while !shared.detected.load(Ordering::Acquire) && started.elapsed() < RUN_TIMEOUT {
            std::thread::sleep(Duration::from_millis(1));
        }

        if shared.detected.load(Ordering::Acquire) {
            // Both instants are rebuilt from the halves the callbacks stored, shifted by their
            // in-buffer sample offset, and only then subtracted - via cpal's own checked API.
            // `None` means the capture instant precedes the playback instant, i.e. the two
            // timestamps do not describe a roundtrip. That is a missing measurement, not a zero.
            let pulse_at = shifted_instant(
                shared.pulse_secs.load(Ordering::Relaxed),
                shared.pulse_subsec_nanos.load(Ordering::Relaxed),
                shared.pulse_offset.load(Ordering::Relaxed),
                rate,
            );
            let detect_at = shifted_instant(
                shared.detect_secs.load(Ordering::Relaxed),
                shared.detect_subsec_nanos.load(Ordering::Relaxed),
                shared.detect_offset.load(Ordering::Relaxed),
                rate,
            );
            let ts_delta = match (pulse_at, detect_at) {
                (Some(pulse), Some(detect)) => detect.checked_duration_since(pulse),
                _ => None,
            };

            let frame_diff = shared.detect_frame.load(Ordering::Relaxed) as i64
                - shared.pulse_frame.load(Ordering::Relaxed) as i64;
            let frame_smp = frame_diff as f64;
            frame_samples.push(frame_smp);

            let ts_column = match ts_delta {
                Some(delta) => {
                    let ts_smp = delta.as_secs_f64() * rate_f;
                    ts_samples.push(ts_smp);
                    format!(
                        "{:>8.1} Samples ({:>6.2} ms)",
                        ts_smp,
                        ts_smp * 1000.0 / rate_f
                    )
                }
                None => {
                    ts_invalid += 1;
                    // Same width as the valid column so the table stays readable.
                    format!("{:>28}", "ungueltig (kein Roundtrip)")
                }
            };
            println!(
                "Lauf {run:>3}: Zeitstempel {ts_column}  |  Frame-Zaehler {:>8.1} Samples ({:>6.2} ms)",
                frame_smp,
                frame_smp * 1000.0 / rate_f
            );
        } else {
            misses += 1;
            shared.armed.store(false, Ordering::Relaxed);
            println!("Lauf {run:>3}: keine Detektion innerhalb von {RUN_TIMEOUT:?} (Ausreisser)");
        }

        std::thread::sleep(GAP);
    }

    drop(input);
    drop(output);

    // ---- e) statistics ------------------------------------------------------------------
    println!("\n===== Ergebnis =====");
    println!("Laeufe gesamt: {runs}, davon ohne Detektion (Ausreisser): {misses}");
    println!(
        "Methode 1 (Zeitstempel): {} von {} detektierten Laeufen gueltig, {ts_invalid} ungueltig.",
        ts_samples.len(),
        frame_samples.len()
    );
    println!();
    print_stats("Methode 1: Zeitstempel (playback/capture)", &ts_samples, rate);
    if ts_samples.is_empty() && ts_invalid > 0 {
        println!(
            "  ACHTUNG: Methode 1 hat in KEINEM Lauf einen gueltigen Wert geliefert. Die\n\
             Zeitstempel des Treibers (bei ASIO speist cpal sie aus timeGetTime()) lagen jedes\n\
             Mal in der falschen Reihenfolge - Erfassung vor Ausgabe. Fuer diese Messung sind\n\
             sie damit unbrauchbar; es zaehlt ausschliesslich Methode 2."
        );
    }
    println!();
    print_stats("Methode 2: Frame-Zaehler (absolute Indizes)", &frame_samples, rate);
    println!(
        "\nHinweis: Der Frame-Zaehler beider Streams startet beim jeweils ersten Callback bei 0.\n\
         Ein konstanter Versatz zwischen beiden Methoden ist deshalb erwartbar; aussagekraeftig\n\
         ist vor allem die Streuung. Laufen beide Methoden stark auseinander, ist genau das das\n\
         Messergebnis."
    );
    println!(
        "\nXruns waehrend der Messung: {}, sonstige Stream-Fehler: {}",
        shared.xruns.load(Ordering::Relaxed),
        shared.errors.load(Ordering::Relaxed)
    );
    println!(
        "\nZum Vergleich: ein Puffer sind {} Frames = {:.2} ms.",
        setup.buffer_frames(),
        setup.buffer_frames() as f64 * 1000.0 / rate_f
    );
    println!(
        "Zur Plausibilitaet des Absolutwerts: dieselbe Messung bei mehreren Puffergroessen\n\
         (z.B. 128 / 256 / 512) laufen lassen - die Latenz muss linear mitwachsen. Die oben\n\
         ausgegebene Puffergroesse laut Treiber ist der Bezugswert dafuer."
    );
    Ok(())
}

fn count_error(shared: &Arc<Shared>, err: cpal::Error) {
    if err.kind() == ErrorKind::Xrun {
        shared.xruns.fetch_add(1, Ordering::Relaxed);
    } else {
        shared.errors.fetch_add(1, Ordering::Relaxed);
    }
}
