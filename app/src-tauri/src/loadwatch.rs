//! The control thread's side of the audio load (`looper_engine::load`): turns the counters the
//! output callback publishes into **overruns** a human can count, a rate-limited log line, a marker
//! in the running recording and the `load` part of the status event.
//!
//! Three counters can say "the audio was late" - a late block, a late wake-up and the driver's own
//! overload notification (cpal's `Xrun`, which on macOS is `kAudioDeviceProcessorOverload`). One
//! slow callback typically moves two of them, and with the input and the output on the same device
//! the driver reports the one overload once per stream. So everything that rises within
//! [`COALESCE`] of the previous rise is **one** overrun; the counter on screen counts those.
//!
//! An overrun is written to the log and to the recording once it is complete - [`COALESCE`] after
//! its last rise - so the line and the marker name everything that came together.

use std::time::{Duration, Instant};

use looper_engine::load::LoadReading;

use crate::proto::LoadStatusEvent;

/// Counter rises closer together than this belong to one overrun.
pub const COALESCE: Duration = Duration::from_millis(60);
/// At most one log line per this interval; overruns in between are summed into the next line.
pub const LOG_INTERVAL: Duration = Duration::from_secs(2);

/// What the counters say, in one place.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Counts {
    pub late_blocks: u64,
    pub late_wakeups: u64,
    /// cpal `Xrun`s - CoreAudio's processor overload on macOS.
    pub overloads: u64,
}

const LATE_BLOCK: u8 = 1;
const LATE_WAKEUP: u8 = 2;
const OVERLOAD: u8 = 4;

/// One overrun: what rose, where, how bad.
#[derive(Clone, Debug, PartialEq)]
pub struct Incident {
    /// 1 for the first overrun of the engine session.
    pub number: u64,
    kinds: u8,
    /// Engine position: the start of the late block, or the estimate when only the driver spoke.
    pub frame: u64,
    exact: bool,
    /// Load of the late callback in percent; 0 when only the driver reported it.
    pub percent: f32,
    /// Local wall-clock time, `HH:MM:SS`.
    pub wall: String,
    last_rise: Instant,
}

impl Incident {
    /// "late block + driver overload".
    pub fn kind_text(&self) -> String {
        let mut parts = Vec::new();
        if self.kinds & LATE_BLOCK != 0 {
            parts.push("late block");
        }
        if self.kinds & LATE_WAKEUP != 0 {
            parts.push("late wake-up");
        }
        if self.kinds & OVERLOAD != 0 {
            parts.push("driver overload");
        }
        parts.join(" + ")
    }
}

#[derive(Debug, Default)]
pub struct LoadWatch {
    seen: Counts,
    /// Overruns counted since the engine started.
    pub incidents: u64,
    open: Option<Incident>,
    /// The newest overrun, open or closed - what the status shows.
    last: Option<Incident>,
    last_log: Option<Instant>,
    held: u64,
    held_worst: f32,
}

impl LoadWatch {
    /// One turn of the host thread. `pos` is the estimated engine position (used only when the
    /// driver reported an overload the callback did not see), `wall` the local time for a new
    /// overrun. Returns an overrun once it is complete.
    pub fn observe(
        &mut self,
        now: Instant,
        counts: Counts,
        reading: &LoadReading,
        pos: u64,
        wall: impl FnOnce() -> String,
    ) -> Option<Incident> {
        let mut kinds = 0u8;
        if counts.late_blocks > self.seen.late_blocks {
            kinds |= LATE_BLOCK;
        }
        if counts.late_wakeups > self.seen.late_wakeups {
            kinds |= LATE_WAKEUP;
        }
        if counts.overloads > self.seen.overloads {
            kinds |= OVERLOAD;
        }
        self.seen = counts;

        let mut closed = None;
        if kinds != 0 {
            let exact = kinds & (LATE_BLOCK | LATE_WAKEUP) != 0 && reading.last_frame.is_some();
            let (frame, percent) = if exact {
                (reading.last_frame.unwrap_or(pos), reading.last_percent)
            } else {
                (pos, 0.0)
            };
            match self.open.as_mut() {
                Some(open) if now.duration_since(open.last_rise) < COALESCE => {
                    open.kinds |= kinds;
                    open.last_rise = now;
                    // The callback's own frame beats the host's estimate.
                    if exact && !open.exact {
                        open.frame = frame;
                        open.exact = true;
                    }
                    open.percent = open.percent.max(percent);
                }
                _ => {
                    closed = self.open.take();
                    self.incidents += 1;
                    self.open = Some(Incident {
                        number: self.incidents,
                        kinds,
                        frame,
                        exact,
                        percent,
                        wall: wall(),
                        last_rise: now,
                    });
                }
            }
            self.last = self.open.clone();
        } else if self
            .open
            .as_ref()
            .is_some_and(|open| now.duration_since(open.last_rise) >= COALESCE)
        {
            closed = self.open.take();
        }
        closed
    }

