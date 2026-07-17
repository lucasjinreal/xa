//! Crash diagnostics and terminal recovery for interactive terminal surfaces.

use std::backtrace::Backtrace;
use std::fs::{File, OpenOptions};
use std::io::{self, Write};
use std::panic::{self, PanicHookInfo};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Once;

static TUI_ACTIVE: AtomicBool = AtomicBool::new(false);
static INSTALL_HOOK: Once = Once::new();

/// Marks an interval in which XA owns the terminal. While active, a panic is
/// restored to the normal screen before its diagnostic location is printed.
pub(crate) struct TuiGuard;

impl TuiGuard {
    pub(crate) fn enter() -> Self {
        install_panic_hook();
        TUI_ACTIVE.store(true, Ordering::SeqCst);
        Self
    }
}

impl Drop for TuiGuard {
    fn drop(&mut self) {
        TUI_ACTIVE.store(false, Ordering::SeqCst);
    }
}

/// Save a non-panic error that escaped a TUI event loop.
pub(crate) fn report_error(error: &(dyn std::error::Error + 'static)) {
    restore_terminal();
    let path = write_log("TUI error", &format_error_chain(error));
    eprintln!(
        "xa TUI exited with an error. Diagnostic log: {}",
        path.display()
    );
}

fn install_panic_hook() {
    INSTALL_HOOK.call_once(|| {
        let previous = panic::take_hook();
        panic::set_hook(Box::new(move |info| {
            if TUI_ACTIVE.load(Ordering::SeqCst) {
                restore_terminal();
                let path = write_log("TUI panic", &format_panic(info));
                eprintln!("\nxa TUI crashed. Diagnostic log: {}", path.display());
            }
            previous(info);
        }));
    });
}

fn restore_terminal() {
    let _ = crossterm::terminal::disable_raw_mode();
    let mut stdout = io::stdout();
    let _ = crossterm::execute!(
        stdout,
        crossterm::event::DisableBracketedPaste,
        crossterm::terminal::LeaveAlternateScreen,
        crossterm::cursor::Show,
    );
    let _ = stdout.flush();
}

fn format_panic(info: &PanicHookInfo<'_>) -> String {
    let payload = if let Some(message) = info.payload().downcast_ref::<&str>() {
        (*message).to_string()
    } else if let Some(message) = info.payload().downcast_ref::<String>() {
        message.clone()
    } else {
        "non-string panic payload".to_string()
    };
    let location = info
        .location()
        .map(|location| {
            format!(
                "{}:{}:{}",
                location.file(),
                location.line(),
                location.column()
            )
        })
        .unwrap_or_else(|| "unknown location".to_string());
    format!(
        "panic: {payload}\nlocation: {location}\n\nbacktrace:\n{}",
        Backtrace::force_capture()
    )
}

fn format_error_chain(error: &(dyn std::error::Error + 'static)) -> String {
    let mut out = format!("error: {error}");
    let mut source = error.source();
    while let Some(error) = source {
        out.push_str(&format!("\ncaused by: {error}"));
        source = error.source();
    }
    out.push_str(&format!("\n\nbacktrace:\n{}", Backtrace::force_capture()));
    out
}

fn write_log(kind: &str, details: &str) -> PathBuf {
    let timestamp = chrono::Local::now().format("%Y%m%d-%H%M%S-%3f");
    let base = format!("xa-tui-crash-{timestamp}-{}", std::process::id());
    for suffix in 0..1000 {
        let name = if suffix == 0 {
            format!("{base}.log")
        } else {
            format!("{base}-{suffix}.log")
        };
        let path = std::env::temp_dir().join(name);
        match OpenOptions::new().write(true).create_new(true).open(&path) {
            Ok(mut file) => {
                let _ = write!(
                    file,
                    "XA {kind}\ntime: {}\npid: {}\n\n{details}\n",
                    chrono::Local::now().to_rfc3339(),
                    std::process::id(),
                );
                return path;
            }
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(error) => return write_fallback(kind, details, &error),
        }
    }
    write_fallback(
        kind,
        details,
        &io::Error::new(io::ErrorKind::AlreadyExists, "too many log name collisions"),
    )
}

fn write_fallback(kind: &str, details: &str, error: &io::Error) -> PathBuf {
    let path = std::env::temp_dir().join(format!("xa-tui-crash-{}.log", std::process::id()));
    if let Ok(mut file) = File::create(&path) {
        let _ = write!(
            file,
            "XA {kind}\nlog creation warning: {error}\n\n{details}\n"
        );
    }
    path
}
