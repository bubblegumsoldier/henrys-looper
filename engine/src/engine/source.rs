//! Instrument sources: a track whose sound is made by a plugin that is played over MIDI, instead of
//! coming in through an input channel of the interface.
//!
//! ```text
//!  MIDI-Treiber-Callback ──(Atomic-Maske des Ports)──┬─► Steuer-Queue ─► Host-Thread ─► Bindings
//!                                                     └─► Schnell-Queue (LiveEvent, Host-Zeit)
//!                                                              │
//!  Audio-Thread:  Port-Feeds ─► Route je Spur ─► Mischstufe (Position m + C, sortiert)
//!                                                   │  Notentracker je Quelle
//!                                                   ▼
//!                                    Instrument (Quellen-Slot) ─► Quellpuffer der Spur
//!                                                   │                    │
//!                                                   │      Mithoeren ◄───┤ (statt Eingangskanal)
//!                                                   │      Aufnahme  ◄───┘ Ausgangsachse, minus C + Plugin-Latenz
//! ```
//!
//! # Why a slot of its own and not an insert
//!
//! An insert processes a signal that is already there. An instrument *is* the signal: it sits where
//! the input channel sits on an audio track, before the built-in chain, the inserts and the panner,
//! and what is recorded from it is its dry output - the same rule as for a microphone. So it gets a
//! slot of its own per track ([`SourceRack`]), filled through its own queue, and a track whose input
//! is [`super::frame::TrackInput::Instrument`] reads its "live input" from that slot's output.
//!
//! # Time: when a key is played, and when it sounds
//!
//! The MIDI driver stamps every message with the host clock (`mach_absolute_time` on macOS, in the
//! same unit cpal stamps the output callback with). The output callback tells the engine when the
//! first frame of the block will be *heard* ([`HostClock::playback_ns`]). An event stamped `t` was
//! therefore played while the musician heard musical position
//!
//! ```text
//! m = out_pos + (t - T_playback) * sample_rate
//! ```
//!
//! which is a moment in the past: `t` is older than the callback and `T_playback` lies one output
//! latency in the future. That `m` is what a loop has to store - the musician played in time with
//! what he heard - and it is exact, because both stamps come from the same clock.
//!
//! Rendering the event "as soon as possible" would put it at offset 0 of the next block, i.e. with
//! a delay that depends on where between two callbacks the key went down - a jitter of up to one
//! block that a recorded loop would keep for ever. Instead every event is rendered at the fixed
//! position `m + C`, with `C = L_out + one block` ([`EngineCore::set_clock`] latches it on the first
//! clocked callback). Every event then has exactly the same delay between key and sound, and the
//! recording takes the instrument's output back by exactly that amount (plus the plugin's own
//! latency, see `process::record_sources`), so a note played on beat `B` lands on `B` in the layer.
//!
//! Without a clock (the offline simulation, the calibration, the CLI) every event goes to the start
//! of the block it arrives in.
//!
//! # Musical time for what is stored
//!
//! [`NoteEvent`] carries a position in **ticks** ([`PPQ`] per quarter note) rather than in samples.
//! Nothing stores notes yet - that is what a MIDI loop will do - but the format is fixed now, so a
//! MIDI layer can be quantised and re-timed without touching what was played.
//!
//! # Real time
//!
//! Everything the audio thread touches is allocated in [`SourceRack::new`]. Instruments and port
//! feeds arrive through lock-free queues and leave through retire queues, to be dropped on the
//! thread that sent them. The engine never drops either itself; if a retire queue is full it leaks
//! rather than call a plugin's destructor inside the callback.

use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use rtrb::{Consumer, Producer, RingBuffer};

use super::command::MAX_TRACKS;
use super::insert::{MAX_BLOCK, PluginTransport};
use super::timeline::Timeline;
use super::track::MAX_LAYERS;

/// Ticks per quarter note. Divisible by 3 and by 5, so triplets and quintuplets land on whole
/// ticks; at 120 BPM and 48 kHz one tick is about 1.6 samples.
pub const PPQ: u32 = 15_360;

/// MIDI input ports the engine can listen to at once.
pub const MAX_PORTS: usize = 16;

/// Events one track can have waiting for their position. Far more than ten fingers and a pedal
/// produce in one block; what does not fit is dropped and counted.
pub const PENDING_EVENTS: usize = 2048;

/// Capacity of a port's fast queue between the driver callback and the audio thread.
pub const FEED_QUEUE: usize = 1024;

/// Output buses an instrument may have. Only bus 0 is heard for now; the others are there so a
/// drum plugin with separate outputs does not need a different trait later.
pub const MAX_SOURCE_BUSES: usize = 4;

/// Note sources a track can have: its live input, and one per (future) MIDI layer.
pub const SOURCES: usize = 1 + MAX_LAYERS;

/// How many messages the audio thread takes out of each queue per callback.
const MESSAGES_PER_BLOCK: usize = 16;

/// An event further in the future than this (in frames) can only come from a clock that jumped;
/// it is played now instead of being held back for ever.
const FUTURE_LIMIT: i64 = 1 << 18;

// ---------------------------------------------------------------------------------------------
// Events
// ---------------------------------------------------------------------------------------------

/// One channel message as it left the driver, with the host time it was stamped with.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LiveEvent {
    /// Host clock in nanoseconds, the same base as [`HostClock::playback_ns`].
    pub host_ns: u64,
    /// Status and data bytes; a one-byte message leaves the third at 0.
    pub msg: [u8; 3],
}

/// One event handed to an instrument: a raw MIDI message at a frame offset inside the block.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BlockEvent {
    pub offset: u32,
    pub msg: [u8; 3],
}

/// Which source an event comes from. Every source has its own note tracker, so stopping a MIDI
/// loop ends exactly the notes that loop started and not the ones the musician is holding.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SourceId(pub u8);

impl SourceId {
    pub const LIVE: SourceId = SourceId(0);
    /// Everything the track's MIDI layers play. Each layer keeps its own tracker as well
    /// (`super::midiloop::MidiClip::tracker`); this one is what a panic ends.
    pub const LOOP: SourceId = SourceId(1);

    /// The source of MIDI layer `n` (0-based). Reserved for the MIDI loop.
    pub fn layer(n: usize) -> SourceId {
        SourceId((1 + n).min(SOURCES - 1) as u8)
    }
}

/// What a channel message is, without its channel.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NoteKind {
    Off,
    On,
    PolyPressure,
    Control,
    Program,
    Pressure,
    Bend,
}

/// A channel message at a musical position, in ticks - the format a MIDI layer stores.
///
/// `tick` counts from whatever origin the owner chooses (a loop's index 0); the conversion to and
/// from frames is [`ticks_of_frames`] / [`frames_of_ticks`]. A note on with velocity 0 is turned
/// into a note off here, once, so nothing downstream has to know that convention.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct NoteEvent {
    pub tick: i64,
    /// 0-based MIDI channel.
    pub channel: u8,
    pub kind: NoteKind,
    pub d1: u8,
    pub d2: u8,
}

