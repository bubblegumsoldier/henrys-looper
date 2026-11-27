//! From a user action to timed engine commands - the part that needs no audio device.
//!
//! This is the one place where a key press or a button becomes a set of commands with sample
//! positions on them. Both front ends use it: the CLI's `live` subcommand and the desktop app's
//! host thread. It used to exist twice, once on each side, which meant every rule had to be
//! changed twice and only one copy was under test.
//!
//! Three ideas carry the whole file:
//!
//! * **Everything musical happens on a grid point.** A command is stamped with an absolute sample
//!   position and the audio thread executes it exactly there, so it does not matter how long the
//!   command takes to get there - as long as it arrives before its own time.
//! * **`guard` is what makes that true.** Every scheduled position is at least `guard` samples
//!   ahead of where the engine is estimated to be, which is comfortably more than one round
//!   through the callbacks.
//! * **Which grid is [`Quantize`].** The bar grid is fine for corrections; for starting a take it
//!   is the wrong unit, and the reason is in the next section.
//!
//! # Why the loop grid is the default
//!
//! With bar quantisation and an eight-bar loop, a recording pressed in bar 3 starts in bar 4 and
//! then runs eight bars from *there* - across the loop the rest of the session is counting in. To
//! get a take that lines up, the musician has to wait until bar 8 is on the display, hit the key
//! inside that last bar, and have the instrument back in playing position before the downbeat. That
//! is the complaint this module answers.
//!
//! With [`Quantize::Loop`] the same key press in bar 3 arms the take for the start of bar 9: press
//! early, then take six bars to get ready. The take is on the loop grid either way, so it can never
//! straddle a loop boundary.
//!
//! # Which grid a take snaps to
//!
//! There are two rasters, and the choice between them is not a preference but a consequence of how
//! a track stores its layers (`track.rs`): every layer is addressed as
//! `(position - origin) mod loop_len`.
//!
//! * **Track has a loop** (`loop_len > 0`): its own grid, `origin + n * loop_len`, is the only
//!   correct answer for a further layer. The global bar grid would generally miss it - bar
//!   boundaries are rounded individually, so `bar_start(origin_bar + n * bars)` and
//!   `origin + n * loop_len` can differ by a sample or two, and at an odd time signature they
//!   differ more often than not. A layer starting one sample off would still *mix* correctly (the
//!   modulo takes care of that), but the musician would be playing against a loop whose top is not
//!   where he was told it is, and every following overdub would inherit the offset.
//! * **Track is empty**: there is no `origin` yet, so the raster is counted from the start of the
//!   engine - bar 0, bar `bars`, bar `2*bars`, the same grid the click has been marking out since
//!   the session began. That makes the first take of every track land on the same boundaries, which
//!   is what lets two tracks recorded minutes apart play as one loop.
//!
//! A *new* loop (`r`) always uses the global grid, even on a track that already holds one: it
//! throws the old geometry away and defines a new `origin`, and the global grid is what the click
//! and every other track are following.
//!
//! # What stays on the bar grid, and why
//!
//! Stop and playback keep bar quantisation in both modes. They are corrections, not takes: "stop
//! this" that waits up to eight bars reads as a broken button, and a musician reaching for it wants
//! it to act. Playback in particular loses nothing by it - a loop is read as
//! `(position - origin) mod loop_len`, so switching it on mid-loop resumes at the phase the loop is
//! in rather than at a random offset. Only a take, which has to *fill* a loop from its top, needs
//! the loop grid.
//!
//! # Stop at the end of the bar, and stop now
//!
//! A stop pressed by hand waits for the end of the bar that is playing ([`StopAt::BarEnd`]): a
//! loop cut mid-phrase sounds like a mistake, one that ends on the bar line sounds like an ending.
//! The cut mid-bar is still there, as an explicit second way ([`StopAt::Now`]). A stop that waits
//! is a [`PendingKind::Stop`] like a take's countdown, so the display counts down to it; pressing
//! it again, or pressing play, record, overdub or clear on that track, takes it back with
//! [`Command::CancelStop`] (see [`Scheduler::stop`] and [`Scheduler::supersede_stop`]). The score
//! never goes through here - its stops are its own and stay where the runner puts them.

use super::command::{Command, MAX_TRACKS};
use super::timeline::Timeline;
use super::track::TrackState;

/// Which grid a take is quantised to.
/// The score format spells these `quantize: bar` / `quantize: loop`, so the serde names are the
/// same words the musician types; the compiled score reuses this very type rather than a copy.
#[derive(
    Clone, Copy, Debug, Default, PartialEq, Eq, clap::ValueEnum, serde::Serialize, serde::Deserialize,
)]
#[serde(rename_all = "lowercase")]
pub enum Quantize {
    /// Next bar boundary. What the looper did before there was a choice.
    Bar,
    /// Next loop boundary, so one early press is enough. The default, see the module comment.
    #[default]
    Loop,
}

impl Quantize {
    /// German word for the display.
    pub fn label(self) -> &'static str {
        match self {
            Quantize::Bar => "Bar",
            Quantize::Loop => "Loop",
        }
    }

    /// One German sentence explaining what this mode does to a key press.
    pub fn explanation(self) -> &'static str {
        match self {
            Quantize::Bar => "Recording starts at the next bar line.",
            Quantize::Loop => "Recording starts at the top of the next loop.",
        }
    }
}

/// What a track is waiting for. Used for the count-in display, not for the engine.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PendingKind {
    Record,
    Overdub,
    Play,
    Stop,
}

impl PendingKind {
    /// German word for the display, in the same spirit as [`TrackState::label`].
    pub fn label(self) -> &'static str {
        match self {
            PendingKind::Record => "Record",
            PendingKind::Overdub => "Overdub",
            PendingKind::Play => "Play",
            PendingKind::Stop => "Stop",
        }
    }
}

