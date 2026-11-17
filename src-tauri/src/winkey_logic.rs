//! Decides which Win-key presses are "lone" (pure; no Windows calls).
//!
//! The hook builds an [`Ev`] for each event it sees and asks [`Press::on`] what
//! to do. Held keys come from a snapshot Windows gives at Win-down ([`Held`]),
//! so missed key-ups (Win+L, Ctrl+Alt+Del, elevated windows) can't wedge it;
//! tracking only spans one press, from Win down to Win up.

// used by winkey.rs (Task 4)
#![allow(dead_code)]

/// Keys and buttons already down when Win went down, read from Windows
/// (`GetAsyncKeyState`) by the caller. `Default` is "nothing held".
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Held {
    pub ctrl: bool,
    pub shift: bool,
    pub alt: bool,
    pub other_win: bool,
    pub mouse_button: bool,
    /// This same Win key was already down: the event is an auto-repeat (or the
    /// start of the press was missed). A low-level hook runs before Windows
    /// updates the key state, so a fresh press reads `false` here.
    pub win_already_down: bool,
}

impl Held {
    fn any_other(&self) -> bool {
        self.ctrl || self.shift || self.alt || self.other_win || self.mouse_button
    }
}

/// One input event as the hook sees it. `flick` means the event carries
/// Flick's own tag; other programs' injected events have `flick: false` and
/// count like physical ones.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Ev {
    /// The watched Win key went down (also its auto-repeat).
    WinDown { physical: bool, held: Held },
    /// The watched Win key went up.
    WinUp,
    /// Any other key went down.
    KeyDown { flick: bool },
    MouseDown { flick: bool },
    Wheel { flick: bool },
    /// The other Win key went down while this one is held.
    OtherWinDown,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Verdict {
    /// Let the event through; nothing else to do.
    Pass,
    /// Let it through and install the mouse hook.
    ArmMouse,
    /// Let it through and remove the mouse hook.
    DisarmMouse,
    /// The press (this Win-up) was lone. The caller decides whether to take it
    /// and removes the mouse hook.
    Lone,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum State {
    /// No press in progress.
    Idle,
    /// Win is down and nothing else has happened yet.
    Armed,
    /// Win is down but this press can no longer be lone.
    Spoiled,
}

/// The state of the current Win press.
#[derive(Debug)]
pub struct Press {
    state: State,
    /// Whether the caller has the mouse hook in (as far as our verdicts said).
    mouse_hooked: bool,
}

impl Press {
    pub fn new() -> Press {
        Press { state: State::Idle, mouse_hooked: false }
    }

    /// Feed one event. `button_held_now` is the mouse button state read at
    /// Win-up; it is ignored for other events.
    pub fn on(&mut self, ev: Ev, button_held_now: bool) -> Verdict {
        match ev {
            Ev::WinUp => {
                let was = self.state;
                self.state = State::Idle;
                if was == State::Armed && !button_held_now {
                    self.mouse_hooked = false;
                    return Verdict::Lone;
                }
            }
            Ev::WinDown { physical: false, .. } => self.state = State::Spoiled,
            Ev::WinDown { physical: true, held } => {
                if held.win_already_down {
                    // An auto-repeat keeps the press as it is; a repeat with no
                    // press means its start was missed, so it never arms.
                } else if held.any_other() {
                    self.state = State::Spoiled;
                } else {
                    self.state = State::Armed;
                }
            }
            Ev::KeyDown { flick } | Ev::MouseDown { flick } | Ev::Wheel { flick } => {
                if !flick {
                    self.spoil();
                }
            }
            Ev::OtherWinDown => self.spoil(),
        }
        self.sync_mouse()
    }

    fn spoil(&mut self) {
        if self.state == State::Armed {
            self.state = State::Spoiled;
        }
    }

    /// The mouse hook is wanted exactly while a press is armed.
    fn sync_mouse(&mut self) -> Verdict {
        let want = self.state == State::Armed;
        let had = std::mem::replace(&mut self.mouse_hooked, want);
        match (had, want) {
            (false, true) => Verdict::ArmMouse,
            (true, false) => Verdict::DisarmMouse,
            _ => Verdict::Pass,
        }
    }
}

/// Whether a lone press opens Flick (and is swallowed) or is left to Windows.
pub fn take_lone(setting_on: bool, paused: bool, bar_ready: bool, full_screen: bool) -> bool {
    setting_on && !paused && bar_ready && !full_screen
}

/// How long the hook may lag behind real input before it's reinstalled.
const STALE_MS: u64 = 2_000;
/// The least time between two reinstalls.
const REINSTALL_GAP_MS: u64 = 10_000;

/// Reinstalls the hook quietly when Windows has seen input the hook hasn't.
/// `GetLastInputInfo` also counts input the hook can't see (lock screen,
/// elevated windows), so this is only a hint and is rate-limited.
#[derive(Debug)]
pub struct Watchdog {
    last_reinstall_ms: Option<u64>,
}

impl Watchdog {
    pub fn new() -> Watchdog {
        Watchdog { last_reinstall_ms: None }
    }

    pub fn should_reinstall(&mut self, now_ms: u64, last_hook_event_ms: u64, last_input_ms: u64) -> bool {
        if last_input_ms <= last_hook_event_ms.saturating_add(STALE_MS) {
            return false;
        }
        if let Some(last) = self.last_reinstall_ms {
            if now_ms.saturating_sub(last) < REINSTALL_GAP_MS {
                return false;
            }
        }
        self.last_reinstall_ms = Some(now_ms);
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn none() -> Held {
        Held::default()
    }

    fn down() -> Ev {
        Ev::WinDown { physical: true, held: none() }
    }

    fn repeat() -> Ev {
        Ev::WinDown { physical: true, held: Held { win_already_down: true, ..none() } }
    }

    fn armed() -> Press {
        let mut p = Press::new();
        assert_eq!(p.on(down(), false), Verdict::ArmMouse);
        p
    }

    #[test]
    fn lone_left_or_right_win_is_lone() {
        let mut p = Press::new();
        assert_eq!(p.on(down(), false), Verdict::ArmMouse);
        assert_eq!(p.on(Ev::WinUp, false), Verdict::Lone);
    }

    #[test]
    fn win_combination_passes() {
        let mut p = armed();
        assert_eq!(p.on(Ev::KeyDown { flick: false }, false), Verdict::DisarmMouse);
        assert_eq!(p.on(Ev::KeyDown { flick: false }, false), Verdict::Pass);
        assert_eq!(p.on(Ev::WinUp, false), Verdict::Pass);
    }

    #[test]
    fn ctrl_win_d_is_a_combination_from_the_start() {
        let mut p = Press::new();
        let held = Held { ctrl: true, ..none() };
        assert_eq!(p.on(Ev::WinDown { physical: true, held }, false), Verdict::Pass);
        assert_eq!(p.on(Ev::KeyDown { flick: false }, false), Verdict::Pass);
        assert_eq!(p.on(Ev::WinUp, false), Verdict::Pass);
        for held in [
            Held { shift: true, ..none() },
            Held { alt: true, ..none() },
            Held { other_win: true, ..none() },
            Held { mouse_button: true, ..none() },
        ] {
            let mut p = Press::new();
            assert_eq!(p.on(Ev::WinDown { physical: true, held }, false), Verdict::Pass);
            assert_eq!(p.on(Ev::WinUp, false), Verdict::Pass);
        }
    }

    #[test]
    fn click_or_wheel_spoils() {
        let mut p = armed();
        assert_eq!(p.on(Ev::MouseDown { flick: false }, false), Verdict::DisarmMouse);
        assert_eq!(p.on(Ev::WinUp, false), Verdict::Pass);

        let mut p = armed();
        assert_eq!(p.on(Ev::Wheel { flick: false }, false), Verdict::DisarmMouse);
        assert_eq!(p.on(Ev::WinUp, false), Verdict::Pass);
    }

    #[test]
    fn click_then_auto_repeat_stays_spoiled() {
        // Win keeps auto-repeating through a click; the repeats must not re-arm.
        let mut p = armed();
        assert_eq!(p.on(Ev::MouseDown { flick: false }, false), Verdict::DisarmMouse);
        assert_eq!(p.on(repeat(), false), Verdict::Pass);
        assert_eq!(p.on(repeat(), false), Verdict::Pass);
        assert_eq!(p.on(Ev::WinUp, false), Verdict::Pass);
    }

    #[test]
    fn button_held_at_up_spoils() {
        // Not a lone press; the mouse hook is still in, so it is removed.
        let mut p = armed();
        assert_eq!(p.on(Ev::WinUp, true), Verdict::DisarmMouse);
        assert_eq!(p.on(down(), false), Verdict::ArmMouse);
        assert_eq!(p.on(Ev::WinUp, false), Verdict::Lone);
    }

    #[test]
    fn flick_injections_are_ignored() {
        let mut p = armed();
        assert_eq!(p.on(Ev::KeyDown { flick: true }, false), Verdict::Pass);
        assert_eq!(p.on(Ev::MouseDown { flick: true }, false), Verdict::Pass);
        assert_eq!(p.on(Ev::Wheel { flick: true }, false), Verdict::Pass);
        assert_eq!(p.on(Ev::WinUp, false), Verdict::Lone);
    }

    #[test]
    fn other_programs_injections_spoil() {
        let mut p = armed();
        assert_eq!(p.on(Ev::KeyDown { flick: false }, false), Verdict::DisarmMouse);
        assert_eq!(p.on(Ev::WinUp, false), Verdict::Pass);
    }

    #[test]
    fn injected_win_never_arms() {
        let mut p = Press::new();
        assert_eq!(p.on(Ev::WinDown { physical: false, held: none() }, false), Verdict::Pass);
        assert_eq!(p.on(Ev::WinUp, false), Verdict::Pass);

        // An injected Win down during an armed press spoils it.
        let mut p = armed();
        assert_eq!(p.on(Ev::WinDown { physical: false, held: none() }, false), Verdict::DisarmMouse);
        assert_eq!(p.on(Ev::WinUp, false), Verdict::Pass);
    }

    #[test]
    fn auto_repeat_keeps_it_armed() {
        let mut p = armed();
        assert_eq!(p.on(repeat(), false), Verdict::Pass);
        assert_eq!(p.on(repeat(), false), Verdict::Pass);
        assert_eq!(p.on(Ev::WinUp, false), Verdict::Lone);
    }

    #[test]
    fn both_win_keys_spoil() {
        let mut p = armed();
        assert_eq!(p.on(Ev::OtherWinDown, false), Verdict::DisarmMouse);
        assert_eq!(p.on(Ev::WinUp, false), Verdict::Pass);
    }

    #[test]
    fn stale_modifiers_never_wedge() {
        // Win+L: the key-up of Win goes to the lock screen and is never seen.
        let mut p = armed();
        assert_eq!(p.on(Ev::KeyDown { flick: false }, false), Verdict::DisarmMouse);
        assert_eq!(p.on(down(), false), Verdict::ArmMouse);
        assert_eq!(p.on(Ev::WinUp, false), Verdict::Lone);

        // A missed Win-up of a press that was a combination from the start.
        let mut p = Press::new();
        let held = Held { ctrl: true, alt: true, ..none() };
        assert_eq!(p.on(Ev::WinDown { physical: true, held }, false), Verdict::Pass);
        assert_eq!(p.on(down(), false), Verdict::ArmMouse);
        assert_eq!(p.on(Ev::WinUp, false), Verdict::Lone);

        // A missed Win-up of a clean press: the new press arms without a second install.
        let mut p = armed();
        assert_eq!(p.on(down(), false), Verdict::Pass);
        assert_eq!(p.on(Ev::WinUp, false), Verdict::Lone);

        // A missed Win-up of a clean press, then a combination: the hook comes out.
        let mut p = armed();
        let held = Held { shift: true, ..none() };
        assert_eq!(p.on(Ev::WinDown { physical: true, held }, false), Verdict::DisarmMouse);
        assert_eq!(p.on(Ev::WinUp, false), Verdict::Pass);

        // Win-up with nothing armed.
        let mut p = Press::new();
        assert_eq!(p.on(Ev::WinUp, false), Verdict::Pass);
        assert_eq!(p.on(Ev::WinUp, true), Verdict::Pass);
    }

    #[test]
    fn a_repeat_with_no_press_never_arms() {
        // Hook (re)installed while Win was already held: the start was missed.
        let mut p = Press::new();
        assert_eq!(p.on(repeat(), false), Verdict::Pass);
        assert_eq!(p.on(Ev::WinUp, false), Verdict::Pass);
    }

    #[test]
    fn events_with_nothing_armed_pass() {
        let mut p = Press::new();
        for ev in [
            Ev::KeyDown { flick: false },
            Ev::MouseDown { flick: false },
            Ev::Wheel { flick: false },
            Ev::OtherWinDown,
        ] {
            assert_eq!(p.on(ev, false), Verdict::Pass);
        }
        assert_eq!(p.on(down(), false), Verdict::ArmMouse);
        assert_eq!(p.on(Ev::WinUp, false), Verdict::Lone);
    }

    #[test]
    fn take_lone_needs_everything() {
        for on in [false, true] {
            for paused in [false, true] {
                for bar_ready in [false, true] {
                    for full_screen in [false, true] {
                        let want = on && !paused && bar_ready && !full_screen;
                        assert_eq!(
                            take_lone(on, paused, bar_ready, full_screen),
                            want,
                            "{on} {paused} {bar_ready} {full_screen}"
                        );
                    }
                }
            }
        }
        assert!(take_lone(true, false, true, false));
    }

    #[test]
    fn watchdog_reinstalls_on_stale_hook_at_most_every_10s() {
        let mut w = Watchdog::new();
        assert!(w.should_reinstall(5000, 1000, 3500));
        assert!(!w.should_reinstall(9000, 1000, 3500));
        assert!(w.should_reinstall(15001, 1000, 3500));

        let mut w = Watchdog::new();
        assert!(!w.should_reinstall(5000, 1000, 3000));
        assert!(!w.should_reinstall(5000, 1000, 1500));
        assert!(!w.should_reinstall(5000, 4000, 3500));
    }
}
