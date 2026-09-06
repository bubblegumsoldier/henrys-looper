//! Diagnostics in a file, because `stdout` does not exist in the release build.
//!
//! `windows_subsystem = "windows"` means the process has no console, and on Windows a `println!`
//! against a null standard handle does not quietly do nothing - it panics. That matters beyond our
//! own logging: the engine library prints its reports with `println!`, and the calibration is
//! nothing *but* such a report.
//!
//! So instead of replacing every `println!`, the process's standard output and standard error are
//! redirected into a file at startup, with `SetStdHandle`. Rust's `Stdout` asks Windows for the
//! handle on every write, so this catches everything printed anywhere in the process, including
//! the whole `calibrate` report, with no dependency and no second sink to keep in sync.
//!
//! Set `LOOPER_LOG_STDOUT=1` to skip the redirect and keep the console (useful under
//! `cargo tauri dev`).
//!
//! Nothing here may be called from an audio callback: it writes to a file.

use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Instant, SystemTime, UNIX_EPOCH};

/// Only true once a sink is known to exist. Every log call checks it, so a failed redirect in a
/// window-subsystem build degrades to silence instead of a panic.
static ENABLED: AtomicBool = AtomicBool::new(false);
static PATH: OnceLock<PathBuf> = OnceLock::new();
static START: OnceLock<Instant> = OnceLock::new();

/// Whether diagnostics are being written at all.
pub fn enabled() -> bool {
    ENABLED.load(Ordering::Relaxed)
}

/// Path of the log file. Set even when writing to it failed, so the message can name it.
pub fn path() -> PathBuf {
    PATH.get().cloned().unwrap_or_default()
}

/// Write one line. Silently does nothing while no sink exists.
pub fn write(line: &str) {
    if !enabled() {
        return;
    }
    let secs = START.get().map(|s| s.elapsed().as_secs_f64()).unwrap_or(0.0);
    println!("[{secs:9.3}] {line}");
}

/// `log!("... {}", value)` - same syntax as `println!`, but into the log file.
macro_rules! log {
    ($($arg:tt)*) => { $crate::logfile::write(&format!($($arg)*)) };
}
pub(crate) use log;

/// Open the log file in `dir` and point the process's standard handles at it.
///
/// Returns the path in every case; whether anything is actually written is [`enabled`].
pub fn init(dir: &Path) -> PathBuf {
    let _ = START.set(Instant::now());
    let file = dir.join("henrys-looper.log");
    let _ = PATH.set(file.clone());

    if std::env::var("LOOPER_LOG_STDOUT").is_ok_and(|v| v == "1") {
        // Keep the console: only useful when one exists.
        ENABLED.store(has_console(), Ordering::Relaxed);
    } else {
        let _ = std::fs::create_dir_all(dir);
        let redirected = redirect_std_handles(&file);
        ENABLED.store(redirected || has_console(), Ordering::Relaxed);
    }

    write(&format!(
        "Henrys Looper {} gestartet, {}",
        env!("CARGO_PKG_VERSION"),
        utc_now()
    ));
    write(&format!("Logdatei: {}", file.display()));
    file
}

fn utc_now() -> String {
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0) as i64;
    format_utc(secs)
}

/// Epoch seconds as `YYYY-MM-DD hh:mm:ss UTC`.
///
/// The civil-date conversion is Howard Hinnant's `civil_from_days`: shift the era to start in
/// March so the leap day is the last day of the year, and the month-length pattern becomes a
/// closed formula. Cheaper than a date dependency for the one line it is used in.
fn format_utc(secs: i64) -> String {
    let days = secs.div_euclid(86_400);
    let time = secs.rem_euclid(86_400);
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    format!(
        "{y:04}-{m:02}-{d:02} {:02}:{:02}:{:02} UTC",
        time / 3_600,
        (time / 60) % 60,
        time % 60
    )
}

#[cfg(windows)]
mod win {
    use std::ffi::c_void;

    pub type Handle = *mut c_void;

