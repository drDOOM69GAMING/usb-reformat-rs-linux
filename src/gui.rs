//! The window: a Windows 98 style dialog, drawn with GTK 4 and styled to match.
//!
//! The layout, the wording and the workflow are carried over from the Windows
//! build: the same drive list, the same "Type WIPE to confirm" gate, the same
//! Pause and Cancel buttons, and the same etched group boxes.
//!
//! As in the original, this process runs as your normal user. Only the worker
//! it launches through `pkexec` holds root, so one authorisation prompt covers
//! the whole job and the freshly formatted drive is seen by your desktop
//! session rather than by root's.

use std::cell::{Cell, RefCell};
use std::os::unix::process::ExitStatusExt;
use std::path::PathBuf;
use std::rc::Rc;
use std::time::Duration;

use gtk::prelude::*;
use gtk4 as gtk;

use crate::device::{self, Disk};
use crate::partition::FsKind;
use crate::privs;

const APP_ID: &str = "org.wayneamd.UsbReformat";
const WINDOW_TITLE: &str = "RE-Formatter";

const FULL_WIPE_OFF: &str = "Full wipe (slow) - click to choose passes.  Unticked = quick wipe.";

const READY: &str = "Ready. Select a drive, tick the box, type WIPE, then click WIPE DRIVE(S).";

