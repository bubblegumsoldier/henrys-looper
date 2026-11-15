//! The engine's half of MIDI loops (`super::midiloop` has the data and the control side).
//!
//! # Recording
//!
//! A MIDI-loop track records nothing from its instrument's output. Every event routed to it comes
//! with the position the musician *heard* when he played it (`super::source::heard_at`), and that
//! position is the whole recording: it is exact whatever the rendering does, so the track's live
//! notes are rendered as early as possible instead of at the fixed delay `C`.
//!
//! The take machinery is the one audio takes use (`super::grid`); only what drives it differs. An
//! audio take is advanced by the input frames that reach it; a MIDI take by the heard positions of
//! its events and, once per block, by the furthest position an event can still come from
//! (`out_pos - C`). A take therefore closes at its end at the latest `C` frames after the output
//! has passed it, just as an audio take waits for its last `R` frames.
//!
//! # Boundary rules
//!
//! * The notes a take started and has not ended when it closes become its **tail**: their offs are
//!   still recorded into that layer, at the position they were played, past the end - the play
//!   list wraps them to the start. The tail ends when every note is up, at the latest one loop
//!   length after the take's end; what is still down then (and an off that would make a note
//!   longer than the loop) is ended one frame before the end of the note's pass. A second tail
//!   ends the first one the same way.
//! * Offs and pedal-ups of notes a take never saw start are not recorded - they belong to
//!   something else. A pedal held down when a take begins is recorded as pressed at its start.
//! * A take that begins from nothing accepts events from a 32nd note before its start on (the
//!   pre-roll); see `super::midiloop`.
//!
//! # Playback
//!
//! Each rendered piece of a block (commands cut blocks into pieces) asks every playing MIDI layer
//! for its events in the window of the piece, moved earlier by the instrument's latency plus the
//! track's plugin latency, so they sound on time. They enter the track's event stage with the live
//! events and go to the same instrument. Each layer tracks what it has sounding; stopping, holding,
//! muting, undoing or clearing a layer ends exactly those notes and leaves the live keys alone.

use super::super::command::MAX_TRACKS;
use super::super::frame::Frame;
use super::super::midiloop::{
    CLIP_STOCK, ClipEndpoint, ClipNews, MidiClip, OpenNotes, RawEvent, pass_end,
};
use super::super::source::{NoteEvent, ticks_of_frames};
use super::super::track::LayerStore;
use super::EngineCore;

/// Events of the last moments per track, so a take that begins can take its pre-roll.
const RECENT: usize = 64;

/// The engine's MIDI-loop state. Allocated on the control thread in [`EngineCore::attach_clips`].
pub struct MidiLoops {
    pub(super) endpoint: ClipEndpoint,
    stock: Vec<Box<MidiClip>>,
    takes: Vec<OpenNotes>,
    tails: Vec<OpenNotes>,
    recent: Vec<[(i64, [u8; 3]); RECENT]>,
    recent_next: [usize; MAX_TRACKS],
    recent_len: [usize; MAX_TRACKS],
    /// Per track: the earliest position the running take accepts, below its start while it may
    /// take a pre-roll.
    accept_from: [u64; MAX_TRACKS],
}

impl MidiLoops {
    pub fn new(endpoint: ClipEndpoint) -> Self {
        Self {
            endpoint,
            stock: Vec::with_capacity(CLIP_STOCK),
            takes: (0..MAX_TRACKS).map(|_| OpenNotes::new()).collect(),
            tails: (0..MAX_TRACKS).map(|_| OpenNotes::new()).collect(),
            recent: vec![[(0, [0; 3]); RECENT]; MAX_TRACKS],
            recent_next: [0; MAX_TRACKS],
            recent_len: [0; MAX_TRACKS],
            accept_from: [0; MAX_TRACKS],
        }
    }

    #[cfg(test)]
    pub fn stock(&self) -> usize {
        self.stock.len()
    }
}

/// A note-off message, whatever form it came in.
#[inline]
fn is_off(msg: [u8; 3]) -> bool {
    msg[0] & 0xF0 == 0x80 || (msg[0] & 0xF0 == 0x90 && msg[2] == 0)
}

#[inline]
fn is_on(msg: [u8; 3]) -> bool {
    msg[0] & 0xF0 == 0x90 && msg[2] > 0
}

