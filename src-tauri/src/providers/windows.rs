//! Windows provider: switch to, close or end open windows.

use std::sync::Arc;

use crate::fuzzy;
use crate::provider::{HelpLine, Modifier, Outcome, Provider, Query, ResultItem};
use crate::winlist::{WinInfo, WindowList};

/// Above the engine's learned-boost cap (300), so a window you are looking for
/// outranks an app that merely has a habit.
pub const WINDOW_BONUS: u32 = 310;
/// The bonus applies only when the app's name matches at least this well
/// (word prefix or better); weaker matches rank like any other row.
const STRONG: u32 = 800;
/// Window rows a plain search may take among other providers' results.
const PLAIN_SHARE: usize = 3;

pub struct WindowsProvider {
    list: Arc<WindowList>,
}

impl WindowsProvider {
    pub fn new(list: Arc<WindowList>) -> Self {
        Self { list }
    }
}

const NEEDS_ADMIN: &str = "Needs administrator";

fn file_name(exe: &str) -> String {
    std::path::Path::new(exe).file_name().map(|s| s.to_string_lossy().to_lowercase()).unwrap_or_default()
}

fn stem(exe: &str) -> String {
    std::path::Path::new(exe).file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default()
}

fn is_explorer(exe: &str) -> bool {
    file_name(exe) == "explorer.exe"
}

/// All windows of one app (one exe path).
struct Group {
    app: String,
    exe: String,
    wins: Vec<WinInfo>,
}

impl Group {
    fn elevated(&self) -> bool {
        self.wins.iter().any(|w| w.elevated)
    }
}

fn groups(snap: &[WinInfo]) -> Vec<Group> {
    let mut out: Vec<Group> = Vec::new();
    for w in snap {
        match out.iter_mut().find(|g| g.exe.eq_ignore_ascii_case(&w.exe)) {
            Some(g) => g.wins.push(w.clone()),
            None => out.push(Group { app: w.app.clone(), exe: w.exe.clone(), wins: vec![w.clone()] }),
        }
    }
    out
}

fn app_score(text: &str, g: &Group) -> Option<u32> {
    fuzzy::score(text, &g.app).into_iter().chain(fuzzy::score(text, &stem(&g.exe))).max()
}

fn window_row(w: &WinInfo, score: u32) -> ResultItem {
    ResultItem {
        id: format!("win:{}", w.hwnd),
        provider: "windows",
        title: w.title.clone(),
        subtitle: format!("{} \u{b7} window", w.app),
        score,
        icon: format!("file:{}", w.exe),
        ..Default::default()
    }
}

fn close_row(g: &Group, score: u32) -> ResultItem {
    let n = g.wins.len();
    ResultItem {
        id: format!("close:{}", g.exe.to_lowercase()),
        provider: "windows",
        title: format!("Close {} \u{b7} {} window{}", g.app, n, if n == 1 { "" } else { "s" }),
        subtitle: if g.elevated() { NEEDS_ADMIN.to_string() } else { "Asks each window to close".to_string() },
        score,
        icon: format!("file:{}", g.exe),
        ..Default::default()
    }
}

fn kill_row(g: &Group, score: u32) -> ResultItem {
    ResultItem {
        id: format!("kill:{}", g.exe.to_lowercase()),
        provider: "windows",
        title: format!("End {}", g.app),
        subtitle: if g.elevated() { NEEDS_ADMIN.to_string() } else { "Unsaved work is lost".to_string() },
        score,
        confirm: true,
        icon: format!("file:{}", g.exe),
        ..Default::default()
    }
}

fn quit_all_row(score: u32) -> ResultItem {
    ResultItem {
        id: "quit-all".to_string(),
        provider: "windows",
        title: "Close all windows".to_string(),
        subtitle: "Apps may still ask to save".to_string(),
        score,
        confirm: true,
        icon: "builtin:power".to_string(),
        ..Default::default()
    }
}

/// `close <text>` / `kill <text>` -> (verb, text).
fn command(text: &str) -> Option<(&'static str, &str)> {
    let (verb, rest) = text.split_once(char::is_whitespace)?;
    let rest = rest.trim();
    let verb = if verb.eq_ignore_ascii_case("close") {
        "close"
    } else if verb.eq_ignore_ascii_case("kill") {
        "kill"
    } else {
        return None;
    };
    (!rest.is_empty()).then_some((verb, rest))
}

