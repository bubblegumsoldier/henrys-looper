//! Phase 2: the loop core - several tracks, unlimited layers.
//!
//! ```text
//!  Steuer-Thread (main)                       Audio-Thread (Ausgabe-Callback)
//!  ┌───────────────────────┐  Kommandos    ┌────────────────────────────────────┐
//!  │ Tastatur, Anzeige,    │ ────────────► │ EngineCore::process                │
//!  │ Allokation, Planung   │ ◄──────────── │  Zeitachse · Klick · Tracks · Ebenen│
//!  └───────────────────────┘  Status       └────────────────────────────────────┘
//!            │  leere Ebenen-Puffer (Vec<f32>)      ▲ Eingangs-FIFO
//!            ├─────────────────────────────►        │ (ganze Frames)
//!            └◄──── gebrauchte Puffer ──────        │
//!                                          Eingangs-Callback
//! ```
//!
//! Module map:
//!
//! * [`timeline`] - sample position <-> bar/beat, rounding-error free. Pure arithmetic.
//! * [`command`] - timestamped commands, the status snapshot, the lock-free channels in both
//!   directions and the control thread's stock of empty layer buffers.
//! * [`track`] - tracks, their layers, and the grid all layers of a track share.
//! * [`fx`] - the effect chain of one track: high-pass, three-band EQ, compressor, tempo-
//!   synchronous delay, reverb, plus the ready-made presets. Sits in the playback and monitoring
//!   path only; what is recorded stays dry.
//! * [`schedule`] - a user action becomes timed commands: which grid a take snaps to, how much
//!   head start it gets, and how far away it still is. Shared by the CLI and the desktop app.
//! * [`metro`] - the click, as a pure function of position.
//! * [`process`] - the audio-thread brain, including the latency-compensation derivation.
//! * [`live`] - cpal wiring, keyboard and terminal display.
//! * [`calibrate`] - checks the compensation value against real hardware via a loopback cable.
//! * `sim` / `tests` (test builds only) - the offline audio run and the accuracy proofs.

pub mod calibrate;
pub mod command;
pub mod fx;
pub mod live;
pub mod metro;
pub mod process;
pub mod schedule;
pub mod timeline;
pub mod track;

#[cfg(test)]
mod sim;
#[cfg(test)]
mod tests;