/// One scheduled action of a track, remembered so the display can count down to it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Pending {
    kind: PendingKind,
    /// Absolute sample position the action takes effect at.
    at: u64,
    /// A stop pressed by hand, which a second press (or a newer action) can take back: `Some(false)`
    /// for a playback stop (`StopPlay`), `Some(true)` for the early end of a running take
    /// (`StopRecord` + `StartPlay`). `None` for everything else - in particular for the score's own
    /// stops, which nothing in free looping may take back.
    stop: Option<bool>,
}

/// When a stop pressed by hand takes effect. See [`Scheduler::stop`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StopAt {
    /// At the end of the bar that is playing - the default of the stop button, the `S` key and
    /// the `track.<n>.stop` MIDI target.
    BarEnd,
    /// Now (one head start from now), cutting the loop mid-bar: the small red half of the button,
    /// `Shift+S`, `track.<n>.stop_now`.
    Now,
}

/// How far a track still is from its scheduled action.
///
/// `bars` and `beats` are what is left of the way there, split at the bar line: five bars and two
/// beats means the action happens two beats into the sixth bar from now. Both are zero and `kind`
/// is `None` when nothing is scheduled.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Lead {
    pub kind: Option<PendingKind>,
    pub bars: u32,
    pub beats: u32,
}

impl Lead {
    /// Whether anything is scheduled at all.
    pub fn is_pending(&self) -> bool {
        self.kind.is_some()
    }

    /// German text for a terminal or a button: `Aufnahme in 5 Takten`, `Overdub in 2 Schlaegen`.
    pub fn text(&self) -> Option<String> {
        let kind = self.kind?;
        Some(match (self.bars, self.beats) {
            (0, 0) => format!("{} now", kind.label()),
            (0, 1) => format!("{} in 1 beat", kind.label()),
            (0, b) => format!("{} in {b} beats", kind.label()),
            (1, 0) => format!("{} in 1 bar", kind.label()),
            (t, 0) => format!("{} in {t} bars", kind.label()),
            (1, 1) => format!("{} in 1 bar and 1 beat", kind.label()),
            (1, b) => format!("{} in 1 bar and {b} beats", kind.label()),
            (t, 1) => format!("{} in {t} bars and 1 beat", kind.label()),
            (t, b) => format!("{} in {t} bars and {b} beats", kind.label()),
        })
    }
}

/// Commands to send, plus the German sentence describing what was scheduled.
#[derive(Debug, Clone, PartialEq)]
pub struct Scheduled {
    pub commands: Vec<Command>,
    pub message: String,
}

/// Turns actions into commands for one engine configuration.
#[derive(Debug, Clone, Copy)]
pub struct Scheduler {
    pub timeline: Timeline,
    /// Configured loop length in bars.
    pub bars: u32,
    /// Head start every scheduled command gets, so it can never arrive after its own time.
    pub guard: u64,
    /// Which grid a take snaps to.
    pub quantize: Quantize,
    /// What each track is waiting for, so the display can count down. Entries expire on their own
    /// once the engine has passed them, so nothing has to acknowledge them.
    pending: [Option<Pending>; MAX_TRACKS],
}

impl Scheduler {
    /// `guard` is four buffers plus 20 ms, the same value the CLI always used: more than one round
    /// through both callbacks, so a command is always in the waiting room before it is due.
    pub fn new(timeline: Timeline, bars: u32, buffer_frames: u32, quantize: Quantize) -> Self {
        Self {
            timeline,
            bars,
            guard: (buffer_frames * 4 + timeline.sample_rate() / 50) as u64,
            quantize,
            pending: [None; MAX_TRACKS],
        }
    }

    /// Where the audio thread is now, as well as the control side can know: the position of the
    /// newest status snapshot plus the time that has passed since it arrived.
    ///
    /// Both streams run on the interface clock, so this is good to a few samples - and the rest is
    /// absorbed by `guard` and by quantising to a grid point.
    pub fn estimated_pos(&self, last: Option<(u64, std::time::Duration)>) -> u64 {
        match last {
            Some((pos, since)) => {
                pos + (since.as_secs_f64() * self.timeline.sample_rate() as f64) as u64
            }
            None => 0,
        }
    }

    /// Next bar boundary far enough ahead for the audio thread to still see the command.
    pub fn next_bar(&self, est: u64) -> u64 {
        self.timeline.bar_start_at_or_after(est + self.guard)
    }

    /// Position for a command that has no musical meaning and should act as soon as possible.
    pub fn soon(&self, est: u64) -> u64 {
        est + self.guard
    }

    /// Where a take may begin: the next point of the configured grid that the audio thread can
    /// still be told about in time.
    ///
    /// `loop_grid` is the track's own `(origin, loop_len)` when it has one. See the module comment
    /// for why that beats the global grid whenever it exists.
    pub fn next_start(&self, est: u64, loop_grid: Option<(u64, u64)>) -> u64 {
        let earliest = est + self.guard;
        match self.quantize {
            Quantize::Bar => self.timeline.bar_start_at_or_after(earliest),
            Quantize::Loop => match loop_grid {
                Some((origin, loop_len)) if loop_len > 0 => {
                    Scheduler::loop_grid_at_or_after(origin, loop_len, earliest)
                }
                _ => self.timeline.loop_start_at_or_after(earliest, self.bars),
            },
        }
    }

    /// The point of a track's own loop grid `origin + n * loop_len` at or after `pos`.
    ///
    /// Public and free-standing because the score runner needs the very same grid: a section
    /// boundary is a *bar* line, and a bar line can sit a sample or two beside `origin + n *
    /// loop_len` because every bar boundary is rounded on its own. A further layer has to land on
    /// the loop's grid and not on the bar's - see the module comment.
    pub fn loop_grid_at_or_after(origin: u64, loop_len: u64, pos: u64) -> u64 {
        if loop_len == 0 || pos <= origin {
            return origin;
        }
        // Whole passes since the origin, rounded up: the next top of *this* loop.
        origin + (pos - origin).div_ceil(loop_len) * loop_len
    }

