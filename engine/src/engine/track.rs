//! Tracks, layers, and the loop geometry every layer of a track shares.
//!
//! Mono `Vec<f32>` per layer (the plan settles this: guitar and voice are mono, which halves memory
//! and CPU). Every buffer is allocated *and zeroed* by the control thread and handed over; the audio
//! thread only writes into it, reads from it, and hands it back. It never grows, never shrinks and
//! is never zero-filled inside the callback.
//!
//! # Why all layers of a track are aligned by construction
//!
//! A track owns exactly two numbers that define its grid:
//!
//! * `origin` - the musical position that loop index 0 corresponds to, set by the first take.
//! * `loop_len` - the loop length in samples, likewise set by the first take.
//!
//! Every layer, no matter which bar it was recorded in, is addressed as
//!
//! ```text
//! index = (musical position - origin) mod loop_len
//! ```
//!
//! so loop index `n` means the same musical instant in every layer. There is no per-layer offset
//! that could drift, and mixing is a plain sum over the same index. A layer recorded in bar 41 is
//! therefore aligned with the first one to the sample - not approximately, but by arithmetic.
//!
//! Two consequences of the zeroed buffer are worth spelling out:
//!
//! * Positions an overdub never reached stay 0.0 and add nothing to the sum. No "written" bitmap is
//!   needed, and nothing has to be cleared inside the callback.
//! * While an overdub is being recorded, the sample it is about to write at index `i` is read `R`
//!   samples *before* it is written (the write pointer trails the play pointer by the latency
//!   compensation), so the running layer contributes silence on its own first pass. It becomes
//!   audible on the next pass, which is exactly what overdubbing sounds like.

/// Hard ceiling of layers per track. Not a pre-allocation: layers are allocated one at a time in
/// loop length. It exists so a runaway overdub hits a clear German message instead of the memory
/// limit of the machine.
pub const MAX_LAYERS: usize = 16;

/// What the track is doing, for the display.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum TrackState {
    /// No layer.
    #[default]
    Empty,
    /// A recording is scheduled but its start has not been reached yet.
    Armed,
    /// The first take is running; it defines loop length and origin.
    Recording,
    /// A further layer is being recorded into an existing loop.
    Overdub,
    /// Content available, silent.
    Ready,
    /// Content available, playing.
    Playing,
}

impl TrackState {
    /// German label for the terminal display.
    pub fn label(self) -> &'static str {
        match self {
            TrackState::Empty => "leer",
            TrackState::Armed => "scharf",
            TrackState::Recording => "Aufnahme",
            TrackState::Overdub => "Overdub",
            TrackState::Ready => "bereit",
            TrackState::Playing => "Wiedergabe",
        }
    }
}

/// One recorded layer: a buffer somebody else allocated, plus how it is mixed.
pub struct Layer {
    /// Pre-allocated and zeroed by the control thread. Its length is the maximum loop length.
    buffer: Vec<f32>,
    gain: f32,
    muted: bool,
}

impl Layer {
    fn new(buffer: Vec<f32>) -> Self {
        Self {
            buffer,
            gain: 1.0,
            muted: false,
        }
    }

    /// Layer content, for tests and later WAV export.
    #[cfg(test)]
    pub fn content(&self, len: u64) -> &[f32] {
        &self.buffer[..len as usize]
    }
}

/// A take that is already scheduled while the previous one is still finishing.
///
/// This exists because of the latency compensation: when the output reaches the end of a take, its
/// last `R` samples are still travelling in from the interface, so the take cannot be closed yet.
/// Recording eight bars and overdubbing the next eight without a pause - the normal way to build a
/// loop - would otherwise collide with itself. The buffer is fetched at scheduling time, in the
/// control thread's supply, and only becomes a layer when the running take really ends.
pub struct PendingTake {
    start: u64,
    end: Option<u64>,
    defines_loop: bool,
    buffer: Vec<f32>,
}

/// Open recording window of one track, in musical coordinates.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Take {
    /// First musical sample that goes into the layer.
    pub start: u64,
    /// One past the last musical sample, once a stop has been received.
    pub end: Option<u64>,
    /// Index of the layer this take writes into.
    pub layer: usize,
    /// A take that defines the loop geometry (the first layer of a track) writes sequentially from
    /// index 0; an overdub writes into the existing grid and wraps at the loop end.
    pub defines_loop: bool,
}

