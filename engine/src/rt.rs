//! Real-time diagnostics for the audio thread: an allocation counter that only counts inside an
//! audio callback, and the calling thread's own CPU time.
//!
//! # The allocation counter
//!
//! [`CountingAlloc`] is a global allocator that forwards everything to the system allocator and,
//! on the way, asks one thread-local flag: is this thread inside an audio callback right now
//! ([`arm`])? If it is, the allocation, the reallocation or the free is counted - in plain
//! thread-local cells, so counting costs a TLS read and an add, and nothing is shared. The
//! callback reads its own counters ([`counts`]) and hands the difference on with its block
//! profile (`crate::load`).
//!
//! It is **always on**: outside a callback the cost is one thread-local read and a branch per
//! allocation, which no measurement can find, and inside a correct callback nothing allocates. The
//! desktop app and the CLI install it as their global allocator, the engine's own test binary does
//! too - which is what lets `rt_tests` run the owner's whole score and demand zero.
//!
//! Why an allocation in the callback matters even when it is fast on average: `malloc` takes a
//! lock shared with every other thread of the process, may ask the kernel for fresh pages (a
//! system call, page faults on first touch) and `free` of a large block hands pages back
//! (`munmap`, which takes the process's VM map lock exclusively). Any of them can stall the audio
//! thread for milliseconds while another thread is busy in the allocator.
//!
//! # Thread CPU time
//!
//! [`thread_times`] reads the calling thread's own user and system CPU time (macOS:
//! `thread_info(THREAD_EXTENDED_INFO)`, a Mach call that does not block - it takes this thread's
//! own, uncontended lock - and costs well under a microsecond). Around a callback it answers the
//! question the wall clock cannot: of the time a late block took, how much did the thread spend
//! **computing** (user), **in the kernel** (system: page faults, system calls) and **not running at
//! all** (wall minus both: preempted, or blocked on a lock or on I/O such as a page-in).

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;

/// Allocations and frees counted on this thread while it was armed.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct RtCounts {
    pub allocs: u64,
    pub frees: u64,
    /// Bytes requested by those allocations.
    pub bytes: u64,
}

impl RtCounts {
    /// What happened between `earlier` and `self`.
    pub fn since(self, earlier: RtCounts) -> RtCounts {
        RtCounts {
            allocs: self.allocs.wrapping_sub(earlier.allocs),
            frees: self.frees.wrapping_sub(earlier.frees),
            bytes: self.bytes.wrapping_sub(earlier.bytes),
        }
    }

    pub fn is_zero(self) -> bool {
        self.allocs == 0 && self.frees == 0
    }
}

thread_local! {
    // `const` and without a destructor: no lazy initialisation, no registration, nothing that
    // could itself allocate - which matters, because the allocator reads these.
    static ARMED: Cell<bool> = const { Cell::new(false) };
    static ALLOCS: Cell<u64> = const { Cell::new(0) };
    static FREES: Cell<u64> = const { Cell::new(0) };
    static BYTES: Cell<u64> = const { Cell::new(0) };
}

/// Marks the current thread as "inside an audio callback" until dropped.
pub struct Armed {
    was: bool,
}

impl Drop for Armed {
    #[inline]
    fn drop(&mut self) {
        let was = self.was;
        let _ = ARMED.try_with(|a| a.set(was));
    }
}

/// Count every allocation and free on this thread until the guard is dropped. Nests.
#[inline]
pub fn arm() -> Armed {
    let was = ARMED.try_with(|a| a.replace(true)).unwrap_or(false);
    Armed { was }
}

/// Whether this thread is armed right now.
#[inline]
pub fn armed() -> bool {
    ARMED.try_with(Cell::get).unwrap_or(false)
}

/// This thread's counters (only ever moved while it was armed).
#[inline]
pub fn counts() -> RtCounts {
    RtCounts {
        allocs: ALLOCS.try_with(Cell::get).unwrap_or(0),
        frees: FREES.try_with(Cell::get).unwrap_or(0),
        bytes: BYTES.try_with(Cell::get).unwrap_or(0),
    }
}

#[inline(always)]
fn note_alloc(size: usize) {
    if armed() {
        let _ = ALLOCS.try_with(|c| c.set(c.get() + 1));
        let _ = BYTES.try_with(|c| c.set(c.get() + size as u64));
        #[cfg(test)]
        trace::note("alloc");
    }
}

#[inline(always)]
fn note_free() {
    if armed() {
        let _ = FREES.try_with(|c| c.set(c.get() + 1));
        #[cfg(test)]
        trace::note("free");
    }
}

/// Tests only: keep the backtraces of the first few counted events, so a failing real-time test
/// says *where* the callback allocated.
#[cfg(test)]
pub mod trace {
    use std::cell::{Cell, RefCell};

