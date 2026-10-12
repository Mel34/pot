//! Terminal ownership: always restored, whatever happens.

use std::io::{self, Stdout};
use std::thread;

use crossterm::{
    cursor::Show,
    execute,
    terminal::{EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode},
};
use ratatui::{Terminal, backend::CrosstermBackend};

pub type Tui = Terminal<CrosstermBackend<Stdout>>;

/// Puts the terminal into TUI mode and puts it back when dropped, including
/// on early returns and (together with [`install_panic_hook`]) on panics.
pub struct TerminalGuard {
    terminal: Tui,
}

impl TerminalGuard {
    pub fn enter() -> io::Result<Self> {
        enable_raw_mode()?;

        let mut stdout = io::stdout();

        if let Err(error) = execute!(stdout, EnterAlternateScreen) {
            let _ = disable_raw_mode();
            return Err(error);
        }

        match Terminal::new(CrosstermBackend::new(stdout)) {
            Ok(terminal) => Ok(Self { terminal }),
            Err(error) => {
                restore();
                Err(error)
            }
        }
    }

    pub fn terminal(&mut self) -> &mut Tui {
        &mut self.terminal
    }

    /// Temporarily restore normal terminal mode so sudo can prompt interactively.
    pub fn suspend(&mut self) -> io::Result<()> {
        restore();
        Ok(())
    }

    /// Re-enter TUI raw mode and alternate screen.
    pub fn resume(&mut self) -> io::Result<()> {
        enable_raw_mode()?;
        execute!(io::stdout(), EnterAlternateScreen)?;
        self.terminal.clear()
    }


}

impl Drop for TerminalGuard {
    fn drop(&mut self) {
        restore();
    }
}

fn restore() {
    let _ = disable_raw_mode();
    let _ = execute!(io::stdout(), LeaveAlternateScreen, Show);
}

/// Panics on the main thread restore the terminal before the message is
/// printed (otherwise it is lost in the alternate screen). Panics on helper
/// threads are only logged: printing would corrupt the display.
pub fn install_panic_hook() {
    let default_hook = std::panic::take_hook();

    std::panic::set_hook(Box::new(move |info| {
        let current = thread::current();

        crate::diag::log(&format!(
            "panic in thread {:?}: {info}",
            current.name().unwrap_or("<unnamed>")
        ));

        if current.name() == Some("main") {
            restore();
            default_hook(info);
        }
    }));
}
