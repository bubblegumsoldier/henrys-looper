//! Commands with a musical timestamp, the status snapshot coming back, and the buffer traffic
//! between the two threads.
//!
//! The control thread may send a command arbitrarily early; the audio thread executes it at the
//! sample it is stamped for. That is what makes the engine independent of thread scheduling: no
//! amount of jitter between the threads can move a musical event, as long as the command arrives
//! before its time.
//!
//! Everything crossing the boundary is `Copy` and fixed size, so nothing is allocated, freed or
//! locked on the audio side. The only exception is the layer buffers themselves, which are
//! allocated *and zeroed* in the control thread and handed over as whole `Vec`s through
//! [`BufferChannel`] / [`LayerPool`]:
//!
//! ```text
//!  Steuer-Thread                                        Audio-Thread
//!  LayerPool.reserve ──push──► install-Queue ──pop──► EngineCore.spares
//!        ▲                                                  │ take_spare()
//!        │                                                  ▼
//!        └────────pop──── retire-Queue ◄──push──── Layer eines Tracks
//! ```
//!
//! No `Vec` is ever created or dropped on the right-hand side of that picture.

use rtrb::{Consumer, Producer, PushError, RingBuffer};

use super::timeline::TimeSignature;
use super::track::TrackState;

/// Hard ceiling of tracks. Bounds the fixed-size status snapshot, which has to stay `Copy`.
pub const MAX_TRACKS: usize = 8;

/// One scheduled engine action.
///
/// `at` is an absolute position on the engine's sample timeline - the same axis the audio callback
/// counts in. Commands without `at` take effect at the next block boundary; they carry no musical
/// meaning that would be worth a sample-accurate timestamp.
///
/// `track` and `layer` are zero-based indices; user-facing numbering adds one.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Command {
    /// Begin a *new* loop on this track: existing layers are returned and the first layer defines
    /// origin and loop length. `at` is the position of the first musical sample of the loop.
    StartRecord { track: usize, at: u64 },
    /// Begin a *further* layer on the existing loop of this track. On an empty track this behaves
    /// exactly like `StartRecord`, so an overdub can never fail just because nothing is there yet.
    StartOverdub { track: usize, at: u64 },
    /// End the running take; `at` is the position just after its last musical sample, so a
    /// loop-defining take produces a loop of exactly `at - start` samples.
    StopRecord { track: usize, at: u64 },
    StartPlay { track: usize, at: u64 },
    StopPlay { track: usize, at: u64 },
    /// Throw this track's layers away; the buffers go back to the control thread.
    ClearTrack { track: usize, at: u64 },
    /// Reset every track to the state of a fresh start and return every buffer.
    ClearAll { at: u64 },
    /// Input monitoring (hearing yourself through the engine) for one track, independent of whether
    /// that track is playing.
    SetMonitor { track: usize, on: bool },
    SetLayerMute {
        track: usize,
        layer: usize,
        muted: bool,
    },
    SetLayerGain {
        track: usize,
        layer: usize,
        gain: f32,
    },
    /// Remove one layer; its buffer travels back to the control thread.
    RemoveLayer { track: usize, layer: usize },
    /// Metronome on or off. The click grid keeps running either way.
    SetClick { on: bool },
    /// New tempo, time signature and layer length. Only accepted while every track is empty and
    /// idle, because it redefines what every sample position means.
    SetTempo {
        bpm: f64,
        signature: TimeSignature,
        /// Length in samples the control thread now allocates layer buffers at.
        layer_capacity: u64,
    },
    /// Stop everything (playback and recording) and mark the engine as finished.
    Stop,
}

impl Command {
    /// The position this command is scheduled for, if it has one.
    pub fn at(&self) -> Option<u64> {
        match self {
            Command::StartRecord { at, .. }
            | Command::StartOverdub { at, .. }
            | Command::StopRecord { at, .. }
            | Command::StartPlay { at, .. }
            | Command::StopPlay { at, .. }
            | Command::ClearTrack { at, .. }
            | Command::ClearAll { at } => Some(*at),
            Command::SetMonitor { .. }
            | Command::SetLayerMute { .. }
            | Command::SetLayerGain { .. }
            | Command::RemoveLayer { .. }
            | Command::SetClick { .. }
            | Command::SetTempo { .. }
            | Command::Stop => None,
        }
    }

