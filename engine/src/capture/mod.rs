//! Recording a performance to disk: one stereo stem per track (with its effects), an optional dry
//! stem per track, and the mix - all the same length, sample-synchronous, starting on the same
//! frame.
//!
//! ```text
//!  Audio-Thread                                     Schreib-Thread                Platte
//!  ┌───────────────────────────────────┐  rtrb<f32>  ┌───────────────────────┐   ┌──────────────┐
//!  │ render: Stem-Abgriff je Spur, Mix │ ──────────► │ entschachteln, 24 Bit │──►│ 01 voice.wav │
//!  │ record_input: Live-Signal bei     │  rtrb<Gap>  │ Luecken -> Stille     │   │ ...          │
//!  │   m = k - R einsortiert           │ ──────────► │ Header jede Sekunde   │   │ Mix.wav      │
//!  │ Sammelpuffer, `lag` Frames Verzug │             └───────────────────────┘   │ session.json │
//!  └───────────────────────────────────┘                                         └──────────────┘
//! ```
//!
//! # What lands in which file
//!
//! Every file is indexed on the **musical timeline** - the engine's `out_pos` axis that bars,
//! beats, the click and every take are measured on. Frame 0 of every file is the capture's
//! `start` position (bar 1 of the first section for a score run).
//!
//! * **Stem** (`01 voice.wav`, stereo): this track's contribution to the Main bus, exactly as it
//!   was added into the Main bus - after the built-in effect chain, the plugin inserts, the fader,
//!   mute/solo and the panner, before the Main bus inserts and the Main bus volume. On a device
//!   whose two buses share one output ("collapsed", a two-output interface) that is everything the
//!   track sends anywhere, because there is only one mix. *Plus* the live complement below.
//! * **Live complement** (part of the stem): while the track records, overdubs or hears through,
//!   the input it is playing is added to the stem **latency-aligned** - every input frame `k` is
//!   placed at `m = k - R`, the same position the recording puts it at, so the first pass of a
//!   loop and its playback afterwards sit on the same samples. It goes through the track's fader,
//!   mute/solo and panner, *not* through the effect chain or the inserts (those run once per bus in
//!   real time on what the room hears; they cannot be fed a second, time-shifted signal). It is only
//!   added when the live input does **not** already reach Main through this track (monitoring off,
//!   or the monitored input routed to the Monitor bus only) - otherwise it would be in the stem
//!   twice. When it does reach Main it is in the stem the way the room heard it: through the
//!   effects, and `R` frames late, like everything the hear-through path plays.
//! * **Dry stem** (`01 voice (dry).wav`, the track's own channel count): loop playback (all
//!   audible layers with their layer gains, nothing else) plus the raw live input while the track
//!   records, overdubs or hears through, latency-aligned the same way. Before effects, inserts,
//!   fader, mute/solo and panner, and without the monitor volume. On a mono track this is mono.
//! * **Mix** (`Mix.wav`, stereo): the Main bus after its inserts, before the Main bus volume and
//!   without the click. With no bus inserts it is exactly the sum of the stems minus their live
//!   complements.
//!
//! # Why the files lag behind the music
//!
//! A live input frame is placed `R` frames *earlier* than the output frame it arrived with. The
//! audio thread therefore collects every file in a small ring of pre-allocated frames
//! ([`CaptureSession`]) and only hands a frame on once nothing can land on it any more: `lag`
//! frames behind the output position, where `lag` covers the largest compensation of any track
//! plus two driver blocks. Live frames that arrive even later (a compensation raised mid-run past
//! that margin) are counted as `late` and dropped rather than smeared.
//!
//! # Real time
//!
//! The audio thread only adds into the collecting ring and copies finished frames into a lock-free
//! queue. When that queue is full (the disk could not keep up), the frames are **dropped and a gap
//! is recorded** instead: the writer fills the gap with exactly as many frames of silence, so every
//! file stays sample-synchronous, and the incident is counted. Nothing in here blocks, allocates or
//! touches a file.