impl NoteEvent {
    pub fn from_midi(tick: i64, msg: [u8; 3]) -> Option<Self> {
        let status = msg[0];
        if !(0x80..0xF0).contains(&status) {
            return None;
        }
        let kind = match status & 0xF0 {
            0x80 => NoteKind::Off,
            0x90 if msg[2] == 0 => NoteKind::Off,
            0x90 => NoteKind::On,
            0xA0 => NoteKind::PolyPressure,
            0xB0 => NoteKind::Control,
            0xC0 => NoteKind::Program,
            0xD0 => NoteKind::Pressure,
            _ => NoteKind::Bend,
        };
        Some(Self {
            tick,
            channel: status & 0x0F,
            kind,
            d1: msg[1] & 0x7F,
            d2: msg[2] & 0x7F,
        })
    }

    pub fn to_midi(&self) -> [u8; 3] {
        let nibble = match self.kind {
            NoteKind::Off => 0x80,
            NoteKind::On => 0x90,
            NoteKind::PolyPressure => 0xA0,
            NoteKind::Control => 0xB0,
            NoteKind::Program => 0xC0,
            NoteKind::Pressure => 0xD0,
            NoteKind::Bend => 0xE0,
        };
        [nibble | (self.channel & 0x0F), self.d1, self.d2]
    }
}

/// A span of frames in ticks at the timeline's tempo, rounded to the nearest tick.
pub fn ticks_of_frames(timeline: &Timeline, frames: i64) -> i64 {
    let per_tick = timeline.samples_per_quarter() / f64::from(PPQ);
    (frames as f64 / per_tick).round() as i64
}

/// A span of ticks in frames at the timeline's tempo, rounded to the nearest frame.
pub fn frames_of_ticks(timeline: &Timeline, ticks: i64) -> i64 {
    let per_tick = timeline.samples_per_quarter() / f64::from(PPQ);
    (ticks as f64 * per_tick).round() as i64
}

#[inline]
fn is_channel_message(msg: [u8; 3]) -> bool {
    (0x80..0xF0).contains(&msg[0])
}

/// Controllers that belong to playing rather than to steering the looper: modulation, breath,
/// expression and the three pedals. Every other controller on an instrument's channel stays with
/// the MIDI bindings, so the knobs of a keyboard can still be learned.
#[inline]
pub fn is_playing_controller(cc: u8) -> bool {
    matches!(cc, 1 | 2 | 11 | 64 | 66 | 67)
}

// ---------------------------------------------------------------------------------------------
// The routing mask the driver callback reads
// ---------------------------------------------------------------------------------------------

/// Set while MIDI learn is armed. Learning wins over playing: the next key goes to the learn mode,
/// and the host refuses it there if an instrument plays that key (see the app's MIDI bridge).
static LEARNING: AtomicBool = AtomicBool::new(false);

pub fn set_learning(on: bool) {
    LEARNING.store(on, Ordering::Relaxed);
}

pub fn learning() -> bool {
    LEARNING.load(Ordering::Relaxed)
}

/// Which messages of one input port belong to an instrument. Read by the MIDI driver callback for
/// every message, written by the host thread when the routing changes. Atomics only.
#[derive(Debug, Default)]
pub struct PortRoute {
    /// Bit `c` set: some instrument listens to channel `c` (0-based) of this port.
    channels: AtomicU32,
    /// Per channel, the notes an instrument plays - 128 bits as two words. A note outside every
    /// instrument's range stays with the bindings, which is what lets a pad bank steer the looper
    /// while the keys play the piano on the same channel.
    notes: [[AtomicU64; 2]; 16],
}

impl PortRoute {
    pub fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    /// Replace the whole mask with the union of these routes.
    pub fn set(&self, routes: &[TrackRoute]) {
        let mut channels = 0u32;
        let mut notes = [[0u64; 2]; 16];
        for route in routes {
            for (c, words) in notes.iter_mut().enumerate() {
                if route.channels & (1 << c) == 0 {
                    continue;
                }
                channels |= 1 << c;
                for n in route.low..=route.high.min(127) {
                    words[(n / 64) as usize] |= 1u64 << (n % 64);
                }
            }
        }
        for (c, words) in notes.iter().enumerate() {
            self.notes[c][0].store(words[0], Ordering::Relaxed);
            self.notes[c][1].store(words[1], Ordering::Relaxed);
        }
        self.channels.store(channels, Ordering::Release);
    }

    pub fn clear(&self) {
        self.set(&[]);
    }

    /// Whether any instrument listens on this port at all.
    pub fn active(&self) -> bool {
        self.channels.load(Ordering::Relaxed) != 0
    }

    /// Whether this message goes to an instrument (and therefore not to the bindings).
    #[inline]
    pub fn takes(&self, msg: [u8; 3]) -> bool {
        if !is_channel_message(msg) || learning() {
            return false;
        }
        let c = (msg[0] & 0x0F) as usize;
        if self.channels.load(Ordering::Acquire) & (1 << c) == 0 {
            return false;
        }
        match msg[0] & 0xF0 {
            0x80 | 0x90 | 0xA0 => self.covers(c, msg[1]),
            0xB0 => is_playing_controller(msg[1]),
            0xD0 | 0xE0 => true,
            _ => false,
        }
    }

    /// Whether note `note` on 0-based channel `c` is played by an instrument.
    #[inline]
    pub fn covers(&self, c: usize, note: u8) -> bool {
        let n = note & 0x7F;
        c < 16
            && self.channels.load(Ordering::Acquire) & (1 << c) != 0
            && self.notes[c][(n / 64) as usize].load(Ordering::Relaxed) & (1u64 << (n % 64)) != 0
    }
}

/// Which port, channels and notes feed one instrument track.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TrackRoute {
    /// Bit `p` set: port slot `p` feeds this track.
    pub ports: u32,
    /// Bit `c` set: 0-based channel `c` feeds this track.
    pub channels: u16,
    /// Lowest and highest note this track plays, inclusive.
    pub low: u8,
    pub high: u8,
}

impl TrackRoute {
    /// Nothing reaches the track.
    pub const NONE: TrackRoute = TrackRoute {
        ports: 0,
        channels: 0,
        low: 0,
        high: 127,
    };

    pub const ALL_CHANNELS: u16 = 0xFFFF;

    /// Whether an event of port slot `port` reaches this track.
    #[inline]
    pub fn takes(&self, port: usize, msg: [u8; 3]) -> bool {
        if port >= 32 || self.ports & (1 << port) == 0 || !is_channel_message(msg) {
            return false;
        }
        if self.channels & (1 << (msg[0] & 0x0F)) == 0 {
            return false;
        }
        match msg[0] & 0xF0 {
            0x80 | 0x90 | 0xA0 => (self.low..=self.high).contains(&msg[1]),
            0xB0 => is_playing_controller(msg[1]),
            0xD0 | 0xE0 => true,
            _ => false,
        }
    }
}