    /// The track this command addresses, if it addresses one.
    pub fn track(&self) -> Option<usize> {
        match self {
            Command::StartRecord { track, .. }
            | Command::StartOverdub { track, .. }
            | Command::StopRecord { track, .. }
            | Command::StartPlay { track, .. }
            | Command::StopPlay { track, .. }
            | Command::ClearTrack { track, .. }
            | Command::SetMonitor { track, .. }
            | Command::SetLayerMute { track, .. }
            | Command::SetLayerGain { track, .. }
            | Command::RemoveLayer { track, .. } => Some(*track),
            Command::ClearAll { .. }
            | Command::SetClick { .. }
            | Command::SetTempo { .. }
            | Command::Stop => None,
        }
    }
}

/// Why the engine refused the last command it refused. Travels in the status snapshot so the
/// control thread can print a German sentence instead of just a counter.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Refusal {
    #[default]
    None,
    /// Tempo change while something is recorded, recording or playing.
    Tempo,
    /// The layer ceiling of a track was reached.
    LayerLimit,
    /// No prepared buffer was available for a new layer.
    NoBuffer,
    /// The command named a track or layer that does not exist.
    NoSuchTarget,
    /// A layer operation while that track is recording.
    Busy,
}

impl Refusal {
    /// German explanation for the terminal, or `None` when nothing was refused.
    pub fn message(self) -> Option<&'static str> {
        match self {
            Refusal::None => None,
            Refusal::Tempo => Some(
                "Tempo abgelehnt: es ist noch etwas aufgenommen oder es laeuft etwas. Erst alles leeren (a).",
            ),
            Refusal::LayerLimit => {
                Some("Ebenen-Obergrenze je Track erreicht. Erst eine Ebene entfernen (w <nr>).")
            }
            Refusal::NoBuffer => Some(
                "Kein vorbereiteter Puffer frei - der Steuer-Thread kommt mit dem Nachlegen nicht nach.",
            ),
            Refusal::NoSuchTarget => Some("Das Kommando meinte einen Track oder eine Ebene, die es nicht gibt."),
            Refusal::Busy => Some("Ebenen lassen sich nicht bearbeiten, waehrend dieser Track aufnimmt."),
        }
    }
}

/// Per-track part of the status snapshot.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct TrackStatus {
    pub state: TrackState,
    pub layers: u8,
    /// Bit `i` set means layer `i` is muted.
    pub muted_mask: u32,
    /// Loop length in samples, 0 while nothing is recorded.
    pub loop_len: u64,
    /// Musical position loop index 0 sits at, 0 while nothing is recorded. Together with `loop_len`
    /// this is the track's own grid, which is what a further layer has to be quantised to.
    pub origin: u64,
    /// Samples already written during a running loop-defining take.
    pub filled: u64,
    /// Peak of this track's input channel, absolute value.
    pub input_peak: f32,
    /// Peak this track contributed to the output, absolute value. Its audible layers only -
    /// monitoring goes straight to the output and is not counted here.
    pub output_peak: f32,
    pub monitor: bool,
    pub playing: bool,
    /// Zero-based input channel of the device this track records.
    pub input_channel: u8,
}