    pub const INVALID_HANDLE_VALUE: Handle = -1isize as Handle;
    pub const STD_OUTPUT_HANDLE: u32 = -11i32 as u32;
    pub const STD_ERROR_HANDLE: u32 = -12i32 as u32;
    /// Append-only write access. A handle opened this way always writes at the end of the file,
    /// no matter what the file pointer says - which is exactly what shared use needs.
    pub const FILE_APPEND_DATA: u32 = 0x0004;
    pub const FILE_SHARE_READ: u32 = 0x0000_0001;
    pub const FILE_SHARE_WRITE: u32 = 0x0000_0002;
    pub const FILE_SHARE_DELETE: u32 = 0x0000_0004;
    pub const OPEN_ALWAYS: u32 = 4;
    pub const FILE_ATTRIBUTE_NORMAL: u32 = 0x0000_0080;

    unsafe extern "system" {
        pub fn CreateFileW(
            file_name: *const u16,
            desired_access: u32,
            share_mode: u32,
            security_attributes: *mut c_void,
            creation_disposition: u32,
            flags_and_attributes: u32,
            template_file: Handle,
        ) -> Handle;
        pub fn SetStdHandle(std_handle: u32, handle: Handle) -> i32;
        pub fn GetStdHandle(std_handle: u32) -> Handle;
    }
}

#[cfg(windows)]
fn has_console() -> bool {
    let handle = unsafe { win::GetStdHandle(win::STD_OUTPUT_HANDLE) };
    !handle.is_null() && handle != win::INVALID_HANDLE_VALUE
}

#[cfg(windows)]
fn redirect_std_handles(file: &Path) -> bool {
    use std::os::windows::ffi::OsStrExt;

    let wide: Vec<u16> = file
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect();
    // Safety: `wide` is a NUL-terminated UTF-16 path that outlives the call, and every other
    // argument is a documented constant or a null pointer where the API allows one.
    let handle = unsafe {
        win::CreateFileW(
            wide.as_ptr(),
            win::FILE_APPEND_DATA,
            win::FILE_SHARE_READ | win::FILE_SHARE_WRITE | win::FILE_SHARE_DELETE,
            std::ptr::null_mut(),
            win::OPEN_ALWAYS,
            win::FILE_ATTRIBUTE_NORMAL,
            std::ptr::null_mut(),
        )
    };
    if handle == win::INVALID_HANDLE_VALUE || handle.is_null() {
        return false;
    }
    // The handle is deliberately never closed: it has to stay valid for the life of the process,
    // and the process ending closes it.
    let out = unsafe { win::SetStdHandle(win::STD_OUTPUT_HANDLE, handle) };
    let err = unsafe { win::SetStdHandle(win::STD_ERROR_HANDLE, handle) };
    out != 0 && err != 0
}

#[cfg(not(windows))]
fn has_console() -> bool {
    true
}

#[cfg(not(windows))]
fn redirect_std_handles(_file: &Path) -> bool {
    // Only Windows loses its console to the subsystem flag; elsewhere stdout stays usable.
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Against dates looked up in a calendar, including both leap-day cases the formula is built
    /// around (2000 is a leap year, 2100 is not).
    #[test]
    fn epoch_seconds_become_the_right_calendar_date() {
        assert_eq!(format_utc(0), "1970-01-01 00:00:00 UTC");
        assert_eq!(format_utc(86_399), "1970-01-01 23:59:59 UTC");
        assert_eq!(format_utc(951_782_400), "2000-02-29 00:00:00 UTC");
        assert_eq!(format_utc(1_709_164_800), "2024-02-29 00:00:00 UTC");
        assert_eq!(format_utc(4_107_542_400), "2100-03-01 00:00:00 UTC");
        assert_eq!(format_utc(1_788_726_896), "2026-09-06 20:34:56 UTC");
    }

    /// Logging must be inert until it has been initialised - a `println!` without a standard
    /// handle panics, and that would take the whole window down.
    #[test]
    fn writing_before_init_does_nothing() {
        if !enabled() {
            write("das darf nicht knallen");
        }
    }
}
