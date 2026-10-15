//! The privileged worker: the part that actually touches a disk.
//!
//! Stage numbering matches the progress screen, and every stage is logged with
//! a timestamp to a file the unprivileged GUI tails. Pause and cancel travel
//! the other way, as two files in a control directory, which keeps the two
//! processes from needing to share anything more intimate than a temp folder.
//!
//! The order is deliberate and mirrors what `diskpart` needed on Windows:
//!   1. unmount everything living on the disk, or the raw write is refused
//!   2. erase the old signatures, so a stale table cannot survive
//!   3. overwrite the whole device (the only slow part, and the only
//!      cancellable one)
//!   4. write a fresh table with exactly one partition
//!   5. format that partition
//!   6. read the result back and report it

use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use crate::device::{self, Disk};
use crate::partition::{self, FsKind};
use crate::util::human_size;
use crate::wipe;

/// How many stages the progress display counts through.
pub const STAGE_COUNT: usize = 6;

pub fn stage_name(n: usize) -> &'static str {
    match n {
        1 => "Preparing and unmounting",
        2 => "Erasing old signatures",
        3 => "Overwriting the whole device",
        4 => "Writing a fresh partition table",
        5 => "Formatting",
        6 => "Verifying the result",
        _ => "Working",
    }
}

/// What the job should actually do to each device.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Plan {
    /// Overwrite and leave blank. No table, no filesystem.
    WipeDevice,
    /// No overwrite. Just put one partition and a filesystem on it.
    FormatOnly,
    /// The full Acronis style recipe: wipe, partition, format, verify.
    WipeAndFormat,
}

impl Plan {
    pub fn from_name(name: &str) -> Option<Plan> {
        match name.to_ascii_lowercase().as_str() {
            "wipe" | "wipe-only" | "wipeonly" => Some(Plan::WipeDevice),
            "format" | "format-only" | "formatonly" => Some(Plan::FormatOnly),
            "wipe-and-format" | "wipeandformat" | "full" => Some(Plan::WipeAndFormat),
            _ => None,
        }
    }

    pub fn describe(self) -> &'static str {
        match self {
            Plan::WipeDevice => "wipe the device and leave it blank",
            Plan::FormatOnly => "create one partition and format it",
            Plan::WipeAndFormat => "wipe, create one partition, format it, then verify",
        }
    }
}

/// Everything the worker needs, parsed from the command line by `--run`.
#[derive(Debug, Clone)]
pub struct Job {
    pub devices: Vec<PathBuf>,
    pub fs: FsKind,
    pub label: String,
    pub passes: u32,
    pub plan: Plan,
    pub log: PathBuf,
    pub ctl: Option<PathBuf>,
}

/// Timestamped logging to both the console and the log file the GUI reads.
pub struct Logger {
    start: Instant,
    file: Mutex<File>,
}

impl Logger {
    pub fn open(path: &Path) -> Result<Logger, String> {
        if let Some(parent) = path.parent() {
            if !parent.as_os_str().is_empty() {
                std::fs::create_dir_all(parent).ok();
            }
        }
        let file = OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(true)
            .open(path)
            .map_err(|e| format!("cannot write the log at {}: {e}", path.display()))?;

        // The worker runs as root; the GUI reads this file as a normal user, so
        // it has to stay world readable even if root's umask is tight.
        set_world_readable(path);

        Ok(Logger {
            start: Instant::now(),
            file: Mutex::new(file),
        })
    }

    pub fn say(&self, message: &str) {
        let elapsed = self.start.elapsed().as_secs();
        let line = format!("[{:02}:{:02}] {message}", elapsed / 60, elapsed % 60);
        println!("{line}");
        if let Ok(mut file) = self.file.lock() {
            let _ = writeln!(file, "{line}");
            let _ = file.flush();
        }
    }

    pub fn heading(&self, message: &str) {
        self.say("");
        self.say(message);
    }

