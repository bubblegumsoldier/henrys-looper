//! `calibrate` subcommand: check `--latency-samples` against the real hardware, with a number
//! instead of an opinion.
//!
//! The offline test `recorded_click_loopback_lands_bit_identical_on_the_grid` in `tests.rs` proves
//! the compensation arithmetic against a simulated delay line. This is the same experiment on the
//! real device: with a loopback cable in place the engine records its own click, and the recorded
//! click has to sit on the beat grid of the loop buffer. Where it does not, the distance is the
//! error of the compensation value - signed, in samples.
//!
//! # Why the deviation is exactly the correction, and why the sign is `+`
//!
//! From the derivation in `process.rs`: an input sample arriving at input index `k` is stored at
//! musical position `m = k - R`, where `R` is the value of `--latency-samples`. Let `R_true` be
//! what the hardware actually does. The click for beat `B` leaves the engine at output position
//! `B`, comes back at input index `B + R_true`, and is therefore stored at
//!
//! ```text
//! m = (B + R_true) - R = B + (R_true - R)
//! ```
//!
//! So the signed deviation measured here,
//!
//! ```text
//! d = (found position) - (expected beat boundary) = R_true - R
//! ```
//!
//! is precisely the amount `R` is off by, and the corrected value is
//!
//! ```text
//! R_true = R + d
//! ```
//!
//! Plus, not minus. Read as a sentence: the click landed *late* in the loop (`d > 0`) because the
//! engine subtracted *too little* - the real roundtrip is longer than we told it.
//!
//! # Residual error of the onset detection
//!
//! The click is not a rectangular pulse like the one in `latency.rs`; it has a 1 ms attack and a
//! decaying envelope (see `metro.rs`). Detection therefore looks for the *onset* - the first sample
//! above a threshold derived from the noise floor - and that sample is necessarily a few samples
//! **after** the true start of the tone, because the tone has to climb through the threshold first.
//!
//! Order of magnitude at 48 kHz with the default threshold of 0.02: the offbeat tone (800 Hz)
//! crosses it around sample 5, the downbeat tone (1600 Hz) around sample 4 - both counted from the
//! beat boundary. Attenuation in the loopback cable pushes this later, a hotter input signal
//! earlier. The bias is therefore **systematically positive and of the order of a handful of
//! samples**, which is why anything below roughly 10 samples (0.2 ms) counts as "correct" here and
//! the recommendation errs on the side of a slightly too large `R`. Both click frequencies share
//! one envelope, so downbeat and offbeat carry the same bias and the measurement does not depend on
//! which of the two it looks at.
//!
//! Real-time rules are unchanged: the audio callbacks only push into a FIFO and drive
//! [`EngineCore`]. All analysis happens on the finished loop buffer in the main thread, after the
//! recording is over.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::time::{Duration, Instant};

use clap::Args;
use cpal::traits::StreamTrait;
use cpal::{ErrorKind, InputCallbackInfo, OutputCallbackInfo};

use crate::audio::{self, DeviceOpts};
use crate::meter::{Stats, dbfs, fmt_dbfs, print_stats};

use super::command::{
    BufferChannel, Command, Status, StatusReceiver, buffer_channel, command_channel, status_channel,
};
use super::frame::TrackInput;
use super::metro::Metronome;
use super::process::{
    EngineConfig, EngineCore, OUT_CHANNELS, check_loop, loop_capacity, spread_frame,
};
use super::timeline::{TimeSignature, Timeline};
use super::track::{Track, TrackState};

/// Input FIFO size in units of the audio buffer size, as in `live.rs`.
const INPUT_FIFO_BUFFERS: u32 = 64;
/// Status snapshots per second the audio thread produces.
const STATUS_HZ: u32 = 200;
const NOISE_SECONDS: f64 = 0.5;
/// Noise floor above this level means the loopback is far too hot or feeding back.
const MAX_NOISE_PEAK: f32 = 0.1; // -20 dBFS
/// Detection threshold as a multiple of the measured noise peak.
const NOISE_FACTOR: f32 = 20.0;
const MIN_THRESHOLD: f32 = 0.02;
/// The click peaks at 0.5 before the cable attenuates it, so a threshold beyond this could never
/// be reached and the measurement would silently find nothing.
const MAX_THRESHOLD: f32 = 0.2;
/// Deviation up to this many samples is inside the residual error of the onset detection.
const GOOD_ENOUGH_SAMPLES: f64 = 10.0;
/// A run is only conclusive if at least this share of the expected clicks was found.
const MIN_DETECTION_RATIO: f64 = 0.9;
/// Extra time granted on top of the musical duration before a run is given up on.
const RUN_SLACK: f64 = 5.0;

#[derive(Args, Debug, Clone)]
pub struct CalibrateOpts {
    /// Tempo in Schlaegen pro Minute (Viertel; die Messung laeuft in 4/4)
    #[arg(long, default_value_t = 100.0)]
    pub bpm: f64,

    /// Laenge einer Messaufnahme in Takten
    #[arg(long, default_value_t = 4)]
    pub bars: u32,

