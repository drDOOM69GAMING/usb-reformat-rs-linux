//! Partition tables and filesystems.
//!
//! The Windows build shelled out to `diskpart` for every one of these steps.
//! Linux splits the same job across three small tools that each do one thing:
//! `wipefs` removes old signatures, `sfdisk` writes the table, and `mkfs.*`
//! creates the filesystem. Using `sfdisk` rather than `parted` for the table
//! buys us an explicit partition type byte, so an exFAT stick really is tagged
//! as exFAT instead of inheriting whatever `parted` guessed.

use std::path::{Path, PathBuf};

use crate::util::{have, run, run_quiet, run_with_input};

/// Filesystems offered in the format menu.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FsKind {
    Exfat,
    Fat32,
    Ntfs,
    Ext4,
}

impl FsKind {
    pub fn all() -> [FsKind; 4] {
        [FsKind::Exfat, FsKind::Fat32, FsKind::Ntfs, FsKind::Ext4]
    }

    pub fn display(self) -> &'static str {
        match self {
            FsKind::Exfat => "exFAT",
            FsKind::Fat32 => "FAT32",
            FsKind::Ntfs => "NTFS",
            FsKind::Ext4 => "ext4",
        }
    }

    pub fn mkfs_tool(self) -> &'static str {
        match self {
            FsKind::Exfat => "mkfs.exfat",
            FsKind::Fat32 => "mkfs.vfat",
            FsKind::Ntfs => "mkfs.ntfs",
            FsKind::Ext4 => "mkfs.ext4",
        }
    }

    /// MBR partition type byte. Getting this right is what makes Windows and
    /// firmware recognise the stick without reformatting it again.
    pub fn mbr_type(self) -> &'static str {
        match self {
            FsKind::Exfat => "07", // NTFS/exFAT/HPFS
            FsKind::Fat32 => "0c", // FAT32 LBA
            FsKind::Ntfs => "07",  // NTFS/exFAT/HPFS
            FsKind::Ext4 => "83",  // Linux
        }
    }

    pub fn from_name(name: &str) -> Option<FsKind> {
        match name.to_ascii_lowercase().as_str() {
            "exfat" => Some(FsKind::Exfat),
            "fat32" | "vfat" | "fat" => Some(FsKind::Fat32),
            "ntfs" => Some(FsKind::Ntfs),
            "ext4" | "ext" => Some(FsKind::Ext4),
            _ => None,
        }
    }

    /// `mkfs.vfat` writes at most 11 characters, and old FAT drivers only see
    /// uppercase, so the label is normalised before every format.
    pub fn normalize_label(self, label: &str) -> String {
        let trimmed: String = label.trim().chars().take(11).collect();
        match self {
            FsKind::Fat32 => trimmed.to_uppercase(),
            _ => trimmed,
        }
    }
}

/// `wipefs -a`: drop every old filesystem and partition signature.
pub fn erase_signatures(device: &Path) -> Result<String, String> {
    run("wipefs", &["-a", &device.display().to_string()])
}

/// Write a fresh MBR with a single partition spanning the whole device.
pub fn write_single_partition_table(device: &Path, fs: FsKind) -> Result<String, String> {
    // sfdisk script: `label: dos`, then `start,size,type`. Empty start and size
    // mean "auto", which gives us alignment plus the full remaining capacity.
    let script = format!("label: dos\n,,{}\n", fs.mbr_type());
    run_with_input(
        "sfdisk",
        &["--quiet", &device.display().to_string()],
        &script,
    )
}

/// Ask the kernel to re-read the table so the partition node appears.
pub fn reread_table(device: &Path) -> Result<(), String> {
    let dev = device.display().to_string();
    if have("partprobe") {
        return run_quiet("partprobe", &[&dev]);
    }
    run_quiet("blockdev", &["--rereadpt", &dev])
}

/// The node for partition `index` of `device`: `sda` -> `sda1`,
/// `nvme0n1` -> `nvme0n1p1`, `mmcblk0` -> `mmcblk0p1`.
pub fn partition_node(device: &Path, index: u32) -> PathBuf {
    let name = device
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_default();
    let needs_separator = name
        .chars()
        .last()
        .map(|c| c.is_ascii_digit())
        .unwrap_or(false);
    if needs_separator {
        PathBuf::from(format!("{}p{}", device.display(), index))
    } else {
        PathBuf::from(format!("{}{}", device.display(), index))
    }
}

/// Whether a filesystem can be created right now. NTFS also counts the copy
/// baked into this binary, so it does not depend on `ntfsprogs` being installed.
pub fn format_available(fs: FsKind) -> bool {
    if fs == FsKind::Ntfs {
        crate::ntfstool::available()
    } else {
        have(fs.mkfs_tool())
    }
}