    /// Machine-readable progress for the GUI's bar. The `##` prefix keeps it
    /// out of sight in the human readable log pane.
    pub fn mark(&self, percent: f64) {
        if let Ok(mut file) = self.file.lock() {
            let _ = writeln!(file, "##PROG {percent:.1}");
            let _ = file.flush();
        }
    }
}

fn set_world_readable(path: &Path) {
    use std::os::unix::fs::PermissionsExt;
    let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o644));
}

/// Pause and cancel, watched as files in a control directory.
pub struct Control {
    stop: Arc<AtomicBool>,
    pause: Arc<AtomicBool>,
    quit: Arc<AtomicBool>,
    watcher: Option<JoinHandle<()>>,
}

impl Control {
    pub fn new(dir: Option<PathBuf>) -> Control {
        let stop = Arc::new(AtomicBool::new(false));
        let pause = Arc::new(AtomicBool::new(false));
        let quit = Arc::new(AtomicBool::new(false));

        let watcher = dir.map(|dir| {
            let stop = Arc::clone(&stop);
            let pause = Arc::clone(&pause);
            let quit = Arc::clone(&quit);
            std::thread::spawn(move || {
                while !quit.load(Ordering::SeqCst) {
                    if dir.join("cancel").exists() {
                        stop.store(true, Ordering::SeqCst);
                    }
                    pause.store(dir.join("pause").exists(), Ordering::SeqCst);
                    std::thread::sleep(Duration::from_millis(200));
                }
            })
        });

        Control {
            stop,
            pause,
            quit,
            watcher,
        }
    }

    pub fn stop_flag(&self) -> &AtomicBool {
        &self.stop
    }

    pub fn pause_flag(&self) -> &AtomicBool {
        &self.pause
    }

    pub fn stopped(&self) -> bool {
        self.stop.load(Ordering::SeqCst)
    }
}

impl Drop for Control {
    fn drop(&mut self) {
        self.quit.store(true, Ordering::SeqCst);
        if let Some(handle) = self.watcher.take() {
            let _ = handle.join();
        }
    }
}

/// Run the whole job. Returns the process exit code.
pub fn run(job: &Job) -> i32 {
    let logger = match Logger::open(&job.log) {
        Ok(logger) => logger,
        Err(e) => {
            eprintln!("{e}");
            return 2;
        }
    };
    let control = Control::new(job.ctl.clone());

    logger.heading("usb-reformat - privileged worker");
    logger.say(&format!("Plan:       {}", job.plan.describe()));
    logger.say(&format!("Filesystem: {}", job.fs.display()));
    logger.say(&format!("Wipe mode:  {}", wipe::pass_label(job.passes)));
    if !job.label.is_empty() {
        logger.say(&format!("Label:      {}", job.label));
    }
    logger.say(&format!("Devices:    {}", job.devices.len()));

    let disks = match device::list_disks() {
        Ok(disks) => disks,
        Err(e) => {
            logger.say(&format!("Could not read the disk list: {e}"));
            return 2;
        }
    };
    let system_disk = device::root_disk(&disks);

    let mut failures = 0;
    for device in &job.devices {
        if let Err(e) = run_one(
            device,
            job,
            &disks,
            system_disk.as_deref(),
            &logger,
            &control,
        ) {
            logger.say(&format!("FAILED: {e}"));
            failures += 1;
        }
        if control.stopped() {
            break;
        }
    }

    logger.say("");
    if control.stopped() {
        logger.say("Stopped at your request. The device may be left part wiped.");
        return 3;
    }
    if failures > 0 {
        logger.say(&format!(
            "{failures} device(s) did not finish. See the errors above."
        ));
        return 1;
    }
    logger.say("All done.");
    0
}

