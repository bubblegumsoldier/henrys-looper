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
//! # The two axes, and the unit both of them count in
//!
//! Because cpal has no duplex callback, input and output are two streams. The engine therefore
//! counts two things, and keeping them apart is what makes the compensation correct:
//!
//! * `out_pos` - the musical timeline. Advanced by one per produced output **frame**. Bars, beats,
//!   the click and every command timestamp live on this axis.
//! * `in_index` - how many input **frames** the engine has consumed so far. Because the input
//!   callback pushes into a FIFO that starts empty and nothing is ever dropped from it, consumed
//!   frame number `k` *is* input frame number `k` of the session.
//!
//! Both are **frames**, not samples, and that distinction is the one this module has to be read
//! with. A device input frame holds one sample per device input channel; an engine output frame
//! holds two, left and right, because the mix bus is stereo without exception; a track's loop
//! buffer holds one or two per frame depending on what that track records. None of that changes
//! either counter, because a frame is one instant of audio whatever it is made of.
//!
//! # Latency compensation - the derivation
//!
//! Measured in phase 0 on this machine: `R = 827` frames at 128 frames / 48 kHz, standard
//! deviation zero over sixty runs. `R` is the roundtrip: what the engine writes at output position
//! `P` comes back at input index `P + R`. It covers output buffering, DA conversion, cable, AD
//! conversion and input buffering.
//!
//! Now follow one note the musician plays:
//!
//! 1. The engine writes the click for beat `B` at output position `P = B`.
//! 2. The musician *hears* that click after the output half of the roundtrip, `L_out`.
//! 3. He plays in time with what he hears, so his note leaves the instrument at that same moment.
//! 4. His note reaches the engine after the input half, `L_in`, i.e. at input index
//!    `B + L_out + L_in = B + R`.
//!
//! So the input frame carrying a note that was played *on* beat `B` arrives at index `B + R`.
//! Turned around, the frame at input index `k` was played at musical position
//!
//! ```text
//! m = k - R          (R in Frames, nicht in Samples)
//! ```
//!
//! and that is the position it must be stored at. In terms of the write pointer: it is moved
//! **backwards on the musical timeline** by `R` frames relative to the arrival of the frame - the
//! material is pulled earlier, into the past, because it arrived late. Storing it at `k` instead
//! would push every recorded layer 827 frames (17 ms) behind the click, and a second layer recorded
//! against the first would sit another 17 ms further back.
//!
//! **`R` belongs to a track, not to the engine.** The derivation above follows *one* signal on
//! *one* way in, and two sources need not share that way. A microphone and a guitar do - both go
//! through the AD converter of the same interface - but a plugin host such as Cantabile feeding us
//! over an ASIO router does not: its audio is already digital, passes no converter, and therefore
//! arrives *earlier*. Every track carries its own `R` ([`super::track::TrackLatency`]);
//! [`EngineConfig::latency_frames`] is the default the tracks that do not follow. The recording
//! loop reads `track.latency_frames()`, and everything above holds per track.
//!
//! **Stereo changes nothing in this formula, and that is the point of stating its unit.** `R` was
//! measured as a delay in time and is therefore a number of frames; a stereo track's two channels
//! travel through the same converter at the same instant and are therefore stored at the same `m`.
//! What would break is treating `R` as a count of *samples*: on a stereo buffer that would displace
//! the take by half its value and, worse, by an odd offset that swaps the two channels. The engine
//! never multiplies a position by a channel count - that happens in exactly two places in
//! `track.rs`, on the way into and out of a buffer, and nowhere else.
//!
//! The derivation is likewise untouched by phase 2 and it is what keeps *layers* aligned: whatever
//! the musician hears - click, first layer, third layer - travels the same `L_out`, and whatever he
//! plays travels the same `L_in`. Every take, in every bar, on every track, mono or stereo, is
//! therefore stored on the same grid. The per-layer arithmetic that follows from it lives in
//! `track.rs`.
//!
//! Playback needs no correction at all. The loop is written to the output at position `p`, and
//! reaches the ears through exactly the same `L_out` as the click at `p` - so click and loop line
//! up for the listener by construction. Only the recording path is asymmetric, and only it is
//! compensated. Compensating both would be the classic double correction and would put the loop
//! 827 frames *ahead* of the click.
//!
//! Sign check that survives a sleepless night: `R` is positive, the input is always *late*,
//! therefore recorded material must be moved *earlier*, therefore `m = k - R`, never `k + R`.
//! `tests.rs` nails this down with a simulated 827-frame loopback and zero tolerance, for a mono
//! and for a stereo track; getting the sign wrong misses by 1654 frames.
//!
//! One consequence worth knowing: while recording, the write pointer trails the play pointer by
//! `R` frames. The loop must therefore be longer than `R` - a 17 ms loop cannot work - and the
//! last `R` frames of a fresh take are still being written while the head of the loop is already
//! playing. The zeroed layer buffer covers that gap with silence instead of stale memory.
//!
//! # Where the effects sit, and where they deliberately do not
//!
//! ```text
//!  Eingang ─┬─────────────────────────────────────────────► Loop-Puffer   (trocken!)
//!           │
//!           └─► Mithoeren ─┐
//!                          ├─► Effektkette ─► Panorama ─┐
//!  Ebenen-Summe ───────────┘   (stereo)                 ├─► Mixbus (L/R) ─► Ausgang
//!  Klick ───────────────────────────────────────────────┘   (mittig)
//! ```
//!
//! [`EngineCore::record_input`] reads the raw input slice and writes it into the layer buffer
//! untouched - the chain is not in that path and cannot get into it, because `record_input` never
//! sees a `Chain`. What is recorded is dry, always, and that is the decision the plan states:
//! *"aufgenommen wird trocken"*. An effect baked into a take cannot be undone; one on playback can
//! be re-dialled between two passes of the same loop.
//!
//! The click stays outside every chain and outside every panner. It is a reference, not music: it
//! goes into both bus channels at the same level, so it sits in the middle of the head wherever the
//! tracks have been placed, and a compressed metronome with reverb on it would be a worse
//! reference.