/// Create the filesystem on a partition (or, for testing, a plain image file).
///
/// NTFS is the odd one out: its formatter may be the system `mkfs.ntfs` or the
/// copy carried inside this binary, so it is resolved rather than looked up on
/// PATH. The arguments are the same either way.
pub fn format(part: &Path, fs: FsKind, label: Option<&str>) -> Result<String, String> {
    let target = part.display().to_string();
    let normalised = label.map(|l| fs.normalize_label(l)).unwrap_or_default();

    let args: Vec<String> = match fs {
        FsKind::Exfat => {
            let mut a = vec!["-q".to_string()];
            if !normalised.is_empty() {
                a.push("-n".into());
                a.push(normalised.clone());
            }
            a.push(target.clone());
            a
        }
        FsKind::Fat32 => {
            let mut a = vec!["-F".into(), "32".into()];
            if !normalised.is_empty() {
                a.push("-n".into());
                a.push(normalised.clone());
            }
            a.push(target.clone());
            a
        }
        FsKind::Ntfs => {
            let mut a = vec!["-F".into(), "-Q".into()];
            if !normalised.is_empty() {
                a.push("-L".into());
                a.push(normalised.clone());
            }
            a.push(target.clone());
            a
        }
        FsKind::Ext4 => {
            let mut a = vec!["-F".into(), "-q".into()];
            if !normalised.is_empty() {
                a.push("-L".into());
                a.push(normalised.clone());
            }
            a.push(target.clone());
            a
        }
    };

    if fs == FsKind::Ntfs {
        return crate::ntfstool::resolve()?.run(&args);
    }

    let tool = fs.mkfs_tool();
    if !have(tool) {
        return Err(format!(
            "{tool} is not installed (needed to make {} filesystems)",
            fs.display()
        ));
    }

    let borrowed: Vec<&str> = args.iter().map(String::as_str).collect();
    run(tool, &borrowed)
}

/// Unmount every mount point that belongs to `device`'s children, then the
/// partitions themselves, so a raw write is not refused by a busy device.
pub fn unmount_all(paths: &[String]) -> Vec<String> {
    let mut errors = Vec::new();
    // Deepest paths first, so a bind mount under /run/media/... goes before its
    // parent.
    let mut sorted: Vec<&String> = paths.iter().collect();
    sorted.sort_by_key(|p| std::cmp::Reverse(p.len()));
    for p in sorted {
        if let Err(e) = run_quiet("umount", &[p]) {
            if !e.contains("not mounted") && !e.contains("no mount point") {
                errors.push(format!("{p}: {e}"));
            }
        }
    }
    errors
}

/// Read back what `lsblk` now thinks the device looks like, for the verify step.
pub fn verify_layout(device: &Path) -> Result<String, String> {
    let out = run(
        "lsblk",
        &[
            "-P",
            "-b",
            "-o",
            "NAME,PATH,SIZE,FSTYPE,LABEL,MOUNTPOINTS",
            &device.display().to_string(),
        ],
    )?;
    let mut lines: Vec<String> = Vec::new();
    for line in out.lines() {
        for field in ["PATH", "FSTYPE", "LABEL", "SIZE"] {
            if let Some(value) = field_value(line, field) {
                if !value.is_empty() {
                    lines.push(format!("{field}={value}"));
                }
            }
        }
        lines.push("--".to_string());
    }
    Ok(lines.join(" "))
}

