//! The performance again: the score's runner driving the live engine, offline, with the input
//! the musician played - reconstructed from the dry stems.
//!
//! # Why a re-performance
//!
//! Between a track's dry stem and its stem sit gains the dry stem does not carry: the monitor
//! fade of the score (`monitor_fade`), the hand-off between tracks sharing an input (`monitor:
//! always` giving way), the edge fades of monitoring and takes, the monitor level - and the live
//! input reaches the chain at the moment it arrived (one roundtrip late), not on the grid. Every
//! one of them is decided by the runner's commands and the engine's state, frame by frame. So the
//! render does not compute them: it runs the runner over the recording's own score and timeline
//! against a real [`EngineCore`] with every track, and lets the engine apply them - and the
//! recorder (`crate::capture`) tap the stems and the mix, exactly where it tapped them live.
//!
//! # Where the input comes from
//!
//! The recorder put into each track's dry stem, at musical position `m`:
//!
//! ```text
//! dry(m) = loop playback(m) + w(k) * input(k),   k = m + R   (the input frame that arrived at k)
//! ```
//!
//! with `w` the weight the recorder gave the live input (1 while the track records or monitors,
//! faded at the edges, 0 otherwise). `w` and the loop playback are the engine's own: `w` depends
//! only on the commands, not on the signal, so a first pass with silence ([`Pass::Weights`])
//! collects it for every frame ([`crate::engine::process::Probe`]); the loop playback is what the
//! re-performance itself plays at `m` - its layers were recorded from the same reconstructed
//! input. Then, for every input frame `k` and channel, from the track on that channel that hears
//! it best (largest `w`):
//!
//! ```text
//! input(k) = (dry(m) - loop playback(m)) / w(k)
//! ```
//!
//! Where no track on a channel had `w > 0`, nothing of that input reached any file or any
//! speaker, and it is silence. The re-performance's own dry stems are checked against the
//! recorded ones - that is the proof that the reconstruction holds.

use crate::capture::{CaptureSpec, Layout, capture_channel, capture_pair};
use crate::engine::bus::{BUS_COUNT, BUS_SAMPLES, BusRouting};
use crate::engine::command::{
    Command, CommandSender, LayerPool, Status, StatusReceiver, buffer_channel, command_channel,
    status_channel,
};
use crate::engine::frame::{Channels, Frame, TrackInput};
use crate::engine::insert::{Insert, InsertMsg, InsertSender, InsertTarget, insert_channel};
use crate::engine::process::{EngineConfig, EngineCore, loop_capacity, spare_slots_for};
use crate::engine::runner::{Runner, setup_commands};
use crate::engine::timeline::{TimeSignature, Timeline};
use crate::engine::track::{MAX_LAYERS, Track, TrackLatency};
use crate::score::CompiledScore;
use crate::score::mixer::mixer_commands;

/// How the performance is set up. Everything here comes from the recording, except the two
/// settings `session.json` does not hold (see `outputs` and `monitor_gain`).
#[derive(Clone, Debug)]
pub struct PerformSpec {
    /// The recording's score - its tracks, sections and timing - with the `mixer:` (and the
    /// tracks' pan) of the score being rendered.
    pub score: CompiledScore,
    pub sample_rate: u32,
    /// Frames of the recording.
    pub frames: u64,
    /// The live engine position of frame 0 (`engine_position`): only its phase against the block
    /// matters - blocks start where they started live.
    pub first_abs: u64,
    pub count_in_bars: u32,
    /// Each track's `R`, from `session.json`.
    pub latency: Vec<u64>,
    pub block: usize,
    /// Output channels of the device: 2 means Main and Monitor leave on one pair - one mix, so a
    /// monitored input reaches Main. Not stored in a recording.
    pub outputs: usize,
    /// The monitored inputs' level (the app's "inputs"), linear. Taken from the score's
    /// `mixer.buses.inputs` when it has one; otherwise not stored in a recording.
    pub monitor_gain: f32,
}

/// What one pass gives back.
#[derive(Default)]
pub struct Performed {
    /// First pass: per track, per file frame of the dry position `m`, the weight of the live input.
    pub weights: Vec<Vec<f32>>,
    /// Second pass: frames whose weight differed from the first pass's (should be none - the
    /// weights depend on the commands only).
    pub weight_mismatches: u64,
}

