//! Crash forensics (ADR 0072, Slice 0): turn a vanished window into data.
//!
//! Release builds are `windows_subsystem = "windows"` — no console, and until
//! this module existed no panic hook and no log file, so every crash was
//! invisible. The GPU-contention crash class has TWO killer species and they
//! need different catchers:
//!
//! - **Rust panics** (the eframe/wgpu device-loss class): a panic hook writes
//!   `workspace/crash.log` — timestamp, payload, location, backtrace, and the
//!   in-flight job label the pipeline worker maintains.
//! - **C `abort()`** (whisper.cpp/ggml's `GGML_ASSERT` on a failed CUDA
//!   allocation): NOT a Rust panic — a panic hook never runs. msvcrt raises
//!   `SIGABRT` first, so a signal handler writes a marker (fixed bytes + the
//!   job label, via a pre-opened CRT fd — no allocation in the handler).
//!
//! Alongside: when the process has no stderr (the no-console GUI),
//! `GGML_ASSERT`'s own message, wgpu validation text, and inherited-stderr
//! children (the llm-judge's llama.cpp load logs) all write into the void —
//! so stderr is redirected into `workspace/logs/stderr.log` at both the Win32
//! level (`SetStdHandle`, covers Rust `eprintln!` and child inheritance) and
//! the CRT level (`_open_osfhandle` + `_dup2` onto fd 2, covers C `fprintf`).
//! A `tracing` file layer (`workspace/logs/yc.log`) plus the worker's
//! `job start:` / `job done:` journal lines make even a handler-less vanish
//! attributable from the log tail.
//!
//! `YC_TEST_PANIC=1` / `YC_TEST_ABORT=1` inject each killer species right
//! after init (GUI never opens), so the bars are testable without a real
//! crash: ADR 0072 bars (0a)/(0b).

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicI32, AtomicU8, AtomicUsize, Ordering};
use std::sync::OnceLock;

// The CRT + kernel32 surface this module needs — declared directly (the app
// deliberately carries no libc/windows-sys dependency for four functions).
#[cfg(windows)]
#[link(name = "kernel32")]
extern "system" {
    fn GetStdHandle(std_handle: u32) -> isize;
    fn SetStdHandle(std_handle: u32, handle: isize) -> i32;
}
#[cfg(windows)]
extern "C" {
    /// Wrap a Win32 HANDLE as a CRT fd (takes ownership of the handle).
    fn _open_osfhandle(osfhandle: isize, flags: i32) -> i32;
    /// Make `fd2` refer to `fd1`'s file — used to force CRT fd 2 (stderr).
    fn _dup2(fd1: i32, fd2: i32) -> i32;
    /// CRT write — the only thing safe enough to call inside the SIGABRT
    /// handler (no allocation, no locks of ours).
    fn _write(fd: i32, buf: *const core::ffi::c_void, count: u32) -> i32;
    /// msvcrt `signal` — SIGABRT support is exactly why it exists here.
    fn signal(signum: i32, handler: extern "C" fn(i32)) -> isize;
    fn abort() -> !;
}

#[cfg(windows)]
const STD_ERROR_HANDLE: u32 = -12i32 as u32;
#[cfg(windows)]
const INVALID_HANDLE_VALUE: isize = -1;
/// msvcrt's SIGABRT (ucrt `signal.h`).
#[cfg(windows)]
const SIGABRT: i32 = 22;
/// `_O_APPEND` for `_open_osfhandle`.
#[cfg(windows)]
const O_APPEND: i32 = 0x0008;

/// Where `crash.log` lives — set by [`init`], read by the panic hook.
static CRASH_LOG: OnceLock<PathBuf> = OnceLock::new();
/// Pre-opened CRT fd onto `crash.log` for the SIGABRT handler. -1 = unavailable.
static CRASH_FD: AtomicI32 = AtomicI32::new(-1);

/// The in-flight job label, readable from a signal handler: fixed atomic
/// bytes + a length published AFTER the bytes (store 0 first so a torn read
/// sees a short label, never garbage).
const JOB_CAP: usize = 192;
static JOB_LEN: AtomicUsize = AtomicUsize::new(0);
#[allow(clippy::declare_interior_mutable_const)]
static JOB_BYTES: [AtomicU8; JOB_CAP] = {
    #[allow(clippy::declare_interior_mutable_const)]
    const ZERO: AtomicU8 = AtomicU8::new(0);
    [ZERO; JOB_CAP]
};