pub struct Track {
    /// Zero-based channel index of the interleaved input frame this track records.
    input_channel: usize,
    /// Room for `MAX_LAYERS` from the start, so pushing a layer never reallocates.
    layers: Vec<Layer>,
    /// Musical position that loop index 0 corresponds to.
    origin: u64,
    /// Loop length in samples; 0 means "no loop yet".
    loop_len: u64,
    /// Samples written contiguously during a loop-defining take. Only that take needs it: it is
    /// what bounds the loop length when the take is closed.
    filled: u64,
    take: Option<Take>,
    pending: Option<PendingTake>,
    playing: bool,
    monitor: bool,
    /// Monitoring state a fresh start would have, restored by "alles loeschen".
    monitor_default: bool,
    /// Peak of this track's input channel since the last status snapshot.
    input_peak: f32,
    /// Peak this track contributed to the output since the last status snapshot, i.e. the sum of
    /// its audible layers. Monitoring is not part of it - that path never touches a layer.
    output_peak: f32,
}

impl Track {
    /// Built by the control thread - the layer vector is the only allocation, and it happens here.
    pub fn new(input_channel: usize, monitor: bool) -> Self {
        Self {
            input_channel,
            layers: Vec::with_capacity(MAX_LAYERS),
            origin: 0,
            loop_len: 0,
            filled: 0,
            take: None,
            pending: None,
            playing: false,
            monitor,
            monitor_default: monitor,
            input_peak: 0.0,
            output_peak: 0.0,
        }
    }

    #[inline]
    pub fn input_channel(&self) -> usize {
        self.input_channel
    }

    #[inline]
    pub fn layer_count(&self) -> usize {
        self.layers.len()
    }

    #[inline]
    pub fn loop_len(&self) -> u64 {
        self.loop_len
    }

    #[inline]
    pub fn filled(&self) -> u64 {
        self.filled
    }

    #[inline]
    pub fn has_content(&self) -> bool {
        !self.layers.is_empty() && self.loop_len > 0
    }

    #[inline]
    pub fn playing(&self) -> bool {
        self.playing
    }

    #[inline]
    pub fn set_playing(&mut self, on: bool) {
        self.playing = on;
    }

    #[inline]
    pub fn monitor(&self) -> bool {
        self.monitor
    }

    #[inline]
    pub fn set_monitor(&mut self, on: bool) {
        self.monitor = on;
    }

    #[inline]
    pub fn take(&self) -> Option<Take> {
        self.take
    }

    /// Bit `i` is set when layer `i` is muted. Fits `MAX_LAYERS` and travels in the status snapshot.
    pub fn muted_mask(&self) -> u32 {
        let mut mask = 0u32;
        for (i, layer) in self.layers.iter().enumerate() {
            if layer.muted {
                mask |= 1 << i;
            }
        }
        mask
    }

    pub fn state(&self, musical_input_pos: u64) -> TrackState {
        match self.take {
            Some(take) if musical_input_pos < take.start => TrackState::Armed,
            Some(take) if take.defines_loop => TrackState::Recording,
            Some(_) => TrackState::Overdub,
            None if self.layers.is_empty() => TrackState::Empty,
            None if self.playing => TrackState::Playing,
            None => TrackState::Ready,
        }
    }

    #[inline]
    pub fn note_input_peak(&mut self, magnitude: f32) {
        if magnitude > self.input_peak {
            self.input_peak = magnitude;
        }
    }

    #[inline]
    pub fn take_input_peak(&mut self) -> f32 {
        std::mem::replace(&mut self.input_peak, 0.0)
    }

    #[inline]
    pub fn note_output_peak(&mut self, magnitude: f32) {
        if magnitude > self.output_peak {
            self.output_peak = magnitude;
        }
    }

    #[inline]
    pub fn take_output_peak(&mut self) -> f32 {
        std::mem::replace(&mut self.output_peak, 0.0)
    }