use super::command::{
    BUFFER_KINDS, BufferEndpoint, Command, CommandReceiver, MAX_TRACKS, Refusal, Status,
    StatusSender, TrackStatus,
};
use super::frame::{Channels, Frame};
use super::metro::Metronome;
use super::timeline::Timeline;
use super::track::{MAX_LAYERS, Track, TrackLatency};

/// Size of the waiting room for commands that have been taken out of the queue but are not due
/// yet. Allocated once, in the control thread; the audio thread only ever pushes into it and
/// removes from it.
const SCHEDULED_SLOTS: usize = 64;

/// Channels in one output frame. The mix bus is stereo without exception, whatever the tracks are.
pub const OUT_CHANNELS: usize = 2;

/// Everything the engine needs to exist. Assembled by the control thread.
pub struct EngineConfig {
    pub timeline: Timeline,
    /// Default roundtrip latency in **frames**, see the module comment. It applies to every track
    /// that carries no measured value of its own, which is all of them in a setup with a single
    /// source - nobody should have to maintain eight numbers for one microphone.
    pub latency_frames: u64,
    /// Number of channels in one interleaved device input frame.
    pub input_channels: usize,
    /// The tracks, with their input channels. Built by the control thread, never resized here.
    pub tracks: Vec<Track>,
    /// Empty vectors with room for the buffer stock, one per buffer kind (mono, stereo). Their
    /// capacities are the stock sizes.
    pub spares: [Vec<Vec<f32>>; BUFFER_KINDS],
    /// Length in **frames** a layer buffer must have to be usable. In samples that is this times
    /// the channel count of the track it is meant for.
    pub layer_frames: u64,
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
    /// Default compensation for tracks without one of their own, in frames.
    latency: u64,
    /// Musical timeline position of the next output sample.
    out_pos: u64,
    /// Number of input frames consumed so far; also the absolute index of the next one.
    in_index: u64,
    input_channels: usize,
    tracks: Vec<Track>,
    /// Prepared, zeroed buffers waiting to become layers, one stock per kind (mono, stereo).
    /// Neither grows past its capacity, so pushing into it cannot allocate.
    spares: [Vec<Vec<f32>>; BUFFER_KINDS],
    spare_slots: [usize; BUFFER_KINDS],
    layer_frames: u64,
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
    output_peak: [f32; 2],
    ignored_commands: u64,
    refusal: Refusal,
    buffers_taken: [u64; BUFFER_KINDS],
    stopped: bool,
}