/// Snapshot the audio thread pushes back to the control thread.
///
/// A single `Copy` struct instead of many message kinds: pushing it costs one memcpy of a few
/// hundred bytes, and the control thread simply keeps the newest one it finds.
#[derive(Clone, Copy, Debug, Default)]
pub struct Status {
    /// Absolute sample position of the engine (the output timeline).
    pub pos: u64,
    /// Zero-based bar at `pos`.
    pub bar: u64,
    /// Zero-based beat inside that bar.
    pub beat: u32,
    /// Samples elapsed inside the current beat.
    pub beat_offset: u64,
    /// Length of one beat, so the control thread can render a progress bar without a timeline.
    pub samples_per_beat: f64,
    pub tracks: [TrackStatus; MAX_TRACKS],
    pub track_count: u8,
    /// Peak of the produced output block, absolute value.
    pub output_peak: f32,
    pub click: bool,
    pub bpm: f64,
    /// Commands the engine refused, in total.
    pub ignored_commands: u64,
    /// Why the last refusal happened.
    pub refusal: Refusal,
    /// Prepared buffers the engine currently holds ready for the next layer.
    pub spares: u32,
    /// Buffers the engine has taken out of the install queue since it started. Together with what
    /// the control thread pushed, this says exactly how many are still in flight.
    pub buffers_taken: u64,
    /// Set once a `Stop` command has been executed.
    pub stopped: bool,
}

impl Status {
    /// The tracks that actually exist, without the unused tail of the fixed-size array.
    pub fn tracks(&self) -> &[TrackStatus] {
        &self.tracks[..(self.track_count as usize).min(MAX_TRACKS)]
    }
}

/// Control thread -> audio thread.
pub struct CommandSender {
    tx: Producer<Command>,
}

impl CommandSender {
    /// Fails only if the queue is full, which means the audio thread stopped consuming.
    pub fn send(&mut self, cmd: Command) -> Result<(), String> {
        self.tx
            .push(cmd)
            .map_err(|_| "Kommando-Queue ist voll - der Audio-Thread holt nichts mehr ab.".to_string())
    }
}

/// Audio thread side of the command queue.
pub struct CommandReceiver {
    rx: Consumer<Command>,
}

impl CommandReceiver {
    #[inline]
    pub fn pop(&mut self) -> Option<Command> {
        self.rx.pop().ok()
    }
}

pub fn command_channel(capacity: usize) -> (CommandSender, CommandReceiver) {
    let (tx, rx) = RingBuffer::<Command>::new(capacity);
    (CommandSender { tx }, CommandReceiver { rx })
}

/// Audio thread -> control thread.
pub struct StatusSender {
    tx: Producer<Status>,
}

impl StatusSender {
    /// Dropping the update when the queue is full is the right behaviour here: status is a
    /// snapshot, and the audio thread must never wait for the display.
    #[inline]
    pub fn push(&mut self, status: Status) {
        let _ = self.tx.push(status);
    }
}

pub struct StatusReceiver {
    rx: Consumer<Status>,
}

impl StatusReceiver {
    /// Newest status in the queue, discarding everything older.
    pub fn latest(&mut self) -> Option<Status> {
        let mut last = None;
        while let Ok(s) = self.rx.pop() {
            last = Some(s);
        }
        last
    }
}

pub fn status_channel(capacity: usize) -> (StatusSender, StatusReceiver) {
    let (tx, rx) = RingBuffer::<Status>::new(capacity);
    (StatusSender { tx }, StatusReceiver { rx })
}

/// Hand-over of layer buffers between the threads.
///
/// `install_tx` carries freshly allocated, zeroed buffers to the audio thread, `retire_rx` carries
/// used ones back so they are dropped or recycled where that is allowed.
pub struct BufferChannel {
    pub install_tx: Producer<Vec<f32>>,
    pub retire_rx: Consumer<Vec<f32>>,
}

pub struct BufferEndpoint {
    pub install_rx: Consumer<Vec<f32>>,
    pub retire_tx: Producer<Vec<f32>>,
}

impl BufferChannel {
    /// Control-thread side: hand one buffer over.
    pub fn install(&mut self, buffer: Vec<f32>) -> Result<(), String> {
        self.install_tx
            .push(buffer)
            .map_err(|_| "Puffer-Queue ist voll.".to_string())
    }

    pub fn drain_retired(&mut self) {
        // Dropping happens here, in the control thread, never in the callback.
        while self.retire_rx.pop().is_ok() {}
    }
}

