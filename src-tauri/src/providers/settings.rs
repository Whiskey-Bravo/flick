//! Settings provider: Windows Settings pages and a few classic system tools.

use crate::fuzzy;
use crate::launch;
use crate::quote::system32;
use crate::provider::{HelpLine, Modifier, Outcome, Provider, Query, ResultItem};

pub struct SettingsPage {
    pub title: &'static str,
    /// An `ms-settings:` URI, or a program the shell can open.
    pub uri: &'static str,
    /// Parameters for the program; empty for most entries.
    pub args: &'static str,
    pub keywords: &'static [&'static str],
}

const fn page(
    title: &'static str,
    uri: &'static str,
    keywords: &'static [&'static str],
) -> SettingsPage {
    SettingsPage { title, uri, args: "", keywords }
}

/// A program started with parameters.
const fn page_with(
    title: &'static str,
    program: &'static str,
    args: &'static str,
    keywords: &'static [&'static str],
) -> SettingsPage {
    SettingsPage { title, uri: program, args, keywords }
}

/// URIs are from Microsoft's "Launch the Windows Settings app" reference.
pub const PAGES: &[SettingsPage] = &[
    page("Bluetooth & devices", "ms-settings:bluetooth", &["bluetooth", "pair", "devices"]),
    page("Display", "ms-settings:display", &["screen", "resolution", "monitor", "brightness"]),
    page("Sound", "ms-settings:sound", &["audio", "volume", "speakers", "microphone"]),
    page("Wi-Fi", "ms-settings:network-wifi", &["wifi", "wireless", "internet"]),
    page("Network & internet", "ms-settings:network-status", &["network", "ethernet", "vpn", "proxy"]),
    page(
        "Installed apps",
        "ms-settings:appsfeatures",
        &["add or remove programs", "uninstall", "programs", "apps and features"],
    ),
    page("Default apps", "ms-settings:defaultapps", &["default browser", "file associations", "open with"]),
    page("Windows Update", "ms-settings:windowsupdate", &["update", "upgrade", "patch"]),
    page("Storage", "ms-settings:storagesense", &["disk space", "storage sense", "cleanup"]),
    page("Power & battery", "ms-settings:powersleep", &["power", "battery", "energy"]),
    page("Personalisation", "ms-settings:personalization", &["personalization", "theme", "colors", "customize"]),
    page("Background", "ms-settings:personalization-background", &["wallpaper", "desktop picture", "image"]),
    page("Taskbar", "ms-settings:taskbar", &["task bar", "start menu", "icons"]),
    page("Mouse", "ms-settings:mousetouchpad", &["touchpad", "pointer", "cursor", "scroll"]),
    page("Typing", "ms-settings:typing", &["keyboard", "autocorrect", "text suggestions"]),
    page("Date & time", "ms-settings:dateandtime", &["clock", "time zone", "timezone"]),
    page("Language", "ms-settings:regionlanguage", &["region", "input method", "keyboard layout"]),
    page("Notifications", "ms-settings:notifications", &["alerts", "focus", "do not disturb"]),
    page("Privacy & security", "ms-settings:privacy", &["privacy", "permissions", "location", "camera"]),
    page("Startup apps", "ms-settings:startupapps", &["startup", "autostart", "boot", "login items"]),
    page("About", "ms-settings:about", &["system info", "pc name", "specs", "version"]),
    page("Accounts", "ms-settings:yourinfo", &["account", "user", "profile", "your info"]),
    page("Sign-in options", "ms-settings:signinoptions", &["password", "pin", "windows hello", "login"]),
    page("Printers & scanners", "ms-settings:printers", &["printer", "scanner", "print"]),
    page("Control Panel", "control.exe", &["classic settings", "legacy"]),
    page("Device Manager", "devmgmt.msc", &["drivers", "hardware"]),
    page("Task Manager", "taskmgr.exe", &["processes", "performance", "end task"]),
    page_with(
        "Environment variables",
        "rundll32.exe",
        "sysdm.cpl,EditEnvironmentVariables",
        &["env", "path variable"],
    ),
];

/// Best of the title score and (keyword score - 50).
fn page_score(text: &str, p: &SettingsPage) -> Option<u32> {
    let title = fuzzy::score(text, p.title);
    let keyword = p
        .keywords
        .iter()
        .filter_map(|k| fuzzy::score(text, k))
        .max()
        .map(|s| s.saturating_sub(50).max(1));
    title.max(keyword)
}

impl SettingsPage {
    /// Result id: the URI or program, plus the parameters when there are any,
    /// so two entries for one program (say `rundll32.exe`) do not collide.
    pub fn id(&self) -> String {
        if self.args.is_empty() {
            self.uri.to_string()
        } else {
            format!("{} {}", self.uri, self.args)
        }
    }
}

