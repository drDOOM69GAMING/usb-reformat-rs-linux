//! Disk and partition enumeration for Linux.
//!
//! Windows reached into SetupAPI and opened `\\.\PhysicalDriveN` handles. Here
//! the same picture comes from `lsblk`, which every systemd distro ships, plus a
//! few files in `/sys` and `/proc`.
//!
//! Two things the Windows build had to reconstruct the hard way come for free:
//! a multi partition stick shares one physical disk (so we group by disk and
//! list it once), and the disk backing `/` is easy to identify so it can be
//! refused outright.

use std::collections::HashSet;

use crate::util::{human_size, run};

/// One partition (or child volume) belonging to a physical disk.
#[derive(Debug, Clone, Default)]
pub struct Partition {
    pub path: String,
    pub size_bytes: u64,
    pub fstype: String,
    pub label: String,
    pub mountpoints: Vec<String>,
    /// `part`, `lvm`, `crypt`, ...
    pub kind: String,
}

impl Partition {
    pub fn mounted(&self) -> bool {
        !self.mountpoints.is_empty()
    }

    pub fn size_human(&self) -> String {
        human_size(self.size_bytes)
    }

    /// `exFAT 'BACKUP' 14.9 GB`, with the volume kind called out when it is
    /// something other than a plain partition.
    pub fn describe(&self) -> String {
        let fs = if self.fstype.is_empty() {
            "unformatted".to_string()
        } else {
            self.fstype.clone()
        };
        let kind = if self.kind.is_empty() || self.kind == "part" {
            String::new()
        } else {
            format!("{} ", self.kind)
        };
        match (self.label.is_empty(), self.mounted()) {
            (false, true) => format!(
                "{kind}{fs}  '{}'  {}  mounted at {}",
                self.label,
                self.size_human(),
                self.mountpoints.join(", ")
            ),
            (false, false) => format!("{kind}{fs}  '{}'  {}", self.label, self.size_human()),
            (true, true) => format!(
                "{kind}{fs}  {}  mounted at {}",
                self.size_human(),
                self.mountpoints.join(", ")
            ),
            (true, false) => format!("{kind}{fs}  {}", self.size_human()),
        }
    }
}

/// A whole physical device. This is the unit the wipe operates on, never a
/// single partition.
#[derive(Debug, Clone, Default)]
pub struct Disk {
    pub name: String,
    pub path: String,
    pub model: String,
    pub vendor: String,
    pub serial: String,
    pub tran: String,
    pub size_bytes: u64,
    pub removable: bool,
    pub hotplug: bool,
    pub read_only: bool,
    /// `lsblk` ROTA. False means the kernel says it is not a spinning disk.
    pub rotational: bool,
    pub partitions: Vec<Partition>,
}

impl Disk {
    pub fn size_human(&self) -> String {
        human_size(self.size_bytes)
    }

    /// Prefer the model, then the vendor, then the serial, then the device
    /// name. Two identical sticks are most reliably told apart by serial.
    pub fn title(&self) -> String {
        let model = self.model.trim();
        if !model.is_empty() {
            return model.to_string();
        }
        let vendor = self.vendor.trim();
        if !vendor.is_empty() {
            return vendor.to_string();
        }
        let serial = self.serial.trim();
        if !serial.is_empty() {
            return serial.to_string();
        }
        self.name.clone()
    }