impl BufferEndpoint {
    /// Audio-thread side: take a prepared buffer if one is waiting.
    #[inline]
    pub fn take_new(&mut self) -> Option<Vec<f32>> {
        self.install_rx.pop().ok()
    }

    /// Send a used buffer back to the control thread.
    ///
    /// If the return queue is unexpectedly full, the buffer is leaked rather than dropped: a
    /// deallocation inside the audio callback can block, a leaked buffer cannot. The return queue
    /// has room for every buffer that can ever be in the engine at once, so this cannot happen
    /// unless the control thread has stopped draining.
    #[inline]
    pub fn retire(&mut self, buffer: Vec<f32>) {
        if let Err(PushError::Full(buffer)) = self.retire_tx.push(buffer) {
            std::mem::forget(buffer);
        }
    }
}

pub fn buffer_channel(install_capacity: usize, retire_capacity: usize) -> (BufferChannel, BufferEndpoint) {
    let (install_tx, install_rx) = RingBuffer::<Vec<f32>>::new(install_capacity);
    let (retire_tx, retire_rx) = RingBuffer::<Vec<f32>>::new(retire_capacity);
    (
        BufferChannel {
            install_tx,
            retire_rx,
        },
        BufferEndpoint {
            install_rx,
            retire_tx,
        },
    )
}

/// The control thread's stock of empty layer buffers.
///
/// Its whole job is that an overdub never has to wait for `malloc`: the engine is kept supplied
/// with `slots` prepared buffers, and everything that comes back is zeroed here and reused. All
/// allocating, zeroing and dropping happens in [`LayerPool::service`], which only ever runs in the
/// control thread.
pub struct LayerPool {
    channel: BufferChannel,
    layer_len: usize,
    slots: usize,
    reserve: Vec<Vec<f32>>,
    /// Buffers pushed into the install queue since the start.
    pushed: u64,
    /// Buffers taken back out of the retire queue since the start.
    reclaimed: u64,
}

impl LayerPool {
    pub fn new(channel: BufferChannel, layer_len: usize, slots: usize) -> Self {
        Self {
            channel,
            layer_len,
            slots,
            reserve: Vec::with_capacity(slots),
            pushed: 0,
            reclaimed: 0,
        }
    }

    /// Reclaim what came back, then top the engine up to `slots` prepared buffers.
    ///
    /// `status` is the newest snapshot, or `None` before the first one arrives. `spares` and
    /// `buffers_taken` in it are what makes the accounting exact: buffers still sitting in the
    /// install queue are `pushed - buffers_taken`, and the engine holds `spares` more. Layers in
    /// use are deliberately *not* counted, so using a layer immediately triggers a refill.
    pub fn service(&mut self, status: Option<&Status>) {
        while let Ok(mut buffer) = self.channel.retire_rx.pop() {
            self.reclaimed += 1;
            if buffer.len() == self.layer_len && self.reserve.len() < self.slots {
                // Zeroing belongs here: a layer buffer must arrive empty, and a memset of several
                // megabytes has no business in an audio callback.
                buffer.fill(0.0);
                self.reserve.push(buffer);
            }
            // Everything else is simply dropped - in the control thread, where that is allowed.
        }

        let taken = status.map(|s| s.buffers_taken).unwrap_or(0);
        let queued = self.pushed.saturating_sub(taken) as usize;
        let mut available = status.map(|s| s.spares as usize).unwrap_or(0) + queued;
        while available < self.slots {
            let buffer = self
                .reserve
                .pop()
                .unwrap_or_else(|| vec![0.0f32; self.layer_len]);
            match self.channel.install_tx.push(buffer) {
                Ok(()) => {
                    self.pushed += 1;
                    available += 1;
                }
                Err(PushError::Full(buffer)) => {
                    self.reserve.push(buffer);
                    break;
                }
            }
        }
    }

    /// New layer length after a tempo change: the stock is worthless at the wrong length.
    pub fn set_layer_len(&mut self, layer_len: usize) {
        self.layer_len = layer_len;
        self.reserve.clear();
    }