fn run_one(
    device: &Path,
    job: &Job,
    disks: &[Disk],
    system_disk: Option<&str>,
    logger: &Logger,
    control: &Control,
) -> Result<(), String> {
    let device_path = device.display().to_string();
    logger.heading(&format!("=== {device_path} ==="));

    let disk = disks
        .iter()
        .find(|d| d.path == device_path || d.name == device_path)
        .cloned()
        .ok_or_else(|| format!("{device_path} is not one of the physical disks"))?;

    if system_disk == Some(disk.name.as_str()) {
        return Err(format!(
            "refusing to touch {}: that disk holds the running system (/)",
            disk.path
        ));
    }

    // Same protection the Windows build had. The GUI refuses this too and says
    // so with a dialog; this is the backstop for a hand run worker.
    if job.plan != Plan::FormatOnly && job.passes > 0 && disk.is_ssd() {
        return Err(format!(
            "full (multi-pass) wipe is blocked for solid state drives: {} is an SSD. \
             Overwriting an SSD does not securely erase it and only wears it out. \
             Choose a quick wipe, or pick a USB or hard drive.",
            disk.path
        ));
    }

    logger.say(&format!(
        "{} | {} | {} | {}",
        disk.title(),
        disk.size_human(),
        disk.bus_label(),
        disk.kind_tag()
    ));

    // Stage 1: get everything off the disk.
    logger.say(&format!("[1/{STAGE_COUNT}] {}", stage_name(1)));
    let mounted = device::mountpoints_on(&disk);
    if mounted.is_empty() {
        logger.say("  nothing is mounted on this disk");
    } else {
        logger.say(&format!("  unmounting {}", mounted.join(", ")));
        for error in partition::unmount_all(&mounted) {
            logger.say(&format!("  could not unmount {error}"));
        }
        logger.say("  done");
    }
    if control.stopped() {
        return Err("cancelled".into());
    }

    // Stage 2: old signatures.
    if job.plan == Plan::FormatOnly {
        logger.say(&format!("[2/{STAGE_COUNT}] {} - skipped", stage_name(2)));
    } else {
        logger.say(&format!("[2/{STAGE_COUNT}] {}", stage_name(2)));
        match partition::erase_signatures(device) {
            Ok(_) => logger.say("  removed any old filesystem signatures"),
            Err(e) => logger.say(&format!("  wipefs reported: {e}")),
        }
    }

    // Stage 3: the overwrite.
    if job.plan == Plan::FormatOnly {
        logger.say(&format!("[3/{STAGE_COUNT}] {} - skipped", stage_name(3)));
    } else if job.passes == 0 {
        logger.say(&format!("[3/{STAGE_COUNT}] {}", stage_name(3)));
        logger.say("  quick wipe selected, no overwrite passes");
    } else {
        logger.say(&format!("[3/{STAGE_COUNT}] {}", stage_name(3)));
        logger.say(&format!("  {}", wipe::pass_label(job.passes)));
        logger.say(&format!("  device is {}", human_size(disk.size_bytes)));
        let order: Vec<String> = wipe::pass_schedule(job.passes)
            .iter()
            .map(|pass| pass.name())
            .collect();
        logger.say(&format!("  pass order: {}", order.join(", ")));

        let mut last_report = Instant::now();
        let mut progress = |done: u64, total: u64| {
            let finished = total > 0 && done >= total;
            if finished || last_report.elapsed() >= Duration::from_secs(2) {
                let percent = if total == 0 {
                    0.0
                } else {
                    done as f64 / total as f64 * 100.0
                };
                logger.say(&format!(
                    "  {} of {} written ({percent:.1}%)",
                    human_size(done),
                    human_size(total)
                ));
                logger.mark(percent);
                last_report = Instant::now();
            }
        };

        let mut wipe_ctl = wipe::WipeCtl {
            progress: &mut progress,
            stop: control.stop_flag(),
            pause: control.pause_flag(),
        };
        wipe::overwrite(device, disk.size_bytes, job.passes, &mut wipe_ctl)?;
        logger.say("  overwrite complete");
    }

    if control.stopped() {
        return Err("cancelled".into());
    }

    if job.plan == Plan::WipeDevice {
        logger.say(&format!("[6/{STAGE_COUNT}] {}", stage_name(6)));
        match partition::verify_layout(device) {
            Ok(layout) => logger.say(&format!("  {layout}")),
            Err(e) => logger.say(&format!("  {e}")),
        }
        return Ok(());
    }

    // Stage 4: one partition spanning the device.
    logger.say(&format!("[4/{STAGE_COUNT}] {}", stage_name(4)));
    partition::write_single_partition_table(device, job.fs)?;
    let _ = partition::reread_table(device);
    logger.say("  wrote a DOS table with a single partition covering the device");

    // Stage 5: format it.
    logger.say(&format!("[5/{STAGE_COUNT}] {}", stage_name(5)));
    let partition_path = wait_for_partition(device, Duration::from_secs(10))?;
    logger.say(&format!(
        "  formatting {} as {}",
        partition_path.display(),
        job.fs.display()
    ));
    let label = if job.label.trim().is_empty() {
        None
    } else {
        Some(job.label.as_str())
    };
    partition::format(&partition_path, job.fs, label)?;
    logger.say("  filesystem created");

    // Stage 6: read it back.
    logger.say(&format!("[6/{STAGE_COUNT}] {}", stage_name(6)));
    match partition::verify_layout(device) {
        Ok(layout) => logger.say(&format!("  {layout}")),
        Err(e) => logger.say(&format!("  {e}")),
    }

    Ok(())
}