    /// Sum of all audible layers at musical position `pos`.
    ///
    /// Positions that were never written return silence rather than stale memory, because every
    /// buffer arrives zeroed from the control thread.
    #[inline]
    pub fn read(&self, pos: u64) -> f32 {
        if self.loop_len == 0 || pos < self.origin {
            return 0.0;
        }
        let idx = ((pos - self.origin) % self.loop_len) as usize;
        let mut sum = 0.0f32;
        for layer in &self.layers {
            if layer.muted {
                continue;
            }
            // `loop_len` never exceeds a buffer length, so this always hits; `get` keeps a
            // hypothetical mismatch from panicking inside the audio callback.
            if let Some(&v) = layer.buffer.get(idx) {
                sum += v * layer.gain;
            }
        }
        sum
    }

    /// Write one sample of the running take. `pos` is the *musical* position of the sample, i.e.
    /// latency compensation has already been applied by the caller.
    #[inline]
    pub fn write(&mut self, pos: u64, sample: f32) {
        let Some(take) = self.take else {
            return;
        };
        let Some(layer) = self.layers.get_mut(take.layer) else {
            return;
        };
        let idx = if take.defines_loop {
            let idx = pos.wrapping_sub(self.origin);
            if idx >= layer.buffer.len() as u64 {
                return;
            }
            // A loop-defining take is written strictly sequentially, so this is the only value
            // `idx` can have.
            debug_assert_eq!(idx, self.filled, "Luecke im Loop-Puffer");
            self.filled = idx + 1;
            idx
        } else {
            if self.loop_len == 0 {
                return;
            }
            (pos - self.origin) % self.loop_len
        };
        if let Some(slot) = layer.buffer.get_mut(idx as usize) {
            *slot = sample;
        }
    }

    /// Musical position at which the running take has to end at the latest, if any.
    ///
    /// A loop-defining take is bounded by the buffer; an overdub is bounded by one pass through the
    /// loop, so it can never overwrite what it has just recorded.
    #[inline]
    pub fn take_limit(&self) -> Option<u64> {
        let take = self.take?;
        let layer = self.layers.get(take.layer)?;
        if take.defines_loop {
            Some(take.start + layer.buffer.len() as u64)
        } else {
            Some(take.start + self.loop_len)
        }
    }

    /// Start the first take of a new loop. The caller has already returned the old layers.
    pub fn begin_loop_take(&mut self, start: u64, buffer: Vec<f32>) {
        debug_assert!(self.layers.len() < MAX_LAYERS);
        self.origin = start;
        self.loop_len = 0;
        self.filled = 0;
        self.playing = false;
        self.layers.push(Layer::new(buffer));
        self.take = Some(Take {
            start,
            end: None,
            layer: self.layers.len() - 1,
            defines_loop: true,
        });
    }

    /// Start a further layer on the existing grid.
    pub fn begin_overdub_take(&mut self, start: u64, buffer: Vec<f32>) {
        debug_assert!(self.layers.len() < MAX_LAYERS);
        self.layers.push(Layer::new(buffer));
        self.take = Some(Take {
            start,
            end: None,
            layer: self.layers.len() - 1,
            defines_loop: false,
        });
    }

    /// Is a take running whose end is already known, i.e. one that is only waiting for its last
    /// `R` samples to arrive? A further take may be scheduled behind it.
    #[inline]
    pub fn take_is_finishing(&self) -> bool {
        self.take.map(|t| t.end.is_some()).unwrap_or(false)
    }

    #[inline]
    pub fn pending_defines_loop(&self) -> bool {
        self.pending.as_ref().map(|p| p.defines_loop).unwrap_or(false)
    }

    /// Queue a take behind the running one. Returns the buffer of a pending take that was replaced.
    pub fn set_pending(
        &mut self,
        start: u64,
        defines_loop: bool,
        buffer: Vec<f32>,
    ) -> Option<Vec<f32>> {
        let previous = self.pending.take().map(|p| p.buffer);
        self.pending = Some(PendingTake {
            start,
            end: None,
            defines_loop,
            buffer,
        });
        previous
    }

