//! One track's channel strip, run offline by the live engine itself.
//!
//! [`OfflineStrip`] is an [`EngineCore`] with exactly one track, driven block by block the way the
//! output callback drives it - no device, no thread, no clock. The dry signal goes in as that
//! track's **live input** with monitoring on at unity, which is a bit-exact way into the chain:
//!
//! * `monitor_frame` multiplies by a monitor gain of exactly 1.0 (no fade, no ramp), and
//!   `Track::render_sources` adds it to an empty loop sum (`0.0 + x = x`), so the chain sees the
//!   very samples the dry file holds - the same thing it saw live, where the loop playback came in
//!   through the layer sum instead;
//! * everything after that is the engine's own code: the per-frame path or, with plugin inserts,
//!   the block path with the chain split around them (`render_split_chain`), the fader with its
//!   smoother, the panner, the bus sum. Nothing here touches a sample.
//!
//! **Latency is made up the way the live engine makes it up on the loop.** Live, a track's loop is
//! read `read_ahead` frames ahead of the output position (octaver plus the plugins that are not
//! bypassed, [`EngineCore::read_ahead_of`]), so whatever the chain delays lands back on the grid.
//! The dry file holds the loop on its own grid (`capture::CaptureSession::track_frame`), so feeding
//! it from frame 0 and dropping the first `read_ahead` output frames is the same arithmetic.
//!
//! **The state before bar 1.** Before frame 0 the strip runs [`OfflineStrip::settle`] frames of
//! silence, so the fader and the effect switches are at rest and a chain that has been silent for
//! longer than its longest tail is idle - what a track that waited through the count-in is.

use crate::engine::bus::{BUS_COUNT, BUS_SAMPLES, BusRouting, BusSend};
use crate::engine::command::{
    Command, CommandSender, StatusReceiver, TrackStatus, buffer_channel, command_channel,
    status_channel,
};
use crate::engine::frame::{Channels, TrackInput};
use crate::engine::insert::{Insert, InsertMsg, InsertSender, InsertTarget, insert_channel};
use crate::engine::process::{EngineConfig, EngineCore};
use crate::engine::timeline::{TimeSignature, Timeline};
use crate::engine::track::Track;
use crate::score::mixer::{TrackMix, track_commands};

/// How one strip is to be built.
#[derive(Clone, Copy, Debug)]
pub struct StripSpec {
    pub sample_rate: u32,
    pub bpm: f64,
    pub signature: TimeSignature,
    /// Channel count of the dry signal: a mono track is fanned out exactly as live.
    pub channels: Channels,
    pub pan: f32,
    /// Fader, mute and chain, as the score's `mixer:` entry resolves.
    pub mix: TrackMix,
    /// Frames per process call. The live default is 128.
    pub block: usize,
}

/// One track of the live engine, run offline.
pub struct OfflineStrip {
    core: EngineCore,
    commands: CommandSender,
    status: StatusReceiver,
    inserts: Option<InsertSender>,
    channels: usize,
    block: usize,
    input: Vec<f32>,
    output: Vec<f32>,
}

impl OfflineStrip {
    pub fn new(spec: StripSpec) -> Self {
        let timeline = Timeline::new(spec.sample_rate, spec.bpm, spec.signature);
        let input = match spec.channels {
            Channels::Mono => TrackInput::Mono(0),
            Channels::Stereo => TrackInput::Stereo { left: 0, right: 1 },
        };
        // Monitoring on from the start (level 1, no fade), loop and input on both buses: the input
        // goes into the Main chain whatever the routing, and with one output pair there is one mix.
        let track = Track::new(input, true, spec.pan, spec.sample_rate)
            .with_sends(BusSend::BOTH, BusSend::BOTH);
        let (commands, command_rx) = command_channel(256);
        let (status_tx, status) = status_channel(4);
        // No take is ever recorded: the buffer stock stays empty.
        let (_buffers, buffer_endpoint) = buffer_channel(1, 1);
        let core = EngineCore::new(EngineConfig {
            timeline,
            latency_frames: 0,
            input_channels: spec.channels.count(),
            tracks: vec![track],
            spares: [Vec::new(), Vec::new()],
            layer_frames: 1,
            commands: command_rx,
            status: status_tx,
            buffers: buffer_endpoint,
            monitor_gain: 1.0,
            click: false,
            click_gain: 0.0,
            bus_gain: [1.0; BUS_COUNT],
            routing: BusRouting::default_for(2),
            output_channels: 2,
            status_interval: 1 << 30,
        });
        let block = spec.block.clamp(1, crate::engine::insert::MAX_BLOCK);
        let mut strip = Self {
            core,
            commands,
            status,
            inserts: None,
            channels: spec.channels.count(),
            block,
            input: vec![0.0; block * spec.channels.count()],
            output: vec![0.0; block * BUS_SAMPLES],
        };
        // The same commands loading a score sends (`Session::queue_mix`), against a fresh track.
        for cmd in track_commands(0, &spec.mix, &TrackStatus::default()) {
            strip.send(cmd);
        }
        strip
    }

