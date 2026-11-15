//! MIDI loops: an instrument track whose takes store the notes that were played instead of the
//! sound they made.
//!
//! ```text
//!  gespielt ─► Rohdaten (MidiClip.raw, Frame + Tick ab Durchlauf-Anfang, unveraendert)
//!                 │                           │
//!                 │ Raster none: Audio-Thread │ Nachricht je Ereignis (ClipNews)
//!                 ▼                           ▼
//!          Abspielliste (MidiClip.play) ◄── Steuer-Thread baut sie neu (Raster, ClipList)
//!                 │
//!                 ▼  je Segment: Ereignisse im Fenster, um die Plugin-Latenz vorgezogen
//!          Event-Mischstufe der Spur (`super::source`) ─► dasselbe Instrument wie live
//! ```
//!
//! # Raw data and the play list
//!
//! A clip keeps two lists. `raw` is what was played, in the order it arrived: the exact frame it
//! was heard at and its musical position in ticks, both counted from the start of the loop pass the
//! take began in. Nothing ever rewrites it - quantising is a property of how it is *played*. `play`
//! is what the engine plays: every event at a frame offset inside the loop, sorted, with note offs
//! before everything else at the same offset (so a note that ends where the next one starts is
//! released first, also across the loop boundary).
//!
//! With the grid at `none` the audio thread keeps `play` itself, one sorted insert per recorded
//! event - which is what lets the first pass after a recording play its downbeat. With any other
//! grid the control thread builds the list from its mirror of the raw data ([`build_play_list`])
//! and swaps it in through a queue; from then on the clip is `external` and the audio thread only
//! appends to `raw`. The audio thread never quantises.
//!
//! # Boundary rules (see `super::process` for where they are applied)
//!
//! * A note whose off comes after the take's end keeps being listened to (the "tail"): the off is
//!   stored where it was played, past the loop length, and the play list wraps it to the start.
//! * A note that never gets its off - the transport stopped, the pass was left for good - or that
//!   would sound longer than the loop, is ended one frame before the end of its pass.
//! * A take that begins from nothing also takes what was played up to a 32nd note before its start
//!   (the "pre-roll"): stored at a negative frame, it plays at the end of the loop, right before
//!   the one, exactly as early as it was played - and a grid puts it on the one.
//!
//! # Real time
//!
//! Clips are allocated by the control thread at a fixed capacity ([`CLIP_EVENTS`]) and recycled
//! there. A full clip drops further events and counts them; it never grows.

use std::collections::HashMap;

use rtrb::{Consumer, Producer, PushError, RingBuffer};

use super::command::MAX_TRACKS;
use super::source::{NoteEvent, NoteKind, NoteTracker, PPQ};
use super::timeline::Timeline;

/// Events one clip can hold, raw and in its play list each. A dense pass of a played piano with the
/// pedal and a modulation wheel is a few thousand.
pub const CLIP_EVENTS: usize = 16_384;

/// Clips the engine keeps ready, and as many again in flight.
pub const CLIP_STOCK: usize = 4;

/// One played event: the frame it was heard at and the event with its tick, both counted from the
/// start of the loop pass its take began in.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RawEvent {
    pub frame: i64,
    pub ev: NoteEvent,
}

/// One event of a play list: a frame offset inside the loop and the message.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PlayEvent {
    pub offset: u32,
    pub msg: [u8; 3],
}

impl PlayEvent {
    /// Order among events at the same offset: note offs first, then controllers and the rest,
    /// note ons last.
    #[inline]
    pub fn rank(msg: [u8; 3]) -> u8 {
        match msg[0] & 0xF0 {
            0x80 => 0,
            0x90 if msg[2] == 0 => 0,
            0x90 => 2,
            _ => 1,
        }
    }

    #[inline]
    fn key(&self) -> (u32, u8) {
        (self.offset, Self::rank(self.msg))
    }
}

/// The note grid a MIDI layer is played on. Never changes what was recorded.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum NoteGrid {
    #[default]
    None,
    Quarter,
    Eighth,
    Sixteenth,
    EighthTriplet,
    SixteenthTriplet,
}