    /// `removable` / `FIXED - risky`, mirroring the Windows wording.
    pub fn kind_tag(&self) -> &'static str {
        if self.removable || self.hotplug {
            "removable"
        } else {
            "FIXED - risky"
        }
    }

    /// True only for real solid state media: not spinning, not a USB or
    /// hot-plugged device, and sitting on an internal bus.
    ///
    /// Overwriting an SSD does not securely erase it (the controller remaps
    /// blocks), and the multi-pass schedules just wear it out, so the full wipe
    /// is refused for these exactly as it was on Windows. Keeping the check
    /// narrow means a USB stick that reports no rotation still gets the full
    /// treatment.
    pub fn is_ssd(&self) -> bool {
        !self.rotational
            && !self.removable
            && !self.hotplug
            && matches!(self.tran.as_str(), "sata" | "ata" | "nvme" | "scsi" | "sas")
    }

    pub fn media_label(&self) -> &'static str {
        if self.is_ssd() {
            "SSD"
        } else if self.removable || self.hotplug {
            "removable"
        } else if self.rotational {
            "hard disk"
        } else {
            "flash"
        }
    }

    /// Interface as the user would say it: USB, SATA, NVMe, ...
    pub fn bus_label(&self) -> String {
        match self.tran.as_str() {
            "usb" => "USB".to_string(),
            "sata" | "ata" => "SATA".to_string(),
            "nvme" => "NVMe".to_string(),
            "mmc" | "sd" => "SD".to_string(),
            "" => "unknown".to_string(),
            other => other.to_uppercase(),
        }
    }

    /// The one-line form shown in the drive list.
    ///
    /// Windows printed `G:   MODEL   'LABEL'   14.9 GB free   [USB / removable / disk 3]`.
    /// Without drive letters the device node takes that slot, and the current
    /// filesystem takes the place of the free space, which is what the wipe is
    /// about to replace.
    pub fn gui_line(&self) -> String {
        let content = match self.partitions.first() {
            Some(part) if !part.fstype.is_empty() => {
                if part.label.is_empty() {
                    part.fstype.clone()
                } else {
                    format!("{} '{}'", part.fstype, trim_to(&part.label, 18))
                }
            }
            Some(_) => "unformatted".to_string(),
            None => "no partitions".to_string(),
        };
        let parts = if self.partitions.is_empty() {
            "blank".to_string()
        } else {
            format!("{} part", self.partitions.len())
        };
        format!(
            "{:<10} {:<26} {:>9}  {:<26} [{} / {} / {}]",
            self.path,
            trim_to(&self.title(), 26),
            self.size_human(),
            content,
            self.bus_label(),
            self.media_label(),
            parts
        )
    }
}

/// Trim a label so list rows stay aligned.
pub fn trim_to(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        return text.to_string();
    }
    let head: String = text.chars().take(max.saturating_sub(2)).collect();
    format!("{}..", head.trim_end())
}

/// Physical disks, each with its partitions nested underneath.
pub fn list_disks() -> Result<Vec<Disk>, String> {
    let out = run(
        "lsblk",
        &[
            "-P",
            "-b",
            "-o",
            "NAME,PATH,TYPE,SIZE,MODEL,VENDOR,SERIAL,TRAN,RM,HOTPLUG,RO,ROTA,FSTYPE,LABEL,MOUNTPOINTS",
        ],
    )?;

    let mut disks: Vec<Disk> = Vec::new();
    for line in out.lines() {
        let fields = parse_pairs(line);
        let get = |key: &str| {
            fields
                .iter()
                .find(|(k, _)| k == key)
                .map(|(_, v)| v.clone())
                .unwrap_or_default()
        };
        let name = get("NAME");
        if name.is_empty() {
            continue;
        }
        let kind = get("TYPE");

        if kind == "disk" {
            if is_virtual(&name) {
                continue;
            }
            disks.push(Disk {
                name,
                path: get("PATH"),
                model: get("MODEL"),
                vendor: get("VENDOR"),
                serial: get("SERIAL"),
                tran: get("TRAN"),
                size_bytes: get("SIZE").parse().unwrap_or(0),
                removable: get("RM") == "1",
                hotplug: get("HOTPLUG") == "1",
                read_only: get("RO") == "1",
                rotational: get("ROTA") != "0",
                partitions: Vec::new(),
            });
            continue;
        }

        // Anything else is a child of the most recent disk. `lsblk -P` prints a
        // parent immediately before its children, so "last disk" is the owner.
        if let Some(disk) = disks.last_mut() {
            if !name.starts_with(disk.name.as_str()) {
                continue;
            }
            let mountpoints: Vec<String> = get("MOUNTPOINTS")
                .split('\n')
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(str::to_string)
                .collect();
            disk.partitions.push(Partition {
                path: get("PATH"),
                size_bytes: get("SIZE").parse().unwrap_or(0),
                fstype: get("FSTYPE"),
                label: get("LABEL"),
                mountpoints,
                kind,
            });
        }
    }

    Ok(disks)
}