fn field_value(line: &str, key: &str) -> Option<String> {
    let needle = format!("{key}=\"");
    let start = line.find(&needle)? + needle.len();
    let rest = &line[start..];
    let end = rest.find('"')?;
    Some(rest[..end].to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn partition_type_bytes_are_correct() {
        assert_eq!(FsKind::Exfat.mbr_type(), "07");
        assert_eq!(FsKind::Ntfs.mbr_type(), "07");
        assert_eq!(FsKind::Fat32.mbr_type(), "0c");
        assert_eq!(FsKind::Ext4.mbr_type(), "83");
    }

    #[test]
    fn fat_labels_are_uppercased_and_capped() {
        assert_eq!(FsKind::Fat32.normalize_label("backup stick"), "BACKUP STIC");
        assert_eq!(FsKind::Exfat.normalize_label("Backup"), "Backup");
    }

    #[test]
    fn partition_nodes_follow_disk_naming() {
        assert_eq!(
            partition_node(Path::new("/dev/sda"), 1),
            PathBuf::from("/dev/sda1")
        );
        assert_eq!(
            partition_node(Path::new("/dev/nvme0n1"), 1),
            PathBuf::from("/dev/nvme0n1p1")
        );
        assert_eq!(
            partition_node(Path::new("/dev/mmcblk0"), 2),
            PathBuf::from("/dev/mmcblk0p2")
        );
        assert_eq!(
            partition_node(Path::new("/dev/sdc"), 3),
            PathBuf::from("/dev/sdc3")
        );
    }

    #[test]
    fn fs_names_round_trip() {
        for fs in FsKind::all() {
            assert_eq!(FsKind::from_name(fs.display()), Some(fs));
            assert!(!fs.mkfs_tool().is_empty());
        }
        assert_eq!(FsKind::from_name("VFAT"), Some(FsKind::Fat32));
        assert_eq!(FsKind::from_name("hfs"), None);
    }

    /// sfdisk works on a plain file, so the table writer can be exercised
    /// without touching a real device or needing root.
    #[test]
    fn writes_a_single_partition_table_over_a_whole_image() {
        if !have("sfdisk") || !have("wipefs") {
            eprintln!("sfdisk/wipefs not installed, skipping");
            return;
        }
        let dir = std::env::temp_dir().join(format!("usb-reformat-table-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let img = dir.join("disk.img");

        let size = 64 * 1024 * 1024u64;
        std::fs::File::create(&img).unwrap().set_len(size).unwrap();

        erase_signatures(&img).unwrap();
        write_single_partition_table(&img, FsKind::Exfat).unwrap();

        let dump = run("sfdisk", &["-d", &img.display().to_string()]).unwrap();
        assert!(dump.contains("label: dos"), "dump was: {dump}");
        assert!(
            dump.contains("type=7"),
            "exFAT should be type 7, dump: {dump}"
        );

        // And the FAT32 mapping really differs.
        erase_signatures(&img).unwrap();
        write_single_partition_table(&img, FsKind::Fat32).unwrap();
        let dump = run("sfdisk", &["-d", &img.display().to_string()]).unwrap();
        assert!(
            dump.contains("type=c"),
            "FAT32 should be type c, dump: {dump}"
        );

        std::fs::remove_dir_all(&dir).ok();
    }

    /// `mkfs.*` all accept a plain file, which means the exact argument vectors
    /// can be checked here instead of hoping they are right on a real stick.
    #[test]
    fn formats_an_image_with_the_real_tools() {
        let dir = std::env::temp_dir().join(format!("usb-reformat-fmt-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let img = dir.join("fs.img");
        let path = img.display().to_string();

        let size = 96 * 1024 * 1024u64;

        if have("mkfs.ext4") {
            std::fs::File::create(&img).unwrap().set_len(size).unwrap();
            format(&img, FsKind::Ext4, Some("TESTVOL")).unwrap();
            let out = run("blkid", &[&path]).unwrap_or_default();
            assert!(out.contains("ext4"), "blkid said: {out}");
            assert!(out.contains("TESTVOL"), "blkid said: {out}");
        }

        if have("mkfs.exfat") {
            std::fs::File::create(&img).unwrap().set_len(size).unwrap();
            erase_signatures(&img).ok();
            format(&img, FsKind::Exfat, Some("Stick")).unwrap();
            let out = run("blkid", &[&path]).unwrap_or_default();
            assert!(out.contains("exfat"), "blkid said: {out}");
            assert!(out.contains("Stick"), "blkid said: {out}");
        }

        if have("mkfs.vfat") {
            std::fs::File::create(&img).unwrap().set_len(size).unwrap();
            erase_signatures(&img).ok();
            format(&img, FsKind::Fat32, Some("stick")).unwrap();
            let out = run("blkid", &[&path]).unwrap_or_default();
            assert!(out.contains("vfat"), "blkid said: {out}");
            // FAT labels come out uppercase, which is what Windows shows.
            assert!(out.contains("STICK"), "blkid said: {out}");
        }

        std::fs::remove_dir_all(&dir).ok();
    }

    /// NTFS falls back to the copy baked into the binary when the system has no
    /// `mkntfs`, so this covers the whole unpack-and-run path when it is used.
    #[test]
    fn formats_an_image_as_ntfs() {
        if !format_available(FsKind::Ntfs) {
            eprintln!("no NTFS formatter available, skipping");
            return;
        }
        let dir = std::env::temp_dir().join(format!("usb-reformat-ntfs-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let img = dir.join("ntfs.img");
        let path = img.display().to_string();

        std::fs::File::create(&img)
            .unwrap()
            .set_len(96 * 1024 * 1024)
            .unwrap();
        format(&img, FsKind::Ntfs, Some("PACKED")).unwrap();

        if have("blkid") {
            let out = run("blkid", &[&path]).unwrap_or_default();
            assert!(out.contains("ntfs"), "blkid said: {out}");
            assert!(out.contains("PACKED"), "blkid said: {out}");
        } else {
            // The OEM id in the NTFS boot sector is the fallback check.
            use std::io::Read;
            let mut boot = [0u8; 11];
            std::fs::File::open(&img)
                .unwrap()
                .read_exact(&mut boot)
                .unwrap();
            assert_eq!(&boot[3..11], b"NTFS    ");
        }

        std::fs::remove_dir_all(&dir).ok();
    }
}