    /// Zu pruefende Latenzkompensation in Samples (Standard: Messwert aus Phase 0)
    #[arg(long, default_value_t = 827)]
    pub latency_samples: u64,

    /// Anzahl der Messdurchlaeufe
    #[arg(long, default_value_t = 5)]
    pub runs: u32,

    /// Lautstaerke des Klicks
    #[arg(long, default_value_t = 1.0)]
    pub click_gain: f32,
}

// ---------------------------------------------------------------------------------------------
// Analysis - pure functions on a finished buffer, proven offline in the tests at the bottom
// ---------------------------------------------------------------------------------------------

/// What one recorded loop says about the compensation value.
#[derive(Clone, Debug, Default)]
pub struct ClickAnalysis {
    /// Beat boundaries inside the loop, i.e. the number of clicks that must be found.
    pub expected: usize,
    /// Beat boundaries an onset was matched to.
    pub matched: usize,
    /// Onsets that belong to no beat boundary, or a second onset on one that already had a match.
    pub spurious: usize,
    /// Signed distance of every matched onset from its beat boundary, in samples. Positive means
    /// the click landed late in the loop.
    pub deviations: Vec<f64>,
    /// Peak of the analysed buffer, for the "is there anything in there at all" question.
    pub peak: f32,
}

impl ClickAnalysis {
    /// Mean deviation, or `None` when nothing was detected. The recommendation is built on this,
    /// so an empty loop can never produce one.
    pub fn mean(&self) -> Option<f64> {
        Stats::of(&self.deviations).map(|s| s.mean)
    }

    /// Whether the result may be read as a statement about the latency at all.
    ///
    /// The dangerous failure mode of this test is a silent loop: no onsets means no deviations,
    /// and an empty list of deviations would otherwise average to a perfect zero. So too few
    /// detections is a finding in its own right, never a pass.
    pub fn conclusive(&self) -> bool {
        self.expected > 0 && (self.matched as f64) >= self.expected as f64 * MIN_DETECTION_RATIO
    }
}

/// Onset positions (as offsets into `buf`) of everything crossing `threshold`.
///
/// `blanking` samples after a hit nothing is looked at, so the body of one click cannot be counted
/// several times. See the module comment for the systematic bias of this method.
pub fn detect_onsets(buf: &[f32], threshold: f32, blanking: u64) -> Vec<u64> {
    let mut onsets = Vec::new();
    let mut blocked_until = 0u64;
    for (i, &sample) in buf.iter().enumerate() {
        let i = i as u64;
        if i < blocked_until {
            continue;
        }
        if sample.abs() > threshold {
            onsets.push(i);
            blocked_until = i + blanking;
        }
    }
    onsets
}

/// Compare the clicks in a recorded loop against the beat grid they should sit on.
///
/// `origin` is the musical position of `buf[0]`, i.e. the position recording started at.
///
/// Boundary effect worth knowing: if the deviation is negative, the click of the first beat lands
/// just before `buf[0]` and cannot be found, while the click belonging to the beat at the end of
/// the loop moves inside it and is counted as being outside the grid. One click of each is
/// therefore lost at the edges - visible in the counts, harmless for the mean.
pub fn analyse_loop(
    buf: &[f32],
    origin: u64,
    timeline: &Timeline,
    threshold: f32,
    blanking: u64,
) -> ClickAnalysis {
    let mut result = ClickAnalysis {
        peak: buf.iter().fold(0.0f32, |p, s| p.max(s.abs())),
        ..Default::default()
    };
    if buf.is_empty() {
        return result;
    }
    let end = origin + buf.len() as u64;

    // Beat boundaries covered by this buffer. `origin` is a bar boundary in practice, but the
    // first beat is searched for rather than assumed.
    let mut first_beat = timeline.beat_index_at(origin);
    if timeline.beat_start(first_beat) < origin {
        first_beat += 1;
    }
    let mut last_beat = first_beat;
    while timeline.beat_start(last_beat) < end {
        result.expected += 1;
        last_beat += 1;
    }

    // An onset further from a beat boundary than this is not a mistimed click but something else
    // entirely, and would poison the mean.
    let max_distance = timeline.samples_per_beat() / 4.0;
    let mut hit: Vec<u64> = Vec::with_capacity(result.expected);

    for offset in detect_onsets(buf, threshold, blanking) {
        let pos = origin + offset;
        let (beat, deviation) = nearest_beat(timeline, pos);
        let in_range = beat >= first_beat && beat < last_beat;
        if !in_range || deviation.abs() > max_distance || hit.contains(&beat) {
            result.spurious += 1;
            continue;
        }
        hit.push(beat);
        result.deviations.push(deviation);
    }
    result.matched = hit.len();
    result
}

/// Beat boundary closest to `pos`, plus the signed distance to it (positive: `pos` is later).
fn nearest_beat(timeline: &Timeline, pos: u64) -> (u64, f64) {
    let beat = timeline.beat_index_at(pos);
    let before = timeline.beat_start(beat);
    let after = timeline.beat_start(beat + 1);
    if pos - before <= after - pos {
        (beat, (pos - before) as f64)
    } else {
        (beat + 1, -((after - pos) as f64))
    }
}

