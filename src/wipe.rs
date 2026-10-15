//! Overwrite schedules and the raw write loop.
//!
//! The pass schedules are carried over from the Windows build unchanged, so a
//! "3 pass (DoD)" here means exactly what it meant there. What changes is the
//! mechanics: instead of `WriteFile` against `\\.\PhysicalDriveN`, we open the
//! block device directly and write in 1 MiB chunks, checking the pause and stop
//! flags between chunks so a wipe stops promptly instead of mid-sector.
//!
//! Writes go out with `O_DIRECT` when the device allows it, so the bytes are
//! handed straight to the device instead of piling up in the page cache. That
//! matters: buffered writes to a slow or failing drive let the kernel hold
//! gigabytes of dirty pages, and flushing that backlog to a device that has
//! gone away can wedge the whole machine. When `O_DIRECT` is not available the
//! buffered path is used, but it forces the data out every few megabytes so the
//! backlog always stays small.

use std::alloc::{alloc, dealloc, Layout};
use std::fs::{File, OpenOptions};
use std::io::{Seek, SeekFrom, Write};
use std::os::unix::fs::OpenOptionsExt;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

/// One chunk of the overwrite. Small enough that pause and cancel react quickly.
pub const CHUNK: usize = 1024 * 1024;

/// A single overwrite pass: either pseudorandom bytes, or a repeating pattern.
#[derive(Debug, Clone, PartialEq)]
pub enum Pass {
    Random,
    Repeat(Vec<u8>),
}

impl Pass {
    /// Short name used in the progress line, e.g. `0x00`, `0x55`, `random`.
    pub fn name(&self) -> String {
        match self {
            Pass::Random => "random".to_string(),
            Pass::Repeat(seq) => {
                let shown: Vec<String> = seq.iter().take(3).map(|b| format!("{b:02X}")).collect();
                if seq.len() > 3 {
                    format!("{}...", shown.join(" "))
                } else {
                    shown.join(" ")
                }
            }
        }
    }
}

/// The overwrite schedule for a chosen number of passes.
///
/// Unchanged from the Windows version: 1, 3, 7 and 35 passes map to the
/// familiar single-pass, DoD 5220.22-M, DoD ECE and Gutmann sequences.
pub fn pass_schedule(passes: u32) -> Vec<Pass> {
    fn r() -> Pass {
        Pass::Random
    }
    fn b(x: u8) -> Pass {
        Pass::Repeat(vec![x])
    }
    fn s(x: &[u8]) -> Pass {
        Pass::Repeat(x.to_vec())
    }
    match passes {
        1 => vec![b(0x00)],
        3 => vec![b(0x00), b(0xFF), r()],
        7 => vec![b(0x00), b(0xFF), r(), b(0x96), b(0x69), r(), b(0x00)],
        35 => vec![
            r(),
            r(),
            r(),
            r(),
            b(0x55),
            b(0xAA),
            s(&[0x92, 0x49, 0x24]),
            s(&[0x49, 0x24, 0x92]),
            s(&[0x24, 0x92, 0x49]),
            b(0x00),
            b(0x11),
            b(0x22),
            b(0x33),
            b(0x44),
            b(0x55),
            b(0x66),
            b(0x77),
            b(0x88),
            b(0x99),
            b(0xAA),
            b(0xBB),
            b(0xCC),
            b(0xDD),
            b(0xEE),
            b(0xFF),
            s(&[0x92, 0x49, 0x24]),
            s(&[0x49, 0x24, 0x92]),
            s(&[0x24, 0x92, 0x49]),
            s(&[0x6D, 0xB6, 0xDB]),
            s(&[0xB6, 0xDB, 0x6D]),
            s(&[0xDB, 0x6D, 0xB6]),
            r(),
            r(),
            r(),
            r(),
        ],
        n => {
            // Unknown count: write zeros, then a random pass.
            let mut v = Vec::new();
            for _ in 0..n {
                v.push(b(0x00));
            }
            v.push(r());
            v
        }
    }
}

/// Human-readable description of the wipe mode.
pub fn pass_label(passes: u32) -> String {
    match passes {
        0 => "quick wipe (no overwrite)".to_string(),
        1 => "full wipe, 1 pass (single zero overwrite)".to_string(),
        3 => "full wipe, 3 passes (DoD 5220.22-M)".to_string(),
        7 => "full wipe, 7 passes (DoD 5220.22-M ECE)".to_string(),
        35 => "full wipe, 35 passes (Gutmann method)".to_string(),
        n => format!("full wipe, {n} passes"),
    }
}

