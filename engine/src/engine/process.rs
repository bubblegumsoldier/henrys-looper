//! The audio-thread brain: one function that turns an input block plus a command queue into an
//! output block.
//!
//! Deliberately free of cpal: `process` is fed slices, so the whole thing can be driven offline
//! with synthetic buffers and its sample accuracy can be proven without opening a device.
//!
//! Real-time rules that hold for every line below: no allocation, no locking, no logging, no
//! formatting, no file access. Only pre-allocated buffers, `Copy` values and lock-free queues.
//!
//! # The two axes
//!
//! Because cpal has no duplex callback, input and output are two streams. The engine therefore
//! counts two things, and keeping them apart is what makes the compensation correct:
//!
//! * `out_pos` - the musical timeline. Advanced by one per produced output sample. Bars, beats,
//!   the click and every command timestamp live on this axis.
//! * `in_index` - how many input samples the engine has consumed so far. Because the input
//!   callback pushes into a FIFO that starts empty and nothing is ever dropped from it, consumed
//!   sample number `k` *is* input sample number `k` of the session.
//!
//! # Latency compensation - the derivation
//!
//! Measured in phase 0 on this machine: `R = 827` samples at 128 frames / 48 kHz, standard
//! deviation zero over sixty runs. `R` is the roundtrip: a sample the engine writes at output
//! position `P` comes back at input index `P + R`. It covers output buffering, DA conversion,
//! cable, AD conversion and input buffering.
//!
//! Now follow one note the musician plays:
//!
//! 1. The engine writes the click for beat `B` at output position `P = B`.
//! 2. The musician *hears* that click after the output half of the roundtrip, `L_out`.
//! 3. He plays in time with what he hears, so his note leaves the instrument at that same moment.
//! 4. His note reaches the engine after the input half, `L_in`, i.e. at input index
//!    `B + L_out + L_in = B + R`.
//!
//! So the input sample carrying a note that was played *on* beat `B` arrives at index `B + R`.
//! Turned around, the sample at input index `k` was played at musical position
//!
//! ```text
//! m = k - R
//! ```
//!
//! and that is the position it must be stored at. In terms of the write pointer: it is moved
//! **backwards on the musical timeline** by `R` samples relative to the arrival of the sample -
//! the material is pulled earlier, into the past, because it arrived late. Storing it at `k`
//! instead would push every recorded layer 827 samples (17 ms) behind the click, and a second
//! layer recorded against the first would sit another 17 ms further back.
//!
//! Playback needs no correction at all. The loop is written to the output at position `p`, and
//! reaches the ears through exactly the same `L_out` as the click at `p` - so click and loop line
//! up for the listener by construction. Only the recording path is asymmetric, and only it is
//! compensated. Compensating both would be the classic double correction and would put the loop
//! 827 samples *ahead* of the click.
//!
//! Sign check that survives a sleepless night: `R` is positive, the input is always *late*,
//! therefore recorded material must be moved *earlier*, therefore `m = k - R`, never `k + R`.
//! `tests.rs` nails this down with a simulated 827-sample loopback and zero tolerance; getting
//! the sign wrong misses by 1654 samples.
//!
//! One consequence worth knowing: while recording, the write pointer trails the play pointer by
//! `R` samples. The loop must therefore be longer than `R` - a 17 ms loop cannot work - and the
//! last `R` samples of a fresh take are still being written while the head of the loop is already
//! playing. `LoopTrack::filled` covers that gap with silence instead of stale memory.

use super::command::{BufferEndpoint, Command, CommandReceiver, Status, StatusSender};
use super::metro::Metronome;
use super::timeline::Timeline;
use super::track::{LoopTrack, TrackState};

/// Open recording window in musical coordinates.
#[derive(Clone, Copy, Debug)]
struct RecordWindow {
    /// First musical sample that goes into the loop.
    start: u64,
    /// One past the last musical sample, once `StopRecord` has been received.
    end: Option<u64>,
}