    /// Whether `incident` may be logged now. `Some(n)` when it may, with `n` overruns held back
    /// since the previous line (and their worst load in [`LoadWatch::held_worst`] before the call);
    /// `None` when it is held back for the next line.
    pub fn admit_log(&mut self, now: Instant, incident: &Incident) -> Option<(u64, f32)> {
        if self
            .last_log
            .is_some_and(|at| now.duration_since(at) < LOG_INTERVAL)
        {
            self.held += 1;
            self.held_worst = self.held_worst.max(incident.percent);
            return None;
        }
        self.last_log = Some(now);
        Some((
            std::mem::take(&mut self.held),
            std::mem::take(&mut self.held_worst),
        ))
    }

    /// Overruns held back and not followed by another one within [`LOG_INTERVAL`]: their count and
    /// worst load, once, so a burst is never left out of the log.
    pub fn flush_log(&mut self, now: Instant) -> Option<(u64, f32)> {
        if self.held == 0
            || self
                .last_log
                .is_some_and(|at| now.duration_since(at) < LOG_INTERVAL)
        {
            return None;
        }
        self.last_log = Some(now);
        Some((
            std::mem::take(&mut self.held),
            std::mem::take(&mut self.held_worst),
        ))
    }

    /// The `load` part of the status event.
    pub fn event(&self, reading: &LoadReading, overloads: u64, sample_rate: u32) -> LoadStatusEvent {
        let block = reading.block_frames;
        LoadStatusEvent {
            avg_percent: round1(reading.avg_percent),
            peak_percent: round1(reading.peak_percent),
            plugin_percent: round1(reading.plugin_percent),
            overruns: self.incidents,
            late_blocks: reading.late_blocks,
            late_wakeups: reading.late_wakeups,
            driver_overloads: overloads,
            last_overrun: self.last.as_ref().map(|i| i.wall.clone()),
            last_overrun_kind: self.last.as_ref().map(Incident::kind_text),
            last_overrun_percent: self.last.as_ref().map_or(0.0, |i| round1(i.percent)),
            block_frames: block,
            budget_ms: if sample_rate > 0 {
                f64::from(block) * 1000.0 / f64::from(sample_rate)
            } else {
                0.0
            },
        }
    }
}

fn round1(v: f32) -> f32 {
    (v * 10.0).round() / 10.0
}

#[cfg(test)]
mod tests {
    use super::*;

    fn reading(frame: Option<u64>, percent: f32) -> LoadReading {
        LoadReading {
            last_frame: frame,
            last_percent: percent,
            block_frames: 128,
            ..Default::default()
        }
    }

    fn counts(late_blocks: u64, late_wakeups: u64, overloads: u64) -> Counts {
        Counts {
            late_blocks,
            late_wakeups,
            overloads,
        }
    }

    const MS: Duration = Duration::from_millis(1);

    #[test]
    fn a_late_block_and_the_drivers_overload_right_after_are_one_overrun() {
        let mut w = LoadWatch::default();
        let t0 = Instant::now();
        let r = reading(Some(48_000), 118.0);
        assert!(w.observe(t0, counts(1, 0, 0), &r, 48_300, || "21:14:05".into()).is_none());
        assert_eq!(w.incidents, 1);
        // The driver's notification, once per stream, a few ms later.
        assert!(w.observe(t0 + 4 * MS, counts(1, 0, 2), &r, 48_500, || unreachable!()).is_none());
        assert_eq!(w.incidents, 1, "one overrun, not three");
        // Nothing for the coalescing time: the overrun is complete.
        assert!(w.observe(t0 + 30 * MS, counts(1, 0, 2), &r, 49_000, || unreachable!()).is_none());
        let done = w.observe(t0 + 70 * MS, counts(1, 0, 2), &r, 50_000, || unreachable!()).unwrap();
        assert_eq!(done.number, 1);
        assert_eq!(done.frame, 48_000, "the callback's own frame, not the estimate");
        assert_eq!(done.percent, 118.0);
        assert_eq!(done.wall, "21:14:05");
        assert_eq!(done.kind_text(), "late block + driver overload");
        assert!(w.observe(t0 + 200 * MS, counts(1, 0, 2), &r, 60_000, || unreachable!()).is_none());
    }

