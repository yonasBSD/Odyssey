//! Terminal lifecycle: raw-mode setup, alternate screen, and cleanup.

use crossterm::event::{DisableMouseCapture, EnableMouseCapture};
use crossterm::execute;
use crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};
use log::debug;
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;
use std::io::{self, Stdout};

const USER_ENV_VAR: &str = "USER";
const USERNAME_ENV_VAR: &str = "USERNAME";

fn resolve_user_name_from_env(
    user: Result<String, std::env::VarError>,
    username: Result<String, std::env::VarError>,
) -> String {
    user.or(username).unwrap_or_else(|_| "user".to_string())
}

/// Enter raw mode, switch to the alternate screen, and enable mouse capture.
pub fn setup_terminal() -> anyhow::Result<Terminal<CrosstermBackend<Stdout>>> {
    debug!("setting up terminal");
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen, EnableMouseCapture)?;
    Ok(Terminal::new(CrosstermBackend::new(stdout))?)
}

/// Restore the terminal to its original state.
///
/// This should always be called before the process exits, even on error.
pub fn restore_terminal(terminal: &mut Terminal<CrosstermBackend<Stdout>>) -> anyhow::Result<()> {
    debug!("restoring terminal");
    disable_raw_mode()?;
    execute!(
        terminal.backend_mut(),
        LeaveAlternateScreen,
        DisableMouseCapture
    )?;
    terminal.show_cursor()?;
    Ok(())
}

/// Resolve the current UNIX user name, with a safe fallback.
pub fn resolve_user_name() -> String {
    resolve_user_name_from_env(std::env::var(USER_ENV_VAR), std::env::var(USERNAME_ENV_VAR))
}

#[cfg(test)]
mod tests {
    use super::resolve_user_name_from_env;
    use pretty_assertions::assert_eq;
    use std::env::VarError;

    #[test]
    fn resolve_user_name_prefers_user_env_var() {
        assert_eq!(
            resolve_user_name_from_env(Ok("alice".to_string()), Ok("bob".to_string())),
            "alice"
        );
    }

    #[test]
    fn resolve_user_name_falls_back_to_username_env_var() {
        assert_eq!(
            resolve_user_name_from_env(Err(VarError::NotPresent), Ok("bob".to_string())),
            "bob"
        );
    }

    #[test]
    fn resolve_user_name_uses_default_when_env_is_missing() {
        assert_eq!(
            resolve_user_name_from_env(Err(VarError::NotPresent), Err(VarError::NotPresent)),
            "user"
        );
    }
}