    fn send(&mut self, cmd: Command) {
        // 256 slots against at most a few dozen commands per mixer entry.
        let _ = self.commands.send(cmd);
    }

    /// Put plugin inserts into the track's slots, in slot order, before anything is rendered.
    /// What the host does when it starts a slot (`plugins.rs`, `install`).
    pub fn install(&mut self, inserts: Vec<(usize, Insert)>) -> Result<(), String> {
        if inserts.is_empty() {
            return Ok(());
        }
        if self.inserts.is_none() {
            let (sender, endpoint) = insert_channel(64);
            self.core.attach_inserts(endpoint);
            self.inserts = Some(sender);
        }
        let sender = self.inserts.as_mut().expect("attached above");
        for (slot, insert) in inserts {
            sender.send(InsertMsg::Install {
                target: InsertTarget::Track(0),
                slot,
                insert,
            })?;
        }
        Ok(())
    }

    /// The read-ahead the engine works with right now (see the module comment).
    pub fn read_ahead(&self) -> u64 {
        self.core.read_ahead_of(0)
    }

    /// Run `frames` frames of silence and throw the output away.
    pub fn settle(&mut self, frames: u64) {
        let mut left = frames;
        while left > 0 {
            let n = (self.block as u64).min(left) as usize;
            self.input[..n * self.channels].fill(0.0);
            self.process(n);
            left -= n as u64;
        }
    }

    /// One block of `n` frames from `self.input` into `self.output`.
    fn process(&mut self, n: usize) {
        self.core.process(
            &self.input[..n * self.channels],
            &mut self.output[..n * BUS_SAMPLES],
        );
        // Snapshots are dropped when nobody reads them; reading keeps the queue short anyway.
        let _ = self.status.latest();
    }

    /// Render `dry` (interleaved, this strip's channel count) into `frames` frames of the Main bus,
    /// stereo interleaved, with the loop read ahead by `ahead` frames as the engine reads it.
    /// Input beyond the end of `dry` is silence, so tails ring out to `frames`.
    pub fn render(&mut self, dry: &[f32], frames: u64, ahead: u64) -> Vec<f32> {
        let ch = self.channels;
        let dry_frames = (dry.len() / ch) as u64;
        let mut out = Vec::with_capacity(frames as usize * 2);
        // `j` counts input frames fed; output frame `j - ahead` is what the file keeps.
        let mut j: u64 = 0;
        let total = frames + ahead;
        while j < total {
            let n = (self.block as u64).min(total - j) as usize;
            for i in 0..n {
                let k = j + i as u64;
                for c in 0..ch {
                    self.input[i * ch + c] = if k < dry_frames {
                        dry[k as usize * ch + c]
                    } else {
                        0.0
                    };
                }
            }
            self.process(n);
            for i in 0..n {
                let k = j + i as u64;
                if k >= ahead {
                    // Main bus, left and right - the track's contribution, as the stem tap sees it.
                    out.push(self.output[i * BUS_SAMPLES]);
                    out.push(self.output[i * BUS_SAMPLES + 1]);
                }
            }
            j += n as u64;
        }
        out
    }
}

impl Drop for OfflineStrip {
    fn drop(&mut self) {
        if let Some(sender) = self.inserts.as_mut() {
            sender.drain_retired();
        }
    }
}