// ---------------------------------------------------------------------------------------------
// Port feeds: the fast queue from one driver callback into the audio thread
// ---------------------------------------------------------------------------------------------

/// Where a feed's queue end lives while no engine holds it. A new engine (device change, score
/// loaded) takes it from here; dropping the old engine puts it back.
#[derive(Debug, Default)]
pub struct FeedHome {
    rx: Mutex<Option<Consumer<LiveEvent>>>,
}

/// The audio thread's end of one port's fast queue.
///
/// Never dropped on the audio thread: a feed that is replaced or detached goes back through the
/// retire queue. Dropping it anywhere else returns the queue end to its [`FeedHome`], which is how
/// it survives an engine being rebuilt around it.
pub struct PortFeed {
    pub port: usize,
    rx: Option<Consumer<LiveEvent>>,
    home: Arc<FeedHome>,
}

impl Drop for PortFeed {
    fn drop(&mut self) {
        if let Some(rx) = self.rx.take()
            && let Ok(mut slot) = self.home.rx.lock()
        {
            *slot = Some(rx);
        }
    }
}

/// The control side of one port's feed: hands the queue end to an engine.
#[derive(Clone, Debug)]
pub struct FeedHandle {
    home: Arc<FeedHome>,
}

impl FeedHandle {
    /// The queue end, packed for the engine as port slot `port` - `None` while an engine holds it.
    pub fn take_feed(&self, port: usize) -> Option<PortFeed> {
        let rx = self.home.rx.lock().ok()?.take()?;
        Some(PortFeed {
            port,
            rx: Some(rx),
            home: Arc::clone(&self.home),
        })
    }
}

/// A fresh fast queue: the producer goes into the driver callback.
pub fn port_feed() -> (Producer<LiveEvent>, FeedHandle) {
    let (tx, rx) = RingBuffer::new(FEED_QUEUE);
    let home = Arc::new(FeedHome {
        rx: Mutex::new(Some(rx)),
    });
    (tx, FeedHandle { home })
}

// ---------------------------------------------------------------------------------------------
// Instruments
// ---------------------------------------------------------------------------------------------

/// One output bus of an instrument for one block.
pub struct StereoOut<'a> {
    pub l: &'a mut [f32],
    pub r: &'a mut [f32],
}