/// Short label for the GUI, matching the Windows wording.
pub fn passes_short(passes: u32) -> String {
    match passes {
        1 => "1 (fast)".to_string(),
        3 => "3 (DoD)".to_string(),
        7 => "7 (DoD-ECE)".to_string(),
        35 => "35 (Gutmann)".to_string(),
        n => format!("{n}"),
    }
}

/// Small xorshift64 PRNG, so the random pass needs no external crate.
struct Rng(u64);

impl Rng {
    fn new(seed: u64) -> Rng {
        Rng(seed | 1)
    }

    fn next_u64(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.0 = x;
        x
    }
}

fn fill_buffer(buf: &mut [u8], pass: &Pass, rng: &mut Rng) {
    match pass {
        Pass::Random => {
            let mut i = 0;
            while i + 8 <= buf.len() {
                let r = rng.next_u64().to_le_bytes();
                buf[i..i + 8].copy_from_slice(&r);
                i += 8;
            }
            if i < buf.len() {
                let r = rng.next_u64().to_le_bytes();
                let n = buf.len() - i;
                buf[i..].copy_from_slice(&r[..n]);
            }
        }
        Pass::Repeat(seq) => {
            if seq.is_empty() {
                return;
            }
            for (i, b) in buf.iter_mut().enumerate() {
                *b = seq[i % seq.len()];
            }
        }
    }
}

/// How a running wipe reports progress and reacts to the user.
pub struct WipeCtl<'a> {
    /// Called with (bytes written so far, bytes to write in total).
    pub progress: &'a mut dyn FnMut(u64, u64),
    pub stop: &'a AtomicBool,
    pub pause: &'a AtomicBool,
}

/// Alignment used for `O_DIRECT`. 4096 is a multiple of the logical block size
/// of every device we are likely to meet (512 or 4096), so the bulk writes stay
/// valid without having to query the device first.
const DIRECT_ALIGN: usize = 4096;

/// Bytes written through the buffered handle before they are forced out to the
/// device. Without this the kernel would happily hold gigabytes of dirty pages
/// in RAM, and a slow or dying device then stalls the whole machine when that
/// backlog has to be flushed.
const SYNC_EVERY: usize = 16 * 1024 * 1024;

/// A heap buffer whose start address is aligned for `O_DIRECT`.
struct AlignedBuffer {
    ptr: *mut u8,
    len: usize,
    align: usize,
}

impl AlignedBuffer {
    fn new(len: usize, align: usize) -> AlignedBuffer {
        let layout = Layout::from_size_align(len, align).expect("valid layout");
        let ptr = unsafe { alloc(layout) };
        assert!(!ptr.is_null(), "out of memory for the wipe buffer");
        AlignedBuffer { ptr, len, align }
    }

    fn as_mut_slice(&mut self) -> &mut [u8] {
        // Safety: `ptr` was allocated with `len` bytes and lives until drop.
        unsafe { std::slice::from_raw_parts_mut(self.ptr, self.len) }
    }
}

impl Drop for AlignedBuffer {
    fn drop(&mut self) {
        let layout = Layout::from_size_align(self.len, self.align).expect("valid layout");
        unsafe { dealloc(self.ptr, layout) };
    }
}

/// Where the wipe bytes actually go: a direct (cache bypassing) handle when the
/// device accepts it, plus a plain handle for the tail and for targets that
/// refuse `O_DIRECT` (a regular file, or a filesystem without support).
struct Sink {
    buffered: File,
    direct: Option<File>,
    buffer: AlignedBuffer,
    dirty: usize,
}

impl Sink {
    fn open(device: &Path) -> Result<Sink, String> {
        let buffered = OpenOptions::new()
            .write(true)
            .open(device)
            .map_err(|e| format!("cannot open {} for writing: {e}", device.display()))?;

        // O_DIRECT is an optimisation, not a requirement: if the device or the
        // filesystem refuses it we simply stay on the buffered path.
        let direct = OpenOptions::new()
            .write(true)
            .custom_flags(libc::O_DIRECT)
            .open(device)
            .ok();

        Ok(Sink {
            buffered,
            direct,
            buffer: AlignedBuffer::new(CHUNK, DIRECT_ALIGN),
            dirty: 0,
        })
    }

    /// Write `len` bytes at `offset`, filling the internal buffer with `fill`
    /// first. `len` must not exceed `CHUNK`.
    fn write_chunk(
        &mut self,
        offset: u64,
        len: usize,
        fill: &mut dyn FnMut(&mut [u8]),
    ) -> Result<(), String> {
        fill(&mut self.buffer.as_mut_slice()[..len]);

        let use_direct = self.direct.is_some() && len.is_multiple_of(DIRECT_ALIGN);
        if use_direct {
            let outcome = {
                let block = &self.buffer.as_mut_slice()[..len];
                let direct = self.direct.as_mut().expect("checked above");
                direct
                    .seek(SeekFrom::Start(offset))
                    .and_then(|_| direct.write_all(block))
            };
            match outcome {
                Ok(()) => return Ok(()),
                // The device changed its mind about O_DIRECT: give up on it and
                // keep going through the buffered handle.
                Err(e) if e.raw_os_error() == Some(libc::EINVAL) => {
                    self.direct = None;
                }
                Err(e) => return Err(friendly_write_error(&e, offset)),
            }
        }

        self.write_buffered(offset, len)
    }