/// Everything the engine needs to exist. Assembled by the control thread.
pub struct EngineConfig {
    pub timeline: Timeline,
    /// Roundtrip latency in samples, see the module comment.
    pub latency_samples: u64,
    /// Pre-allocated loop buffer. Its length is the maximum loop length.
    pub buffer: Vec<f32>,
    pub commands: CommandReceiver,
    pub status: StatusSender,
    pub buffers: BufferEndpoint,
    pub monitor: bool,
    pub monitor_gain: f32,
    pub click: bool,
    pub click_gain: f32,
    /// How often a status snapshot is pushed, in samples.
    pub status_interval: u64,
}

pub struct EngineCore {
    timeline: Timeline,
    metro: Metronome,
    latency: u64,
    /// Musical timeline position of the next output sample.
    out_pos: u64,
    /// Number of input samples consumed so far; also the absolute index of the next one.
    in_index: u64,
    track: LoopTrack,
    record: Option<RecordWindow>,
    playing: bool,
    monitor: bool,
    monitor_gain: f32,
    click: bool,
    click_gain: f32,
    commands: CommandReceiver,
    status: StatusSender,
    buffers: BufferEndpoint,
    status_interval: u64,
    since_status: u64,
    input_peak: f32,
    output_peak: f32,
    ignored_commands: u64,
    stopped: bool,
}

impl EngineCore {
    pub fn new(cfg: EngineConfig) -> Self {
        Self {
            timeline: cfg.timeline,
            metro: Metronome::new(cfg.timeline.sample_rate()),
            latency: cfg.latency_samples,
            out_pos: 0,
            in_index: 0,
            track: LoopTrack::from_buffer(cfg.buffer),
            record: None,
            playing: false,
            monitor: cfg.monitor,
            monitor_gain: cfg.monitor_gain,
            click: cfg.click,
            click_gain: cfg.click_gain,
            commands: cfg.commands,
            status: cfg.status,
            buffers: cfg.buffers,
            status_interval: cfg.status_interval.max(1),
            since_status: 0,
            input_peak: 0.0,
            output_peak: 0.0,
            ignored_commands: 0,
            stopped: false,
        }
    }

    #[cfg(test)]
    pub fn track(&self) -> &LoopTrack {
        &self.track
    }

    /// One audio block.
    ///
    /// `input` holds the input samples consumed for this block, starting at absolute input index
    /// `in_index`; it may be shorter than `output` when the input FIFO ran dry, and then simply
    /// fewer input samples are accounted for - the mapping `m = k - R` stays intact because `k`
    /// counts consumed samples, not elapsed time.
    ///
    /// `output` is the mono output block starting at `out_pos`. It is fully overwritten.
    pub fn process(&mut self, input: &[f32], output: &mut [f32]) {
        self.install_pending_buffer();
        self.render_with_commands(output);
        self.mix_monitor(input, output);
        self.clamp_and_meter(output);
        self.record_input(input);

        self.out_pos += output.len() as u64;
        self.in_index += input.len() as u64;
        self.since_status += output.len() as u64;
        if self.since_status >= self.status_interval {
            self.since_status = 0;
            self.publish_status();
        }
    }

    /// A loop buffer handed over by the control thread replaces the current one - but never in the
    /// middle of a take, because that would throw away what is being recorded right now.
    fn install_pending_buffer(&mut self) {
        if self.record.is_some() {
            return;
        }
        if let Some(new_buffer) = self.buffers.take_new() {
            self.playing = false;
            let old = self.track.swap_buffer(new_buffer);
            self.buffers.retire(old);
        }
    }

