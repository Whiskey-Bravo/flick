//! What runs once the bar has hidden: the paste into the window that was in
//! front, Windows' clipboard history, the snip overlay and the colour picker.

use std::sync::Arc;
use std::time::Duration;

use crate::commands::AfterHide;
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
    /// Waits until the hidden bar is off the screen.
    pub settle: fn(),
    /// How many times the bar has been shown (`window::shows`).
    pub shows: fn() -> u64,
}

impl AfterHide for FollowUps {
    fn run(&self, then: Then, started: u64) {
        // The overlay, Win+V and the picker's frozen frame must not show the
        // bar, so those wait for it to go; a paste does not.
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
            Then::StartMenu => {
                (self.settle)();
                let _ = crate::controls::open_start_menu();
            }
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
        let follow = FollowUps {
            desktop: desktop.clone(),
            launcher: launcher.clone(),
            previous: || Some(7),
            pick_color: Box::new(move || {
                counter.fetch_add(1, Ordering::SeqCst);
            }),
            settle: count_settle,
            shows: || 3,
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

        // Nothing reached a part it should not have.
        assert_eq!(keys(&desktop, "ctrl_v"), 1);
        assert_eq!(launcher.calls().len(), 1);
    }

    fn paste_follow(desktop: Arc<FakeDesktop>) -> FollowUps {
        FollowUps {
            desktop,
            launcher: Arc::new(RecordingLauncher::default()),
            previous: || Some(7),
            pick_color: Box::new(|| {}),
            settle: count_settle,
            shows: || 3,
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
}