    #[test]
    fn an_overload_only_the_driver_saw_is_placed_at_the_estimate() {
        let mut w = LoadWatch::default();
        let t0 = Instant::now();
        let r = reading(None, 0.0);
        w.observe(t0, counts(0, 0, 1), &r, 96_000, || "21:00:00".into());
        let done = w.observe(t0 + 100 * MS, counts(0, 0, 1), &r, 99_000, || unreachable!()).unwrap();
        assert_eq!(done.frame, 96_000);
        assert_eq!(done.percent, 0.0);
        assert_eq!(done.kind_text(), "driver overload");
    }

    #[test]
    fn two_overruns_apart_are_counted_twice_and_the_first_closes_when_the_second_starts() {
        let mut w = LoadWatch::default();
        let t0 = Instant::now();
        w.observe(t0, counts(1, 0, 0), &reading(Some(100), 105.0), 0, || "a".into());
        let first = w
            .observe(t0 + 20 * MS, counts(1, 0, 0), &reading(Some(100), 105.0), 0, || unreachable!());
        assert!(first.is_none());
        // 80 ms after the last rise: a new one. It closes the first.
        let first = w
            .observe(t0 + 80 * MS, counts(1, 1, 0), &reading(Some(9_000), 40.0), 0, || "b".into())
            .unwrap();
        assert_eq!(first.number, 1);
        assert_eq!(w.incidents, 2);
        let status = w.event(&reading(Some(9_000), 40.0), 0, 48_000);
        assert_eq!(status.overruns, 2);
        assert_eq!(status.last_overrun.as_deref(), Some("b"));
        assert_eq!(status.last_overrun_kind.as_deref(), Some("late wake-up"));
        assert_eq!(status.last_overrun_percent, 40.0);
        assert!((status.budget_ms - 2.6667).abs() < 0.001);
    }

    #[test]
    fn the_log_takes_one_line_per_interval_and_sums_the_rest() {
        let mut w = LoadWatch::default();
        let t0 = Instant::now();
        let inc = |percent: f32| Incident {
            number: 0,
            kinds: LATE_BLOCK,
            frame: 0,
            exact: true,
            percent,
            wall: String::new(),
            last_rise: t0,
        };
        assert_eq!(w.admit_log(t0, &inc(110.0)), Some((0, 0.0)));
        assert_eq!(w.admit_log(t0 + 500 * MS, &inc(130.0)), None);
        assert_eq!(w.admit_log(t0 + 900 * MS, &inc(120.0)), None);
        assert_eq!(w.flush_log(t0 + 1500 * MS), None, "not yet");
        assert_eq!(w.flush_log(t0 + 2100 * MS), Some((2, 130.0)));
        assert_eq!(w.flush_log(t0 + 9000 * MS), None, "said once");
        // After a flush the next one waits for the interval again, and carries nothing.
        assert_eq!(w.admit_log(t0 + 3000 * MS, &inc(101.0)), None);
        assert_eq!(w.admit_log(t0 + 4200 * MS, &inc(102.0)), Some((1, 101.0)));
    }

    #[test]
    fn the_status_part_is_quiet_while_nothing_happened() {
        let w = LoadWatch::default();
        let r = LoadReading {
            avg_percent: 23.456,
            peak_percent: 61.04,
            plugin_percent: 12.0,
            block_frames: 256,
            ..Default::default()
        };
        let e = w.event(&r, 0, 48_000);
        assert_eq!(e.avg_percent, 23.5);
        assert_eq!(e.peak_percent, 61.0);
        assert_eq!(e.overruns, 0);
        assert_eq!(e.last_overrun, None);
        assert_eq!(e.block_frames, 256);
        assert!((e.budget_ms - 5.3333).abs() < 0.001);
    }
}