impl NoteGrid {
    pub const ALL: [NoteGrid; 6] = [
        NoteGrid::None,
        NoteGrid::Quarter,
        NoteGrid::Eighth,
        NoteGrid::Sixteenth,
        NoteGrid::EighthTriplet,
        NoteGrid::SixteenthTriplet,
    ];

    /// One grid step in ticks; `None` for no grid.
    pub fn step(self) -> Option<i64> {
        let q = i64::from(PPQ);
        match self {
            NoteGrid::None => None,
            NoteGrid::Quarter => Some(q),
            NoteGrid::Eighth => Some(q / 2),
            NoteGrid::Sixteenth => Some(q / 4),
            NoteGrid::EighthTriplet => Some(q / 3),
            NoteGrid::SixteenthTriplet => Some(q / 6),
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            NoteGrid::None => "none",
            NoteGrid::Quarter => "1/4",
            NoteGrid::Eighth => "1/8",
            NoteGrid::Sixteenth => "1/16",
            NoteGrid::EighthTriplet => "1/8t",
            NoteGrid::SixteenthTriplet => "1/16t",
        }
    }

    pub fn parse(s: &str) -> Option<NoteGrid> {
        Self::ALL.into_iter().find(|g| g.as_str() == s.trim())
    }
}

/// A MIDI layer's storage. See the module comment.
pub struct MidiClip {
    id: u32,
    /// The longest loop this clip may define, in frames - what an audio buffer's length is to an
    /// audio take. Set by the engine when the take begins.
    max_frames: u64,
    raw: Vec<RawEvent>,
    play: Vec<PlayEvent>,
    /// The play list came from the control thread (a grid); the audio thread no longer inserts.
    external: bool,
    /// What this layer's playback has sounding, so muting, undoing or stopping it ends exactly
    /// its own notes.
    pub tracker: NoteTracker,
    /// Note ons in `raw`, for the display.
    notes: u32,
    /// Events that did not fit.
    lost: u32,
}

impl MidiClip {
    /// Control thread: a clip with room for `capacity` events.
    pub fn new(id: u32, capacity: usize) -> Box<Self> {
        Box::new(Self {
            id,
            max_frames: 0,
            raw: Vec::with_capacity(capacity),
            play: Vec::with_capacity(capacity),
            external: false,
            tracker: NoteTracker::default(),
            notes: 0,
            lost: 0,
        })
    }

    /// Control thread: empty it for reuse under a new id. Keeps the allocations.
    pub fn recycle(&mut self, id: u32) {
        self.id = id;
        self.max_frames = 0;
        self.raw.clear();
        self.play.clear();
        self.external = false;
        self.tracker = NoteTracker::default();
        self.notes = 0;
        self.lost = 0;
    }

    #[inline]
    pub fn id(&self) -> u32 {
        self.id
    }

    pub fn capacity(&self) -> usize {
        self.raw.capacity()
    }

    #[inline]
    pub fn max_frames(&self) -> u64 {
        self.max_frames
    }

    #[inline]
    pub fn set_max_frames(&mut self, frames: u64) {
        self.max_frames = frames;
    }

    pub fn raw(&self) -> &[RawEvent] {
        &self.raw
    }

    pub fn play(&self) -> &[PlayEvent] {
        &self.play
    }

    pub fn notes(&self) -> u32 {
        self.notes
    }

    pub fn lost(&self) -> u32 {
        self.lost
    }

    pub fn external(&self) -> bool {
        self.external
    }

    /// Store one played event. With a known loop length and a play list of its own, the event is
    /// also put into the play list. Returns false when the clip is full.
    #[inline]
    pub fn record(&mut self, event: RawEvent, loop_len: u64) -> bool {
        if self.raw.len() >= self.raw.capacity() {
            self.lost += 1;
            return false;
        }
        self.raw.push(event);
        if event.ev.kind == NoteKind::On {
            self.notes += 1;
        }
        if loop_len > 0 && !self.external {
            self.insert_play(event, loop_len);
        }
        true
    }

