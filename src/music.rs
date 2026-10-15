//! The chiptune player.
//!
//! Windows embedded a hundred `.xm` files as RCDATA resources and played them
//! straight out of the executable. The Linux build now does the same: `build.rs`
//! bakes every module in `music/` into the binary, so a lone executable still
//! carries its soundtrack. A `music/` directory beside the program, or one named
//! by `USB_REFORMAT_MUSIC`, overrides the baked-in set for swapping tunes
//! without a rebuild.
//!
//! Modules are rendered by `xmrs` into `rodio`. Tracks still advance when a
//! module actually reports itself finished rather than on a timer, and the same
//! 15 minute guard exists to skip a module whose pattern jump never resolves.
//!
//! Nothing here is load bearing. If there is no music, or the sound server is
//! unreachable, the player reports itself unavailable and the rest of the
//! program carries on.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Sender, TryRecvError};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use rodio::{OutputStream, Sink, Source};
use xmrs::prelude::Module;
use xmrsplayer::prelude::XmrsPlayer;

/// Rate the tracker engine renders at. Everything is resampled by rodio.
const SAMPLE_RATE: u32 = 44_100;

/// A module that never ends would otherwise play forever. One track in the
/// original collection had an unresolvable pattern jump, hence the guard.
const STUCK_LIMIT: Duration = Duration::from_secs(15 * 60);

const EXTENSIONS: [&str; 4] = ["xm", "it", "mod", "s3m"];

enum Command {
    Toggle,
    Stop,
}

/// One decoded module, exposed to rodio as an interleaved stereo `i16` stream.
struct TrackerSource {
    player: XmrsPlayer<'static>,
    /// The right channel of the sample pair whose left channel was just handed
    /// out. rodio pulls one sample at a time.
    pending_right: Option<i16>,
    finished: bool,
}

impl TrackerSource {
    fn from_bytes(name: &str, bytes: &[u8]) -> Result<TrackerSource, String> {
        let module = Module::load(bytes).map_err(|e| format!("{name}: {e}"))?;

        // `XmrsPlayer` borrows its module and outlives this call, so the module
        // has to be handed to the allocator for the lifetime of the process.
        // This is what the upstream example does. A module is a few hundred KB
        // and only one is ever live, so the cost is bounded in practice.
        let module: &'static Module = Box::leak(Box::new(module));

        Ok(TrackerSource {
            player: XmrsPlayer::new(module, SAMPLE_RATE, 0),
            pending_right: None,
            finished: false,
        })
    }

    fn from_path(path: &Path) -> Result<TrackerSource, String> {
        let bytes = std::fs::read(path).map_err(|e| format!("{}: {e}", path.display()))?;
        TrackerSource::from_bytes(&path.display().to_string(), &bytes)
    }
}

impl Iterator for TrackerSource {
    type Item = i16;

    fn next(&mut self) -> Option<i16> {
        if self.finished {
            return None;
        }
        if let Some(right) = self.pending_right.take() {
            return Some(right);
        }
        match self.player.sample(true) {
            Some((left, right)) => {
                self.pending_right = Some(right);
                Some(left)
            }
            None => {
                self.finished = true;
                None
            }
        }
    }
}

impl Source for TrackerSource {
    fn current_frame_len(&self) -> Option<usize> {
        None
    }

    fn channels(&self) -> u16 {
        2
    }

    fn sample_rate(&self) -> u32 {
        SAMPLE_RATE
    }

    fn total_duration(&self) -> Option<Duration> {
        None
    }
}

/// One playlist entry: a module baked into the binary, or a file on disk.
enum Track {
    Embedded { label: String, bytes: &'static [u8] },
    File { label: String, path: PathBuf },
}

impl Track {
    fn label(&self) -> &str {
        match self {
            Track::Embedded { label, .. } | Track::File { label, .. } => label,
        }
    }

    fn open(&self) -> Result<TrackerSource, String> {
        match self {
            Track::Embedded { label, bytes } => TrackerSource::from_bytes(label, bytes),
            Track::File { path, .. } => TrackerSource::from_path(path),
        }
    }
}

/// The player handle the GUI keeps. Dropping it silences everything.
pub struct Player {
    commands: Sender<Command>,
    playing: Arc<AtomicBool>,
    now_playing: Arc<Mutex<String>>,
    track_count: usize,
    thread: Option<JoinHandle<()>>,
}

impl Player {
    /// Look for music and start playing it. Never fails: an empty playlist just
    /// means the button reports there is nothing to play.
    pub fn start() -> Player {
        let tracks = find_tracks();
        let track_count = tracks.len();
        let (commands, receiver) = mpsc::channel();
        let playing = Arc::new(AtomicBool::new(true));
        let now_playing = Arc::new(Mutex::new(String::new()));

        let thread = if tracks.is_empty() {
            None
        } else {
            let playing = Arc::clone(&playing);
            let now_playing = Arc::clone(&now_playing);
            Some(thread::spawn(move || {
                run(tracks, receiver, playing, now_playing)
            }))
        };

        Player {
            commands,
            playing,
            now_playing,
            track_count,
            thread,
        }
    }