/// Something that makes sound from MIDI: a hosted instrument plugin, or a test double.
///
/// `render` is called on the audio thread with `events` sorted by offset, every offset below the
/// block length, and `outs[b].l.len() == outs[b].r.len() <= MAX_BLOCK`. The buffers arrive
/// zeroed. Only bus 0 is heard for now; an instrument with more outputs may fill as many of
/// `outs` as it has. It must not allocate, lock or block.
pub trait SourceProcessor: Send {
    fn render(&mut self, events: &[BlockEvent], outs: &mut [StereoOut<'_>]);
    /// Where the musical clock stands at the first frame of the next [`Self::render`] call. Set by
    /// the rack right before every call; see [`PluginTransport`].
    fn set_transport(&mut self, transport: &PluginTransport) {
        let _ = transport;
    }
    /// How many output buses this instrument has.
    fn buses(&self) -> usize {
        1
    }
}

/// What the control side and the audio thread share about one instrument. Atomics only.
#[derive(Debug, Default)]
pub struct SourceShared {
    latency: AtomicU32,
    /// Process calls so far - proof that the instrument runs.
    blocks: AtomicU64,
    peak_out: AtomicU32,
    peak_ns: AtomicU32,
    /// Set by the control side: end every note this instrument is playing.
    panic: AtomicBool,
}

impl SourceShared {
    pub fn new(latency: u32) -> Arc<Self> {
        let shared = Self::default();
        shared.latency.store(latency, Ordering::Relaxed);
        Arc::new(shared)
    }

    pub fn set_latency(&self, frames: u32) {
        self.latency.store(frames, Ordering::Relaxed);
    }

    pub fn latency(&self) -> u32 {
        self.latency.load(Ordering::Relaxed)
    }

    pub fn blocks(&self) -> u64 {
        self.blocks.load(Ordering::Relaxed)
    }

    pub fn take_peak(&self) -> f32 {
        f32::from_bits(self.peak_out.swap(0, Ordering::Relaxed))
    }

    pub fn take_peak_us(&self) -> f32 {
        self.peak_ns.swap(0, Ordering::Relaxed) as f32 / 1000.0
    }

    /// Ask the audio thread to end every note of this instrument at the next block.
    pub fn request_panic(&self) {
        self.panic.store(true, Ordering::Relaxed);
    }

    #[inline]
    fn note_block(&self, peak: f32, ns: u32) {
        self.blocks.fetch_add(1, Ordering::Relaxed);
        if peak > f32::from_bits(self.peak_out.load(Ordering::Relaxed)) {
            self.peak_out.store(peak.to_bits(), Ordering::Relaxed);
        }
        if ns > self.peak_ns.load(Ordering::Relaxed) {
            self.peak_ns.store(ns, Ordering::Relaxed);
        }
    }
}

/// One loaded instrument on its way into or out of a track's source slot.
pub struct Instrument {
    pub processor: Box<dyn SourceProcessor>,
    pub shared: Arc<SourceShared>,
}

impl Instrument {
    pub fn new(processor: Box<dyn SourceProcessor>, shared: Arc<SourceShared>) -> Self {
        Self { processor, shared }
    }
}

/// From the plugin side (main thread) to the rack.
pub enum InstrumentMsg {
    Install { track: usize, instrument: Instrument },
    Remove { track: usize },
}

/// From the MIDI side (host thread) to the rack.
pub enum FeedMsg {
    /// Listen to a port. A feed already in that slot goes back.
    Attach(PortFeed),
    Detach { port: usize },
    /// Which ports, channels and notes reach a track.
    Route { track: usize, route: TrackRoute },
    /// End every note on every instrument - the global "all notes off".
    PanicAll,
    /// End every note of one track's instrument.
    Panic { track: usize },
    /// Shift the live notes of a track by `semitones` (-48..=48) before they reach its instrument
    /// - a pad controller that sends an octave below what the drum kit expects. The key range of
    /// the route applies to the note as it arrives, before the shift. A change ends the notes the
    /// musician holds, so no note off goes out with a different shift than its note on.
    Transpose { track: usize, semitones: i8 },
}

/// The largest transposition, in semitones, either way.
pub const MAX_TRANSPOSE: i8 = 48;

/// A note message moved by `semitones`, or `None` when the note leaves 0..=127. Everything that is
/// not a note (controllers, bend, pressure) passes unchanged.
#[inline]
pub fn transposed(msg: [u8; 3], semitones: i8) -> Option<[u8; 3]> {
    if semitones == 0 {
        return Some(msg);
    }
    match msg[0] & 0xF0 {
        0x80 | 0x90 | 0xA0 => {
            let n = i16::from(msg[1] & 0x7F) + i16::from(semitones);
            (0..=127).contains(&n).then_some([msg[0], n as u8, msg[2]])
        }
        _ => Some(msg),
    }
}

/// Control side of the instrument queue.
pub struct InstrumentSender {
    tx: Producer<InstrumentMsg>,
    retired: Consumer<Instrument>,
}

impl InstrumentSender {
    pub fn send(&mut self, msg: InstrumentMsg) -> Result<(), String> {
        self.tx
            .push(msg)
            .map_err(|_| "The instrument queue is full. Try again in a moment.".to_string())
    }

    /// Drop what the audio thread handed back, here on the calling thread.
    pub fn drain_retired(&mut self) -> usize {
        let mut n = 0;
        while let Ok(instrument) = self.retired.pop() {
            drop(instrument);
            n += 1;
        }
        n
    }
}

/// Control side of the feed and routing queue.
pub struct FeedSender {
    tx: Producer<FeedMsg>,
    retired: Consumer<PortFeed>,
}

impl FeedSender {
    pub fn send(&mut self, msg: FeedMsg) -> Result<(), String> {
        self.tx
            .push(msg)
            .map_err(|_| "The MIDI routing queue is full.".to_string())
    }

    /// Feeds the engine let go of; dropping them sends their queue end home.
    pub fn drain_retired(&mut self) -> usize {
        let mut n = 0;
        while let Ok(feed) = self.retired.pop() {
            drop(feed);
            n += 1;
        }
        n
    }
}

/// The audio side of both queues.
pub struct SourceEndpoint {
    instruments: Consumer<InstrumentMsg>,
    instrument_retire: Producer<Instrument>,
    feeds: Consumer<FeedMsg>,
    feed_retire: Producer<PortFeed>,
}

pub fn source_channel(capacity: usize) -> (InstrumentSender, FeedSender, SourceEndpoint) {
    let (itx, irx) = RingBuffer::new(capacity);
    let (iret_tx, iret_rx) = RingBuffer::new(capacity * 2 + MAX_TRACKS);
    let (ftx, frx) = RingBuffer::new(capacity);
    let (fret_tx, fret_rx) = RingBuffer::new(capacity * 2 + MAX_PORTS);
    (
        InstrumentSender {
            tx: itx,
            retired: iret_rx,
        },
        FeedSender {
            tx: ftx,
            retired: fret_rx,
        },
        SourceEndpoint {
            instruments: irx,
            instrument_retire: iret_tx,
            feeds: frx,
            feed_retire: fret_tx,
        },
    )
}

// ---------------------------------------------------------------------------------------------
// Note tracker
// ---------------------------------------------------------------------------------------------

/// Which notes one source has sounding, so they can all be ended at once: on panic, when the
/// instrument is swapped, when the routing changes or the port goes away.
#[derive(Clone, Copy, Debug, Default)]
pub struct NoteTracker {
    on: [[u64; 2]; 16],
    /// Channels whose sustain pedal is down.
    sustain: u16,
    /// Channels this source has sent anything on since the last release.
    used: u16,
}

impl NoteTracker {
    pub fn note(&mut self, msg: [u8; 3]) {
        if !is_channel_message(msg) {
            return;
        }
        let c = (msg[0] & 0x0F) as usize;
        self.used |= 1 << c;
        let n = msg[1] & 0x7F;
        let (word, bit) = ((n / 64) as usize, 1u64 << (n % 64));
        match msg[0] & 0xF0 {
            0x90 if msg[2] > 0 => self.on[c][word] |= bit,
            0x80 | 0x90 => self.on[c][word] &= !bit,
            0xB0 if msg[1] == 64 => {
                if msg[2] >= 64 {
                    self.sustain |= 1 << c;
                } else {
                    self.sustain &= !(1 << c);
                }
            }
            _ => {}
        }
    }

    /// Notes sounding right now (pressed, not released).
    pub fn held(&self) -> u32 {
        self.on.iter().flatten().map(|w| w.count_ones()).sum()
    }

    pub fn is_on(&self, channel: u8, note: u8) -> bool {
        let n = note & 0x7F;
        self.on[(channel & 0x0F) as usize][(n / 64) as usize] & (1u64 << (n % 64)) != 0
    }

    pub fn idle(&self) -> bool {
        self.used == 0
    }

    /// Channels whose sustain pedal this source holds down.
    pub fn sustain(&self) -> u16 {
        self.sustain
    }

    /// End this source's own notes and lift its pedal - and nothing else. Unlike
    /// [`NoteTracker::release`] there is no "all notes off": a MIDI layer that is stopped shares its
    /// instrument with the live keys, and those keep sounding.
    pub fn release_own(&mut self, mut emit: impl FnMut([u8; 3])) {
        for c in 0..16u8 {
            let words = self.on[c as usize];
            for (w, word) in words.iter().enumerate() {
                let mut bits = *word;
                while bits != 0 {
                    let b = bits.trailing_zeros() as u8;
                    bits &= bits - 1;
                    emit([0x80 | c, w as u8 * 64 + b, 0]);
                }
            }
            if self.sustain & (1 << c) != 0 {
                emit([0xB0 | c, 64, 0]);
            }
        }
        *self = Self::default();
    }

    /// Emit what ends everything this source started: a note off per held note, the sustain pedal
    /// up where it was down, and "all notes off" (CC 123) on every channel that was used - the
    /// last for instruments that keep voices of their own the tracker cannot see. Then forget.
    pub fn release(&mut self, mut emit: impl FnMut([u8; 3])) {
        for c in 0..16u8 {
            let words = self.on[c as usize];
            for (w, word) in words.iter().enumerate() {
                let mut bits = *word;
                while bits != 0 {
                    let b = bits.trailing_zeros() as u8;
                    bits &= bits - 1;
                    emit([0x80 | c, w as u8 * 64 + b, 0]);
                }
            }
            if self.sustain & (1 << c) != 0 {
                emit([0xB0 | c, 64, 0]);
            }
            if self.used & (1 << c) != 0 {
                emit([0xB0 | c, 123, 0]);
            }
        }
        *self = Self::default();
    }
}

// ---------------------------------------------------------------------------------------------
// The rack
// ---------------------------------------------------------------------------------------------

/// The host clock for the next process call, from the output callback.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct HostClock {
    /// When the first frame of the next process call will be heard, host nanoseconds.
    pub playback_ns: u64,
    /// When the callback that renders it started, host nanoseconds.
    pub callback_ns: u64,
    /// Frames of one driver callback.
    pub block_frames: u32,
}

impl HostClock {
    /// `C`: output latency plus one block, in frames - the fixed delay between a key and its
    /// sound that makes the delay the same for every key. See the module comment.
    pub fn fixed_delay(&self, sample_rate: u32) -> u64 {
        let out_ns = self.playback_ns.saturating_sub(self.callback_ns);
        let out_frames = (out_ns as f64 * f64::from(sample_rate) / 1e9).round() as u64;
        out_frames + u64::from(self.block_frames)
    }
}

/// One event waiting for its position.
#[derive(Clone, Copy, Debug)]
struct Pending {
    /// Output position it is rendered at.
    at: u64,
    msg: [u8; 3],
    source: SourceId,
    /// Meant for "right now" (an ending): not counted as late.
    now: bool,
}

/// One track's stereo source output for a whole process call.
pub struct SourceOut {
    pub l: Vec<f32>,
    pub r: Vec<f32>,
}

/// The audio thread's side: one source slot per track, the port feeds, the event stage and the
/// output buffers. Everything here is allocated in [`SourceRack::new`].
pub struct SourceRack {
    endpoint: SourceEndpoint,
    slots: Vec<Option<Instrument>>,
    routes: [TrackRoute; MAX_TRACKS],
    feeds: Vec<Option<PortFeed>>,
    pending: Vec<Vec<Pending>>,
    trackers: Vec<[NoteTracker; SOURCES]>,
    /// Set when the live notes of a track have to be ended before its next block.
    flush_live: [bool; MAX_TRACKS],
    panic_all: bool,
    /// Events for one chunk, sorted - what the instrument gets.
    scratch: Vec<BlockEvent>,
    /// Second and further buses of an instrument; rendered and not heard yet.
    spare_buses: Vec<(Vec<f32>, Vec<f32>)>,
    /// Per track, the source output of the current process call.
    pub(crate) out: Vec<SourceOut>,
    /// Frames each output buffer holds.
    capacity: usize,
    /// Note ons that reached each track since the engine started - the activity light.
    notes: [u32; MAX_TRACKS],
    /// Events that arrived after the position they belonged to and were played late.
    late: u64,
    /// Events lost because a track's waiting list was full.
    dropped: u64,
    leaked: u64,
    /// The clock at the first frame of the next [`SourceRack::render`]; set by the engine.
    transport: PluginTransport,
    /// Per track: a MIDI loop - live events are rendered as early as possible (the recording
    /// position comes from the time stamp, not from the rendering), and every routed event is
    /// also handed to the engine with the position it was heard at.
    midi: [bool; MAX_TRACKS],
    /// Per track, the semitones its live notes are shifted by ([`FeedMsg::Transpose`]).
    transpose: [i8; MAX_TRACKS],
    /// Per MIDI-loop track, the events of this call with their heard position, for recording.
    pub(crate) heard: Vec<Vec<(i64, [u8; 3])>>,
    /// Per track, what the instrument was sent in this process call, as (frame in the call, msg) -
    /// for the recorder's MIDI file. Only collected while `tap` is set.
    pub(crate) sent: Vec<Vec<(u32, [u8; 3])>>,
    tap: bool,
    /// Nanoseconds spent inside the instruments since [`SourceRack::take_busy_ns`] - the plugin
    /// share of the audio load (`crate::load`).
    busy_ns: u64,
}

impl SourceRack {
    /// `capacity` is the largest process call in frames; the host knows it from its scratch size.
    pub fn new(endpoint: SourceEndpoint, capacity: usize) -> Self {
        let capacity = capacity.max(1);
        Self {
            endpoint,
            slots: (0..MAX_TRACKS).map(|_| None).collect(),
            routes: [TrackRoute::NONE; MAX_TRACKS],
            feeds: (0..MAX_PORTS).map(|_| None).collect(),
            pending: (0..MAX_TRACKS)
                .map(|_| Vec::with_capacity(PENDING_EVENTS))
                .collect(),
            trackers: vec![[NoteTracker::default(); SOURCES]; MAX_TRACKS],
            flush_live: [false; MAX_TRACKS],
            panic_all: false,
            scratch: Vec::with_capacity(PENDING_EVENTS + 64 * SOURCES),
            spare_buses: (1..MAX_SOURCE_BUSES)
                .map(|_| (vec![0.0; MAX_BLOCK], vec![0.0; MAX_BLOCK]))
                .collect(),
            out: (0..MAX_TRACKS)
                .map(|_| SourceOut {
                    l: vec![0.0; capacity],
                    r: vec![0.0; capacity],
                })
                .collect(),
            capacity,
            notes: [0; MAX_TRACKS],
            late: 0,
            dropped: 0,
            leaked: 0,
            transport: PluginTransport::standing(48_000.0),
            midi: [false; MAX_TRACKS],
            transpose: [0; MAX_TRACKS],
            heard: (0..MAX_TRACKS)
                .map(|_| Vec::with_capacity(PENDING_EVENTS))
                .collect(),
            sent: (0..MAX_TRACKS)
                .map(|_| Vec::with_capacity(PENDING_EVENTS))
                .collect(),
            tap: false,
            busy_ns: 0,
        }
    }