    #[inline]
    fn insert_play(&mut self, event: RawEvent, loop_len: u64) {
        if self.play.len() >= self.play.capacity() {
            self.lost += 1;
            return;
        }
        let p = PlayEvent {
            offset: wrap(event.frame, loop_len),
            msg: event.ev.to_midi(),
        };
        let key = p.key();
        // Stable: behind every event with the same key, so equal positions keep arrival order.
        let i = self.play.partition_point(|e| e.key() <= key);
        self.play.insert(i, p);
    }

    /// The loop length has just become known: build the play list from what is recorded so far.
    /// Bounded by the clip size; nearly every insert is an append, because `raw` is in time order.
    pub fn rebuild_play(&mut self, loop_len: u64) {
        if self.external || loop_len == 0 {
            return;
        }
        self.play.clear();
        for i in 0..self.raw.len() {
            let event = self.raw[i];
            self.insert_play(event, loop_len);
        }
    }

    /// Swap in a play list the control thread built; the old one is handed back for dropping.
    pub fn install_list(&mut self, list: &mut Vec<PlayEvent>) {
        std::mem::swap(&mut self.play, list);
        self.external = true;
    }

    /// Indices of the play-list events with an offset in `from..to` (both inside the loop).
    #[inline]
    pub fn window(&self, from: u64, to: u64) -> (usize, usize) {
        let a = self.play.partition_point(|e| u64::from(e.offset) < from);
        let b = self.play.partition_point(|e| u64::from(e.offset) < to);
        (a, b.max(a))
    }
}

/// A frame relative to the loop's start, wrapped into the loop.
#[inline]
pub fn wrap(frame: i64, loop_len: u64) -> u32 {
    let l = loop_len.max(1) as i64;
    frame.rem_euclid(l) as u32
}

/// The play list of a clip, built from its raw events: the control thread's half of quantising.
///
/// `frames_per_tick` comes from the timeline the clip was recorded at. Without a grid the result is
/// exactly what the audio thread builds incrementally. With one, every note on moves to the nearest
/// grid line (strength 100 %), and its note off moves by the same amount, so the length of the note
/// is kept; controllers, pitch bend and pressure stay where they were played.
pub fn build_play_list(
    raw: &[RawEvent],
    loop_len: u64,
    grid: NoteGrid,
    frames_per_tick: f64,
) -> Vec<PlayEvent> {
    let mut list: Vec<PlayEvent> = Vec::with_capacity(raw.len().max(1));
    if loop_len == 0 {
        return list;
    }
    let mut frames: Vec<i64> = raw.iter().map(|e| e.frame).collect();
    if let Some(step) = grid.step() {
        // Pair every note on with the next off of the same key and channel, in raw order.
        let mut open: HashMap<(u8, u8), usize> = HashMap::new();
        for (i, e) in raw.iter().enumerate() {
            let key = (e.ev.channel, e.ev.d1);
            match e.ev.kind {
                NoteKind::On => {
                    let q = (e.ev.tick as f64 / step as f64).round() as i64 * step;
                    frames[i] = (q as f64 * frames_per_tick).round() as i64;
                    open.insert(key, i);
                }
                NoteKind::Off => {
                    if let Some(on) = open.remove(&key) {
                        frames[i] = e.frame + (frames[on] - raw[on].frame);
                    }
                }
                _ => {}
            }
        }
    }
    for (e, f) in raw.iter().zip(frames) {
        list.push(PlayEvent {
            offset: wrap(f, loop_len),
            msg: e.ev.to_midi(),
        });
    }
    list.sort_by_key(|p| p.key());
    list
}

/// Frames per tick at a timeline's tempo.
pub fn frames_per_tick(timeline: &Timeline) -> f64 {
    timeline.samples_per_quarter() / f64::from(PPQ)
}

// ---------------------------------------------------------------------------------------------
// The open notes of one take, for the boundary rules
// ---------------------------------------------------------------------------------------------

/// No note open.
const CLOSED: i64 = i64::MIN;

/// Which notes a take (or the tail of a closed one) has started and not ended yet, with the frame
/// each started at. Fixed size, allocated once per track.
#[derive(Clone)]
pub struct OpenNotes {
    /// The clip these notes were recorded into.
    pub clip: u32,
    /// Musical position of frame 0 of the clip's raw data.
    pub base: u64,
    on: Box<[[i64; 128]; 16]>,
    /// Frame of a pressed sustain pedal, per channel.
    pedal: [i64; 16],
    open: u32,
    /// Active at all.
    pub live: bool,
    /// For a tail: the position at which it gives up waiting.
    pub until: u64,
}

