//! What runs once the bar has hidden: the paste into the window that was in
//! front, Windows' clipboard history, the snip overlay, the colour picker and
//! the Start menu.

use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use crate::commands::AfterHide;
use crate::file_ops::FileOps;
use crate::launch::Launcher;
use crate::paste::{paste_into, Desktop, PASTE_WAIT, POLL};
use crate::provider::Then;
use crate::providers::screen::SNIP_URI;

/// The parts each follow-up reaches. Real ones in `setup`, fakes in tests.
pub struct FollowUps {
    pub desktop: Arc<dyn Desktop>,
    pub launcher: Arc<dyn Launcher>,
    /// The window that was in front before the bar opened.
    pub previous: fn() -> Option<isize>,
    pub pick_color: Box<dyn Fn() + Send + Sync>,
    /// Asks where to save the QR code for the text, then saves it. Blocks on
    /// the Save window, so it only runs on the follow-up's own thread.
    pub save_qr: Box<dyn Fn(&str) + Send + Sync>,
    /// Waits until the hidden bar is off the screen.
    pub settle: fn(),
    /// How many times the bar has been shown (`window::shows`).
    pub shows: fn() -> u64,
    /// Compress and the cloud keep/free pair.
    pub ops: Arc<dyn FileOps>,
    /// Brings the bar back with an error shown.
    pub reopen: Box<dyn Fn(&str) + Send + Sync>,
}

impl FollowUps {
    /// Shows `error` in the bar again, unless the bar has been shown since the
    /// action started: it then belongs to something else.
    fn report(&self, result: Result<(), String>, started: u64) {
        if let Err(error) = result {
            if (self.shows)() == started {
                (self.reopen)(&error);
            }
        }
    }
}

impl AfterHide for FollowUps {
    fn run(&self, then: Then, started: u64) {
        // The overlay, Win+V, the picker's frozen frame and Start must not
        // show the bar, so those wait for it to go; a paste does not.
        match then {
            Then::Paste { before } => {
                // The item is already on the clipboard: when the paste cannot
                // happen, a copy is what the user gets.
                let shows = self.shows;
                let still_current = move || shows() == started;
                let _ = paste_into((self.previous)(), self.desktop.as_ref(), before, &still_current, PASTE_WAIT, POLL);
            }
            Then::ClipboardHistory => {
                (self.settle)();
                let _ = self.desktop.send_win_v();
            }
            Then::Snip => {
                (self.settle)();
                let _ = self.launcher.open(SNIP_URI);
            }
            Then::PickColor => {
                (self.settle)();
                (self.pick_color)();
            }
            Then::SaveQr { text } => {
                (self.settle)();
                (self.save_qr)(&text);
            }
            Then::StartMenu => {
                (self.settle)();
                let _ = self.desktop.open_start_menu();
            }
            Then::Compress { path } => self.report(self.ops.compress(Path::new(&path)).map(|_| ()), started),
            Then::CloudPin { path, keep } => self.report(self.ops.set_cloud_pin(Path::new(&path), keep), started),
        }
    }
}

/// How long after the next composition the hidden bar is surely gone (it may
/// be fading out).
const SETTLE: Duration = Duration::from_millis(150);

