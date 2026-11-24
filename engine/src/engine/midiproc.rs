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
//!   longer than the loop, in the tail or in the take) is ended one frame before the end of the
//!   note's pass (`super::super::midiloop::note_end`). A second tail ends the first one the same
//!   way.
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
//! A key the loop and the musician both hold is one voice: it ends when the last of them lets go
//! (`super::super::source`), so releasing a chord held over the end of the take does not silence
//! the loop that has struck it again.

use super::super::command::MAX_TRACKS;
use super::super::midiloop::{
    CLIP_STOCK, ClipEndpoint, ClipNews, MidiClip, OpenNotes, RawEvent, note_end,
};
use super::super::source::{NoteEvent, SourceRack, ticks_of_frames};
use super::super::track::{LayerStore, Track};
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
            let mut rack = self.sources.as_deref_mut();
            let out_pos = self.out_pos;
            'tracks: for (t, track) in self.tracks.iter_mut().enumerate() {
                if !track.midi_loop() {
                    continue;
                }
                let len = track.loop_len();
                // The read position the loop has played up to (`play_midi`), as a loop offset -
                // `None` while it does not play.
                let advance = rack.as_deref().map_or(0, |r| r.latency(t)) + track.playback_advance();
                let origin = track.grid().origin;
                let read = out_pos + advance;
                let at = (len > 0 && track.playing() && !track.held() && read >= origin)
                    .then(|| ((read - origin) % len) as u32);
                for layer in track.layers_mut() {
                    if let Some(clip) = layer.clip_mut()
                        && clip.id() == list.clip
                    {
                        clip.install_list(&mut list.list, len);
                        // What the old list started ends now: a note it played early would
                        // otherwise sound twice when the new list plays it later in the pass.
                        // A note the new list has sounding right here stays: its off is in the
                        // new list. (A chord held over the end of the take is quantised the
                        // moment its late off arrives; ending it then would silence the loop
                        // for the rest of the pass.)
                        if let Some(rack) = rack.as_deref_mut() {
                            let mut tracker = clip.tracker;
                            let play = &*clip;
                            tracker.release_except(
                                |c, n| at.is_some_and(|at| play.sounds_at(c, n, at)),
                                |msg| rack.push_now(t, msg),
                            );
                            clip.tracker = tracker;
                        }
                        break 'tracks;
                    }
                }
            }
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

    /// Forget the tail of `t` without writing its note-offs: the pass it belongs to was dropped,
    /// and its notes are handed to the next pass, which will see their note-offs itself.
    pub(super) fn midi_drop_tail(&mut self, t: usize) {
        if let Some(midi) = self.midi.as_deref_mut() {
            midi.tails[t].reset();
        }
    }

    /// End every note of the tail of `t` one frame before the end of its pass.
    fn close_tail(&mut self, t: usize) {
        let len = self.tracks[t].loop_len();
        let preroll = self.midi_preroll();
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
            let frame = note_end(on, len, preroll);
            let frame = self
                .midi
                .as_deref()
                .and_then(|m| m.tails[t].cut)
                .map_or(frame, |cut| frame.min(cut.max(on)));
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

    /// `Command::CutTake` arrived for the MIDI take that runs on `t` (it is sent right behind the
    /// take's start): from now on its notes know where the take stops taking them.
    pub(super) fn midi_cut_known(&mut self, t: usize) {
        let Some(take) = self.tracks[t].take() else {
            return;
        };
        let Some((at, from)) = self.cuts[t] else {
            return;
        };
        if at != take.start {
            return;
        }
        if let Some(midi) = self.midi.as_deref_mut()
            && midi.takes[t].live
        {
            let base = midi.takes[t].base;
            midi.takes[t].cut = Some(from as i64 - base as i64);
        }
    }

    /// A MIDI take began on `t`. `from_idle`: nothing was running before it on this track, so it
    /// may take its pre-roll. Also true when the pass before it was judged empty apart from notes
    /// in its last pre-roll window (`super::latch::Verdict::EarlyOnly`): those are taken over here.
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
        let preroll = self.midi_preroll();
        let sustain = self.sources.as_deref().map_or(0, |r| r.live_sustain(t));
        if let Some(c) = self.tracks[t].layer_mut(take.layer).and_then(|l| l.clip_mut()) {
            c.set_base(base);
        }
        let Some(midi) = self.midi.as_deref_mut() else {
            return;
        };
        midi.takes[t].start(clip, base);
        if let Some((at, from)) = self.cuts[t]
            && at == take.start
        {
            midi.takes[t].cut = Some(from as i64 - base as i64);
        }
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
        let preroll = self.midi_preroll();
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
        // A take with a cut: what is still held ends on the cut.
        let frame = tail.cut.map_or(frame, |cut| frame.min(cut));
        // Longer than the loop cannot be played back: it ends before its pass is over. Its off
        // must not be stored where it was played - wrapped, that is right after the note's own
        // start, and the note held over the end would be cut to the fragment after the wrap.
        let frame = if frame - start >= len as i64 {
            note_end(start, len, preroll)
        } else {
            frame
        };
        self.store_event(t, clip, frame, msg);
        if settled && let Some(midi) = self.midi.as_deref_mut() {
            midi.tails[t].reset();
        }
        if is_off(msg) {
            self.adopt_held(t, clip, pos, c, msg[1]);
        }
        only_tail
    }

    /// The musician lets go of a key he has held since the take of `clip` - over its end, into the
    /// loop's playback. If the loop has that note sounding here and has not struck it itself
    /// (its on lay before the loop started playing: a chord played a touch early for the one, on
    /// a track without a grid), the loop takes the held key over and its own off ends it; his
    /// release would otherwise silence the loop until the note comes round again.
    ///
    /// A grid's play list comes from the control thread and does not know this off yet; such a
    /// note is struck on its grid line by the loop itself, and the event stage keeps it sounding
    /// past the live release (`super::super::source`, one voice per key).
    fn adopt_held(&mut self, t: usize, clip: u32, pos: u64, channel: u8, note: u8) {
        let track = &self.tracks[t];
        let len = track.loop_len();
        let origin = track.grid().origin;
        if len == 0 || track.held() || pos < origin || !track.sounds_at(pos) {
            return;
        }
        let at = ((pos - origin) % len) as u32;
        let Some(rack) = self.sources.as_deref_mut() else {
            return;
        };
        for layer in self.tracks[t].layers_mut() {
            if layer.muted() {
                continue;
            }
            let Some(c) = layer.clip_mut() else {
                continue;
            };
            if c.id() != clip {
                continue;
            }
            if !c.external()
                && !c.tracker.is_on(channel, note)
                && c.sounds_at(channel, note, at)
                && rack.adopt(t, channel, note)
            {
                c.tracker.note([0x90 | channel, note, 1]);
            }
            break;
        }
    }

    /// An event at musical position `pos` for the running take of `t`, if it takes it.
    fn take_event(&mut self, t: usize, pos: u64, msg: [u8; 3]) {
        let Some(take) = self.tracks[t].take() else {
            return;
        };
        let len = self.tracks[t].loop_len();
        let preroll = self.midi_preroll();
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
        let mut frame = pos as i64 - notes.base as i64;
        let c = msg[0] & 0x0F;
        // The end of the take that is not recorded (`skip_end`): nothing new starts there, and
        // what is let go there ends on the cut.
        if let Some((at, from)) = self.cuts[t]
            && at == take.start
        {
            let cut = from as i64 - notes.base as i64;
            notes.cut = Some(cut);
            if frame >= cut {
                if is_on(msg) || (is_pedal(msg) && msg[2] >= 64) {
                    return;
                }
                frame = cut;
            }
        }
        // Where the note or pedal this event ends was started, if the take saw it start.
        let mut started: Option<i64> = None;
        let keep = if is_on(msg) {
            notes.open_note(c, msg[1], frame);
            true
        } else if is_off(msg) {
            started = notes.on_frame(c, msg[1]);
            notes.close_note(c, msg[1]);
            started.is_some()
        } else if is_pedal(msg) {
            if msg[2] >= 64 {
                let fresh = notes.pedal_open(c).is_none();
                notes.press_pedal(c, frame);
                fresh
            } else {
                started = notes.pedal_open(c);
                notes.release_pedal(c);
                started.is_some()
            }
        } else {
            true
        };
        if !keep {
            return;
        }
        // A key pressed a touch early for the one (the pre-roll) and let go just before the end:
        // longer than the loop, it cannot be played back as it was - stored as played, its off
        // would come right after its own on. It ends as a note held over the end does.
        if let Some(on) = started
            && len > 0
            && frame - on >= len as i64
        {
            frame = note_end(on, len, preroll);
        }
        let clip = notes.clip;
        if is_on(msg) {
            // Something was played: a latched pass with this in it is kept.
            self.latch.note_midi(t, pos);
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
    ///
    /// A re-anchoring (`Command::RestartLoop`) splits the window where the read position reaches
    /// it: before, the old grid (and only if the track plays); from there on, the new one. Every
    /// note the old phase left sounding is ended on that very sample, ahead of the new phase's
    /// first notes - its note-off would otherwise come from a part of the loop that is now skipped,
    /// and the note would hang.
    pub(super) fn play_midi(&mut self, from: u64, to: u64) {
        if self.midi.is_none() || from >= to {
            return;
        }
        let Some(rack) = self.sources.as_deref_mut() else {
            return;
        };
        for (t, track) in self.tracks.iter_mut().enumerate() {
            if !track.midi_loop() || track.held() {
                continue;
            }
            let len = track.loop_len();
            if len == 0 {
                continue;
            }
            let advance = rack.latency(t) + track.playback_advance();
            let (a, b) = (from + advance, to + advance);
            match track.restart() {
                Some((at, _)) if at < b => {
                    let old = track.grid().origin;
                    if track.playing() && a < at {
                        play_span(track, rack, t, a, at, old, advance);
                    }
                    if a <= at {
                        // The switch lies in this window: what the old phase left on ends here.
                        let off = at - advance;
                        for layer in track.layers_mut() {
                            if let Some(clip) = layer.clip_mut() {
                                clip.tracker.release_own(|msg| rack.push_loop(t, off, msg));
                            }
                        }
                    }
                    if track.sounds_at(at) {
                        play_span(track, rack, t, a.max(at), b, at, advance);
                    }
                }
                _ => {
                    if track.playing() {
                        let origin = track.grid().origin;
                        play_span(track, rack, t, a, b, origin, advance);
                    }
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

/// The events of every audible MIDI layer of `track` for read positions `a..b` on the grid that
/// starts at `origin`, handed to the event stage `advance` frames earlier on the output axis.
fn play_span(
    track: &mut Track,
    rack: &mut SourceRack,
    t: usize,
    a: u64,
    b: u64,
    origin: u64,
    advance: u64,
) {
    let len = track.loop_len();
    let a = a.max(origin);
    if a >= b || len == 0 {
        return;
    }
    for layer in track.layers_mut() {
        if layer.muted() {
            continue;
        }
        let gain = layer.gain();
        let Some(clip) = layer.clip_mut() else {
            continue;
        };
        let base = clip.base();
        let mut p = a;
        while p < b {
            let offset = (p - origin) % len;
            let pass = p - offset;
            let stop = b.min(pass + len);
            let (lo, hi) = clip.window(offset, offset + (stop - p));
            for i in lo..hi {
                let e = clip.play()[i];
                // Played live in this very pass of its take: it has sounded already.
                if e.heard_live(pass + u64::from(e.offset), base) {
                    continue;
                }
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