/// Where the second pass's input comes from.
pub enum Source<'a> {
    /// Reconstructed from a recording.
    Recording {
        /// Per track: the recorded dry stem, interleaved in the track's channel count (empty for
        /// an instrument track).
        dry: &'a [Vec<f32>],
        /// The weights of the first pass.
        weights: &'a [Vec<f32>],
    },
    /// A synthetic performance: `played(k, channel)` is device input channel `channel` at input
    /// index `k`. For the tests, which make recordings this way.
    Played(&'a dyn Fn(u64, usize) -> f32),
}

/// Which pass.
pub enum Pass<'a> {
    /// Silence in, the recorder's weights out.
    Weights,
    /// An input in, the recorder's files out.
    Audio {
        source: Source<'a>,
        /// Plugin inserts per track, slot and insert.
        inserts: Vec<Vec<(usize, Insert)>>,
        /// Called once per block - the host's plugin latency poll.
        poll: &'a mut dyn FnMut(),
        /// Every super-frame the recorder hands on, with its file frame (see
        /// [`crate::capture::Layout`]: `[mix L, R, track 0 stem L, R, track 0 dry (1|2), ...]`).
        sink: &'a mut dyn FnMut(u64, &[f32]),
    },
}

struct Engine {
    core: EngineCore,
    commands: CommandSender,
    status: StatusReceiver,
    pool: LayerPool,
    last: Option<Status>,
    inserts: Option<InsertSender>,
}

fn build(spec: &PerformSpec, timeline: Timeline) -> Result<Engine, String> {
    let score = &spec.score;
    let rate = spec.sample_rate;
    let max_bars = score.sections.iter().map(|s| s.bars).max().unwrap_or(1).max(1);
    let layer_frames = loop_capacity(&timeline, max_bars);
    let kinds: Vec<Channels> = score.tracks.iter().map(|t| t.track_input().channels()).collect();
    let slots = spare_slots_for(&kinds, 3);
    let n = score.tracks.len();
    let (channel, endpoint) =
        buffer_channel(slots[0] + slots[1] + 8, n * MAX_LAYERS + slots[0] + slots[1] + 8);
    let mut pool = LayerPool::new(channel, layer_frames as usize, slots);
    pool.service(None);
    let tracks: Vec<Track> = score
        .tracks
        .iter()
        .zip(&spec.latency)
        .map(|(t, &r)| {
            Track::new(t.track_input(), false, t.pan, rate)
                .with_latency(TrackLatency::measured(r as u32))
                .with_sends(t.loop_send(), t.monitor_send())
        })
        .collect();
    let input_channels = score
        .tracks
        .iter()
        .filter(|t| !t.is_instrument())
        .map(|t| t.track_input().highest() + 1)
        .max()
        .unwrap_or(1);
    let (commands, command_rx) = command_channel(4096);
    let (status_tx, status) = status_channel(64);
    let outputs = spec.outputs.max(2);
    let mut core = EngineCore::new(EngineConfig {
        timeline,
        latency_frames: spec.latency.first().copied().unwrap_or(0),
        input_channels,
        tracks,
        spares: [Vec::with_capacity(slots[0]), Vec::with_capacity(slots[1])],
        layer_frames,
        commands: command_rx,
        status: status_tx,
        buffers: endpoint,
        monitor_gain: spec.monitor_gain,
        click: false,
        click_gain: 0.0,
        bus_gain: [1.0; BUS_COUNT],
        routing: BusRouting::default_for(outputs),
        output_channels: outputs,
        status_interval: spec.block as u64,
    });
    // The app opens with the transport standing; the runner's start moves it.
    core.hold_transport();
    core.attach_probe();
    Ok(Engine {
        core,
        commands,
        status,
        pool,
        last: None,
        inserts: None,
    })
}

impl Engine {
    fn send(&mut self, cmd: Command) -> Result<(), String> {
        self.commands.send(cmd).map_err(|e| format!("engine command queue: {e:?}"))
    }
}

/// Where the runner is started (`warm` is the block it is started in, `est` what it is told the
/// position is) and where bar 1 of the first section - file frame 0 - then falls: on the same
/// phase of the audio block as live (`first_abs`), after a second of warm-up.
pub fn start_positions(spec: &PerformSpec) -> Result<(u64, u64, u64), String> {
    let block = spec.block.clamp(1, crate::engine::insert::MAX_BLOCK) as u64;
    let score = &spec.score;
    let timeline = Timeline::new(
        spec.sample_rate,
        score.bpm,
        TimeSignature::new(score.beats_per_bar, score.beat_unit),
    );
    let mut trial =
        Runner::new(score.clone(), timeline, block as u32)?.with_count_in(spec.count_in_bars);
    trial.start(0);
    let first_rel = trial.scheduler().timeline.bar_start(u64::from(spec.count_in_bars));
    let warm = (u64::from(spec.sample_rate) / block + 1) * block;
    let delta = (spec.first_abs % block + block - (warm + first_rel) % block) % block;
    let est = warm + delta;
    Ok((warm, est, est + first_rel))
}

