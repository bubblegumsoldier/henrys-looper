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
//!
//! # What the line says
//!
//! Each late callback hands over its own [`LateDetail`]: how long it took, when it started
//! relative to the previous one, and its profile (phases, tracks, plugins, commands applied,
//! allocations, CPU time). The line reports **each late callback separately** - the late block
//! with its own duration, the late wake-up with the gap before it. (Until 2026-11-21 it quoted one
//! "load" for the whole overrun: the load of whichever late callback came last. A late block is
//! followed by a late wake-up - the next callback starts late *because* the one before ran long -
//! so the line named the quick second callback's 7 % and lost the slow one's duration.)

use std::time::{Duration, Instant};

use looper_engine::load::{LateDetail, Lateness, LoadReading};

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
    /// Input callbacks that found the input FIFO full: input frames were **lost**. The one kind
    /// that cuts a running take - the frames after the gap are recorded right after the frames
    /// before it, a one-sample jump in the loop on every pass (the owner's `theme_b`, 2026-11-21).
    pub input_lost: u64,
    /// Output callbacks that found the input FIFO short: nothing lost, but the monitored input is
    /// silent for the rest of that block.
    pub input_late: u64,
}

const LATE_BLOCK: u8 = 1;
const LATE_WAKEUP: u8 = 2;
const OVERLOAD: u8 = 4;
const INPUT_LOST: u8 = 8;
const INPUT_LATE: u8 = 16;

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
    /// The late callbacks that belong to it, in order (at most [`MAX_DETAILS`]).
    pub details: Vec<LateDetail>,
}

