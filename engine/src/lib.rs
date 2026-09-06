//! looper-engine - the loop core plus the measuring tools it grew out of.
//!
//! The crate has two front ends and one body:
//!
//! * `src/main.rs` - the CLI (`list`, `thru`, `click`, `latency`, `soak`, `live`, `calibrate`).
//!   It is the measuring instrument and stays exactly as it was.
//! * `app/src-tauri` - the desktop app. It uses [`audio`] to open devices and [`engine`] to run
//!   the loop core, and puts a window in front of it instead of a keyboard.
//!
//! Real-time rule for everything below: no allocation, no locking, no logging, no formatting and
//! no file access inside an audio callback. Callbacks only touch atomics, pre-allocated buffers
//! and lock-free ring buffers.

pub mod audio;
pub mod click;
pub mod duplex;
pub mod engine;
pub mod latency;
pub mod meter;
pub mod score;
pub mod soak;

use std::sync::mpsc::{Receiver, channel};

/// Non-blocking "press Enter to stop": a helper thread owns stdin, the main loop polls the
/// channel. Never touched from an audio callback.
pub fn wait_for_enter() -> Receiver<()> {
    let (tx, rx) = channel();
    std::thread::spawn(move || {
        let mut line = String::new();
        let _ = std::io::stdin().read_line(&mut line);
        let _ = tx.send(());
    });
    rx
}