    /// Remember what a track is waiting for, so [`Scheduler::lead`] can count down to it.
    fn arm(&mut self, track: usize, kind: PendingKind, at: u64) {
        if let Some(slot) = self.pending.get_mut(track) {
            *slot = Some(Pending {
                kind,
                at,
                stop: None,
            });
        }
    }

    /// Forget every countdown. What a transport stop needs: the engine drops everything that was
    /// armed, and a display still counting down to it would be counting down to nothing.
    pub fn disarm_all(&mut self) {
        self.pending = [None; MAX_TRACKS];
    }

    /// Forget the countdown of one track - its take was undone.
    pub fn disarm_track(&mut self, track: usize) {
        self.disarm(track);
    }

    fn disarm(&mut self, track: usize) {
        if let Some(slot) = self.pending.get_mut(track) {
            *slot = None;
        }
    }

    /// How far `to` still is from `from`, in whole beats split at the bar line.
    ///
    /// The distance is counted between the beat `from` is in and the beat `to` falls on. That is
    /// the same arithmetic the timeline uses for the display, so a countdown built on it reaches
    /// zero on exactly the beat the engine acts on. Public because the score runner counts down to
    /// a *section* boundary rather than to a track's own action, and both countdowns have to mean
    /// the same thing on screen.
    pub fn distance(&self, from: u64, to: u64) -> (u32, u32) {
        let beats_per_bar = self.timeline.beats_per_bar().max(1);
        let left = self
            .timeline
            .beat_index_at(to)
            .saturating_sub(self.timeline.beat_index_at(from));
        ((left / beats_per_bar) as u32, (left % beats_per_bar) as u32)
    }

    /// How far track `track` still is from its scheduled action, measured from `pos`.
    pub fn lead(&self, track: usize, pos: u64) -> Lead {
        let Some(Some(pending)) = self.pending.get(track).copied() else {
            return Lead::default();
        };
        if pos >= pending.at {
            return Lead::default();
        }
        let (bars, beats) = self.distance(pos, pending.at);
        Lead {
            kind: Some(pending.kind),
            bars,
            beats,
        }
    }

    /// The three commands every take is made of, plus the pending entry the display counts down to.
    ///
    /// `end` is the position just after the last musical sample, so a loop-defining take produces a
    /// loop of exactly `end - start` frames. The trailing [`Command::StartPlay`] is what makes the
    /// switch from recording to playing seamless at that very sample.
    fn take_commands(&mut self, track: usize, start: u64, end: u64, new_loop: bool) -> Vec<Command> {
        let kind = if new_loop {
            PendingKind::Record
        } else {
            PendingKind::Overdub
        };
        self.arm(track, kind, start);
        let head = if new_loop {
            Command::StartRecord { track, at: start }
        } else {
            Command::StartOverdub { track, at: start }
        };
        vec![
            head,
            Command::StopRecord { track, at: end },
            Command::StartPlay { track, at: end },
        ]
    }

    /// A new loop beginning at a position the caller already knows, over `bars` bars.
    ///
    /// This is the entry point the score runner uses: a section starts on a boundary the runner
    /// computed itself, so there is nothing left to quantise - running it through
    /// [`Scheduler::next_start`] a second time could only move it. [`Scheduler::record`] is this
    /// method with the position worked out from a key press.
    pub fn record_at(&mut self, track: usize, name: &str, start: u64, bars: u32) -> Scheduled {
        let bar = self.timeline.bar_index_at(start);
        let end = self.timeline.position_of(bar + bars as u64, 0);
        Scheduled {
            commands: self.take_commands(track, start, end, true),
            message: format!("\"{name}\": Record armed - a new loop over {bars} bars."),
        }
    }

    /// A further layer between two positions the caller already knows.
    pub fn overdub_at(
        &mut self,
        track: usize,
        name: &str,
        start: u64,
        end: u64,
        layers: u8,
    ) -> Scheduled {
        Scheduled {
            commands: self.take_commands(track, start, end, false),
            message: format!("\"{name}\": Overdub armed - layer {}.", layers as usize + 1),
        }
    }

    /// Playback from a position the caller already knows.
    pub fn play_at(&mut self, track: usize, name: &str, at: u64) -> Scheduled {
        self.arm(track, PendingKind::Play, at);
        Scheduled {
            commands: vec![Command::StartPlay { track, at }],
            message: format!("\"{name}\": Play armed."),
        }
    }

    /// Silence from a position the caller already knows. Only the playback is switched off; what is
    /// recorded stays where it is.
    pub fn stop_play_at(&mut self, track: usize, name: &str, at: u64) -> Scheduled {
        self.arm(track, PendingKind::Stop, at);
        Scheduled {
            commands: vec![Command::StopPlay { track, at }],
            message: format!("\"{name}\": Stop armed."),
        }
    }

    /// A new loop: replaces whatever the track holds, over the configured number of bars.
    ///
    /// Always the global grid - the take defines a new `origin`, so there is no old geometry worth
    /// aligning to.
    pub fn record(&mut self, track: usize, name: &str, est: u64) -> Scheduled {
        let start = self.next_start(est, None);
        self.record_at(track, name, start, self.bars)
    }

    /// A further layer. It covers exactly one pass of the existing loop and starts on that loop's
    /// own grid; on an empty track it is the first take and gets the configured number of bars, so
    /// an overdub can never fail just because nothing is there yet.
    pub fn overdub(
        &mut self,
        track: usize,
        name: &str,
        est: u64,
        origin: u64,
        loop_len: u64,
        layers: u8,
    ) -> Scheduled {
        let start = self.next_start(est, Some((origin, loop_len)));
        let end = if loop_len > 0 {
            start + loop_len
        } else {
            let bar = self.timeline.bar_index_at(start);
            self.timeline.position_of(bar + self.bars as u64, 0)
        };
        self.overdub_at(track, name, start, end, layers)
    }