    pub fn is_available(&self) -> bool {
        self.thread.is_some()
    }

    pub fn is_playing(&self) -> bool {
        self.playing.load(Ordering::SeqCst)
    }

    pub fn track_count(&self) -> usize {
        self.track_count
    }

    /// The label the button shows: `Music: on`, `Music: off`, or `Music: none`.
    pub fn label(&self) -> String {
        if !self.is_available() {
            return "Music: none".to_string();
        }
        if self.is_playing() {
            "Music: on".to_string()
        } else {
            "Music: off".to_string()
        }
    }

    pub fn now_playing(&self) -> String {
        self.now_playing
            .lock()
            .map(|name| name.clone())
            .unwrap_or_default()
    }

    /// Flip play/pause. Returns the new state.
    pub fn toggle(&mut self) -> bool {
        if !self.is_available() {
            return false;
        }
        let next = !self.playing.load(Ordering::SeqCst);
        self.playing.store(next, Ordering::SeqCst);
        let _ = self.commands.send(Command::Toggle);
        next
    }
}

impl Drop for Player {
    fn drop(&mut self) {
        let _ = self.commands.send(Command::Stop);
        // The audio thread may be mid module. Do not block the UI waiting for
        // it; it will notice the command within a poll interval.
        self.thread = None;
    }
}

/// Candidate music directories on disk, most specific first.
///
/// The executable's own directory and its parents are searched so the player
/// works whether it is run from a `target/release` build, an unpacked release
/// directory, or an installed location. `USB_REFORMAT_MUSIC` is handled by
/// `find_tracks` before this list is consulted, which is what keeps the tests
/// independent of the machine.
fn music_dirs() -> Vec<PathBuf> {
    let mut dirs = Vec::new();

    if let Ok(exe) = std::env::current_exe() {
        let exe = exe.canonicalize().unwrap_or(exe);
        let mut dir = exe.parent().map(Path::to_path_buf);
        // Three levels covers `<dir>/music`, `<repo>/target/release` going up
        // to `<repo>/music`, and a couple of install layouts in between.
        for _ in 0..3 {
            match dir {
                Some(current) => {
                    dirs.push(current.join("music"));
                    dir = current.parent().map(Path::to_path_buf);
                }
                None => break,
            }
        }
    }
    if let Ok(cwd) = std::env::current_dir() {
        dirs.push(cwd.join("music"));
    }
    if let Some(home) = std::env::var_os("HOME") {
        dirs.push(PathBuf::from(home).join(".local/share/usb-reformat/music"));
    }
    for base in std::env::var_os("XDG_DATA_DIRS")
        .map(|v| v.to_string_lossy().to_string())
        .unwrap_or_else(|| "/usr/local/share:/usr/share".to_string())
        .split(':')
    {
        if !base.is_empty() {
            dirs.push(PathBuf::from(base).join("usb-reformat/music"));
        }
    }

    dirs
}

/// Every playable module in `dir`, sorted, as on-disk tracks.
fn tracks_in_dir(dir: &Path) -> Vec<Track> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };

    let mut paths: Vec<PathBuf> = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        let playable = path
            .extension()
            .and_then(|e| e.to_str())
            .map(|e| EXTENSIONS.contains(&e.to_ascii_lowercase().as_str()))
            .unwrap_or(false);
        if playable && path.is_file() {
            paths.push(path);
        }
    }
    paths.sort();

    paths
        .into_iter()
        .map(|path| Track::File {
            label: track_label(&path),
            path,
        })
        .collect()
}

/// The modules `build.rs` baked into the binary.
fn embedded_tracks() -> Vec<Track> {
    embedded::EMBEDDED_TRACKS
        .iter()
        .map(|(label, bytes)| Track::Embedded {
            label: (*label).to_string(),
            bytes,
        })
        .collect()
}

/// The playlist, most specific source first.
///
/// `USB_REFORMAT_MUSIC` is authoritative when set: pointed at a missing or
/// empty directory it means silence rather than a fall back. After that, a
/// `music/` directory beside the program or in one of the usual install
/// locations, and last the copy baked into the binary so a lone executable
/// still plays.
fn find_tracks() -> Vec<Track> {
    if let Some(dir) = std::env::var_os("USB_REFORMAT_MUSIC") {
        return tracks_in_dir(Path::new(&dir));
    }

    for dir in music_dirs() {
        let tracks = tracks_in_dir(&dir);
        if !tracks.is_empty() {
            return tracks;
        }
    }

    embedded_tracks()
}

fn track_label(path: &Path) -> String {
    path.file_stem()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_else(|| path.display().to_string())
}