/// Record what the pipeline worker is doing, for the crash writers. Called at
/// every job boundary (and "idle" when the queue runs dry) — cheap enough to
/// never think about.
pub fn set_current_job(label: &str) {
    let bytes = label.as_bytes();
    let n = bytes.len().min(JOB_CAP);
    JOB_LEN.store(0, Ordering::SeqCst);
    for (slot, b) in JOB_BYTES.iter().zip(bytes.iter().take(n)) {
        slot.store(*b, Ordering::Relaxed);
    }
    JOB_LEN.store(n, Ordering::SeqCst);
}

/// Snapshot of [`set_current_job`]'s label (allocating — panic hook use only;
/// the SIGABRT handler reads the atomics directly).
fn current_job() -> String {
    let n = JOB_LEN.load(Ordering::SeqCst).min(JOB_CAP);
    let bytes: Vec<u8> = JOB_BYTES.iter().take(n).map(|b| b.load(Ordering::Relaxed)).collect();
    String::from_utf8_lossy(&bytes).into_owned()
}

/// `SIGABRT` handler: a C `abort()` is killing the process (in this codebase
/// that means ggml/CUDA — GGML_ASSERT on a failed VRAM allocation is the
/// GPU-contention working-crash). Write a fixed marker + the job label to the
/// pre-opened `crash.log` fd and return — the CRT then finishes terminating.
#[cfg(windows)]
extern "C" fn on_sigabrt(_sig: i32) {
    let fd = CRASH_FD.load(Ordering::SeqCst);
    if fd < 0 {
        return;
    }
    let write = |bytes: &[u8]| unsafe {
        let _ = _write(fd, bytes.as_ptr().cast(), bytes.len() as u32);
    };
    write(b"\n=== ABORT (SIGABRT): a C abort() killed the process ===\n");
    write(b"class: ggml/CUDA assert is the known in-process abort()er (ADR 0072);\n");
    write(b"       its own message, if any, is in workspace/logs/stderr.log\n");
    write(b"job: ");
    let n = JOB_LEN.load(Ordering::SeqCst).min(JOB_CAP);
    // One byte at a time keeps the handler allocation-free; n is tiny.
    for slot in JOB_BYTES.iter().take(n) {
        let b = [slot.load(Ordering::Relaxed)];
        write(&b);
    }
    write(b"\n(wall-clock: see the tail of workspace/logs/yc.log)\n");
}

/// Wall-clock UTC stamp without a chrono dependency (Hinnant civil-from-days).
fn utc_stamp() -> String {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let (days, rem) = (secs / 86_400, secs % 86_400);
    let (h, m, s) = (rem / 3600, (rem % 3600) / 60, rem % 60);
    let z = days as i64 + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097) as u64;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe as i64 + era * 400 + i64::from(month <= 2);
    format!("{year:04}-{month:02}-{d:02} {h:02}:{m:02}:{s:02} UTC")
}

/// Append to `crash.log` via std (safe contexts only — the panic hook).
fn append_crash_log(text: &str) {
    use std::io::Write as _;
    let Some(path) = CRASH_LOG.get() else { return };
    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(path) {
        let _ = f.write_all(text.as_bytes());
    }
}

/// Redirect stderr into `logs/stderr.log` when the process has none (the
/// no-console GUI): Win32 level for Rust + child inheritance, CRT level for
/// C `fprintf(stderr)` (ggml). No-op when a real stderr exists (dev console,
/// piped runs) — those keep their live stream.
#[cfg(windows)]
fn redirect_stderr_if_headless(log_dir: &Path) {
    use std::os::windows::io::AsRawHandle as _;
    unsafe {
        let h = GetStdHandle(STD_ERROR_HANDLE);
        if h != 0 && h != INVALID_HANDLE_VALUE {
            return;
        }
        let path = log_dir.join("stderr.log");
        // Two independent handles on the same append-mode file: one becomes
        // the Win32 std handle, one is consumed by `_open_osfhandle` (which
        // takes ownership) for the CRT fd. Both leak deliberately — they must
        // live until the process dies.
        let Ok(win32) = std::fs::OpenOptions::new().create(true).append(true).open(&path) else {
            return;
        };
        SetStdHandle(STD_ERROR_HANDLE, win32.as_raw_handle() as isize);
        std::mem::forget(win32);
        if let Ok(crt) = std::fs::OpenOptions::new().create(true).append(true).open(&path) {
            let handle = crt.as_raw_handle() as isize;
            std::mem::forget(crt);
            let fd = _open_osfhandle(handle, O_APPEND);
            if fd >= 0 {
                let _ = _dup2(fd, 2);
            }
        }
    }
}