    /// Render the output block, splitting it at every command boundary.
    ///
    /// This is the heart of the sample accuracy: a command timed for the middle of the block takes
    /// effect exactly there, because the block is rendered as two segments with the command
    /// applied in between - not at the start of the block and not at its end.
    fn render_with_commands(&mut self, output: &mut [f32]) {
        let len = output.len();
        let block_end = self.out_pos + len as u64;
        let mut cursor = 0usize;
        loop {
            // Position at which rendering has to pause for the next command. A command whose time
            // has already passed (control thread was late) acts right here rather than in the past.
            let due = self.next_due(block_end, self.out_pos + cursor as u64);
            let boundary = match due {
                Some(at) => ((at - self.out_pos) as usize).min(len),
                None => len,
            };
            if boundary > cursor {
                let start = self.out_pos + cursor as u64;
                self.render(&mut output[cursor..boundary], start);
                cursor = boundary;
            }
            match due {
                Some(_) => {
                    let cmd = self
                        .commands
                        .pop()
                        .expect("peek meldete ein Kommando, pop muss es liefern");
                    self.apply(cmd);
                }
                None => break,
            }
        }
    }

    /// Position of the next command that must act inside this block, or `None`.
    #[inline]
    fn next_due(&self, block_end: u64, not_before: u64) -> Option<u64> {
        let cmd = self.commands.peek()?;
        match cmd.at() {
            // Untimed commands act at the next opportunity.
            None => Some(not_before),
            Some(at) if at < block_end => Some(at.max(not_before)),
            Some(_) => None,
        }
    }

    /// Click plus loop playback for one uninterrupted segment.
    #[inline]
    fn render(&mut self, out: &mut [f32], start: u64) {
        for (i, slot) in out.iter_mut().enumerate() {
            let pos = start + i as u64;
            let mut value = 0.0f32;
            if self.click {
                value += self.metro.sample_at(&self.timeline, pos) * self.click_gain;
            }
            if self.playing {
                value += self.track.read(pos);
            }
            *slot = value;
        }
    }

    /// Input monitoring is a straight pass-through of the block that was just consumed. Its delay
    /// is the device roundtrip and has nothing to do with loop alignment - the loop is aligned by
    /// position, not by when a sample happens to travel through the engine.
    #[inline]
    fn mix_monitor(&mut self, input: &[f32], output: &mut [f32]) {
        if !self.monitor {
            return;
        }
        let n = input.len().min(output.len());
        for i in 0..n {
            output[i] += input[i] * self.monitor_gain;
        }
    }

    #[inline]
    fn clamp_and_meter(&mut self, output: &mut [f32]) {
        let mut peak = 0.0f32;
        for slot in output.iter_mut() {
            let v = slot.clamp(-1.0, 1.0);
            *slot = v;
            let a = v.abs();
            if a > peak {
                peak = a;
            }
        }
        self.output_peak = self.output_peak.max(peak);
    }

    /// Write the consumed input block into the loop, latency-compensated.
    #[inline]
    fn record_input(&mut self, input: &[f32]) {
        let mut peak = 0.0f32;
        for (j, &sample) in input.iter().enumerate() {
            let a = sample.abs();
            if a > peak {
                peak = a;
            }
            let Some(window) = self.record else {
                continue;
            };
            let k = self.in_index + j as u64;
            if k < self.latency {
                // Nothing that arrives in the first R samples of the session was played after
                // position 0, so there is no musical position to store it at.
                continue;
            }
            // The one line the whole compensation comes down to. See the module comment.
            let pos = k - self.latency;
            if pos < window.start {
                continue;
            }
            if let Some(end) = window.end
                && pos >= end
            {
                self.finish_take(end);
                continue;
            }
            if pos - window.start >= self.track.capacity() {
                // Never scheduled to stop and the buffer is full: close the take at the buffer
                // length rather than silently dropping samples.
                self.finish_take(window.start + self.track.capacity());
                continue;
            }
            self.track.write(pos, sample);
        }
        self.input_peak = self.input_peak.max(peak);
    }

    fn finish_take(&mut self, end: u64) {
        self.track.finish_take(end);
        self.record = None;
    }

