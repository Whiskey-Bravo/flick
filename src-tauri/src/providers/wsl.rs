//! WSL commands: `> command` runs the command in the configured distro in a new terminal.

use std::sync::{Arc, RwLock};

use crate::launch::{Launcher, Shell};
use crate::provider::{HelpLine, Modifier, Outcome, Provider, Query, ResultItem, Route};

/// `> command`.
pub const ROUTE: Route = Route { prefix: ">", needs_space: false };

pub struct WslProvider {
    launcher: Arc<dyn Launcher>,
    distro: RwLock<String>,
}

impl WslProvider {
    pub fn new(launcher: Arc<dyn Launcher>, distro: &str) -> Arc<Self> {
        Arc::new(Self { launcher, distro: RwLock::new(distro.to_string()) })
    }

    /// Called from the config reload thread while queries run.
    pub fn set_distro(&self, distro: &str) {
        *self.distro.write().unwrap_or_else(|e| e.into_inner()) = distro.to_string();
    }

    fn distro(&self) -> String {
        self.distro.read().unwrap_or_else(|e| e.into_inner()).clone()
    }
}

impl Provider for WslProvider {
    fn name(&self) -> &'static str {
        "wsl"
    }

    fn query(&self, q: &Query) -> Vec<ResultItem> {
        // No distro means no WSL: offer nothing.
        if q.text.trim().is_empty() || self.distro().is_empty() {
            return Vec::new();
        }
        vec![ResultItem {
            id: q.text.clone(),
            provider: "wsl",
            title: format!("> {}", q.text),
            subtitle: format!("run in {}", self.distro()),
            score: 1000,
            icon: "builtin:wsl".into(),
            ..Default::default()
        }]
    }

    fn activate(&self, id: &str, _modifier: Modifier) -> Result<Outcome, String> {
        let shell = Shell::Wsl { distro: self.distro() };
        // No folder: the command starts in the Linux home.
        self.launcher.run_command(&shell, id, &[], true, false, None)?;
        Ok(Outcome::Hide)
    }

    /// Typed command lines are never learned or written to disk.
    fn rank_key(&self, _id: &str) -> Option<String> {
        None
    }

    fn route(&self) -> Option<Route> {
        Some(ROUTE)
    }

    fn help(&self) -> Vec<HelpLine> {
        if self.distro().is_empty() {
            return Vec::new();
        }
        vec![HelpLine {
            example: "> htop -d 10".into(),
            about: format!("run a command in WSL ({}) in a new terminal", self.distro()),
            fill: "> ".into(),
        }]
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::launch::testing::RecordingLauncher;

    fn setup() -> (Arc<WslProvider>, Arc<RecordingLauncher>) {
        let rec = Arc::new(RecordingLauncher::default());
        (WslProvider::new(rec.clone(), "Debian"), rec)
    }

    fn q(text: &str) -> Query {
        Query { text: text.into(), scope: None }
    }

    #[test]
    fn empty_command_gives_nothing() {
        let (p, _) = setup();
        assert!(p.query(&q("")).is_empty());
        assert!(p.query(&q("   ")).is_empty());
    }

    #[test]
    fn one_result_echoing_the_command() {
        let (p, _) = setup();
        let r = p.query(&q("htop -d 10"));
        assert_eq!(r.len(), 1);
        assert_eq!(r[0].id, "htop -d 10");
        assert_eq!(r[0].provider, "wsl");
        assert_eq!(r[0].title, "> htop -d 10");
        assert_eq!(r[0].subtitle, "run in Debian");
        assert_eq!(r[0].score, 1000);
        assert!(!r[0].confirm);
    }

    #[test]
    fn activate_runs_in_configured_distro_with_terminal_kept_open() {
        let (p, rec) = setup();
        for m in [Modifier::None, Modifier::Ctrl, Modifier::CtrlShift] {
            assert_eq!(p.activate("ls -la", m), Ok(Outcome::Hide));
        }
        assert_eq!(
            rec.calls(),
            vec!["run_command Wsl(Debian) env=[] keep=true elevated=false cwd=-: ls -la"; 3]
        );
    }

    #[test]
    fn launcher_error_is_returned_unchanged() {
        let (p, rec) = setup();
        rec.fail_next("invalid wsl distro name");
        assert_eq!(p.activate("ls", Modifier::None), Err("invalid wsl distro name".to_string()));
        assert_eq!(rec.calls().len(), 1);
    }

    #[test]
    fn set_distro_changes_subtitle_and_launch() {
        let (p, rec) = setup();
        p.set_distro("Ubuntu");
        assert_eq!(p.query(&q("whoami"))[0].subtitle, "run in Ubuntu");
        assert_eq!(p.help()[0].about, "run a command in WSL (Ubuntu) in a new terminal");
        p.activate("whoami", Modifier::None).unwrap();
        assert_eq!(rec.calls(), vec!["run_command Wsl(Ubuntu) env=[] keep=true elevated=false cwd=-: whoami"]);
    }

    #[test]
    fn no_distro_hides_wsl_results_and_help() {
        let rec = Arc::new(RecordingLauncher::default());
        let p = WslProvider::new(rec.clone(), "");
        assert!(p.query(&q("ls")).is_empty());
        assert!(p.help().is_empty());
    }

    #[test]
    fn commands_are_never_ranked() {
        let (p, _) = setup();
        assert_eq!(p.rank_key("htop -d 10"), None);
    }

    #[test]
    fn routes_on_greater_than() {
        let (p, _) = setup();
        assert_eq!(p.name(), "wsl");
        assert_eq!(p.route(), Some(Route { prefix: ">", needs_space: false }));
    }

    #[test]
    fn help_line_shows_the_example() {
        let (p, _) = setup();
        assert_eq!(
            p.help(),
            vec![HelpLine {
                example: "> htop -d 10".into(),
                about: "run a command in WSL (Debian) in a new terminal".into(),
                fill: "> ".into(),
            }]
        );
    }
}
