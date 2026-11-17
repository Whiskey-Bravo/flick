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
}

impl AfterHide for FollowUps {
    fn run(&self, then: Then) {
        if then == Then::Paste {
            // The item is already on the clipboard: when the paste cannot
            // happen, a copy is what the user gets.
            let _ = paste_into((self.previous)(), self.desktop.as_ref(), PASTE_WAIT, POLL);
            return;
        }
        // The overlay, Win+V and the picker's frozen frame must not show the bar.
        (self.settle)();
        match then {
            Then::ClipboardHistory => {
                let _ = self.desktop.send_win_v();
            }
            Then::Snip => {
                let _ = self.launcher.open(SNIP_URI);
            }
            Then::PickColor => (self.pick_color)(),
            Then::Paste => {}
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
        };
        let keys = |d: &FakeDesktop, key: &str| d.calls().iter().filter(|c| *c == key).count();

        follow.run(Then::Paste);
        assert_eq!(keys(&desktop, "ctrl_v"), 1);
        assert_eq!(keys(&desktop, "win_v"), 0);
        assert_eq!(settles(), 0, "a paste does not wait");

        follow.run(Then::ClipboardHistory);
        assert_eq!(keys(&desktop, "win_v"), 1);
        assert_eq!(settles(), 1);

        follow.run(Then::Snip);
        assert_eq!(launcher.calls(), ["open ms-screenclip:"]);
        assert_eq!(settles(), 2);

        follow.run(Then::PickColor);
        assert_eq!(picks.load(Ordering::SeqCst), 1);
        assert_eq!(settles(), 3);

        // Nothing reached a part it should not have.
        assert_eq!(keys(&desktop, "ctrl_v"), 1);
        assert_eq!(launcher.calls().len(), 1);
    }
}
