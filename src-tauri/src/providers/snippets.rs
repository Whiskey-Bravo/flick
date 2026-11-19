//! Snippets: texts saved in the config (`[[snippet]]`, a name and a text).
//! Type the name; Enter puts the text on the clipboard and pastes it into the
//! window that was in front, Ctrl+Enter only copies it. `snippets` lists them
//! all. Nothing is counted or remembered: the texts live in the config only.

use std::sync::{Arc, RwLock};

use crate::clipboard::Clipboard;
use crate::config::SnippetDef;
use crate::dirs::one_line;
use crate::fuzzy;
use crate::paste::Sequence;
use crate::provider::{own_name_score, HelpLine, Modifier, Outcome, Provider, Query, ResultItem, Then};

const NAME: &str = "snippets";
const ICON: &str = "builtin:clipboard";
/// The word that lists every snippet.
pub const LIST_WORD: &str = "snippets";
/// Characters of the text shown under the name.
const PREVIEW: usize = 90;
const GONE: &str = "That snippet is gone.";

pub struct SnippetsProvider {
    defs: RwLock<Vec<SnippetDef>>,
    clipboard: Arc<dyn Clipboard>,
    sequence: Sequence,
}

impl SnippetsProvider {
    pub fn new(clipboard: Arc<dyn Clipboard>, sequence: Sequence) -> Self {
        Self { defs: RwLock::new(Vec::new()), clipboard, sequence }
    }

    /// Follows the config's `[[snippet]]` tables.
    pub fn set(&self, defs: Vec<SnippetDef>) {
        *self.defs.write().unwrap_or_else(|e| e.into_inner()) = defs;
    }

    /// The snippets, read in place: a query copies only the rows it returns.
    fn defs(&self) -> std::sync::RwLockReadGuard<'_, Vec<SnippetDef>> {
        self.defs.read().unwrap_or_else(|e| e.into_inner())
    }
}

fn id_of(def: &SnippetDef) -> String {
    format!("snippet:{}", def.name.to_lowercase())
}

fn row(def: &SnippetDef, score: u32) -> ResultItem {
    ResultItem {
        id: id_of(def),
        provider: NAME,
        title: def.name.clone(),
        subtitle: format!("Snippet \u{b7} {}", one_line(&def.text, PREVIEW)),
        score,
        icon: ICON.to_string(),
        ..Default::default()
    }
}

