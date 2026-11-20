//! "Open in Claude" on a folder row: the bar asks for an optional prompt,
//! then the Claude app starts a new Code session in that folder with the
//! prompt filled in (never sent). The app asks the user to confirm a folder
//! that comes from a link. Shown only while the app's `claude://` link is
//! registered (`launch::claude_app`).

use std::sync::Arc;

use crate::launch::Launcher;
use crate::provider::{Modifier, Outcome, Provider, Query, ResultItem};
use crate::providers::shortcuts::encode_url_component;

/// The bar's prompt after "Open in Claude".
pub const PROMPT: &str = "Prompt for Claude (optional)";
/// Scopes this provider answers: `claude:<folder>`.
const SCOPE: &str = "claude:";
/// The Claude app's link for a new Code session.
const NEW_SESSION: &str = "claude://code/new";
/// The Claude app's icon (as the built-in `claude` shortcut shows it).
pub const ICON: &str = r"shell:AppsFolder\Claude_pzs8sxrjxfjjc!Claude";

/// The step that asks for the prompt for `folder`.
pub fn step(folder: &str) -> Outcome {
    Outcome::Step { prompt: PROMPT.into(), scope: format!("{SCOPE}{folder}") }
}

/// The link that opens `folder` in a new Code session, with `prompt` filled
/// in when there is one.
fn link(folder: &str, prompt: &str) -> String {
    let mut link = format!("{NEW_SESSION}?folder={}", encode_url_component(folder));
    let prompt = prompt.trim();
    if !prompt.is_empty() {
        link.push_str("&q=");
        link.push_str(&encode_url_component(prompt));
    }
    link
}

pub struct ClaudeProvider {
    launcher: Arc<dyn Launcher>,
}

impl ClaudeProvider {
    pub fn new(launcher: Arc<dyn Launcher>) -> Arc<Self> {
        Arc::new(ClaudeProvider { launcher })
    }
}

impl Provider for ClaudeProvider {
    fn name(&self) -> &'static str {
        "claude"
    }

    fn owns_scope(&self, scope: &str) -> bool {
        scope.starts_with(SCOPE)
    }

    /// One row: Enter opens the folder with what was typed as the prompt.
    fn query(&self, q: &Query) -> Vec<ResultItem> {
        let Some(folder) = q.scope.as_deref().and_then(|s| s.strip_prefix(SCOPE)).filter(|f| !f.is_empty()) else {
            return Vec::new();
        };
        let prompt = q.text.trim();
        let (title, subtitle) = if prompt.is_empty() {
            ("Open in Claude without a prompt".to_string(), folder.to_string())
        } else {
            (prompt.to_string(), format!("Open {folder} in Claude with this prompt"))
        };
        vec![ResultItem {
            id: link(folder, prompt),
            provider: "claude",
            title,
            subtitle,
            score: 1000,
            icon: ICON.into(),
            ..Default::default()
        }]
    }

    fn activate(&self, id: &str, _modifier: Modifier) -> Result<Outcome, String> {
        if !id.starts_with(NEW_SESSION) {
            return Err("unknown item".into());
        }
        self.launcher.open(id)?;
        Ok(Outcome::Hide)
    }

    /// The id carries the typed prompt: never learned.
    fn rank_key(&self, _id: &str) -> Option<String> {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::launch::testing::RecordingLauncher;

    fn query(p: &ClaudeProvider, scope: &str, text: &str) -> Vec<ResultItem> {
        p.query(&Query { text: text.into(), scope: Some(scope.into()) })
    }

    #[test]
    fn the_link_carries_the_folder_and_only_a_real_prompt() {
        assert_eq!(link(r"C:\Users\me\proj", ""), "claude://code/new?folder=C%3A%5CUsers%5Cme%5Cproj");
        assert_eq!(link(r"C:\p", "  "), "claude://code/new?folder=C%3A%5Cp");
        assert_eq!(link(r"C:\my proj", " fix the tests & run "), "claude://code/new?folder=C%3A%5Cmy%20proj&q=fix%20the%20tests%20%26%20run");
    }

    #[test]
    fn the_step_asks_for_the_prompt_in_the_folders_scope() {
        let p = ClaudeProvider::new(Arc::new(RecordingLauncher::default()));
        let Outcome::Step { prompt, scope } = step(r"C:\proj") else { panic!("not a step") };
        assert_eq!(prompt, PROMPT);
        assert!(p.owns_scope(&scope));
        assert!(!p.owns_scope("shortcut:proj"));
    }

    #[test]
    fn one_row_opens_the_folder_with_what_was_typed() {
        let launcher = Arc::new(RecordingLauncher::default());
        let p = ClaudeProvider::new(launcher.clone());
        let empty = query(&p, r"claude:C:\proj", "");
        assert_eq!(empty.len(), 1);
        assert_eq!((empty[0].title.as_str(), empty[0].subtitle.as_str()), ("Open in Claude without a prompt", r"C:\proj"));
        let typed = query(&p, r"claude:C:\proj", "add a readme");
        assert_eq!(typed[0].title, "add a readme");
        assert_eq!(p.activate(&typed[0].id, Modifier::None), Ok(Outcome::Hide));
        assert_eq!(launcher.calls(), ["open claude://code/new?folder=C%3A%5Cproj&q=add%20a%20readme"]);
        assert!(p.rank_key(&typed[0].id).is_none());
    }

    #[test]
    fn nothing_outside_its_scope() {
        let p = ClaudeProvider::new(Arc::new(RecordingLauncher::default()));
        assert!(p.query(&Query { text: "claude".into(), scope: None }).is_empty());
        assert!(query(&p, "claude:", "x").is_empty());
        assert!(p.activate("https://example.com", Modifier::None).is_err());
    }
}
