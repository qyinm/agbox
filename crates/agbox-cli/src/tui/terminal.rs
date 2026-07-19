//! Best-effort terminal restoration guard.

use std::io::{self, Stdout};

use crossterm::{
    cursor::{Hide, Show},
    execute,
    terminal::{EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode},
};

/// Restores raw mode and the alternate screen if setup completed.
#[derive(Debug)]
pub struct TerminalGuard {
    active: bool,
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
            let mut stdout = io::stdout();
            // Each operation is intentionally attempted independently: a
            // partially detached terminal must still regain a visible cursor
            // and canonical input mode after a draw or panic-path failure.
            let _ = execute!(stdout, Show, LeaveAlternateScreen);
            let _ = disable_raw_mode();
        }
    }
}
