//! Lock-free level metering and small statistics helpers.

use std::sync::atomic::{AtomicU32, Ordering};

/// Peak level shared between an audio callback and the main thread.
///
/// For non-negative floats the IEEE-754 bit pattern orders the same way as the value itself, so a
/// single `fetch_max` on the raw bits is enough - no locks, no allocation in the callback.
pub struct PeakMeter {
    bits: AtomicU32,
}

impl Default for PeakMeter {
    fn default() -> Self {
        Self::new()
    }
}

impl PeakMeter {
    pub const fn new() -> Self {
        Self {
            bits: AtomicU32::new(0),
        }
    }

    /// Callback side: record a block peak (absolute value).
    #[inline]
    pub fn push(&self, peak: f32) {
        let clean = if peak.is_finite() { peak.max(0.0) } else { 0.0 };
        self.bits.fetch_max(clean.to_bits(), Ordering::Relaxed);
    }

    /// Main-thread side: read and reset.
    pub fn take(&self) -> f32 {
        f32::from_bits(self.bits.swap(0, Ordering::Relaxed))
    }
}

pub fn dbfs(linear: f32) -> f32 {
    if linear > 0.0 {
        20.0 * linear.log10()
    } else {
        f32::NEG_INFINITY
    }
}

pub fn fmt_dbfs(linear: f32) -> String {
    let db = dbfs(linear);
    if db.is_finite() {
        format!("{db:>6.1} dBFS")
    } else {
        "  -inf dBFS".to_string()
    }
}

pub struct Stats {
    pub count: usize,
    pub min: f64,
    pub max: f64,
    pub median: f64,
    pub mean: f64,
    pub stddev: f64,
}

impl Stats {
    pub fn of(values: &[f64]) -> Option<Stats> {
        if values.is_empty() {
            return None;
        }
        let mut sorted = values.to_vec();
        sorted.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
        let n = sorted.len();
        let median = if n % 2 == 1 {
            sorted[n / 2]
        } else {
            (sorted[n / 2 - 1] + sorted[n / 2]) / 2.0
        };
        let mean = sorted.iter().sum::<f64>() / n as f64;
        // Sample standard deviation; with a single run there is no spread to report.
        let stddev = if n > 1 {
            (sorted.iter().map(|v| (v - mean).powi(2)).sum::<f64>() / (n - 1) as f64).sqrt()
        } else {
            0.0
        };
        Some(Stats {
            count: n,
            min: sorted[0],
            max: sorted[n - 1],
            median,
            mean,
            stddev,
        })
    }
}

/// Print one statistics block, values given in samples, converted to milliseconds via `rate`.
pub fn print_stats(title: &str, samples: &[f64], rate: u32) {
    let ms = |s: f64| s * 1000.0 / rate as f64;
    match Stats::of(samples) {
        None => println!("{title}: keine gueltigen Messwerte."),
        Some(s) => {
            println!("{title} ({} Laeufe):", s.count);
            println!(
                "  Minimum:           {:>10.1} Samples   {:>8.3} ms",
                s.min,
                ms(s.min)
            );
            println!(
                "  Median:            {:>10.1} Samples   {:>8.3} ms",
                s.median,
                ms(s.median)
            );
            println!(
                "  Mittelwert:        {:>10.1} Samples   {:>8.3} ms",
                s.mean,
                ms(s.mean)
            );
            println!(
                "  Maximum:           {:>10.1} Samples   {:>8.3} ms",
                s.max,
                ms(s.max)
            );
            println!(
                "  Standardabweichung:{:>10.1} Samples   {:>8.3} ms",
                s.stddev,
                ms(s.stddev)
            );
        }
    }
}
