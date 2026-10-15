//! Small process and formatting helpers.
//!
//! The Windows build drove everything through raw device handles. Here almost
//! every primitive is a small, well known command line tool, so this module is
//! the whole "system layer": run a command, capture output, format a size.

use std::path::{Path, PathBuf};
use std::process::Command;

/// Run a command and return its stdout, or the tool's own stderr as the error.
pub fn run(program: &str, args: &[&str]) -> Result<String, String> {
    let out = Command::new(program)
        .args(args)
        .output()
        .map_err(|e| format!("could not run {program}: {e}"))?;
    if !out.status.success() {
        let err = String::from_utf8_lossy(&out.stderr).trim().to_string();
        return Err(if err.is_empty() {
            format!("{program} exited with {:?}", out.status.code())
        } else {
            err
        });
    }
    Ok(String::from_utf8_lossy(&out.stdout).to_string())
}

/// Run a command, discarding stdout, returning only success / failure.
pub fn run_quiet(program: &str, args: &[&str]) -> Result<(), String> {
    run(program, args).map(|_| ())
}

/// Run a command with a script fed to its stdin. `sfdisk` takes the partition
/// table this way.
pub fn run_with_input(program: &str, args: &[&str], input: &str) -> Result<String, String> {
    use std::io::Write;
    use std::process::Stdio;

    let mut child = Command::new(program)
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("could not run {program}: {e}"))?;

    child
        .stdin
        .as_mut()
        .ok_or_else(|| format!("{program}: no stdin"))?
        .write_all(input.as_bytes())
        .map_err(|e| format!("{program}: could not write its input: {e}"))?;

    let out = child
        .wait_with_output()
        .map_err(|e| format!("{program}: {e}"))?;

    if !out.status.success() {
        let err = String::from_utf8_lossy(&out.stderr).trim().to_string();
        return Err(if err.is_empty() {
            format!("{program} exited with {:?}", out.status.code())
        } else {
            err
        });
    }
    Ok(String::from_utf8_lossy(&out.stdout).to_string())
}

/// Run a command with extra environment variables set. The bundled NTFS
/// formatter needs this so it can find the library it was unpacked beside.
pub fn run_env(program: &Path, args: &[&str], envs: &[(&str, &str)]) -> Result<String, String> {
    let mut command = Command::new(program);
    command.args(args);
    for (key, value) in envs {
        command.env(key, value);
    }

    let out = command
        .output()
        .map_err(|e| format!("could not run {}: {e}", program.display()))?;
    if !out.status.success() {
        let err = String::from_utf8_lossy(&out.stderr).trim().to_string();
        return Err(if err.is_empty() {
            format!("{} exited with {:?}", program.display(), out.status.code())
        } else {
            err
        });
    }
    Ok(String::from_utf8_lossy(&out.stdout).to_string())
}

/// The full path to a tool on PATH, if it is there and executable.
pub fn which(program: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path)
        .map(|dir| dir.join(program))
        .find(|candidate| candidate.is_file() && is_executable(candidate))
}

/// Is this tool on PATH?
pub fn have(program: &str) -> bool {
    which(program).is_some()
}

fn is_executable(path: &std::path::Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(path)
        .map(|m| m.permissions().mode() & 0o111 != 0)
        .unwrap_or(false)
}

/// Sizes are shown in powers of 1000, the same units drive vendors use, so a
/// "500 GB" stick does not read as 465.
pub fn human_size(bytes: u64) -> String {
    const UNITS: [&str; 6] = ["B", "KB", "MB", "GB", "TB", "PB"];
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1000.0 && unit < UNITS.len() - 1 {
        value /= 1000.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{bytes} B")
    } else {
        format!("{value:.1} {}", UNITS[unit])
    }
}

/// Is the current process uid 0?
pub fn is_root() -> bool {
    // geteuid through libc is available via std's metadata trick for free, but
    // the simplest portable check is the real uid reported by /proc.
    std::fs::read_to_string("/proc/self/status")
        .ok()
        .and_then(|s| {
            s.lines()
                .find(|l| l.starts_with("Uid:"))
                .and_then(|l| l.split_whitespace().nth(2).map(str::to_string))
        })
        .map(|uid| uid == "0")
        .unwrap_or(false)
}

/// The package that provides a formatting or system tool, for install hints.
pub fn package_for(tool: &str) -> Option<&'static str> {
    match tool {
        "mkfs.exfat" => Some("exfatprogs"),
        "mkfs.vfat" => Some("dosfstools"),
        "mkfs.ntfs" | "mkntfs" => Some("ntfsprogs"),
        "mkfs.ext4" => Some("e2fsprogs"),
        "lsblk" | "findmnt" | "wipefs" | "sfdisk" | "partprobe" | "umount" | "blockdev" => {
            Some("util-linux")
        }
        "pkexec" => Some("polkit"),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn human_sizes_use_decimal_units() {
        assert_eq!(human_size(0), "0 B");
        assert_eq!(human_size(999), "999 B");
        assert_eq!(human_size(1000), "1.0 KB");
        assert_eq!(human_size(500_000_000_000), "500.0 GB");
        assert_eq!(human_size(2_000_000_000_000), "2.0 TB");
    }

    #[test]
    fn formatting_tools_name_the_package_that_provides_them() {
        assert_eq!(package_for("mkfs.ntfs"), Some("ntfsprogs"));
        assert_eq!(package_for("mkntfs"), Some("ntfsprogs"));
        assert_eq!(package_for("mkfs.exfat"), Some("exfatprogs"));
        assert_eq!(package_for("mkfs.vfat"), Some("dosfstools"));
        assert_eq!(package_for("mkfs.ext4"), Some("e2fsprogs"));
        assert_eq!(package_for("not-a-tool"), None);
    }

    #[test]
    fn have_and_which_agree() {
        assert_eq!(have("sh"), which("sh").is_some());
        assert!(!have("definitely-not-a-real-tool-xyz"));
        assert_eq!(which("definitely-not-a-real-tool-xyz"), None);
    }
}
