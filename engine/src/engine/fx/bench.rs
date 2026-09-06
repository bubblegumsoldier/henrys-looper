//! How much of the audio callback the effects cost.
//!
//! Five effects times eight tracks is a lot of arithmetic to fit into one buffer, and the reverb
//! is by far the biggest single item. The plan's own risk list says "Allokation oder Sperre im
//! Audio-Thread" is what breaks a looper; running out of *time* breaks it just as thoroughly, and
//! unlike a lock it does not show up in a code review. So it is measured.
//!
//! The budget is fixed by the buffer size the engine targets:
//!
//! ```text
//! 128 Frames / 48 000 Hz = 2.667 ms pro Callback
//! ```
//!
//! Everything the callback does - click, layer mixing, recording, and now the chains - has to fit
//! inside that. The measurement below reports nanoseconds per **frame** per effect and turns them
//! into a share of that budget, once for one track and once for the eight the engine allows. A
//! frame is two samples since the chain became stereo, so these numbers are directly comparable
//! with the mono ones they replace: they say what one instant of audio costs, which is what the
//! callback is billed for.
//!
//! Run it from a **release** build, where the numbers mean something:
//!
//! ```text
//! looper-engine fxbench
//! ```
//!
//! The unit test `bench_fits_into_the_callback_budget` runs the same code so a regression cannot
//! hide, but it only asserts a hard limit outside debug builds - `cargo test` compiles at
//! opt-level 1, where a reverb is several times slower than in the shipping binary and any
//! absolute threshold would either be useless or flaky.

use std::hint::black_box;
use std::time::Instant;

use super::super::frame::Frame;
use super::biquad::{Biquad, Coeffs};
use super::delay::{Delay, DelayNote};
use super::dynamics::{CompSettings, Compressor};
use super::reverb::Reverb;
use super::{Chain, FxPreset};

/// The buffer size phase 0 settled on.
pub const BUDGET_FRAMES: usize = 128;

/// One measured item.
#[derive(Clone, Debug)]
pub struct Row {
    pub name: &'static str,
    /// Nanoseconds per **frame**, i.e. per instant of audio. Since the chain is stereo throughout,
    /// a frame is two samples everywhere below - which is what makes this number comparable with
    /// the callback budget, since a callback has to produce frames rather than samples.
    pub ns_per_sample: f64,
}

impl Row {
    /// Share of one callback budget this costs for a single track, in percent.
    pub fn percent_of_budget(&self, sample_rate: u32) -> f64 {
        let budget_ns = BUDGET_FRAMES as f64 / sample_rate as f64 * 1e9;
        self.ns_per_sample * BUDGET_FRAMES as f64 / budget_ns * 100.0
    }
}

#[derive(Clone, Debug)]
pub struct Report {
    pub sample_rate: u32,
    pub samples: usize,
    pub rows: Vec<Row>,
}

impl Report {
    pub fn get(&self, name: &str) -> Option<&Row> {
        self.rows.iter().find(|r| r.name == name)
    }

    /// Callback budget in microseconds, for the printout.
    pub fn budget_us(&self) -> f64 {
        BUDGET_FRAMES as f64 / self.sample_rate as f64 * 1e6
    }
}

/// A signal that keeps every effect busy: a tone whose level swings well across a compressor
/// threshold, so no branch in the chain is left untaken.
///
/// Built once into a buffer rather than computed inside the timing loop. Two `sin` calls per
/// sample cost more than a whole biquad, and measuring them along with the effect would have made
/// every row of the table look the same.
fn test_signal(samples: usize, sample_rate: u32) -> Vec<f32> {
    (0..samples)
        .map(|i| {
            let t = i as f32 / sample_rate as f32;
            let envelope = 0.55 + 0.45 * (std::f32::consts::TAU * 3.0 * t).sin();
            envelope * (std::f32::consts::TAU * 220.0 * t).sin() * 0.7
        })
        .collect()
}

