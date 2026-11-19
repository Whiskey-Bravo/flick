//! Commands the bar front end invokes. None of them run on the main thread.

use std::sync::Arc;

use tauri::{AppHandle, Manager, State};

use crate::launch;
use crate::provider::{Action, Modifier, Outcome, ResultItem, Then};
use crate::providers::path::Unfolded;
use crate::window;
use crate::AppState;

/// In-memory only. `async` runs it on the async runtime instead of the main thread.
/// `seq` is the page's number for this search: a search is cancelled only by a
/// higher number, so the newest one always completes. Its answer is `None` when
/// a higher number began meanwhile; the page has asked again and drops it, and
/// it is never shown as "no results". No `seq` (or 0): never cancelled.
#[tauri::command(async)]
pub fn query(text: String, scope: Option<String>, seq: Option<u64>, state: State<AppState>) -> Option<Vec<ResultItem>> {
    let timer = crate::timing::query_timer();
    let rows = state.engine.query_newest(&text, scope.as_deref(), seq.unwrap_or(0));
    timer.finish(&text, rows.as_ref().map_or(0, Vec::len));
    rows
}

/// The fresh row of a live row (the clock), for the bar's once-a-second refresh.
/// In-memory only; nothing is ranked, recorded or pinned, and it does not cancel
/// a search in progress.
#[tauri::command(async)]
pub fn live_row(provider: String, id: String, state: State<AppState>) -> Option<ResultItem> {
    let timer = crate::timing::tick_timer();
    let item = live_item(&provider, &id, |p, i| state.engine.item(p, i));
    timer.finish();
    item
}

/// What a folder row has to show under it: memory only, the read runs on the
/// loader's threads. Nothing is ranked or recorded and no search is cancelled.
#[tauri::command(async)]
pub fn unfold(path: String, state: State<AppState>) -> Unfolded {
    state.engine.unfold(&path)
}

/// Only the `answers` provider has live rows: any other provider is not asked
/// (`item` can reach providers that read devices or the disk).
fn live_item(provider: &str, id: &str, ask: impl FnOnce(&str, &str) -> Option<ResultItem>) -> Option<ResultItem> {
    if provider != "answers" {
        return None;
    }
    ask(provider, id).filter(|i| i.live)
}

/// Runs a follow-up once the bar has hidden (paste, clipboard history, snip,
/// colour picker). Looked up as managed state `Arc<dyn AfterHide>`; without
/// it a follow-up is skipped.
pub trait AfterHide: Send + Sync {
    /// `started` is the show count when the bar hid: a follow-up that is
    /// still waiting when the bar is shown again gives up.
    fn run(&self, then: Then, started: u64);
}

/// What to do with the bar once an activation has finished.
#[derive(Debug, PartialEq)]
enum After {
    Hide,
    /// Hide, then run the follow-up.
    Then(Then),
    /// Show again (without `bar:shown`) so the error can be seen.
    Reveal,
    Leave,
}

/// A cancelled UAC prompt is not an error: hide silently, like Win search
/// (the engine has already skipped recording a pick, as for any `Err`).
/// Other errors bring the bar back only after Ctrl+Shift+Enter, where the UAC
/// prompt took focus; otherwise the user may have moved on (a broken shortcut
/// takes seconds) and a reveal would steal their keystrokes.
fn settle(result: Result<Outcome, String>, modifier: Modifier) -> (Result<Outcome, String>, After) {
    match result {
        Ok(Outcome::Hide) => (Ok(Outcome::Hide), After::Hide),
        Ok(Outcome::HideThen { then }) => (Ok(Outcome::Hide), After::Then(then)),
        // The bar stays: it shows the filled text, the next step, or stays as is.
        Ok(outcome @ (Outcome::Fill { .. } | Outcome::Step { .. } | Outcome::Keep)) => (Ok(outcome), After::Leave),
        Err(e) if e == launch::CANCELLED => (Ok(Outcome::Hide), After::Hide),
        Err(e) if modifier == Modifier::CtrlShift => (Err(e), After::Reveal),
        Err(e) => (Err(e), After::Leave),
    }
}

/// `after` for an activation that began at show count `started`, now that the
/// count is `now`: when the bar was shown again meanwhile it belongs to a new
/// session, which a late result must neither hide nor bring back.
fn after_for(after: After, started: u64, now: u64) -> After {
    match after {
        After::Hide | After::Then(_) | After::Reveal if started != now => After::Leave,
        other => other,
    }
}

/// Activation can block for seconds (UAC prompt, broken shortcut), so it runs
/// on a blocking-pool thread. See `settle` and `after_for` for what happens
/// to the bar.
#[tauri::command]
pub async fn activate(
    provider: String,
    id: String,
    modifier: Modifier,
    text: String,
    scope: Option<String>,
    state: State<'_, AppState>,
    app: AppHandle,
) -> Result<Outcome, String> {
    let engine = state.engine.clone();
    let started = window::shows();
    let result = tauri::async_runtime::spawn_blocking(move || engine.activate(&provider, &id, modifier, &text, scope.as_deref()))
        .await
        .map_err(|e| format!("activation failed: {e}"))
        .and_then(|r| r);
    let (result, after) = settle(result, modifier);
    finish(&app, after_for(after, started, window::shows()));
    result
}

/// Does what `after` says to the bar. A follow-up runs on its own thread,
/// never on the async runtime: a paste waits up to 500 ms and the colour
/// picker runs a message loop.
fn finish(app: &AppHandle, after: After) {
    match after {
        After::Hide => window::hide(app),
        After::Then(then) => {
            let started = window::shows();
            window::hide(app);
            if let Some(follow) = app.try_state::<Arc<dyn AfterHide>>() {
                let follow = Arc::clone(&follow);
                std::thread::spawn(move || follow.run(then, started));
            }
        }
        After::Reveal => window::reveal(app),
        After::Leave => {}
    }
}