pub mod run;
pub mod session;
pub mod smf;
pub mod wav;
pub mod writer;

#[cfg(test)]
mod tests;

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use crate::engine::bus::{Bus, BusSend};
use crate::engine::frame::{Channels, Frame};
use crate::engine::track::Track;

/// Frames of safety on top of the largest compensation before a frame is handed on.
pub const LAG_MARGIN: u64 = 2048;
/// Below this a frame counts as silent for the tail: -80 dBFS.
pub const SILENCE: f32 = 1.0e-4;
/// Silence that ends the tail after a stop.
pub const TAIL_SILENCE_SECS: f64 = 0.5;
/// The longest tail after a stop, whatever still rings.
pub const MAX_TAIL_SECS: f64 = 6.0;
/// Seconds of audio the queue between audio and writer thread holds.
pub const QUEUE_SECONDS: f64 = 4.0;
/// "No stop requested yet."
pub const NO_STOP: u64 = u64::MAX;

/// Which files one capture writes, in the order their channels sit in one super-frame:
/// `[mix L, mix R, track 0 stem L, R, (track 0 dry 1|2), track 1 stem L, R, ...]`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Layout {
    /// Channel count of each track, which is the channel count of its dry stem.
    pub tracks: Vec<Channels>,
    /// Whether dry stems are written at all.
    pub dry: bool,
}

impl Layout {
    /// Samples in one super-frame.
    pub fn width(&self) -> usize {
        2 + self
            .tracks
            .iter()
            .map(|c| 2 + if self.dry { c.count() } else { 0 })
            .sum::<usize>()
    }

    /// Offset of track `t`'s stem in a super-frame.
    pub fn stem_offset(&self, t: usize) -> usize {
        2 + self.tracks[..t]
            .iter()
            .map(|c| 2 + if self.dry { c.count() } else { 0 })
            .sum::<usize>()
    }

    /// Offset of track `t`'s dry stem, if dry stems are written.
    pub fn dry_offset(&self, t: usize) -> Option<usize> {
        self.dry.then(|| self.stem_offset(t) + 2)
    }
}

/// A run of frames that was dropped because the queue was full: `frames` frames of silence belong
/// after the first `after` frames that did make it through.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Gap {
    pub after: u64,
    pub frames: u64,
}

/// Counters both sides can read. Atomics only.
#[derive(Debug)]
pub struct Shared {
    /// Absolute output position from which on nothing new is played; [`NO_STOP`] while running.
    stop_at: AtomicU64,
    /// Frames handed on so far - the length every file has (or will have once the gaps are filled).
    frames: AtomicU64,
    /// Frames that had to be replaced by silence because the queue was full.
    dropped: AtomicU64,
    /// How often the queue ran full - one incident per run of dropped blocks.
    overflows: AtomicU64,
    /// Live input frames that arrived too late for their place and were left out.
    late: AtomicU64,
    /// Set once the last frame has been handed on.
    done: AtomicBool,
    /// Whether any frame so far carried more than [`SILENCE`].
    audible: AtomicBool,
}

impl Shared {
    fn new() -> Self {
        Self {
            stop_at: AtomicU64::new(NO_STOP),
            frames: AtomicU64::new(0),
            dropped: AtomicU64::new(0),
            overflows: AtomicU64::new(0),
            late: AtomicU64::new(0),
            done: AtomicBool::new(false),
            audible: AtomicBool::new(false),
        }
    }

    /// Ask the audio thread to end the capture at `pos` plus the tail.
    pub fn request_stop(&self, pos: u64) {
        self.stop_at.fetch_min(pos, Ordering::AcqRel);
    }

    pub fn stop_at(&self) -> Option<u64> {
        let v = self.stop_at.load(Ordering::Acquire);
        (v != NO_STOP).then_some(v)
    }

    pub fn frames(&self) -> u64 {
        self.frames.load(Ordering::Acquire)
    }