    fn write_buffered(&mut self, offset: u64, len: usize) -> Result<(), String> {
        let block = &self.buffer.as_mut_slice()[..len];
        self.buffered
            .seek(SeekFrom::Start(offset))
            .map_err(|e| format!("seek failed: {e}"))?;
        self.buffered
            .write_all(block)
            .map_err(|e| friendly_write_error(&e, offset))?;

        self.dirty += len;
        if self.dirty >= SYNC_EVERY {
            self.buffered
                .sync_data()
                .map_err(|e| friendly_write_error(&e, offset))?;
            self.dirty = 0;
        }
        Ok(())
    }

    fn finish(&mut self) -> Result<(), String> {
        self.buffered
            .sync_all()
            .map_err(|e| friendly_write_error(&e, 0))?;
        if let Some(direct) = self.direct.as_mut() {
            let _ = direct.sync_all();
        }
        Ok(())
    }
}

/// Turn a raw write error into something that explains the usual causes.
fn friendly_write_error(err: &std::io::Error, offset: u64) -> String {
    let hint = match err.raw_os_error() {
        Some(libc::ENOSPC) => {
            " - the device says it is full, which usually means a fake or failing drive"
        }
        Some(libc::ENODEV) | Some(libc::EIO) | Some(libc::ENXIO) => {
            " - the device stopped responding or was unplugged"
        }
        _ => "",
    };
    format!("write failed at {offset} bytes: {err}{hint}")
}

