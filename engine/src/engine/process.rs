//! The audio-thread brain: one function that turns an input block plus a command queue into an
//! output block.
//!
//! Deliberately free of cpal: `process` is fed slices, so the whole thing can be driven offline
//! with synthetic buffers and its sample accuracy can be proven without opening a device.
//!
//! Real-time rules that hold for every line below: no allocation, no locking, no logging, no
//! formatting, no file access. Only pre-allocated buffers, `Copy` values and lock-free queues.
//! Layer buffers arrive ready-made from the control thread and go back the same way; nothing here
//! ever creates or drops a `Vec`.
//!
//! # The two axes
//!
//! Because cpal has no duplex callback, input and output are two streams. The engine therefore
//! counts two things, and keeping them apart is what makes the compensation correct:
//!
//! * `out_pos` - the musical timeline. Advanced by one per produced output sample. Bars, beats,
//!   the click and every command timestamp live on this axis.
//! * `in_index` - how many input *frames* the engine has consumed so far. Because the input
//!   callback pushes into a FIFO that starts empty and nothing is ever dropped from it, consumed
//!   frame number `k` *is* input frame number `k` of the session. A frame holds one sample per
//!   device input channel; which of them a track records is that track's business.
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
//! This is unchanged by phase 2 and it is what keeps *layers* aligned as well: whatever the
//! musician hears - click, first layer, third layer - travels the same `L_out`, and whatever he
//! plays travels the same `L_in`. Every take, in every bar, on every track, is therefore stored on
//! the same grid. The per-layer arithmetic that follows from it lives in `track.rs`.
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
//! playing. The zeroed layer buffer covers that gap with silence instead of stale memory.

use super::command::{
    BufferEndpoint, Command, CommandReceiver, MAX_TRACKS, Refusal, Status, StatusSender,
    TrackStatus,
};
use super::metro::Metronome;
use super::timeline::Timeline;
use super::track::{MAX_LAYERS, Track};

/// Size of the waiting room for commands that have been taken out of the queue but are not due
/// yet. Allocated once, in the control thread; the audio thread only ever pushes into it and
/// removes from it.
const SCHEDULED_SLOTS: usize = 64;

/// Everything the engine needs to exist. Assembled by the control thread.
pub struct EngineConfig {
    pub timeline: Timeline,
    /// Roundtrip latency in samples, see the module comment.
    pub latency_samples: u64,
    /// Number of channels in one interleaved input frame.
    pub input_channels: usize,
    /// The tracks, with their input channels. Built by the control thread, never resized here.
    pub tracks: Vec<Track>,
    /// Empty vector with room for the buffer stock. Its capacity is the stock size.
    pub spares: Vec<Vec<f32>>,
    /// Length a layer buffer must have to be usable.
    pub layer_capacity: u64,
    pub commands: CommandReceiver,
    pub status: StatusSender,
    pub buffers: BufferEndpoint,
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
    /// Number of input frames consumed so far; also the absolute index of the next one.
    in_index: u64,
    input_channels: usize,
    tracks: Vec<Track>,
    /// Prepared, zeroed buffers waiting to become layers. Never grows past its capacity, so
    /// pushing into it cannot allocate.
    spares: Vec<Vec<f32>>,
    spare_slots: usize,
    layer_capacity: u64,
    monitor_gain: f32,
    click: bool,
    click_gain: f32,
    commands: CommandReceiver,
    /// Commands taken out of the queue but not executed yet, see [`EngineCore::next_due`].
    scheduled: Vec<Command>,
    status: StatusSender,
    buffers: BufferEndpoint,
    status_interval: u64,
    since_status: u64,
    output_peak: f32,
    ignored_commands: u64,
    refusal: Refusal,
    buffers_taken: u64,
    stopped: bool,
}

impl EngineCore {
    pub fn new(cfg: EngineConfig) -> Self {
        let spare_slots = cfg.spares.capacity();
        Self {
            timeline: cfg.timeline,
            metro: Metronome::new(cfg.timeline.sample_rate()),
            latency: cfg.latency_samples,
            out_pos: 0,
            in_index: 0,
            input_channels: cfg.input_channels.max(1),
            tracks: cfg.tracks,
            spares: cfg.spares,
            spare_slots,
            layer_capacity: cfg.layer_capacity,
            monitor_gain: cfg.monitor_gain,
            click: cfg.click,
            click_gain: cfg.click_gain,
            commands: cfg.commands,
            scheduled: Vec::with_capacity(SCHEDULED_SLOTS),
            status: cfg.status,
            buffers: cfg.buffers,
            status_interval: cfg.status_interval.max(1),
            since_status: 0,
            output_peak: 0.0,
            ignored_commands: 0,
            refusal: Refusal::None,
            buffers_taken: 0,
            stopped: false,
        }
    }