    pub fn dropped(&self) -> u64 {
        self.dropped.load(Ordering::Relaxed)
    }

    pub fn overflows(&self) -> u64 {
        self.overflows.load(Ordering::Relaxed)
    }

    pub fn late(&self) -> u64 {
        self.late.load(Ordering::Relaxed)
    }

    pub fn done(&self) -> bool {
        self.done.load(Ordering::Acquire)
    }

    pub fn audible(&self) -> bool {
        self.audible.load(Ordering::Relaxed)
    }
}

/// What a capture is built from.
#[derive(Clone, Debug)]
pub struct CaptureSpec {
    /// Absolute output position of file frame 0.
    pub start: u64,
    pub layout: Layout,
    pub sample_rate: u32,
    /// The longest block `EngineCore::process` is ever called with.
    pub max_block: usize,
    /// The largest compensation of any track, in frames - sizes the collecting ring.
    pub max_latency: u64,
    /// Seconds of audio the queue to the writer holds - [`QUEUE_SECONDS`] in the app. A test that
    /// runs the engine far faster than real time asks for more, so a busy machine cannot turn a
    /// test of something else into an overflow.
    pub queue_seconds: f64,
}

/// One MIDI message an instrument track sent to its instrument, at a file frame.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MidiCapture {
    pub track: u8,
    /// Frame of the recording (0 is its first frame), on the output axis like the stems.
    pub frame: u64,
    pub msg: [u8; 3],
}

/// MIDI messages the queue to the writer holds. A pianist with the pedal sends a few dozen a
/// second; the writer takes them every few milliseconds.
const MIDI_QUEUE: usize = 32_768;

/// Which stem slots one track writes to, precomputed so the audio thread does no arithmetic on the
/// layout.
#[derive(Clone, Copy, Debug)]
struct Slots {
    stem: usize,
    dry: Option<usize>,
    dry_channels: usize,
}

/// The audio-thread half of one capture. Built on the control thread (every buffer is allocated
/// here) and handed to the engine through [`CapturePort::install`].
pub struct CaptureSession {
    start: u64,
    width: usize,
    slots: Vec<Slots>,
    /// The collecting ring: `ring_frames` super-frames, indexed by absolute position modulo its size.
    acc: Vec<f32>,
    ring_frames: u64,
    mask: u64,
    /// Next absolute position to hand on; everything before it is gone.
    next: u64,
    /// How far behind the output position frames are handed on. Set when the engine installs the
    /// session, from the compensation its tracks really have.
    lag: u64,
    max_block: u64,
    data: rtrb::Producer<f32>,
    gaps: rtrb::Producer<Gap>,
    /// A gap that could not be announced yet; no data goes out before it is.
    pending_gap: Option<Gap>,
    /// Frames that made it into the data queue.
    pushed: u64,
    silent_run: u64,
    tail_silence: u64,
    max_tail: u64,
    shared: Arc<Shared>,
    midi: rtrb::Producer<MidiCapture>,
}

/// The writer's half of one capture.
pub struct CaptureReader {
    data: rtrb::Consumer<f32>,
    gaps: rtrb::Consumer<Gap>,
    next_gap: Option<Gap>,
    /// Data frames consumed so far (gaps not counted).
    consumed: u64,
    /// Frames delivered so far, gaps included.
    delivered: u64,
    width: usize,
    layout: Layout,
    shared: Arc<Shared>,
    start: u64,
    sample_rate: u32,
    midi: rtrb::Consumer<MidiCapture>,
}

