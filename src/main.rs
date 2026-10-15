//! usb-reformat (Linux) - disk utility in the spirit of the Acronis rescue
//! media tools, minus anything to do with backup or restore.
//!
//! What it does:
//!   * lists physical disks once each, with their real model and full layout
//!   * wipes a whole device (quick, or DoD / Gutmann multi-pass overwrite)
//!   * creates and deletes partitions
//!   * formats partitions (exFAT, FAT32, NTFS, ext4)
//!   * verifies the result and reports it
//!
//! What it deliberately does not do: images, backups, clones, restore points.
//!
//! Usage:
//!   usb-reformat                 Windows 98 style GUI (default)
//!   usb-reformat --list          list disks and partitions (read-only)
//!   usb-reformat --status        check the tools this relies on
//!   usb-reformat --run ...       privileged worker launched by the GUI

mod device;
#[cfg(feature = "music")]
mod music;
mod ntfstool;
mod partition;
mod pipeline;
mod privs;
mod util;
mod wipe;

#[cfg(feature = "gui")]
mod gui;

use std::path::PathBuf;
use std::process::exit;

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let flag = |name: &str| args.iter().any(|a| a == name);

    if flag("--help") || flag("-h") {
        print_help();
        return;
    }

    if flag("--list") {
        if let Err(e) = cmd_list() {
            eprintln!("{e}");
            exit(1);
        }
        return;
    }

    if flag("--status") || flag("--tools") {
        cmd_status();
        return;
    }

    if flag("--run") {
        match cmd_run(&args) {
            Ok(code) => exit(code),
            Err(e) => {
                eprintln!("{e}");
                exit(2);
            }
        }
    }

    #[cfg(feature = "gui")]
    {
        gui::run();
    }

    #[cfg(not(feature = "gui"))]
    {
        print_help();
    }
}

fn print_help() {
    println!(
        "\
usb-reformat - wipe, partition and format drives

USAGE:
    usb-reformat [OPTION]

OPTIONS:
    --list      List physical disks and their partitions (read-only)
    --status    Check that the required command line tools are installed
    --help      Show this message

The worker flags (--run, --device, --fs, --label, --passes, --plan, --log, --ctl)
belong to the GUI. They are what the authorisation prompt runs, and are not
meant to be typed by hand.

Without an option the Windows 98 style GUI is opened. That is the intended way
to run it: the GUI does the disk work through a privileged worker after asking
for authorisation once. This tool only manages disks. It never backs anything
up, images anything, or restores anything."
    );
}

fn cmd_list() -> Result<(), String> {
    let disks = device::list_disks()?;
    if disks.is_empty() {
        println!("No physical disks found.");
        return Ok(());
    }

    let root = device::root_disk(&disks);
    let used = device::in_use_disks(&disks);

    println!("Physical disks ({} found):\n", disks.len());
    for disk in &disks {
        let mut tags: Vec<&str> = vec![disk.kind_tag()];
        if root.as_deref() == Some(disk.name.as_str()) {
            tags.push("SYSTEM - cannot be wiped");
        } else if used.contains(&disk.name) {
            tags.push("in use - mounted");
        }
        if disk.is_ssd() {
            tags.push("SSD - full wipe blocked");
        }
        if disk.read_only {
            tags.push("read-only");
        }

        println!(
            "  {:<10} {:<32} {:>9}  {}  [{}]",
            disk.path,
            device::trim_to(&disk.title(), 32),
            disk.size_human(),
            disk.bus_label(),
            tags.join(" / ")
        );

        if disk.partitions.is_empty() {
            println!("      (no partitions)");
        }
        for part in &disk.partitions {
            println!("      {:<14} {}", part.path, part.describe());
        }
        println!();
    }

    Ok(())
}

