//! The Settings item picker's backend: searches through the engine, like the
//! bar, and keeps only rows a workspace or a direct hotkey can store.

use crate::config::ItemRef;
use crate::engine::HELP;
use crate::provider::ResultItem;
use crate::providers::workspaces;
use crate::AppState;
use tauri::State;

/// One row of the picker.
#[derive(Clone, Debug, PartialEq, serde::Serialize)]
pub struct PickRow {
    pub provider: String,
    pub id: String,
    pub title: String,
    pub subtitle: String,
    pub icon: String,
    pub confirm: bool,
}

/// Rows the picker shows at most.
pub const MAX_PICKS: usize = 20;

/// Keeps the rows that can be stored by key (the test pins use: `pinnable`),
/// never help lines. A workspace also leaves out rows that ask for a confirm and
/// other workspaces. First [`MAX_PICKS`], order kept.
pub fn pick_rows(rows: Vec<ResultItem>, for_workspace: bool) -> Vec<PickRow> {
    rows.into_iter()
        .filter(|r| r.pinnable && r.provider != HELP)
        .filter(|r| !for_workspace || (!r.confirm && r.provider != workspaces::NAME))
        .take(MAX_PICKS)
        .map(|r| PickRow {
            provider: r.provider.to_string(),
            id: r.id,
            title: r.title,
            subtitle: r.subtitle,
            icon: r.icon,
            confirm: r.confirm,
        })
        .collect()
}

/// Whether a stored item is still there: it resolves, or its provider has not
/// finished its first scan (so it cannot tell yet).
pub fn found(item: Option<&ResultItem>, provider_ready: bool) -> bool {
    item.is_some() || !provider_ready
}

#[tauri::command(async)]
pub fn settings_search_items(text: String, for_workspace: bool, state: State<AppState>) -> Vec<PickRow> {
    // Typed commands give no row that can be stored: never start a `tr`/`def`
    // lookup or a device or drive read for text typed in the picker.
    if state.providers.typed_command(&text) {
        return Vec::new();
    }
    pick_rows(state.engine.query(&text, None), for_workspace)
}

#[tauri::command(async)]
pub fn settings_items_found(items: Vec<ItemRef>, state: State<AppState>) -> Vec<bool> {
    items
        .iter()
        .map(|i| found(state.engine.item(&i.provider, &i.id).as_ref(), state.engine.ready(&i.provider)))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(provider: &'static str, id: &str, pinnable: bool, confirm: bool) -> ResultItem {
        ResultItem {
            id: id.into(),
            provider,
            title: format!("T {id}"),
            subtitle: format!("S {id}"),
            icon: format!("builtin:{id}"),
            pinnable,
            confirm,
            ..Default::default()
        }
    }

    fn ids(rows: &[PickRow]) -> Vec<String> {
        rows.iter().map(|r| format!("{}:{}", r.provider, r.id)).collect()
    }

    #[test]
    fn picker_rows_leave_out_unstorable_rows() {
        let rows = vec![
            row("apps", "a", true, false),
            row("shortcuts", "arg:typed", false, false),
            row(HELP, "timer", true, false),
            row("files", "b", true, false),
        ];
        for for_workspace in [true, false] {
            let picks = pick_rows(rows.clone(), for_workspace);
            assert_eq!(ids(&picks), ["apps:a", "files:b"]);
            assert_eq!(
                picks[0],
                PickRow {
                    provider: "apps".into(),
                    id: "a".into(),
                    title: "T a".into(),
                    subtitle: "S a".into(),
                    icon: "builtin:a".into(),
                    confirm: false,
                }
            );
        }
        let many: Vec<ResultItem> = (0..30).map(|i| row("apps", &i.to_string(), true, false)).collect();
        let picks = pick_rows(many, true);
        assert_eq!(picks.len(), MAX_PICKS);
        assert_eq!(picks.first().unwrap().id, "0");
        assert_eq!(picks.last().unwrap().id, "19");
    }

    #[test]
    fn workspace_picker_leaves_out_confirm_rows_and_workspaces() {
        let rows = vec![
            row("power", "off", true, true),
            row(workspaces::NAME, "work", true, false),
            row("apps", "a", true, false),
        ];
        assert_eq!(ids(&pick_rows(rows, true)), ["apps:a"]);
    }

    #[test]
    fn hotkey_picker_keeps_confirm_rows_and_workspaces() {
        let rows = vec![
            row("power", "off", true, true),
            row(workspaces::NAME, "work", true, false),
            row("apps", "a", true, false),
        ];
        let picks = pick_rows(rows, false);
        assert_eq!(ids(&picks), ["power:off", "workspaces:work", "apps:a"]);
        assert!(picks[0].confirm);
    }

    #[test]
    fn found_is_true_while_a_provider_is_not_ready() {
        let r = row("apps", "a", true, false);
        assert!(found(Some(&r), true));
        assert!(found(Some(&r), false));
        assert!(found(None, false));
        assert!(!found(None, true));
    }
}
