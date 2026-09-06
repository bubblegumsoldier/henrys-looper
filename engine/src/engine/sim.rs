//! Offline audio run: drives [`EngineCore`] with synthetic buffers exactly the way the driver
//! would, so sample accuracy can be proven without opening a device.
//!
//! The simulated device is a pure delay line: whatever the engine writes at output position `P`
//! reappears on the loopback input channel at input index `P + roundtrip`, on top of whatever the
//! simulated musician plays. That is the entire physical model, and it is enough - phase 0 measured
//! the roundtrip as a constant with a standard deviation of exactly zero, which is precisely what a
//! delay line is. The cable is fed from the **left** bus channel, as a real cable is plugged into
//! output 1.
//!
//! Input and output indices advance in lockstep here (the input FIFO of the real program adds a
//! constant transit of its own; see `live.rs`), so `input index == output position`. Both count
//! frames; the output history is kept as two de-interleaved channels so a test can name a side.
//!
//! The simulation also plays the *control* thread: [`Sim::step`] services the [`LayerPool`] before
//! every block, exactly as the main loop of `live.rs` does. That is what makes the buffer traffic
//! testable - a leaked layer shows up as a missing reclaim.

use super::command::{
    Command, CommandSender, LayerPool, Status, StatusReceiver, buffer_channel, command_channel,
    status_channel,
};
use super::frame::{Channels, Frame, TrackInput};
use super::process::{
    EngineConfig, EngineCore, OUT_CHANNELS, loop_capacity, spare_slots_for,
};
use super::timeline::{TimeSignature, Timeline};
use super::track::{MAX_LAYERS, Track, TrackLatency};

/// One simulated track: which input it records, whether it monitors from the start, where it sits
/// in the stereo field, and what it compensates.
#[derive(Clone, Copy, Debug)]
pub struct TrackSpec {
    pub input: TrackInput,
    pub monitor: bool,
    pub pan: f32,
    /// Latency compensation of this track. The default follows the engine's global value, which is
    /// what every test written before the per-track value expects.
    pub latency: TrackLatency,
}

impl TrackSpec {
    /// A mono track on one input channel, centred, not monitoring.
    pub fn on(channel: usize) -> Self {
        Self {
            input: TrackInput::Mono(channel),
            monitor: false,
            pan: 0.0,
            latency: TrackLatency::INHERITED,
        }
    }

    /// A stereo track on a pair of input channels, centred, not monitoring.
    pub fn stereo(left: usize, right: usize) -> Self {
        Self {
            input: TrackInput::Stereo { left, right },
            monitor: false,
            pan: 0.0,
            latency: TrackLatency::INHERITED,
        }
    }

    /// This track compensates its own measured number instead of the engine's default.
    pub fn compensating(mut self, frames: u32) -> Self {
        self.latency = TrackLatency::measured(frames);
        self
    }

    /// A manual surcharge on top of whatever this track's base is.
    pub fn trimmed(mut self, trim: i32) -> Self {
        self.latency.trim = trim;
        self
    }

    pub fn monitoring(mut self) -> Self {
        self.monitor = true;
        self
    }

    pub fn panned(mut self, pan: f32) -> Self {
        self.pan = pan;
        self
    }

    pub fn channels(self) -> Channels {
        self.input.channels()
    }
}

pub struct SimSpec {
    pub sample_rate: u32,
    pub bpm: f64,
    pub signature: TimeSignature,
    pub bars: u32,
    /// What the engine compensates by default, in frames. A track can override it - see
    /// [`TrackSpec::compensating`].
    pub latency: u64,
    /// What the simulated hardware actually does. Normally the same as `latency`; making them
    /// differ is how a test can show that a wrong value really does misalign the recording.
    pub roundtrip: u64,
    /// Frames per callback.
    pub block: usize,
    pub click: bool,
    pub tracks: Vec<TrackSpec>,
    pub input_channels: usize,
    /// Input channel the simulated loopback cable feeds, as the real cable feeds input 1.
    /// `None` means no cable: the input carries only what the musician plays. That is the normal
    /// case for a looper - a cable from the output back into a recorded channel is a feedback loop,
    /// and the phase 0 latency measurement is the one situation it belongs in.
    pub loopback_channel: Option<usize>,
    /// Layer length in frames. `None` means "as long as the loop needs".
    pub layer_frames: Option<u64>,
    /// Prepared buffers the control thread keeps in flight, per kind that is actually used.
    pub spare_slots: usize,
}

impl Default for SimSpec {
    fn default() -> Self {
        Self {
            sample_rate: 48_000,
            bpm: 100.0,
            signature: TimeSignature::new(4, 4),
            bars: 8,
            latency: 827,
            roundtrip: 827,
            block: 128,
            click: false,
            tracks: vec![TrackSpec::on(0)],
            input_channels: 1,
            loopback_channel: Some(0),
            layer_frames: None,
            spare_slots: 3,
        }
    }
}

/// A signal for one input channel: `(frame index, channel) -> sample`.
pub type Played = Box<dyn Fn(u64, usize) -> f32>;

/// Wrap a mono signal so it appears on channel 0 and nowhere else.
pub fn mono(f: impl Fn(u64) -> f32 + 'static) -> Played {
    Box::new(move |k, ch| if ch == 0 { f(k) } else { 0.0 })
}

pub fn silence() -> Played {
    Box::new(|_, _| 0.0)
}