    const KEEP: usize = 6;

    thread_local! {
        static ON: Cell<bool> = const { Cell::new(false) };
        static KEPT: RefCell<Vec<String>> = const { RefCell::new(Vec::new()) };
    }

    /// Start (or stop) keeping backtraces on this thread.
    pub fn keep(on: bool) {
        ON.with(|c| c.set(on));
    }

    /// The backtraces kept so far on this thread.
    pub fn kept() -> Vec<String> {
        KEPT.with(|k| k.borrow().clone())
    }

    pub(super) fn note(what: &str) {
        if !ON.try_with(Cell::get).unwrap_or(false) {
            return;
        }
        // Capturing allocates: disarmed (and not tracing) while it runs.
        ON.with(|c| c.set(false));
        let was = super::ARMED.with(|a| a.replace(false));
        let full = KEPT.with(|k| k.borrow().len() >= KEEP);
        if !full {
            let bt = std::backtrace::Backtrace::force_capture().to_string();
            // The frames of our own crate are what matters.
            let ours: Vec<&str> = bt
                .lines()
                .filter(|l| l.contains("looper_engine::") || l.contains("engine/src"))
                .take(24)
                .collect();
            let text = format!("{what}:\n{}", ours.join("\n"));
            KEPT.with(|k| k.borrow_mut().push(text));
        }
        super::ARMED.with(|a| a.set(was));
        ON.with(|c| c.set(true));
    }
}

/// The system allocator, counting while the calling thread is armed. See the module comment.
pub struct CountingAlloc;

unsafe impl GlobalAlloc for CountingAlloc {
    #[inline]
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        note_alloc(layout.size());
        unsafe { System.alloc(layout) }
    }

    #[inline]
    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        note_alloc(layout.size());
        unsafe { System.alloc_zeroed(layout) }
    }

    #[inline]
    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        note_free();
        unsafe { System.dealloc(ptr, layout) }
    }

    #[inline]
    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        // A reallocation is an allocation and a free as far as the audio thread is concerned.
        note_alloc(new_size);
        note_free();
        unsafe { System.realloc(ptr, layout, new_size) }
    }
}

/// CPU time the calling thread has used so far, split into user and system time, in ns.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ThreadTimes {
    pub user_ns: u64,
    pub system_ns: u64,
    /// The thread's current scheduling priority (macOS: 97 for a real-time audio thread, 31 for a
    /// default thread). 0 where unknown.
    pub priority: i32,
    /// Its scheduling policy: 1 timeshare, 2 round robin, 4 FIFO; time-constraint threads report
    /// their underlying policy. 0 where unknown.
    pub policy: i32,
}

impl ThreadTimes {
    pub fn since(self, earlier: ThreadTimes) -> ThreadTimes {
        ThreadTimes {
            user_ns: self.user_ns.saturating_sub(earlier.user_ns),
            system_ns: self.system_ns.saturating_sub(earlier.system_ns),
            priority: self.priority,
            policy: self.policy,
        }
    }

    pub fn cpu_ns(self) -> u64 {
        self.user_ns + self.system_ns
    }
}

#[cfg(target_os = "macos")]
mod mach {
    #[repr(C)]
    pub struct ThreadExtendedInfo {
        pub pth_user_time: u64,
        pub pth_system_time: u64,
        pub pth_cpu_usage: i32,
        pub pth_policy: i32,
        pub pth_run_state: i32,
        pub pth_flags: i32,
        pub pth_sleep_time: i32,
        pub pth_curpri: i32,
        pub pth_priority: i32,
        pub pth_maxpriority: i32,
        pub pth_name: [u8; 64],
    }

    pub const THREAD_EXTENDED_INFO: i32 = 5;
    pub const THREAD_EXTENDED_INFO_COUNT: u32 =
        (std::mem::size_of::<ThreadExtendedInfo>() / std::mem::size_of::<u32>()) as u32;

    unsafe extern "C" {
        pub fn pthread_self() -> usize;
        /// The Mach port of a pthread, without taking a new reference (unlike `mach_thread_self`,
        /// whose port right would have to be given back on every call).
        pub fn pthread_mach_thread_np(thread: usize) -> u32;
        pub fn thread_info(thread: u32, flavor: i32, info: *mut i32, count: *mut u32) -> i32;
    }
}