    /// Whether track `track` is a MIDI loop - see the field.
    #[inline]
    pub fn set_midi(&mut self, track: usize, on: bool) {
        if let Some(m) = self.midi.get_mut(track) {
            *m = on;
        }
    }

    /// Collect what each instrument is sent (for a MIDI file of the session).
    #[inline]
    pub fn set_tap(&mut self, on: bool) {
        self.tap = on;
    }

    /// Channels on which the musician holds the sustain pedal of a track down.
    pub fn live_sustain(&self, track: usize) -> u16 {
        self.trackers
            .get(track)
            .map_or(0, |t| t[SourceId::LIVE.0 as usize].sustain())
    }

    /// An ending for a track's instrument that has to go out before anything else at the start of
    /// the next rendered piece - a MIDI layer that is stopped, muted or removed.
    #[inline]
    pub fn push_now(&mut self, track: usize, msg: [u8; 3]) {
        let Some(list) = self.pending.get_mut(track) else {
            return;
        };
        if list.len() >= PENDING_EVENTS {
            self.dropped += 1;
            return;
        }
        let i = list.partition_point(|e| e.now);
        list.insert(i, Pending {
            at: 0,
            msg,
            source: SourceId::LOOP,
            now: true,
        });
    }

    /// One event of a track's MIDI layers at output position `at`.
    #[inline]
    pub fn push_loop(&mut self, track: usize, at: u64, msg: [u8; 3]) {
        let Some(list) = self.pending.get_mut(track) else {
            return;
        };
        if list.len() >= PENDING_EVENTS {
            self.dropped += 1;
            return;
        }
        let i = list.partition_point(|e| e.at <= at);
        list.insert(i, Pending {
            at,
            msg,
            source: SourceId::LOOP,
            now: false,
        });
    }

    /// The clock at the first frame of the next [`SourceRack::render`]. Each chunk of that call
    /// hands its instruments the clock moved on to the chunk's own first frame.
    #[inline]
    pub fn set_transport(&mut self, transport: PluginTransport) {
        self.transport = transport;
    }

    pub fn capacity(&self) -> usize {
        self.capacity
    }

    pub fn has_instrument(&self, track: usize) -> bool {
        self.slots.get(track).is_some_and(Option::is_some)
    }

    /// Latency the instrument of a track reports, in frames; 0 without one.
    pub fn latency(&self, track: usize) -> u64 {
        self.slots
            .get(track)
            .and_then(Option::as_ref)
            .map_or(0, |i| u64::from(i.shared.latency()))
    }