/// German one-liner classifying a mean deviation.
fn verdict(mean: f64) -> String {
    if mean.abs() <= GOOD_ENOUGH_SAMPLES {
        format!(
            "Die mittlere Abweichung liegt mit {:.1} Samples im Rahmen der Impulsdetektion \
             (Grenze {:.0} Samples). Der eingestellte Wert passt.",
            mean, GOOD_ENOUGH_SAMPLES
        )
    } else if mean > 0.0 {
        format!(
            "Der aufgenommene Klick sitzt im Mittel {:.1} Samples ZU SPAET im Loop. Die \
             Kompensation zieht zu wenig ab, der Wert muss groesser werden.",
            mean
        )
    } else {
        format!(
            "Der aufgenommene Klick sitzt im Mittel {:.1} Samples ZU FRUEH im Loop. Die \
             Kompensation zieht zu viel ab, der Wert muss kleiner werden.",
            -mean
        )
    }
}

/// Rebuild the device flags so the recommendation can be printed as a command line the user can
/// paste without thinking about it.
fn device_flags(dev: &DeviceOpts) -> String {
    let mut parts = vec![
        format!("--host {}", dev.host),
        format!("--rate {}", dev.rate),
        format!("--buffer {}", dev.buffer),
    ];
    if let Some(name) = &dev.device {
        parts.insert(1, format!("--device \"{name}\""));
    }
    if let Some(c) = dev.in_channels {
        parts.push(format!("--in-channels {c}"));
    }
    if let Some(c) = dev.out_channels {
        parts.push(format!("--out-channels {c}"));
    }
    if dev.force_buffer {
        parts.push("--force-buffer".to_string());
    }
    parts.join(" ")
}

// ---------------------------------------------------------------------------------------------
// The measurement on real hardware
// ---------------------------------------------------------------------------------------------

#[derive(Default)]
struct Shared {
    measure_noise: AtomicBool,
    noise_peak: AtomicU32,
    noise_sq_sum: AtomicU64,
    noise_frames: AtomicU64,
    xruns: AtomicU64,
    errors: AtomicU64,
    /// Input callbacks that could not push - samples lost, every later position shifted.
    overruns: AtomicU64,
    /// Output callbacks that found the FIFO short.
    underruns: AtomicU64,
}

impl Shared {
    fn count_error(&self, err: cpal::Error) {
        if err.kind() == ErrorKind::Xrun {
            self.xruns.fetch_add(1, Ordering::Relaxed);
        } else {
            self.errors.fetch_add(1, Ordering::Relaxed);
        }
    }
}

/// Fixed-point scale for accumulating a sum of squares in an atomic integer, as in `latency.rs`.
const SQ_SCALE: f64 = 1_048_576.0; // 2^20

/// Where the audio thread is right now, extrapolated from the newest status snapshot.
fn estimated_pos(last: Option<(Status, Instant)>, rate: u32) -> u64 {
    match last {
        Some((s, seen)) => s.pos + (seen.elapsed().as_secs_f64() * rate as f64) as u64,
        None => 0,
    }
}

/// Wait until a status snapshot satisfies `ready`, keeping `last` up to date on the way.
fn poll_until(
    rx: &mut StatusReceiver,
    last: &mut Option<(Status, Instant)>,
    timeout: Duration,
    ready: impl Fn(&Status) -> bool,
) -> Result<Status, String> {
    let deadline = Instant::now() + timeout;
    loop {
        if let Some(s) = rx.latest() {
            *last = Some((s, Instant::now()));
            if ready(&s) {
                return Ok(s);
            }
        }
        if Instant::now() >= deadline {
            return Err(
                "Die Engine hat die Messaufnahme nicht rechtzeitig abgeschlossen - Streams oder \
                 Geraet pruefen."
                    .to_string(),
            );
        }
        std::thread::sleep(Duration::from_millis(5));
    }
}

/// Collect the loop buffer the engine hands back after a fresh one was installed.
fn take_retired(buffers: &mut BufferChannel, timeout: Duration) -> Result<Vec<f32>, String> {
    let deadline = Instant::now() + timeout;
    loop {
        if let Ok(buffer) = buffers.retire_rx.pop() {
            return Ok(buffer);
        }
        if Instant::now() >= deadline {
            return Err(
                "Der Audio-Thread hat den aufgenommenen Loop-Puffer nicht zurueckgegeben."
                    .to_string(),
            );
        }
        std::thread::sleep(Duration::from_millis(5));
    }
}