/// The calling thread's CPU times, or `None` where the platform does not say. Real-time safe on
/// macOS: one Mach call, no allocation.
#[cfg(target_os = "macos")]
#[inline]
pub fn thread_times() -> Option<ThreadTimes> {
    let mut info = std::mem::MaybeUninit::<mach::ThreadExtendedInfo>::zeroed();
    let mut count = mach::THREAD_EXTENDED_INFO_COUNT;
    // SAFETY: `info` is a zeroed buffer of exactly the size `count` announces; the port of our own
    // thread is valid for as long as the thread runs.
    let rc = unsafe {
        let port = mach::pthread_mach_thread_np(mach::pthread_self());
        mach::thread_info(
            port,
            mach::THREAD_EXTENDED_INFO,
            info.as_mut_ptr().cast(),
            &mut count,
        )
    };
    if rc != 0 {
        return None;
    }
    // SAFETY: the kernel filled it (and it was zeroed before).
    let info = unsafe { info.assume_init() };
    Some(ThreadTimes {
        user_ns: info.pth_user_time,
        system_ns: info.pth_system_time,
        priority: info.pth_curpri,
        policy: info.pth_policy,
    })
}

#[cfg(not(target_os = "macos"))]
#[inline]
pub fn thread_times() -> Option<ThreadTimes> {
    None
}

/// The macOS quality-of-service classes a helper thread can ask for. None of them can preempt the
/// audio callback (CoreAudio runs it as a time-constraint thread, above every QoS class); what
/// they decide is which helper gets the CPU first and on which cores.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Qos {
    /// The thread that feeds the audio thread its commands, MIDI and buffers on a 4 ms tick.
    UserInteractive,
    /// Work nobody waits for from one moment to the next. (Not used for the recorder: its writes
    /// would be throttled behind other disk traffic.)
    Utility,
}

/// Give the calling thread a QoS class (macOS; a no-op elsewhere). Returns whether it took.
pub fn set_qos(qos: Qos) -> bool {
    #[cfg(target_os = "macos")]
    {
        unsafe extern "C" {
            fn pthread_set_qos_class_self_np(class: u32, relative_priority: i32) -> i32;
        }
        let class = match qos {
            Qos::UserInteractive => 0x21,
            Qos::Utility => 0x11,
        };
        // SAFETY: changes only the calling thread's own scheduling class.
        unsafe { pthread_set_qos_class_self_np(class, 0) == 0 }
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = qos;
        false
    }
}

/// Page faults of the whole process so far: (minor - served without I/O, major - read from disk).
/// A system call; for tests and the control thread, **not** for the callback.
#[cfg(unix)]
pub fn process_faults() -> Option<(u64, u64)> {
    #[repr(C)]
    struct Timeval {
        sec: i64,
        usec: i64,
    }
    #[repr(C)]
    struct Rusage {
        utime: Timeval,
        stime: Timeval,
        maxrss: i64,
        ixrss: i64,
        idrss: i64,
        isrss: i64,
        minflt: i64,
        majflt: i64,
        rest: [i64; 8],
    }
    unsafe extern "C" {
        fn getrusage(who: i32, usage: *mut Rusage) -> i32;
    }
    let mut usage = std::mem::MaybeUninit::<Rusage>::zeroed();
    // SAFETY: RUSAGE_SELF (0) and a zeroed struct of the layout the C library fills.
    let rc = unsafe { getrusage(0, usage.as_mut_ptr()) };
    if rc != 0 {
        return None;
    }
    // SAFETY: filled by the call above.
    let usage = unsafe { usage.assume_init() };
    Some((usage.minflt.max(0) as u64, usage.majflt.max(0) as u64))
}

#[cfg(not(unix))]
pub fn process_faults() -> Option<(u64, u64)> {
    None
}

/// How much of the calling thread's stack [`prefault_stack`] touches: 128 KB, or half the stack if
/// that is smaller.
pub const STACK_PREFAULT: usize = 128 * 1024;

/// Touch the next [`STACK_PREFAULT`] bytes of the calling thread's stack below the current frame,
/// so that a deep call path that runs for the first time much later - the first MIDI take of a
/// run, a plugin's rarely used branch - does not take a page fault for its stack on the way.
/// Called once, on the first audio callback; no allocation, no system call.
#[inline(never)]
pub fn prefault_stack() {
    let size = stack_size().unwrap_or(0);
    // Without a known size only a little is touched: overrunning a small stack would crash.
    let budget = if size == 0 {
        16 * 1024
    } else {
        STACK_PREFAULT.min(size / 2)
    };
    touch_stack(budget);
}

#[inline(never)]
fn touch_stack(bytes: usize) {
    const CHUNK: usize = 8 * 1024;
    let mut probe = [0u8; CHUNK];
    for i in (0..CHUNK).step_by(1024) {
        *std::hint::black_box(&mut probe[i]) = 1;
    }
    std::hint::black_box(&mut probe);
    if bytes > CHUNK {
        touch_stack(bytes - CHUNK);
    }
    std::hint::black_box(&probe);
}