/// Build both halves of a capture. Control thread only - this is where its memory is allocated.
pub fn capture_pair(spec: &CaptureSpec) -> (Box<CaptureSession>, CaptureReader) {
    let width = spec.layout.width();
    let max_block = spec.max_block.max(1) as u64;
    let ring_frames = (spec.max_latency + LAG_MARGIN + 4 * max_block)
        .next_power_of_two()
        .max(1024);
    let queue =
        ((spec.sample_rate as f64 * spec.queue_seconds) as usize).max(4 * max_block as usize);
    let (data_tx, data_rx) = rtrb::RingBuffer::<f32>::new(queue * width);
    let (gap_tx, gap_rx) = rtrb::RingBuffer::<Gap>::new(256);
    let (midi_tx, midi_rx) = rtrb::RingBuffer::<MidiCapture>::new(MIDI_QUEUE);
    let shared = Arc::new(Shared::new());
    let slots = (0..spec.layout.tracks.len())
        .map(|t| Slots {
            stem: spec.layout.stem_offset(t),
            dry: spec.layout.dry_offset(t),
            dry_channels: spec.layout.tracks[t].count(),
        })
        .collect();
    let rate = spec.sample_rate as f64;
    let session = Box::new(CaptureSession {
        start: spec.start,
        width,
        slots,
        acc: vec![0.0; ring_frames as usize * width],
        ring_frames,
        mask: ring_frames - 1,
        next: spec.start,
        lag: spec.max_latency + LAG_MARGIN + 2 * max_block,
        max_block,
        data: data_tx,
        gaps: gap_tx,
        pending_gap: None,
        pushed: 0,
        silent_run: 0,
        tail_silence: (rate * TAIL_SILENCE_SECS) as u64,
        max_tail: (rate * MAX_TAIL_SECS) as u64,
        shared: Arc::clone(&shared),
        midi: midi_tx,
    });
    let reader = CaptureReader {
        data: data_rx,
        gaps: gap_rx,
        next_gap: None,
        consumed: 0,
        delivered: 0,
        width,
        layout: spec.layout.clone(),
        shared,
        start: spec.start,
        sample_rate: spec.sample_rate,
        midi: midi_rx,
    };
    (session, reader)
}

/// Whether this track's live input already reaches the Main bus through its own strip - in which
/// case it is in the stem already, and the latency-aligned complement must not add it again.
#[inline]
pub fn live_reaches_main(track: &Track, collapsed: bool) -> bool {
    if !track.monitor() {
        return false;
    }
    let send: BusSend = track.monitor_send();
    if collapsed {
        !send.is_silent()
    } else {
        send.on(Bus::Main)
    }
}

impl CaptureSession {
    /// Absolute position of file frame 0.
    pub fn start(&self) -> u64 {
        self.start
    }

    pub fn shared(&self) -> &Arc<Shared> {
        &self.shared
    }

    /// Called by the engine when it takes the session over: the lag follows the compensation the
    /// tracks really use, never beyond what the ring can hold.
    pub fn install(&mut self, max_latency: u64) {
        let ceiling = self.ring_frames.saturating_sub(2 * self.max_block);
        self.lag = (max_latency + LAG_MARGIN + 2 * self.max_block).min(ceiling);
    }

    /// The ring offset of absolute position `pos`, if that position is still open.
    #[inline(always)]
    fn slot(&self, pos: u64) -> Option<usize> {
        if pos < self.next || pos >= self.next + self.ring_frames {
            return None;
        }
        Some((pos & self.mask) as usize * self.width)
    }

    #[inline(always)]
    fn add2(&mut self, base: usize, f: Frame) {
        self.acc[base] += f.l;
        self.acc[base + 1] += f.r;
    }

    /// One output frame of one track: `stem` is what it added to the Main bus at `pos`. Also takes
    /// the loop playback for the dry stem straight from the track.
    #[inline]
    pub fn track_frame(&mut self, t: usize, pos: u64, stem: Frame, track: &Track) {
        let Some(base) = self.slot(pos) else {
            return;
        };
        let Some(slots) = self.slots.get(t).copied() else {
            return;
        };
        self.add2(base + slots.stem, stem);
        if let Some(dry) = slots.dry
            && track.playing()
            && !track.held()
        {
            // The loop on its own grid, without the read-ahead for plugin latency: the dry stem is
            // musical position, not what was fed to a plugin.
            let f = track.read(pos);
            self.acc[base + dry] += f.l;
            if slots.dry_channels == 2 {
                self.acc[base + dry + 1] += f.r;
            }
        }
    }