    /// Buffers that came back from the audio thread. Used by the tests to prove nothing leaks.
    #[cfg(test)]
    pub fn reclaimed(&self) -> u64 {
        self.reclaimed
    }

    #[cfg(test)]
    pub fn pushed(&self) -> u64 {
        self.pushed
    }

    /// Give everything back at the end of a session.
    pub fn drain(&mut self) {
        self.channel.drain_retired();
        self.reserve.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn timestamps_are_reported_per_variant() {
        assert_eq!(Command::StartRecord { track: 0, at: 42 }.at(), Some(42));
        assert_eq!(Command::StopPlay { track: 1, at: 7 }.at(), Some(7));
        assert_eq!(Command::ClearAll { at: 9 }.at(), Some(9));
        assert_eq!(
            Command::SetMonitor {
                track: 0,
                on: true
            }
            .at(),
            None
        );
        assert_eq!(Command::Stop.at(), None);
        assert_eq!(Command::StartOverdub { track: 3, at: 1 }.track(), Some(3));
        assert_eq!(Command::ClearAll { at: 1 }.track(), None);
    }

    #[test]
    fn commands_arrive_in_the_order_they_were_sent() {
        let (mut tx, mut rx) = command_channel(8);
        tx.send(Command::StartRecord { track: 0, at: 10 }).unwrap();
        tx.send(Command::StopRecord { track: 0, at: 20 }).unwrap();
        assert_eq!(rx.pop(), Some(Command::StartRecord { track: 0, at: 10 }));
        assert_eq!(rx.pop(), Some(Command::StopRecord { track: 0, at: 20 }));
        assert_eq!(rx.pop(), None);
    }

    #[test]
    fn status_receiver_keeps_only_the_newest() {
        let (mut tx, mut rx) = status_channel(8);
        for pos in 0..5u64 {
            tx.push(Status {
                pos,
                ..Default::default()
            });
        }
        assert_eq!(rx.latest().map(|s| s.pos), Some(4));
        assert!(rx.latest().is_none());
    }

    #[test]
    fn buffers_travel_both_ways() {
        let (mut control, mut audio) = buffer_channel(2, 2);
        control.install(vec![0.0; 16]).unwrap();
        let taken = audio.take_new().expect("Puffer kommt an");
        assert_eq!(taken.len(), 16);
        audio.retire(taken);
        control.drain_retired();
        assert!(audio.take_new().is_none());
    }

    /// The pool keeps exactly `slots` buffers in flight, recycles what comes back, and zeroes it
    /// on the way - the three properties an overdub depends on.
    #[test]
    fn the_pool_keeps_the_engine_supplied_and_recycles_what_comes_back() {
        let (control, mut audio) = buffer_channel(8, 8);
        let mut pool = LayerPool::new(control, 32, 3);
        pool.service(None);
        assert_eq!(pool.pushed(), 3, "Vorrat wird sofort angelegt");

        // The engine takes all three and turns them into layers.
        let mut taken = Vec::new();
        while let Some(b) = audio.take_new() {
            taken.push(b);
        }
        assert_eq!(taken.len(), 3);
        let status = Status {
            spares: 0,
            buffers_taken: 3,
            ..Default::default()
        };

        // Two of them come back dirty, as a removed layer does.
        for _ in 0..2 {
            let mut used = taken.pop().expect("Puffer");
            used.fill(0.7);
            audio.retire(used);
        }
        pool.service(Some(&status));
        assert_eq!(pool.reclaimed(), 2);
        assert_eq!(pool.pushed(), 6, "der Vorrat wird wieder auf drei gebracht");

        // The reserve is used before anything new is allocated, so these two are the recycled ones.
        let a = audio.take_new().expect("Puffer");
        let b = audio.take_new().expect("Puffer");
        assert!(
            a.iter().chain(b.iter()).all(|&s| s == 0.0),
            "wiederverwendete Puffer muessen leer beim Audio-Thread ankommen"
        );
    }
}
