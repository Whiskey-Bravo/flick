//! Provider interface: what every result source implements.

use std::sync::Arc;

pub struct Query {
    pub text: String,
    pub scope: Option<String>,
}

#[derive(Clone, Copy, PartialEq, Debug, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Modifier {
    None,
    Ctrl,
    CtrlShift,
}

/// Where a row came from in the bar.
#[derive(Clone, Copy, Debug, Default, PartialEq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Origin {
    #[default]
    Search,
    Pin,
    Recent,
}

/// Something a row offers besides its Enter action, shown in its actions menu.
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Action {
    /// Show the item in Explorer.
    Reveal,
    /// Open a folder in Explorer.
    OpenFolder,
    CopyPath,
    /// Run as administrator. The bar sends it as Ctrl+Shift+Enter through
    /// `activate`, never to [`Provider::act`].
    Admin,
    Uninstall,
    Close,
    CloseWindow,
}

#[derive(Clone, Debug, Default, serde::Serialize)]
pub struct ResultItem {
    /// Unique within its provider, stable across queries.
    pub id: String,
    pub provider: &'static str,
    pub title: String,
    pub subtitle: String,
    /// From the provider, before ranking boost: normally 0..=1000; shortcuts
    /// add 50 so a user's own command wins a tie.
    pub score: u32,
    /// UI must ask for a second Enter.
    pub confirm: bool,
    /// Icon key: `file:<path>`, `ext:<.lower>`, `folder`, or `builtin:<name>`.
    pub icon: String,
    pub origin: Origin,
    /// Set by the engine.
    pub pinned: bool,
    pub pinnable: bool,
    /// The actions menu, in order. Pin/Unpin is added by the bar from `pinnable`.
    pub actions: Vec<Action>,
    /// Text Tab puts in the bar ("" = none).
    pub complete: String,
    /// A computed answer: in plain queries the engine puts it above every other row.
    #[serde(skip)]
    pub top: bool,
}

/// A provider that owns a typed prefix: when the text matches, only it is queried.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Route {
    pub prefix: &'static str,
    /// true for word prefixes ("help"): the prefix must be followed by a space or the end.
    pub needs_space: bool,
}

/// One line of the built-in help list.
#[derive(Clone, Debug, PartialEq)]
pub struct HelpLine {
    /// What to type, shown as the title, e.g. "> htop -d 10".
    pub example: String,
    /// One short sentence, shown as the subtitle.
    pub about: String,
    /// Text put in the bar when the line is picked ("" = just clear).
    pub fill: String,
}

#[derive(Debug, PartialEq, serde::Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Outcome {
    /// Done, hide the bar.
    Hide,
    /// Done, bar stays.
    Keep,
    /// Show `prompt`; next queries carry `scope`.
    Step { prompt: String, scope: String },
    /// Put `text` in the bar and keep it open (picked help lines).
    Fill { text: String },
}

pub trait Provider: Send + Sync {
    fn name(&self) -> &'static str;
    /// Scopes this provider answers when `Query.scope` is Some. Default: none.
    fn owns_scope(&self, _scope: &str) -> bool {
        false
    }
    fn query(&self, q: &Query) -> Vec<ResultItem>;
    fn activate(&self, id: &str, modifier: Modifier) -> Result<Outcome, String>;
    /// Key under which picks of `id` are counted and boosted. `None` opts the
    /// item out of learning: nothing is boosted and nothing is written to disk.
    /// Providers whose ids carry typed text must return `None` or a key without it.
    fn rank_key(&self, id: &str) -> Option<String> {
        Some(id.to_string())
    }
    /// Typed prefix this provider owns. Default: none (queried for plain text).
    fn route(&self) -> Option<Route> {
        None
    }
    /// Lines for the built-in `help` list, in display order.
    fn help(&self) -> Vec<HelpLine> {
        vec![]
    }
    /// Rows this provider may take in a plain (unrouted, unscoped) query
    /// while other providers have results; rows left over after every other
    /// result still go to it. Default: no limit.
    fn plain_share(&self) -> usize {
        usize::MAX
    }
    /// Called by the engine for a query that did not reach this provider
    /// (routed, scoped, help or blank text). Memory only. Default: nothing.
    fn skipped(&self) {}
    /// Rows shown when the bar is opened with no text (welcome, later
    /// favourites and history). Default: none.
    fn empty_query(&self) -> Vec<ResultItem> {
        vec![]
    }
    /// The row `query` would return for `id` (score 0), if it can be rebuilt.
    fn item(&self, _id: &str) -> Option<ResultItem> {
        None
    }
    /// False until the provider's first scan has been swapped in.
    fn ready(&self) -> bool {
        true
    }
    /// Runs one of the row's [`ResultItem::actions`] (never `Admin`). The
    /// engine records nothing for it.
    fn act(&self, _id: &str, _action: Action) -> Result<Outcome, String> {
        Err("can't do that here".to_string())
    }
    /// Whether this provider owns `text` (trimmed) by its shape, e.g. a typed
    /// path: then it alone answers it, like a route. Default: no.
    fn claims(&self, _text: &str) -> bool {
        false
    }
}

/// Lets a provider that is also used elsewhere (e.g. apps, refreshed by a
/// background thread) be handed to the engine as a shared `Arc`.
impl<P: Provider + ?Sized> Provider for Arc<P> {
    fn name(&self) -> &'static str {
        (**self).name()
    }
    fn owns_scope(&self, scope: &str) -> bool {
        (**self).owns_scope(scope)
    }
    fn query(&self, q: &Query) -> Vec<ResultItem> {
        (**self).query(q)
    }
    fn activate(&self, id: &str, modifier: Modifier) -> Result<Outcome, String> {
        (**self).activate(id, modifier)
    }
    fn rank_key(&self, id: &str) -> Option<String> {
        (**self).rank_key(id)
    }
    fn route(&self) -> Option<Route> {
        (**self).route()
    }
    fn help(&self) -> Vec<HelpLine> {
        (**self).help()
    }
    fn plain_share(&self) -> usize {
        (**self).plain_share()
    }
    fn skipped(&self) {
        (**self).skipped()
    }
    fn empty_query(&self) -> Vec<ResultItem> {
        (**self).empty_query()
    }
    fn item(&self, id: &str) -> Option<ResultItem> {
        (**self).item(id)
    }
    fn ready(&self) -> bool {
        (**self).ready()
    }
    fn act(&self, id: &str, action: Action) -> Result<Outcome, String> {
        (**self).act(id, action)
    }
    fn claims(&self, text: &str) -> bool {
        (**self).claims(text)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn action_serialises_snake_case() {
        assert_eq!(serde_json::to_value(Action::CopyPath).unwrap(), json!("copy_path"));
        assert_eq!(serde_json::to_value(Action::OpenFolder).unwrap(), json!("open_folder"));
        assert_eq!(serde_json::to_value(Action::CloseWindow).unwrap(), json!("close_window"));
        assert_eq!(serde_json::from_value::<Action>(json!("reveal")).unwrap(), Action::Reveal);
    }

    #[test]
    fn result_item_serialises_actions_and_complete() {
        let v = serde_json::to_value(ResultItem {
            actions: vec![Action::Reveal],
            complete: "x".into(),
            ..Default::default()
        })
        .unwrap();
        assert_eq!(v["actions"], json!(["reveal"]));
        assert_eq!(v["complete"], json!("x"));
        assert!(v.get("revealable").is_none() && v.get("elevatable").is_none(), "{v}");
    }
}