    /// The Main bus at `pos`, after its inserts and before its volume, without the click.
    #[inline]
    pub fn mix_frame(&mut self, pos: u64, mix: Frame) {
        if let Some(base) = self.slot(pos) {
            self.add2(base, mix);
        }
    }

    /// One live input frame of one track, already at its musical position `pos = k - R`. `live`
    /// says whether the track is recording, overdubbing or hearing through right now - only then
    /// is the input music rather than the room.
    #[inline]
    pub fn live_frame(
        &mut self,
        t: usize,
        pos: u64,
        raw: Frame,
        track: &Track,
        live: bool,
        collapsed: bool,
    ) {
        if !live || pos < self.start {
            return;
        }
        let Some(base) = self.slot(pos) else {
            // Before the oldest open frame - it arrived later than the lag allows - or so far
            // ahead that it cannot be a real position. Either way it is left out and counted.
            if self.shared.done.load(Ordering::Relaxed) {
                return;
            }
            self.shared.late.fetch_add(1, Ordering::Relaxed);
            return;
        };
        let Some(slots) = self.slots.get(t).copied() else {
            return;
        };
        if let Some(dry) = slots.dry {
            self.acc[base + dry] += raw.l;
            if slots.dry_channels == 2 {
                self.acc[base + dry + 1] += raw.r;
            }
        }
        if !live_reaches_main(track, collapsed) {
            let placed = track.strip(raw, track.fader_level());
            self.add2(base + slots.stem, placed);
        }
    }

    /// End of one engine block: hand on every frame nothing can land on any more. Returns true
    /// once the capture is complete (stop plus tail), after which the engine retires it.
    pub fn end_block(&mut self, out_end: u64) -> bool {
        if self.shared.done.load(Ordering::Relaxed) {
            return true;
        }
        let stop = self.shared.stop_at.load(Ordering::Acquire);
        if stop <= self.start {
            // Stopped before the first frame was due (during the count-in): nothing to write.
            self.shared.done.store(true, Ordering::Release);
            return true;
        }
        let target = out_end.saturating_sub(self.lag);
        if target <= self.next {
            return false;
        }
        let mut n = 0u64;
        let mut finished = false;
        let mut audible = false;
        while self.next + n < target {
            let pos = self.next + n;
            let base = (pos & self.mask) as usize * self.width;
            let loud = self.acc[base..base + self.width]
                .iter()
                .any(|v| v.abs() > SILENCE);
            audible |= loud;
            n += 1;
            if pos >= stop {
                self.silent_run = if loud { 0 } else { self.silent_run + 1 };
                if self.silent_run >= self.tail_silence || pos + 1 >= stop + self.max_tail {
                    finished = true;
                    break;
                }
            }
        }
        if audible {
            self.shared.audible.store(true, Ordering::Relaxed);
        }
        self.emit(n);
        if finished {
            self.shared.done.store(true, Ordering::Release);
        }
        finished
    }

    /// A message track `t` sent its instrument at output position `pos` - live or from a MIDI loop.
    /// Before the recording's first frame or after its end it is not part of it; a full queue drops
    /// it (and counts nothing: the audio files are what the run is judged by).
    #[inline]
    pub fn midi_event(&mut self, t: usize, pos: u64, msg: [u8; 3]) {
        if pos < self.start || self.shared.done.load(Ordering::Relaxed) {
            return;
        }
        let stop = self.shared.stop_at.load(Ordering::Relaxed);
        if pos >= stop.saturating_add(self.max_tail) {
            return;
        }
        let _ = self.midi.push(MidiCapture {
            track: t.min(255) as u8,
            frame: pos - self.start,
            msg,
        });
    }

    /// End the capture right where it is - a new one replaces it, or the engine is told to stop.
    pub fn finish_now(&mut self) {
        self.shared.done.store(true, Ordering::Release);
    }

