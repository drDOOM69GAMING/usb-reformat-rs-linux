//! Asking for root, the same way the DNS app does: one `pkexec` prompt, no
//! password stored anywhere, no setuid binary shipped.
//!
//! The non privileged half (the GUI, or `--list`) never needs root at all. The
//! moment a real disk operation starts, the GUI re-runs this same executable
//! through `pkexec` with `--run`, and only that worker holds root. That mirrors
//! the Windows design, where the UI ran as the logged in user and only a small
//! helper touched the physical drive.

use std::path::PathBuf;
use std::process::{Child, Command, Stdio};

use crate::util::{have, is_root};

/// Absolute path to this executable, which `pkexec` insists on.
pub fn self_exe() -> Result<PathBuf, String> {
    std::env::current_exe().map_err(|e| format!("cannot find my own path: {e}"))
}

pub fn have_pkexec() -> bool {
    have("pkexec")
}

/// Start the privileged worker in the background and hand back the child so the
/// GUI can wait for it. Output goes to the log file the worker is given, not to
/// this process's stdout.
pub fn spawn_elevated(args: &[String]) -> Result<Child, String> {
    let exe = self_exe()?;

    let mut command = if is_root() {
        let mut c = Command::new(exe);
        c.args(args);
        c
    } else {
        if !have_pkexec() {
            return Err(
                "pkexec is not installed, so I cannot ask for permission to touch a disk".into(),
            );
        }
        let mut c = Command::new("pkexec");
        c.arg(exe).args(args);
        c
    };

    command
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| format!("could not start the privileged worker: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn self_exe_is_absolute() {
        let exe = self_exe().unwrap();
        assert!(exe.is_absolute(), "{exe:?} should be absolute");
    }
}
