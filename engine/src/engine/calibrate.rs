//! `calibrate` subcommand: check the latency compensation against the real hardware, with a number
//! instead of an opinion.
//!
//! # One input at a time, because one number no longer fits
//!
//! The compensation is per track (see [`super::track::TrackLatency`]), so a measurement is per
//! *input*: `--for-track` picks which of the configured tracks the engine records its click on,
//! and the recommendation that comes out is that track's measured value. The default is the first
//! track, which on the default setup is input 1 - exactly what this command did when there was
//! only one number.
//!
//! For a source that arrives through an ASIO router (a plugin host, say) this needs **no cable**:
//! the router loops the output back into the input itself. What such a measurement covers is the
//! way from the host's output to ours - not what the host does inside itself, which is why the
//! track value has a manual surcharge next to the measured one. This command never touches that
//! surcharge.
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
//! musical position `m = k - R`, where `R` is the value under test (`--latency-frames`, or a
//! track's own). Let `R_true` be
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
//! **This bias is level-dependent, and that is the trap of the whole procedure.** Measured on this
//! machine at one and the same latency: a loop peaking at -27 dBFS showed 32 samples of deviation,
//! the same run at -18 dBFS showed 7.2. A signal that climbs slowly needs longer to cross a fixed
//! threshold, and every sample of that climb is added to the result. Calibrating from a quiet loop
//! therefore does not measure the latency, it measures the level - and burns the difference into
//! the setup. Hence [`GOOD_PEAK`]: below it the report says so, on every run and again at the end.
//! A cross-correlation would be the level-independent way and is not built.
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
use super::live::{TrackDef, resolve_tracks};
use super::metro::Metronome;
use super::bus::{BUS_SAMPLES, place_buses};
use super::process::{
    EngineConfig, EngineCore, check_loop, loop_capacity, measurement_routing,
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
/// Loop peak from which the onset detection is trustworthy: -18 dBFS.
///
/// Not a round number picked for looks. Measured on this machine at an unchanged latency: -27 dBFS
/// gave 32 samples of deviation, -18 dBFS gave 7.2. Below this the result says more about the gain
/// than about the interface, and the report has to say so - a musician who trims his input too
/// quietly would otherwise calibrate a measurement artefact into his setup.
const GOOD_PEAK: f32 = 0.126;
/// A run is only conclusive if at least this share of the expected clicks was found.
const MIN_DETECTION_RATIO: f64 = 0.9;
/// Extra time granted on top of the musical duration before a run is given up on.
const RUN_SLACK: f64 = 5.0;

#[derive(Args, Debug, Clone)]
pub struct CalibrateOpts {
    /// Tempo in beats per minute (quarter notes; the measurement runs in 4/4)
    #[arg(long, default_value_t = 100.0)]
    pub bpm: f64,

    /// Length of one measurement recording in bars
    #[arg(long, default_value_t = 4)]
    pub bars: u32,

    /// Latency compensation to check, in frames (default: on Windows the value measured
    /// in phase 0, on macOS an unmeasured estimate)
    #[arg(long = "latency-frames", alias = "latency-samples", default_value_t = audio::DEFAULT_LATENCY_FRAMES)]
    pub latency_frames: u64,

    /// Number of measurement runs
    #[arg(long, default_value_t = 5)]
    pub runs: u32,

    /// Click volume
    #[arg(long, default_value_t = 1.0)]
    pub click_gain: f32,

    /// Track as NAME:CHANNEL (mono) or NAME:CHANNEL-CHANNEL (stereo), repeatable - the same
    /// spelling as for live. Without it the same default applies as there (stimme:1, gitarre:2).
    #[arg(long = "track", value_name = "NAME:CHANNEL[-CHANNEL][@PAN]")]
    pub tracks: Vec<String>,

    /// Which track to measure, 1-based. The measurement uses its input; for a stereo track the
    /// left of the two.
    #[arg(long = "for-track", default_value_t = 1)]
    pub for_track: usize,
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

/// One-liner classifying a mean deviation.
fn verdict(mean: f64) -> String {
    if mean.abs() <= GOOD_ENOUGH_SAMPLES {
        format!(
            "The mean deviation of {:.1} samples is within the tolerance of the onset detection \
             (limit {:.0} samples). The current value is right.",
            mean, GOOD_ENOUGH_SAMPLES
        )
    } else if mean > 0.0 {
        format!(
            "On average the recorded click lands {:.1} samples TOO LATE in the loop. The \
             compensation subtracts too little, the value must go up.",
            mean
        )
    } else {
        format!(
            "On average the recorded click lands {:.1} samples TOO EARLY in the loop. The \
             compensation subtracts too much, the value must go down.",
            -mean
        )
    }
}

/// Warning about a loop that was recorded too quietly to trust.
///
/// The numbers in it are the ones from the module comment, and they are in it on purpose: "Pegel
/// zu leise" invites a shrug, "32 Samples statt 7,2 bei gleicher Latenz" does not.
fn low_peak_warning(peak: f32) -> String {
    format!(
        "  WARNING: the loop only reaches {} - the onset detection depends on the level.\n\
         \x20 Measured on this machine at an unchanged latency: -27 dBFS gave 32 samples of\n\
         \x20 deviation, -18 dBFS only 7.2. Setting the level too low calibrates a measurement\n\
         \x20 artefact into the setup. Turn it up first (--click-gain, input gain on the\n\
         \x20 interface, aim for at least {}), then measure again.",
        fmt_dbfs(peak),
        fmt_dbfs(GOOD_PEAK)
    )
}

/// Which of the configured tracks is being measured, or a sentence saying why not.
fn pick_track(defs: &[TrackDef], for_track: usize) -> Result<usize, String> {
    if for_track == 0 || for_track > defs.len() {
        let list: Vec<String> = defs
            .iter()
            .enumerate()
            .map(|(i, d)| format!("{} {} (input {})", i + 1, d.name, d.input.label()))
            .collect();
        return Err(format!(
            "There is no --for-track {for_track}. Available: {}.",
            list.join(", ")
        ));
    }
    Ok(for_track - 1)
}

/// The `--track` arguments that describe this set of tracks, so a recommendation can be printed as
/// a command line that really names the track it talks about.
fn track_flags(defs: &[TrackDef]) -> String {
    defs.iter()
        .map(|d| format!("--track {}:{}", d.name, d.input.label().replace('+', "-")))
        .collect::<Vec<_>>()
        .join(" ")
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
                "The engine did not finish the measurement recording in time - check the streams \
                 or the device."
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
                "The audio thread did not hand back the recorded loop buffer."
                    .to_string(),
            );
        }
        std::thread::sleep(Duration::from_millis(5));
    }
}