    /// Copy `n` frames from `next` on into the queue (or record them as a gap), clear them in the
    /// ring and move on.
    fn emit(&mut self, n: u64) {
        if n == 0 {
            return;
        }
        if let Some(gap) = self.pending_gap
            && self.gaps.push(gap).is_ok()
        {
            self.pending_gap = None;
        }
        let first = (self.next & self.mask) as usize;
        let count = n as usize;
        // The ring may wrap inside these frames: at most two contiguous pieces.
        let head = count.min(self.ring_frames as usize - first);
        let tail = count - head;
        let samples = count * self.width;
        let fits = self.pending_gap.is_none() && self.data.slots() >= samples;
        if fits {
            let a = &self.acc[first * self.width..(first + head) * self.width];
            let b = &self.acc[..tail * self.width];
            if let Ok(chunk) = self.data.write_chunk_uninit(samples) {
                chunk.fill_from_iter(a.iter().chain(b.iter()).copied());
            }
            self.pushed += n;
        } else {
            match self.pending_gap.as_mut() {
                Some(gap) => gap.frames += n,
                None => {
                    self.pending_gap = Some(Gap {
                        after: self.pushed,
                        frames: n,
                    });
                    self.shared.overflows.fetch_add(1, Ordering::Relaxed);
                }
            }
            self.shared.dropped.fetch_add(n, Ordering::Relaxed);
        }
        self.acc[first * self.width..(first + head) * self.width].fill(0.0);
        self.acc[..tail * self.width].fill(0.0);
        self.next += n;
        self.shared
            .frames
            .store(self.next - self.start, Ordering::Release);
    }
}

/// What [`CaptureReader::read`] found.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Read {
    /// This many super-frames were appended.
    Frames(usize),
    /// Nothing there right now; ask again.
    Empty,
    /// The capture is over and every frame has been delivered.
    Finished,
}

impl CaptureReader {
    pub fn layout(&self) -> &Layout {
        &self.layout
    }

    pub fn width(&self) -> usize {
        self.width
    }

    pub fn shared(&self) -> &Arc<Shared> {
        &self.shared
    }

    pub fn start(&self) -> u64 {
        self.start
    }

    pub fn sample_rate(&self) -> u32 {
        self.sample_rate
    }

    /// Every MIDI message that arrived since the last call.
    pub fn take_midi(&mut self, mut f: impl FnMut(MidiCapture)) {
        while let Ok(m) = self.midi.pop() {
            f(m);
        }
    }

    /// Frames delivered so far, silence for gaps included.
    pub fn delivered(&self) -> u64 {
        self.delivered
    }

    /// Append up to `max` super-frames to `out`, gaps filled with silence.
    pub fn read(&mut self, out: &mut Vec<f32>, max: usize) -> Read {
        if self.next_gap.is_none() {
            self.next_gap = self.gaps.pop().ok();
        }
        if let Some(gap) = self.next_gap
            && gap.after <= self.consumed
        {
            let n = (gap.frames as usize).min(max);
            out.resize(out.len() + n * self.width, 0.0);
            self.delivered += n as u64;
            if n as u64 == gap.frames {
                self.next_gap = None;
            } else {
                self.next_gap = Some(Gap {
                    after: gap.after,
                    frames: gap.frames - n as u64,
                });
            }
            return Read::Frames(n);
        }
        let mut available = self.data.slots() / self.width;
        if let Some(gap) = self.next_gap {
            available = available.min((gap.after - self.consumed) as usize);
        }
        let n = available.min(max);
        if n > 0 {
            if let Ok(chunk) = self.data.read_chunk(n * self.width) {
                let (a, b) = chunk.as_slices();
                out.extend_from_slice(a);
                out.extend_from_slice(b);
                chunk.commit_all();
            }
            self.consumed += n as u64;
            self.delivered += n as u64;
            return Read::Frames(n);
        }
        // Nothing to read. Over when the audio side says so (or is gone with the engine) and the
        // queues are empty; a trailing gap that could never be announced is filled up to the
        // length the audio side reported.
        let over = self.shared.done() || self.data.is_abandoned();
        if over && self.data.slots() == 0 && self.gaps.slots() == 0 && self.next_gap.is_none() {
            let total = self.shared.frames();
            if self.delivered < total {
                let n = ((total - self.delivered) as usize).min(max);
                out.resize(out.len() + n * self.width, 0.0);
                self.delivered += n as u64;
                return Read::Frames(n);
            }
            return Read::Finished;
        }
        Read::Empty
    }
}

