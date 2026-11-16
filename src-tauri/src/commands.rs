//! Commands the bar front end invokes. None of them run on the main thread.

use tauri::{AppHandle, State};

use crate::launch;
use crate::provider::{Modifier, Outcome, ResultItem};
use crate::window;
use crate::AppState;

/// In-memory only. `async` runs it on the async runtime instead of the main thread.
#[tauri::command(async)]
pub fn query(text: String, scope: Option<String>, state: State<AppState>) -> Vec<ResultItem> {
    state.engine.query(&text, scope.as_deref())
}

/// What to do with the bar once an activation has finished.
#[derive(Debug, PartialEq)]
enum After {
    Hide,
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
        After::Hide | After::Reveal if started != now => After::Leave,
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
    match after_for(after, started, window::shows()) {
        After::Hide => window::hide(&app),
        After::Reveal => window::reveal(&app),
        After::Leave => {}
    }
    result
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

/// Shows the item's location in the file manager, then hides the bar.
/// Opening a window can block, so it runs on a blocking-pool thread.
#[tauri::command]
pub async fn reveal(provider: String, id: String, state: State<'_, AppState>, app: AppHandle) -> Result<(), String> {
    let engine = state.engine.clone();
    tauri::async_runtime::spawn_blocking(move || engine.reveal(&provider, &id))
        .await
        .map_err(|e| format!("reveal failed: {e}"))
        .and_then(|r| r)?;
    window::hide(&app);
    Ok(())
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
    fn fill_and_step_outcomes_never_hide_the_bar() {
        let fill = || Ok(Outcome::Fill { text: "search ".into() });
        let step = || Ok(Outcome::Step { prompt: "proj".into(), scope: "shortcut:proj".into() });
        for m in [Modifier::None, Modifier::Ctrl, Modifier::CtrlShift] {
            assert_eq!(settle(fill(), m), (fill(), After::Leave));
            assert_eq!(settle(step(), m), (step(), After::Leave));
        }
    }
}