impl OpenNotes {
    pub fn new() -> Self {
        Self {
            clip: 0,
            base: 0,
            on: Box::new([[CLOSED; 128]; 16]),
            pedal: [CLOSED; 16],
            open: 0,
            live: false,
            until: 0,
        }
    }

    pub fn start(&mut self, clip: u32, base: u64) {
        self.reset();
        self.clip = clip;
        self.base = base;
        self.live = true;
    }

    pub fn reset(&mut self) {
        if self.open > 0 {
            for ch in self.on.iter_mut() {
                ch.fill(CLOSED);
            }
        }
        self.pedal = [CLOSED; 16];
        self.open = 0;
        self.live = false;
    }

    #[inline]
    pub fn is_open(&self, channel: u8, note: u8) -> bool {
        self.on[(channel & 15) as usize][(note & 127) as usize] != CLOSED
    }

    #[inline]
    pub fn on_frame(&self, channel: u8, note: u8) -> Option<i64> {
        let f = self.on[(channel & 15) as usize][(note & 127) as usize];
        (f != CLOSED).then_some(f)
    }

    #[inline]
    pub fn pedal_open(&self, channel: u8) -> Option<i64> {
        let f = self.pedal[(channel & 15) as usize];
        (f != CLOSED).then_some(f)
    }

    #[inline]
    pub fn open_note(&mut self, channel: u8, note: u8, frame: i64) {
        let slot = &mut self.on[(channel & 15) as usize][(note & 127) as usize];
        if *slot == CLOSED {
            self.open += 1;
        }
        *slot = frame;
    }

    #[inline]
    pub fn close_note(&mut self, channel: u8, note: u8) {
        let slot = &mut self.on[(channel & 15) as usize][(note & 127) as usize];
        if *slot != CLOSED {
            *slot = CLOSED;
            self.open -= 1;
        }
    }

    #[inline]
    pub fn press_pedal(&mut self, channel: u8, frame: i64) {
        let slot = &mut self.pedal[(channel & 15) as usize];
        if *slot == CLOSED {
            self.open += 1;
            *slot = frame;
        }
    }

    #[inline]
    pub fn release_pedal(&mut self, channel: u8) {
        let slot = &mut self.pedal[(channel & 15) as usize];
        if *slot != CLOSED {
            *slot = CLOSED;
            self.open -= 1;
        }
    }

    /// Nothing open any more.
    #[inline]
    pub fn settled(&self) -> bool {
        self.open == 0
    }

    /// Take out the next open note or pedal at or after slot `cursor` (advanced past it), as
    /// `(channel, note or None for the pedal, start frame)`. Starting at 0 and calling until
    /// `None` empties everything in one pass over the slots.
    pub fn pop_one(&mut self, cursor: &mut usize) -> Option<(u8, Option<u8>, i64)> {
        while self.open > 0 && *cursor < 16 * 129 {
            let (c, k) = (*cursor / 129, *cursor % 129);
            *cursor += 1;
            if k < 128 {
                let slot = &mut self.on[c][k];
                if *slot != CLOSED {
                    let on = *slot;
                    *slot = CLOSED;
                    self.open -= 1;
                    return Some((c as u8, Some(k as u8), on));
                }
            } else if self.pedal[c] != CLOSED {
                let on = self.pedal[c];
                self.pedal[c] = CLOSED;
                self.open -= 1;
                return Some((c as u8, None, on));
            }
        }
        None
    }

    /// Every open note and pedal, as `(channel, note or None for the pedal, start frame)`, then
    /// forgotten. Bounded: 16 x 128 slots, and only while something is open.
    pub fn drain(&mut self, mut f: impl FnMut(u8, Option<u8>, i64)) {
        if self.open > 0 {
            for c in 0..16u8 {
                for n in 0..128u8 {
                    let slot = &mut self.on[c as usize][n as usize];
                    if *slot != CLOSED {
                        f(c, Some(n), *slot);
                        *slot = CLOSED;
                    }
                }
                if self.pedal[c as usize] != CLOSED {
                    f(c, None, self.pedal[c as usize]);
                    self.pedal[c as usize] = CLOSED;
                }
            }
        }
        self.open = 0;
        self.live = false;
    }
}