/// A bare program name started with parameters comes from System32 by
/// absolute path, so nothing on `PATH` or next to Flick runs in its place.
fn program_path(program: &str) -> String {
    if program.contains(['\\', '/']) {
        program.to_string()
    } else {
        system32(program)
    }
}

fn open_page(p: &SettingsPage) -> Result<(), String> {
    if p.uri.starts_with("ms-settings:") {
        launch::open_uri(p.uri)
    } else if !p.args.is_empty() {
        launch::open_with(&program_path(p.uri), p.args, false)
    } else {
        launch::open(p.uri)
    }
}

pub struct SettingsProvider;

impl Provider for SettingsProvider {
    fn name(&self) -> &'static str {
        "settings"
    }

    fn query(&self, q: &Query) -> Vec<ResultItem> {
        let text = q.text.trim();
        if text.is_empty() {
            return Vec::new();
        }
        let mut hits: Vec<ResultItem> = PAGES
            .iter()
            .filter_map(|p| {
                page_score(text, p).map(|score| ResultItem {
                    id: p.id(),
                    provider: "settings",
                    title: p.title.to_string(),
                    subtitle: "Settings".to_string(),
                    score,
                    confirm: false,
                })
            })
            .collect();
        hits.sort_by(|a, b| b.score.cmp(&a.score));
        hits
    }

    fn help(&self) -> Vec<HelpLine> {
        vec![HelpLine {
            example: "bluetooth".into(),
            about: "open a Windows Settings page".into(),
            fill: String::new(),
        }]
    }

    fn activate(&self, id: &str, _modifier: Modifier) -> Result<Outcome, String> {
        let p = PAGES
            .iter()
            .find(|p| p.id() == id)
            .ok_or_else(|| "unknown settings page".to_string())?;
        open_page(p)?;
        Ok(Outcome::Hide)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    fn q(text: &str) -> Query {
        Query { text: text.into(), scope: None }
    }

    #[test]
    fn has_help_line() {
        let help = SettingsProvider.help();
        assert_eq!(help.len(), 1);
        assert_eq!(help[0].example, "bluetooth");
        assert_eq!(help[0].about, "open a Windows Settings page");
        assert_eq!(help[0].fill, "");
    }

    #[test]
    fn bluetooth_by_title() {
        let r = SettingsProvider.query(&q("blue"));
        assert_eq!(r[0].id, "ms-settings:bluetooth");
    }

    #[test]
    fn installed_apps_by_synonym() {
        let r = SettingsProvider.query(&q("add or remove"));
        assert_eq!(r[0].title, "Installed apps");
    }

    #[test]
    fn page_ids_are_unique() {
        let set: HashSet<_> = PAGES.iter().map(SettingsPage::id).collect();
        assert_eq!(set.len(), PAGES.len());
        // Entries without parameters keep their old id (pick counts survive).
        assert_eq!(PAGES[0].id(), "ms-settings:bluetooth");
        // Two entries for the same program do not collide.
        let a = page_with("A", "rundll32.exe", "one.cpl,X", &[]);
        let b = page_with("B", "rundll32.exe", "two.cpl,Y", &[]);
        assert_ne!(a.id(), b.id());
    }

    #[test]
    fn programs_with_parameters_start_from_system32() {
        let p = PAGES.iter().find(|p| p.title == "Environment variables").unwrap();
        let exe = program_path(p.uri);
        assert!(exe.ends_with(r"\System32\rundll32.exe") && std::path::Path::new(&exe).is_absolute(), "{exe}");
    }

    #[test]
    fn empty_query_returns_nothing() {
        assert!(SettingsProvider.query(&q("  ")).is_empty());
    }

    #[test]
    fn keyword_hits_score_50_below_the_same_title_hit() {
        // "wallpaper" is an exact keyword of Background: 1000 - 50.
        let r = SettingsProvider.query(&q("wallpaper"));
        assert_eq!(r[0].title, "Background");
        assert_eq!(r[0].score, 950);
        assert_eq!(SettingsProvider.query(&q("background"))[0].score, 1000);
    }

    #[test]
    fn env_finds_environment_variables() {
        let r = SettingsProvider.query(&q("env"));
        assert_eq!(r[0].title, "Environment variables");
        assert_eq!(SettingsProvider.query(&q("path variable"))[0].title, "Environment variables");
        let p = PAGES.iter().find(|p| p.title == "Environment variables").unwrap();
        assert_eq!((p.uri, p.args), ("rundll32.exe", "sysdm.cpl,EditEnvironmentVariables"));
        assert_eq!(r[0].id, p.id());
    }

    #[test]
    fn sleep_is_not_a_settings_keyword() {
        assert!(SettingsProvider.query(&q("sleep")).iter().all(|r| r.title != "Power & battery"));
    }
}