/// The calling thread's stack size, where the platform says.
#[cfg(target_os = "macos")]
fn stack_size() -> Option<usize> {
    unsafe extern "C" {
        fn pthread_self() -> usize;
        fn pthread_get_stacksize_np(thread: usize) -> usize;
    }
    // SAFETY: plain queries about the calling thread.
    let size = unsafe { pthread_get_stacksize_np(pthread_self()) };
    (size > 0).then_some(size)
}

#[cfg(not(target_os = "macos"))]
fn stack_size() -> Option<usize> {
    None
}

/// A zeroed buffer whose memory really exists. `vec![0.0; n]` asks the system for zeroed pages,
/// which it hands out lazily: the first write to each page then faults - in the audio thread, if
/// that is where the buffer is first written. Writing one zero per page here, on the thread that
/// allocates, makes every page resident before the buffer is handed on.
pub fn resident_zeros(len: usize) -> Vec<f32> {
    let mut buffer = vec![0.0f32; len];
    touch(&mut buffer);
    buffer
}

/// Write every page of `buffer` once (with the value already there), so none of them faults on
/// its first use later. 4 KB steps: the smallest page size there is.
pub fn touch<T: Copy>(buffer: &mut [T]) {
    let step = (4096 / std::mem::size_of::<T>().max(1)).max(1);
    for i in (0..buffer.len()).step_by(step) {
        let v = buffer[i];
        *std::hint::black_box(&mut buffer[i]) = v;
    }
}

/// The same for the room a vector has **beyond** its length - what `Vec::with_capacity` reserves
/// and the audio thread later pushes into. The length stays as it is.
pub fn touch_spare<T>(buffer: &mut Vec<T>) {
    let step = (4096 / std::mem::size_of::<T>().max(1)).max(1);
    let spare = buffer.spare_capacity_mut();
    for i in (0..spare.len()).step_by(step) {
        // Zero bytes into uninitialised room: nothing is read, nothing becomes part of the vector.
        *std::hint::black_box(&mut spare[i]) = std::mem::MaybeUninit::zeroed();
    }
}

/// Make every slot of a fresh lock-free queue resident: write zero bytes into all of its (still
/// uninitialised) room on the thread that creates it, without committing anything - the queue stays
/// empty. `rtrb` allocates its ring uninitialised, so without this the producer - often the audio
/// thread - takes a page fault every few kilobytes until the ring has gone round once (the
/// recorder's queue: 20 MB, one fault per block for its first four seconds; the status queue: one
/// every seventh snapshot; the MIDI clips' news: one every few hundred notes).
pub fn prefault_ring<T>(tx: &mut rtrb::Producer<T>) {
    let n = tx.slots();
    if let Ok(mut chunk) = tx.write_chunk_uninit(n) {
        let (a, b) = chunk.as_mut_slices();
        for half in [a, b] {
            let step = (4096 / std::mem::size_of::<T>().max(1)).max(1);
            for i in (0..half.len()).step_by(step) {
                *std::hint::black_box(&mut half[i]) = std::mem::MaybeUninit::zeroed();
            }
        }
        // Dropped uncommitted: nothing becomes readable.
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_an_armed_thread_counts() {
        let before = counts();
        let v: Vec<u8> = Vec::with_capacity(100);
        drop(v);
        assert_eq!(counts(), before, "not armed: nothing counted");
        let g = arm();
        let v: Vec<u8> = Vec::with_capacity(100);
        drop(v);
        drop(g);
        let d = counts().since(before);
        assert_eq!((d.allocs, d.frees), (1, 1));
        assert!(d.bytes >= 100);
        assert!(!armed());
    }

    #[test]
    fn another_thread_is_not_counted() {
        let _g = arm();
        let before = counts();
        std::thread::spawn(|| {
            let _ = vec![0u8; 64];
        })
        .join()
        .unwrap();
        // `spawn` itself allocates on this thread (the thread's packet); the vector in the other
        // thread must not show up here, so only bound what spawning costs.
        let d = counts().since(before);
        assert!(d.allocs < 16, "{d:?}");
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn thread_times_move_with_work_and_cost_little() {
        let a = thread_times().expect("thread_info");
        let mut x = 0u64;
        for i in 0..5_000_000u64 {
            x = x.wrapping_mul(6364136223846793005).wrapping_add(i);
        }
        std::hint::black_box(x);
        let b = thread_times().unwrap();
        assert!(b.since(a).user_ns > 0, "{a:?} {b:?}");
        let t = std::time::Instant::now();
        for _ in 0..1000 {
            std::hint::black_box(thread_times());
        }
        let per = t.elapsed().as_nanos() / 1000;
        eprintln!("thread_times: {per} ns per call");
        assert!(per < 20_000, "{per} ns per call");
    }
}