/// Run the performance once. Returns the weights (first pass) or hands every recorded super-frame
/// to the sink (second pass).
pub fn perform(spec: &PerformSpec, pass: Pass<'_>) -> Result<Performed, String> {
    let score = &spec.score;
    let rate = spec.sample_rate;
    let block = spec.block.clamp(1, crate::engine::insert::MAX_BLOCK);
    let n = score.tracks.len();
    if spec.latency.len() != n {
        return Err("session.json and the score do not list the same tracks".to_string());
    }
    if let Some(r) = score
        .tracks
        .iter()
        .zip(&spec.latency)
        .filter(|(t, _)| !t.is_instrument())
        .map(|(_, r)| *r)
        .find(|r| *r < block as u64)
    {
        return Err(format!(
            "a track compensates only {r} frames, less than one block of {block}; the input cannot \
             be reconstructed a block ahead"
        ));
    }
    let timeline = Timeline::new(
        rate,
        score.bpm,
        TimeSignature::new(score.beats_per_bar, score.beat_unit),
    );
    let mut engine = build(spec, timeline)?;

    // Load: what the app sends when a score is loaded - setup, the recorded compensation, the mix.
    for cmd in setup_commands(score) {
        engine.send(cmd)?;
    }
    for (t, &r) in spec.latency.iter().enumerate() {
        if !score.tracks[t].is_instrument() {
            engine.send(Command::SetTrackLatency {
                track: t,
                latency: TrackLatency::measured(r as u32),
            })?;
        }
    }
    if let Some(mixer) = &score.mixer {
        let names: Vec<String> = score.tracks.iter().map(|t| t.name.clone()).collect();
        for cmd in mixer_commands(mixer, &names, &[]) {
            engine.send(cmd)?;
        }
    }

    let (warm, est, first) = start_positions(spec)?;
    let b = block as u64;
    let mut runner =
        Runner::new(score.clone(), timeline, block as u32)?.with_count_in(spec.count_in_bars);

    let frames = spec.frames;
    let mut out = Performed::default();
    if matches!(pass, Pass::Weights) {
        out.weights = vec![vec![0.0f32; frames as usize]; n];
    }
    let inputs = |t: usize| -> Vec<usize> {
        match score.tracks[t].track_input() {
            TrackInput::Mono(c) => vec![c],
            TrackInput::Stereo { left, right } => vec![left, right],
            TrackInput::Instrument => Vec::new(),
        }
    };
    let track_inputs: Vec<Vec<usize>> = (0..n).map(inputs).collect();
    let in_ch = score
        .tracks
        .iter()
        .filter(|t| !t.is_instrument())
        .map(|t| t.track_input().highest() + 1)
        .max()
        .unwrap_or(1);

    // The second pass: plugins, the recorder, and what it needs to reconstruct the input.
    let audio = matches!(pass, Pass::Audio { .. });
    let (mut reader, mut port) = (None, None);
    // The loop playback each track's dry stem got, kept for the last `ring` positions.
    let ring = (spec.latency.iter().copied().max().unwrap_or(0) + 4 * b).next_power_of_two() as usize;
    let mut loops = vec![vec![Frame::SILENT; ring]; n];
    let (source, mut poll, mut sink) = match pass {
        Pass::Weights => (None, None, None),
        Pass::Audio {
            source,
            inserts,
            poll,
            sink,
        } => {
            let any = inserts.iter().any(|v| !v.is_empty());
            if any {
                let (mut sender, endpoint) = insert_channel(256);
                engine.core.attach_inserts(endpoint);
                for (t, list) in inserts.into_iter().enumerate() {
                    for (slot, insert) in list {
                        sender.send(InsertMsg::Install {
                            target: InsertTarget::Track(t),
                            slot,
                            insert,
                        })?;
                    }
                }
                engine.inserts = Some(sender);
            }
            let (p, endpoint) = capture_channel();
            engine.core.attach_capture(endpoint);
            let (session, r) = capture_pair(&CaptureSpec {
                start: first,
                layout: Layout {
                    tracks: score.tracks.iter().map(|t| t.track_input().channels()).collect(),
                    dry: true,
                },
                sample_rate: rate,
                max_block: block,
                max_latency: spec.latency.iter().copied().max().unwrap_or(0),
                queue_seconds: 30.0,
            });
            let mut p = p;
            p.install(session)?;
            reader = Some(r);
            port = Some(p);
            (Some(source), Some(poll), Some(sink))
        }
    };
    let width = Layout {
        tracks: score.tracks.iter().map(|t| t.track_input().channels()).collect(),
        dry: true,
    }
    .width();

    let mut input = vec![0.0f32; block * in_ch];
    let mut output = vec![0.0f32; block * BUS_SAMPLES];
    let mut delivered = 0u64;
    let mut buf: Vec<f32> = Vec::new();
    let mut pos = 0u64;
    let mut started = false;
    // Enough to finish: the warm-up, the count-in, the take, the tail of the recorder's lag.
    let limit = first + frames + 30 * u64::from(rate);
    loop {
        if let Some(s) = engine.status.latest() {
            engine.last = Some(s);
        }
        engine.pool.service(engine.last.as_ref());
        if !started && pos >= warm {
            runner.start(est);
            started = true;
        }
        if started {
            runner.tick(pos);
        }
        for cmd in runner.take_commands() {
            engine.send(cmd)?;
        }
        if let Some(poll) = poll.as_mut() {
            poll();
        }

        // The input of this block.
        input.fill(0.0);
        let weights = match &source {
            Some(Source::Recording { weights, .. }) => Some(*weights),
            _ => None,
        };
        if let Some(Source::Played(played)) = &source {
            for i in 0..block {
                for c in 0..in_ch {
                    input[i * in_ch + c] = played(pos + i as u64, c);
                }
            }
        }
        if let Some(Source::Recording { dry, weights }) = &source {
            for i in 0..block {
                let k = pos + i as u64;
                for c in 0..in_ch {
                    // The track that hears this input best: one whose loop is silent there first
                    // (nothing to subtract, so nothing a wrong loop could spoil), then the largest
                    // weight.
                    let mut best: Option<(usize, usize, usize, f32, bool)> = None;
                    for t in 0..n {
                        let Some(j) = track_inputs[t].iter().position(|&x| x == c) else {
                            continue;
                        };
                        let Some(m) = k.checked_sub(spec.latency[t] + first) else {
                            continue;
                        };
                        if m >= frames {
                            continue;
                        }
                        let w = weights[t][m as usize];
                        if w <= 0.0 {
                            continue;
                        }
                        let quiet = loops[t][(m as usize + first as usize) & (ring - 1)].is_silent();
                        let better = match best {
                            None => true,
                            Some(b) => (quiet, w) > (b.4, b.3),
                        };
                        if better {
                            best = Some((t, j, m as usize, w, quiet));
                        }
                    }
                    if let Some((t, j, m, w, _)) = best {
                        let ch = track_inputs[t].len();
                        let d = dry[t].get(m * ch + j).copied().unwrap_or(0.0);
                        let l = loops[t][(m + first as usize) & (ring - 1)];
                        let lv = if j == 0 { l.l } else { l.r };
                        input[i * in_ch + c] = (d - lv) / w;
                    }
                }
            }
        }

        engine.core.process(&input, &mut output);

        let probe = engine.core.take_probe();
        for (t, p, f) in probe.loops {
            loops[t][p as usize & (ring - 1)] = f;
        }
        for (t, k, w) in probe.weights {
            if let Some(m) = k.checked_sub(spec.latency[t] + first)
                && m < frames
            {
                match (audio, weights) {
                    (false, _) => out.weights[t][m as usize] = w,
                    (true, Some(first_pass)) => {
                        if first_pass[t][m as usize] != w {
                            out.weight_mismatches += 1;
                        }
                    }
                    (true, None) => {}
                }
            }
        }
        if let Some(reader) = reader.as_mut() {
            loop {
                buf.clear();
                match reader.read(&mut buf, 4096) {
                    crate::capture::Read::Frames(got) => {
                        for f in 0..got {
                            if delivered < frames
                                && let Some(sink) = sink.as_mut()
                            {
                                sink(delivered, &buf[f * width..(f + 1) * width]);
                            }
                            delivered += 1;
                        }
                    }
                    _ => break,
                }
            }
        }
        if let Some(port) = port.as_mut() {
            port.collect();
        }
        pos += b;
        let done = if audio {
            delivered >= frames
        } else {
            pos >= first + frames + 4 * b + spec.latency.iter().copied().max().unwrap_or(0)
        };
        if done {
            break;
        }
        if pos > limit {
            return Err(format!(
                "the re-performance did not deliver the recording's {frames} frames (got {delivered})"
            ));
        }
    }
    if let Some(sender) = engine.inserts.as_mut() {
        sender.drain_retired();
    }
    drop(engine);
    Ok(out)
}