impl Default for OpenNotes {
    fn default() -> Self {
        Self::new()
    }
}

/// The frame a missing note off is put at: one frame before the end of the pass the note started
/// in.
#[inline]
pub fn pass_end(on_frame: i64, loop_len: u64) -> i64 {
    let l = loop_len.max(1) as i64;
    (on_frame.div_euclid(l) + 1) * l - 1
}

// ---------------------------------------------------------------------------------------------
// Queues between the control thread and the engine
// ---------------------------------------------------------------------------------------------

/// What the engine tells the control thread about the clips it records into.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ClipNews {
    /// A take began writing into `clip` on `track`.
    Begin { track: u8, clip: u32 },
    /// One event went into `clip`.
    Event { track: u8, clip: u32, raw: RawEvent },
}

/// A play list the control thread built for one clip.
pub struct ClipList {
    pub clip: u32,
    pub list: Vec<PlayEvent>,
}

/// The engine's end of the clip queues.
pub struct ClipEndpoint {
    install: Consumer<Box<MidiClip>>,
    retire: Producer<Box<MidiClip>>,
    lists: Consumer<ClipList>,
    lists_back: Producer<Vec<PlayEvent>>,
    news: Producer<ClipNews>,
    /// News that did not fit; the mirror of the control thread is then incomplete.
    pub news_lost: u64,
    pub leaked: u64,
}

impl ClipEndpoint {
    #[inline]
    pub fn take_new(&mut self) -> Option<Box<MidiClip>> {
        self.install.pop().ok()
    }

    /// Hand a clip back. Leaked rather than dropped if the queue is full, which cannot happen while
    /// the control thread drains it.
    #[inline]
    pub fn retire(&mut self, clip: Box<MidiClip>) {
        if let Err(PushError::Full(clip)) = self.retire.push(clip) {
            std::mem::forget(clip);
            self.leaked += 1;
        }
    }

    #[inline]
    pub fn next_list(&mut self) -> Option<ClipList> {
        self.lists.pop().ok()
    }

    #[inline]
    pub fn list_back(&mut self, list: Vec<PlayEvent>) {
        if let Err(PushError::Full(list)) = self.lists_back.push(list) {
            std::mem::forget(list);
            self.leaked += 1;
        }
    }

    #[inline]
    pub fn tell(&mut self, news: ClipNews) {
        if self.news.push(news).is_err() {
            self.news_lost += 1;
        }
    }
}

/// What the control thread knows about one clip.
struct Mirror {
    track: usize,
    raw: Vec<RawEvent>,
    /// Raw data changed since the last list was sent.
    dirty: bool,
    /// The clip plays a list from here; every change has to be sent.
    external: bool,
    /// Loop length the last list was built for.
    built_for: u64,
    grid: NoteGrid,
}

/// The control thread's side: keeps the engine supplied with empty clips, mirrors what is recorded
/// into them, and builds the play lists for every track that has a grid.
pub struct ClipPool {
    install: Producer<Box<MidiClip>>,
    retire: Consumer<Box<MidiClip>>,
    lists: Producer<ClipList>,
    lists_back: Consumer<Vec<PlayEvent>>,
    news: Consumer<ClipNews>,
    reserve: Vec<Box<MidiClip>>,
    next_id: u32,
    capacity: usize,
    mirrors: HashMap<u32, Mirror>,
    grids: [NoteGrid; MAX_TRACKS],
    /// Clips handed out since the start and taken back, for the tests.
    pub reclaimed: u64,
}