#[inline]
fn is_pedal(msg: [u8; 3]) -> bool {
    msg[0] & 0xF0 == 0xB0 && msg[1] == 64
}

impl EngineCore {
    /// Give the engine its MIDI-loop stage. Control thread, before the engine moves into the
    /// callback.
    pub fn attach_clips(&mut self, endpoint: ClipEndpoint) {
        self.midi = Some(Box::new(MidiLoops::new(endpoint)));
    }

    /// Top the clip stock up and swap in the play lists the control thread built.
    pub(super) fn collect_clips(&mut self) {
        let Some(midi) = self.midi.as_deref_mut() else {
            return;
        };
        while midi.stock.len() < CLIP_STOCK {
            let Some(clip) = midi.endpoint.take_new() else {
                break;
            };
            midi.stock.push(clip);
        }
        for _ in 0..16 {
            let Some(mut list) = midi.endpoint.next_list() else {
                break;
            };
            let mut found = false;
            'tracks: for track in self.tracks.iter_mut() {
                if !track.midi_loop() {
                    continue;
                }
                for layer in track.layers_mut() {
                    if let Some(clip) = layer.clip_mut()
                        && clip.id() == list.clip
                    {
                        clip.install_list(&mut list.list);
                        found = true;
                        break 'tracks;
                    }
                }
            }
            let _ = found;
            midi.endpoint.list_back(list.list);
        }
    }

    /// A clip for a new MIDI take, if one is ready.
    pub(super) fn take_clip(&mut self) -> Option<Box<MidiClip>> {
        let mut clip = self.midi.as_deref_mut()?.stock.pop()?;
        clip.set_max_frames(self.layer_frames);
        Some(clip)
    }

    /// Storage for a new take of `track`: a clip on a MIDI-loop track, a buffer otherwise.
    pub(super) fn take_store(&mut self, track: usize) -> Option<LayerStore> {
        if self.tracks[track].midi_loop() {
            self.take_clip().map(LayerStore::Midi)
        } else {
            let channels = self.tracks[track].channels();
            self.take_spare(channels).map(LayerStore::Audio)
        }
    }

    /// Hand a layer's storage back to the control thread, whatever it holds. A clip first ends
    /// what its playback has sounding.
    pub(super) fn retire_store(&mut self, track: usize, store: LayerStore) {
        match store {
            LayerStore::Audio(buffer) => self.buffers.retire(buffer),
            LayerStore::Midi(mut clip) => {
                if let Some(rack) = self.sources.as_deref_mut() {
                    clip.tracker.release_own(|msg| rack.push_now(track, msg));
                }
                match self.midi.as_deref_mut() {
                    Some(midi) => midi.endpoint.retire(clip),
                    // Cannot happen - a clip only comes from that stage - and is never dropped here.
                    None => std::mem::forget(clip),
                }
            }
        }
    }

    /// End what the MIDI layers of `track` have sounding - all of them, or only layer `only`.
    pub(super) fn release_layers(&mut self, track: usize, only: Option<usize>) {
        let Some(rack) = self.sources.as_deref_mut() else {
            return;
        };
        let Some(t) = self.tracks.get_mut(track) else {
            return;
        };
        if !t.midi_loop() {
            return;
        }
        for (i, layer) in t.layers_mut().iter_mut().enumerate() {
            if only.is_some_and(|o| o != i) {
                continue;
            }
            if let Some(clip) = layer.clip_mut() {
                clip.tracker.release_own(|msg| rack.push_now(track, msg));
            }
        }
    }

    /// The MIDI part of a process call that comes before rendering: tell the event stage which
    /// tracks are MIDI loops, then record what was played.
    pub(super) fn record_midi(&mut self) {
        if self.midi.is_none() {
            return;
        }
        for t in 0..self.tracks.len().min(MAX_TRACKS) {
            let midi_loop = self.tracks[t].midi_loop();
            let Some(rack) = self.sources.as_deref_mut() else {
                return;
            };
            rack.set_midi(t, midi_loop);
            if !midi_loop {
                continue;
            }
            // In arrival order per port; put them in time order (stable, in place, a handful).
            let heard = &mut rack.heard[t];
            for i in 1..heard.len() {
                let mut j = i;
                while j > 0 && heard[j - 1].0 > heard[j].0 {
                    heard.swap(j - 1, j);
                    j -= 1;
                }
            }
            let n = heard.len();
            for i in 0..n {
                let Some((m, msg)) = self.sources.as_deref().and_then(|r| r.heard[t].get(i).copied())
                else {
                    break;
                };
                self.midi_event(t, m, msg);
            }
        }
    }

    /// After a process call: every MIDI take has come as far as an event can still come from.
    pub(super) fn advance_midi(&mut self) {
        if self.midi.is_none() {
            return;
        }
        for t in 0..self.tracks.len().min(MAX_TRACKS) {
            if self.tracks[t].midi_loop() {
                let pos = self.musical_input_pos(t);
                self.midi_advance(t, pos);
            }
        }
    }

    /// Close what has ended by musical position `pos`.
    fn midi_advance(&mut self, t: usize, pos: u64) {
        for _ in 0..4 {
            let Some(take) = self.tracks[t].take() else {
                break;
            };
            if pos < take.start {
                break;
            }
            self.tracks[t].advance_filled(pos);
            let ended = match take.end {
                Some(end) if pos >= end => Some(end),
                _ => match self.tracks[t].take_limit() {
                    Some(limit) if pos >= limit => Some(limit),
                    _ => None,
                },
            };
            match ended {
                Some(end) => self.finish_take(t, end),
                None => break,
            }
        }
        let expired = self
            .midi
            .as_deref()
            .is_some_and(|m| m.tails[t].live && pos >= m.tails[t].until);
        if expired {
            self.close_tail(t);
        }
    }

    /// The running MIDI take of `t` is about to close at `end`: what it has open becomes its tail.
    pub(super) fn midi_take_closing(&mut self, t: usize) {
        if !self.tracks[t].midi_loop() {
            return;
        }
        self.close_tail(t);
        let Some(midi) = self.midi.as_deref_mut() else {
            return;
        };
        let (takes, tails) = (&mut midi.takes, &mut midi.tails);
        std::mem::swap(&mut takes[t], &mut tails[t]);
        takes[t].reset();
        if tails[t].settled() {
            tails[t].reset();
        }
    }

    /// The take has closed and the loop length is final: give the tail its deadline.
    pub(super) fn midi_take_closed(&mut self, t: usize, end: u64) {
        let len = self.tracks[t].loop_len();
        if let Some(midi) = self.midi.as_deref_mut() {
            midi.tails[t].until = end + len;
        }
    }

    /// End every note of the tail of `t` one frame before the end of its pass.
    fn close_tail(&mut self, t: usize) {
        let len = self.tracks[t].loop_len();
        let Some(midi) = self.midi.as_deref_mut() else {
            return;
        };
        if !midi.tails[t].live {
            return;
        }
        let clip = midi.tails[t].clip;
        // One slot scan over 16 x 129 entries in all, however many are open.
        let mut cursor = 0usize;
        loop {
            let Some((c, note, on)) = self
                .midi
                .as_deref_mut()
                .and_then(|m| m.tails[t].pop_one(&mut cursor))
            else {
                break;
            };
            let frame = pass_end(on, len);
            let msg = match note {
                Some(n) => [0x80 | c, n, 0],
                None => [0xB0 | c, 64, 0],
            };
            self.store_event(t, clip, frame, msg);
        }
        if let Some(midi) = self.midi.as_deref_mut() {
            midi.tails[t].reset();
        }
    }

    /// A MIDI take began on `t`. `from_idle`: nothing was running before it on this track, so it
    /// may take its pre-roll.
    pub(super) fn midi_take_begun(&mut self, t: usize, from_idle: bool) {
        if !self.tracks[t].midi_loop() {
            return;
        }
        let Some(take) = self.tracks[t].take() else {
            return;
        };
        let Some(clip) = self
            .tracks[t]
            .layer(take.layer)
            .and_then(|l| l.clip())
            .map(MidiClip::id)
        else {
            return;
        };
        let origin = self.tracks[t].origin();
        let len = self.tracks[t].loop_len();
        let base = if take.defines_loop || len == 0 || take.start < origin {
            take.start
        } else {
            take.start - (take.start - origin) % len
        };
        let preroll = (self.timeline.samples_per_quarter() / 8.0).round() as u64;
        let sustain = self.sources.as_deref().map_or(0, |r| r.live_sustain(t));
        let Some(midi) = self.midi.as_deref_mut() else {
            return;
        };
        midi.takes[t].start(clip, base);
        midi.accept_from[t] = if from_idle {
            take.start.saturating_sub(preroll)
        } else {
            take.start
        };
        midi.endpoint.tell(ClipNews::Begin {
            track: t as u8,
            clip,
        });
        // A pedal that is down already holds what this take will play.
        for c in 0..16u8 {
            if sustain & (1 << c) != 0 {
                self.take_event(t, take.start, [0xB0 | c, 64, 127]);
            }
        }
        if from_idle {
            // What was played just before the start and has already arrived.
            let Some(midi) = self.midi.as_deref() else {
                return;
            };
            let from = midi.accept_from[t];
            let len = midi.recent_len[t];
            let first = (midi.recent_next[t] + RECENT - len) % RECENT;
            let mut early = [(0i64, [0u8; 3]); RECENT];
            let mut n = 0;
            for k in 0..len {
                let (m, msg) = midi.recent[t][(first + k) % RECENT];
                if m >= from as i64 && m < take.start as i64 {
                    early[n] = (m, msg);
                    n += 1;
                }
            }
            for &(m, msg) in &early[..n] {
                self.take_event(t, m as u64, msg);
            }
        }
    }

    /// One event that reached MIDI-loop track `t`, heard at `m`.
    fn midi_event(&mut self, t: usize, m: i64, msg: [u8; 3]) {
        if let Some(midi) = self.midi.as_deref_mut() {
            let i = midi.recent_next[t];
            midi.recent[t][i] = (m, msg);
            midi.recent_next[t] = (i + 1) % RECENT;
            midi.recent_len[t] = (midi.recent_len[t] + 1).min(RECENT);
        }
        if m < 0 {
            return;
        }
        let pos = m as u64;
        self.midi_advance(t, pos);
        if self.tail_event(t, pos, msg) {
            return;
        }
        self.take_event(t, pos, msg);
    }

    /// An off or pedal-up of a note the tail still waits for. Returns true when the event was the
    /// tail's alone.
    fn tail_event(&mut self, t: usize, pos: u64, msg: [u8; 3]) -> bool {
        let len = self.tracks[t].loop_len();
        let Some(midi) = self.midi.as_deref_mut() else {
            return false;
        };
        let tail = &mut midi.tails[t];
        if !tail.live || len == 0 {
            return false;
        }
        let c = msg[0] & 0x0F;
        let frame = pos as i64 - tail.base as i64;
        let clip = tail.clip;
        let (start, only_tail) = if is_off(msg) {
            match tail.on_frame(c, msg[1]) {
                Some(on) => {
                    tail.close_note(c, msg[1]);
                    (on, true)
                }
                None => return false,
            }
        } else if is_pedal(msg) && msg[2] < 64 {
            match tail.pedal_open(c) {
                Some(on) => {
                    tail.release_pedal(c);
                    // The take may hold the pedal as well; it gets the release too.
                    (on, false)
                }
                None => return false,
            }
        } else {
            return false;
        };
        let settled = tail.settled();
        // Longer than the loop cannot be played back: it ends before its pass is over.
        let frame = if frame - start >= len as i64 {
            pass_end(start, len)
        } else {
            frame
        };
        self.store_event(t, clip, frame, msg);
        if settled && let Some(midi) = self.midi.as_deref_mut() {
            midi.tails[t].reset();
        }
        only_tail
    }

    /// An event at musical position `pos` for the running take of `t`, if it takes it.
    fn take_event(&mut self, t: usize, pos: u64, msg: [u8; 3]) {
        let Some(take) = self.tracks[t].take() else {
            return;
        };
        let Some(midi) = self.midi.as_deref_mut() else {
            return;
        };
        let notes = &mut midi.takes[t];
        if !notes.live || pos < midi.accept_from[t].min(take.start) {
            return;
        }
        if take.end.is_some_and(|end| pos >= end) {
            return;
        }
        let frame = pos as i64 - notes.base as i64;
        let c = msg[0] & 0x0F;
        let keep = if is_on(msg) {
            notes.open_note(c, msg[1], frame);
            true
        } else if is_off(msg) {
            let open = notes.is_open(c, msg[1]);
            notes.close_note(c, msg[1]);
            open
        } else if is_pedal(msg) {
            if msg[2] >= 64 {
                let fresh = notes.pedal_open(c).is_none();
                notes.press_pedal(c, frame);
                fresh
            } else {
                let open = notes.pedal_open(c).is_some();
                notes.release_pedal(c);
                open
            }
        } else {
            true
        };
        if !keep {
            return;
        }
        let clip = notes.clip;
        if is_on(msg) {
            // Something was played: a latched pass with this in it is kept.
            self.latch.note(t, Frame::mono(1.0));
        }
        self.store_event(t, clip, frame, msg);
    }

    /// Put one event into clip `clip` of track `t` and tell the control thread.
    fn store_event(&mut self, t: usize, clip: u32, frame: i64, msg: [u8; 3]) {
        let tick = ticks_of_frames(&self.timeline, frame);
        let Some(ev) = NoteEvent::from_midi(tick, msg) else {
            return;
        };
        let raw = RawEvent { frame, ev };
        let len = self.tracks[t].loop_len();
        let mut stored = false;
        for layer in self.tracks[t].layers_mut() {
            if let Some(c) = layer.clip_mut()
                && c.id() == clip
            {
                stored = c.record(raw, len);
                break;
            }
        }
        if stored && let Some(midi) = self.midi.as_deref_mut() {
            midi.endpoint.tell(ClipNews::Event {
                track: t as u8,
                clip,
                raw,
            });
        }
    }

    /// The running loop-defining take of `t` has just learned its length: its layer can play.
    pub(super) fn midi_length_known(&mut self, t: usize) {
        if !self.tracks[t].midi_loop() {
            return;
        }
        let len = self.tracks[t].loop_len();
        if let Some(take) = self.tracks[t].take()
            && take.defines_loop
            && let Some(clip) = self.tracks[t].layer_mut(take.layer).and_then(|l| l.clip_mut())
        {
            clip.rebuild_play(len);
        }
    }

    /// Hand every playing MIDI layer's events for output positions `from..to` to the event stage.
    pub(super) fn play_midi(&mut self, from: u64, to: u64) {
        if self.midi.is_none() || from >= to {
            return;
        }
        let Some(rack) = self.sources.as_deref_mut() else {
            return;
        };
        for (t, track) in self.tracks.iter_mut().enumerate() {
            if !track.midi_loop() || !track.playing() || track.held() {
                continue;
            }
            let len = track.loop_len();
            if len == 0 {
                continue;
            }
            let advance = rack.latency(t) + track.playback_advance();
            let origin = track.origin();
            let a = (from + advance).max(origin);
            let b = to + advance;
            if a >= b {
                continue;
            }
            for layer in track.layers_mut() {
                if layer.muted() {
                    continue;
                }
                let gain = layer.gain();
                let Some(clip) = layer.clip_mut() else {
                    continue;
                };
                let mut p = a;
                while p < b {
                    let offset = (p - origin) % len;
                    let pass = p - offset;
                    let stop = b.min(pass + len);
                    let (lo, hi) = clip.window(offset, offset + (stop - p));
                    for i in lo..hi {
                        let e = clip.play()[i];
                        let mut msg = e.msg;
                        if is_on(msg) && gain != 1.0 {
                            let v = (f32::from(msg[2]) * gain).round().clamp(0.0, 127.0) as u8;
                            if v == 0 {
                                continue;
                            }
                            msg[2] = v;
                        }
                        clip.tracker.note(msg);
                        let at = pass + u64::from(e.offset) - advance;
                        rack.push_loop(t, at, msg);
                    }
                    p = stop;
                }
            }
        }
    }

    /// Note ons recorded per layer of a track, for the status.
    pub(super) fn layer_notes(&self, t: usize) -> [u16; super::super::track::MAX_LAYERS] {
        let mut out = [0u16; super::super::track::MAX_LAYERS];
        for (i, layer) in self.tracks[t].layers().iter().enumerate() {
            if let Some(clip) = layer.clip() {
                out[i] = clip.notes().min(u32::from(u16::MAX)) as u16;
            }
        }
        out
    }

    #[cfg(test)]
    pub fn clip_stock(&self) -> usize {
        self.midi.as_deref().map_or(0, MidiLoops::stock)
    }
}
