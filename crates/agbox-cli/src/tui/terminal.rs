//! Best-effort terminal restoration guard.

use std::{
    io::{self, Stdout},
    panic::PanicHookInfo,
};

use crossterm::{
    cursor::{Hide, Show},
    execute,
    terminal::{EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode},
};

type PanicHook = Box<dyn Fn(&PanicHookInfo<'_>) + Send + Sync + 'static>;

/// Restores raw mode and the alternate screen if setup completed.
#[derive(Debug)]
pub struct TerminalGuard {
    active: bool,
}

/// Restores a terminal before emitting a deliberately redacted panic notice.
/// The TUI owns this process, so replacing the process hook for its lifetime
/// avoids accidentally printing a work summary, source path, or evidence
/// payload while the terminal is still in raw mode.
pub struct PanicHookGuard {
    previous: Option<PanicHook>,
}

impl std::fmt::Debug for PanicHookGuard {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("PanicHookGuard")
            .finish_non_exhaustive()
    }
}

impl PanicHookGuard {
    #[must_use]
    pub fn install() -> Self {
        let previous = std::panic::take_hook();
        std::panic::set_hook(Box::new(|_| {
            restore_terminal();
            eprintln!("agbox tui stopped unexpectedly; terminal restored");
        }));
        Self {
            previous: Some(previous),
        }
    }
}

impl Drop for PanicHookGuard {
    fn drop(&mut self) {
        if !std::thread::panicking()
            && let Some(previous) = self.previous.take()
        {
            std::panic::set_hook(previous);
        }
    }
}

impl TerminalGuard {
    /// Enables terminal mode atomically enough for interactive use.
    ///
    /// # Errors
    ///
    /// Returns terminal I/O errors after restoring raw mode on partial setup.
    pub fn enter(stdout: &mut Stdout) -> io::Result<Self> {
        enable_raw_mode()?;
        if let Err(error) = execute!(stdout, EnterAlternateScreen, Hide) {
            let _ = disable_raw_mode();
            let _ = execute!(stdout, Show, LeaveAlternateScreen);
            return Err(error);
        }
        Ok(Self { active: true })
    }
}

impl Drop for TerminalGuard {
    fn drop(&mut self) {
        if self.active {
            restore_terminal();
        }
    }
}

fn restore_terminal() {
    let mut stdout = io::stdout();
    // Each operation is intentionally attempted independently: a partially
    // detached terminal must still regain a visible cursor and canonical input
    // mode after a draw or panic-path failure.
    let _ = execute!(stdout, Show, LeaveAlternateScreen);
    let _ = disable_raw_mode();
}