/// The kernel does not always publish `/dev/sda1` the instant the table lands,
/// so wait for the node to appear rather than failing the job on a race.
fn wait_for_partition(device: &Path, timeout: Duration) -> Result<PathBuf, String> {
    let node = partition::partition_node(device, 1);
    let deadline = Instant::now() + timeout;
    loop {
        if node.exists() {
            return Ok(node);
        }
        if Instant::now() >= deadline {
            return Err(format!(
                "{} never appeared; the kernel may need a rescan",
                node.display()
            ));
        }
        let _ = partition::reread_table(device);
        std::thread::sleep(Duration::from_millis(250));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A stand-in for a USB stick mounted at a path. It has to look like a USB
    /// device, not an internal SSD, or the solid state guard would refuse to
    /// run the very job under test.
    fn fake_usb_stick(path: &str, size: u64) -> Disk {
        Disk {
            name: "fakedisk".to_string(),
            path: path.to_string(),
            size_bytes: size,
            tran: "usb".to_string(),
            removable: true,
            hotplug: true,
            rotational: false,
            ..Default::default()
        }
    }

    #[test]
    fn plans_parse_from_names() {
        assert_eq!(Plan::from_name("wipe"), Some(Plan::WipeDevice));
        assert_eq!(Plan::from_name("format-only"), Some(Plan::FormatOnly));
        assert_eq!(
            Plan::from_name("Wipe-And-Format"),
            Some(Plan::WipeAndFormat)
        );
        assert_eq!(Plan::from_name("backup"), None);
    }

    #[test]
    fn stage_names_cover_every_stage() {
        for n in 1..=STAGE_COUNT {
            assert_ne!(stage_name(n), "Working");
        }
    }

    #[test]
    fn logger_creates_a_world_readable_file() {
        use std::os::unix::fs::PermissionsExt;
        let dir = std::env::temp_dir().join(format!("usb-reformat-log-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("worker.log");

        let logger = Logger::open(&path).unwrap();
        logger.say("hello");
        drop(logger);

        let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o644, "log should be readable by the GUI");

        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.contains("hello"));

        std::fs::remove_dir_all(&dir).ok();
    }

    /// A disk image is a block device as far as every tool we call is
    /// concerned, so the whole worker (logging, control files, wipefs, the
    /// overwrite loop, the verify step) can be run for real without root and
    /// without touching a physical disk.
    #[test]
    fn worker_wipes_an_image_end_to_end() {
        if !crate::util::have("wipefs") {
            eprintln!("wipefs not installed, skipping");
            return;
        }

        let dir = std::env::temp_dir().join(format!("usb-reformat-e2e-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let image = dir.join("fake-disk.img");
        let ctl = dir.join("ctl");
        std::fs::create_dir_all(&ctl).unwrap();

        let size = 4 * 1024 * 1024u64;
        let mut file = File::create(&image).unwrap();
        file.write_all(&vec![0xAAu8; size as usize]).unwrap();
        drop(file);

        let device_path = image.display().to_string();
        let fake = fake_usb_stick(&device_path, size);

        let job = Job {
            devices: vec![image.clone()],
            fs: FsKind::Exfat,
            label: "TEST".to_string(),
            passes: 1,
            plan: Plan::WipeDevice,
            log: dir.join("worker.log"),
            ctl: Some(ctl.clone()),
        };

        let logger = Logger::open(&job.log).unwrap();
        let control = Control::new(job.ctl.clone());
        run_one(
            &image,
            &job,
            std::slice::from_ref(&fake),
            None,
            &logger,
            &control,
        )
        .expect("wipe-only job should succeed on an image");

        let data = std::fs::read(&image).unwrap();
        assert_eq!(data.len() as u64, size);
        assert!(
            data.iter().all(|b| *b == 0x00),
            "the image should be zeroed after a 1 pass wipe"
        );

        let log = std::fs::read_to_string(&job.log).unwrap();
        assert!(log.contains("Preparing and unmounting"), "log: {log}");
        assert!(log.contains("Verifying the result"), "log: {log}");

        std::fs::remove_dir_all(&dir).ok();
    }

    /// The solid state guard must refuse a full wipe even when the worker is
    /// driven directly, not just from the GUI.
    #[test]
    fn worker_refuses_a_full_wipe_on_an_ssd() {
        let dir = std::env::temp_dir().join(format!("usb-reformat-ssd-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let image = dir.join("ssd.img");
        let size = 1024 * 1024u64;
        File::create(&image).unwrap().set_len(size).unwrap();

        let device_path = image.display().to_string();
        let ssd = Disk {
            name: "fakedisk".to_string(),
            path: device_path,
            size_bytes: size,
            tran: "nvme".to_string(),
            rotational: false,
            ..Default::default()
        };

        let job = Job {
            devices: vec![image.clone()],
            fs: FsKind::Exfat,
            label: String::new(),
            passes: 1,
            plan: Plan::WipeDevice,
            log: dir.join("worker.log"),
            ctl: None,
        };

        let logger = Logger::open(&job.log).unwrap();
        let control = Control::new(None);
        let error = run_one(
            &image,
            &job,
            std::slice::from_ref(&ssd),
            None,
            &logger,
            &control,
        )
        .expect_err("a full wipe on an SSD must be refused");
        assert!(error.contains("solid state"), "unexpected error: {error}");

        std::fs::remove_dir_all(&dir).ok();
    }

    /// A cancel file dropped into the control directory must stop the write.
    #[test]
    fn worker_stops_when_a_cancel_file_appears() {
        let dir =
            std::env::temp_dir().join(format!("usb-reformat-e2e-stop-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let image = dir.join("fake-disk.img");
        let ctl = dir.join("ctl");
        std::fs::create_dir_all(&ctl).unwrap();

        let size = 32 * 1024 * 1024u64;
        File::create(&image).unwrap().set_len(size).unwrap();
        std::fs::write(ctl.join("cancel"), b"").unwrap();

        let device_path = image.display().to_string();
        let fake = fake_usb_stick(&device_path, size);

        let job = Job {
            devices: vec![image.clone()],
            fs: FsKind::Exfat,
            label: String::new(),
            passes: 1,
            plan: Plan::WipeDevice,
            log: dir.join("worker.log"),
            ctl: Some(ctl.clone()),
        };

        let logger = Logger::open(&job.log).unwrap();
        let control = Control::new(job.ctl.clone());
        let result = run_one(
            &image,
            &job,
            std::slice::from_ref(&fake),
            None,
            &logger,
            &control,
        );
        assert!(result.is_err(), "a cancelled job must not report success");

        std::fs::remove_dir_all(&dir).ok();
    }
}