// ---------------------------------------------------------------------------------------------
// Handing sessions to the engine and back
// ---------------------------------------------------------------------------------------------

/// Control side: new sessions in, finished ones back (they are dropped here, never in the audio
/// thread).
pub struct CapturePort {
    incoming: rtrb::Producer<Box<CaptureSession>>,
    retired: rtrb::Consumer<Box<CaptureSession>>,
}

/// Engine side of the same.
pub struct CaptureEndpoint {
    incoming: rtrb::Consumer<Box<CaptureSession>>,
    retired: rtrb::Producer<Box<CaptureSession>>,
}

pub fn capture_channel() -> (CapturePort, CaptureEndpoint) {
    let (in_tx, in_rx) = rtrb::RingBuffer::new(4);
    let (ret_tx, ret_rx) = rtrb::RingBuffer::new(8);
    (
        CapturePort {
            incoming: in_tx,
            retired: ret_rx,
        },
        CaptureEndpoint {
            incoming: in_rx,
            retired: ret_tx,
        },
    )
}

impl CapturePort {
    /// Hand a session to the engine. It takes over on its next block; a session it was still
    /// running is ended there.
    pub fn install(&mut self, session: Box<CaptureSession>) -> Result<(), String> {
        self.incoming
            .push(session)
            .map_err(|_| "The recorder queue is full.".to_string())
    }

    /// Drop sessions the engine is done with. Call regularly on the control thread.
    pub fn collect(&mut self) {
        while self.retired.pop().is_ok() {}
    }
}

/// The engine's slot: the running session and the way in and out.
pub struct CaptureSlot {
    endpoint: CaptureEndpoint,
    active: Option<Box<CaptureSession>>,
    /// A finished session the return queue had no room for yet.
    retiring: Option<Box<CaptureSession>>,
}

impl CaptureSlot {
    pub fn new(endpoint: CaptureEndpoint) -> Self {
        Self {
            endpoint,
            active: None,
            retiring: None,
        }
    }

    /// Once per block, before rendering. `max_latency` is the largest compensation of any track.
    pub fn collect(&mut self, max_latency: u64) {
        if let Some(old) = self.retiring.take() {
            self.retire(old);
        }
        if self.retiring.is_some() {
            // The return queue is still full; taking another session now could leave two to hand
            // back and nowhere to keep the second.
            return;
        }
        if let Ok(mut fresh) = self.endpoint.incoming.pop() {
            if let Some(mut old) = self.active.take() {
                old.finish_now();
                self.retire(old);
            }
            fresh.install(max_latency);
            self.active = Some(fresh);
        }
    }

    #[inline]
    pub fn session(&mut self) -> Option<&mut CaptureSession> {
        self.active.as_deref_mut()
    }

    /// Once per block, after rendering and recording.
    pub fn end_block(&mut self, out_end: u64) {
        let finished = match self.active.as_deref_mut() {
            Some(session) => session.end_block(out_end),
            None => return,
        };
        if finished
            && self.retiring.is_none()
            && let Some(old) = self.active.take()
        {
            self.retire(old);
        }
    }

    fn retire(&mut self, session: Box<CaptureSession>) {
        if let Err(rtrb::PushError::Full(session)) = self.endpoint.retired.push(session) {
            // Kept and offered again next block; dropping it here would free memory in the
            // audio thread.
            self.retiring = Some(session);
        }
    }
}