/// Time one effect over the whole signal, in nanoseconds per **frame**.
///
/// A frame is what a callback has to produce, and since the stereo rebuild it carries two samples
/// everywhere in the chain - so the numbers below are directly comparable with the callback budget
/// and with the mono measurement they replace, which is the comparison that matters.
fn time<F: FnMut(Frame) -> Frame>(signal: &[f32], mut f: F) -> f64 {
    // One warm-up pass, so the measurement does not include filling the caches with the delay
    // lines and the reverb buffers.
    for &x in signal.iter().take(signal.len() / 8) {
        black_box(f(Frame::mono(x)));
    }
    let started = Instant::now();
    for &x in signal {
        black_box(f(black_box(Frame::mono(x))));
    }
    started.elapsed().as_nanos() as f64 / signal.len() as f64
}

/// Measure every effect on its own and the whole chain, using `seconds` of audio per item.
pub fn measure(sample_rate: u32, seconds: f64) -> Report {
    let samples = (sample_rate as f64 * seconds).max(1.0) as usize;
    let signal = test_signal(samples, sample_rate);
    let mut rows = Vec::new();

    // One filter instance per channel, exactly as the chain holds them.
    let mut hp = [Biquad::new(Coeffs::high_pass(
        sample_rate,
        80.0,
        std::f32::consts::FRAC_1_SQRT_2,
    )); 2];
    rows.push(Row {
        name: "Hochpass",
        ns_per_sample: time(&signal, |x| {
            Frame::new(hp[0].process(x.l), hp[1].process(x.r))
        }),
    });

    let voice = FxPreset::Voice.settings();
    let mut bands: Vec<[Biquad; 2]> = voice
        .bands
        .iter()
        .map(|b| [Biquad::new(Coeffs::band(sample_rate, b.kind, b.hz, b.q, b.gain_db)); 2])
        .collect();
    rows.push(Row {
        name: "EQ (3 Baender)",
        ns_per_sample: time(&signal, |x| {
            let mut y = x;
            for b in bands.iter_mut() {
                y = Frame::new(b[0].process(y.l), b[1].process(y.r));
            }
            y
        }),
    });

    let mut comp = Compressor::new(sample_rate, CompSettings::default());
    rows.push(Row {
        name: "Kompressor",
        ns_per_sample: time(&signal, |x| comp.process(x)),
    });

    let mut delay = Delay::new(sample_rate, DelayNote::DottedEighth, 0.35, 0.25);
    delay.set_quarter_samples(sample_rate as f64 * 60.0 / 100.0);
    rows.push(Row {
        name: "Delay",
        ns_per_sample: time(&signal, |x| delay.process(x)),
    });

    let mut reverb = Reverb::new(sample_rate, 0.62, 0.45, 0.18);
    reverb.snap_mix(0.18);
    rows.push(Row {
        name: "Hall",
        ns_per_sample: time(&signal, |x| reverb.process(x)),
    });

    // The chain as the preset leaves it: high-pass, EQ, compressor, reverb - no delay.
    let mut chain = Chain::new(sample_rate);
    chain.load_preset(FxPreset::Voice);
    chain.set_quarter_samples(sample_rate as f64 * 60.0 / 100.0);
    rows.push(Row {
        name: "Kette \"stimme\"",
        ns_per_sample: time(&signal, |x| chain.process(x)),
    });

    // Everything on, including the delay: the most expensive state a track can be in.
    let mut full = Chain::new(sample_rate);
    full.load_preset(FxPreset::Voice);
    full.set_enabled(super::FxSlot::Delay, true);
    full.set_quarter_samples(sample_rate as f64 * 60.0 / 100.0);
    rows.push(Row {
        name: "Kette komplett",
        ns_per_sample: time(&signal, |x| full.process(x)),
    });

    // A track that is silent for longer than its tail. This is the state seven of eight tracks
    // are in most of the time, and the reason the idle check in `Chain::process` exists.
    let mut idle = Chain::new(sample_rate);
    idle.load_preset(FxPreset::Voice);
    idle.set_quarter_samples(sample_rate as f64 * 60.0 / 100.0);
    for _ in 0..(idle.tail_samples() as usize + 16) {
        idle.process(Frame::SILENT);
    }
    let started = Instant::now();
    for _ in 0..samples {
        black_box(idle.process(black_box(Frame::SILENT)));
    }
    rows.push(Row {
        name: "Kette still (Leerlauf)",
        ns_per_sample: started.elapsed().as_nanos() as f64 / samples as f64,
    });

    let mut bypassed = Chain::new(sample_rate);
    bypassed.load_preset(FxPreset::Voice);
    bypassed.set_bypass(true);
    for _ in 0..sample_rate as usize {
        bypassed.process(Frame::SILENT);
    }
    rows.push(Row {
        name: "Kette umgangen",
        ns_per_sample: time(&signal, |x| bypassed.process(x)),
    });

    Report {
        sample_rate,
        samples,
        rows,
    }
}