/// Every colour here is the real Windows 98 palette, not a modern guess. The
/// button face was the warmer #D4D0C8, not the #F0F0F0 that Windows 2000 and
/// later report.
const CSS: &str = "
window {
  background-color: #d4d0c8;
  color: #000000;
  font-family: \"MS Sans Serif\", \"Tahoma\", \"DejaVu Sans\", sans-serif;
  font-size: 11px;
}
label { color: #000000; }
button {
  background-image: none;
  background-color: #d4d0c8;
  color: #000000;
  border-width: 2px;
  border-style: solid;
  border-color: #ffffff #808080 #808080 #ffffff;
  border-radius: 0;
  box-shadow: 1px 1px 0 0 #000000;
  padding: 3px 10px;
  min-height: 18px;
}
button:hover { background-color: #dcd8d0; }
button:active, button:checked {
  border-color: #808080 #ffffff #ffffff #808080;
  box-shadow: none;
  padding: 4px 9px 2px 11px;
}
button:disabled { color: #808080; box-shadow: 1px 1px 0 0 #a0a0a0; }
entry {
  background-color: #ffffff;
  color: #000000;
  border-width: 2px;
  border-style: solid;
  border-color: #808080 #ffffff #ffffff #808080;
  border-radius: 0;
  box-shadow: 1px 1px 0 0 #000000;
  padding: 1px 3px;
  min-height: 18px;
}
frame > border { border: 1px solid #808080; border-radius: 0; }
frame > label { background-color: #d4d0c8; color: #000000; padding: 0 4px; }
list { background-color: #ffffff; color: #000000; }
list > row { background-color: #ffffff; color: #000000; padding: 1px 4px; border-radius: 0; }
list > row label { color: #000000; font-family: \"DejaVu Sans Mono\", \"Liberation Mono\", monospace; font-size: 10px; }
list > row:selected { background-color: #000080; }
list > row:selected label { color: #ffffff; }
scrolledwindow {
  background-color: #ffffff;
  border-width: 2px;
  border-style: solid;
  border-color: #808080 #ffffff #ffffff #808080;
  border-radius: 0;
}
textview, textview > text {
  background-color: #ffffff;
  color: #000000;
  font-family: \"DejaVu Sans Mono\", \"Liberation Mono\", monospace;
  font-size: 10px;
}
progressbar > trough {
  background-color: #ffffff;
  border-width: 2px;
  border-style: solid;
  border-color: #808080 #ffffff #ffffff #808080;
  border-radius: 0;
  padding: 1px;
  min-height: 18px;
}
progressbar progress {
  background-color: #000080;
  background-image: repeating-linear-gradient(to right, #000080 0px, #000080 9px, #ffffff 9px, #ffffff 11px);
  border-radius: 0;
  min-height: 16px;
}
label.status {
  background-color: #d4d0c8;
  border-width: 2px;
  border-style: solid;
  border-color: #808080 #ffffff #ffffff #808080;
  padding: 2px 6px;
}
label.title { font-weight: bold; font-size: 12px; }
checkbutton check {
  background-color: #ffffff;
  border-width: 1px;
  border-style: solid;
  border-color: #808080 #ffffff #ffffff #808080;
  border-radius: 0;
  min-width: 13px;
  min-height: 13px;
}
checkbutton check:checked { background-color: #ffffff; color: #000080; }
radiobutton radio {
  background-color: #ffffff;
  border-width: 1px;
  border-style: solid;
  border-color: #808080 #ffffff #ffffff #808080;
  border-radius: 8px;
  min-width: 12px;
  min-height: 12px;
}
radiobutton radio:checked { background-color: #ffffff; color: #000080; }
window.dark { background-color: #1f1f1f; color: #ffffff; }
window.dark label { color: #ffffff; }
window.dark button {
  background-color: #2d2d2d;
  color: #ffffff;
  border-color: #606060 #101010 #101010 #606060;
  box-shadow: 1px 1px 0 0 #000000;
}
window.dark button:hover { background-color: #3a3a3a; }
window.dark entry { background-color: #2d2d2d; color: #ffffff; border-color: #101010 #606060 #606060 #101010; }
window.dark frame > border { border-color: #606060; }
window.dark frame > label { background-color: #1f1f1f; color: #ffffff; }
window.dark list { background-color: #2d2d2d; color: #ffffff; }
window.dark list > row { background-color: #2d2d2d; color: #ffffff; }
window.dark list > row label { color: #ffffff; }
window.dark list > row:selected { background-color: #4040a0; }
window.dark list > row:selected label { color: #ffffff; }
window.dark scrolledwindow { background-color: #2d2d2d; border-color: #101010 #606060 #606060 #101010; }
window.dark textview, window.dark textview > text { background-color: #2d2d2d; color: #ffffff; }
window.dark progressbar > trough { background-color: #3a3a3a; border-color: #101010 #606060 #606060 #101010; }
window.dark progressbar progress {
  background-color: #4a4aff;
  background-image: repeating-linear-gradient(to right, #4a4aff 0px, #4a4aff 9px, #3a3a3a 9px, #3a3a3a 11px);
}
window.dark label.status { background-color: #1f1f1f; border-color: #101010 #606060 #606060 #101010; }
window.dark checkbutton check, window.dark radiobutton radio {
  background-color: #2d2d2d;
  border-color: #101010 #606060 #606060 #101010;
}
";

/// Widgets worth keeping a handle on.
struct Ui {
    window: gtk::ApplicationWindow,
    list: gtk::ListBox,
    radios: Vec<(FsKind, gtk::CheckButton)>,
    label_entry: gtk::Entry,
    agree: gtk::CheckButton,
    full_wipe: gtk::CheckButton,
    confirm_entry: gtk::Entry,
    hide_fixed: gtk::CheckButton,
    rescan_button: gtk::Button,
    exit_button: gtk::Button,
    log_view: gtk::TextView,
    log_scroller: gtk::ScrolledWindow,
    log_button: gtk::Button,
    theme_button: gtk::Button,
    music_button: gtk::Button,
    progress: gtk::ProgressBar,
    status: gtk::Label,
    wipe_button: gtk::Button,
    pause_button: gtk::Button,
    cancel_button: gtk::Button,
}

/// Everything that changes while the window is up.
struct State {
    /// All disks as last scanned.
    all: Vec<Disk>,
    /// The subset currently listed (fixed disks may be hidden).
    shown: Vec<Disk>,
    hide_fixed: bool,
    passes: u32,
    running: bool,
    paused: bool,
    dark: bool,
    worker: Option<std::process::Child>,
    ctl_dir: Option<PathBuf>,
    log_path: Option<PathBuf>,
    /// How many bytes of the log have already been shown.
    log_seen: usize,
    #[cfg(feature = "music")]
    music: Option<crate::music::Player>,
}

struct App {
    ui: Ui,
    state: RefCell<State>,
}

impl App {
    fn set_status(&self, text: &str) {
        self.ui.status.set_label(text);
        // The label is ellipsized so a long line cannot stretch the window;
        // keep the whole text reachable by hovering.
        self.ui.status.set_tooltip_text(Some(text));
    }

    fn shown_disks(&self) -> Vec<Disk> {
        self.state.borrow().shown.clone()
    }

    fn selected_disks(&self) -> Vec<Disk> {
        let shown = self.shown_disks();
        let mut selected = Vec::new();
        for row in self.ui.list.selected_rows() {
            let index = row.index();
            if index >= 0 {
                if let Some(disk) = shown.get(index as usize) {
                    selected.push(disk.clone());
                }
            }
        }
        selected
    }

    fn selected_fs(&self) -> FsKind {
        self.ui
            .radios
            .iter()
            .find(|(_, button)| button.is_active())
            .map(|(fs, _)| *fs)
            .unwrap_or(FsKind::Exfat)
    }

    /// Put the form back the way it started, so a wipe never leaves the typed
    /// confirmation, the tick boxes, the chosen filesystem or a half filled
    /// progress bar behind. Called whenever the worker ends, for any reason.
    fn reset_form(&self) {
        self.ui.confirm_entry.set_text("");
        self.ui.label_entry.set_text("");
        self.ui.agree.set_active(false);
        self.ui.full_wipe.set_active(false);
        self.ui.full_wipe.set_label(Some(FULL_WIPE_OFF));
        self.state.borrow_mut().passes = 0;
        for (fs, button) in &self.ui.radios {
            button.set_active(*fs == FsKind::Exfat);
        }
        self.ui.progress.set_fraction(0.0);
        self.refresh();
    }

    /// Re-read the disk list and rebuild the rows. The disk that holds the
    /// running system is left out of the list entirely, so it can never be
    /// selected; the worker refuses it again as a second line of defence.
    fn refresh(&self) {
        let disks = match device::list_disks() {
            Ok(disks) => disks,
            Err(e) => {
                self.set_status(&format!("Could not read the disk list: {e}"));
                return;
            }
        };

        let hide_fixed = self.state.borrow().hide_fixed;
        let system = device::root_disk(&disks);
        let shown: Vec<Disk> = disks
            .iter()
            .filter(|disk| system.as_deref() != Some(disk.name.as_str()))
            .filter(|disk| !(hide_fixed && !disk.removable && !disk.hotplug))
            .cloned()
            .collect();

        self.ui.list.remove_all();
        for disk in &shown {
            let text = disk.gui_line();
            let label = gtk::Label::new(Some(&text));
            label.set_xalign(0.0);
            label.set_selectable(false);
            let row = gtk::ListBoxRow::new();
            row.set_child(Some(&label));
            self.ui.list.append(&row);
        }

        let (all, shown) = (disks, shown);
        {
            let mut state = self.state.borrow_mut();
            state.all = all;
            state.shown = shown;
        }
        self.ensure_selection();
    }

    /// Keep at least one row highlighted so nothing looks like it is selected
    /// when it is not.
    fn ensure_selection(&self) {
        if self.ui.list.selected_rows().is_empty() {
            if let Some(row) = self.ui.list.row_at_index(0) {
                self.ui.list.select_row(Some(&row));
            }
        }
    }

    fn on_selection_changed(&self) {
        let selected = self.selected_disks();
        if selected.is_empty() {
            self.set_status("Nothing selected.");
            return;
        }
        // Name the whole drive, not just its node or the filesystem on it: a
        // user juggling several sticks needs to see which one is selected.
        let names: Vec<String> = selected.iter().map(Disk::selection_label).collect();
        self.set_status(&format!("Selected: {}", names.join(" | ")));
    }

    fn on_full_wipe_toggled(self: &Rc<Self>) {
        if !self.ui.full_wipe.is_active() {
            self.state.borrow_mut().passes = 0;
            self.ui.full_wipe.set_label(Some(FULL_WIPE_OFF));
            self.set_status("Quick wipe selected (no overwrite).");
            return;
        }

        // Solid state drives never get a multi-pass overwrite.
        let ssds = self.selected_ssds();
        if !ssds.is_empty() {
            self.state.borrow_mut().passes = 0;
            self.ui.full_wipe.set_active(false);
            self.ui.full_wipe.set_label(Some(FULL_WIPE_OFF));
            self.set_status("Full wipe is not allowed on SSD drives.");
            alert(
                &self.ui.window,
                "SSD protected",
                &format!(
                    "Full (multi-pass) wipe is blocked for solid state drives.\n\n{}\n\n\
                     Overwriting an SSD does not securely erase it and only wears it out. \
                     Use a quick wipe for it, or pick a USB or hard drive for a full wipe.",
                    ssds.join(", ")
                ),
            );
            return;
        }

        let current = self.state.borrow().passes.max(1);
        match show_pass_dialog(&self.ui.window, current) {
            Some(passes) => {
                self.state.borrow_mut().passes = passes;
                self.ui.full_wipe.set_label(Some(&format!(
                    "Full wipe ON: {passes} pass(es) [{}] - click to change.",
                    crate::wipe::passes_short(passes)
                )));
                self.set_status(&format!("{} selected.", crate::wipe::pass_label(passes)));
            }
            None => {
                self.state.borrow_mut().passes = 0;
                self.ui.full_wipe.set_active(false);
                self.ui.full_wipe.set_label(Some(FULL_WIPE_OFF));
                self.set_status("Full wipe cancelled; quick wipe selected.");
            }
        }
    }

    fn selected_ssds(&self) -> Vec<String> {
        self.selected_disks()
            .iter()
            .filter(|disk| disk.is_ssd())
            .map(|disk| disk.path.clone())
            .collect()
    }

    fn set_busy(&self, busy: bool) {
        {
            let mut state = self.state.borrow_mut();
            state.running = busy;
            if !busy {
                state.paused = false;
            }
        }
        self.ui.list.set_sensitive(!busy);
        self.ui.agree.set_sensitive(!busy);
        self.ui.full_wipe.set_sensitive(!busy);
        self.ui.confirm_entry.set_sensitive(!busy);
        self.ui.hide_fixed.set_sensitive(!busy);
        self.ui.rescan_button.set_sensitive(!busy);
        self.ui.label_entry.set_sensitive(!busy);
        for (_, button) in &self.ui.radios {
            button.set_sensitive(!busy);
        }
        self.ui.pause_button.set_sensitive(busy);
        self.ui.cancel_button.set_sensitive(busy);
        self.ui.pause_button.set_label("Pause");
        if busy {
            self.ui.wipe_button.set_sensitive(false);
        } else {
            self.update_wipe_button();
        }
    }

    /// WIPE only lights up once the box is ticked and WIPE has been typed, the
    /// same gate as the original.
    fn update_wipe_button(&self) {
        if self.state.borrow().running {
            return;
        }
        let ready = self.ui.agree.is_active()
            && self
                .ui
                .confirm_entry
                .text()
                .trim()
                .eq_ignore_ascii_case("WIPE")
            && !self.ui.list.selected_rows().is_empty();
        self.ui.wipe_button.set_sensitive(ready);
    }

    fn start_wipe(self: &Rc<Self>) {
        if self.state.borrow().running {
            return;
        }

        let selected = self.selected_disks();
        if selected.is_empty() {
            alert(
                &self.ui.window,
                "Nothing selected",
                "Select a drive in the list first.",
            );
            return;
        }
        if !self.ui.agree.is_active() {
            alert(
                &self.ui.window,
                "Not confirmed",
                "Tick 'I understand this erases ALL data on the selected drive(s).' first.",
            );
            return;
        }
        if !self
            .ui
            .confirm_entry
            .text()
            .trim()
            .eq_ignore_ascii_case("WIPE")
        {
            alert(
                &self.ui.window,
                "Not confirmed",
                "Type WIPE in the confirmation box, then click WIPE DRIVE(S).",
            );
            return;
        }

        // The system disk is never touched, whatever the confirmation says.
        let all = self.state.borrow().all.clone();
        let system = device::root_disk(&all);
        if let Some(disk) = selected
            .iter()
            .find(|disk| system.as_deref() == Some(disk.name.as_str()))
        {
            alert(
                &self.ui.window,
                "System disk refused",
                &format!(
                    "{} holds the running system (/). It cannot be wiped from here.",
                    disk.path
                ),
            );
            return;
        }

        let fs = self.selected_fs();
        if !crate::partition::format_available(fs) {
            let install = crate::util::package_for(fs.mkfs_tool())
                .map(|package| format!(" Install it first (on Arch: sudo pacman -S {package})."))
                .unwrap_or_default();
            alert(
                &self.ui.window,
                "Missing tool",
                &format!(
                    "{} is not installed, so {} cannot be created.{}",
                    fs.mkfs_tool(),
                    fs.display(),
                    install
                ),
            );
            return;
        }

        let passes = self.state.borrow().passes;
        if passes > 0 {
            let ssds = self.selected_ssds();
            if !ssds.is_empty() {
                alert(
                    &self.ui.window,
                    "SSD protected",
                    &format!(
                        "Full wipe is blocked for the solid state drive(s): {}",
                        ssds.join(", ")
                    ),
                );
                return;
            }
        }

        // A fixed disk needs its device node typed out again, the same second
        // check the Windows worker asked for with the drive letter.
        for disk in &selected {
            if !disk.removable && !disk.hotplug {
                let prompt = format!(
                    "{} is a FIXED disk (not removable) - double-check this is really the drive you mean.\n\n\
                     Type {} again to confirm wiping this fixed disk:",
                    disk.path, disk.path
                );
                let answer = prompt_line(&self.ui.window, "Fixed disk - are you sure?", &prompt);
                if answer.as_deref().map(str::trim) != Some(disk.path.as_str()) {
                    self.set_status(&format!(
                        "{}: fixed disk not confirmed - skipping.",
                        disk.path
                    ));
                    return;
                }
            }
        }

        // Hand the job to the privileged worker.
        let ctl_dir = std::env::temp_dir().join(format!("usb-reformat-{}", std::process::id()));
        if let Err(e) = std::fs::create_dir_all(&ctl_dir) {
            alert(&self.ui.window, "Could not start", &format!("{e}"));
            return;
        }
        let log_path = ctl_dir.join("worker.log");
        let _ = std::fs::remove_file(ctl_dir.join("cancel"));
        let _ = std::fs::remove_file(ctl_dir.join("pause"));
        let _ = std::fs::write(&log_path, b"");

        let mut args = vec!["--run".to_string()];
        for disk in &selected {
            args.push("--device".to_string());
            args.push(disk.path.clone());
        }
        args.push("--fs".to_string());
        args.push(fs.display().to_string());
        args.push("--label".to_string());
        args.push(self.ui.label_entry.text().trim().to_string());
        args.push("--passes".to_string());
        args.push(passes.to_string());
        args.push("--plan".to_string());
        args.push("wipe-and-format".to_string());
        args.push("--log".to_string());
        args.push(log_path.display().to_string());
        args.push("--ctl".to_string());
        args.push(ctl_dir.display().to_string());

        self.ui.log_view.buffer().set_text("");
        {
            let mut state = self.state.borrow_mut();
            state.log_seen = 0;
            state.log_path = Some(log_path);
            state.ctl_dir = Some(ctl_dir.clone());
        }

        match privs::spawn_elevated(&args) {
            Ok(child) => {
                self.state.borrow_mut().worker = Some(child);
                self.ui.progress.set_fraction(0.0);
                self.set_busy(true);
                let fs_name = fs.display();
                self.set_status(&format!(
                    "Started on {}. Approve the prompt, then watch the log. Formatting as {}.",
                    selected
                        .iter()
                        .map(Disk::selection_label)
                        .collect::<Vec<_>>()
                        .join(" | "),
                    fs_name
                ));
                self.spawn_poll();
            }
            Err(e) => {
                self.set_busy(false);
                alert(&self.ui.window, "Could not start the worker", &e);
            }
        }
    }

    fn spawn_poll(self: &Rc<Self>) {
        let app = Rc::clone(self);
        gtk::glib::timeout_add_local(Duration::from_millis(250), move || {
            if app.poll() {
                gtk::glib::ControlFlow::Continue
            } else {
                gtk::glib::ControlFlow::Break
            }
        });
    }

    /// Show whatever the worker has appended since last time. Only whole lines
    /// are consumed so a read can never split a character.
    fn read_new_log(&self) {
        let path = match self.state.borrow().log_path.clone() {
            Some(path) => path,
            None => return,
        };
        let Ok(bytes) = std::fs::read(&path) else {
            return;
        };
        let seen = self.state.borrow().log_seen;
        if bytes.len() <= seen {
            return;
        }
        let chunk = &bytes[seen..];
        let cut = match chunk.iter().rposition(|b| *b == b'\n') {
            Some(index) => index + 1,
            None => return,
        };
        let text = String::from_utf8_lossy(&chunk[..cut]).to_string();
        self.state.borrow_mut().log_seen = seen + cut;

        let mut visible = String::new();
        let mut percent = None;
        for line in text.lines() {
            match line.strip_prefix("##PROG ") {
                Some(value) => percent = value.trim().parse::<f64>().ok(),
                None => {
                    visible.push_str(line);
                    visible.push('\n');
                }
            }
        }

        if !visible.is_empty() {
            let buffer = self.ui.log_view.buffer();
            let mut end = buffer.end_iter();
            buffer.insert(&mut end, &visible);
            let mut end = buffer.end_iter();
            self.ui
                .log_view
                .scroll_to_iter(&mut end, 0.0, false, 0.0, 0.0);
        }
        if let Some(value) = percent {
            self.ui
                .progress
                .set_fraction((value / 100.0).clamp(0.0, 1.0));
        }
    }

    /// Returns true while the worker is still running.
    fn poll(self: &Rc<Self>) -> bool {
        self.read_new_log();

        let finished = {
            let mut state = self.state.borrow_mut();
            match state.worker.as_mut() {
                Some(child) => match child.try_wait() {
                    Ok(Some(status)) => Some((status.code(), status.signal())),
                    Ok(None) => None,
                    Err(_) => Some((None, None)),
                },
                None => return false,
            }
        };

        let Some((code, signal)) = finished else {
            return true;
        };

        self.read_new_log();
        {
            let mut state = self.state.borrow_mut();
            state.worker = None;
        }
        self.set_busy(false);

        // Whatever brought the worker down, the window goes back to idle with a
        // clean form. The status text is set after the reset because clearing
        // the tick boxes fires their handlers, which set a status of their own.
        self.reset_form();

        match (code, signal) {
            (Some(0), _) => {
                self.set_status("Done. The drive was wiped, repartitioned and formatted.");
            }
            (Some(3), _) => {
                self.set_status("Cancelled. The drive may be part wiped; the form has been reset.");
            }
            (Some(other), _) => {
                self.set_status(&format!(
                    "The worker stopped with code {other}. See the log below."
                ));
                self.ui.log_scroller.set_visible(true);
                self.ui.log_button.set_label("Hide log");
            }
            (None, signal) => {
                // Killed rather than exiting by itself: say so plainly instead
                // of leaving the window stuck in the busy state.
                let how = match signal {
                    Some(n) => format!("was stopped by signal {n}"),
                    None => "ended unexpectedly".to_string(),
                };
                self.set_status(&format!(
                    "The worker {how}. The drive may be left part wiped; the log below shows how far it got."
                ));
                self.ui.log_scroller.set_visible(true);
                self.ui.log_button.set_label("Hide log");
            }
        }
        false
    }

    fn toggle_pause(&self) {
        let state = self.state.borrow();
        if !state.running {
            return;
        }
        let Some(dir) = state.ctl_dir.clone() else {
            return;
        };
        drop(state);

        let flag = dir.join("pause");
        let now_paused = !flag.exists();
        let result = if now_paused {
            std::fs::write(&flag, b"")
        } else {
            std::fs::remove_file(&flag).or_else(|e| {
                if e.kind() == std::io::ErrorKind::NotFound {
                    Ok(())
                } else {
                    Err(e)
                }
            })
        };

        match result {
            Ok(()) => {
                self.state.borrow_mut().paused = now_paused;
                if now_paused {
                    self.ui.pause_button.set_label("Resume");
                    self.set_status("Paused - the write stops between chunks.");
                } else {
                    self.ui.pause_button.set_label("Pause");
                    self.set_status("Resumed - writing continues.");
                }
            }
            Err(_) => self.set_status("Could not pause (control file not writable)."),
        }
    }

    fn cancel_wipe(&self) {
        let state = self.state.borrow();
        if !state.running {
            return;
        }
        let Some(dir) = state.ctl_dir.clone() else {
            return;
        };
        drop(state);

        if std::fs::write(dir.join("cancel"), b"").is_ok() {
            self.set_status(
                "Cancelling - the write stops between chunks. A drive that has gone unresponsive can take a moment.",
            );
        } else {
            self.set_status("Could not cancel (control file not writable).");
        }
    }

    fn toggle_log(&self) {
        let visible = !self.ui.log_scroller.is_visible();
        self.ui.log_scroller.set_visible(visible);
        self.ui
            .log_button
            .set_label(if visible { "Hide log" } else { "Show log" });
    }

    fn toggle_theme(&self) {
        let dark = !self.state.borrow().dark;
        self.state.borrow_mut().dark = dark;
        if dark {
            self.ui.window.add_css_class("dark");
            self.ui.theme_button.set_label("Light mode");
        } else {
            self.ui.window.remove_css_class("dark");
            self.ui.theme_button.set_label("Dark mode");
        }
    }

    fn toggle_hide_fixed(&self) {
        let hide = self.ui.hide_fixed.is_active();
        self.state.borrow_mut().hide_fixed = hide;
        self.refresh();
        self.set_status(if hide {
            "Fixed (non-removable) drives are hidden."
        } else {
            "Fixed (non-removable) drives are shown."
        });
    }

    fn toggle_music(&self) {
        #[cfg(feature = "music")]
        {
            let mut state = self.state.borrow_mut();
            if let Some(player) = state.music.as_mut() {
                let playing = player.toggle();
                let label = player.label();
                let now = player.now_playing();
                drop(state);
                self.ui.music_button.set_label(&label);
                if playing {
                    self.set_status(&format!("Music resumed: {now}"));
                } else {
                    self.set_status("Music paused.");
                }
                return;
            }
            drop(state);
            self.set_status(
                "No music found. Put .xm, .it, .mod or .s3m files in a music folder beside the program.",
            );
        }
        #[cfg(not(feature = "music"))]
        self.set_status("This build was made without the music feature.");
    }
}

fn main_quit(app: &Rc<App>) {
    let ctl_dir = {
        let state = app.state.borrow();
        if !state.running {
            return;
        }
        state.ctl_dir.clone()
    };
    // Ask the worker to stop rather than killing it, so it is never interrupted
    // in the middle of a syscall.
    if let Some(dir) = ctl_dir {
        let _ = std::fs::write(dir.join("cancel"), b"");
    }
}

/// A modal message box. Non blocking: the answer is not needed.
fn alert(window: &gtk::ApplicationWindow, title: &str, message: &str) {
    let detail = message;
    let dialog = gtk::AlertDialog::builder()
        .modal(true)
        .message(title)
        .detail(detail)
        .build();
    dialog.show(Some(window.upcast_ref::<gtk::Window>()));
}

/// A modal dialog with one text box, returning what was typed.
///
/// GTK 4 removed `Dialog::run`, so this nests a main loop to wait for the
/// answer. That is the same shape as the nested message loop the Windows build
/// used for its own dialogs.
fn prompt_line(window: &gtk::ApplicationWindow, title: &str, prompt: &str) -> Option<String> {
    let dialog = modal_window(window, title);

    let content = gtk::Box::new(gtk::Orientation::Vertical, 8);
    content.set_margin_top(12);
    content.set_margin_bottom(12);
    content.set_margin_start(14);
    content.set_margin_end(14);

    let label = gtk::Label::new(Some(prompt));
    label.set_wrap(true);
    label.set_xalign(0.0);
    label.set_max_width_chars(52);

    let entry = gtk::Entry::new();
    entry.set_width_chars(24);

    let ok = gtk::Button::with_label("OK");
    let cancel = gtk::Button::with_label("Cancel");

    content.append(&label);
    content.append(&entry);
    content.append(&button_row(&ok, &cancel));
    dialog.set_child(Some(&content));

    let answer = Rc::new(RefCell::new(None::<String>));
    let main_loop = gtk::glib::MainLoop::new(None, false);

    {
        let answer = Rc::clone(&answer);
        let entry = entry.clone();
        let dialog = dialog.clone();
        let main_loop = main_loop.clone();
        ok.connect_clicked(move |_| {
            *answer.borrow_mut() = Some(entry.text().to_string());
            dialog.close();
            main_loop.quit();
        });
    }
    {
        let answer = Rc::clone(&answer);
        let dialog = dialog.clone();
        let main_loop = main_loop.clone();
        // The entry hands itself to the callback, so there is nothing to clone.
        let trigger = entry.clone();
        trigger.connect_activate(move |entry| {
            *answer.borrow_mut() = Some(entry.text().to_string());
            dialog.close();
            main_loop.quit();
        });
    }
    {
        let dialog = dialog.clone();
        let main_loop = main_loop.clone();
        cancel.connect_clicked(move |_| {
            dialog.close();
            main_loop.quit();
        });
    }

    dialog.present();
    main_loop.run();

    // Bind before returning: the temporary `Ref` outlives the local if left as
    // a tail expression.
    let value = answer.borrow().clone();
    drop(answer);
    value
}

/// A frameless-ish modal window: GTK 4.10 deprecated `GtkDialog` in favour of
/// building the window by hand, so that is what this does.
fn modal_window(parent: &gtk::ApplicationWindow, title: &str) -> gtk::Window {
    gtk::Window::builder()
        .title(title)
        .transient_for(parent)
        .modal(true)
        .resizable(false)
        .build()
}

/// A Win98 style button row: the buttons sit on the right, OK before Cancel.
fn button_row(ok: &gtk::Button, cancel: &gtk::Button) -> gtk::Box {
    let row = gtk::Box::new(gtk::Orientation::Horizontal, 6);
    let spacer = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    spacer.set_hexpand(true);
    row.append(&spacer);
    row.append(ok);
    row.append(cancel);
    row
}

/// The pass chooser, matching the original's four options.
fn show_pass_dialog(window: &gtk::ApplicationWindow, current: u32) -> Option<u32> {
    let dialog = modal_window(window, "Full wipe options");

    let content = gtk::Box::new(gtk::Orientation::Vertical, 6);
    content.set_margin_top(12);
    content.set_margin_bottom(12);
    content.set_margin_start(14);
    content.set_margin_end(14);

    let prompt = gtk::Label::new(Some(
        "Choose the overwrite method. More passes = more secure, but much slower and more wear on the drive:",
    ));
    prompt.set_wrap(true);
    prompt.set_xalign(0.0);
    prompt.set_max_width_chars(54);
    content.append(&prompt);

    let choices: [(u32, &str); 4] = [
        (1, "1 pass   - quick full wipe (write zeros once)"),
        (3, "3 passes - DoD 5220.22-M  (US Dept. of Defense)"),
        (7, "7 passes - DoD 5220.22-M ECE"),
        (35, "35 passes - Gutmann method (slowest)"),
    ];

    let current = current.max(1);
    let selected = Rc::new(Cell::new(current));
    let mut group: Option<gtk::CheckButton> = None;
    for (passes, text) in choices {
        let button = gtk::CheckButton::with_label(text);
        match &group {
            Some(first) => button.set_group(Some(first)),
            None => group = Some(button.clone()),
        }
        if passes == current {
            button.set_active(true);
        }
        let slot = Rc::clone(&selected);
        button.connect_toggled(move |button| {
            if button.is_active() {
                slot.set(passes);
            }
        });
        content.append(&button);
    }

    let note = gtk::Label::new(Some(
        "Every sector is overwritten before the drive is re-formatted.",
    ));
    note.set_xalign(0.0);
    content.append(&note);

    let ok = gtk::Button::with_label("OK");
    let cancel = gtk::Button::with_label("Cancel");
    content.append(&button_row(&ok, &cancel));
    dialog.set_child(Some(&content));

    let answer = Rc::new(Cell::new(None::<u32>));
    let main_loop = gtk::glib::MainLoop::new(None, false);

    {
        let answer = Rc::clone(&answer);
        let selected = Rc::clone(&selected);
        let dialog = dialog.clone();
        let main_loop = main_loop.clone();
        ok.connect_clicked(move |_| {
            answer.set(Some(selected.get()));
            dialog.close();
            main_loop.quit();
        });
    }
    {
        let dialog = dialog.clone();
        let main_loop = main_loop.clone();
        cancel.connect_clicked(move |_| {
            dialog.close();
            main_loop.quit();
        });
    }

    dialog.present();
    main_loop.run();

    answer.get()
}

/// A Win98 group box: an etched frame with the caption sitting on the border.
fn group(caption: &str, child: &impl IsA<gtk::Widget>) -> gtk::Frame {
    let frame = gtk::Frame::new(None);
    frame.set_label(Some(caption));
    frame.set_child(Some(child));
    frame
}

fn build_ui(application: &gtk::Application) {
    let window = gtk::ApplicationWindow::builder()
        .application(application)
        .title(WINDOW_TITLE)
        .default_width(560)
        .default_height(560)
        .build();

    let provider = gtk::CssProvider::new();
    provider.load_from_string(CSS);
    if let Some(display) = gtk::gdk::Display::default() {
        gtk::style_context_add_provider_for_display(
            &display,
            &provider,
            gtk::STYLE_PROVIDER_PRIORITY_APPLICATION,
        );
    }

    let root = gtk::Box::new(gtk::Orientation::Vertical, 6);
    root.set_margin_top(6);
    root.set_margin_bottom(6);
    root.set_margin_start(8);
    root.set_margin_end(8);

    // --- toolbar ---
    let toolbar = gtk::Box::new(gtk::Orientation::Horizontal, 6);
    let title = gtk::Label::new(Some(WINDOW_TITLE));
    title.add_css_class("title");
    title.set_xalign(0.0);
    title.set_hexpand(true);
    let music_button = gtk::Button::with_label("Music: on");
    let theme_button = gtk::Button::with_label("Dark mode");
    let log_button = gtk::Button::with_label("Show log");
    toolbar.append(&title);
    toolbar.append(&music_button);
    toolbar.append(&theme_button);
    toolbar.append(&log_button);
    root.append(&toolbar);

    // --- drives ---
    let list = gtk::ListBox::new();
    list.set_selection_mode(gtk::SelectionMode::Multiple);
    // GTK's single-click path in MULTIPLE mode only *adds* the clicked row
    // (gtk_list_box_select_row_internal never clears the others), which makes a
    // plain click behave like Ctrl+click and hooks every drive together. Turning
    // single-click activation off routes a plain click through the path that
    // replaces the selection, so: plain click = this one only, Ctrl+click = add
    // or remove, Shift+click = range. That is the Windows behaviour users expect.
    list.set_activate_on_single_click(false);
    let list_scroller = gtk::ScrolledWindow::new();
    list_scroller.set_child(Some(&list));
    list_scroller.set_policy(gtk::PolicyType::Automatic, gtk::PolicyType::Automatic);
    list_scroller.set_height_request(126);
    root.append(&group(
        "Drives to erase - the WHOLE device is wiped (all its partitions)",
        &list_scroller,
    ));

    // --- format ---
    let format_row = gtk::Box::new(gtk::Orientation::Horizontal, 10);
    format_row.set_margin_top(8);
    format_row.set_margin_bottom(8);
    format_row.set_margin_start(10);
    format_row.set_margin_end(10);

    let mut radios: Vec<(FsKind, gtk::CheckButton)> = Vec::new();
    let mut group_head: Option<gtk::CheckButton> = None;
    for fs in FsKind::all() {
        let text = match fs {
            FsKind::Exfat => "exFAT  (recommended)".to_string(),
            other => other.display().to_string(),
        };
        let button = gtk::CheckButton::with_label(&text);
        match &group_head {
            Some(head) => button.set_group(Some(head)),
            None => {
                group_head = Some(button.clone());
                button.set_active(true);
            }
        }
        radios.push((fs, button.clone()));
        format_row.append(&button);
    }

    let label_spacer = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    label_spacer.set_hexpand(true);
    let label_text = gtk::Label::new(Some("Label:"));
    let label_entry = gtk::Entry::new();
    label_entry.set_width_chars(14);
    label_entry.set_max_length(11);
    label_entry.set_placeholder_text(Some("optional"));
    format_row.append(&label_spacer);
    format_row.append(&label_text);
    format_row.append(&label_entry);
    root.append(&group("New partition format", &format_row));

    // --- confirmations ---
    let agree =
        gtk::CheckButton::with_label("I understand this erases ALL data on the selected drive(s).");
    agree.set_margin_start(4);
    root.append(&agree);

    let full_wipe = gtk::CheckButton::with_label(FULL_WIPE_OFF);
    full_wipe.set_margin_start(4);
    root.append(&full_wipe);

    // --- confirm row ---
    let confirm_row = gtk::Box::new(gtk::Orientation::Horizontal, 6);
    let confirm_label = gtk::Label::new(Some("Type WIPE to confirm:"));
    let confirm_entry = gtk::Entry::new();
    confirm_entry.set_width_chars(8);
    let hide_fixed = gtk::CheckButton::with_label("Hide fixed drives");
    let rescan_button = gtk::Button::with_label("Re-scan");
    let exit_button = gtk::Button::with_label("Exit");
    confirm_row.append(&confirm_label);
    confirm_row.append(&confirm_entry);

    let confirm_spacer = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    confirm_spacer.set_hexpand(true);
    confirm_row.append(&confirm_spacer);
    confirm_row.append(&hide_fixed);
    confirm_row.append(&rescan_button);
    confirm_row.append(&exit_button);
    root.append(&confirm_row);

    // --- actions ---
    let action_row = gtk::Box::new(gtk::Orientation::Horizontal, 6);
    let wipe_button = gtk::Button::with_label("WIPE DRIVE(S)");
    wipe_button.set_hexpand(true);
    let pause_button = gtk::Button::with_label("Pause");
    let cancel_button = gtk::Button::with_label("Cancel");
    action_row.append(&wipe_button);
    action_row.append(&pause_button);
    action_row.append(&cancel_button);
    root.append(&action_row);

    // --- progress ---
    let progress = gtk::ProgressBar::new();
    progress.set_show_text(false);
    progress.set_fraction(0.0);
    progress.set_height_request(24);
    root.append(&progress);

    // --- log ---
    let log_view = gtk::TextView::new();
    log_view.set_editable(false);
    log_view.set_cursor_visible(false);
    log_view.set_monospace(true);
    log_view.set_wrap_mode(gtk::WrapMode::None);
    let log_scroller = gtk::ScrolledWindow::new();
    log_scroller.set_child(Some(&log_view));
    log_scroller.set_policy(gtk::PolicyType::Automatic, gtk::PolicyType::Automatic);
    log_scroller.set_height_request(140);
    log_scroller.set_visible(false);
    root.append(&log_scroller);

    // --- status bar ---
    // A plain `GtkLabel` asks for its whole text as its *minimum* width, so a
    // long line here (a drive label, or several drives at once) drags the whole
    // window out to fit it. Capping the width request and ellipsizing keeps the
    // window at its designed size no matter what the status says; the full text
    // is still available as a tooltip (see `App::set_status`).
    let status = gtk::Label::new(Some(READY));
    status.add_css_class("status");
    status.set_xalign(0.0);
    status.set_ellipsize(gtk::pango::EllipsizeMode::End);
    status.set_max_width_chars(1);
    root.append(&status);

    window.set_child(Some(&root));

    let app = Rc::new(App {
        ui: Ui {
            window: window.clone(),
            list,
            radios,
            label_entry,
            agree,
            full_wipe,
            confirm_entry,
            hide_fixed,
            rescan_button,
            exit_button,
            log_view,
            log_scroller,
            log_button,
            theme_button,
            music_button,
            progress,
            status,
            wipe_button,
            pause_button,
            cancel_button,
        },
        state: RefCell::new(State {
            all: Vec::new(),
            shown: Vec::new(),
            hide_fixed: false,
            passes: 0,
            running: false,
            paused: false,
            dark: true,
            worker: None,
            ctl_dir: None,
            log_path: None,
            log_seen: 0,
            #[cfg(feature = "music")]
            music: None,
        }),
    });

    // Start the chiptunes now, so music is playing the moment the window
    // appears, exactly as the original did.
    #[cfg(feature = "music")]
    {
        let player = crate::music::Player::start();
        let label = player.label();
        let count = player.track_count();
        let now = player.now_playing();
        app.ui.music_button.set_label(&label);
        app.state.borrow_mut().music = Some(player);
        if count > 0 {
            app.set_status(&format!("Now playing: {now}  ({count} tracks)"));
        }
    }

    // Start in dark mode: the light Win98 palette is hard to read on this
    // machine's display, so dark is the default and the button switches back.
    app.ui.window.add_css_class("dark");
    app.ui.theme_button.set_label("Light mode");

    app.refresh();
    app.set_busy(false);
    app.update_wipe_button();

    // --- signals ---
    // Each block clones the widget it connects to first, so the `Rc<App>` can
    // be moved into the closure without the widget access still borrowing it.
    {
        let signal = app.ui.list.clone();
        let app = Rc::clone(&app);
        signal.connect_selected_rows_changed(move |_| {
            app.on_selection_changed();
            app.update_wipe_button();
        });
    }
    {
        let signal = app.ui.agree.clone();
        let app = Rc::clone(&app);
        signal.connect_toggled(move |_| app.update_wipe_button());
    }
    {
        let signal = app.ui.confirm_entry.clone();
        let app = Rc::clone(&app);
        signal.connect_changed(move |_| app.update_wipe_button());
    }
    {
        let signal = app.ui.full_wipe.clone();
        let app = Rc::clone(&app);
        signal.connect_toggled(move |_| app.on_full_wipe_toggled());
    }
    {
        let signal = app.ui.hide_fixed.clone();
        let app = Rc::clone(&app);
        signal.connect_toggled(move |_| app.toggle_hide_fixed());
    }
    {
        let signal = app.ui.rescan_button.clone();
        let app = Rc::clone(&app);
        signal.connect_clicked(move |_| {
            app.refresh();
            app.set_status("Drive list refreshed.");
        });
    }
    {
        let signal = app.ui.wipe_button.clone();
        let app = Rc::clone(&app);
        signal.connect_clicked(move |_| app.start_wipe());
    }
    {
        let signal = app.ui.pause_button.clone();
        let app = Rc::clone(&app);
        signal.connect_clicked(move |_| app.toggle_pause());
    }
    {
        let signal = app.ui.cancel_button.clone();
        let app = Rc::clone(&app);
        signal.connect_clicked(move |_| app.cancel_wipe());
    }
    {
        let signal = app.ui.log_button.clone();
        let app = Rc::clone(&app);
        signal.connect_clicked(move |_| app.toggle_log());
    }
    {
        let signal = app.ui.theme_button.clone();
        let app = Rc::clone(&app);
        signal.connect_clicked(move |_| app.toggle_theme());
    }
    {
        let signal = app.ui.music_button.clone();
        let app = Rc::clone(&app);
        signal.connect_clicked(move |_| app.toggle_music());
    }
    {
        let signal = app.ui.exit_button.clone();
        let app = Rc::clone(&app);
        let window = window.clone();
        signal.connect_clicked(move |_| {
            main_quit(&app);
            window.close();
        });
    }
    {
        let app = Rc::clone(&app);
        window.connect_close_request(move |_| {
            main_quit(&app);
            gtk::glib::Propagation::Proceed
        });
    }

    window.present();
}

/// Entry point used by `main`.
pub fn run() {
    let application = gtk::Application::builder().application_id(APP_ID).build();
    application.connect_activate(build_ui);
    application.run();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_progress_fill_is_a_row_of_blocks() {
        // GTK 4 nests the fill as `progressbar > trough > progress`, so the
        // descendant selector has to match or the fill stays a thin theme line.
        assert!(CSS.contains("progressbar progress {"), "fill rule missing");
        assert!(
            CSS.contains("repeating-linear-gradient"),
            "the Windows 98 block pattern is missing"
        );
        assert!(
            CSS.contains("#000080 0px, #000080 9px, #ffffff 9px, #ffffff 11px"),
            "the block and gap widths are wrong"
        );
    }

    #[test]
    fn css_declares_the_windows_98_palette() {
        assert!(CSS.contains("#d4d0c8"), "button face colour missing");
        assert!(CSS.contains("#808080"), "shadow colour missing");
        assert!(CSS.contains("#000080"), "selection colour missing");
    }

    #[test]
    fn css_starts_dark_and_still_has_a_light_palette() {
        assert!(
            CSS.contains("window.dark list > row { background-color: #2d2d2d"),
            "dark rows need an explicit background"
        );
        assert!(
            CSS.contains("window.dark list > row label { color: #ffffff; }"),
            "dark rows need readable label text"
        );
        assert!(
            CSS.contains("list > row label { color: #000000;"),
            "light rows need readable label text"
        );
    }

    #[test]
    fn the_ready_line_matches_the_original() {
        assert!(READY.starts_with("Ready. Select a drive, tick the box, type WIPE"));
    }

    #[test]
    fn full_wipe_off_text_matches_the_original() {
        assert!(FULL_WIPE_OFF.starts_with("Full wipe (slow) - click to choose passes."));
        assert!(FULL_WIPE_OFF.ends_with("Unticked = quick wipe."));
    }
}