    pub fn notes(&self, track: usize) -> u32 {
        self.notes.get(track).copied().unwrap_or(0)
    }

    pub fn late(&self) -> u64 {
        self.late
    }

    pub fn dropped(&self) -> u64 {
        self.dropped
    }

    pub fn leaked(&self) -> u64 {
        self.leaked
    }

    /// Notes the live input of a track holds right now.
    pub fn held(&self, track: usize) -> u32 {
        self.trackers
            .get(track)
            .map_or(0, |t| t[SourceId::LIVE.0 as usize].held())
    }

    /// Apply what both control sides sent. Bounded work.
    pub fn collect(&mut self) {
        for _ in 0..MESSAGES_PER_BLOCK {
            let Ok(msg) = self.endpoint.instruments.pop() else {
                break;
            };
            match msg {
                InstrumentMsg::Install { track, instrument } => {
                    if track >= MAX_TRACKS {
                        self.retire_instrument(instrument);
                        continue;
                    }
                    // The new instrument starts with nothing sounding; the old one is gone.
                    self.reset_track(track);
                    if let Some(old) = self.slots[track].replace(instrument) {
                        self.retire_instrument(old);
                    }
                }
                InstrumentMsg::Remove { track } => {
                    if let Some(old) = self.slots.get_mut(track).and_then(Option::take) {
                        self.retire_instrument(old);
                    }
                    if track < MAX_TRACKS {
                        self.reset_track(track);
                    }
                }
            }
        }
        for _ in 0..MESSAGES_PER_BLOCK {
            let Ok(msg) = self.endpoint.feeds.pop() else {
                break;
            };
            match msg {
                FeedMsg::Attach(mut feed) => {
                    let port = feed.port;
                    if port >= MAX_PORTS {
                        self.retire_feed(feed);
                        continue;
                    }
                    // Whatever queued up while no engine listened is stale: played now it would be a
                    // burst of old notes. Bounded by the queue size, and only on attach.
                    if let Some(rx) = feed.rx.as_mut() {
                        while rx.pop().is_ok() {}
                    }
                    if let Some(old) = self.feeds[port].replace(feed) {
                        self.retire_feed(old);
                    }
                }
                FeedMsg::Detach { port } => {
                    if let Some(old) = self.feeds.get_mut(port).and_then(Option::take) {
                        self.retire_feed(old);
                    }
                    // The port is gone: no note off will ever come from it.
                    for t in 0..MAX_TRACKS {
                        if port < 32 && self.routes[t].ports & (1 << port) != 0 {
                            self.flush_live[t] = true;
                        }
                    }
                }
                FeedMsg::Route { track, route } => {
                    if track < MAX_TRACKS && self.routes[track] != route {
                        self.routes[track] = route;
                        self.flush_live[track] = true;
                    }
                }
                FeedMsg::Transpose { track, semitones } => {
                    let semitones = semitones.clamp(-MAX_TRANSPOSE, MAX_TRANSPOSE);
                    if track < MAX_TRACKS && self.transpose[track] != semitones {
                        self.transpose[track] = semitones;
                        self.flush_live[track] = true;
                    }
                }
                FeedMsg::PanicAll => self.panic_all = true,
                FeedMsg::Panic { track } => {
                    if let Some(instrument) = self.slots.get(track).and_then(Option::as_ref) {
                        instrument.shared.panic.store(true, Ordering::Relaxed);
                    }
                }
            }
        }
    }

    fn reset_track(&mut self, track: usize) {
        self.trackers[track] = [NoteTracker::default(); SOURCES];
        self.flush_live[track] = false;
        self.pending[track].clear();
    }

    fn retire_instrument(&mut self, instrument: Instrument) {
        if let Err(rtrb::PushError::Full(instrument)) =
            self.endpoint.instrument_retire.push(instrument)
        {
            std::mem::forget(instrument);
            self.leaked += 1;
        }
    }

    fn retire_feed(&mut self, feed: PortFeed) {
        if let Err(rtrb::PushError::Full(feed)) = self.endpoint.feed_retire.push(feed) {
            std::mem::forget(feed);
            self.leaked += 1;
        }
    }

    /// Take every event out of the port feeds and queue it on each track it is routed to, at the
    /// output position it has to sound at.
    ///
    /// `place` turns a host time into that position; without a clock it answers `block_start`.
    ///
    /// A MIDI-loop track gets its events at `start`, as early as possible, and every one of them
    /// also lands in its `heard` list with the position `heard` gives it.
    pub fn gather(&mut self, start: u64, place: impl Fn(u64) -> u64, heard: impl Fn(u64) -> i64) {
        for list in self.heard.iter_mut() {
            list.clear();
        }
        for p in 0..MAX_PORTS {
            let Some(feed) = self.feeds[p].as_mut() else {
                continue;
            };
            let Some(rx) = feed.rx.as_mut() else {
                continue;
            };
            // Bounded: a port's queue holds at most FEED_QUEUE events.
            while let Ok(event) = rx.pop() {
                let fixed = place(event.host_ns);
                for t in 0..MAX_TRACKS {
                    if !self.routes[t].takes(p, event.msg) {
                        continue;
                    }
                    // The range was checked on the note as it came in; now it is moved.
                    let Some(msg) = transposed(event.msg, self.transpose[t]) else {
                        continue;
                    };
                    let at = if self.midi[t] { start } else { fixed };
                    // A MIDI loop stores what the instrument got: changing the transposition later
                    // moves what is played live, not the layers already recorded.
                    if self.midi[t] && self.heard[t].len() < PENDING_EVENTS {
                        self.heard[t].push((heard(event.host_ns), msg));
                    }
                    if msg[0] & 0xF0 == 0x90 && msg[2] > 0 {
                        self.notes[t] = self.notes[t].wrapping_add(1);
                    }
                    let list = &mut self.pending[t];
                    if list.len() >= PENDING_EVENTS {
                        self.dropped += 1;
                        continue;
                    }
                    // Sorted insert, stable for equal positions: later arrivals go behind.
                    let i = list.partition_point(|e| e.at <= at);
                    list.insert(i, Pending {
                        at,
                        msg,
                        source: SourceId::LIVE,
                        now: false,
                    });
                }
            }
        }
    }

    /// Time spent inside the instruments since the last call, and reset.
    #[inline]
    pub fn take_busy_ns(&mut self) -> u64 {
        std::mem::take(&mut self.busy_ns)
    }

    /// Render every instrument for one process call of `frames` frames starting at `start`, into
    /// [`SourceRack::out`]. Long calls are cut into pieces of [`MAX_BLOCK`].
    pub fn render(&mut self, start: u64, frames: usize) {
        self.render_range(start, 0, frames);
    }

