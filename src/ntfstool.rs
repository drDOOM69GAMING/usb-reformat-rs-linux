//! Where the NTFS formatter comes from.
//!
//! On Windows everything sat in the box. On Linux the usual arrangement is that
//! `mkntfs` comes from the `ntfsprogs` package, but this program is often copied
//! around as a single file, so a copy is baked into the binary as well. A system
//! `mkfs.ntfs` or `mkntfs` is preferred when it is present, and the baked copy is
//! used otherwise. Either way NTFS shows up as an option that works.
//!
//! Unpacking only ever happens in the privileged worker, into a directory that
//! `mkdtemp` created for root with mode 0700, so nothing on a user writable path
//! is executed as root.

use std::ffi::{CStr, CString, OsStr};
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};

use crate::util;

/// The baked-in formatter and the shared library it links against.
mod bundled {
    include!(concat!(env!("OUT_DIR"), "/embedded_ntfs.rs"));
}

/// A runnable NTFS formatter: the program, and the directory to find its
/// library in when it came out of the binary.
pub struct Formatter {
    program: PathBuf,
    library_dir: Option<PathBuf>,
}

impl Formatter {
    /// Run the formatter, returning its output or a readable error.
    ///
    /// A freshly written executable can report "text file busy" if another
    /// thread forked while it was being written and the child inherited the
    /// write handle. That is a momentary condition, so it is retried briefly
    /// before being reported.
    pub fn run(&self, args: &[String]) -> Result<String, String> {
        let borrowed: Vec<&str> = args.iter().map(String::as_str).collect();
        let library = self
            .library_dir
            .as_ref()
            .map(|dir| dir.to_string_lossy().to_string());

        let mut last = String::new();
        for attempt in 0..20 {
            let result = match &library {
                Some(path) => util::run_env(&self.program, &borrowed, &[("LD_LIBRARY_PATH", path)]),
                None => util::run_env(&self.program, &borrowed, &[]),
            };

            match result {
                Ok(out) => return Ok(out),
                Err(e) if e.contains("Text file busy") && attempt < 19 => {
                    last = e;
                    std::thread::sleep(std::time::Duration::from_millis(50));
                }
                Err(e) => return Err(e),
            }
        }
        Err(last)
    }
}

/// Is an NTFS formatter available, from the system or from this binary?
pub fn available() -> bool {
    system_tool().is_some() || !bundled::MKNTFS.is_empty()
}

/// The `mkfs.ntfs` or `mkntfs` the system provides, if any.
fn system_tool() -> Option<PathBuf> {
    ["mkfs.ntfs", "mkntfs"].into_iter().find_map(util::which)
}

/// A formatter ready to run: the system's when there is one, otherwise the copy
/// unpacked from this binary.
pub fn resolve() -> Result<Formatter, String> {
    if let Some(program) = system_tool() {
        return Ok(Formatter {
            program,
            library_dir: None,
        });
    }

    if bundled::MKNTFS.is_empty() {
        return Err(
            "no NTFS formatter is available. Install 'ntfsprogs' (it provides mkntfs) \
             or use a build that bundles one."
                .to_string(),
        );
    }

    let dir = unpack()?;
    Ok(Formatter {
        program: dir.join("mkntfs"),
        library_dir: Some(dir),
    })
}

/// Write the bundled formatter and library into a fresh private directory.
fn unpack() -> Result<PathBuf, String> {
    let dir = private_dir()?;
    write(&dir.join("libntfs-3g.so.91"), bundled::LIBNTFS, 0o644)?;
    write(&dir.join("mkntfs"), bundled::MKNTFS, 0o755)?;
    Ok(dir)
}

/// A directory only root can reach. `/run` is preferred because it is already
/// root only; `/tmp` is the fallback, and `mkdtemp` still gives us a directory
/// that is ours and mode 0700.
fn private_dir() -> Result<PathBuf, String> {
    for base in ["/run", "/tmp"] {
        let template = format!("{base}/usb-reformat-tools-XXXXXX");
        let Ok(template) = CString::new(template) else {
            continue;
        };
        let mut buffer = template.into_bytes_with_nul();

        // Safety: `buffer` is a NUL terminated, writable, mutable string, which
        // is exactly what `mkdtemp` asks for. On success it returns a pointer to
        // the same buffer with the XXXXXX replaced.
        let created = unsafe { libc::mkdtemp(buffer.as_mut_ptr() as *mut libc::c_char) };
        if created.is_null() {
            continue;
        }

        // Safety: `mkdtemp` returned a pointer to `buffer`, still NUL terminated.
        let bytes = unsafe { CStr::from_ptr(created) }.to_bytes();
        return Ok(PathBuf::from(OsStr::from_bytes(bytes)));
    }

    Err("could not create a private directory for the NTFS tools".to_string())
}

fn write(path: &Path, data: &[u8], mode: u32) -> Result<(), String> {
    std::fs::write(path, data).map_err(|e| format!("{}: {e}", path.display()))?;
    set_mode(path, mode)
}

fn set_mode(path: &Path, mode: u32) -> Result<(), String> {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode))
        .map_err(|e| format!("{}: {e}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_formatter_is_always_available_with_the_bundled_copy() {
        // The checkout carries tools/ntfs, so this should hold. It also holds on
        // a system that has ntfsprogs installed, which is the point.
        assert!(available());
    }

    #[test]
    fn the_system_formatter_wins_when_it_exists() {
        if system_tool().is_some() {
            let formatter = resolve().unwrap();
            assert!(formatter.library_dir.is_none());
        }
    }

    #[test]
    fn unpacking_puts_both_files_in_a_private_directory() {
        if bundled::MKNTFS.is_empty() {
            return;
        }
        let dir = unpack().unwrap();

        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(&dir).unwrap().permissions().mode() & 0o777;
        assert_eq!(
            mode, 0o700,
            "the tools directory must not be world reachable"
        );

        assert!(dir.join("mkntfs").is_file());
        assert!(dir.join("libntfs-3g.so.91").is_file());
        let tool = std::fs::metadata(dir.join("mkntfs")).unwrap();
        assert!(
            tool.permissions().mode() & 0o111 != 0,
            "mkntfs must be executable"
        );

        std::fs::remove_dir_all(&dir).ok();
    }
}
