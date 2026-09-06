//! Commands with a musical timestamp, and the lock-free channels that carry them.
//!
//! The control thread may send a command arbitrarily early; the audio thread executes it at the
//! sample it is stamped for. That is what makes the engine independent of thread scheduling: no
//! amount of jitter between the threads can move a musical event, as long as the command arrives
//! before its time.
//!
//! Everything crossing the boundary is `Copy` and fixed size, so nothing is allocated, freed or
//! locked on the audio side. The only exception is the loop buffer itself, which is allocated in
//! the control thread and handed over as a whole `Vec` through [`BufferChannel`] - the audio
//! thread only ever swaps the pointer and returns the old buffer for the control thread to drop.

use rtrb::{Consumer, Producer, RingBuffer};

use super::timeline::TimeSignature;
use super::track::TrackState;

/// One scheduled engine action.
///
/// `at` is an absolute position on the engine's sample timeline - the same axis the audio callback
/// counts in. Commands without `at` take effect at the next block boundary; they carry no musical
/// meaning that would be worth a sample-accurate timestamp.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Command {
    /// Begin recording; `at` is the position of the first *musical* sample that lands in the loop.
    StartRecord { at: u64 },
    /// End recording; `at` is the position just after the last musical sample of the loop, so the
    /// loop length is exactly `at - start`.
    StopRecord { at: u64 },
    StartPlay { at: u64 },
    StopPlay { at: u64 },
    /// Throw the recorded loop away. The memory stays; only the content is invalidated.
    ClearTrack { at: u64 },
    /// Input monitoring (hearing yourself through the engine) on or off.
    SetMonitor { on: bool },
    /// Metronome on or off. The click grid keeps running either way.
    SetClick { on: bool },
    /// New tempo and time signature. Only accepted while the track is empty and idle, because it
    /// redefines what every sample position means.
    SetTempo {
        bpm: f64,
        signature: TimeSignature,
    },
    /// Stop everything (playback and recording) and mark the engine as finished.
    Stop,
}

impl Command {
    /// The position this command is scheduled for, if it has one.
    pub fn at(&self) -> Option<u64> {
        match self {
            Command::StartRecord { at }
            | Command::StopRecord { at }
            | Command::StartPlay { at }
            | Command::StopPlay { at }
            | Command::ClearTrack { at } => Some(*at),
            Command::SetMonitor { .. }
            | Command::SetClick { .. }
            | Command::SetTempo { .. }
            | Command::Stop => None,
        }
    }
}

/// Snapshot the audio thread pushes back to the control thread.
///
/// A single `Copy` struct instead of many message kinds: pushing it costs one memcpy of a few
/// dozen bytes, and the control thread simply keeps the newest one it finds.
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
    pub track: TrackState,
    /// Loop length in samples, 0 while nothing is recorded.
    pub loop_len: u64,
    /// Samples already written into the loop buffer during the running take.
    pub filled: u64,
    /// Peak of the input block that was consumed, absolute value.
    pub input_peak: f32,
    /// Peak of the produced output block, absolute value.
    pub output_peak: f32,
    pub monitor: bool,
    pub click: bool,
    pub bpm: f64,
    /// Commands the engine refused (currently only a tempo change while the track is busy).
    pub ignored_commands: u64,
    /// Set once a `Stop` command has been executed.
    pub stopped: bool,
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
    /// Look at the next command without removing it. Callback-safe.
    #[inline]
    pub fn peek(&self) -> Option<Command> {
        self.rx.peek().ok().copied()
    }

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

/// Hand-over of loop buffers between the threads.
///
/// `install` carries a freshly allocated buffer to the audio thread, `retire` carries the old one
/// back so it is dropped where dropping is allowed. Both directions use the same capacity, so a
/// buffer that was accepted can always be returned.
pub struct BufferChannel {
    pub install_tx: Producer<Vec<f32>>,
    pub retire_rx: Consumer<Vec<f32>>,
}

pub struct BufferEndpoint {
    pub install_rx: Consumer<Vec<f32>>,
    pub retire_tx: Producer<Vec<f32>>,
}

impl BufferChannel {
    /// Control-thread side: hand a new loop buffer over and drop whatever comes back.
    pub fn install(&mut self, buffer: Vec<f32>) -> Result<(), String> {
        self.drain_retired();
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
    /// Audio-thread side: take a new buffer if one is waiting and give the old one back.
    #[inline]
    pub fn take_new(&mut self) -> Option<Vec<f32>> {
        self.install_rx.pop().ok()
    }

    /// Send a retired buffer back to the control thread.
    ///
    /// If the return queue is unexpectedly full, the buffer is leaked rather than dropped: a
    /// deallocation inside the audio callback can block, a leaked buffer cannot. Both queues have
    /// the same capacity, so this cannot happen unless the control thread has stopped draining.
    #[inline]
    pub fn retire(&mut self, buffer: Vec<f32>) {
        if let Err(rtrb::PushError::Full(buffer)) = self.retire_tx.push(buffer) {
            std::mem::forget(buffer);
        }
    }
}

pub fn buffer_channel(capacity: usize) -> (BufferChannel, BufferEndpoint) {
    let (install_tx, install_rx) = RingBuffer::<Vec<f32>>::new(capacity);
    let (retire_tx, retire_rx) = RingBuffer::<Vec<f32>>::new(capacity);
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn timestamps_are_reported_per_variant() {
        assert_eq!(Command::StartRecord { at: 42 }.at(), Some(42));
        assert_eq!(Command::StopPlay { at: 7 }.at(), Some(7));
        assert_eq!(Command::SetMonitor { on: true }.at(), None);
        assert_eq!(Command::Stop.at(), None);
    }

    #[test]
    fn commands_arrive_in_order_and_can_be_peeked() {
        let (mut tx, mut rx) = command_channel(8);
        tx.send(Command::StartRecord { at: 10 }).unwrap();
        tx.send(Command::StopRecord { at: 20 }).unwrap();
        assert_eq!(rx.peek(), Some(Command::StartRecord { at: 10 }));
        assert_eq!(rx.peek(), Some(Command::StartRecord { at: 10 }));
        assert_eq!(rx.pop(), Some(Command::StartRecord { at: 10 }));
        assert_eq!(rx.pop(), Some(Command::StopRecord { at: 20 }));
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
        let (mut control, mut audio) = buffer_channel(2);
        control.install(vec![0.0; 16]).unwrap();
        let taken = audio.take_new().expect("Puffer kommt an");
        assert_eq!(taken.len(), 16);
        audio.retire(taken);
        control.drain_retired();
        assert!(audio.take_new().is_none());
    }
}