/// Loop devices, zram, ramdisks and device-mapper nodes are not candidates for
/// "wipe this whole stick", so they never reach the list.
fn is_virtual(name: &str) -> bool {
    ["loop", "zram", "ram", "md", "dm-", "sr", "fd", "nbd", "rbd"]
        .iter()
        .any(|prefix| name.starts_with(prefix))
}

/// Which physical disk a device node or mount source belongs to.
pub fn disk_for_source<'a>(source: &str, disks: &'a [Disk]) -> Option<&'a str> {
    let src = source.trim();
    if !src.starts_with("/dev/") {
        return None;
    }
    for disk in disks {
        if src == disk.path {
            return Some(disk.name.as_str());
        }
        if let Some(rest) = src.strip_prefix(disk.path.as_str()) {
            // sda1, nvme0n1p2, mmcblk0p3 but not sda matching sdab
            let first = rest.chars().next()?;
            if first.is_ascii_digit() || first == 'p' {
                return Some(disk.name.as_str());
            }
        }
    }
    None
}

/// Every disk that currently has something mounted on it or holds swap.
pub fn in_use_disks(disks: &[Disk]) -> HashSet<String> {
    let mut used = HashSet::new();

    if let Ok(out) = run("findmnt", &["-rn", "-o", "SOURCE,TARGET"]) {
        for line in out.lines() {
            let source = line.split_whitespace().next().unwrap_or("");
            if let Some(name) = disk_for_source(source, disks) {
                used.insert(name.to_string());
            }
        }
    }

    if let Ok(swaps) = std::fs::read_to_string("/proc/swaps") {
        for line in swaps.lines().skip(1) {
            let source = line.split_whitespace().next().unwrap_or("");
            if let Some(name) = disk_for_source(source, disks) {
                used.insert(name.to_string());
            }
        }
    }

    used
}

/// The disk that hosts `/`. Never allowed to be wiped.
pub fn root_disk(disks: &[Disk]) -> Option<String> {
    let out = run("findmnt", &["-n", "-o", "SOURCE", "--target", "/"]).ok()?;
    disk_for_source(out.trim(), disks).map(str::to_string)
}

/// Every mount point that lives on `disk`, so it can be unmounted before a
/// raw write. `/` and swap can never be unmounted, which is exactly why the
/// disk holding them is refused up front.
pub fn mountpoints_on(disk: &Disk) -> Vec<String> {
    let mut points = Vec::new();
    for part in &disk.partitions {
        for mp in &part.mountpoints {
            if mp.starts_with('/') {
                points.push(mp.clone());
            }
        }
    }
    points
}

// ---------------------------------------------------------------------------
// lsblk -P parsing
// ---------------------------------------------------------------------------

