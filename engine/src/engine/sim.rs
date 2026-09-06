//! Offline audio run: drives [`EngineCore`] with synthetic buffers exactly the way the driver
//! would, so sample accuracy can be proven without opening a device.
//!
//! The simulated device is a pure delay line: whatever the engine writes at output position `P`
//! reappears at input index `P + roundtrip`, on top of whatever the simulated musician plays.
//! That is the entire physical model, and it is enough - phase 0 measured the roundtrip as a
//! constant with a standard deviation of exactly zero, which is precisely what a delay line is.
//!
//! Input and output indices advance in lockstep here (the input FIFO of the real program adds a
//! constant transit of its own; see `live.rs`), so `input index == output position`.

use super::command::{
    BufferChannel, Command, CommandSender, Status, StatusReceiver, buffer_channel,
    command_channel, status_channel,
};
use super::process::{EngineConfig, EngineCore, loop_capacity};
use super::timeline::{TimeSignature, Timeline};

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
    pub monitor: bool,
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
            monitor: false,
        }
    }
}

pub struct Sim {
    pub core: EngineCore,
    commands: CommandSender,
    status: StatusReceiver,
    buffers: BufferChannel,
    /// Signal the simulated musician produces, indexed by absolute input index.
    played: Box<dyn Fn(u64) -> f32>,
    roundtrip: u64,
    block: usize,
    pos: u64,
    in_buf: Vec<f32>,
    out_buf: Vec<f32>,
    /// Everything the engine has written, indexed by output position.
    pub out_history: Vec<f32>,
}

impl Sim {
    pub fn new(spec: SimSpec, played: Box<dyn Fn(u64) -> f32>) -> Self {
        let timeline = Timeline::new(spec.sample_rate, spec.bpm, spec.signature);
        let capacity = loop_capacity(&timeline, spec.bars) as usize;
        let (commands, command_rx) = command_channel(256);
        let (status_tx, status) = status_channel(4096);
        let (buffers, endpoint) = buffer_channel(4);
        let core = EngineCore::new(EngineConfig {
            timeline,
            latency_samples: spec.latency,
            buffer: vec![0.0; capacity],
            commands: command_rx,
            status: status_tx,
            buffers: endpoint,
            monitor: spec.monitor,
            monitor_gain: 1.0,
            click: spec.click,
            click_gain: 1.0,
            status_interval: spec.block as u64,
        });
        Self {
            core,
            commands,
            status,
            buffers,
            played,
            roundtrip: spec.roundtrip,
            block: spec.block,
            pos: 0,
            in_buf: vec![0.0; spec.block],
            out_buf: vec![0.0; spec.block],
            out_history: Vec::new(),
        }
    }

    pub fn send(&mut self, cmd: Command) {
        self.commands.send(cmd).expect("Kommando-Queue voll");
    }

    pub fn latest_status(&mut self) -> Option<Status> {
        self.status.latest()
    }

    /// Hand a freshly allocated loop buffer to the engine, the way the control thread does when
    /// the tempo or the number of bars changes.
    pub fn install_buffer(&mut self, samples: usize) {
        self.buffers
            .install(vec![0.0; samples])
            .expect("Puffer-Queue voll");
    }

    /// Drop whatever the engine handed back.
    pub fn drain_retired(&mut self) {
        self.buffers.drain_retired();
    }

    /// One driver callback.
    pub fn step(&mut self) {
        for j in 0..self.block {
            let k = self.pos + j as u64;
            let mut value = (self.played)(k);
            if k >= self.roundtrip {
                let idx = (k - self.roundtrip) as usize;
                if idx < self.out_history.len() {
                    value += self.out_history[idx];
                }
            }
            self.in_buf[j] = value;
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

    pub fn pos(&self) -> u64 {
        self.pos
    }
}