/// Waits for the desktop to compose a frame without the bar, then a little more.
pub fn settle() {
    // SAFETY: no arguments; fails harmlessly when composition is off.
    let _ = unsafe { windows::Win32::Graphics::Dwm::DwmFlush() };
    std::thread::sleep(SETTLE);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::file_ops::FakeFileOps;
    use crate::launch::testing::RecordingLauncher;
    use crate::paste::FakeDesktop;
    use std::cell::Cell;
    use std::sync::atomic::{AtomicUsize, Ordering};

    thread_local! {
        static SETTLES: Cell<usize> = const { Cell::new(0) };
    }

    fn count_settle() {
        SETTLES.with(|c| c.set(c.get() + 1));
    }

    fn settles() -> usize {
        SETTLES.with(Cell::get)
    }

    #[test]
    fn each_follow_up_reaches_its_part() {
        let desktop = Arc::new(FakeDesktop::new(vec![Some(7)]));
        let launcher = Arc::new(RecordingLauncher::default());
        let picks = Arc::new(AtomicUsize::new(0));
        let counter = picks.clone();
        let saved = Arc::new(std::sync::Mutex::new(Vec::new()));
        let saved_in = saved.clone();
        let follow = FollowUps {
            desktop: desktop.clone(),
            launcher: launcher.clone(),
            previous: || Some(7),
            pick_color: Box::new(move || {
                counter.fetch_add(1, Ordering::SeqCst);
            }),
            save_qr: Box::new(move |text| saved_in.lock().unwrap().push(text.to_string())),
            settle: count_settle,
            shows: || 3,
            ops: Arc::new(FakeFileOps::default()),
            reopen: Box::new(|_| {}),
        };
        let keys = |d: &FakeDesktop, key: &str| d.calls().iter().filter(|c| *c == key).count();

        follow.run(Then::Paste { before: 0 }, 3);
        assert_eq!(keys(&desktop, "ctrl_v"), 1);
        assert_eq!(keys(&desktop, "win_v"), 0);
        assert_eq!(settles(), 0, "a paste does not wait");

        follow.run(Then::ClipboardHistory, 3);
        assert_eq!(keys(&desktop, "win_v"), 1);
        assert_eq!(settles(), 1);

        follow.run(Then::Snip, 3);
        assert_eq!(launcher.calls(), ["open ms-screenclip:"]);
        assert_eq!(settles(), 2);

        follow.run(Then::PickColor, 3);
        assert_eq!(picks.load(Ordering::SeqCst), 1);
        assert_eq!(settles(), 3);

        follow.run(Then::StartMenu, 3);
        assert_eq!(keys(&desktop, "start_menu"), 1);
        assert_eq!(settles(), 4);

        follow.run(Then::SaveQr { text: "hi".into() }, 3);
        assert_eq!(*saved.lock().unwrap(), ["hi"]);
        assert_eq!(settles(), 5);

        // Nothing reached a part it should not have.
        assert_eq!(keys(&desktop, "ctrl_v"), 1);
        assert_eq!(keys(&desktop, "win_v"), 1);
        assert_eq!(keys(&desktop, "start_menu"), 1);
        assert_eq!(picks.load(Ordering::SeqCst), 1);
        assert_eq!(launcher.calls().len(), 1);
    }

    fn paste_follow(desktop: Arc<FakeDesktop>) -> FollowUps {
        FollowUps {
            desktop,
            launcher: Arc::new(RecordingLauncher::default()),
            previous: || Some(7),
            pick_color: Box::new(|| {}),
            save_qr: Box::new(|_| {}),
            settle: count_settle,
            shows: || 3,
            ops: Arc::new(FakeFileOps::default()),
            reopen: Box::new(|_| {}),
        }
    }

    #[test]
    fn a_paste_carries_the_number_read_before_the_set() {
        // The clipboard still holds number 9, the one read before the set:
        // the item never arrived, so nothing is pasted.
        let desktop = Arc::new(FakeDesktop::new(vec![Some(7)]).sequence(vec![9]));
        paste_follow(desktop.clone()).run(Then::Paste { before: 9 }, 3);
        assert!(desktop.calls().iter().all(|c| c != "ctrl_v" && !c.starts_with("bring_to_front")));

        let desktop = Arc::new(FakeDesktop::new(vec![Some(7)]).sequence(vec![10]));
        paste_follow(desktop.clone()).run(Then::Paste { before: 9 }, 3);
        assert_eq!(desktop.calls().iter().filter(|c| *c == "ctrl_v").count(), 1);
    }

    #[test]
    fn a_paste_for_an_earlier_show_sends_nothing() {
        let desktop = Arc::new(FakeDesktop::new(vec![Some(7)]));
        paste_follow(desktop.clone()).run(Then::Paste { before: 0 }, 2);
        assert!(desktop.calls().iter().all(|c| c != "ctrl_v" && !c.starts_with("bring_to_front")));
    }

    /// Follow-ups over a fake file ops; `reopened` collects the errors shown.
    fn file_follow(ops: Arc<FakeFileOps>, shows: fn() -> u64, reopened: Arc<std::sync::Mutex<Vec<String>>>) -> FollowUps {
        FollowUps {
            desktop: Arc::new(FakeDesktop::new(vec![Some(7)])),
            launcher: Arc::new(RecordingLauncher::default()),
            previous: || Some(7),
            pick_color: Box::new(|| {}),
            save_qr: Box::new(|_| {}),
            settle: count_settle,
            shows,
            ops,
            reopen: Box::new(move |error| reopened.lock().unwrap().push(error.to_string())),
        }
    }

    #[test]
    fn compress_and_cloud_pin_reach_the_ops() {
        let ops = Arc::new(FakeFileOps::default());
        let reopened = Arc::default();
        let follow = file_follow(ops.clone(), || 3, Arc::clone(&reopened));
        follow.run(Then::Compress { path: r"C:\d\a.txt".into() }, 3);
        follow.run(Then::CloudPin { path: r"C:\d\b.txt".into(), keep: true }, 3);
        follow.run(Then::CloudPin { path: r"C:\d\c.txt".into(), keep: false }, 3);
        assert_eq!(
            ops.calls(),
            [r"compress C:\d\a.txt", r"set_cloud_pin C:\d\b.txt true", r"set_cloud_pin C:\d\c.txt false"]
        );
        assert!(reopened.lock().unwrap().is_empty(), "success shows nothing");
    }

    #[test]
    fn after_hide_reopens_on_failure_only_in_the_same_session() {
        let ops = Arc::new(FakeFileOps::default());
        ops.fail("compress", "disk full");
        ops.fail("set_cloud_pin", "access denied");
        let reopened = Arc::default();
        let follow = file_follow(ops.clone(), || 3, Arc::clone(&reopened));
        follow.run(Then::Compress { path: r"C:\d\a.txt".into() }, 3);
        follow.run(Then::CloudPin { path: r"C:\d\a.txt".into(), keep: true }, 3);
        assert_eq!(*reopened.lock().unwrap(), ["disk full", "access denied"]);
        // The bar was shown again since: the error stays unsaid.
        follow.run(Then::Compress { path: r"C:\d\a.txt".into() }, 2);
        follow.run(Then::CloudPin { path: r"C:\d\a.txt".into(), keep: false }, 2);
        assert_eq!(reopened.lock().unwrap().len(), 2);
        assert_eq!(ops.calls().len(), 4, "the work still ran");
    }
}