fn cmd_status() {
    println!("usb-reformat status\n");

    let root = util::is_root();
    println!(
        "  running as root: {}",
        if root {
            "yes"
        } else {
            "no (the GUI asks via pkexec)"
        }
    );

    let tools: [(&str, &str); 12] = [
        ("lsblk", "list disks and partitions"),
        ("findmnt", "work out the layout"),
        ("wipefs", "erase filesystem signatures"),
        ("sfdisk", "write the partition table"),
        ("partprobe", "make the kernel re-read the table"),
        ("mkfs.exfat", "exFAT formatting (exfatprogs)"),
        ("mkfs.vfat", "FAT32 formatting (dosfstools)"),
        (
            "mkfs.ntfs",
            "NTFS formatting (ntfsprogs, or the bundled copy)",
        ),
        ("mkfs.ext4", "ext4 formatting (e2fsprogs)"),
        ("umount", "unmount before writing"),
        ("blockdev", "fallback table rescan"),
        ("pkexec", "ask for authorisation"),
    ];

    let mut missing = 0;
    for (tool, what) in tools {
        // NTFS also counts as present when it is the copy baked into this
        // binary, which is what keeps a lone executable able to format NTFS.
        let present = if tool == "mkfs.ntfs" {
            ntfstool::available()
        } else {
            util::have(tool)
        };

        if present {
            let note = if tool == "mkfs.ntfs" && !util::have("mkfs.ntfs") {
                "  [bundled]"
            } else {
                ""
            };
            println!("  [ok]      {:<11} {what}{note}", tool);
        } else {
            missing += 1;
            let optional = matches!(tool, "mkfs.ntfs" | "mkfs.ext4" | "pkexec");
            let hint = util::package_for(tool)
                .map(|package| format!("  (pacman -S {package})"))
                .unwrap_or_default();
            println!(
                "  [{}] {:<11} {what}{hint}",
                if optional { "optional" } else { "MISSING " },
                tool
            );
        }
    }

    if missing == 0 {
        println!("\nAll tools present.");
    } else {
        println!("\nEvery one of these lives in the base repos; install the ones you need.");
    }
}

/// `--run` is the privileged worker the GUI starts. Everything about the job
/// arrives on the command line so the two processes share no state.
fn cmd_run(args: &[String]) -> Result<i32, String> {
    let devices: Vec<PathBuf> = values_of(args, "--device")
        .into_iter()
        .map(PathBuf::from)
        .collect();
    if devices.is_empty() {
        return Err("--run needs at least one --device /dev/sdX".into());
    }

    let fs_name = value_of(args, "--fs").unwrap_or_else(|| "exfat".to_string());
    let fs = partition::FsKind::from_name(&fs_name).ok_or_else(|| {
        format!("unknown filesystem '{fs_name}' (try exfat, fat32, ntfs or ext4)")
    })?;

    let plan_name = value_of(args, "--plan").unwrap_or_else(|| "wipe-and-format".to_string());
    let plan = pipeline::Plan::from_name(&plan_name)
        .ok_or_else(|| format!("unknown plan '{plan_name}'"))?;

    let passes = match value_of(args, "--passes") {
        Some(v) => v
            .parse::<u32>()
            .map_err(|e| format!("bad --passes '{v}': {e}"))?,
        None => 1,
    };

    let log = PathBuf::from(
        value_of(args, "--log").ok_or("--run needs --log <path to write progress to>")?,
    );
    let ctl = value_of(args, "--ctl").map(PathBuf::from);

    if !util::is_root() {
        eprintln!("note: not running as root, so writing to a real disk will be refused");
    }

    let job = pipeline::Job {
        devices,
        fs,
        label: value_of(args, "--label").unwrap_or_default(),
        passes,
        plan,
        log,
        ctl,
    };

    Ok(pipeline::run(&job))
}

/// The value following the first occurrence of `name`.
fn value_of(args: &[String], name: &str) -> Option<String> {
    args.iter()
        .position(|a| a == name)
        .and_then(|i| args.get(i + 1))
        .cloned()
}

/// Every value following an occurrence of `name`.
fn values_of(args: &[String], name: &str) -> Vec<String> {
    let mut found = Vec::new();
    for (i, arg) in args.iter().enumerate() {
        if arg == name {
            if let Some(value) = args.get(i + 1) {
                found.push(value.clone());
            }
        }
    }
    found
}
