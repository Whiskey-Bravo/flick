//! Power provider: lock, sleep, sign out, restart, shut down.

use std::os::windows::process::CommandExt;
use std::process::{Command, Stdio};

use crate::fuzzy;
use crate::provider::{HelpLine, Modifier, Outcome, Provider, Query, ResultItem};

/// Subtracted from the fuzzy score so an app matching the same text ranks first.
pub const POWER_PENALTY: u32 = 200;

const CREATE_NO_WINDOW: u32 = 0x0800_0000;

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum PowerAction {
    Lock,
    Sleep,
    SignOut,
    Restart,
    ShutDown,
}

/// (action, title, needs a second Enter)
const ACTIONS: [(PowerAction, &str, bool); 5] = [
    (PowerAction::Lock, "Lock", false),
    (PowerAction::Sleep, "Sleep", false),
    (PowerAction::SignOut, "Sign out", true),
    (PowerAction::Restart, "Restart", true),
    (PowerAction::ShutDown, "Shut down", true),
];

pub trait PowerRunner: Send + Sync {
    fn run(&self, a: PowerAction) -> Result<(), String>;
}

/// The real thing. Never use in tests.
pub struct SystemPower;

fn shutdown(args: &[&str]) -> Result<(), String> {
    let root = std::env::var_os("SystemRoot").unwrap_or_else(|| r"C:\Windows".into());
    let exe = std::path::PathBuf::from(root).join(r"System32\shutdown.exe");
    let status = Command::new(exe)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .creation_flags(CREATE_NO_WINDOW)
        .status()
        .map_err(|_| "could not run shutdown".to_string())?;
    if status.success() {
        Ok(())
    } else {
        Err("shutdown failed".to_string())
    }
}

impl PowerRunner for SystemPower {
    fn run(&self, a: PowerAction) -> Result<(), String> {
        use windows::Win32::System::Power::SetSuspendState;
        use windows::Win32::System::Shutdown::LockWorkStation;
        match a {
            // SAFETY: no pointers or preconditions.
            PowerAction::Lock => {
                unsafe { LockWorkStation() }.map_err(|_| "could not lock".to_string())
            }
            PowerAction::Sleep => {
                // SAFETY: no pointers or preconditions.
                if unsafe { SetSuspendState(false, false, false) } {
                    Ok(())
                } else {
                    Err("could not sleep".to_string())
                }
            }
            PowerAction::SignOut => shutdown(&["/l"]),
            PowerAction::Restart => shutdown(&["/r", "/t", "0"]),
            PowerAction::ShutDown => shutdown(&["/s", "/t", "0"]),
        }
    }
}

pub struct PowerProvider {
    runner: Box<dyn PowerRunner>,
}

impl PowerProvider {
    pub fn new(runner: Box<dyn PowerRunner>) -> Self {
        Self { runner }
    }
}

fn id_of(title: &str) -> String {
    title.to_lowercase()
}

fn row(title: &str, confirm: bool, score: u32) -> ResultItem {
    ResultItem {
        id: id_of(title),
        provider: "power",
        title: title.to_string(),
        subtitle: "Power".to_string(),
        score,
        confirm,
        icon: "builtin:power".into(),
        pinnable: true,
        ..Default::default()
    }
}

impl Provider for PowerProvider {
    fn name(&self) -> &'static str {
        "power"
    }

    fn query(&self, q: &Query) -> Vec<ResultItem> {
        let text = q.text.trim();
        if text.is_empty() {
            return Vec::new();
        }
        let mut hits: Vec<ResultItem> = ACTIONS
            .iter()
            .filter_map(|&(_, title, confirm)| {
                fuzzy::score(text, title).map(|s| row(title, confirm, s.saturating_sub(POWER_PENALTY).max(1)))
            })
            .collect();
        hits.sort_by(|a, b| b.score.cmp(&a.score));
        hits
    }

    fn item(&self, id: &str) -> Option<ResultItem> {
        ACTIONS.iter().find(|(_, title, _)| id_of(title) == id).map(|&(_, title, confirm)| row(title, confirm, 0))
    }

    /// Power actions never learn: a boost would let them outrank apps for the
    /// same text, and Lock and Sleep run on a single Enter.
    fn rank_key(&self, _id: &str) -> Option<String> {
        None
    }

    fn help(&self) -> Vec<HelpLine> {
        vec![HelpLine {
            example: "lock / sleep / restart / shut down / sign out".into(),
            about: "power actions; the last three ask for a second Enter".into(),
            fill: String::new(),
        }]
    }

    fn activate(&self, id: &str, _modifier: Modifier) -> Result<Outcome, String> {
        let &(action, _, _) = ACTIONS
            .iter()
            .find(|(_, title, _)| id_of(title) == id)
            .ok_or_else(|| "unknown power action".to_string())?;
        self.runner.run(action)?;
        Ok(Outcome::Hide)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};

    struct RecordingRunner(Arc<Mutex<Vec<PowerAction>>>);

    impl PowerRunner for RecordingRunner {
        fn run(&self, a: PowerAction) -> Result<(), String> {
            self.0.lock().unwrap().push(a);
            Ok(())
        }
    }

    fn provider() -> (PowerProvider, Arc<Mutex<Vec<PowerAction>>>) {
        let log = Arc::new(Mutex::new(Vec::new()));
        (PowerProvider::new(Box::new(RecordingRunner(log.clone()))), log)
    }

    #[test]
    fn power_rows_are_pinnable_but_not_learned() {
        let (p, _) = provider();
        let r = p.item("restart").unwrap();
        assert!(r.confirm && r.pinnable && r.actions.is_empty());
        assert_eq!((r.title.as_str(), r.score, r.icon.as_str()), ("Restart", 0, "builtin:power"));
        assert_eq!(p.rank_key("restart"), None);
        assert!(p.item("nope").is_none());
    }

    fn q(text: &str) -> Query {
        Query { text: text.into(), scope: None }
    }

    #[test]
    fn has_help_line() {
        let (p, _) = provider();
        let help = p.help();
        assert_eq!(help.len(), 1);
        assert_eq!(help[0].example, "lock / sleep / restart / shut down / sign out");
        assert_eq!(help[0].about, "power actions; the last three ask for a second Enter");
        assert_eq!(help[0].fill, "");
    }

    #[test]
    fn destructive_actions_need_confirm() {
        let (p, _) = provider();
        for (text, want) in [
            ("restart", true),
            ("shut down", true),
            ("sign out", true),
            ("lock", false),
            ("sleep", false),
        ] {
            let r = p.query(&q(text));
            assert_eq!(r[0].confirm, want, "{text}");
        }
    }

    #[test]
    fn activate_runs_the_matching_action() {
        let (p, log) = provider();
        assert_eq!(p.activate("restart", Modifier::None), Ok(Outcome::Hide));
        assert_eq!(*log.lock().unwrap(), vec![PowerAction::Restart]);
    }

    #[test]
    fn unknown_id_runs_nothing() {
        let (p, log) = provider();
        assert!(p.activate("nope", Modifier::None).is_err());
        assert!(log.lock().unwrap().is_empty());
    }

    #[test]
    fn power_scores_below_equal_app_match() {
        let (p, _) = provider();
        let r = p.query(&q("restart"));
        assert_eq!(r[0].score, 1000 - POWER_PENALTY);
    }
}