    #[cfg(test)]
    pub fn track(&self, index: usize) -> &Track {
        &self.tracks[index]
    }

    #[cfg(test)]
    pub fn track_count(&self) -> usize {
        self.tracks.len()
    }

    #[cfg(test)]
    pub fn spare_count(&self) -> usize {
        self.spares.len()
    }

    /// One audio block.
    ///
    /// `input` holds the interleaved input frames consumed for this block, starting at absolute
    /// input index `in_index`; it may be shorter than `output` when the input FIFO ran dry, and
    /// then simply fewer input frames are accounted for - the mapping `m = k - R` stays intact
    /// because `k` counts consumed frames, not elapsed time.
    ///
    /// `output` is the mono output block starting at `out_pos`. It is fully overwritten.
    pub fn process(&mut self, input: &[f32], output: &mut [f32]) {
        self.refill_spares();
        self.collect_commands();
        let frames = input.len() / self.input_channels;

        self.render_with_commands(output);
        self.mix_monitor(input, frames, output);
        self.clamp_and_meter(output);
        self.record_input(input, frames);

        self.out_pos += output.len() as u64;
        self.in_index += frames as u64;
        self.since_status += output.len() as u64;
        if self.since_status >= self.status_interval {
            self.since_status = 0;
            self.publish_status();
        }
    }

    /// Collect prepared buffers from the control thread. Bounded work: at most `spare_slots` pops.
    fn refill_spares(&mut self) {
        while self.spares.len() < self.spare_slots {
            match self.buffers.take_new() {
                Some(buffer) => {
                    self.buffers_taken += 1;
                    self.spares.push(buffer);
                }
                None => break,
            }
        }
    }

    /// One prepared buffer, or `None` if the stock ran dry. Buffers of the wrong length (left over
    /// from a tempo change) are sent back instead of being used.
    fn take_spare(&mut self) -> Option<Vec<f32>> {
        while let Some(buffer) = self.spares.pop() {
            if buffer.len() as u64 >= self.layer_capacity {
                return Some(buffer);
            }
            self.buffers.retire(buffer);
        }
        None
    }