impl Provider for WindowsProvider {
    fn name(&self) -> &'static str {
        "windows"
    }

    fn query(&self, q: &Query) -> Vec<ResultItem> {
        let text = q.text.trim();
        if text.is_empty() {
            return Vec::new();
        }
        let snap = self.list.snapshot();
        if text.eq_ignore_ascii_case("quit all") {
            return vec![quit_all_row(1000)];
        }
        let mut rows: Vec<ResultItem> = if let Some((verb, rest)) = command(text) {
            groups(&snap)
                .iter()
                .filter(|g| verb == "close" || !is_explorer(&g.exe))
                .filter_map(|g| {
                    let s = app_score(rest, g)? + WINDOW_BONUS;
                    Some(if verb == "close" { close_row(g, s) } else { kill_row(g, s) })
                })
                .collect()
        } else {
            snap.iter()
                .filter_map(|w| {
                    // The bonus puts a window above its own app's row, so it needs the
                    // app's name to match well; a title match alone ranks like an app.
                    let title = fuzzy::score(text, &w.title);
                    let app = fuzzy::score(text, &w.app).into_iter().chain(fuzzy::score(text, &stem(&w.exe))).max();
                    let boosted = app.filter(|s| *s >= STRONG).map(|s| s + WINDOW_BONUS);
                    let best = title.into_iter().chain(boosted).chain(app).max()?;
                    Some(window_row(w, best))
                })
                .collect()
        };
        rows.sort_by(|a, b| b.score.cmp(&a.score));
        rows
    }

    fn item(&self, id: &str) -> Option<ResultItem> {
        if id == "quit-all" {
            return Some(quit_all_row(0));
        }
        let snap = self.list.snapshot();
        if let Some(h) = id.strip_prefix("win:") {
            let h: isize = h.parse().ok()?;
            return snap.iter().find(|w| w.hwnd == h).map(|w| window_row(w, 0));
        }
        if let Some(exe) = id.strip_prefix("close:") {
            return groups(&snap).iter().find(|g| g.exe.to_lowercase() == exe).map(|g| close_row(g, 0));
        }
        if let Some(exe) = id.strip_prefix("kill:") {
            return groups(&snap)
                .iter()
                .find(|g| g.exe.to_lowercase() == exe && !is_explorer(&g.exe))
                .map(|g| kill_row(g, 0));
        }
        None
    }

    fn plain_share(&self) -> usize {
        PLAIN_SHARE
    }

    fn help(&self) -> Vec<HelpLine> {
        vec![HelpLine {
            example: "close chrome / quit all".into(),
            about: "close or end an app; `kill` forces it".into(),
            fill: String::new(),
        }]
    }

    /// Windows come and go; nothing here is worth learning.
    fn rank_key(&self, _id: &str) -> Option<String> {
        None
    }

    fn activate(&self, id: &str, _modifier: Modifier) -> Result<Outcome, String> {
        let ops = self.list.ops();
        let snap = self.list.snapshot();
        if id == "quit-all" {
            for w in snap.iter().filter(|w| !is_explorer(&w.exe) && !w.elevated) {
                let _ = ops.close(w.hwnd, w.pid);
            }
            return Ok(Outcome::Hide);
        }
        if let Some(h) = id.strip_prefix("win:") {
            let h: isize = h.parse().map_err(|_| "That window is gone.".to_string())?;
            if !ops.exists(h) {
                return Err("That window is gone.".to_string());
            }
            ops.focus(h)?;
            return Ok(Outcome::Hide);
        }
        let (verb, exe) = if let Some(e) = id.strip_prefix("close:") {
            ("close", e)
        } else if let Some(e) = id.strip_prefix("kill:") {
            ("kill", e)
        } else {
            return Err("unknown window action".to_string());
        };
        let g = groups(&snap)
            .into_iter()
            .find(|g| g.exe.to_lowercase() == exe)
            .ok_or_else(|| "Those windows are gone.".to_string())?;
        if g.elevated() {
            return Err(NEEDS_ADMIN.to_string());
        }
        if verb == "close" {
            let closed = g.wins.iter().filter(|w| ops.close(w.hwnd, w.pid).is_ok()).count();
            if closed == 0 {
                return Err("That window is gone.".to_string());
            }
        } else {
            if is_explorer(&g.exe) {
                return Err("Explorer can't be ended from here.".to_string());
            }
            let mut pids: Vec<u32> = g.wins.iter().map(|w| w.pid).collect();
            pids.sort_unstable();
            pids.dedup();
            let mut failed: Option<String> = None;
            for pid in pids {
                if let Err(e) = ops.kill(pid, &g.exe) {
                    failed.get_or_insert(e);
                }
            }
            if let Some(e) = failed {
                return Err(e);
            }
        }
        Ok(Outcome::Hide)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::winlist::WindowOps;
    use std::sync::Mutex;

    struct FakeOps {
        wins: Mutex<Vec<WinInfo>>,
        gone: Mutex<Vec<isize>>,
        calls: Mutex<Vec<String>>,
        args: Mutex<Vec<String>>,
    }

    impl FakeOps {
        fn calls(&self) -> Vec<String> {
            self.calls.lock().unwrap().clone()
        }
    }

    impl WindowOps for FakeOps {
        fn list(&self) -> Vec<WinInfo> {
            self.wins.lock().unwrap().clone()
        }
        fn exists(&self, hwnd: isize) -> bool {
            !self.gone.lock().unwrap().contains(&hwnd)
        }
        fn focus(&self, hwnd: isize) -> Result<(), String> {
            self.calls.lock().unwrap().push(format!("focus {hwnd}"));
            Ok(())
        }
        fn close(&self, hwnd: isize, pid: u32) -> Result<(), String> {
            self.calls.lock().unwrap().push(format!("close {hwnd}"));
            self.args.lock().unwrap().push(format!("close {hwnd} {pid}"));
            Ok(())
        }
        fn kill(&self, pid: u32, exe: &str) -> Result<(), String> {
            self.calls.lock().unwrap().push(format!("kill {pid}"));
            self.args.lock().unwrap().push(format!("kill {pid} {exe}"));
            Ok(())
        }
    }

    fn win(hwnd: isize, pid: u32, title: &str, exe: &str, app: &str, elevated: bool) -> WinInfo {
        WinInfo { hwnd, pid, title: title.into(), exe: exe.into(), app: app.into(), elevated }
    }

    fn setup(wins: Vec<WinInfo>) -> (WindowsProvider, Arc<FakeOps>) {
        let ops = Arc::new(FakeOps { wins: Mutex::new(wins), gone: Mutex::new(vec![]), calls: Mutex::new(vec![]), args: Mutex::new(vec![]) });
        let list = WindowList::new(ops.clone());
        list.refresh();
        (WindowsProvider::new(list), ops)
    }

    fn q(text: &str) -> Query {
        Query { text: text.into(), scope: None }
    }

    fn mail_and_word() -> Vec<WinInfo> {
        vec![
            win(1, 10, "Inbox - Mail", r"C:\Apps\Mail\mail.exe", "Mail", false),
            win(2, 11, "Report.docx - Word", r"C:\Apps\Word\winword.exe", "Word", false),
        ]
    }

    fn chromes() -> Vec<WinInfo> {
        vec![
            win(1, 20, "Docs", r"C:\Apps\Chrome\chrome.exe", "Google Chrome", false),
            win(2, 20, "News", r"C:\Apps\Chrome\chrome.exe", "Google Chrome", false),
            win(3, 30, "C:\\", r"C:\Windows\explorer.exe", "Windows Explorer", false),
        ]
    }

    #[test]
    fn typing_part_of_a_title_lists_the_window() {
        let (p, _) = setup(mail_and_word());
        let rows = p.query(&q("inbox"));
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].id, "win:1");
        assert_eq!(rows[0].title, "Inbox - Mail");
        assert_eq!(rows[0].subtitle, "Mail · window");
        assert_eq!(rows[0].icon, r"file:C:\Apps\Mail\mail.exe");
        assert!(!rows[0].pinnable && !rows[0].confirm);
        assert_eq!(rows[0].provider, "windows");
    }

    #[test]
    fn window_rows_score_above_the_learned_cap() {
        let (p, _) = setup(mail_and_word());
        let rows = p.query(&q("mail"));
        assert!(rows[0].score >= fuzzy::score("mail", "Mail").unwrap() + 310);
        assert_eq!(WINDOW_BONUS, 310);
    }

    #[test]
    fn only_a_strong_app_name_match_gets_the_bonus() {
        let (p, _) = setup(vec![win(1, 10, "Wordle - Google Chrome", r"C:\Apps\Chrome\chrome.exe", "Google Chrome", false)]);
        // The title alone matches: ranks like any app row, no bonus.
        let title_only = p.query(&q("wordle"))[0].score;
        assert_eq!(title_only, fuzzy::score("wordle", "Wordle - Google Chrome").unwrap());
        assert_eq!(p.query(&q("word"))[0].score, fuzzy::score("word", "Wordle - Google Chrome").unwrap());
        // A weak (substring) app name match gets none either.
        assert_eq!(p.query(&q("oogle"))[0].score, fuzzy::score("oogle", "Google Chrome").unwrap());
        // The app's name (or exe) matching well gets it.
        assert_eq!(p.query(&q("chrome"))[0].score, 1000 + WINDOW_BONUS);
        assert_eq!(p.query(&q("google"))[0].score, 900 + WINDOW_BONUS);
    }

    #[test]
    fn plain_search_takes_few_window_rows() {
        let (p, _) = setup(mail_and_word());
        assert_eq!(p.plain_share(), 3);
    }

    #[test]
    fn enter_focuses_the_window_and_hides() {
        let (p, ops) = setup(mail_and_word());
        assert_eq!(p.activate("win:1", Modifier::None), Ok(Outcome::Hide));
        assert_eq!(ops.calls(), vec!["focus 1"]);
    }

    #[test]
    fn a_window_gone_since_show_reports_it() {
        let (p, ops) = setup(mail_and_word());
        ops.gone.lock().unwrap().push(1);
        assert_eq!(p.activate("win:1", Modifier::None), Err("That window is gone.".to_string()));
        assert!(ops.calls().is_empty());
    }

    #[test]
    fn close_groups_windows_by_app() {
        let (p, ops) = setup(chromes());
        let rows = p.query(&q("close chrome"));
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].title, "Close Google Chrome · 2 windows");
        assert_eq!(rows[0].id, r"close:c:\apps\chrome\chrome.exe");
        assert!(!rows[0].confirm);
        assert_eq!(p.activate(&rows[0].id, Modifier::None), Ok(Outcome::Hide));
        assert_eq!(ops.calls(), vec!["close 1", "close 2"]);
        assert_eq!(ops.args.lock().unwrap().clone(), vec!["close 1 20", "close 2 20"]);
        let one = p.query(&q("close explorer"));
        assert_eq!(one[0].title, "Close Windows Explorer · 1 window");
    }

    #[test]
    fn kill_needs_confirm_and_never_offers_explorer() {
        let (p, ops) = setup(chromes());
        let rows = p.query(&q("kill chrome"));
        assert_eq!(rows.len(), 1);
        assert!(rows[0].confirm);
        assert_eq!(rows[0].title, "End Google Chrome");
        assert_eq!(rows[0].subtitle, "Unsaved work is lost");
        assert!(p.query(&q("kill explorer")).is_empty());
        assert_eq!(p.activate(&rows[0].id, Modifier::None), Ok(Outcome::Hide));
        assert_eq!(ops.calls(), vec!["kill 20"]);
        assert_eq!(ops.args.lock().unwrap().clone(), vec![r"kill 20 C:\Apps\Chrome\chrome.exe"]);
        assert!(p.activate(r"kill:c:\windows\explorer.exe", Modifier::None).is_err());
    }

    #[test]
    fn quit_all_closes_everything_but_explorer() {
        let (p, ops) = setup(chromes());
        let rows = p.query(&q("Quit All"));
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].id, "quit-all");
        assert_eq!(rows[0].title, "Close all windows");
        assert_eq!(rows[0].subtitle, "Apps may still ask to save");
        assert!(rows[0].confirm);
        assert_eq!(p.activate("quit-all", Modifier::None), Ok(Outcome::Hide));
        assert_eq!(ops.calls(), vec!["close 1", "close 2"]);
    }

    #[test]
    fn elevated_windows_can_be_switched_to_but_not_closed_or_ended() {
        let (p, ops) = setup(vec![win(5, 50, "Task Manager", r"C:\Windows\taskmgr.exe", "Task Manager", true)]);
        let rows = p.query(&q("task"));
        assert_eq!(rows[0].subtitle, "Task Manager \u{b7} window");
        assert_eq!(p.activate("win:5", Modifier::None), Ok(Outcome::Hide));
        assert_eq!(ops.calls(), vec!["focus 5"]);
        ops.calls.lock().unwrap().clear();
        let rows = p.query(&q("close task"));
        assert_eq!(rows[0].subtitle, "Needs administrator");
        assert_eq!(p.activate(&rows[0].id, Modifier::None), Err("Needs administrator".to_string()));
        let rows = p.query(&q("kill task"));
        assert_eq!(rows[0].subtitle, "Needs administrator");
        assert_eq!(p.activate(&rows[0].id, Modifier::None), Err("Needs administrator".to_string()));
        assert!(ops.calls().is_empty());
    }

    #[test]
    fn rows_are_never_learned_and_can_be_rebuilt() {
        let (p, _) = setup(mail_and_word());
        assert_eq!(p.rank_key("win:1"), None);
        assert_eq!(p.item("win:1").unwrap().score, 0);
        assert!(p.item("win:99").is_none());
        assert_eq!(p.item("quit-all").unwrap().title, "Close all windows");
    }

    #[test]
    fn nothing_listed_until_refresh() {
        let ops = Arc::new(FakeOps {
            wins: Mutex::new(mail_and_word()),
            gone: Mutex::new(vec![]),
            calls: Mutex::new(vec![]),
            args: Mutex::new(vec![]),
        });
        let list = WindowList::new(ops);
        assert!(list.snapshot().is_empty());
        assert!(WindowsProvider::new(list.clone()).query(&q("inbox")).is_empty());
        list.refresh();
        assert_eq!(list.snapshot().len(), 2);
    }

    #[test]
    fn refresh_replaces_the_snapshot() {
        let (_p, ops) = setup(mail_and_word());
        let list = WindowList::new(ops.clone());
        list.refresh();
        *ops.wins.lock().unwrap() = vec![win(9, 90, "Only", r"C:\x\only.exe", "Only", false)];
        list.refresh();
        let snap = list.snapshot();
        assert_eq!(snap.len(), 1);
        assert_eq!(snap[0].hwnd, 9);
    }

    #[test]
    fn terminate_and_close_need_a_live_match() {
        use crate::winlist::{may_close, may_terminate};
        let chrome = r"C:\Apps\Chrome\chrome.exe";
        assert!(may_terminate(chrome, &chrome.to_uppercase(), 20, 1));
        assert!(!may_terminate(r"C:\Otherpp.exe", chrome, 20, 1));
        assert!(!may_terminate(chrome, chrome, 1, 1));
        assert!(!may_terminate(chrome, chrome, 0, 1));
        assert!(!may_terminate("", "", 20, 1));
        let ex = r"C:\Windows\Explorer.exe";
        assert!(!may_terminate(ex, ex, 30, 1));
        assert!(may_close(20, 20, 1, "Chrome_WidgetWin_1"));
        assert!(!may_close(21, 20, 1, "Chrome_WidgetWin_1"));
        assert!(!may_close(1, 1, 1, "X"));
        assert!(!may_close(0, 0, 1, "X"));
        assert!(!may_close(20, 20, 1, "Progman"));
        assert!(!may_close(20, 20, 1, "shell_traywnd"));
    }

    #[test]
    fn listable_skips_shell_tool_cloaked_and_own_windows() {
        use crate::winlist::listable;
        assert!(listable("Chrome_WidgetWin_1", 5, 1, "Docs", false, false, true));
        assert!(!listable("Progman", 5, 1, "Program Manager", false, false, true));
        assert!(!listable("WorkerW", 5, 1, "x", false, false, true));
        assert!(!listable("Shell_TrayWnd", 5, 1, "x", false, false, true));
        assert!(!listable("Shell_SecondaryTrayWnd", 5, 1, "x", false, false, true));
        assert!(!listable("A", 5, 1, "x", true, false, true));
        assert!(!listable("A", 5, 1, "x", false, true, true));
        assert!(!listable("A", 5, 1, "x", false, false, false));
        assert!(!listable("A", 1, 1, "x", false, false, true));
        assert!(!listable("A", 5, 1, "  ", false, false, true));
    }
}