/// German table for the terminal.
pub fn print(report: &Report) {
    println!(
        "Rechenlast der Effektkette, gemessen ueber {:.1} s Audio je Zeile bei {} Hz.",
        report.samples as f64 / report.sample_rate as f64,
        report.sample_rate
    );
    println!(
        "Budget eines Callbacks: {} Frames = {:.3} ms\n",
        BUDGET_FRAMES,
        report.budget_us() / 1000.0
    );
    println!(
        "{:<26} {:>12} {:>14} {:>16}",
        "", "ns/Frame", "% je Track", "% bei 8 Tracks"
    );
    for row in &report.rows {
        let per_track = row.percent_of_budget(report.sample_rate);
        println!(
            "{:<26} {:>12.2} {:>13.2}% {:>15.2}%",
            row.name,
            row.ns_per_sample,
            per_track,
            per_track * 8.0
        );
    }
    println!(
        "\nDie Zeile \"Kette komplett\" ist der ungueltigste Fall: alle fuenf Effekte an, Signal\n\
         durchgehend ueber der Kompressorschwelle. Ein stiller Track kostet die Zeile\n\
         \"Kette still\", weil die Kette nach dem Ausklingen gar nicht mehr rechnet."
    );
}

/// `fxbench` subcommand. Opens no device and makes no sound.
pub fn cmd_fxbench(sample_rate: u32, seconds: f64) -> Result<(), String> {
    if sample_rate < 8_000 || sample_rate > 192_000 {
        return Err(format!(
            "Samplerate {sample_rate} liegt ausserhalb von 8000..192000."
        ));
    }
    if !(0.05..=60.0).contains(&seconds) {
        return Err("--seconds muss zwischen 0.05 und 60 liegen.".to_string());
    }
    let report = measure(sample_rate, seconds);
    print(&report);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The measurement itself has to work, and in a build where the numbers mean something it has
    /// to fit. Half a second of audio per row keeps `cargo test` quick.
    #[test]
    fn bench_fits_into_the_callback_budget() {
        let report = measure(48_000, 0.25);
        assert!(report.rows.len() >= 8);
        for row in &report.rows {
            assert!(
                row.ns_per_sample.is_finite() && row.ns_per_sample >= 0.0,
                "{} liefert keine brauchbare Zeit",
                row.name
            );
        }

        let full = report.get("Kette komplett").expect("Zeile fehlt");
        let idle = report.get("Kette still (Leerlauf)").expect("Zeile fehlt");
        let bypassed = report.get("Kette umgangen").expect("Zeile fehlt");
        // Build-independent claims: the two cheap states really are cheap compared with the
        // expensive one. This is what the early exits in `Chain::process` are for, and it holds
        // whatever the optimisation level is.
        assert!(
            idle.ns_per_sample * 4.0 < full.ns_per_sample,
            "Leerlauf {:.2} ns gegen volle Kette {:.2} ns - die Leerlauf-Erkennung greift nicht",
            idle.ns_per_sample,
            full.ns_per_sample
        );
        assert!(
            bypassed.ns_per_sample * 4.0 < full.ns_per_sample,
            "Bypass {:.2} ns gegen volle Kette {:.2} ns",
            bypassed.ns_per_sample,
            full.ns_per_sample
        );

        // The absolute budget only in an optimised build - see the module comment.
        if !cfg!(debug_assertions) {
            let eight = full.percent_of_budget(48_000) * 8.0;
            assert!(
                eight < 50.0,
                "acht volle Ketten brauchen {eight:.1} % des Callback-Budgets"
            );
        }
    }
}
