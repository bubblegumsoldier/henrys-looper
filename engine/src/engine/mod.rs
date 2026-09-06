//! Phase 1: the loop core.
//!
//! ```text
//!  Steuer-Thread (main)                       Audio-Thread (Ausgabe-Callback)
//!  ┌───────────────────────┐  Kommandos    ┌────────────────────────────────────┐
//!  │ Tastatur, Anzeige,    │ ────────────► │ EngineCore::process                │
//!  │ Allokation, Planung   │ ◄──────────── │  Zeitachse · Klick · Loop · Aufnahme│
//!  └───────────────────────┘  Status       └────────────────────────────────────┘
//!            │  Loop-Puffer (Vec<f32>)              ▲ Eingangs-FIFO
//!            └─────────────────────────────►        │
//!                                          Eingangs-Callback
//! ```
//!
//! Module map:
//!
//! * [`timeline`] - sample position <-> bar/beat, rounding-error free. Pure arithmetic.
//! * [`command`] - timestamped commands and the lock-free channels in both directions.
//! * [`track`] - the loop buffer and its geometry.
//! * [`metro`] - the click, as a pure function of position.
//! * [`process`] - the audio-thread brain, including the latency-compensation derivation.
//! * [`live`] - cpal wiring, keyboard and terminal display.
//! * `sim` / `tests` (test builds only) - the offline audio run and the accuracy proofs.

pub mod command;
pub mod live;
pub mod metro;
pub mod process;
pub mod timeline;
pub mod track;

#[cfg(test)]
mod sim;
#[cfg(test)]
mod tests;