/// Fresh clip queues: the pool for the control thread, the endpoint for the engine.
pub fn clip_channel(capacity: usize) -> (ClipPool, ClipEndpoint) {
    let (itx, irx) = RingBuffer::new(CLIP_STOCK);
    let (rtx, rrx) = RingBuffer::new(MAX_TRACKS * 20 + 4 * CLIP_STOCK);
    let (ltx, lrx) = RingBuffer::new(256);
    let (lbtx, lbrx) = RingBuffer::new(512);
    let (ntx, nrx) = RingBuffer::new(CLIP_EVENTS);
    (
        ClipPool {
            install: itx,
            retire: rrx,
            lists: ltx,
            lists_back: lbrx,
            news: nrx,
            reserve: Vec::new(),
            next_id: 1,
            capacity,
            mirrors: HashMap::new(),
            grids: [NoteGrid::None; MAX_TRACKS],
            reclaimed: 0,
        },
        ClipEndpoint {
            install: irx,
            retire: rtx,
            lists: lrx,
            lists_back: lbtx,
            news: ntx,
            news_lost: 0,
            leaked: 0,
        },
    )
}

impl ClipPool {
    /// The grid track `track`'s MIDI layers are played on. Takes effect on every layer of it with
    /// the next [`ClipPool::service`] - also on the ones recorded before.
    pub fn set_grid(&mut self, track: usize, grid: NoteGrid) {
        if let Some(g) = self.grids.get_mut(track) {
            *g = grid;
        }
    }

    pub fn grid(&self, track: usize) -> NoteGrid {
        self.grids.get(track).copied().unwrap_or_default()
    }

    /// Every grid at once, e.g. after the engine was rebuilt.
    pub fn grids(&self) -> [NoteGrid; MAX_TRACKS] {
        self.grids
    }

    /// The raw events of one clip as the control thread knows them - for tests and the display.
    pub fn mirror(&self, clip: u32) -> Option<&[RawEvent]> {
        self.mirrors.get(&clip).map(|m| m.raw.as_slice())
    }

    /// One turn: take back what the engine returned, top up its stock, follow what it recorded,
    /// and send a new play list for every clip whose list is out of date. `loop_len` answers the
    /// loop length of a track (0 while none is known); `frames_per_tick` is the tempo.
    pub fn service(&mut self, loop_len: impl Fn(usize) -> u64, frames_per_tick: f64) {
        while let Ok(list) = self.lists_back.pop() {
            drop(list);
        }
        self.follow_news();
        // After the news: what the engine said about a clip comes before the clip itself.
        while let Ok(clip) = self.retire.pop() {
            self.reclaimed += 1;
            self.mirrors.remove(&clip.id());
            if self.reserve.len() < CLIP_STOCK * 2 {
                self.reserve.push(clip);
            }
        }
        while self.install.slots() > 0 {
            let id = self.next_id;
            self.next_id = self.next_id.wrapping_add(1).max(1);
            let clip = match self.reserve.pop() {
                Some(mut clip) => {
                    clip.recycle(id);
                    clip
                }
                None => MidiClip::new(id, self.capacity),
            };
            if self.install.push(clip).is_err() {
                break;
            }
        }
        for (&id, m) in self.mirrors.iter_mut() {
            let grid = self.grids.get(m.track).copied().unwrap_or_default();
            let len = loop_len(m.track);
            if len == 0 {
                continue;
            }
            // A clip nobody quantised keeps the list the engine builds itself.
            if !m.external && grid == NoteGrid::None {
                continue;
            }
            if !m.dirty && m.grid == grid && m.built_for == len {
                continue;
            }
            let list = build_play_list(&m.raw, len, grid, frames_per_tick);
            if self.lists.push(ClipList { clip: id, list }).is_ok() {
                m.dirty = false;
                m.external = true;
                m.grid = grid;
                m.built_for = len;
            }
        }
    }