/// Install every forensic catcher and the tracing stack. Called once, first
/// thing in `main`, with the resolved workspace dir. Never fails — a broken
/// disk just means fewer catchers, and the app must still run.
pub fn init(workspace: &Path) {
    let log_dir = workspace.join("logs");
    let _ = std::fs::create_dir_all(&log_dir);

    #[cfg(windows)]
    redirect_stderr_if_headless(&log_dir);

    // Tracing: the pre-existing stdout layer (dev `cargo run` behavior
    // unchanged) plus a persistent file layer, one shared env filter.
    {
        use tracing_subscriber::layer::SubscriberExt as _;
        use tracing_subscriber::util::SubscriberInitExt as _;
        let filter = tracing_subscriber::EnvFilter::try_from_default_env()
            .unwrap_or_else(|_| "info".into());
        let registry = tracing_subscriber::registry()
            .with(filter)
            .with(tracing_subscriber::fmt::layer());
        match std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(log_dir.join("yc.log"))
        {
            Ok(file) => registry
                .with(
                    tracing_subscriber::fmt::layer()
                        .with_writer(std::sync::Arc::new(file))
                        .with_ansi(false),
                )
                .init(),
            Err(_) => registry.init(),
        }
    }

    let crash_path = workspace.join("crash.log");
    let _ = CRASH_LOG.set(crash_path.clone());
    set_current_job("startup (no job yet)");

    // Pre-open the CRT fd + install the SIGABRT catcher (the ggml class).
    #[cfg(windows)]
    unsafe {
        use std::os::windows::io::AsRawHandle as _;
        if let Ok(f) = std::fs::OpenOptions::new().create(true).append(true).open(&crash_path) {
            let handle = f.as_raw_handle() as isize;
            std::mem::forget(f);
            let fd = _open_osfhandle(handle, O_APPEND);
            if fd >= 0 {
                CRASH_FD.store(fd, Ordering::SeqCst);
                signal(SIGABRT, on_sigabrt);
            }
        }
    }

    // The panic hook (the eframe/wgpu class — and any other Rust panic).
    let prev = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let bt = std::backtrace::Backtrace::force_capture();
        let thread = std::thread::current();
        let entry = format!(
            "\n=== PANIC {} ===\njob: {}\nthread: {}\n{}\nbacktrace:\n{}\n",
            utc_stamp(),
            current_job(),
            thread.name().unwrap_or("<unnamed>"),
            info,
            bt,
        );
        append_crash_log(&entry);
        tracing::error!("panic (crash.log written): {info}");
        prev(info);
    }));

    tracing::info!("forensics armed: crash.log + logs/ under {}", workspace.display());
}

/// ADR 0072 bars (0a)/(0b): deliberately trigger each killer species so the
/// catchers are testable without a real crash. Checked right after [`init`];
/// the GUI never opens on an injection run.
pub fn run_test_injections() {
    let on = |v: std::result::Result<String, std::env::VarError>| {
        v.map(|s| s.trim() == "1").unwrap_or(false)
    };
    if on(std::env::var("YC_TEST_PANIC")) {
        set_current_job("YC_TEST_PANIC injection (bar 0a)");
        panic!("YC_TEST_PANIC: injected test panic - if you can read this in crash.log, bar 0a holds");
    }
    if on(std::env::var("YC_TEST_ABORT")) {
        set_current_job("YC_TEST_ABORT injection (bar 0b)");
        eprintln!("YC_TEST_ABORT: raising abort() - the SIGABRT marker should land in crash.log");
        #[cfg(windows)]
        unsafe {
            abort()
        }
        #[cfg(not(windows))]
        std::process::abort();
    }
}