pub fn cmd_calibrate(dev: &DeviceOpts, opts: &CalibrateOpts) -> Result<(), String> {
    if opts.runs == 0 {
        return Err("--runs muss mindestens 1 sein.".to_string());
    }
    let signature = TimeSignature::new(4, 4);
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
    let loop_len_nominal = timeline.span_bars(0, opts.bars);
    println!(
        "Takt:     {:.1} BPM, 4/4, {:.3} Samples pro Schlag",
        opts.bpm,
        timeline.samples_per_beat()
    );
    println!(
        "Messloop: {} Takte = {} Samples = {:.2} s, {} Schlaege",
        opts.bars,
        loop_len_nominal,
        timeline.samples_to_secs(loop_len_nominal),
        opts.bars * 4
    );
    println!(
        "Pruefwert: --latency-samples {} ({:.2} ms)",
        opts.latency_samples,
        opts.latency_samples as f64 * 1000.0 / rate as f64
    );
    println!("Laeufe:   {}", opts.runs);
    println!(
        "\nAufbau: Kabel von Ausgang 1 zurueck in Eingang 1, Direct Monitor am Interface aus.\n\
         Die Engine spielt ihren eigenen Klick ueber den Ausgang und nimmt ihn wieder auf.\n\
         Kopfhoerer vorher abnehmen oder leise drehen.\n"
    );

    let in_channels = setup.in_plan.config.channels as usize;
    let out_channels = setup.out_plan.config.channels as usize;
    let buffer_frames = setup.buffer_frames().max(1);
    let scratch_frames = (buffer_frames * 16) as usize;
    let capacity = loop_capacity(&timeline, opts.bars) as usize;

    let shared = Arc::new(Shared::default());
    shared.measure_noise.store(true, Ordering::Relaxed);

    let (mut producer, mut consumer) =
        rtrb::RingBuffer::<f32>::new((buffer_frames * INPUT_FIFO_BUFFERS) as usize);
    let (mut cmd_tx, cmd_rx) = command_channel(256);
    let (status_tx, mut status_rx) = status_channel(1024);
    let (mut buffers, buffer_endpoint) = buffer_channel(4, 8);

    // Two prepared layer buffers, allocated here in the main thread: one for the running take, one
    // ready for the next run while the recorded one is being analysed.
    for _ in 0..2 {
        buffers.install(vec![0.0f32; capacity])?;
    }

    // One track on input channel 1, the channel the loopback cable feeds. The click starts
    // switched off: the noise floor has to be measured in silence, and it is switched on by command
    // as soon as the threshold is known.
    let core = EngineCore::new(EngineConfig {
        timeline,
        latency_samples: opts.latency_samples,
        // The input callback below feeds the FIFO with channel 1 only, so the engine sees one
        // channel per frame.
        input_channels: 1,
        // The chain stays bypassed for the whole measurement: a calibration compares recorded
        // samples with the grid they were played against, and an effect on the playback path would
        // change what the loopback cable brings back. The track is mono and centred, because the
        // cable carries one channel and the click is in the middle anyway.
        tracks: vec![Track::new(TrackInput::Mono(0), false, 0.0, rate)],
        spares: [Vec::with_capacity(2), Vec::new()],
        layer_frames: capacity as u64,
        commands: cmd_rx,
        status: status_tx,
        buffers: buffer_endpoint,
        monitor_gain: 0.0,
        click: false,
        click_gain: opts.click_gain,
        status_interval: (rate / STATUS_HZ).max(1) as u64,
    });

    // ---- input stream: noise statistics plus the FIFO, nothing else ----------------------
    let sh_in = Arc::clone(&shared);
    let sh_in_err = Arc::clone(&shared);
    let input = audio::build_input(
        &setup.input,
        &setup.in_plan,
        move |data: &[f32], _: &InputCallbackInfo, _offset: usize| {
            let mut overrun = false;
            let mut peak = 0.0f32;
            let mut sq = 0.0f64;
            let measuring = sh_in.measure_noise.load(Ordering::Relaxed);
            let mut frames = 0u64;
            for frame in data.chunks_exact(in_channels) {
                let s = frame[0];
                if producer.push(s).is_err() {
                    overrun = true;
                }
                if measuring {
                    let a = s.abs();
                    if a > peak {
                        peak = a;
                    }
                    sq += (s as f64) * (s as f64);
                    frames += 1;
                }
            }
            if measuring {
                sh_in.noise_peak.fetch_max(peak.to_bits(), Ordering::Relaxed);
                sh_in
                    .noise_sq_sum
                    .fetch_add((sq * SQ_SCALE) as u64, Ordering::Relaxed);
                sh_in.noise_frames.fetch_add(frames, Ordering::Relaxed);
            }
            if overrun {
                sh_in.overruns.fetch_add(1, Ordering::Relaxed);
            }
        },
        move |err| sh_in_err.count_error(err),
    )?;

    // ---- output stream: drives the engine, exactly as in `live` ---------------------------
    let sh_out = Arc::clone(&shared);
    let sh_out_err = Arc::clone(&shared);
    let mut core = core;
    let mut in_scratch = vec![0.0f32; scratch_frames];
    let mut out_scratch = vec![0.0f32; scratch_frames * OUT_CHANNELS];
    let output = audio::build_output(
        &setup.output,
        &setup.out_plan,
        move |data: &mut [f32], _: &OutputCallbackInfo, _offset: usize| {
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
                    sh_out.underruns.fetch_add(1, Ordering::Relaxed);
                }
                core.process(&in_scratch[..got], &mut out_scratch[..n * OUT_CHANNELS]);
                let base = done * out_channels;
                for (i, frame) in data[base..base + n * out_channels]
                    .chunks_exact_mut(out_channels)
                    .enumerate()
                {
                    spread_frame(&out_scratch[i * OUT_CHANNELS..i * OUT_CHANNELS + OUT_CHANNELS], frame);
                }
                done += n;
            }
        },
        move |err| sh_out_err.count_error(err),
    )?;

    if let Ok(frames) = input.buffer_size() {
        println!("Puffer laut Treiber: Eingang {frames} Frames");
    }
    if let Ok(frames) = output.buffer_size() {
        println!("Puffer laut Treiber: Ausgang {frames} Frames");
    }

    // Input first, then output - the same order the 827 samples of phase 0 were measured in.
    input
        .play()
        .map_err(|e| format!("Eingangsstream startet nicht: {e}"))?;
    output
        .play()
        .map_err(|e| format!("Ausgabestream startet nicht: {e}"))?;

    // ---- a) noise floor -------------------------------------------------------------------
    println!("\nMesse Grundrauschen ({NOISE_SECONDS:.1} s, Klick noch aus) ...");
    std::thread::sleep(Duration::from_secs_f64(NOISE_SECONDS));
    shared.measure_noise.store(false, Ordering::Relaxed);
    let noise_peak = f32::from_bits(shared.noise_peak.load(Ordering::Relaxed));
    let noise_frames = shared.noise_frames.load(Ordering::Relaxed);
    if noise_frames == 0 {
        return Err(
            "Der Eingangsstream hat in 0,5 s keinen einzigen Callback geliefert - Geraet oder \
             Treiber pruefen."
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

    let threshold = (NOISE_FACTOR * noise_peak).max(MIN_THRESHOLD);
    if threshold > MAX_THRESHOLD {
        return Err(format!(
            "Grundrauschen zu hoch: die Detektionsschwelle laege bei {:.3} ({:.1} dBFS) und damit \
             ueber dem, was der Klick nach dem Kabel noch erreicht. Gain herunterdrehen oder Kabel \
             pruefen.",
            threshold,
            dbfs(threshold)
        ));
    }
    println!(
        "Detektionsschwelle: {:.4} ({})",
        threshold,
        fmt_dbfs(threshold)
    );

    // One click tone is 30 ms long; twice that is a safe blanking time and still only a tenth of a
    // beat at 100 BPM, so a stray detection between two clicks stays visible instead of being
    // swallowed. Never more than half a beat, so a slow tempo cannot mask the next click.
    let tone_len = Metronome::new(rate).tone_len();
    let blanking = (2 * tone_len).min((timeline.samples_per_beat() / 2.0) as u64).max(1);
    println!("Sperrzeit nach einem Onset: {blanking} Samples\n");

    cmd_tx.send(Command::SetClick { on: true })?;

    // ---- b) the runs ----------------------------------------------------------------------
    let mut last: Option<(Status, Instant)> = None;
    let mut all_deviations: Vec<f64> = Vec::new();
    let mut total_expected = 0usize;
    let mut total_matched = 0usize;
    let mut total_spurious = 0usize;
    let mut run_means: Vec<f64> = Vec::new();
    // Four buffers plus 20 ms of head start, as in `live`: a scheduled command always reaches the
    // audio thread before its own timestamp.
    let guard = (buffer_frames * 4 + rate / 50) as u64;

    for run in 1..=opts.runs {
        let est = estimated_pos(last, rate);
        let start = timeline.bar_start_at_or_after(est + guard);
        let bar = timeline.bar_index_at(start);
        let end = timeline.position_of(bar + opts.bars as u64, 0);
        let loop_len = end - start;

        cmd_tx.send(Command::StartRecord { track: 0, at: start })?;
        cmd_tx.send(Command::StopRecord { track: 0, at: end })?;

        let ahead = (end + opts.latency_samples).saturating_sub(est);
        let timeout = Duration::from_secs_f64(timeline.samples_to_secs(ahead) + RUN_SLACK);
        let done = poll_until(&mut status_rx, &mut last, timeout, |s| {
            let t = s.tracks()[0];
            t.state == TrackState::Ready && t.loop_len == loop_len
        })?;
        let done_loop_len = done.tracks()[0].loop_len;

        // Remove the recorded layer: the engine hands its buffer back through the return queue.
        // That is how the loop content reaches the main thread without the audio thread ever
        // allocating or freeing anything.
        cmd_tx.send(Command::RemoveLayer { track: 0, layer: 0 })?;
        let mut recorded = take_retired(&mut buffers, Duration::from_secs(2))?;
        if (recorded.len() as u64) < done_loop_len {
            return Err(format!(
                "Der zurueckgegebene Puffer ist mit {} Samples kuerzer als der Loop ({} Samples).",
                recorded.len(),
                done_loop_len
            ));
        }

        let analysis = analyse_loop(
            &recorded[..done_loop_len as usize],
            start,
            &timeline,
            threshold,
            blanking,
        );
        println!(
            "----- Lauf {run} von {} (Takt {} bis {}, {} Samples) -----",
            opts.runs,
            bar + 1,
            bar + opts.bars as u64,
            loop_len
        );
        println!(
            "  Klicks erkannt: {} von {} erwartet, {} ausserhalb des Rasters, Loop-Peak {}",
            analysis.matched,
            analysis.expected,
            analysis.spurious,
            fmt_dbfs(analysis.peak)
        );
        if !analysis.conclusive() {
            println!(
                "  ACHTUNG: zu wenige Klicks erkannt. Dieser Lauf ist KEIN Beleg fuer eine\n\
                 \x20 korrekte Kalibrierung, sondern ein Hinweis auf einen zu leisen Pegel, ein\n\
                 \x20 fehlendes Loopback-Kabel oder einen leeren Loop."
            );
        }
        if analysis.deviations.is_empty() {
            println!("  Keine Abweichung berechenbar.\n");
        } else {
            print_stats("  Abweichung", &analysis.deviations, rate);
            println!();
        }

        total_expected += analysis.expected;
        total_matched += analysis.matched;
        total_spurious += analysis.spurious;
        if let Some(mean) = analysis.mean() {
            run_means.push(mean);
        }
        all_deviations.extend_from_slice(&analysis.deviations);

        // Zeroed here in the main thread and handed back, so the next run finds a prepared buffer
        // waiting instead of allocating one.
        recorded.fill(0.0);
        buffers.install(recorded)?;
    }

    drop(input);
    drop(output);
    buffers.drain_retired();

    // ---- c) result ------------------------------------------------------------------------
    println!("===== Ergebnis =====");
    println!(
        "Klicks erkannt: {total_matched} von {total_expected} erwartet ({} ausserhalb des Rasters).",
        total_spurious
    );
    let overruns = shared.overruns.load(Ordering::Relaxed);
    println!(
        "Xruns {}, FIFO leer {}, FIFO uebergelaufen {overruns}, sonstige Fehler {}.",
        shared.xruns.load(Ordering::Relaxed),
        shared.underruns.load(Ordering::Relaxed),
        shared.errors.load(Ordering::Relaxed)
    );
    if overruns > 0 {
        println!(
            "ACHTUNG: Der Eingangs-FIFO ist uebergelaufen. Dabei gehen Eingangssamples verloren,\n\
             und genau das verschiebt jede spaetere Position. Die Messung ist damit unbrauchbar -\n\
             bitte wiederholen."
        );
    }

    if total_matched == 0 {
        return Err(
            "In keinem Lauf wurde auch nur ein Klick erkannt. Das ist KEINE perfekte Kalibrierung,\n\
             sondern eine leere Aufnahme. Zu pruefen: Loopback-Kabel gesteckt (Ausgang 1 in\n\
             Eingang 1), Eingangs-Gain hoch genug, Ausgangspegel nicht auf null, richtiges Geraet\n\
             gewaehlt."
                .to_string(),
        );
    }

    println!();
    print_stats("Abweichung ueber alle Laeufe", &all_deviations, rate);
    if run_means.len() > 1 {
        println!();
        print_stats("Mittelwert je Lauf", &run_means, rate);
    }

    let conclusive = total_expected > 0
        && (total_matched as f64) >= total_expected as f64 * MIN_DETECTION_RATIO;
    let mean = Stats::of(&all_deviations)
        .map(|s| s.mean)
        .expect("mindestens ein Messwert, sonst waere oben abgebrochen worden");
    let recommended = (opts.latency_samples as f64 + mean).round().max(0.0) as u64;

    println!("\n{}", verdict(mean));
    if !conclusive {
        println!(
            "ACHTUNG: Es wurden deutlich weniger Klicks erkannt als erwartet ({total_matched} von\n\
             {total_expected}). Die Zahlen unten stuetzen sich auf zu wenige Treffer - erst den\n\
             Pegel oder die Verkabelung in Ordnung bringen und neu messen."
        );
    }
    println!(
        "\nAktuell {} Samples, mittlere Abweichung {:+.1} Samples ({:+.3} ms).",
        opts.latency_samples,
        mean,
        mean * 1000.0 / rate as f64
    );
    println!(
        "Empfehlung: --latency-samples {recommended}\n\
         (Herleitung: der aufgenommene Klick landet bei R_wahr - R vom Raster entfernt, also ist\n\
         R_wahr = R + Abweichung. Zu spaet aufgenommen heisst: zu wenig abgezogen.)"
    );
    println!(
        "\nUebernehmen mit:\n  looper-engine live {} --bpm {} --latency-samples {recommended}",
        device_flags(dev),
        opts.bpm
    );
    println!(
        "  looper-engine calibrate {} --bpm {} --bars {} --latency-samples {recommended}   (zur Gegenprobe)",
        device_flags(dev),
        opts.bpm,
        opts.bars
    );
    println!(
        "\nHinweis: Die Onset-Detektion erkennt den Klick erst, wenn er die Schwelle ueberschreitet,\n\
         also systematisch ein paar Samples zu spaet. Abweichungen unter {:.0} Samples ({:.2} ms)\n\
         sind deshalb Messrauschen und kein Korrekturbedarf.",
        GOOD_ENOUGH_SAMPLES,
        GOOD_ENOUGH_SAMPLES * 1000.0 / rate as f64
    );
    Ok(())
}

// ---------------------------------------------------------------------------------------------
// Offline tests of the analysis. Nothing here opens an audio device.
// ---------------------------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    const RATE: u32 = 48_000;
    const THRESHOLD: f32 = 0.02;

    fn timeline() -> Timeline {
        Timeline::new(RATE, 100.0, TimeSignature::new(4, 4))
    }

    /// A loop of `bars` bars starting at `origin`, with a single-sample impulse `shift` samples
    /// away from every beat boundary. `shift` may be negative.
    fn loop_with_impulses(timeline: &Timeline, origin: u64, bars: u32, shift: i64) -> Vec<f32> {
        let len = timeline.span_bars(timeline.bar_index_at(origin), bars);
        let mut buf = vec![0.0f32; len as usize];
        let first = timeline.beat_index_at(origin);
        let mut beat = first;
        while timeline.beat_start(beat) < origin + len {
            let pos = timeline.beat_start(beat) as i64 + shift;
            let idx = pos - origin as i64;
            if idx >= 0 && (idx as u64) < len {
                buf[idx as usize] = 0.9;
            }
            beat += 1;
        }
        buf
    }

    #[test]
    fn impulses_exactly_on_the_grid_give_zero_deviation() {
        let t = timeline();
        let origin = t.bar_start(4);
        let buf = loop_with_impulses(&t, origin, 4, 0);

        let a = analyse_loop(&buf, origin, &t, THRESHOLD, 2_880);
        assert_eq!(a.expected, 16, "vier Takte zu vier Schlaegen");
        assert_eq!(a.matched, 16, "jeder Klick muss gefunden werden");
        assert_eq!(a.spurious, 0);
        assert!(a.conclusive());
        for d in &a.deviations {
            assert_eq!(*d, 0.0);
        }
        assert_eq!(a.mean(), Some(0.0));
    }

    #[test]
    fn impulses_shifted_by_forty_samples_give_forty_with_the_right_sign() {
        let t = timeline();
        let origin = t.bar_start(4);

        // Late: the click sits behind its beat, the compensation subtracted too little, the value
        // has to grow. That is the sign the recommendation depends on.
        let late = loop_with_impulses(&t, origin, 4, 40);
        let a = analyse_loop(&late, origin, &t, THRESHOLD, 2_880);
        assert_eq!(a.matched, 16);
        assert_eq!(a.spurious, 0);
        assert_eq!(a.mean(), Some(40.0));
        for d in &a.deviations {
            assert_eq!(*d, 40.0);
        }
        assert_eq!((827.0 + a.mean().unwrap()).round() as u64, 867);

        // Early: mirrored, including the sign. The click of the very first beat falls 40 samples
        // before the start of the loop and is therefore physically not in the buffer - 15 of 16.
        let early = loop_with_impulses(&t, origin, 4, -40);
        let b = analyse_loop(&early, origin, &t, THRESHOLD, 2_880);
        assert_eq!(b.matched, 15, "der erste Klick liegt vor dem Loop-Anfang");
        assert_eq!(b.spurious, 0);
        assert!(b.conclusive(), "15 von 16 sind immer noch aussagekraeftig");
        assert_eq!(b.mean(), Some(-40.0));
        assert_eq!((827.0 + b.mean().unwrap()).round() as u64, 787);
    }

    /// The most dangerous misreading of this test: an empty loop must never look like a perfect
    /// result. No detections means no deviations, no mean and no recommendation.
    #[test]
    fn an_empty_loop_reports_no_clicks_instead_of_a_perfect_result() {
        let t = timeline();
        let origin = t.bar_start(4);
        let len = t.span_bars(4, 4) as usize;

        let silent = vec![0.0f32; len];
        let a = analyse_loop(&silent, origin, &t, THRESHOLD, 2_880);
        assert_eq!(a.expected, 16);
        assert_eq!(a.matched, 0, "in Stille darf nichts erkannt werden");
        assert!(a.deviations.is_empty());
        assert_eq!(a.mean(), None, "ohne Treffer gibt es keinen Mittelwert");
        assert!(!a.conclusive(), "ein leerer Loop ist niemals ein Beleg");

        // Just noise below the threshold counts the same way.
        let mut noisy = vec![0.0f32; len];
        for (i, s) in noisy.iter_mut().enumerate() {
            *s = if i % 2 == 0 { 0.005 } else { -0.005 };
        }
        let b = analyse_loop(&noisy, origin, &t, THRESHOLD, 2_880);
        assert_eq!(b.matched, 0);
        assert_eq!(b.mean(), None);
        assert!(!b.conclusive());

        // And a loop where only a single click made it through is not conclusive either.
        let mut one = vec![0.0f32; len];
        one[0] = 0.9;
        let c = analyse_loop(&one, origin, &t, THRESHOLD, 2_880);
        assert_eq!(c.matched, 1);
        assert_eq!(c.mean(), Some(0.0), "die eine Abweichung ist rechnerisch 0 ...");
        assert!(!c.conclusive(), "... aber ein Treffer von 16 belegt nichts");
    }

    /// The real click, rendered by the metronome itself and analysed with the real threshold. This
    /// pins down the systematic residual error documented in the module comment: a few samples,
    /// always positive, and the same for downbeat (1600 Hz) and offbeat (800 Hz).
    #[test]
    fn the_real_click_is_detected_a_few_samples_late_on_every_beat() {
        let t = Timeline::new(RATE, 100.0, TimeSignature::new(4, 4));
        let metro = Metronome::new(RATE);
        let origin = t.bar_start(4);
        let len = t.span_bars(4, 4);
        let buf: Vec<f32> = (0..len)
            .map(|i| metro.sample_at(&t, origin + i))
            .collect();

        let blanking = (2 * metro.tone_len()).min((t.samples_per_beat() / 2.0) as u64);
        let a = analyse_loop(&buf, origin, &t, THRESHOLD, blanking);
        assert_eq!(a.matched, 16, "jeder Klick wird gefunden");
        assert_eq!(a.spurious, 0, "kein Nachschwinger wird doppelt gezaehlt");
        let mean = a.mean().expect("Treffer vorhanden");
        assert!(
            mean > 0.0 && mean <= GOOD_ENOUGH_SAMPLES,
            "systematischer Restfehler soll positiv und klein sein, ist {mean}"
        );

        // Downbeat and offbeat must not differ meaningfully - the detection may not depend on the
        // click frequency. Every fourth deviation belongs to a downbeat.
        let downbeats: Vec<f64> = a.deviations.iter().step_by(4).copied().collect();
        let offbeats: Vec<f64> = a
            .deviations
            .iter()
            .enumerate()
            .filter(|(i, _)| i % 4 != 0)
            .map(|(_, d)| *d)
            .collect();
        let mean_of = |v: &[f64]| v.iter().sum::<f64>() / v.len() as f64;
        assert!(
            (mean_of(&downbeats) - mean_of(&offbeats)).abs() <= 3.0,
            "Downbeat {} und Offbeat {} duerfen sich kaum unterscheiden",
            mean_of(&downbeats),
            mean_of(&offbeats)
        );
    }

    /// A click shifted by a known amount through the *engine's* own compensation arithmetic: the
    /// analysis has to report exactly that shift, which is what makes the recommendation a
    /// correction and not a guess.
    #[test]
    fn a_wrong_compensation_value_shows_up_as_its_own_difference() {
        let t = timeline();
        let metro = Metronome::new(RATE);
        let origin = t.bar_start(4);
        let len = t.span_bars(4, 4);
        // Engine compensates R, hardware does R_true = R + 40, so a sample stored at musical
        // position m actually carries the click of position m - 40.
        let error = 40i64;
        let buf: Vec<f32> = (0..len)
            .map(|i| metro.sample_at(&t, (origin as i64 + i as i64 - error).max(0) as u64))
            .collect();

        let a = analyse_loop(&buf, origin, &t, THRESHOLD, 2_880);
        let mean = a.mean().expect("Treffer vorhanden");
        // The onset bias of a few samples rides on top of the 40, and nothing else.
        assert!(
            (mean - error as f64).abs() <= GOOD_ENOUGH_SAMPLES,
            "erwartet rund {error} Samples, gemessen {mean}"
        );
        assert!((827.0 + mean).round() as u64 >= 863);
    }

    #[test]
    fn onset_detection_blanks_the_tail_of_a_click() {
        // Three impulses, the middle one inside the blanking window of the first.
        let mut buf = vec![0.0f32; 10_000];
        buf[100] = 0.9;
        buf[200] = 0.9;
        buf[5_000] = 0.9;
        let onsets = detect_onsets(&buf, THRESHOLD, 1_000);
        assert_eq!(onsets, vec![100, 5_000]);
    }

    #[test]
    fn onsets_far_from_any_beat_are_counted_as_spurious() {
        let t = timeline();
        let origin = t.bar_start(4);
        let mut buf = loop_with_impulses(&t, origin, 4, 0);
        // Half a beat away from the grid: not a mistimed click, so it must not enter the mean.
        buf[(t.samples_per_beat() / 2.0) as usize] = 0.9;

        let a = analyse_loop(&buf, origin, &t, THRESHOLD, 2_880);
        assert_eq!(a.matched, 16);
        assert_eq!(a.spurious, 1);
        assert_eq!(a.mean(), Some(0.0), "der Ausreisser verfaelscht nichts");
    }
}