    /// Move everything the control thread sent into the waiting room. Bounded work: at most
    /// `SCHEDULED_SLOTS` pops, and nothing is lost - what does not fit stays in the queue.
    fn collect_commands(&mut self) {
        while self.scheduled.len() < SCHEDULED_SLOTS {
            match self.commands.pop() {
                Some(cmd) => self.scheduled.push(cmd),
                None => break,
            }
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
                Some((_, at)) => ((at - self.out_pos) as usize).min(len),
                None => len,
            };
            if boundary > cursor {
                let start = self.out_pos + cursor as u64;
                self.render(&mut output[cursor..boundary], start);
                cursor = boundary;
            }
            match due {
                Some((index, _)) => {
                    // `remove` on a `Vec` of `Copy` commands is a memmove of a few dozen bytes.
                    let cmd = self.scheduled.remove(index);
                    self.apply(cmd);
                }
                None => break,
            }
        }
    }

    /// The next command that must act inside this block: its slot in the waiting room and the
    /// position it acts at.
    ///
    /// The waiting room is scanned rather than worked through front to back, because commands for
    /// different tracks arrive interleaved and their timestamps are therefore *not* in order: a
    /// stop scheduled for bar 9 on the voice must not hold back a record scheduled for bar 2 on the
    /// guitar. Among commands due at the same position the one that was sent first wins, so the
    /// order inside one track is preserved exactly.
    #[inline]
    fn next_due(&self, block_end: u64, not_before: u64) -> Option<(usize, u64)> {
        let mut best: Option<(usize, u64)> = None;
        for (i, cmd) in self.scheduled.iter().enumerate() {
            // Untimed commands carry no musical meaning and act at the next opportunity.
            let at = cmd.at().unwrap_or(0);
            if best.map(|(_, b)| at < b).unwrap_or(true) {
                best = Some((i, at));
            }
        }
        let (index, at) = best?;
        if at < block_end {
            Some((index, at.max(not_before)))
        } else {
            None
        }
    }

    /// Click plus every playing track for one uninterrupted segment.
    #[inline]
    fn render(&mut self, out: &mut [f32], start: u64) {
        // Per-track output peaks are collected on the stack and folded into the tracks afterwards:
        // inside the sample loop the track list is borrowed immutably, and a fixed-size array
        // costs no allocation. `get_mut` rather than indexing, so a track count beyond the status
        // ceiling degrades to "no meter" instead of a panic in the audio callback.
        let mut peaks = [0.0f32; MAX_TRACKS];
        for (i, slot) in out.iter_mut().enumerate() {
            let pos = start + i as u64;
            let mut value = 0.0f32;
            if self.click {
                value += self.metro.sample_at(&self.timeline, pos) * self.click_gain;
            }
            for (t, track) in self.tracks.iter().enumerate() {
                if track.playing() {
                    let sample = track.read(pos);
                    value += sample;
                    if let Some(peak) = peaks.get_mut(t) {
                        let magnitude = sample.abs();
                        if magnitude > *peak {
                            *peak = magnitude;
                        }
                    }
                }
            }
            *slot = value;
        }
        for (t, track) in self.tracks.iter_mut().enumerate() {
            if let Some(&peak) = peaks.get(t) {
                track.note_output_peak(peak);
            }
        }
    }

    /// Input monitoring per track: a straight pass-through of that track's input channel, switched
    /// independently of whether the track is playing, so the musician can play live over his own
    /// loop. Its delay is the device roundtrip and has nothing to do with loop alignment - the loop
    /// is aligned by position, not by when a sample happens to travel through the engine.
    #[inline]
    fn mix_monitor(&mut self, input: &[f32], frames: usize, output: &mut [f32]) {
        let n = frames.min(output.len());
        let ch = self.input_channels;
        for track in &self.tracks {
            if !track.monitor() {
                continue;
            }
            let c = track.input_channel();
            for (i, slot) in output.iter_mut().take(n).enumerate() {
                *slot += input[i * ch + c] * self.monitor_gain;
            }
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

    /// Write the consumed input frames into the running takes, latency-compensated, and meter every
    /// track's input channel on the way.
    #[inline]
    fn record_input(&mut self, input: &[f32], frames: usize) {
        let ch = self.input_channels;
        for j in 0..frames {
            let k = self.in_index + j as u64;
            let base = j * ch;
            // Nothing that arrives in the first R frames of the session was played after position
            // 0, so there is no musical position to store it at.
            let recordable = k >= self.latency;
            // The one line the whole compensation comes down to. See the module comment.
            let pos = k.wrapping_sub(self.latency);
            for t in 0..self.tracks.len() {
                let c = self.tracks[t].input_channel();
                let sample = match input.get(base + c) {
                    Some(&s) => s,
                    None => continue,
                };
                self.tracks[t].note_input_peak(sample.abs());
                if !recordable {
                    continue;
                }
                let Some(take) = self.tracks[t].take() else {
                    continue;
                };
                if pos < take.start {
                    continue;
                }
                // Two ways a take ends: at the position it was stopped at, or - if it was never
                // stopped - at the buffer length (first layer) respectively after one pass through
                // the loop (overdub), instead of overwriting what it just recorded.
                let ended = match take.end {
                    Some(end) if pos >= end => Some(end),
                    _ => match self.tracks[t].take_limit() {
                        Some(limit) if pos >= limit => Some(limit),
                        _ => None,
                    },
                };
                if let Some(end) = ended {
                    self.finish_take(t, end);
                    // This very sample is the first one *after* the old take - and therefore the
                    // first one of a take queued behind it, if the two are back to back.
                    self.write_into_take(t, pos, sample);
                    continue;
                }
                self.tracks[t].write(pos, sample);
            }
        }
    }

    /// Write one sample into whatever take is open on this track, if the position belongs to it.
    #[inline]
    fn write_into_take(&mut self, track: usize, pos: u64, sample: f32) {
        let Some(take) = self.tracks[track].take() else {
            return;
        };
        if pos < take.start {
            return;
        }
        if take.end.map(|end| pos >= end).unwrap_or(false) {
            return;
        }
        if self.tracks[track]
            .take_limit()
            .map(|limit| pos >= limit)
            .unwrap_or(false)
        {
            return;
        }
        self.tracks[track].write(pos, sample);
    }

    fn apply(&mut self, cmd: Command) {
        // Every track-scoped command is checked once, here, so nothing below can index out of range.
        if let Some(track) = cmd.track()
            && track >= self.tracks.len()
        {
            self.refuse(Refusal::NoSuchTarget);
            return;
        }
        match cmd {
            Command::StartRecord { track, at } => {
                // The past cannot be recorded: input frames older than the current musical input
                // position are already gone, so a late command starts here instead. This also
                // guarantees the first layer is written strictly sequentially.
                let start = at.max(self.musical_input_pos());
                if self.tracks[track].take_is_finishing() {
                    // The running take is only waiting for its tail; queue this one behind it.
                    self.schedule_take(track, start, true);
                } else {
                    self.clear_track(track);
                    self.begin_take(track, start, true);
                }
            }
            Command::StartOverdub { track, at } => {
                let start = at.max(self.musical_input_pos());
                let finishing = self.tracks[track].take_is_finishing();
                if self.tracks[track].take().is_some() && !finishing {
                    self.refuse(Refusal::Busy);
                    return;
                }
                // An overdub on an empty track is simply the first take - the musician should not
                // have to know which key defines the loop. A take that is finishing has already
                // published its loop length, so `has_content` covers that case too.
                let defines_loop = !self.tracks[track].has_content();
                if !defines_loop && self.tracks[track].layer_count() >= MAX_LAYERS {
                    self.refuse(Refusal::LayerLimit);
                    return;
                }
                if finishing {
                    self.schedule_take(track, start, defines_loop);
                } else {
                    if defines_loop {
                        self.clear_track(track);
                    }
                    self.begin_take(track, start, defines_loop);
                }
            }
            Command::StopRecord { track, at } => {
                // The loop length is known now, even though the last R samples of the take are
                // still travelling in from the interface. Publishing it here is what makes the
                // switch from recording to playing seamless at the loop boundary.
                self.tracks[track].set_take_end(at);
            }
            Command::StartPlay { track, .. } => self.tracks[track].set_playing(true),
            Command::StopPlay { track, .. } => self.tracks[track].set_playing(false),
            Command::ClearTrack { track, .. } => self.clear_track(track),
            Command::ClearAll { .. } => {
                for track in 0..self.tracks.len() {
                    self.clear_track(track);
                    self.tracks[track].restore_defaults();
                }
            }
            Command::SetMonitor { track, on } => self.tracks[track].set_monitor(on),
            Command::SetLayerMute {
                track,
                layer,
                muted,
            } => {
                if !self.tracks[track].set_layer_muted(layer, muted) {
                    self.refuse(Refusal::NoSuchTarget);
                }
            }
            Command::SetLayerGain { track, layer, gain } => {
                if !self.tracks[track].set_layer_gain(layer, gain) {
                    self.refuse(Refusal::NoSuchTarget);
                }
            }
            Command::RemoveLayer { track, layer } => {
                // Removing while a take runs would renumber the layer that take writes into.
                if self.tracks[track].take().is_some() {
                    self.refuse(Refusal::Busy);
                    return;
                }
                match self.tracks[track].remove_layer(layer) {
                    Some(buffer) => self.buffers.retire(buffer),
                    None => self.refuse(Refusal::NoSuchTarget),
                }
            }
            Command::SetClick { on } => self.click = on,
            Command::SetTempo {
                bpm,
                signature,
                layer_capacity,
            } => {
                // A tempo change redefines what every sample position means, so it is only safe
                // while nothing is recorded, recording or playing.
                if self.tracks.iter().any(|t| {
                    t.layer_count() > 0 || t.take().is_some() || t.playing()
                }) {
                    self.refuse(Refusal::Tempo);
                } else {
                    self.timeline = Timeline::new(self.timeline.sample_rate(), bpm, signature);
                    self.layer_capacity = layer_capacity;
                    // The stock is the wrong length now; hand it back and let the control thread
                    // send buffers that fit.
                    while let Some(buffer) = self.spares.pop() {
                        self.buffers.retire(buffer);
                    }
                }
            }
            Command::Stop => {
                for track in 0..self.tracks.len() {
                    self.cancel_takes(track);
                    self.tracks[track].set_playing(false);
                }
                self.stopped = true;
            }
        }
    }

    /// Close a take and immediately start whatever was queued behind it.
    fn finish_take(&mut self, track: usize, end: u64) {
        self.tracks[track].finish_take(end);
        if self.tracks[track].pending_defines_loop() {
            // What was queued is a completely new loop, so the old layers go back first.
            self.tracks[track].set_playing(false);
            while let Some(buffer) = self.tracks[track].pop_layer() {
                self.buffers.retire(buffer);
            }
        }
        if let Some(buffer) = self.tracks[track].promote_pending() {
            self.buffers.retire(buffer);
            self.refuse(Refusal::LayerLimit);
        }
    }

    /// Queue a take behind the one that is currently finishing.
    fn schedule_take(&mut self, track: usize, start: u64, defines_loop: bool) {
        let Some(buffer) = self.take_spare() else {
            self.refuse(Refusal::NoBuffer);
            return;
        };
        if let Some(previous) = self.tracks[track].set_pending(start, defines_loop, buffer) {
            self.buffers.retire(previous);
        }
    }

    /// Take a prepared buffer and open a take with it, or report why that failed.
    fn begin_take(&mut self, track: usize, start: u64, defines_loop: bool) {
        if self.tracks[track].layer_count() >= MAX_LAYERS {
            self.refuse(Refusal::LayerLimit);
            return;
        }
        let Some(buffer) = self.take_spare() else {
            self.refuse(Refusal::NoBuffer);
            return;
        };
        if defines_loop {
            self.tracks[track].begin_loop_take(start, buffer);
        } else {
            self.tracks[track].begin_overdub_take(start, buffer);
        }
    }

    /// Cancel whatever this track is doing and give every one of its buffers back.
    fn clear_track(&mut self, track: usize) {
        self.cancel_takes(track);
        self.tracks[track].set_playing(false);
        while let Some(buffer) = self.tracks[track].pop_layer() {
            self.buffers.retire(buffer);
        }
    }

    /// Drop the running and the queued take of one track, returning the queued take's buffer.
    fn cancel_takes(&mut self, track: usize) {
        self.tracks[track].cancel_take();
        if let Some(buffer) = self.tracks[track].take_pending_buffer() {
            self.buffers.retire(buffer);
        }
    }

    fn refuse(&mut self, reason: Refusal) {
        self.ignored_commands += 1;
        self.refusal = reason;
    }

    /// Musical position of the next input frame to be consumed.
    #[inline]
    fn musical_input_pos(&self) -> u64 {
        self.in_index.saturating_sub(self.latency)
    }

    fn publish_status(&mut self) {
        let here = self.timeline.locate(self.out_pos);
        let musical_input = self.musical_input_pos();
        let mut tracks = [TrackStatus::default(); MAX_TRACKS];
        for (i, slot) in tracks.iter_mut().enumerate().take(self.tracks.len()) {
            let track = &mut self.tracks[i];
            *slot = TrackStatus {
                state: track.state(musical_input),
                layers: track.layer_count() as u8,
                muted_mask: track.muted_mask(),
                loop_len: track.loop_len(),
                origin: track.origin(),
                filled: track.filled(),
                input_peak: track.take_input_peak(),
                output_peak: track.take_output_peak(),
                monitor: track.monitor(),
                playing: track.playing(),
                input_channel: track.input_channel() as u8,
            };
        }
        let status = Status {
            pos: self.out_pos,
            bar: here.bar,
            beat: here.beat,
            beat_offset: here.offset,
            samples_per_beat: self.timeline.samples_per_beat(),
            tracks,
            track_count: self.tracks.len() as u8,
            output_peak: self.output_peak,
            click: self.click,
            bpm: self.timeline.bpm(),
            ignored_commands: self.ignored_commands,
            refusal: self.refusal,
            spares: self.spares.len() as u32,
            buffers_taken: self.buffers_taken,
            stopped: self.stopped,
        };
        self.output_peak = 0.0;
        self.status.push(status);
    }
}

/// Number of samples the maximum loop needs, plus a small margin. Also the length of one layer
/// buffer - layers are allocated in loop length, never in some blanket maximum.
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

/// Memory one full session can occupy at most, in bytes - the number behind the layer ceiling.
pub fn max_memory_bytes(layer_capacity: u64, tracks: usize, spares: usize) -> u64 {
    layer_capacity * 4 * (tracks as u64 * MAX_LAYERS as u64 + spares as u64)
}

/// German summary of the layer limits, for the startup banner.
pub fn limits_line(layer_capacity: u64, tracks: usize, spares: usize) -> String {
    format!(
        "Ebenen:   bis zu {} je Track, {:.1} MB je Ebene, hoechstens {:.0} MB fuer {} Tracks",
        MAX_LAYERS,
        layer_capacity as f64 * 4.0 / 1_048_576.0,
        max_memory_bytes(layer_capacity, tracks, spares) as f64 / 1_048_576.0,
        tracks
    )
}

/// Sanity-check a state that is meant to look like a fresh start.
#[cfg(test)]
pub fn is_fresh(status: &Status) -> bool {
    status.tracks().iter().all(|t| {
        t.state == super::track::TrackState::Empty
            && t.layers == 0
            && t.loop_len == 0
            && t.filled == 0
            && t.muted_mask == 0
            && !t.playing
    })
}