/// Parse `KEY="value" KEY="value"` into pairs, decoding the `\xNN` escapes
/// `lsblk` uses (notably `\x0a` between multiple mount points).
fn parse_pairs(line: &str) -> Vec<(String, String)> {
    let chars: Vec<char> = line.chars().collect();
    let mut pairs = Vec::new();
    let mut i = 0;

    while i < chars.len() {
        while i < chars.len() && chars[i] == ' ' {
            i += 1;
        }
        let key_start = i;
        while i < chars.len() && chars[i] != '=' {
            i += 1;
        }
        if i >= chars.len() {
            break;
        }
        let key: String = chars[key_start..i].iter().collect();
        i += 1; // '='
        if i >= chars.len() || chars[i] != '"' {
            continue;
        }
        i += 1;

        let mut value = String::new();
        while i < chars.len() && chars[i] != '"' {
            if chars[i] == '\\' && i + 1 < chars.len() {
                if chars[i + 1] == 'x' && i + 3 < chars.len() {
                    let hex: String = chars[i + 2..i + 4].iter().collect();
                    if let Ok(byte) = u8::from_str_radix(&hex, 16) {
                        value.push(byte as char);
                        i += 4;
                        continue;
                    }
                }
                value.push(chars[i + 1]);
                i += 2;
                continue;
            }
            value.push(chars[i]);
            i += 1;
        }
        i += 1; // closing quote

        if !key.is_empty() {
            pairs.push((key, value));
        }
    }

    pairs
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_lsblk_pairs_with_escapes() {
        let line = r#"NAME="sda" PATH="/dev/sda" TYPE="disk" SIZE="2000398934016" MODEL="ST2000DM008" RM="0" MOUNTPOINTS=""#;
        let pairs = parse_pairs(line);
        let get = |k: &str| {
            pairs
                .iter()
                .find(|(key, _)| key == k)
                .map(|(_, v)| v.as_str())
                .unwrap()
        };
        assert_eq!(get("NAME"), "sda");
        assert_eq!(get("SIZE"), "2000398934016");
        assert_eq!(get("MODEL"), "ST2000DM008");
        assert_eq!(get("MOUNTPOINTS"), "");
    }

    #[test]
    fn decodes_multiple_mountpoints() {
        let pairs = parse_pairs(r#"MOUNTPOINTS="/home\x0a/var/tmp""#);
        assert_eq!(pairs[0].1, "/home\n/var/tmp");
    }

    #[test]
    fn maps_device_nodes_to_their_disk() {
        let disks = vec![
            Disk {
                name: "sda".into(),
                path: "/dev/sda".into(),
                ..Default::default()
            },
            Disk {
                name: "nvme0n1".into(),
                path: "/dev/nvme0n1".into(),
                ..Default::default()
            },
        ];
        assert_eq!(disk_for_source("/dev/sda1", &disks), Some("sda"));
        assert_eq!(disk_for_source("/dev/nvme0n1p2", &disks), Some("nvme0n1"));
        assert_eq!(disk_for_source("/dev/nvme0n1", &disks), Some("nvme0n1"));
        // Must not confuse sdab with sda
        assert_eq!(disk_for_source("/dev/sdab1", &disks), None);
        assert_eq!(disk_for_source("tmpfs", &disks), None);
    }

    #[test]
    fn virtual_devices_are_skipped() {
        assert!(is_virtual("loop0"));
        assert!(is_virtual("zram0"));
        assert!(is_virtual("dm-0"));
        assert!(!is_virtual("sda"));
        assert!(!is_virtual("nvme0n1"));
    }

    #[test]
    fn ssd_detection_spares_solid_state_and_respects_usb() {
        let nvme = Disk {
            name: "nvme0n1".into(),
            path: "/dev/nvme0n1".into(),
            tran: "nvme".into(),
            rotational: false,
            ..Default::default()
        };
        assert!(nvme.is_ssd());
        assert_eq!(nvme.media_label(), "SSD");

        let hdd = Disk {
            name: "sda".into(),
            path: "/dev/sda".into(),
            tran: "sata".into(),
            rotational: true,
            ..Default::default()
        };
        assert!(!hdd.is_ssd());
        assert_eq!(hdd.media_label(), "hard disk");

        // A USB stick that reports no rotation must still be allowed a full
        // wipe, exactly like a spinning external disk.
        let stick = Disk {
            name: "sdc".into(),
            path: "/dev/sdc".into(),
            tran: "usb".into(),
            removable: true,
            rotational: false,
            ..Default::default()
        };
        assert!(!stick.is_ssd());
        assert_eq!(stick.media_label(), "removable");
    }

    #[test]
    fn gui_lines_carry_the_model_bus_and_media() {
        let disk = Disk {
            name: "sdc".into(),
            path: "/dev/sdc".into(),
            model: "TOSHIBA HDWE150".into(),
            tran: "usb".into(),
            size_bytes: 5_000_000_000_000,
            removable: true,
            partitions: vec![Partition {
                path: "/dev/sdc1".into(),
                fstype: "btrfs".into(),
                label: "4TB HDD".into(),
                ..Default::default()
            }],
            ..Default::default()
        };
        let line = disk.gui_line();
        assert!(line.contains("/dev/sdc"), "{line}");
        assert!(line.contains("TOSHIBA"), "{line}");
        assert!(line.contains("5.0 TB"), "{line}");
        assert!(line.contains("btrfs"), "{line}");
        assert!(line.contains("USB"), "{line}");
        assert!(line.contains("removable"), "{line}");
    }
}