    /// The stop button of free looping, read against what the track is doing and against `when`
    /// ([`StopAt`]). `clock_runs` is whether there is a bar to wait for at all: with the transport
    /// standing (or a count-in running) every loop is silent anyway, and a stop that waited for a
    /// bar line nobody hears would only look broken - it acts at once.
    ///
    /// * **Armed** (a take waits for its start): the take is cancelled, as it always was.
    /// * **Recording / overdubbing**: the take ends on the next bar line and the loop plays on -
    ///   unchanged, and the same for both halves of the button: a take that ended mid-bar would
    ///   define a loop that is not a whole number of bars. Pressed again while that early end
    ///   waits, `BarEnd` takes it back (the take runs on to its own end).
    /// * **Anything else**, `Now`: silent from one head start on, as the button always did. A stop
    ///   or play still waiting for its bar line is taken back with it, so nothing wakes the track
    ///   up again a bar later.
    /// * **Anything else**, `BarEnd`: a playing loop stops on the next bar line (the fade sits
    ///   before it, as with every timed stop). Pressed again while that waits, it is taken back. A
    ///   track that is not playing has nothing to wait for: a play armed for a bar line is
    ///   cancelled there, otherwise it is silent at once.
    ///
    /// Bar grid, not loop grid, in both quantise modes - see the module comment.
    pub fn stop(
        &mut self,
        track: usize,
        name: &str,
        est: u64,
        state: TrackState,
        when: StopAt,
        clock_runs: bool,
    ) -> Scheduled {
        let waiting = self.waiting_stop(track, est);
        match state {
            TrackState::Armed => {
                self.disarm(track);
                Scheduled {
                    commands: vec![Command::ClearTrack {
                        track,
                        at: self.soon(est),
                    }],
                    message: "Scheduled recording cancelled.".to_string(),
                }
            }
            TrackState::Recording | TrackState::Overdub => match (waiting, when) {
                (Some((at, true)), StopAt::BarEnd) => self.take_back(track, est, at, true, name),
                (Some((_, true)), StopAt::Now) => Scheduled {
                    commands: Vec::new(),
                    message: "Recording already ends at the next bar line - a take is never cut \
                              mid-bar."
                        .to_string(),
                },
                _ => {
                    let end = self.next_bar(est);
                    self.arm_stop(track, end, true);
                    Scheduled {
                        commands: vec![
                            Command::StopRecord { track, at: end },
                            Command::StartPlay { track, at: end },
                        ],
                        message: "Recording ends at the next bar line.".to_string(),
                    }
                }
            },
            _ if when == StopAt::Now || !clock_runs => {
                let mut commands = self.cancel_commands(track, est);
                commands.push(Command::StopPlay {
                    track,
                    at: self.soon(est),
                });
                self.disarm(track);
                Scheduled {
                    commands,
                    message: if clock_runs || when == StopAt::Now {
                        format!("\"{name}\": stopped.")
                    } else {
                        format!("\"{name}\": stopped - the clock stands, so at once.")
                    },
                }
            }
            TrackState::Playing => match waiting {
                Some((at, false)) => self.take_back(track, est, at, false, name),
                _ => {
                    let at = self.next_bar(est);
                    self.arm_stop(track, at, false);
                    Scheduled {
                        commands: vec![Command::StopPlay { track, at }],
                        message: format!("\"{name}\": stops at the end of the bar."),
                    }
                }
            },
            _ => {
                let commands = self.cancel_commands(track, est);
                self.disarm(track);
                let cancelled = !commands.is_empty();
                Scheduled {
                    commands: if cancelled {
                        commands
                    } else {
                        vec![Command::StopPlay {
                            track,
                            at: self.soon(est),
                        }]
                    },
                    message: if cancelled {
                        format!("\"{name}\": play cancelled.")
                    } else {
                        format!("\"{name}\": stopped.")
                    },
                }
            }
        }
    }

    /// What a newer transport action on this track (play, record, overdub, clear) sends first: a
    /// playback stop the musician armed by hand is taken back, because the newer press is what he
    /// wants now - the same rule as a second press on Stop. Empty when nothing of that kind waits.
    ///
    /// The early end of a running take is left alone: what a newer action does to a running take
    /// was decided long before this button and is not changed by it.
    pub fn supersede_stop(&mut self, track: usize, est: u64) -> Vec<Command> {
        match self.waiting_stop(track, est) {
            Some((at, false)) => {
                self.disarm(track);
                if self.in_time(est, at) {
                    vec![Command::CancelStop {
                        track,
                        at,
                        take: false,
                    }]
                } else {
                    Vec::new()
                }
            }
            _ => Vec::new(),
        }
    }

    /// The stop by hand this track is waiting for, as `(at, take)`, if one is still ahead of `est`.
    pub fn waiting_stop(&self, track: usize, est: u64) -> Option<(u64, bool)> {
        match self.pending.get(track).copied().flatten() {
            Some(Pending {
                kind: PendingKind::Stop,
                at,
                stop: Some(take),
            }) if at > est => Some((at, take)),
            _ => None,
        }
    }

    /// Whether a cancel sent now still reaches a command due at `at` before it acts. The cancel is
    /// untimed and arrives within one head start; the stop acts up to one fade (10 ms) before its
    /// position. A second head start covers the fade with room to spare.
    fn in_time(&self, est: u64, at: u64) -> bool {
        at > est + 2 * self.guard
    }

    /// The second press on a waiting stop: take it back if there is still time, otherwise say that
    /// it happens anyway.
    fn take_back(&mut self, track: usize, est: u64, at: u64, take: bool, name: &str) -> Scheduled {
        if !self.in_time(est, at) {
            return Scheduled {
                commands: Vec::new(),
                message: format!("\"{name}\": too late to cancel - it stops on this bar line."),
            };
        }
        self.disarm(track);
        Scheduled {
            commands: vec![Command::CancelStop { track, at, take }],
            message: if take {
                format!("\"{name}\": stop cancelled - the take runs on to its end.")
            } else {
                format!("\"{name}\": stop cancelled - keeps playing.")
            },
        }
    }