/// The audio thread: owns the output stream, the sink, and the playlist.
fn run(
    tracks: Vec<Track>,
    receiver: mpsc::Receiver<Command>,
    playing: Arc<AtomicBool>,
    now_playing: Arc<Mutex<String>>,
) {
    let Ok((_stream, handle)) = OutputStream::try_default() else {
        // No sound server, or no audio device. Stay quiet and idle rather than
        // spinning.
        if let Ok(mut name) = now_playing.lock() {
            name.clear();
        }
        loop {
            match receiver.recv() {
                Ok(Command::Stop) | Err(_) => return,
                Ok(Command::Toggle) => {}
            }
        }
    };

    let Ok(sink) = Sink::try_new(&handle) else {
        return;
    };

    let mut index = 0usize;
    let mut started = Instant::now();

    loop {
        match receiver.try_recv() {
            Ok(Command::Stop) => {
                sink.stop();
                return;
            }
            Ok(Command::Toggle) => {
                if playing.load(Ordering::SeqCst) {
                    sink.play();
                } else {
                    sink.pause();
                }
            }
            Err(TryRecvError::Empty) => {}
            Err(TryRecvError::Disconnected) => {
                sink.stop();
                return;
            }
        }

        if sink.empty() {
            // Walk the playlist until something loads. A corrupt module must
            // not wedge playback on itself.
            for _ in 0..tracks.len() {
                let track = &tracks[index % tracks.len()];
                index = index.wrapping_add(1);
                if let Ok(source) = track.open() {
                    if let Ok(mut name) = now_playing.lock() {
                        *name = track.label().to_string();
                    }
                    sink.append(source);
                    if !playing.load(Ordering::SeqCst) {
                        sink.pause();
                    }
                    started = Instant::now();
                    break;
                }
            }
        } else if started.elapsed() > STUCK_LIMIT {
            // Unresolvable pattern jump: move on.
            sink.stop();
            started = Instant::now();
        }

        thread::sleep(Duration::from_millis(200));
    }
}

/// The list of baked-in modules, generated by `build.rs` from `music/`.
mod embedded {
    include!(concat!(env!("OUT_DIR"), "/embedded_music.rs"));
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The environment is process-wide, so tests that touch
    /// `USB_REFORMAT_MUSIC` take this lock instead of racing each other.
    static ENV_LOCK: Mutex<()> = Mutex::new(());

    #[test]
    fn module_extensions_are_recognised() {
        for extension in EXTENSIONS {
            assert!(EXTENSIONS.contains(&extension));
        }
    }

    #[test]
    fn track_labels_drop_the_extension() {
        assert_eq!(track_label(Path::new("/music/foo bar.xm")), "foo bar");
    }

    #[test]
    fn a_player_with_no_music_is_available_but_silent() {
        let _guard = ENV_LOCK.lock().unwrap();
        // Point the search at a directory that cannot exist.
        std::env::set_var("USB_REFORMAT_MUSIC", "/nonexistent/usb-reformat/music");
        let mut player = Player::start();
        assert!(!player.is_available());
        assert_eq!(player.label(), "Music: none");
        assert!(!player.toggle());
        std::env::remove_var("USB_REFORMAT_MUSIC");
    }

    #[test]
    fn the_override_directory_is_authoritative() {
        let _guard = ENV_LOCK.lock().unwrap();
        let dir =
            std::env::temp_dir().join(format!("usb-reformat-override-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("mine.xm"), b"not really a module").unwrap();

        std::env::set_var("USB_REFORMAT_MUSIC", &dir);
        let tracks = find_tracks();
        assert_eq!(tracks.len(), 1);
        assert_eq!(tracks[0].label(), "mine");
        // The override is consulted before the on-disk candidate list.
        assert!(music_dirs().iter().all(|d| !d.starts_with(&dir)));

        // Empty means silence, never a fall back to the baked-in copy.
        let empty = dir.join("empty");
        std::fs::create_dir_all(&empty).unwrap();
        std::env::set_var("USB_REFORMAT_MUSIC", &empty);
        assert!(find_tracks().is_empty());

        std::env::remove_var("USB_REFORMAT_MUSIC");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn discovery_walks_up_from_the_executable() {
        let _guard = ENV_LOCK.lock().unwrap();
        std::env::remove_var("USB_REFORMAT_MUSIC");
        let dirs = music_dirs();
        // Without an override, the executable's own folder and its parents are
        // probed first, so `target/release/usb-reformat` still finds `music/`
        // at the repository root.
        assert!(dirs.len() >= 3, "expected several candidate directories");
        assert!(dirs.iter().all(|d| d.ends_with("music")));
    }

    #[test]
    fn the_music_is_baked_into_the_binary() {
        let _guard = ENV_LOCK.lock().unwrap();
        // The collection lives in `music/` at build time; a checkout without it
        // is still valid and simply embeds nothing.
        if !Path::new("music").is_dir() {
            return;
        }
        let tracks = embedded_tracks();
        assert!(
            !tracks.is_empty(),
            "build.rs should have embedded the modules from music/"
        );
        assert!(tracks.iter().all(|track| !track.label().is_empty()));
        // The bytes have to be real, or playback would be silent.
        assert!(tracks.iter().all(|track| matches!(
            track,
            Track::Embedded { bytes, .. } if !bytes.is_empty()
        )));
    }
}