    fn follow_news(&mut self) {
        while let Ok(news) = self.news.pop() {
            match news {
                ClipNews::Begin { track, clip } => {
                    self.mirrors.insert(clip, Mirror {
                        track: track as usize,
                        raw: Vec::new(),
                        dirty: false,
                        external: false,
                        built_for: 0,
                        grid: NoteGrid::None,
                    });
                }
                ClipNews::Event { track, clip, raw } => {
                    let m = self.mirrors.entry(clip).or_insert_with(|| Mirror {
                        track: track as usize,
                        raw: Vec::new(),
                        dirty: false,
                        external: false,
                        built_for: 0,
                        grid: NoteGrid::None,
                    });
                    m.raw.push(raw);
                    m.dirty = true;
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ev(frame: i64, msg: [u8; 3], fpt: f64) -> RawEvent {
        let tick = (frame as f64 / fpt).round() as i64;
        RawEvent {
            frame,
            ev: NoteEvent::from_midi(tick, msg).unwrap(),
        }
    }

    #[test]
    fn the_engine_list_and_the_built_list_without_grid_are_the_same() {
        let fpt = 24_000.0 / f64::from(PPQ);
        let len = 96_000u64;
        let raw = vec![
            ev(-200, [0x90, 36, 100], fpt),
            ev(0, [0x90, 60, 90], fpt),
            ev(300, [0x80, 36, 0], fpt),
            ev(12_000, [0x80, 60, 0], fpt),
            ev(95_999, [0x90, 62, 80], fpt),
            ev(96_100, [0x80, 62, 0], fpt),
            ev(50, [0xB0, 64, 127], fpt),
        ];
        let mut clip = MidiClip::new(1, 64);
        for e in &raw {
            clip.record(*e, len);
        }
        let built = build_play_list(&raw, len, NoteGrid::None, fpt);
        assert_eq!(clip.play(), built.as_slice());
        // The pre-roll hit sits right before the end; the wrapped off at the start, before the on.
        assert_eq!(built.first().unwrap(), &PlayEvent { offset: 0, msg: [0x90, 60, 90] });
        assert!(built.contains(&PlayEvent { offset: 95_800, msg: [0x90, 36, 100] }));
        assert!(built.contains(&PlayEvent { offset: 100, msg: [0x80, 62, 0] }));
    }

    #[test]
    fn offs_come_before_ons_at_the_same_offset() {
        let fpt = 1.0;
        let raw = vec![ev(10, [0x90, 60, 1], fpt), ev(10, [0x80, 61, 0], fpt)];
        let list = build_play_list(&raw, 100, NoteGrid::None, fpt);
        assert_eq!(list[0].msg, [0x80, 61, 0]);
    }

    #[test]
    fn a_grid_moves_the_note_and_keeps_its_length_and_leaves_the_raw_data() {
        let fpt = 24_000.0 / f64::from(PPQ);
        // 1/16 at 120 BPM: 6000 frames. A note 400 frames late and 1000 long; one 300 early
        // before the one, in the pre-roll.
        let raw = vec![
            ev(6_400, [0x90, 60, 100], fpt),
            ev(7_400, [0x80, 60, 0], fpt),
            ev(-300, [0x90, 36, 100], fpt),
            ev(500, [0x80, 36, 0], fpt),
            ev(6_450, [0xE0, 0, 70], fpt),
        ];
        let before = raw.clone();
        let list = build_play_list(&raw, 96_000, NoteGrid::Sixteenth, fpt);
        assert_eq!(raw, before);
        assert!(list.contains(&PlayEvent { offset: 6_000, msg: [0x90, 60, 100] }));
        assert!(list.contains(&PlayEvent { offset: 7_000, msg: [0x80, 60, 0] }));
        assert!(list.contains(&PlayEvent { offset: 0, msg: [0x90, 36, 100] }), "auf die Eins");
        assert!(list.contains(&PlayEvent { offset: 800, msg: [0x80, 36, 0] }));
        assert!(list.contains(&PlayEvent { offset: 6_450, msg: [0xE0, 0, 70] }), "Bend bleibt");
        let triplet = build_play_list(&raw, 96_000, NoteGrid::EighthTriplet, fpt);
        assert!(triplet.contains(&PlayEvent { offset: 8_000, msg: [0x90, 60, 100] }));
    }

    #[test]
    fn grid_names_round_trip() {
        for g in NoteGrid::ALL {
            assert_eq!(NoteGrid::parse(g.as_str()), Some(g));
        }
        assert_eq!(NoteGrid::parse("1/32"), None);
    }

    #[test]
    fn a_missing_off_goes_one_frame_before_the_end_of_its_pass() {
        assert_eq!(pass_end(100, 1_000), 999);
        assert_eq!(pass_end(1_100, 1_000), 1_999);
        assert_eq!(pass_end(-50, 1_000), -1);
    }
}