    /// For a stop that acts at once: take back whatever this track still waits for on a bar line
    /// that would make it sound again - a stop by hand (a `StopPlay` that would then be left over
    /// for a later play to run into) or a play. A play is cancelled with a `StopPlay` on its own
    /// sample: sent later, it wins over the `StartPlay` there.
    fn cancel_commands(&self, track: usize, est: u64) -> Vec<Command> {
        match self.pending.get(track).copied().flatten() {
            Some(Pending {
                kind: PendingKind::Stop,
                at,
                stop: Some(false),
            }) if self.in_time(est, at) => vec![Command::CancelStop {
                track,
                at,
                take: false,
            }],
            Some(Pending {
                kind: PendingKind::Play,
                at,
                ..
            }) if at > self.soon(est) => vec![Command::StopPlay { track, at }],
            _ => Vec::new(),
        }
    }

    fn arm_stop(&mut self, track: usize, at: u64, take: bool) {
        if let Some(slot) = self.pending.get_mut(track) {
            *slot = Some(Pending {
                kind: PendingKind::Stop,
                at,
                stop: Some(take),
            });
        }
    }

    pub fn play(&mut self, track: usize, name: &str, est: u64) -> Scheduled {
        let at = self.next_bar(est);
        self.play_at(track, name, at)
    }

    pub fn clear_track(&mut self, track: usize, name: &str, est: u64) -> Scheduled {
        self.disarm(track);
        Scheduled {
            commands: vec![Command::ClearTrack {
                track,
                at: self.soon(est),
            }],
            message: format!("\"{name}\" cleared."),
        }
    }

    pub fn clear_all(&mut self, est: u64) -> Scheduled {
        self.pending = [None; MAX_TRACKS];
        Scheduled {
            commands: vec![Command::ClearAll { at: self.soon(est) }],
            message: "All cleared - back to a fresh start.".to_string(),
        }
    }

