mod js;
mod state;
mod pty;
mod component;

pub use component::TerminalPanel;
pub use state::{inject_shell_command, set_theme};