/// Late callbacks kept per overrun.
pub const MAX_DETAILS: usize = 4;

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
        if self.kinds & INPUT_LOST != 0 {
            parts.push("input lost");
        }
        if self.kinds & INPUT_LATE != 0 {
            parts.push("input late");
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
    /// [`Self::observe_with`] without late-callback records - the counters alone.
    #[cfg(test)]
    pub fn observe(
        &mut self,
        now: Instant,
        counts: Counts,
        reading: &LoadReading,
        pos: u64,
        wall: impl FnOnce() -> String,
    ) -> Option<Incident> {
        self.observe_with(now, counts, reading, &[], pos, wall)
    }

    /// One turn of the host thread. `details` are the late callbacks the callback queued since the
    /// last turn; `pos` is the estimated engine position (used only when the driver reported an
    /// overload no callback saw), `wall` the local time for a new overrun. Returns an overrun
    /// once it is complete.
    pub fn observe_with(
        &mut self,
        now: Instant,
        counts: Counts,
        reading: &LoadReading,
        details: &[LateDetail],
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
        if counts.input_lost > self.seen.input_lost {
            kinds |= INPUT_LOST;
        }
        if counts.input_late > self.seen.input_late {
            kinds |= INPUT_LATE;
        }
        self.seen = counts;

        let mut closed = None;
        if kinds != 0 {
            let exact = kinds & (LATE_BLOCK | LATE_WAKEUP) != 0 && reading.last_frame.is_some();
            let (mut frame, mut percent) = if exact {
                (reading.last_frame.unwrap_or(pos), reading.last_percent)
            } else {
                (pos, 0.0)
            };
            // The callbacks' own records beat the last-seen numbers: the frame of the first late
            // callback, the load of the slowest.
            if let Some(first) = details.first() {
                frame = first.frame;
                percent = details.iter().map(own_percent).fold(0.0, f32::max);
            }
            match self.open.as_mut() {
                Some(open) if now.duration_since(open.last_rise) < COALESCE => {
                    open.kinds |= kinds;
                    open.last_rise = now;
                    let room = MAX_DETAILS.saturating_sub(open.details.len());
                    open.details.extend(details.iter().take(room).copied());
                    if details.first().is_some() && !open.exact {
                        open.frame = frame;
                        open.exact = true;
                    }
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
                        details: details.iter().take(MAX_DETAILS).copied().collect(),
                    });
                }
            }
            self.last = self.open.clone();
        } else if let Some(open) = self.open.as_mut()
            && !details.is_empty()
        {
            // Details that arrive a tick after their counter (the queue is read separately).
            let room = MAX_DETAILS.saturating_sub(open.details.len());
            open.details.extend(details.iter().take(room).copied());
            let worst = details.iter().map(own_percent).fold(0.0, f32::max);
            open.percent = open.percent.max(worst);
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

/// A late callback's own duration in percent of its budget.
pub fn own_percent(d: &LateDetail) -> f32 {
    if d.budget_ns == 0 {
        0.0
    } else {
        (d.busy_ns as f64 * 100.0 / d.budget_ns as f64) as f32
    }
}

fn ms(ns: u64) -> f64 {
    ns as f64 / 1e6
}

/// What one late callback's record points at, in a few words - the `cause` in `session.json`.
pub fn cause(d: &LateDetail) -> String {
    let p = &d.profile;
    let late_block = matches!(d.lateness, Lateness::Block | Lateness::Both);
    if !late_block {
        // On time itself, but started late.
        return if d.prev_busy_ns > d.budget_ns {
            format!(
                "the callback before it ran {:.2} ms (over its {:.2} ms budget)",
                ms(d.prev_busy_ns),
                ms(d.budget_ns)
            )
        } else {
            format!(
                "woken {:.2} ms late by the system - the callback before it was quick ({:.2} ms), \
                 so the thread waited for its wake-up (other real-time work, the driver, the kernel)",
                ms(d.idle_ns.saturating_sub(d.budget_ns.saturating_sub(d.prev_busy_ns))),
                ms(d.prev_busy_ns)
            )
        };
    }
    let mut why = Vec::new();
    if p.allocs > 0 || p.frees > 0 {
        why.push(format!(
            "{} allocation(s) and {} free(s) on the audio thread",
            p.allocs, p.frees
        ));
    }
    if let Some(off) = p.off_cpu_ns(d.busy_ns)
        && off * 2 > d.busy_ns
    {
        why.push(format!(
            "the thread was not running for {:.2} ms of it (preempted, or waiting on a lock or a \
             page-in)",
            ms(off)
        ));
    }
    if p.cpu_known && u64::from(p.system_ns) * 2 > d.busy_ns {
        why.push(format!(
            "{:.2} ms in the kernel (page faults or system calls)",
            ms(u64::from(p.system_ns))
        ));
    }
    if u64::from(p.plugin_ns) * 2 > d.busy_ns {
        why.push(format!("{:.2} ms inside plugins", ms(u64::from(p.plugin_ns))));
    }
    if why.is_empty()
        && let Some((phase, ns)) = p.slowest_phases(1).first()
    {
        why.push(format!("the engine's {phase} phase ({:.2} ms)", ms(u64::from(*ns))));
    }
    why.join("; ")
}

/// One late callback, fully: for the log line. `names` are the track names in engine order.
pub fn describe(d: &LateDetail, names: &[String]) -> String {
    let p = &d.profile;
    let what = match d.lateness {
        Lateness::Block => "late block",
        Lateness::WakeUp => "late wake-up",
        Lateness::Both => "late block that also started late",
    };
    let mut text = format!(
        "{what} at frame {}: took {:.2} ms ({:.0} % of {:.2} ms), started {:.2} ms after the \
         previous callback (which took {:.2} ms)",
        d.frame,
        ms(d.busy_ns),
        own_percent(d),
        ms(d.budget_ns),
        ms(d.interval_ns),
        ms(d.prev_busy_ns),
    );
    if p.cpu_known {
        text += &format!(
            "; CPU {:.2} ms (system {:.2})",
            ms(u64::from(p.user_ns) + u64::from(p.system_ns)),
            ms(u64::from(p.system_ns))
        );
        if let Some(off) = p.off_cpu_ns(d.busy_ns) {
            text += &format!(", not running {:.2} ms", ms(off));
        }
    }
    let phases: Vec<String> = p
        .slowest_phases(4)
        .iter()
        .map(|(name, ns)| format!("{name} {:.2}", ms(u64::from(*ns))))
        .collect();
    if !phases.is_empty() {
        text += &format!("; phases (ms): {}", phases.join(", "));
    }
    if p.plugin_ns > 0 {
        text += &format!("; plugins {:.2} ms", ms(u64::from(p.plugin_ns)));
    }
    let mut tracks: Vec<(usize, u32)> = p
        .track_ns
        .iter()
        .copied()
        .enumerate()
        .filter(|(_, ns)| *ns > 0)
        .collect();
    tracks.sort_by(|a, b| b.1.cmp(&a.1));
    if !tracks.is_empty() {
        let list: Vec<String> = tracks
            .iter()
            .take(3)
            .map(|(t, ns)| {
                let name = names.get(*t).cloned().unwrap_or_else(|| format!("track {}", t + 1));
                format!("{name} {:.2}", ms(u64::from(*ns)))
            })
            .collect();
        text += &format!("; slowest tracks (ms): {}", list.join(", "));
    }
    let commands = p.command_names();
    if !commands.is_empty() {
        text += &format!("; commands applied: {}", commands.join(", "));
    }
    text += &format!(
        "; allocations {} / frees {}; input FIFO {} frames waiting, {} taken",
        p.allocs, p.frees, p.fifo_frames, p.input_frames
    );
    let prev = &d.prev_profile;
    if matches!(d.lateness, Lateness::WakeUp | Lateness::Both) && prev.commands > 0 {
        text += &format!(
            "; the callback before it applied: {}",
            prev.command_names().join(", ")
        );
    }
    text += &format!(" - {}", cause(d));
    text
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
        counts_with_input(late_blocks, late_wakeups, overloads, 0, 0)
    }

    fn counts_with_input(
        late_blocks: u64,
        late_wakeups: u64,
        overloads: u64,
        input_lost: u64,
        input_late: u64,
    ) -> Counts {
        Counts {
            late_blocks,
            late_wakeups,
            overloads,
            input_lost,
            input_late,
        }
    }

    const MS: Duration = Duration::from_millis(1);

    /// Lost input frames (the input FIFO ran full) are an overrun of their own kind, so a jump
    /// inside a take can be matched to it in `session.json`; together with the driver's overload
    /// that caused them they are one overrun.
    #[test]
    fn lost_input_is_its_own_kind_and_joins_the_overload_that_caused_it() {
        let mut w = LoadWatch::default();
        let t0 = Instant::now();
        let r = reading(None, 0.0);
        let c = counts_with_input(0, 0, 1, 1, 0);
        assert!(w.observe(t0, c, &r, 497_000, || "14:57:43".into()).is_none());
        let done = w.observe(t0 + 70 * MS, c, &r, 498_000, || unreachable!()).unwrap();
        assert_eq!(done.kind_text(), "driver overload + input lost");
        assert_eq!(done.frame, 497_000, "the host's estimate");
        let c = counts_with_input(0, 0, 1, 1, 1);
        assert!(w.observe(t0 + 500 * MS, c, &r, 520_000, || "14:57:44".into()).is_none());
        let done = w.observe(t0 + 600 * MS, c, &r, 530_000, || unreachable!()).unwrap();
        assert_eq!(done.kind_text(), "input late");
        assert_eq!(w.incidents, 2);
    }

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
            details: Vec::new(),
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

    /// The overrun of 2026-11-21 17:26 as the clock saw it: one callback took 6 ms (a late
    /// block), so the next one started late (a late wake-up) and took 0.19 ms - 7 %. The old line
    /// quoted only that 7 %. Now the overrun carries both records, its load is the slow one's, and
    /// the line says which callback was slow, what it did and that the second only followed it.
    #[test]
    fn a_late_block_and_the_late_wake_up_it_causes_are_told_apart() {
        use looper_engine::load::{BlockProfile, LoadClock, Phase};
        const BUDGET: u64 = 2_666_666;
        let (mut clock, shared) = LoadClock::new(48_000);
        let mut details = clock.take_details().unwrap();
        let mut t = 0u64;
        for _ in 0..100 {
            clock.record(t, 80_000, 128, 0);
            t += BUDGET;
        }
        let mut slow = BlockProfile::default();
        slow.add_phase(Phase::Mix, 5_800_000);
        slow.add_phase(Phase::Commands, 150_000);
        slow.add_track(5, 5_700_000);
        slow.plugin_ns = 5_600_000;
        slow.note_command(6); // RestartLoop
        slow.note_command(0); // StartRecord
        slow.user_ns = 5_900_000;
        slow.system_ns = 20_000;
        slow.cpu_known = true;
        clock.record_profiled(t, 6_000_000, 128, &slow);
        t += 6_100_000;
        clock.record(t, 190_000, 128, 0);
        let reading = shared.read();
        let got: Vec<LateDetail> = std::iter::from_fn(|| details.pop().ok()).collect();
        assert_eq!(got.len(), 2);
        assert_eq!(got[0].lateness, Lateness::Block);
        assert_eq!(got[1].lateness, Lateness::WakeUp);

        let mut w = LoadWatch::default();
        let t0 = Instant::now();
        let c = counts(1, 1, 2);
        assert!(w.observe_with(t0, c, &reading, &got, 0, || "17:26:35".into()).is_none());
        let done = w.observe_with(t0 + 70 * MS, c, &reading, &[], 0, || unreachable!()).unwrap();
        assert_eq!(done.frame, 100 * 128, "the slow callback's frame");
        assert!((done.percent - 225.0).abs() < 1.0, "{}", done.percent);
        let names: Vec<String> = ["theme", "bass", "theme_b", "bass_b", "chorus_guitar", "Drums"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        let first = describe(&done.details[0], &names);
        assert!(first.starts_with("late block at frame 12800: took 6.00 ms (225 %"), "{first}");
        assert!(first.contains("Drums 5.70"), "{first}");
        assert!(first.contains("StartRecord, RestartLoop"), "{first}");
        assert!(first.contains("inside plugins"), "{first}");
        let second = describe(&done.details[1], &names);
        eprintln!("{first}\n{second}");
        assert!(second.starts_with("late wake-up"), "{second}");
        assert!(second.contains("the callback before it ran 6.00 ms"), "{second}");
    }
}