    /// Switch the grid while the engine runs. Only takes scheduled from now on are affected; one
    /// that is already armed keeps the position it was given, because the engine already has it.
    pub fn set_quantize(&mut self, quantize: Quantize) -> String {
        self.quantize = quantize;
        format!(
            "Quantize: {} - {}",
            quantize.label(),
            quantize.explanation()
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::timeline::TimeSignature;

    const RATE: u32 = 48_000;
    /// 120 BPM, 4/4, 48 kHz: 24000 samples per beat, 96000 per bar - all integers, so every
    /// expectation below can be written down by hand.
    const BAR: u64 = 96_000;

    fn scheduler(bpm: f64, bars: u32, quantize: Quantize) -> Scheduler {
        Scheduler::new(
            Timeline::new(RATE, bpm, TimeSignature::new(4, 4)),
            bars,
            128,
            quantize,
        )
    }

    fn start_of(plan: &Scheduled) -> u64 {
        plan.commands[0].at().expect("das erste Kommando ist getimt")
    }

    #[test]
    fn the_head_start_is_four_buffers_plus_twenty_milliseconds() {
        assert_eq!(scheduler(120.0, 8, Quantize::Loop).guard, 128 * 4 + 48_000 / 50);
    }

    #[test]
    fn the_loop_grid_is_the_default() {
        assert_eq!(Quantize::default(), Quantize::Loop);
    }

    /// The old behaviour, unchanged: everything musical lands on the next bar boundary that is
    /// still far enough ahead for the head start to fit.
    #[test]
    fn with_bar_quantisation_a_recording_starts_on_the_next_bar_as_before() {
        let mut s = scheduler(120.0, 8, Quantize::Bar);
        let plan = s.record(0, "gitarre", 100_000);
        assert_eq!(
            plan.commands,
            vec![
                Command::StartRecord { track: 0, at: 2 * BAR },
                Command::StopRecord { track: 0, at: 2 * BAR + 8 * BAR },
                Command::StartPlay { track: 0, at: 2 * BAR + 8 * BAR },
            ]
        );
        assert!(plan.message.contains("8 bars"), "{}", plan.message);

        // Exactly on a boundary the *next* one is taken, because the head start has to fit.
        let plan = s.record(1, "stimme", BAR);
        assert_eq!(start_of(&plan), 2 * BAR);
    }

    /// The complaint from the report, as an assertion: at eight bars a key pressed anywhere inside
    /// bars 1 to 8 arms the take for bar 9, so one early press is enough.
    #[test]
    fn with_loop_quantisation_every_bar_of_the_loop_collects_on_the_next_loop_start() {
        let mut s = scheduler(120.0, 8, Quantize::Loop);
        // One-based bar 3 (index 2) and bar 8 (index 7), both mid-bar.
        for bar in [2u64, 7] {
            let plan = s.record(0, "gitarre", bar * BAR + 1_000);
            assert_eq!(start_of(&plan), 8 * BAR, "Takt {} gehoert zu Loop 1", bar + 1);
            assert_eq!(
                plan.commands[1].at(),
                Some(16 * BAR),
                "die Aufnahme dauert genau einen Loop"
            );
        }
        // Bar 1 of the second loop already belongs to the next one.
        let plan = s.record(0, "gitarre", 8 * BAR + 1_000);
        assert_eq!(start_of(&plan), 16 * BAR);
    }

    /// A press that arrives before a loop boundary but close enough that the head start still fits
    /// must land on *that* boundary, not on the one after it.
    #[test]
    fn a_command_on_a_loop_boundary_stays_on_that_boundary() {
        let mut s = scheduler(120.0, 8, Quantize::Loop);
        // `next_start` quantises `est + guard`, so aim the head start exactly at the boundary.
        let est = 8 * BAR - s.guard;
        assert_eq!(s.next_start(est, None), 8 * BAR);
        let plan = s.record(0, "gitarre", est);
        assert_eq!(start_of(&plan), 8 * BAR);
        // One sample later the head start no longer fits and the next loop is taken.
        assert_eq!(s.next_start(est + 1, None), 16 * BAR);
    }

    /// An overdub is one pass of the existing loop, never the configured bar count - otherwise a
    /// layer would be longer than what it is layered onto.
    #[test]
    fn an_overdub_covers_exactly_one_pass_of_the_existing_loop() {
        let mut s = scheduler(120.0, 8, Quantize::Bar);
        let loop_len = 4 * BAR;
        let plan = s.overdub(0, "gitarre", 100_000, 0, loop_len, 2);
        assert_eq!(
            plan.commands,
            vec![
                Command::StartOverdub { track: 0, at: 2 * BAR },
                Command::StopRecord { track: 0, at: 2 * BAR + loop_len },
                Command::StartPlay { track: 0, at: 2 * BAR + loop_len },
            ]
        );
        assert!(plan.message.contains("layer 3"), "{}", plan.message);

        // On an empty track it behaves like a first take of the configured length.
        let plan = s.overdub(0, "gitarre", 100_000, 0, 0, 0);
        assert_eq!(
            plan.commands[1],
            Command::StopRecord { track: 0, at: 2 * BAR + 8 * BAR }
        );
    }

    /// The property the layer stack rests on: a further layer starts exactly on the existing
    /// loop's own grid, `origin + n * loop_len`, and covers exactly one pass. Checked at time
    /// signatures whose bar length is not a whole number of samples, because that is where a grid
    /// derived from bar boundaries would drift away from the track's own.
    #[test]
    fn an_overdub_lands_on_the_existing_loops_own_grid_at_any_time_signature() {
        for (bpm, bpb, unit, bars) in [
            (120.0, 4, 4, 8u32),
            (100.0, 3, 4, 4),
            (137.0, 7, 8, 5),
            (93.5, 5, 4, 3),
        ] {
            let timeline = Timeline::new(RATE, bpm, TimeSignature::new(bpb, unit));
            let mut s = Scheduler::new(timeline, bars, 128, Quantize::Loop);
            // A loop as the first take would have produced it: origin on the loop grid, length the
            // span of `bars` bars from there.
            let origin = timeline.loop_start_at_or_after(0, bars);
            let loop_len = timeline.span_bars(timeline.bar_index_at(origin), bars);

            for pass in 0..12u64 {
                for offset in [1u64, 1_000, loop_len / 3, loop_len - 1] {
                    let est = origin + pass * loop_len + offset;
                    let plan = s.overdub(0, "t", est, origin, loop_len, 1);
                    let start = start_of(&plan);
                    assert!(start >= est + s.guard, "der Vorlauf muss passen");
                    assert_eq!(
                        (start - origin) % loop_len,
                        0,
                        "{bpb}/{unit} bei {bpm}: Ebene liegt sample-genau auf dem Loop-Raster"
                    );
                    assert_eq!(
                        plan.commands[1].at(),
                        Some(start + loop_len),
                        "eine Ebene ist genau einen Durchlauf lang"
                    );
                }
            }
        }
    }

    /// With bar quantisation an overdub keeps the old behaviour: next bar, whatever the loop grid
    /// would say.
    #[test]
    fn bar_quantisation_ignores_the_loop_grid() {
        let mut s = scheduler(120.0, 8, Quantize::Bar);
        // A loop whose origin is deliberately not a multiple of a bar.
        let plan = s.overdub(0, "t", 100_000, 5_000, 8 * BAR, 1);
        assert_eq!(start_of(&plan), 2 * BAR, "die Taktgrenze gewinnt");
    }

    #[test]
    fn stopping_means_three_different_things_depending_on_the_state() {
        let mut s = scheduler(120.0, 8, Quantize::Loop);
        let armed = s.stop(0, "gitarre", 100_000, TrackState::Armed, StopAt::BarEnd, true);
        assert!(matches!(armed.commands[0], Command::ClearTrack { .. }));
        assert!(armed.message.contains("cancelled"), "{}", armed.message);

        let recording = s.stop(0, "gitarre", 100_000, TrackState::Recording, StopAt::BarEnd, true);
        assert_eq!(
            recording.commands,
            vec![
                Command::StopRecord { track: 0, at: 2 * BAR },
                Command::StartPlay { track: 0, at: 2 * BAR },
            ]
        );

        // Playback stops on the next bar line by default ...
        let playing = s.stop(1, "gitarre", 100_000, TrackState::Playing, StopAt::BarEnd, true);
        assert_eq!(playing.commands, vec![Command::StopPlay { track: 1, at: 2 * BAR }]);
        assert!(playing.message.contains("end of the bar"), "{}", playing.message);
        // ... and now, one head start from the press, on the explicit second way.
        let now = s.stop(2, "gitarre", 100_000, TrackState::Playing, StopAt::Now, true);
        assert_eq!(now.commands, vec![Command::StopPlay { track: 2, at: 100_000 + s.guard }]);
    }

    /// A second press on a waiting stop takes it back; so does play, record, overdub or clear.
    #[test]
    fn a_waiting_stop_is_cancelled_by_a_second_press_and_by_a_newer_action() {
        let mut s = scheduler(120.0, 8, Quantize::Loop);
        let est = 100_000;
        s.stop(0, "t", est, TrackState::Playing, StopAt::BarEnd, true);
        assert_eq!(s.lead(0, est).kind, Some(PendingKind::Stop));
        assert_eq!(s.waiting_stop(0, est), Some((2 * BAR, false)));

        let again = s.stop(0, "t", est + 1_000, TrackState::Playing, StopAt::BarEnd, true);
        assert_eq!(
            again.commands,
            vec![Command::CancelStop { track: 0, at: 2 * BAR, take: false }]
        );
        assert!(again.message.contains("cancelled"), "{}", again.message);
        assert_eq!(s.lead(0, est), Lead::default(), "the countdown is gone");

        // A third press arms it again.
        let third = s.stop(0, "t", est + 2_000, TrackState::Playing, StopAt::BarEnd, true);
        assert_eq!(third.commands, vec![Command::StopPlay { track: 0, at: 2 * BAR }]);

        // Play (or record, overdub, clear) supersedes it.
        assert_eq!(
            s.supersede_stop(0, est + 3_000),
            vec![Command::CancelStop { track: 0, at: 2 * BAR, take: false }]
        );
        assert_eq!(s.lead(0, est + 3_000), Lead::default());
        assert!(s.supersede_stop(0, est + 3_000).is_empty(), "nothing left to take back");
    }

    /// Stop now while a stop waits: silent at once, and the waiting one is taken back so a play
    /// pressed before that bar line is not stopped by it.
    #[test]
    fn stop_now_while_a_stop_waits_acts_at_once_and_takes_the_waiting_one_back() {
        let mut s = scheduler(120.0, 8, Quantize::Loop);
        s.stop(0, "t", 100_000, TrackState::Playing, StopAt::BarEnd, true);
        let now = s.stop(0, "t", 110_000, TrackState::Playing, StopAt::Now, true);
        assert_eq!(
            now.commands,
            vec![
                Command::CancelStop { track: 0, at: 2 * BAR, take: false },
                Command::StopPlay { track: 0, at: 110_000 + s.guard },
            ]
        );
        assert_eq!(s.lead(0, 110_000), Lead::default());
    }

    /// Too close to the bar line the cancel could arrive after the stop began its fade: the press
    /// changes nothing and says so, rather than promising what the engine cannot keep.
    #[test]
    fn a_cancel_too_close_to_the_bar_line_is_refused() {
        let mut s = scheduler(120.0, 8, Quantize::Loop);
        s.stop(0, "t", 100_000, TrackState::Playing, StopAt::BarEnd, true);
        let late = 2 * BAR - s.guard;
        let plan = s.stop(0, "t", late, TrackState::Playing, StopAt::BarEnd, true);
        assert!(plan.commands.is_empty());
        assert!(plan.message.contains("too late"), "{}", plan.message);
        assert_eq!(s.lead(0, late).kind, Some(PendingKind::Stop), "still counting down");
        assert!(s.supersede_stop(0, late).is_empty());
    }

    /// A stop during a take ends it on the bar line, as before; pressed again it is taken back with
    /// both of its commands. The small half does not cut a take mid-bar.
    #[test]
    fn the_early_end_of_a_take_can_be_taken_back_but_not_cut_shorter() {
        let mut s = scheduler(120.0, 8, Quantize::Loop);
        let first = s.stop(0, "t", 100_000, TrackState::Overdub, StopAt::BarEnd, true);
        assert_eq!(start_of(&first), 2 * BAR);
        let now = s.stop(0, "t", 101_000, TrackState::Overdub, StopAt::Now, true);
        assert!(now.commands.is_empty(), "{:?}", now.commands);
        assert!(now.message.contains("never cut"), "{}", now.message);
        let again = s.stop(0, "t", 102_000, TrackState::Overdub, StopAt::BarEnd, true);
        assert_eq!(
            again.commands,
            vec![Command::CancelStop { track: 0, at: 2 * BAR, take: true }]
        );
        // A newer action does not touch the early end of a take.
        s.stop(0, "t", 103_000, TrackState::Overdub, StopAt::BarEnd, true);
        assert!(s.supersede_stop(0, 104_000).is_empty());
    }

    /// No bar to wait for: with the clock standing every loop is silent, and a stop acts at once.
    #[test]
    fn with_the_clock_standing_a_stop_acts_at_once() {
        let mut s = scheduler(120.0, 8, Quantize::Loop);
        let plan = s.stop(0, "t", 100_000, TrackState::Playing, StopAt::BarEnd, false);
        assert_eq!(plan.commands, vec![Command::StopPlay { track: 0, at: 100_000 + s.guard }]);
        assert!(plan.message.contains("at once"), "{}", plan.message);
        assert_eq!(s.lead(0, 100_000), Lead::default());
    }

    /// A track that is not playing has nothing to wait for: a play armed for a bar line is
    /// cancelled on that bar line, otherwise it is silent at once.
    #[test]
    fn stop_on_a_track_that_waits_for_play_cancels_the_play() {
        let mut s = scheduler(120.0, 8, Quantize::Loop);
        s.play(0, "t", 100_000);
        let plan = s.stop(0, "t", 100_000, TrackState::Ready, StopAt::BarEnd, true);
        assert_eq!(plan.commands, vec![Command::StopPlay { track: 0, at: 2 * BAR }]);
        assert!(plan.message.contains("play cancelled"), "{}", plan.message);
        assert_eq!(s.lead(0, 100_000), Lead::default());

        let idle = s.stop(1, "t", 100_000, TrackState::Ready, StopAt::BarEnd, true);
        assert_eq!(idle.commands, vec![Command::StopPlay { track: 1, at: 100_000 + s.guard }]);
    }

    /// The score's own stops are not stops by hand: nothing in free looping takes them back.
    #[test]
    fn the_scores_stops_are_never_taken_back() {
        let mut s = scheduler(120.0, 8, Quantize::Loop);
        s.stop_play_at(0, "t", 4 * BAR);
        assert_eq!(s.waiting_stop(0, 100_000), None);
        assert!(s.supersede_stop(0, 100_000).is_empty());
    }

    /// Stop and playback stay on the bar grid even in loop mode - a correction that waits eight
    /// bars is a broken button.
    #[test]
    fn stop_and_playback_stay_on_the_bar_grid_in_loop_mode() {
        let mut s = scheduler(120.0, 8, Quantize::Loop);
        let plan = s.play(0, "gitarre", 100_000);
        assert_eq!(start_of(&plan), 2 * BAR);
        let plan = s.stop(0, "gitarre", 100_000, TrackState::Recording, StopAt::BarEnd, true);
        assert_eq!(start_of(&plan), 2 * BAR);
        let plan = s.stop(1, "gitarre", 100_000, TrackState::Playing, StopAt::BarEnd, true);
        assert_eq!(start_of(&plan), 2 * BAR, "in loop mode too: the bar, not the loop");
    }

    #[test]
    fn the_position_estimate_follows_the_interface_clock() {
        let s = scheduler(120.0, 8, Quantize::Loop);
        assert_eq!(s.estimated_pos(None), 0);
        assert_eq!(
            s.estimated_pos(Some((48_000, std::time::Duration::from_millis(100)))),
            48_000 + 4_800
        );
    }

    /// A fractional beat length is the case that catches an incrementally advanced grid; the plan
    /// must still land on positions the timeline itself computes.
    #[test]
    fn odd_tempos_and_time_signatures_stay_on_the_timeline_grid() {
        let timeline = Timeline::new(RATE, 137.0, TimeSignature::new(7, 8));
        let mut s = Scheduler::new(timeline, 5, 128, Quantize::Bar);
        let plan = s.record(0, "t", 1_000_000);
        let start = start_of(&plan);
        let bar = timeline.bar_index_at(start);
        assert_eq!(start, timeline.bar_start(bar), "Start liegt auf einer Taktgrenze");
        assert_eq!(
            plan.commands[1].at(),
            Some(timeline.position_of(bar + 5, 0)),
            "Ende liegt fuenf Takte spaeter, absolut gerechnet"
        );
    }

    // ---- the count-in --------------------------------------------------------------------

    /// The number on screen has to mean what it says: reading it off at `pos` and walking that
    /// many bars and beats forward has to arrive at the sample the engine acts on.
    #[test]
    fn the_lead_counts_down_to_exactly_the_scheduled_sample() {
        for (bpm, bpb, unit, bars) in [(120.0, 4, 4, 8u32), (137.0, 7, 8, 5), (100.0, 3, 4, 4)] {
            let timeline = Timeline::new(RATE, bpm, TimeSignature::new(bpb, unit));
            let mut s = Scheduler::new(timeline, bars, 128, Quantize::Loop);
            let est = timeline.bar_start(2) + 1_234;
            let plan = s.record(0, "t", est);
            let at = start_of(&plan);

            // Walk from just after the press to the scheduled sample, one beat at a time.
            let first_beat = timeline.beat_index_at(est);
            let last_beat = timeline.beat_index_at(at);
            for beat in first_beat..last_beat {
                let pos = timeline.beat_start(beat);
                let lead = s.lead(0, pos);
                assert_eq!(lead.kind, Some(PendingKind::Record));
                let walked = lead.bars as u64 * bpb as u64 + lead.beats as u64;
                assert_eq!(
                    timeline.beat_start(beat + walked),
                    at,
                    "{bpb}/{unit}: {} Takte und {} Schlaege muessen genau auf den Start fuehren",
                    lead.bars,
                    lead.beats
                );
            }
            // On the scheduled sample itself nothing is pending any more.
            assert_eq!(s.lead(0, at), Lead::default());
            assert!(!s.lead(0, at).is_pending());
        }
    }

    /// The last bar is counted down in beats, which is what the display switches on.
    #[test]
    fn the_lead_turns_into_beats_inside_the_last_bar() {
        let mut s = scheduler(120.0, 8, Quantize::Loop);
        let plan = s.record(0, "gitarre", 1_000);
        assert_eq!(start_of(&plan), 8 * BAR);

        assert_eq!(
            s.lead(0, 0),
            Lead {
                kind: Some(PendingKind::Record),
                bars: 8,
                beats: 0
            }
        );
        assert_eq!(s.lead(0, 0).text().unwrap(), "Record in 8 bars");
        // Three beats into the last bar of the count-in: one beat left.
        let pos = 7 * BAR + 3 * 24_000;
        assert_eq!(
            s.lead(0, pos),
            Lead {
                kind: Some(PendingKind::Record),
                bars: 0,
                beats: 1
            }
        );
        assert_eq!(s.lead(0, pos).text().unwrap(), "Record in 1 beat");
        assert_eq!(s.lead(0, 6 * BAR).text().unwrap(), "Record in 2 bars");
        assert_eq!(s.lead(0, 7 * BAR).text().unwrap(), "Record in 1 bar");
    }

    /// Every action that can be waited for announces itself, and everything that cancels one takes
    /// the announcement away again.
    #[test]
    fn the_lead_follows_what_was_scheduled_and_disappears_when_it_is_cancelled() {
        let mut s = scheduler(120.0, 8, Quantize::Loop);
        assert_eq!(s.lead(0, 0), Lead::default(), "frisch ist nichts geplant");

        s.overdub(0, "t", 1_000, 0, 8 * BAR, 1);
        assert_eq!(s.lead(0, 1_000).kind, Some(PendingKind::Overdub));

        s.play(1, "t", 1_000);
        assert_eq!(s.lead(1, 1_000).kind, Some(PendingKind::Play));

        s.stop(1, "t", 1_000, TrackState::Recording, StopAt::BarEnd, true);
        assert_eq!(s.lead(1, 1_000).kind, Some(PendingKind::Stop));

        // Cancelling an armed take, clearing a track and clearing everything all take it back.
        s.stop(0, "t", 1_000, TrackState::Armed, StopAt::BarEnd, true);
        assert_eq!(s.lead(0, 1_000), Lead::default());
        s.record(0, "t", 1_000);
        s.clear_track(0, "t", 1_000);
        assert_eq!(s.lead(0, 1_000), Lead::default());
        s.record(0, "t", 1_000);
        s.record(1, "t", 1_000);
        s.clear_all(1_000);
        assert_eq!(s.lead(0, 1_000), Lead::default());
        assert_eq!(s.lead(1, 1_000), Lead::default());
        // A track index beyond the ceiling must not panic, only report nothing.
        assert_eq!(s.lead(MAX_TRACKS + 3, 1_000), Lead::default());
    }

    #[test]
    fn switching_the_grid_at_runtime_changes_the_next_take_only() {
        let mut s = scheduler(120.0, 8, Quantize::Loop);
        let plan = s.record(0, "t", 1_000);
        assert_eq!(start_of(&plan), 8 * BAR);
        let message = s.set_quantize(Quantize::Bar);
        assert!(message.contains("Bar"), "{message}");
        // The armed take keeps its position; the engine already has that command.
        assert_eq!(s.lead(0, 0).bars, 8);
        let plan = s.record(1, "t", 1_000);
        assert_eq!(start_of(&plan), BAR, "der naechste Take nimmt das neue Raster");
    }
}