    /// Start the queued take now that the previous one is closed. Returns its buffer instead when
    /// it cannot be started after all.
    pub fn promote_pending(&mut self) -> Option<Vec<f32>> {
        let pending = self.pending.take()?;
        if self.layers.len() >= MAX_LAYERS {
            return Some(pending.buffer);
        }
        if pending.defines_loop {
            self.begin_loop_take(pending.start, pending.buffer);
        } else {
            self.begin_overdub_take(pending.start, pending.buffer);
        }
        if let Some(end) = pending.end {
            self.set_take_end(end);
        }
        None
    }

    /// Hand back the buffer of a queued take, cancelling it.
    pub fn take_pending_buffer(&mut self) -> Option<Vec<f32>> {
        self.pending.take().map(|p| p.buffer)
    }

    /// Announce where the running take ends, before the last sample has been written.
    ///
    /// Needed because the write pointer trails the play pointer by the compensated latency: when
    /// the output reaches the end of the take, the last `R` samples are still on their way in.
    /// Publishing the length early is what lets playback start seamlessly at the loop boundary;
    /// the zeroed buffer keeps the not-yet-written tail silent instead of stale.
    pub fn set_take_end(&mut self, end: u64) {
        // A stop that arrives after a further take was scheduled belongs to that one - the control
        // thread always sends start and stop of a take as a pair, in that order.
        if let Some(pending) = self.pending.as_mut() {
            pending.end = Some(end.max(pending.start));
            return;
        }
        let capacity = self.take_capacity();
        if let Some(take) = self.take.as_mut() {
            let end = end.max(take.start);
            take.end = Some(end);
            if take.defines_loop {
                self.loop_len = (end - take.start).min(capacity);
            }
        }
    }

    /// Close the take. `end` is the musical position just after its last sample.
    pub fn finish_take(&mut self, end: u64) {
        if let Some(take) = self.take.take()
            && take.defines_loop
        {
            self.loop_len = end.saturating_sub(self.origin).min(self.filled);
        }
    }

    fn take_capacity(&self) -> u64 {
        self.take
            .and_then(|t| self.layers.get(t.layer))
            .map(|l| l.buffer.len() as u64)
            .unwrap_or(0)
    }

    /// Hand back the last layer, so the caller can return its buffer to the control thread.
    ///
    /// Emptying a track resets its geometry: the next take defines a new loop.
    pub fn pop_layer(&mut self) -> Option<Vec<f32>> {
        let layer = self.layers.pop()?;
        if self.layers.is_empty() {
            self.origin = 0;
            self.loop_len = 0;
            self.filled = 0;
            self.playing = false;
        }
        Some(layer.buffer)
    }

    /// Remove one layer by index, handing its buffer back. Later layers move down by one.
    pub fn remove_layer(&mut self, index: usize) -> Option<Vec<f32>> {
        if index >= self.layers.len() {
            return None;
        }
        // Moves at most `MAX_LAYERS` `Vec` headers - a memmove of a few hundred bytes, no
        // allocation and no deallocation.
        let layer = self.layers.remove(index);
        if self.layers.is_empty() {
            self.origin = 0;
            self.loop_len = 0;
            self.filled = 0;
            self.playing = false;
        }
        Some(layer.buffer)
    }

    pub fn set_layer_muted(&mut self, index: usize, muted: bool) -> bool {
        match self.layers.get_mut(index) {
            Some(layer) => {
                layer.muted = muted;
                true
            }
            None => false,
        }
    }

    pub fn set_layer_gain(&mut self, index: usize, gain: f32) -> bool {
        match self.layers.get_mut(index) {
            Some(layer) => {
                layer.gain = gain.clamp(0.0, 4.0);
                true
            }
            None => false,
        }
    }

    #[cfg(test)]
    pub fn layer(&self, index: usize) -> Option<&Layer> {
        self.layers.get(index)
    }

    /// Cancel a running take without touching the layers that were finished before it.
    pub fn cancel_take(&mut self) {
        self.take = None;
    }