impl Provider for SnippetsProvider {
    fn name(&self) -> &'static str {
        NAME
    }

    fn query(&self, q: &Query) -> Vec<ResultItem> {
        let text = q.text.trim();
        if text.is_empty() {
            return Vec::new();
        }
        let defs = self.defs();
        if text.eq_ignore_ascii_case(LIST_WORD) {
            // In file order, all above whatever else the word finds.
            return defs.iter().enumerate().map(|(i, d)| row(d, 2000u32.saturating_sub(i as u32))).collect();
        }
        let mut rows: Vec<ResultItem> =
            defs.iter().filter_map(|d| fuzzy::score(text, &d.name).map(|s| row(d, own_name_score(s)))).collect();
        rows.sort_by(|a, b| b.score.cmp(&a.score));
        rows
    }

    fn activate(&self, id: &str, modifier: Modifier) -> Result<Outcome, String> {
        let def = self.defs().iter().find(|d| id_of(d) == id).cloned().ok_or_else(|| GONE.to_string())?;
        // Read before the set: the paste waits for the clipboard to move on.
        let before = (self.sequence)();
        self.clipboard.set_text(&def.text)?;
        Ok(match modifier {
            Modifier::None => Outcome::HideThen { then: Then::Paste { before } },
            // Pasting while Ctrl (and Shift) are held would send the target
            // more than Ctrl+V: only copy.
            Modifier::Ctrl | Modifier::CtrlShift => Outcome::Hide,
        })
    }

    /// The texts are yours and may be private: never counted, never a recent
    /// launch, never a saved search.
    fn rank_key(&self, _id: &str) -> Option<String> {
        None
    }

    fn help(&self) -> Vec<HelpLine> {
        vec![HelpLine {
            example: "your snippets".into(),
            about: "type a snippet's name to paste its text; add them in Settings".into(),
            fill: LIST_WORD.into(),
        }]
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::clipboard::FakeClipboard;
    use std::sync::atomic::{AtomicU32, Ordering};

    fn def(name: &str, text: &str) -> SnippetDef {
        SnippetDef { name: name.into(), text: text.into() }
    }

    fn setup() -> (SnippetsProvider, Arc<FakeClipboard>) {
        let clip = Arc::new(FakeClipboard::default());
        let counter = Arc::new(AtomicU32::new(100));
        let p = SnippetsProvider::new(clip.clone(), Arc::new(move || counter.fetch_add(1, Ordering::SeqCst)));
        p.set(vec![def("sig", "Best regards,\nSam"), def("address", "1 Main Street"), def("Signoff", "Cheers")]);
        (p, clip)
    }

    fn q(text: &str) -> Query {
        Query { text: text.into(), scope: None }
    }

    #[test]
    fn its_whole_name_puts_the_snippet_above_an_open_window_that_starts_with_it() {
        let (p, _) = setup();
        let window = fuzzy::score("sig", "Signal").unwrap() + crate::providers::windows::WINDOW_BONUS + 300;
        assert!(p.query(&q("sig"))[0].score > window);
        assert_eq!(p.query(&q("addr"))[0].score, 900 + 50, "part of a name: as before");
    }

    #[test]
    fn typing_a_name_lists_the_snippet_with_a_one_line_preview() {
        let (p, _) = setup();
        let rows = p.query(&q("addr"));
        assert_eq!(rows.len(), 1);
        assert_eq!((rows[0].id.as_str(), rows[0].title.as_str()), ("snippet:address", "address"));
        assert_eq!(rows[0].subtitle, "Snippet \u{b7} 1 Main Street");
        assert_eq!(rows[0].provider, "snippets");
        assert!(!rows[0].pinnable && !rows[0].confirm);
        // The whole name beats a name that merely starts with it.
        let rows = p.query(&q("sig"));
        assert_eq!(rows.iter().map(|r| r.title.as_str()).collect::<Vec<_>>(), ["sig", "Signoff"]);
        assert_eq!(rows[0].subtitle, "Snippet \u{b7} Best regards,\u{23CE}Sam");
        // The text is never searched, only the name.
        assert!(p.query(&q("street")).is_empty());
        assert!(p.query(&q("")).is_empty());
    }

    #[test]
    fn the_list_word_shows_all_in_file_order() {
        let (p, _) = setup();
        let rows = p.query(&q("Snippets"));
        assert_eq!(rows.iter().map(|r| r.title.as_str()).collect::<Vec<_>>(), ["sig", "address", "Signoff"]);
        assert!(rows.windows(2).all(|w| w[0].score > w[1].score));
        p.set(vec![]);
        assert!(p.query(&q("snippets")).is_empty());
    }

    #[test]
    fn enter_copies_the_whole_text_then_pastes_and_ctrl_enter_only_copies() {
        let (p, clip) = setup();
        assert_eq!(
            p.activate("snippet:sig", Modifier::None),
            Ok(Outcome::HideThen { then: Then::Paste { before: 100 } })
        );
        assert_eq!(p.activate("snippet:signoff", Modifier::Ctrl), Ok(Outcome::Hide));
        assert_eq!(p.activate("snippet:address", Modifier::CtrlShift), Ok(Outcome::Hide));
        assert_eq!(*clip.0.lock().unwrap(), ["Best regards,\nSam", "Cheers", "1 Main Street"]);
    }

    #[test]
    fn a_snippet_removed_since_it_was_listed_copies_nothing() {
        let (p, clip) = setup();
        p.set(vec![]);
        assert_eq!(p.activate("snippet:sig", Modifier::None), Err(GONE.to_string()));
        assert!(clip.0.lock().unwrap().is_empty());
    }

    #[test]
    fn nothing_is_learned_and_help_fills_the_list_word() {
        let (p, _) = setup();
        assert_eq!(p.rank_key("snippet:sig"), None);
        assert!(p.item("snippet:sig").is_none(), "never a recent launch or a pin");
        let help = p.help();
        assert_eq!((help[0].example.as_str(), help[0].fill.as_str()), ("your snippets", "snippets"));
    }
}