    /// Render frames `from..to` of the process call that starts at `start`. The engine renders a
    /// call in the pieces its commands cut it into, so a MIDI layer that starts or stops in the
    /// middle of a call does so on its frame.
    pub fn render_range(&mut self, start: u64, from: usize, to: usize) {
        let to = to.min(self.capacity);
        if from == 0 {
            for sent in self.sent.iter_mut() {
                sent.clear();
            }
        }
        if from >= to {
            return;
        }
        let panic_all = if from == 0 {
            std::mem::take(&mut self.panic_all)
        } else {
            false
        };
        for t in 0..MAX_TRACKS {
            let out = &mut self.out[t];
            out.l[from..to].fill(0.0);
            out.r[from..to].fill(0.0);
            let Some(instrument) = self.slots[t].as_mut() else {
                // Nothing to play it on: what was waiting is dropped, nothing can hang.
                self.pending[t].clear();
                self.flush_live[t] = false;
                continue;
            };
            let panic = from == 0
                && (instrument.shared.panic.swap(false, Ordering::Relaxed) || panic_all);
            let mut done = from;
            while done < to {
                let n = (to - done).min(MAX_BLOCK);
                let chunk_start = start + done as u64;
                let chunk_end = chunk_start + n as u64;
                self.scratch.clear();
                // Endings first, at offset 0 of the first chunk, before anything new starts.
                if done == 0 {
                    let trackers = &mut self.trackers[t];
                    let scratch = &mut self.scratch;
                    let mut push = |msg| scratch.push(BlockEvent { offset: 0, msg });
                    if panic {
                        for tracker in trackers.iter_mut() {
                            tracker.release(&mut push);
                        }
                        self.pending[t].clear();
                    } else if std::mem::take(&mut self.flush_live[t]) {
                        trackers[SourceId::LIVE.0 as usize].release(&mut push);
                    }
                }
                let list = &mut self.pending[t];
                let due = list.partition_point(|e| e.now || e.at < chunk_end);
                for e in &list[..due] {
                    let offset = if e.now {
                        0
                    } else if e.at < chunk_start {
                        self.late += 1;
                        0
                    } else {
                        (e.at - chunk_start) as u32
                    };
                    self.trackers[t][e.source.0 as usize].note(e.msg);
                    self.scratch.push(BlockEvent {
                        offset,
                        msg: e.msg,
                    });
                }
                list.drain(..due);
                if self.tap {
                    let sent = &mut self.sent[t];
                    for e in &self.scratch {
                        if sent.len() < PENDING_EVENTS {
                            sent.push((done as u32 + e.offset, e.msg));
                        }
                    }
                }
                let out = &mut self.out[t];
                let (l, r) = (
                    &mut out.l[done..done + n],
                    &mut out.r[done..done + n],
                );
                let started = std::time::Instant::now();
                let buses = instrument.processor.buses().clamp(1, MAX_SOURCE_BUSES);
                {
                    let mut spares = self.spare_buses.iter_mut();
                    let mut outs: [Option<StereoOut<'_>>; MAX_SOURCE_BUSES] =
                        [None, None, None, None];
                    outs[0] = Some(StereoOut { l, r });
                    for slot in outs.iter_mut().take(buses).skip(1) {
                        if let Some((sl, sr)) = spares.next() {
                            sl[..n].fill(0.0);
                            sr[..n].fill(0.0);
                            *slot = Some(StereoOut {
                                l: &mut sl[..n],
                                r: &mut sr[..n],
                            });
                        }
                    }
                    let mut packed: [StereoOut<'_>; MAX_SOURCE_BUSES] =
                        outs.map(|o| o.unwrap_or(StereoOut { l: &mut [], r: &mut [] }));
                    instrument
                        .processor
                        .set_transport(&self.transport.advanced(done));
                    instrument
                        .processor
                        .render(&self.scratch, &mut packed[..buses]);
                }
                let ns = started.elapsed().as_nanos().min(u32::MAX as u128) as u32;
                let mut peak = 0.0f32;
                for s in out.l[done..done + n]
                    .iter_mut()
                    .chain(out.r[done..done + n].iter_mut())
                {
                    if !s.is_finite() {
                        *s = 0.0;
                    }
                    peak = peak.max(s.abs());
                }
                instrument.shared.note_block(peak, ns);
                self.busy_ns += u64::from(ns);
                done += n;
            }
        }
    }

    /// One frame of a track's source output, from the current process call.
    #[inline(always)]
    pub fn frame(&self, track: usize, i: usize) -> (f32, f32) {
        match self.out.get(track) {
            Some(out) if i < self.capacity => (out.l[i], out.r[i]),
            _ => (0.0, 0.0),
        }
    }
}

/// Where an event stamped `host_ns` sounds, given the clock of this block.
///
/// `m = out_pos + (host_ns - playback_ns) * rate` is the position the musician heard while he
/// played it; it sounds `delay` frames later. Never before `out_pos` - that is the past - and never
/// absurdly far ahead, which only a clock that jumped could produce.
pub fn place_event(host_ns: u64, out_pos: u64, playback_ns: u64, rate: u32, delay: u64) -> u64 {
    let dt = host_ns as i128 - playback_ns as i128;
    let frames = (dt * i128::from(rate) * 2 + if dt >= 0 { 1_000_000_000 } else { -1_000_000_000 })
        / 2_000_000_000;
    let at = out_pos as i128 + frames + delay as i128;
    let at = at.clamp(out_pos as i128, out_pos as i128 + FUTURE_LIMIT as i128);
    at as u64
}

/// The position an event stamped `host_ns` was *heard* at - without the fixed delay and without
/// clamping. What a MIDI layer will store.
pub fn heard_at(host_ns: u64, out_pos: u64, playback_ns: u64, rate: u32) -> i64 {
    let dt = host_ns as i128 - playback_ns as i128;
    let frames = (dt * i128::from(rate) * 2 + if dt >= 0 { 1_000_000_000 } else { -1_000_000_000 })
        / 2_000_000_000;
    (out_pos as i128 + frames) as i64
}

#[cfg(test)]
pub mod testing {
    //! Plain-Rust instruments for the offline tests.

    use super::{BlockEvent, SourceProcessor, StereoOut};

    /// Puts a single sample of `velocity / 127` at the offset of every note on, on both sides,
    /// optionally `latency` frames later - an instrument with a known, exact response.
    pub struct Impulse {
        latency: usize,
        line: Vec<f32>,
        pos: usize,
    }

    impl Impulse {
        pub fn new(latency: usize) -> Self {
            Self {
                latency,
                line: vec![0.0; latency + 1],
                pos: 0,
            }
        }
    }

    impl SourceProcessor for Impulse {
        fn render(&mut self, events: &[BlockEvent], outs: &mut [StereoOut<'_>]) {
            let out = &mut outs[0];
            let n = out.l.len();
            let mut dry = [0.0f32; 4096];
            for e in events {
                if e.msg[0] & 0xF0 == 0x90 && e.msg[2] > 0 && (e.offset as usize) < n {
                    dry[e.offset as usize] += f32::from(e.msg[2]) / 127.0;
                }
            }
            for i in 0..n {
                let v = if self.latency == 0 {
                    dry[i]
                } else {
                    let len = self.line.len();
                    let v = self.line[self.pos];
                    self.line[self.pos] = 0.0;
                    self.line[(self.pos + self.latency) % len] += dry[i];
                    self.pos = (self.pos + 1) % len;
                    v
                };
                out.l[i] = v;
                out.r[i] = v;
            }
        }
    }

    /// Records every event it gets, for checking what the event stage sends.
    pub struct Recorder {
        pub seen: std::sync::Arc<std::sync::Mutex<Vec<(u64, [u8; 3])>>>,
        pub blocks: u64,
        pub frames: u64,
    }

    impl SourceProcessor for Recorder {
        fn render(&mut self, events: &[BlockEvent], outs: &mut [StereoOut<'_>]) {
            let n = outs[0].l.len() as u64;
            if let Ok(mut seen) = self.seen.lock() {
                for e in events {
                    seen.push((self.frames + u64::from(e.offset), e.msg));
                }
            }
            self.blocks += 1;
            self.frames += n;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::timeline::TimeSignature;

    #[test]
    fn a_note_on_with_velocity_zero_is_stored_as_a_note_off() {
        let off = NoteEvent::from_midi(10, [0x93, 60, 0]).unwrap();
        assert_eq!(off.kind, NoteKind::Off);
        assert_eq!(off.channel, 3);
        assert_eq!(off.to_midi(), [0x83, 60, 0]);
        let on = NoteEvent::from_midi(0, [0x90, 64, 100]).unwrap();
        assert_eq!(on.to_midi(), [0x90, 64, 100]);
        assert_eq!(NoteEvent::from_midi(0, [0xE1, 0, 64]).unwrap().kind, NoteKind::Bend);
        assert!(NoteEvent::from_midi(0, [0xF8, 0, 0]).is_none(), "Clock ist keine Note");
    }

    /// PPQ 15360 is finer than a sample at ordinary tempi only in name; what matters is that a
    /// round trip lands within one tick and that triplets are whole numbers.
    #[test]
    fn ticks_and_frames_convert_both_ways() {
        let tl = Timeline::new(48_000, 120.0, TimeSignature::new(4, 4));
        // A quarter at 120 BPM is 24000 frames.
        assert_eq!(ticks_of_frames(&tl, 24_000), i64::from(PPQ));
        assert_eq!(frames_of_ticks(&tl, i64::from(PPQ)), 24_000);
        assert_eq!(PPQ % 3, 0);
        assert_eq!(PPQ % 5, 0);
        for f in [0i64, 1, 7, 1000, 99_999, -500] {
            let back = frames_of_ticks(&tl, ticks_of_frames(&tl, f));
            assert!((back - f).abs() <= 1, "{f} -> {back}");
        }
    }

    #[test]
    fn the_port_mask_sends_only_the_instruments_keys_to_the_fast_queue() {
        let route = PortRoute::new();
        assert!(!route.takes([0x90, 60, 100]), "leer: alles zu den Bindings");
        // Channel 1 plays keys 36..=96; channel 10 (pads) stays with the bindings.
        route.set(&[TrackRoute {
            ports: 1,
            channels: 1,
            low: 36,
            high: 96,
        }]);
        assert!(route.takes([0x90, 60, 100]));
        assert!(route.takes([0x80, 60, 0]));
        assert!(!route.takes([0x90, 30, 100]), "unterhalb des Bereichs");
        assert!(!route.takes([0x99, 60, 100]), "Kanal 10");
        assert!(route.takes([0xB0, 64, 127]), "Sustain");
        assert!(route.takes([0xB0, 1, 10]), "Modulation");
        assert!(!route.takes([0xB0, 20, 10]), "ein Regler bleibt lernbar");
        assert!(route.takes([0xE0, 0, 64]), "Pitchbend");
        assert!(!route.takes([0xC0, 3, 0]), "Programmwechsel");
        assert!(route.covers(0, 36) && route.covers(0, 96) && !route.covers(0, 97));

        set_learning(true);
        assert!(!route.takes([0x90, 60, 100]), "Lernen hat Vorrang");
        set_learning(false);
        route.clear();
        assert!(!route.active());
    }

    #[test]
    fn a_track_route_checks_port_channel_and_range() {
        let r = TrackRoute {
            ports: 0b10,
            channels: TrackRoute::ALL_CHANNELS,
            low: 0,
            high: 127,
        };
        assert!(r.takes(1, [0x95, 60, 1]));
        assert!(!r.takes(0, [0x95, 60, 1]), "anderer Port");
        assert!(!TrackRoute::NONE.takes(0, [0x90, 60, 1]));
    }

    #[test]
    fn the_tracker_ends_exactly_what_is_sounding() {
        let mut t = NoteTracker::default();
        t.note([0x90, 60, 100]);
        t.note([0x90, 64, 100]);
        t.note([0x90, 64, 0]);
        t.note([0x91, 127, 1]);
        t.note([0xB0, 64, 127]);
        assert_eq!(t.held(), 2);
        assert!(t.is_on(0, 60) && !t.is_on(0, 64) && t.is_on(1, 127));
        let mut out = Vec::new();
        t.release(|m| out.push(m));
        assert_eq!(
            out,
            vec![
                [0x80, 60, 0],
                [0xB0, 64, 0],
                [0xB0, 123, 0],
                [0x81, 127, 0],
                [0xB1, 123, 0]
            ]
        );
        assert_eq!(t.held(), 0);
        assert!(t.idle());
    }

    #[test]
    fn an_event_is_placed_by_when_it_was_heard_plus_the_fixed_delay() {
        // 48 kHz: 1 ms is 48 frames. Played 2 ms before the block's first frame is heard.
        let at = place_event(1_000_000_000, 4800, 1_002_000_000, 48_000, 300);
        assert_eq!(at, 4800 - 96 + 300);
        assert_eq!(heard_at(1_000_000_000, 4800, 1_002_000_000, 48_000), 4800 - 96);
        // Too old for the delay: played now, never in the past.
        assert_eq!(place_event(0, 4800, 1_002_000_000, 48_000, 300), 4800);
        let clock = HostClock {
            playback_ns: 1_010_000_000,
            callback_ns: 1_000_000_000,
            block_frames: 128,
        };
        assert_eq!(clock.fixed_delay(48_000), 480 + 128);
    }

    #[test]
    fn a_feed_goes_home_when_its_engine_lets_go() {
        let (mut tx, handle) = port_feed();
        let feed = handle.take_feed(3).expect("frei");
        assert_eq!(feed.port, 3);
        assert!(handle.take_feed(3).is_none(), "ein Feed gehoert genau einer Engine");
        drop(feed);
        let mut again = handle.take_feed(3).expect("zurueck daheim");
        tx.push(LiveEvent {
            host_ns: 1,
            msg: [0x90, 1, 1],
        })
        .unwrap();
        assert_eq!(again.rx.as_mut().unwrap().pop().unwrap().msg, [0x90, 1, 1]);
    }
}