    /// Restore the monitoring state a fresh start would have. Called by "alles loeschen" after the
    /// layers have been returned.
    pub fn restore_defaults(&mut self) {
        self.monitor = self.monitor_default;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn track_with_layer(capacity: usize) -> Track {
        let mut t = Track::new(0, false);
        t.begin_loop_take(1_000, vec![0.0; capacity]);
        t
    }

    #[test]
    fn writes_are_sequential_and_readable() {
        let mut t = track_with_layer(100);
        for i in 0..40u64 {
            t.write(1_000 + i, i as f32);
        }
        t.finish_take(1_040);
        assert_eq!(t.loop_len(), 40);
        assert_eq!(t.read(1_000), 0.0);
        assert_eq!(t.read(1_039), 39.0);
        // Wrap-around: sample 40 of the loop is sample 0 again.
        assert_eq!(t.read(1_040), 0.0);
        assert_eq!(t.read(1_041), 1.0);
        assert_eq!(t.read(1_000 + 40 * 7 + 13), 13.0);
    }

    #[test]
    fn unwritten_positions_are_silent() {
        let mut t = track_with_layer(100);
        for i in 0..10u64 {
            t.write(1_000 + i, 1.0);
        }
        // Pretend the take was closed at +50 although only 10 samples arrived: the loop can only
        // be as long as what was actually written.
        t.finish_take(1_050);
        assert_eq!(t.loop_len(), 10);
        assert_eq!(t.read(1_009), 1.0);
        assert_eq!(t.read(1_010), 1.0); // wrapped, not stale memory
    }

    #[test]
    fn writes_beyond_capacity_are_dropped() {
        let mut t = track_with_layer(8);
        for i in 0..20u64 {
            t.write(1_000 + i, i as f32);
        }
        assert_eq!(t.filled(), 8);
    }

    #[test]
    fn clearing_the_last_layer_resets_the_geometry() {
        let mut t = track_with_layer(64);
        t.write(1_000, 0.5);
        t.finish_take(1_001);
        assert!(t.has_content());
        let buffer = t.pop_layer().expect("Puffer kommt zurueck");
        assert_eq!(buffer.len(), 64);
        assert!(!t.has_content());
        assert_eq!(t.loop_len(), 0);
        assert_eq!(t.read(1_000), 0.0);
        assert_eq!(t.layer_count(), 0);
    }

    /// Three layers, the later ones started in the middle of the loop: every one of them has to
    /// address the same musical instant with the same index.
    #[test]
    fn layers_share_one_grid_regardless_of_where_they_started() {
        let mut t = Track::new(0, false);
        t.begin_loop_take(1_000, vec![0.0; 64]);
        for i in 0..10u64 {
            t.write(1_000 + i, 1.0);
        }
        t.set_take_end(1_010);
        t.finish_take(1_010);
        assert_eq!(t.loop_len(), 10);

        // Second layer starts three loop passes later, in the middle of the loop.
        let start = 1_000 + 3 * 10 + 4;
        t.begin_overdub_take(start, vec![0.0; 64]);
        for i in 0..10u64 {
            t.write(start + i, 10.0);
        }
        t.finish_take(start + 10);

        // Every loop position now carries 1.0 + 10.0, wherever the overdub happened to begin.
        for i in 0..10u64 {
            assert_eq!(t.read(1_000 + i), 11.0, "Loop-Index {i}");
            assert_eq!(t.read(1_000 + 77 * 10 + i), 11.0, "Loop-Index {i}, spaeter");
        }
    }

    #[test]
    fn muting_and_gain_change_the_sum_only() {
        let mut t = Track::new(0, false);
        t.begin_loop_take(0, vec![0.0; 16]);
        for i in 0..4u64 {
            t.write(i, 1.0);
        }
        t.set_take_end(4);
        t.finish_take(4);
        t.begin_overdub_take(0, vec![0.0; 16]);
        for i in 0..4u64 {
            t.write(i, 2.0);
        }
        t.finish_take(4);

        assert_eq!(t.read(0), 3.0);
        assert!(t.set_layer_muted(1, true));
        assert_eq!(t.read(0), 1.0, "stummer Layer faellt aus der Summe");
        assert_eq!(t.muted_mask(), 0b10);
        assert!(t.set_layer_muted(1, false));
        assert!(t.set_layer_gain(1, 0.5));
        assert_eq!(t.read(0), 2.0);
        assert!(!t.set_layer_gain(7, 0.5), "Layer 7 gibt es nicht");
    }
}