    fn apply(&mut self, cmd: Command) {
        match cmd {
            Command::StartRecord { at } => {
                // The past cannot be recorded: input samples older than the current musical input
                // position are already gone, so a late command starts here instead. This also
                // guarantees the loop buffer is written strictly sequentially.
                let start = at.max(self.musical_input_pos());
                self.track.begin_take(start);
                self.record = Some(RecordWindow { start, end: None });
                self.playing = false;
            }
            Command::StopRecord { at } => {
                if let Some(window) = self.record.as_mut() {
                    let end = at.max(window.start);
                    window.end = Some(end);
                    // The loop length is known now, even though the last R samples of the take are
                    // still travelling in from the interface. Publishing it here is what makes the
                    // switch from recording to playing seamless at the loop boundary.
                    self.track.set_length(end - window.start);
                }
            }
            Command::StartPlay { .. } => self.playing = true,
            Command::StopPlay { .. } => self.playing = false,
            Command::ClearTrack { .. } => {
                self.record = None;
                self.playing = false;
                self.track.clear();
            }
            Command::SetMonitor { on } => self.monitor = on,
            Command::SetClick { on } => self.click = on,
            Command::SetTempo { bpm, signature } => {
                // A tempo change redefines what every sample position means, so it is only safe
                // while nothing is recorded, recording or playing.
                if self.track.has_content() || self.record.is_some() || self.playing {
                    self.ignored_commands += 1;
                } else {
                    self.timeline = Timeline::new(self.timeline.sample_rate(), bpm, signature);
                }
            }
            Command::Stop => {
                self.record = None;
                self.playing = false;
                self.stopped = true;
            }
        }
    }

    /// Musical position of the next input sample to be consumed.
    #[inline]
    fn musical_input_pos(&self) -> u64 {
        self.in_index.saturating_sub(self.latency)
    }

    fn state(&self) -> TrackState {
        match self.record {
            Some(window) if self.musical_input_pos() < window.start => TrackState::Armed,
            Some(_) => TrackState::Recording,
            None if !self.track.has_content() => TrackState::Empty,
            None if self.playing => TrackState::Playing,
            None => TrackState::Ready,
        }
    }

    fn publish_status(&mut self) {
        let here = self.timeline.locate(self.out_pos);
        let status = Status {
            pos: self.out_pos,
            bar: here.bar,
            beat: here.beat,
            beat_offset: here.offset,
            samples_per_beat: self.timeline.samples_per_beat(),
            track: self.state(),
            loop_len: self.track.loop_len(),
            filled: self.track.filled(),
            input_peak: self.input_peak,
            output_peak: self.output_peak,
            monitor: self.monitor,
            click: self.click,
            bpm: self.timeline.bpm(),
            ignored_commands: self.ignored_commands,
            stopped: self.stopped,
        };
        self.input_peak = 0.0;
        self.output_peak = 0.0;
        self.status.push(status);
    }
}

/// Number of samples the maximum loop needs, plus a small margin.
///
/// Bar boundaries are rounded individually, so `bars` bars are not always the same number of
/// samples; the margin covers that without a second allocation.
pub fn loop_capacity(timeline: &Timeline, bars: u32) -> u64 {
    timeline.span_bars(0, bars) + 64
}

/// Validate a loop configuration before any buffer is allocated.
pub fn check_loop(timeline: &Timeline, bars: u32, latency: u64) -> Result<(), String> {
    if bars == 0 {
        return Err("--bars muss mindestens 1 sein.".to_string());
    }
    let len = timeline.span_bars(0, bars);
    if len <= latency {
        return Err(format!(
            "Der Loop waere mit {len} Samples kuerzer als die Latenzkompensation ({latency} Samples). \
             Mehr Takte, hoeheres Tempo pruefen oder --latency-samples korrigieren."
        ));
    }
    Ok(())
}
