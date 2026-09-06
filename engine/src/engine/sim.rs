//! Offline audio run: drives [`EngineCore`] with synthetic buffers exactly the way the driver
//! would, so sample accuracy can be proven without opening a device.
//!
//! The simulated device is a pure delay line: whatever the engine writes at output position `P`
//! reappears on the loopback input channel at input index `P + roundtrip`, on top of whatever the
//! simulated musician plays. That is the entire physical model, and it is enough - phase 0 measured
//! the roundtrip as a constant with a standard deviation of exactly zero, which is precisely what a
//! delay line is.
//!
//! Input and output indices advance in lockstep here (the input FIFO of the real program adds a
//! constant transit of its own; see `live.rs`), so `input index == output position`.
//!
//! The simulation also plays the *control* thread: [`Sim::step`] services the [`LayerPool`] before
//! every block, exactly as the main loop of `live.rs` does. That is what makes the buffer traffic
//! testable - a leaked layer shows up as a missing reclaim.

use super::command::{
    Command, CommandSender, LayerPool, Status, StatusReceiver, buffer_channel, command_channel,
    status_channel,
};
use super::process::{EngineConfig, EngineCore, loop_capacity};
use super::timeline::{TimeSignature, Timeline};
use super::track::{MAX_LAYERS, Track};

/// One simulated track: which input channel it records, and whether it monitors from the start.
#[derive(Clone, Copy, Debug)]
pub struct TrackSpec {
    pub channel: usize,
    pub monitor: bool,
}

impl TrackSpec {
    pub fn on(channel: usize) -> Self {
        Self {
            channel,
            monitor: false,
        }
    }
}

pub struct SimSpec {
    pub sample_rate: u32,
    pub bpm: f64,
    pub signature: TimeSignature,
    pub bars: u32,
    /// What the engine compensates for.
    pub latency: u64,
    /// What the simulated hardware actually does. Normally the same as `latency`; making them
    /// differ is how a test can show that a wrong value really does misalign the recording.
    pub roundtrip: u64,
    pub block: usize,
    pub click: bool,
    pub tracks: Vec<TrackSpec>,
    pub input_channels: usize,
    /// Input channel the simulated loopback cable feeds, as the real cable feeds input 1.
    /// `None` means no cable: the input carries only what the musician plays. That is the normal
    /// case for a looper - a cable from the output back into a recorded channel is a feedback loop,
    /// and the phase 0 latency measurement is the one situation it belongs in.
    pub loopback_channel: Option<usize>,
    /// Layer length in samples. `None` means "as long as the loop needs".
    pub layer_capacity: Option<u64>,
    /// Prepared buffers the control thread keeps in flight.
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
            layer_capacity: None,
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
    /// Everything the engine has written, indexed by output position.
    pub out_history: Vec<f32>,
}

impl Sim {
    pub fn new(spec: SimSpec, played: Played) -> Self {
        let timeline = Timeline::new(spec.sample_rate, spec.bpm, spec.signature);
        let layer_capacity = spec
            .layer_capacity
            .unwrap_or_else(|| loop_capacity(&timeline, spec.bars));
        let (commands, command_rx) = command_channel(256);
        let (status_tx, status) = status_channel(4096);
        let track_count = spec.tracks.len();
        let (channel, endpoint) = buffer_channel(
            spec.spare_slots + 4,
            track_count * MAX_LAYERS + spec.spare_slots + 8,
        );
        let mut pool = LayerPool::new(channel, layer_capacity as usize, spec.spare_slots);
        // Fill the stock before the engine exists, the way the control thread does at startup.
        pool.service(None);

        let tracks: Vec<Track> = spec
            .tracks
            .iter()
            .map(|t| Track::new(t.channel, t.monitor, spec.sample_rate))
            .collect();
        let core = EngineCore::new(EngineConfig {
            timeline,
            latency_samples: spec.latency,
            input_channels: spec.input_channels,
            tracks,
            spares: Vec::with_capacity(spec.spare_slots),
            layer_capacity,
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
            out_buf: vec![0.0; spec.block],
            out_history: Vec::new(),
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

    /// Buffers the control thread has handed to the engine.
    pub fn pushed(&self) -> u64 {
        self.pool.pushed()
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
                    if idx < self.out_history.len() {
                        value += self.out_history[idx];
                    }
                }
                self.in_buf[j * ch + c] = value;
            }
        }
        self.core.process(&self.in_buf, &mut self.out_buf);
        self.out_history.extend_from_slice(&self.out_buf);
        self.pos += self.block as u64;
    }

    /// Run until the engine has produced at least `pos` samples.
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