pub fn cmd_calibrate(dev: &DeviceOpts, opts: &CalibrateOpts) -> Result<(), String> {
    if opts.runs == 0 {
        return Err("--runs must be at least 1.".to_string());
    }
    let signature = TimeSignature::new(4, 4);
    let setup = audio::open_duplex(dev)?;
    let rate = setup.sample_rate();
    if setup.out_plan.config.sample_rate != rate {
        return Err(format!(
            "Input and output run at different sample rates ({} and {}). \
             The engine's timeline needs one shared clock.",
            rate, setup.out_plan.config.sample_rate
        ));
    }
    Timeline::validate(rate, opts.bpm, signature)?;
    let timeline = Timeline::new(rate, opts.bpm, signature);
    check_loop(&timeline, opts.bars, opts.latency_frames)?;

    let in_channels = setup.in_plan.config.channels as usize;
    let defs = resolve_tracks(&opts.tracks, in_channels)?;
    let target = pick_track(&defs, opts.for_track)?;
    // The left channel of a pair: both halves of a stereo source pass the same converter at the
    // same instant and therefore share one latency - measuring the second one would produce the
    // same number and cost another run.
    let measure_channel = defs[target].input.first();

    audio::print_setup(&setup);
    let loop_len_nominal = timeline.span_bars(0, opts.bars);
    println!(
        "Tempo:    {:.1} BPM, 4/4, {:.3} samples per beat",
        opts.bpm,
        timeline.samples_per_beat()
    );
    println!(
        "Loop:     {} bars = {} samples = {:.2} s, {} beats",
        opts.bars,
        loop_len_nominal,
        timeline.samples_to_secs(loop_len_nominal),
        opts.bars * 4
    );
    println!(
        "Checking: --latency-frames {} ({:.2} ms)",
        opts.latency_frames,
        opts.latency_frames as f64 * 1000.0 / rate as f64
    );
    println!("Runs:     {}", opts.runs);
    println!(
        "Measuring track {} \"{}\" on input {} (of {} channels).",
        target + 1,
        defs[target].name,
        measure_channel + 1,
        in_channels
    );
    if defs.len() > 1 {
        let others: Vec<String> = defs
            .iter()
            .enumerate()
            .filter(|(i, _)| *i != target)
            .map(|(i, d)| format!("{} {}", i + 1, d.name))
            .collect();
        println!(
            "Measure other tracks with --for-track: {}. Every source with its own path needs its\n\
             own value - a plugin host through an ASIO router has no converter in the way and\n\
             arrives earlier than a microphone.",
            others.join(", ")
        );
    }
    println!(
        "\nSetup: cable from output 1 back into input {}, direct monitor on the interface off.\n\
         If the source comes in through an ASIO router, no cable is needed - the router\n\
         loops the output back into this input itself.\n\
         The engine plays its own click through the output and records it again.\n\
         Take off your headphones or turn them down first.\n\
         Set the level high enough: the onset detection depends on the level; below a loop\n\
         peak of {} it measures the level along with the latency.\n",
        measure_channel + 1,
        fmt_dbfs(GOOD_PEAK)
    );

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

    // One track on the measured channel, the one the loopback feeds. The click starts switched
    // off: the noise floor has to be measured in silence, and it is switched on by command as soon
    // as the threshold is known.
    let core = EngineCore::new(EngineConfig {
        timeline,
        latency_frames: opts.latency_frames,
        // The input callback below feeds the FIFO with the measured channel only, so the engine
        // sees one channel per frame whichever input is being calibrated.
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
        // Both buses at unity on the same output pair. The click lives on the monitor bus, and the
        // loopback cable is in output 1 - with the ordinary four-output default the cable would
        // hear nothing and the measurement would report an absence of signal as a latency. A bus
        // volume other than 1.0 would likewise walk straight into the level dependency the onset
        // detection has, which is the one thing this measurement already has to be careful about.
        bus_gain: [1.0, 1.0],
        routing: measurement_routing(out_channels),
        output_channels: out_channels,
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
                // `measure_channel` was checked against the device's channel count while the
                // tracks were resolved, so this cannot be out of range.
                let s = frame[measure_channel];
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
    let mut out_scratch = vec![0.0f32; scratch_frames * BUS_SAMPLES];
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
                core.process(&in_scratch[..got], &mut out_scratch[..n * BUS_SAMPLES]);
                let routing = core.routing();
                let base = done * out_channels;
                for (i, frame) in data[base..base + n * out_channels]
                    .chunks_exact_mut(out_channels)
                    .enumerate()
                {
                    place_buses(&out_scratch[i * BUS_SAMPLES..(i + 1) * BUS_SAMPLES], frame, &routing);
                }
                done += n;
            }
        },
        move |err| sh_out_err.count_error(err),
    )?;

    if let Ok(frames) = input.buffer_size() {
        println!("Driver buffer: input {frames} frames");
    }
    if let Ok(frames) = output.buffer_size() {
        println!("Driver buffer: output {frames} frames");
    }

    // Input first, then output - the same order the 827 samples of phase 0 were measured in.
    input
        .play()
        .map_err(|e| format!("Input stream does not start: {e}"))?;
    output
        .play()
        .map_err(|e| format!("Output stream does not start: {e}"))?;

    // ---- a) noise floor -------------------------------------------------------------------
    println!("\nMeasuring noise floor ({NOISE_SECONDS:.1} s, click still off) ...");
    std::thread::sleep(Duration::from_secs_f64(NOISE_SECONDS));
    shared.measure_noise.store(false, Ordering::Relaxed);
    let noise_peak = f32::from_bits(shared.noise_peak.load(Ordering::Relaxed));
    let noise_frames = shared.noise_frames.load(Ordering::Relaxed);
    if noise_frames == 0 {
        return Err(
            "The input stream delivered not a single callback in 0.5 s - check the device or \
             driver."
                .to_string(),
        );
    }
    let noise_rms =
        ((shared.noise_sq_sum.load(Ordering::Relaxed) as f64 / SQ_SCALE) / noise_frames as f64)
            .sqrt() as f32;
    println!(
        "Noise floor: RMS {} , peak {}",
        fmt_dbfs(noise_rms),
        fmt_dbfs(noise_peak)
    );
    if noise_peak > MAX_NOISE_PEAK {
        return Err(format!(
            "Input too loud or feedback - check the gain (peak {:.1} dBFS, limit {:.1} dBFS).",
            dbfs(noise_peak),
            dbfs(MAX_NOISE_PEAK)
        ));
    }

    let threshold = (NOISE_FACTOR * noise_peak).max(MIN_THRESHOLD);
    if threshold > MAX_THRESHOLD {
        return Err(format!(
            "Noise floor too high: the detection threshold would be {:.3} ({:.1} dBFS), above \
             what the click still reaches after the cable. Turn the gain down or check the \
             cable.",
            threshold,
            dbfs(threshold)
        ));
    }
    println!(
        "Detection threshold: {:.4} ({})",
        threshold,
        fmt_dbfs(threshold)
    );

    // One click tone is 30 ms long; twice that is a safe blanking time and still only a tenth of a
    // beat at 100 BPM, so a stray detection between two clicks stays visible instead of being
    // swallowed. Never more than half a beat, so a slow tempo cannot mask the next click.
    let tone_len = Metronome::new(rate).tone_len();
    let blanking = (2 * tone_len).min((timeline.samples_per_beat() / 2.0) as u64).max(1);
    println!("Blanking time after an onset: {blanking} samples\n");

    cmd_tx.send(Command::SetClick { on: true })?;

    // ---- b) the runs ----------------------------------------------------------------------
    let mut last: Option<(Status, Instant)> = None;
    let mut all_deviations: Vec<f64> = Vec::new();
    let mut total_expected = 0usize;
    let mut total_matched = 0usize;
    let mut total_spurious = 0usize;
    let mut run_means: Vec<f64> = Vec::new();
    // Quietest loop of all runs - the one that decides whether the result is a latency or a level.
    let mut worst_peak = f32::INFINITY;
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

        let ahead = (end + opts.latency_frames).saturating_sub(est);
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
                "The returned buffer of {} samples is shorter than the loop ({} samples).",
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
            "----- Run {run} of {} (bar {} to {}, {} samples) -----",
            opts.runs,
            bar + 1,
            bar + opts.bars as u64,
            loop_len
        );
        println!(
            "  Clicks detected: {} of {} expected, {} off the grid, loop peak {}",
            analysis.matched,
            analysis.expected,
            analysis.spurious,
            fmt_dbfs(analysis.peak)
        );
        if !analysis.conclusive() {
            println!(
                "  WARNING: too few clicks detected. This run is NO proof of a correct\n\
                 \x20 calibration, but a sign of a level that is too low, a missing loopback\n\
                 \x20 cable or an empty loop."
            );
        } else if analysis.peak < GOOD_PEAK {
            println!("{}", low_peak_warning(analysis.peak));
        }
        if analysis.deviations.is_empty() {
            println!("  No deviation to compute.\n");
        } else {
            print_stats("  Deviation", &analysis.deviations, rate);
            println!();
        }

        total_expected += analysis.expected;
        total_matched += analysis.matched;
        total_spurious += analysis.spurious;
        worst_peak = worst_peak.min(analysis.peak);
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
    println!("===== Result =====");
    println!(
        "Clicks detected: {total_matched} of {total_expected} expected ({} off the grid).",
        total_spurious
    );
    let overruns = shared.overruns.load(Ordering::Relaxed);
    println!(
        "Xruns {}, FIFO empty {}, FIFO overflowed {overruns}, other errors {}.",
        shared.xruns.load(Ordering::Relaxed),
        shared.underruns.load(Ordering::Relaxed),
        shared.errors.load(Ordering::Relaxed)
    );
    if overruns > 0 {
        println!(
            "WARNING: the input FIFO overflowed. Input samples get lost that way, and that\n\
             shifts every later position. The measurement is unusable - please repeat it."
        );
    }

    if total_matched == 0 {
        return Err(
            "Not a single click was detected in any run. That is NOT a perfect calibration but\n\
             an empty recording. Check: loopback cable plugged in (output 1 into input 1), input\n\
             gain high enough, output level not at zero, the right device chosen."
                .to_string(),
        );
    }

    println!();
    print_stats("Deviation over all runs", &all_deviations, rate);
    if run_means.len() > 1 {
        println!();
        print_stats("Mean per run", &run_means, rate);
    }

    let conclusive = total_expected > 0
        && (total_matched as f64) >= total_expected as f64 * MIN_DETECTION_RATIO;
    let mean = Stats::of(&all_deviations)
        .map(|s| s.mean)
        .expect("mindestens ein Messwert, sonst waere oben abgebrochen worden");
    let recommended = (opts.latency_frames as f64 + mean).round().max(0.0) as u64;
    let name = &defs[target].name;

    println!("\n{}", verdict(mean));
    if !conclusive {
        println!(
            "WARNING: far fewer clicks were detected than expected ({total_matched} of\n\
             {total_expected}). The numbers below rest on too few hits - fix the level or the\n\
             cabling first and measure again."
        );
    }
    if worst_peak.is_finite() && worst_peak < GOOD_PEAK {
        println!("{}", low_peak_warning(worst_peak));
    }
    println!(
        "\nCurrently {} frames, mean deviation {:+.1} frames ({:+.3} ms).",
        opts.latency_frames,
        mean,
        mean * 1000.0 / rate as f64
    );
    println!(
        "Recommendation for track {} \"{name}\" on input {}: {recommended} frames\n\
         (Derivation: the recorded click lands R_true - R away from the grid, so\n\
         R_true = R + deviation. Recorded too late means: too little subtracted.)",
        target + 1,
        measure_channel + 1
    );
    println!(
        "\nApply with:\n  looper-engine live {} --bpm {} {} --track-latency {name}:{recommended}",
        device_flags(dev),
        opts.bpm,
        track_flags(&defs),
    );
    if defs.len() == 1 || target == 0 {
        println!(
            "  ... or as the global default for all sources on the same converter:\n\
             \x20 looper-engine live {} --bpm {} --latency-frames {recommended}",
            device_flags(dev),
            opts.bpm
        );
    }
    println!(
        "  looper-engine calibrate {} --bpm {} --bars {} {} --for-track {} --latency-frames {recommended}   (to double-check)",
        device_flags(dev),
        opts.bpm,
        opts.bars,
        track_flags(&defs),
        target + 1
    );
    println!(
        "\nNote: the onset detection only sees the click once it crosses the threshold, so\n\
         systematically a few frames late. Deviations below {:.0} frames ({:.2} ms) are\n\
         therefore measurement noise, not a reason to correct.",
        GOOD_ENOUGH_SAMPLES,
        GOOD_ENOUGH_SAMPLES * 1000.0 / rate as f64
    );
    println!(
        "What was measured here is the path from the output to this input. Latency inside an\n\
         external host (Cantabile and the like) - its own ASIO buffer, the delay its plugins\n\
         report - is not on this path and cannot be measured from outside. That is what the\n\
         manual surcharge is for: --track-latency {name}:{recommended}+<surcharge>.\n\
         The surcharge stays as it is at the next calibration; only the number in front of it \
         is measured."
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

    /// The track selection, which is what makes a measurement per input possible at all. Default
    /// is the first track, i.e. exactly what this command did when there was one global number.
    #[test]
    fn the_measured_track_defaults_to_the_first_one_and_names_the_others_when_it_is_wrong() {
        let defs = resolve_tracks(
            &["stimme:1".to_string(), "cantabile:3-4".to_string()],
            4,
        )
        .expect("Tracks");
        assert_eq!(pick_track(&defs, 1).unwrap(), 0, "Standard ist das alte Verhalten");
        assert_eq!(pick_track(&defs, 2).unwrap(), 1);
        // A stereo track is measured on its left input; both halves share one converter.
        assert_eq!(defs[1].input.first(), 2);

        for wrong in [0usize, 3, 99] {
            let err = pick_track(&defs, wrong).expect_err("gibt es nicht");
            assert!(err.contains("cantabile"), "die Meldung zaehlt auf: {err}");
            assert!(err.contains("input 3+4"), "{err}");
        }

        // The command line the recommendation prints has to describe the same tracks again, in the
        // spelling `live` parses - a stereo pair with a dash, not a plus.
        assert_eq!(
            track_flags(&defs),
            "--track stimme:1 --track cantabile:3-4"
        );
    }

    /// The trap the architecture document warns about, in the output: a quiet loop has to say so
    /// with the numbers that make it convincing.
    #[test]
    fn a_quiet_loop_is_called_out_with_the_measurement_behind_the_rule() {
        let text = low_peak_warning(0.045); // about -27 dBFS
        assert!(text.contains("depends on the level"), "{text}");
        assert!(text.contains("32 samples"), "{text}");
        assert!(text.contains("7.2"), "{text}");
        assert!(text.contains("--click-gain"), "{text}");
        // -18 dBFS is the line, and the constant really is that.
        assert!((dbfs(GOOD_PEAK) + 18.0).abs() < 0.1, "{}", dbfs(GOOD_PEAK));
        assert!(0.045 < GOOD_PEAK && 0.2 > GOOD_PEAK);
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