/// Pins or unpins an item; returns whether it is now pinned.
#[tauri::command(async)]
pub fn toggle_pin(provider: String, id: String, state: State<AppState>) -> bool {
    state.engine.toggle_pin(&provider, &id)
}

/// Past searches, newest first (for Up/Down in the bar).
#[tauri::command(async)]
pub fn past_searches(state: State<AppState>) -> Vec<String> {
    state.engine.past_searches()
}

/// Runs a row's menu action (show in Explorer, copy path, ...). Opening a
/// window can block, so it runs on a blocking-pool thread; the bar is then
/// handled like after `activate`.
#[tauri::command]
pub async fn act(
    provider: String,
    id: String,
    action: Action,
    state: State<'_, AppState>,
    app: AppHandle,
) -> Result<Outcome, String> {
    let engine = state.engine.clone();
    let started = window::shows();
    let result = tauri::async_runtime::spawn_blocking(move || engine.act(&provider, &id, action))
        .await
        .map_err(|e| format!("action failed: {e}"))
        .and_then(|r| r);
    let (result, after) = settle(result, Modifier::None);
    finish(&app, after_for(after, started, window::shows()));
    result
}

/// Renames a file or folder row; the new row id. The bar stays. The shell
/// call can block, so it runs on a blocking-pool thread.
#[tauri::command]
pub async fn rename(provider: String, id: String, new_name: String, state: State<'_, AppState>) -> Result<String, String> {
    let engine = state.engine.clone();
    tauri::async_runtime::spawn_blocking(move || engine.rename(&provider, &id, &new_name))
        .await
        .map_err(|e| format!("rename failed: {e}"))
        .and_then(|r| r)
}

#[tauri::command]
pub fn hide(app: AppHandle) {
    window::hide(&app);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::launch;

    #[test]
    fn live_rows_come_only_from_the_answers_provider() {
        let live = || Some(ResultItem { live: true, ..Default::default() });
        // Another provider is not asked at all.
        let mut asked = false;
        let out = live_item("devices", "connect:headset", |_, _| {
            asked = true;
            live()
        });
        assert!(out.is_none());
        assert!(!asked, "the engine must not be called for another provider");
        // `answers` is asked, and only a live row comes back.
        assert!(live_item("answers", "clock", |_, _| live()).is_some());
        assert!(live_item("answers", "clock", |_, _| Some(ResultItem::default())).is_none());
    }

    #[test]
    fn cancelled_elevation_hides_silently() {
        for m in [Modifier::None, Modifier::Ctrl, Modifier::CtrlShift] {
            assert_eq!(settle(Err(launch::CANCELLED.into()), m), (Ok(Outcome::Hide), After::Hide));
        }
    }

    #[test]
    fn errors_reveal_the_bar_only_after_ctrl_shift() {
        let err = || Err("file not found".to_string());
        assert_eq!(settle(err(), Modifier::CtrlShift), (err(), After::Reveal));
        assert_eq!(settle(err(), Modifier::None), (err(), After::Leave));
        assert_eq!(settle(err(), Modifier::Ctrl), (err(), After::Leave));
    }

    #[test]
    fn hide_outcome_hides_and_others_leave_the_bar() {
        assert_eq!(settle(Ok(Outcome::Hide), Modifier::None), (Ok(Outcome::Hide), After::Hide));
        assert_eq!(settle(Ok(Outcome::Keep), Modifier::None), (Ok(Outcome::Keep), After::Leave));
    }

    #[test]
    fn a_bar_shown_again_meanwhile_is_left_alone() {
        assert_eq!(after_for(After::Hide, 3, 4), After::Leave);
        assert_eq!(after_for(After::Reveal, 3, 4), After::Leave);
        assert_eq!(after_for(After::Leave, 3, 4), After::Leave);
        assert_eq!(after_for(After::Hide, 3, 3), After::Hide);
        assert_eq!(after_for(After::Reveal, 3, 3), After::Reveal);
        assert_eq!(after_for(After::Leave, 3, 3), After::Leave);
    }

    #[test]
    fn hide_then_hides_and_runs_the_follow_up() {
        for m in [Modifier::None, Modifier::Ctrl, Modifier::CtrlShift] {
            for then in [Then::Paste { before: 0 }, Then::ClipboardHistory, Then::Snip, Then::PickColor, Then::StartMenu] {
                assert_eq!(settle(Ok(Outcome::HideThen { then: then.clone() }), m), (Ok(Outcome::Hide), After::Then(then)));
            }
        }
    }

    #[test]
    fn a_follow_up_is_dropped_when_the_bar_was_shown_again() {
        assert_eq!(after_for(After::Then(Then::Paste { before: 0 }), 3, 4), After::Leave);
        assert_eq!(after_for(After::Then(Then::Paste { before: 0 }), 3, 3), After::Then(Then::Paste { before: 0 }));
    }

    #[test]
    fn fill_and_step_outcomes_never_hide_the_bar() {
        let fill = || Ok(Outcome::Fill { text: "search ".into() });
        let step = || Ok(Outcome::Step { prompt: "proj".into(), scope: "shortcut:proj".into() });
        for m in [Modifier::None, Modifier::Ctrl, Modifier::CtrlShift] {
            assert_eq!(settle(fill(), m), (fill(), After::Leave));
            assert_eq!(settle(step(), m), (step(), After::Leave));
        }
    }
}