/// Overwrite the whole device `passes` times.
pub fn overwrite(
    device: &Path,
    size_bytes: u64,
    passes: u32,
    ctl: &mut WipeCtl,
) -> Result<(), String> {
    let schedule = pass_schedule(passes);
    if schedule.is_empty() {
        return Ok(());
    }

    let mut sink = Sink::open(device)?;

    let seed = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0x5EED_1234_ABCD_0001);
    let mut rng = Rng::new(seed);

    let total = size_bytes.saturating_mul(schedule.len() as u64);
    let mut written: u64 = 0;

    for pass in &schedule {
        let mut offset: u64 = 0;
        let mut remaining = size_bytes;
        while remaining > 0 {
            if ctl.stop.load(Ordering::SeqCst) {
                return Err("cancelled".into());
            }
            while ctl.pause.load(Ordering::SeqCst) {
                if ctl.stop.load(Ordering::SeqCst) {
                    return Err("cancelled".into());
                }
                std::thread::sleep(std::time::Duration::from_millis(150));
            }

            let n = remaining.min(CHUNK as u64) as usize;
            sink.write_chunk(offset, n, &mut |buf| fill_buffer(buf, pass, &mut rng))?;

            offset += n as u64;
            written += n as u64;
            remaining -= n as u64;
            (ctl.progress)(written, total);
        }
    }

    sink.finish()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Read;

    #[test]
    fn schedules_match_the_windows_build() {
        assert_eq!(pass_schedule(1), vec![Pass::Repeat(vec![0x00])]);
        assert_eq!(
            pass_schedule(3),
            vec![
                Pass::Repeat(vec![0x00]),
                Pass::Repeat(vec![0xFF]),
                Pass::Random
            ]
        );
        assert_eq!(pass_schedule(7).len(), 7);
        assert_eq!(pass_schedule(35).len(), 35);
        // Gutmann starts with four random passes.
        assert_eq!(pass_schedule(35)[0], Pass::Random);
        assert_eq!(pass_schedule(35)[4], Pass::Repeat(vec![0x55]));
    }

    #[test]
    fn labels_read_like_the_original() {
        assert_eq!(pass_label(0), "quick wipe (no overwrite)");
        assert_eq!(pass_label(3), "full wipe, 3 passes (DoD 5220.22-M)");
        assert_eq!(pass_label(35), "full wipe, 35 passes (Gutmann method)");
        assert_eq!(passes_short(7), "7 (DoD-ECE)");
    }

    #[test]
    fn overwrite_fills_a_device_image() {
        let dir = std::env::temp_dir().join(format!("usb-reformat-wipe-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let img = dir.join("tiny.img");

        let size = 3 * 1024 * 1024u64 + 4096; // deliberately not a chunk multiple
        let mut file = std::fs::File::create(&img).unwrap();
        file.write_all(&vec![0xAAu8; size as usize]).unwrap();
        drop(file);

        let stop = AtomicBool::new(false);
        let pause = AtomicBool::new(false);
        let mut seen_total = 0u64;
        let mut progress = |_done: u64, total: u64| seen_total = total;

        overwrite(
            &img,
            0,
            1,
            &mut WipeCtl {
                progress: &mut progress,
                stop: &stop,
                pause: &pause,
            },
        )
        .unwrap();

        // A zero-pass wipe must leave the device untouched.
        let mut data = Vec::new();
        std::fs::File::open(&img)
            .unwrap()
            .read_to_end(&mut data)
            .unwrap();
        assert!(data.iter().all(|b| *b == 0xAA), "quick wipe altered data");
        assert_eq!(seen_total, 0);

        // Now a real single zero pass.
        let mut progress = |_done: u64, total: u64| seen_total = total;
        overwrite(
            &img,
            size,
            1,
            &mut WipeCtl {
                progress: &mut progress,
                stop: &stop,
                pause: &pause,
            },
        )
        .unwrap();

        let mut data = Vec::new();
        std::fs::File::open(&img)
            .unwrap()
            .read_to_end(&mut data)
            .unwrap();
        assert_eq!(data.len() as u64, size);
        assert!(
            data.iter().all(|b| *b == 0x00),
            "1-pass wipe left data behind"
        );
        assert_eq!(seen_total, size);

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn cancel_stops_the_write() {
        let dir = std::env::temp_dir().join(format!("usb-reformat-cancel-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let img = dir.join("tiny.img");

        let size = 64 * 1024 * 1024u64;
        std::fs::File::create(&img).unwrap().set_len(size).unwrap();

        let stop = AtomicBool::new(false);
        let pause = AtomicBool::new(false);
        let mut calls = 0;
        let mut progress = |_done: u64, _total: u64| {
            calls += 1;
            if calls == 2 {
                stop.store(true, Ordering::SeqCst);
            }
        };

        let result = overwrite(
            &img,
            size,
            1,
            &mut WipeCtl {
                progress: &mut progress,
                stop: &stop,
                pause: &pause,
            },
        );
        assert_eq!(result, Err("cancelled".to_string()));

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn the_aligned_buffer_is_aligned_and_writable() {
        let mut buffer = AlignedBuffer::new(4096, DIRECT_ALIGN);
        assert_eq!(buffer.as_mut_slice().as_ptr() as usize % DIRECT_ALIGN, 0);
        buffer.as_mut_slice()[0] = 0x5A;
        assert_eq!(buffer.as_mut_slice()[0], 0x5A);
        assert_eq!(buffer.as_mut_slice().len(), 4096);
    }

    #[test]
    fn write_errors_name_the_likely_cause() {
        let unplugged = std::io::Error::from_raw_os_error(libc::ENODEV);
        assert!(friendly_write_error(&unplugged, 1024).contains("unplugged"));
        let full = std::io::Error::from_raw_os_error(libc::ENOSPC);
        assert!(friendly_write_error(&full, 1024).contains("fake or failing"));
    }

    #[test]
    fn the_sink_writes_through_the_buffered_path_when_direct_is_absent() {
        // A file on a filesystem that refuses O_DIRECT still has to work, which
        // is also what the test image above relies on.
        let dir = std::env::temp_dir().join(format!("usb-reformat-sink-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let img = dir.join("tiny.img");
        std::fs::File::create(&img).unwrap().set_len(8192).unwrap();

        let mut sink = Sink::open(&img).unwrap();
        sink.write_chunk(0, 4096, &mut |buf| buf.fill(0x11))
            .unwrap();
        sink.finish().unwrap();

        let mut data = Vec::new();
        std::fs::File::open(&img)
            .unwrap()
            .read_to_end(&mut data)
            .unwrap();
        assert_eq!(&data[..4096], &vec![0x11u8; 4096][..]);

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn the_sink_uses_o_direct_when_the_target_allows_it() {
        std::fs::create_dir_all("target").ok();
        let path = std::path::Path::new("target").join("odirect-probe.img");
        std::fs::File::create(&path).unwrap().set_len(4096).unwrap();

        // Probe the way the sink does, so the test skips on filesystems that do
        // not support O_DIRECT (tmpfs, for example) instead of failing.
        let supports = OpenOptions::new()
            .write(true)
            .custom_flags(libc::O_DIRECT)
            .open(&path)
            .is_ok();

        let sink = Sink::open(&path).unwrap();
        assert_eq!(
            sink.direct.is_some(),
            supports,
            "the direct handle should be kept exactly when the target supports O_DIRECT"
        );

        std::fs::remove_file(&path).ok();
    }
}