pub struct Sim {
    pub core: EngineCore,
    commands: CommandSender,
    status: StatusReceiver,
    pool: LayerPool,
    last_status: Option<Status>,
    /// Signal the simulated musician produces, per input frame and channel.
    played: Played,
    roundtrip: u64,
    loopback_channel: Option<usize>,
    input_channels: usize,
    block: usize,
    pos: u64,
    in_buf: Vec<f32>,
    out_buf: Vec<f32>,
    /// Everything the engine has written to the left bus channel, indexed by output position.
    pub out_l: Vec<f32>,
    /// The same for the right one.
    pub out_r: Vec<f32>,
}

impl Sim {
    pub fn new(spec: SimSpec, played: Played) -> Self {
        let timeline = Timeline::new(spec.sample_rate, spec.bpm, spec.signature);
        let layer_frames = spec
            .layer_frames
            .unwrap_or_else(|| loop_capacity(&timeline, spec.bars));
        let (commands, command_rx) = command_channel(256);
        let (status_tx, status) = status_channel(4096);
        let track_count = spec.tracks.len();
        let kinds: Vec<Channels> = spec.tracks.iter().map(|t| t.channels()).collect();
        let slots = spare_slots_for(&kinds, spec.spare_slots);
        let (channel, endpoint) = buffer_channel(
            slots[0] + slots[1] + 8,
            track_count * MAX_LAYERS + slots[0] + slots[1] + 8,
        );
        let mut pool = LayerPool::new(channel, layer_frames as usize, slots);
        // Fill the stock before the engine exists, the way the control thread does at startup.
        pool.service(None);

        let tracks: Vec<Track> = spec
            .tracks
            .iter()
            .map(|t| {
                Track::new(t.input, t.monitor, t.pan, spec.sample_rate).with_latency(t.latency)
            })
            .collect();
        let core = EngineCore::new(EngineConfig {
            timeline,
            latency_frames: spec.latency,
            input_channels: spec.input_channels,
            tracks,
            spares: [
                Vec::with_capacity(slots[0]),
                Vec::with_capacity(slots[1]),
            ],
            layer_frames,
            commands: command_rx,
            status: status_tx,
            buffers: endpoint,
            monitor_gain: 1.0,
            click: spec.click,
            click_gain: 1.0,
            status_interval: spec.block as u64,
        });
        Self {
            core,
            commands,
            status,
            pool,
            last_status: None,
            played,
            roundtrip: spec.roundtrip,
            loopback_channel: spec.loopback_channel,
            input_channels: spec.input_channels,
            block: spec.block,
            pos: 0,
            in_buf: vec![0.0; spec.block * spec.input_channels],
            out_buf: vec![0.0; spec.block * OUT_CHANNELS],
            out_l: Vec::new(),
            out_r: Vec::new(),
        }
    }

    pub fn send(&mut self, cmd: Command) {
        self.commands.send(cmd).expect("Kommando-Queue voll");
    }

    /// Newest status snapshot, also remembered for the pool accounting.
    pub fn latest_status(&mut self) -> Option<Status> {
        if let Some(s) = self.status.latest() {
            self.last_status = Some(s);
        }
        self.last_status
    }

    /// Buffers the control thread has got back from the engine.
    pub fn reclaimed(&self) -> u64 {
        self.pool.reclaimed()
    }

    /// The same, for one kind of buffer only.
    pub fn reclaimed_of(&self, channels: Channels) -> u64 {
        self.pool.reclaimed_of(channels)
    }

    /// Buffers the control thread has handed to the engine.
    pub fn pushed(&self) -> u64 {
        self.pool.pushed()
    }

    pub fn pushed_of(&self, channels: Channels) -> u64 {
        self.pool.pushed_of(channels)
    }

    /// What the control thread does after a `SetTempo`: from now on it allocates at the new length.
    pub fn set_layer_frames(&mut self, layer_frames: usize) {
        self.pool.set_layer_frames(layer_frames);
    }

    /// Both bus channels at one output position.
    pub fn out(&self, pos: u64) -> Frame {
        Frame::new(self.out_l[pos as usize], self.out_r[pos as usize])
    }

    /// One driver callback, preceded by one turn of the control thread.
    pub fn step(&mut self) {
        if let Some(s) = self.status.latest() {
            self.last_status = Some(s);
        }
        self.pool.service(self.last_status.as_ref());

        let ch = self.input_channels;
        for j in 0..self.block {
            let k = self.pos + j as u64;
            for c in 0..ch {
                let mut value = (self.played)(k, c);
                if self.loopback_channel == Some(c) && k >= self.roundtrip {
                    let idx = (k - self.roundtrip) as usize;
                    if idx < self.out_l.len() {
                        // A cable is plugged into output 1, i.e. the left bus channel.
                        value += self.out_l[idx];
                    }
                }
                self.in_buf[j * ch + c] = value;
            }
        }
        self.core.process(&self.in_buf, &mut self.out_buf);
        for frame in self.out_buf.chunks_exact(OUT_CHANNELS) {
            self.out_l.push(frame[0]);
            self.out_r.push(frame[1]);
        }
        self.pos += self.block as u64;
    }

    /// Run until the engine has produced at least `pos` frames.
    pub fn run_to(&mut self, pos: u64) {
        while self.pos < pos {
            self.step();
        }
    }

    /// Run `blocks` further callbacks.
    pub fn run_blocks(&mut self, blocks: u64) {
        let target = self.pos + blocks * self.block as u64;
        self.run_to(target);
    }

    pub fn pos(&self) -> u64 {
        self.pos
    }
}