impl EngineCore {
    pub fn new(cfg: EngineConfig) -> Self {
        let spare_slots = [cfg.spares[0].capacity(), cfg.spares[1].capacity()];
        let mut tracks = cfg.tracks;
        // The control thread builds the tracks and knows what each of them was configured with;
        // only here is the default known, so this is where the two are added up. From now on every
        // track answers with a plain number.
        for track in tracks.iter_mut() {
            track.resolve_latency(cfg.latency_frames);
        }
        Self {
            timeline: cfg.timeline,
            metro: Metronome::new(cfg.timeline.sample_rate()),
            latency: cfg.latency_frames,
            out_pos: 0,
            in_index: 0,
            input_channels: cfg.input_channels.max(1),
            tracks,
            spares: cfg.spares,
            spare_slots,
            layer_frames: cfg.layer_frames,
            monitor_gain: cfg.monitor_gain,
            click: cfg.click,
            click_gain: cfg.click_gain,
            commands: cfg.commands,
            scheduled: Vec::with_capacity(SCHEDULED_SLOTS),
            status: cfg.status,
            buffers: cfg.buffers,
            status_interval: cfg.status_interval.max(1),
            since_status: 0,
            output_peak: [0.0; 2],
            ignored_commands: 0,
            refusal: Refusal::None,
            buffers_taken: [0; BUFFER_KINDS],
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
        self.spares.iter().map(Vec::len).sum()
    }

    #[cfg(test)]
    pub fn spare_count_of(&self, channels: Channels) -> usize {
        self.spares[channels.index()].len()
    }

    /// One audio block.
    ///
    /// `input` holds the interleaved device input frames consumed for this block, starting at
    /// absolute input index `in_index`; it may be shorter than `output` when the input FIFO ran
    /// dry, and then simply fewer input frames are accounted for - the mapping `m = k - R` stays
    /// intact because `k` counts consumed frames, not elapsed time.
    ///
    /// `output` is the **interleaved stereo** output block starting at `out_pos`, i.e. two samples
    /// per frame. It is fully overwritten. Its length must be even; a stray odd sample at the end
    /// is left untouched rather than half a frame being produced.
    pub fn process(&mut self, input: &[f32], output: &mut [f32]) {
        self.refill_spares();
        self.collect_commands();
        self.publish_tempo();
        let in_frames = input.len() / self.input_channels;
        let out_frames = output.len() / OUT_CHANNELS;

        self.render_with_commands(input, in_frames, output, out_frames);
        self.clamp_and_meter(output);
        self.record_input(input, in_frames);

        self.out_pos += out_frames as u64;
        self.in_index += in_frames as u64;
        self.since_status += out_frames as u64;
        if self.since_status >= self.status_interval {
            self.since_status = 0;
            self.publish_status();
        }
    }

    /// Collect prepared buffers from the control thread. Bounded work: at most `spare_slots` pops
    /// per kind, and a buffer that fits neither stock goes straight back.
    fn refill_spares(&mut self) {
        while self.spares[0].len() < self.spare_slots[0]
            || self.spares[1].len() < self.spare_slots[1]
        {
            let Some(buffer) = self.buffers.take_new() else {
                break;
            };
            let kind = self.kind_of(&buffer);
            // **Every** buffer taken out of the install queue is counted, whatever happens to it
            // next. The control thread works out how many are still in flight as
            // `pushed - buffers_taken`; one that disappeared from that sum without being counted
            // would make it believe a buffer is on its way forever and supply one fewer for the
            // rest of the session - which surfaces much later as "kein vorbereiteter Puffer frei"
            // on an overdub. A length that belongs to no current kind can only be a leftover from
            // a tempo change, and the counters are reset there, so charging it to the mono stock
            // is harmless.
            self.buffers_taken[kind.unwrap_or(0)] += 1;
            match kind {
                // The push never allocates: each stock's capacity is its slot count, and this is
                // the only place anything is pushed into one.
                Some(kind) if self.spares[kind].len() < self.spare_slots[kind] => {
                    self.spares[kind].push(buffer);
                }
                // Wrong length (left over from a tempo change) or a stock that is already full:
                // hand it back rather than hold on to something unusable.
                _ => self.buffers.retire(buffer),
            }
        }
    }

    /// Which stock a buffer belongs to, by its length in samples.
    #[inline]
    fn kind_of(&self, buffer: &[f32]) -> Option<usize> {
        (0..BUFFER_KINDS).find(|&k| buffer.len() as u64 == self.layer_frames * (k as u64 + 1))
    }

    /// One prepared buffer of the right channel count, or `None` if that stock ran dry. Buffers of
    /// the wrong length (left over from a tempo change) are sent back instead of being used.
    fn take_spare(&mut self, channels: Channels) -> Option<Vec<f32>> {
        let kind = channels.index();
        let wanted = self.layer_frames * channels.count() as u64;
        while let Some(buffer) = self.spares[kind].pop() {
            if buffer.len() as u64 == wanted {
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

    /// Tell every chain what a quarter note is worth, so a tempo-synchronous delay knows its time.
    ///
    /// Once per block, not per sample, and the chain ignores it unless the number really changed -
    /// which it only does after a `SetTempo`. A quarter note rather than the engine's "beat":
    /// with a beat unit of 8 a beat is an eighth, but a musician who asks for a quarter-note
    /// delay means a quarter note either way.
    #[inline]
    fn publish_tempo(&mut self) {
        let quarter = self.timeline.samples_per_quarter();
        for track in &mut self.tracks {
            track.fx_mut().set_quarter_samples(quarter);
        }
    }

    /// Render the output block, splitting it at every command boundary.
    ///
    /// This is the heart of the sample accuracy: a command timed for the middle of the block takes
    /// effect exactly there, because the block is rendered as two segments with the command
    /// applied in between - not at the start of the block and not at its end.
    ///
    /// `input` and `in_frames` come along because monitoring is now part of the per-track signal:
    /// a track's live input is summed with its layers *before* its effect chain, so the two cannot
    /// be mixed in two separate passes any more.
    fn render_with_commands(
        &mut self,
        input: &[f32],
        in_frames: usize,
        output: &mut [f32],
        out_frames: usize,
    ) {
        let block_end = self.out_pos + out_frames as u64;
        let mut cursor = 0usize;
        loop {
            // Position at which rendering has to pause for the next command. A command whose time
            // has already passed (control thread was late) acts right here rather than in the past.
            let due = self.next_due(block_end, self.out_pos + cursor as u64);
            let boundary = match due {
                Some((_, at)) => ((at - self.out_pos) as usize).min(out_frames),
                None => out_frames,
            };
            if boundary > cursor {
                let start = self.out_pos + cursor as u64;
                // Frame indices become sample offsets only here, on the way into the output slice.
                self.render(
                    &mut output[cursor * OUT_CHANNELS..boundary * OUT_CHANNELS],
                    start,
                    input,
                    cursor,
                    in_frames,
                );
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

    /// Click plus every track for one uninterrupted segment, summed into the stereo bus.
    ///
    /// Per track, in this order: the sum of its audible layers, plus its monitored live input,
    /// through its effect chain, through its panner. Monitoring is a straight pass-through of that
    /// track's input channel or channel pair, switched independently of whether the track is
    /// playing, so the musician can play live over his own loop. Its delay is the device roundtrip
    /// and has nothing to do with loop alignment - the loop is aligned by position, not by when a
    /// sample travels through the engine.
    ///
    /// The click is added to both bus channels at the same level, so it stays in the middle
    /// whatever the tracks are panned to - it is a reference, not part of the arrangement.
    ///
    /// `in_base` is the index, inside this block, of the first output *frame* of this segment; it
    /// is what pairs an output frame with the device input frame that arrived with it. When the
    /// input FIFO ran short (`in_frames` below the block length) the monitor contributes silence
    /// for the rest of the block - exactly what the separate monitor pass used to do.
    #[inline]
    fn render(
        &mut self,
        out: &mut [f32],
        start: u64,
        input: &[f32],
        in_base: usize,
        in_frames: usize,
    ) {
        let channels = self.input_channels;
        let monitor_gain = self.monitor_gain;
        let click = self.click;
        let click_gain = self.click_gain;
        // Disjoint field borrows: the sample loop needs `metro`/`timeline` read-only and `tracks`
        // mutably, and those are different fields of `self`.
        let metro = &self.metro;
        let timeline = &self.timeline;
        let tracks = &mut self.tracks;

        for (i, slot) in out.chunks_exact_mut(OUT_CHANNELS).enumerate() {
            let pos = start + i as u64;
            let mut bus = if click {
                Frame::mono(metro.sample_at(timeline, pos) * click_gain)
            } else {
                Frame::SILENT
            };
            let frame = in_base + i;
            for track in tracks.iter_mut() {
                // The live input of this track, already stereo: one channel fanned out on a mono
                // track, the recorded pair on a stereo one.
                let monitor = if track.monitor() && frame < in_frames {
                    let base = frame * channels;
                    let input_of = |c: usize| match input.get(base + track.input().device_channel(c))
                    {
                        Some(&s) => s * monitor_gain,
                        None => 0.0,
                    };
                    match track.channels() {
                        Channels::Mono => Frame::mono(input_of(0)),
                        Channels::Stereo => Frame::new(input_of(0), input_of(1)),
                    }
                } else {
                    Frame::SILENT
                };
                bus = bus.added(track.render(pos, monitor));
            }
            slot[0] = bus.l;
            slot[1] = bus.r;
        }
    }

    /// Clamp both bus channels into the usable range and meter each of them separately - a mix
    /// that clips only on one side has to show that on the side it happens on.
    #[inline]
    fn clamp_and_meter(&mut self, output: &mut [f32]) {
        let mut peak = [0.0f32; 2];
        for frame in output.chunks_exact_mut(OUT_CHANNELS) {
            for (c, slot) in frame.iter_mut().enumerate() {
                let v = slot.clamp(-1.0, 1.0);
                *slot = v;
                let a = v.abs();
                if a > peak[c] {
                    peak[c] = a;
                }
            }
        }
        for c in 0..OUT_CHANNELS {
            self.output_peak[c] = self.output_peak[c].max(peak[c]);
        }
    }

    /// Write the consumed input frames into the running takes, latency-compensated, and meter every
    /// track's input channels on the way.
    ///
    /// A stereo track reads two device channels of the same input frame and stores them as one
    /// frame of its loop buffer. Both therefore land at the same musical position `m = k - R`,
    /// which is what keeps the two sides of a stereo take exactly aligned - see the module comment
    /// on why `R` is a frame count and never a sample count.
    ///
    /// `R` is read **per track**, because two tracks need not share a way in. The same input frame
    /// `k` therefore lands at different musical positions on two tracks with different values -
    /// displaced by exactly the difference of the two, which is the point of the whole exercise.
    #[inline]
    fn record_input(&mut self, input: &[f32], frames: usize) {
        let ch = self.input_channels;
        for j in 0..frames {
            let k = self.in_index + j as u64;
            let base = j * ch;
            for t in 0..self.tracks.len() {
                let latency = self.tracks[t].latency_frames();
                // Nothing that arrives in the first R frames of the session was played after
                // position 0, so there is no musical position to store it at.
                let recordable = k >= latency;
                // The one line the whole compensation comes down to. See the module comment.
                let pos = k.wrapping_sub(latency);
                let input_of = self.tracks[t].input();
                let count = self.tracks[t].channels().count();
                // One device sample per recorded channel. A frame that is short (the device
                // delivered fewer channels than a track asks for) is skipped whole rather than
                // half-written, which would put one side of a stereo take one frame out.
                let mut recorded = Frame::SILENT;
                let mut complete = true;
                for c in 0..count {
                    match input.get(base + input_of.device_channel(c)) {
                        Some(&s) => recorded.set_channel(c, s),
                        None => complete = false,
                    }
                }
                if !complete {
                    continue;
                }
                for c in 0..count {
                    self.tracks[t].note_input_peak(c, recorded.channel(c).abs());
                }
                // A mono track's single sample is fanned out here, so `Track::write` receives a
                // frame in both cases and stores whatever its channel count says.
                let frame = if count == 1 {
                    Frame::mono(recorded.l)
                } else {
                    recorded
                };
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
                    self.write_into_take(t, pos, frame);
                    continue;
                }
                self.tracks[t].write(pos, frame);
            }
        }
    }

    /// Write one frame into whatever take is open on this track, if the position belongs to it.
    #[inline]
    fn write_into_take(&mut self, track: usize, pos: u64, frame: Frame) {
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
        self.tracks[track].write(pos, frame);
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
                let start = at.max(self.musical_input_pos(track));
                if self.tracks[track].take_is_finishing() {
                    // The running take is only waiting for its tail; queue this one behind it.
                    self.schedule_take(track, start, true);
                } else {
                    self.clear_track(track);
                    self.begin_take(track, start, true);
                }
            }
            Command::StartOverdub { track, at } => {
                let start = at.max(self.musical_input_pos(track));
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
            Command::SetPan { track, pan } => self.tracks[track].set_pan(pan),
            // Two integer additions and a store, on a track that already exists. Nothing is
            // allocated and nothing already recorded is moved: the new value decides where the
            // *next* input frame goes, which is the only honest way to change it while a loop is
            // playing - the material in the buffer was stored with the old one and stays where it
            // was played.
            Command::SetTrackLatency { track, latency } => {
                let default = self.latency;
                self.tracks[track].set_latency(latency, default);
            }
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
                layer_frames,
            } => {
                // A tempo change redefines what every sample position means, so it is only safe
                // while nothing is recorded, recording or playing.
                if self.tracks.iter().any(|t| {
                    t.layer_count() > 0 || t.take().is_some() || t.playing()
                }) {
                    self.refuse(Refusal::Tempo);
                } else {
                    self.timeline = Timeline::new(self.timeline.sample_rate(), bpm, signature);
                    // Only a *changed* length invalidates the stocks, and the two sides of the
                    // buffer accounting have to agree on when that happened - `LayerPool::
                    // set_layer_frames` makes exactly the same comparison. A tempo change that
                    // leaves the loop the same length (a different time signature at the same
                    // BPM, say) keeps its buffers.
                    if layer_frames != self.layer_frames {
                        self.layer_frames = layer_frames;
                        // Both stocks are the wrong length now; hand them back and let the control
                        // thread send buffers that fit.
                        for kind in 0..BUFFER_KINDS {
                            while let Some(buffer) = self.spares[kind].pop() {
                                self.buffers.retire(buffer);
                            }
                        }
                        // Start the buffer accounting over, as the pool does. Buffers of the old
                        // length that are still in the install queue are then charged to a stock
                        // they were not pushed from, which makes the control thread supply *too
                        // many* for a moment - the harmless direction, because the stock size in
                        // the status snapshot puts a hard ceiling on it. Without the reset the
                        // error would point the other way and be permanent.
                        self.buffers_taken = [0; BUFFER_KINDS];
                    }
                }
            }
            // The effect commands are the cheapest kind there is: they change a few numbers on a
            // chain that already exists. Nothing is allocated, and the coefficient arithmetic
            // behind a preset is a few dozen transcendental operations, once.
            Command::SetFxBypass { track, on } => self.tracks[track].fx_mut().set_bypass(on),
            Command::SetFxEnabled { track, slot, on } => {
                self.tracks[track].fx_mut().set_enabled(slot, on)
            }
            Command::SetFxParam { track, param } => self.tracks[track].fx_mut().set_param(param),
            Command::LoadFxPreset { track, preset } => {
                self.tracks[track].fx_mut().load_preset(preset)
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
        let channels = self.tracks[track].channels();
        let Some(buffer) = self.take_spare(channels) else {
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
        let channels = self.tracks[track].channels();
        let Some(buffer) = self.take_spare(channels) else {
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

    /// Musical position of the next input frame to be consumed **on this track**. Two tracks with
    /// different compensation are at different musical positions at the same instant, and a
    /// command that must not land in the past has to be measured against the right one.
    #[inline]
    fn musical_input_pos(&self, track: usize) -> u64 {
        let latency = match self.tracks.get(track) {
            Some(track) => track.latency_frames(),
            None => self.latency,
        };
        self.in_index.saturating_sub(latency)
    }

    fn publish_status(&mut self) {
        let here = self.timeline.locate(self.out_pos);
        let in_index = self.in_index;
        let mut tracks = [TrackStatus::default(); MAX_TRACKS];
        for (i, slot) in tracks.iter_mut().enumerate().take(self.tracks.len()) {
            let track = &mut self.tracks[i];
            let musical_input = in_index.saturating_sub(track.latency_frames());
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
                input_channels: {
                    let [a, b] = track.input().pair();
                    [a.min(255) as u8, b.min(255) as u8]
                },
                channels: track.channels().count() as u8,
                pan: track.pan(),
                latency: track.latency(),
                latency_frames: track.latency_frames().min(u32::MAX as u64) as u32,
                fx: track.fx_mut().status(),
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
            spares: [self.spares[0].len() as u32, self.spares[1].len() as u32],
            buffers_taken: self.buffers_taken,
            stopped: self.stopped,
        };
        self.output_peak = [0.0; 2];
        self.status.push(status);
    }
}

/// Number of **frames** the maximum loop needs, plus a small margin. Also the frame count of one
/// layer buffer - layers are allocated in loop length, never in some blanket maximum. In samples a
/// buffer is this times the channel count of the track it is meant for.
///
/// Bar boundaries are rounded individually, so `bars` bars are not always the same number of
/// frames; the margin covers that without a second allocation.
pub fn loop_capacity(timeline: &Timeline, bars: u32) -> u64 {
    timeline.span_bars(0, bars) + 64
}

/// Copy one stereo bus frame into one device output frame, whatever the device's channel count is.
///
/// * **Two or more channels**: channel 0 is left, channel 1 is right, and any further pair repeats
///   them. That is what makes the loop audible on a four-output interface without a routing dialog,
///   and it is what the loopback measurement expects - a cable in output 1 carries the left bus.
/// * **One channel**: the two sides are summed at half level. A mono output that simply dropped the
///   right channel would silence anything panned hard right, which is a far worse surprise than a
///   fold-down; and half level keeps a centred track at the level it had in the mono engine.
#[inline]
pub fn spread_frame(bus: &[f32], out: &mut [f32]) {
    let l = bus.first().copied().unwrap_or(0.0);
    let r = bus.get(1).copied().unwrap_or(l);
    if out.len() == 1 {
        out[0] = (l + r) * 0.5;
        return;
    }
    for (c, slot) in out.iter_mut().enumerate() {
        *slot = if c % OUT_CHANNELS == 0 { l } else { r };
    }
}

/// Validate a loop configuration before any buffer is allocated.
///
/// `latency` is the **largest** compensation any track uses, because the loop has to be longer
/// than the write pointer trails the play pointer on the worst of them - see [`max_latency`].
pub fn check_loop(timeline: &Timeline, bars: u32, latency: u64) -> Result<(), String> {
    if bars == 0 {
        return Err("--bars muss mindestens 1 sein.".to_string());
    }
    let len = timeline.span_bars(0, bars);
    if len <= latency {
        return Err(format!(
            "Der Loop waere mit {len} Frames kuerzer als die Latenzkompensation ({latency} Frames). \
             Mehr Takte, hoeheres Tempo pruefen oder --latency-frames korrigieren."
        ));
    }
    Ok(())
}

/// The largest compensation in a set of tracks, given the engine's default - the number
/// [`check_loop`] has to be fed.
///
/// The default is included even when every track has its own, because a track added later would
/// follow it, and because a session without any track is still checked.
pub fn max_latency(default: u64, tracks: impl IntoIterator<Item = TrackLatency>) -> u64 {
    tracks
        .into_iter()
        .map(|latency| latency.resolve(default))
        .fold(default, u64::max)
}

/// Memory one full session can occupy at most, in bytes - the number behind the layer ceiling.
///
/// `track_channels` is the sum of every track's channel count (a mono track counts 1, a stereo one
/// 2), `spare_channels` likewise for the prepared buffers. A session of mono tracks therefore
/// really does cost half of what the same session in stereo would - which is the whole reason loop
/// buffers are not stereo across the board.
pub fn max_memory_bytes(layer_frames: u64, track_channels: usize, spare_channels: usize) -> u64 {
    layer_frames * 4 * (track_channels as u64 * MAX_LAYERS as u64 + spare_channels as u64)
}

/// German summary of the layer limits, for the startup banner.
pub fn limits_line(
    layer_frames: u64,
    tracks: usize,
    track_channels: usize,
    spare_channels: usize,
) -> String {
    format!(
        "Ebenen:   bis zu {} je Track, {:.1} MB je Mono-Ebene ({:.1} MB stereo), \
         hoechstens {:.0} MB fuer {} Tracks",
        MAX_LAYERS,
        layer_frames as f64 * 4.0 / 1_048_576.0,
        layer_frames as f64 * 8.0 / 1_048_576.0,
        max_memory_bytes(layer_frames, track_channels, spare_channels) as f64 / 1_048_576.0,
        tracks
    )
}

/// Buffer stock sizes for a set of tracks: `slots` prepared buffers of each kind that is actually
/// used, and none at all of a kind no track can ask for.
pub fn spare_slots_for(tracks: &[Channels], slots: usize) -> [usize; BUFFER_KINDS] {
    let mut needed = [0usize; BUFFER_KINDS];
    for &channels in tracks {
        needed[channels.index()] = slots;
    }
    needed
}

/// Total channel count of a set of tracks - the multiplier in [`max_memory_bytes`].
pub fn total_channels(tracks: &[Channels]) -> usize {
    tracks.iter().map(|c| c.count()).sum()
}

/// Channel count of a stock of prepared buffers, for [`max_memory_bytes`].
pub fn spare_channels(slots: [usize; BUFFER_KINDS]) -> usize {
    slots
        .iter()
        .enumerate()
        .map(|(kind, &n)| n * (kind + 1))
        .sum()
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
