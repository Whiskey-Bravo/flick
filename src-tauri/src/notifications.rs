//! Windows notifications: a toast under Flick's app id (AUMID) when the
//! installer registered it, otherwise (dev builds) and whenever a toast fails,
//! a balloon from a short-lived notification-area icon of Flick's own.
//! Everything else talks to the `Notifier` seam so tests never show anything.

// Nothing calls in here until the reminders and direct hotkeys are wired up.
#![allow(dead_code)]

use std::sync::mpsc;
use std::time::Duration;

use windows::core::{HSTRING, PCWSTR};
use windows::Data::Xml::Dom::XmlDocument;
use windows::Win32::Foundation::{HWND, HINSTANCE};
use windows::Win32::System::Registry::{RegCloseKey, RegOpenKeyExW, HKEY, HKEY_CURRENT_USER, KEY_READ};
use windows::Win32::UI::Shell::{
    ExtractIconW, Shell_NotifyIconW, NIF_ICON, NIF_INFO, NIF_TIP, NIIF_INFO, NIM_ADD, NIM_DELETE, NOTIFYICONDATAW,
};
use windows::Win32::UI::WindowsAndMessaging::{CreateWindowExW, DestroyIcon, DestroyWindow, HICON, HWND_MESSAGE, WINDOW_EX_STYLE, WINDOW_STYLE};
use windows::UI::Notifications::{ToastNotification, ToastNotificationManager};

use crate::launch::with_com;

/// Flick's app id: the bundle identifier, which Tauri's installer stamps on
/// the Start-menu shortcut and our NSIS hook registers under HKCU.
pub const AUMID: &str = "dev.local.flick";
/// Title of every notification Flick shows.
pub const TITLE: &str = "Flick";

/// Registry key whose presence means the installer registered [`AUMID`].
const AUMID_KEY: &str = r"Software\Classes\AppUserModelId\dev.local.flick";
/// `uID` of the balloon's notification-area icon ("FL").
const BALLOON_ID: u32 = 0x464C;
/// How long the balloon's icon stays in the notification area.
const BALLOON_LIFE: Duration = Duration::from_secs(10);
/// How long `show` waits for the balloon's icon to be added.
const BALLOON_WAIT: Duration = Duration::from_millis(900);

/// Shows a notification. Must not block more than about a second.
pub trait Notifier: Send + Sync {
    fn show(&self, title: &str, body: &str) -> Result<(), String>;
}

fn xml_escape(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&apos;"),
            c => out.push(c),
        }
    }
    out
}

/// The toast's XML: a title line, a body line and the default sound.
pub fn toast_xml(title: &str, body: &str) -> String {
    format!(
        "<toast><visual><binding template=\"ToastGeneric\"><text>{}</text><text>{}</text></binding></visual>\
         <audio src=\"ms-winsoundevent:Notification.Default\"/></toast>",
        xml_escape(title),
        xml_escape(body)
    )
}

/// Unregistered: the balloon only. Registered: the toast, and the balloon
/// only when the toast fails.
pub fn deliver(
    registered: bool,
    toast: &dyn Fn() -> Result<(), String>,
    balloon: &dyn Fn() -> Result<(), String>,
) -> Result<(), String> {
    if registered && toast().is_ok() {
        return Ok(());
    }
    balloon()
}

/// Whether the installer registered [`AUMID`] (its HKCU key exists).
pub fn registered() -> bool {
    let path = HSTRING::from(AUMID_KEY);
    let mut key = HKEY::default();
    // SAFETY: `path` is NUL-terminated and outlives the call; the key is
    // closed right after.
    unsafe {
        if RegOpenKeyExW(HKEY_CURRENT_USER, PCWSTR(path.as_ptr()), None, KEY_READ, &mut key).is_err() {
            return false;
        }
        let _ = RegCloseKey(key);
    }
    true
}

/// The real notifier: toast when registered, balloon otherwise or on failure.
pub struct SystemNotifier;

impl Notifier for SystemNotifier {
    fn show(&self, title: &str, body: &str) -> Result<(), String> {
        deliver(registered(), &|| show_toast(title, body), &|| show_balloon(title, body))
    }
}

fn show_toast(title: &str, body: &str) -> Result<(), String> {
    with_com(|| -> windows::core::Result<()> {
        let xml = XmlDocument::new()?;
        xml.LoadXml(&HSTRING::from(toast_xml(title, body)))?;
        let toast = ToastNotification::CreateToastNotification(&xml)?;
        ToastNotificationManager::CreateToastNotifierWithId(&HSTRING::from(AUMID))?.Show(&toast)
    })
    .map_err(|e| format!("toast: {e}"))
}

/// Starts the balloon's thread and waits (briefly) for its icon to be added.
fn show_balloon(title: &str, body: &str) -> Result<(), String> {
    let (title, body) = (title.to_string(), body.to_string());
    let (tx, rx) = mpsc::channel();
    std::thread::Builder::new()
        .name("flick-balloon".into())
        .spawn(move || balloon_thread(&title, &body, &tx))
        .map_err(|e| format!("balloon: {e}"))?;
    match rx.recv_timeout(BALLOON_WAIT) {
        Ok(result) => result,
        // Still adding: it shows (or not) on its own; never block longer.
        Err(mpsc::RecvTimeoutError::Timeout) => Ok(()),
        Err(mpsc::RecvTimeoutError::Disconnected) => Err("balloon: thread ended".into()),
    }
}

/// Copies `text` into a fixed UTF-16 buffer, cut to leave room for the NUL.
fn fill(buf: &mut [u16], text: &str) {
    let units: Vec<u16> = text.encode_utf16().take(buf.len() - 1).collect();
    buf[..units.len()].copy_from_slice(&units);
    buf[units.len()] = 0;
}

/// Removes the icon, frees it and destroys the window on every way out.
struct Balloon {
    hwnd: HWND,
    icon: HICON,
    added: bool,
}

impl Drop for Balloon {
    fn drop(&mut self) {
        // SAFETY: handles this thread created; each freed once, here.
        unsafe {
            if self.added {
                let data = NOTIFYICONDATAW {
                    cbSize: size_of::<NOTIFYICONDATAW>() as u32,
                    hWnd: self.hwnd,
                    uID: BALLOON_ID,
                    ..Default::default()
                };
                let _ = Shell_NotifyIconW(NIM_DELETE, &data);
            }
            if !self.icon.is_invalid() {
                let _ = DestroyIcon(self.icon);
            }
            let _ = DestroyWindow(self.hwnd);
        }
    }
}

/// This module's instance handle.
fn instance() -> HINSTANCE {
    extern "C" {
        static __ImageBase: u8;
    }
    // Only the address is taken, never the value.
    HINSTANCE(std::ptr::addr_of!(__ImageBase) as *mut std::ffi::c_void)
}

fn balloon_thread(title: &str, body: &str, done: &mpsc::Sender<Result<(), String>>) {
    let exe = std::env::current_exe().map(|p| HSTRING::from(p.as_os_str())).unwrap_or_default();
    // SAFETY: a message-only window and icon owned by this thread; the
    // `Balloon` guard removes the icon and destroys both on every way out.
    let guard = unsafe {
        let instance = instance();
        let hwnd = match CreateWindowExW(
            WINDOW_EX_STYLE(0),
            windows::core::w!("STATIC"),
            windows::core::w!(""),
            WINDOW_STYLE(0),
            0,
            0,
            0,
            0,
            Some(HWND_MESSAGE),
            None,
            Some(instance),
            None,
        ) {
            Ok(hwnd) => hwnd,
            Err(e) => {
                let _ = done.send(Err(format!("balloon: {e}")));
                return;
            }
        };
        let mut guard = Balloon { hwnd, icon: ExtractIconW(Some(instance), PCWSTR(exe.as_ptr()), 0), added: false };
        // ExtractIconW returns 1 for "not an icon file": not an icon to free.
        if guard.icon.0 as usize == 1 {
            guard.icon = HICON::default();
        }
        let mut data = NOTIFYICONDATAW {
            cbSize: size_of::<NOTIFYICONDATAW>() as u32,
            hWnd: hwnd,
            uID: BALLOON_ID,
            uFlags: NIF_ICON | NIF_TIP | NIF_INFO,
            hIcon: guard.icon,
            dwInfoFlags: NIIF_INFO,
            ..Default::default()
        };
        fill(&mut data.szTip, TITLE);
        fill(&mut data.szInfoTitle, title);
        fill(&mut data.szInfo, body);
        if !Shell_NotifyIconW(NIM_ADD, &data).as_bool() {
            let _ = done.send(Err("balloon: could not add the icon".into()));
            return;
        }
        guard.added = true;
        guard
    };
    let _ = done.send(Ok(()));
    std::thread::sleep(BALLOON_LIFE);
    drop(guard);
}

#[cfg(test)]
#[derive(Default)]
pub struct FakeNotifier {
    shown: std::sync::Mutex<Vec<(String, String)>>,
}

#[cfg(test)]
impl FakeNotifier {
    /// Every `(title, body)` shown so far, in order.
    pub fn shown(&self) -> Vec<(String, String)> {
        self.shown.lock().unwrap().clone()
    }
}

#[cfg(test)]
impl Notifier for FakeNotifier {
    fn show(&self, title: &str, body: &str) -> Result<(), String> {
        self.shown.lock().unwrap().push((title.to_string(), body.to_string()));
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;

    #[test]
    fn toast_xml_escapes_and_uses_the_default_sound() {
        let xml = toast_xml("Flick", "a<b & \"c\" 'd'");
        assert!(xml.starts_with("<toast>"), "{xml}");
        assert!(xml.contains("<text>a&lt;b &amp; &quot;c&quot; &apos;d&apos;</text>"), "{xml}");
        assert!(xml.contains("ms-winsoundevent:Notification.Default"), "{xml}");
        assert_eq!(
            toast_xml("T", "B"),
            "<toast><visual><binding template=\"ToastGeneric\"><text>T</text><text>B</text></binding></visual>\
             <audio src=\"ms-winsoundevent:Notification.Default\"/></toast>"
        );
    }

    fn run(registered: bool, toast_result: Result<(), String>) -> (u32, u32, Result<(), String>) {
        let (toasts, balloons) = (Cell::new(0), Cell::new(0));
        let out = deliver(
            registered,
            &|| {
                toasts.set(toasts.get() + 1);
                toast_result.clone()
            },
            &|| {
                balloons.set(balloons.get() + 1);
                Ok(())
            },
        );
        (toasts.get(), balloons.get(), out)
    }

    #[test]
    fn unregistered_goes_to_the_balloon() {
        assert_eq!(run(false, Ok(())), (0, 1, Ok(())));
    }

    #[test]
    fn a_failed_toast_falls_back_to_the_balloon() {
        assert_eq!(run(true, Err("no".into())), (1, 1, Ok(())));
    }

    #[test]
    fn a_registered_toast_is_shown_once() {
        assert_eq!(run(true, Ok(())), (1, 0, Ok(())));
    }

    #[test]
    fn a_failed_balloon_is_reported() {
        let out = deliver(true, &|| Err("toast".into()), &|| Err("balloon".into()));
        assert_eq!(out, Err("balloon".into()));
    }

    #[test]
    fn balloon_text_is_cut_to_fit() {
        let mut buf = [7u16; 256];
        fill(&mut buf, &"x".repeat(400));
        assert_eq!(buf[254], 'x' as u16);
        assert_eq!(buf[255], 0);
        let mut small = [7u16; 4];
        fill(&mut small, "ab");
        assert_eq!(small[..3], ['a' as u16, 'b' as u16, 0]);
    }

    #[test]
    fn fake_notifier_records_what_was_shown() {
        let fake = FakeNotifier::default();
        fake.show(TITLE, "Timer done").unwrap();
        assert_eq!(fake.shown(), vec![("Flick".to_string(), "Timer done".to_string())]);
    }

    #[test]
    fn aumid_is_the_bundle_identifier() {
        let conf = include_str!("../tauri.conf.json");
        assert!(conf.contains("\"identifier\": \"dev.local.flick\""));
        assert!(AUMID_KEY.ends_with(AUMID));
    }

    /// The text of `!macro <name>` up to its `!macroend`.
    fn macro_body<'a>(script: &'a str, name: &str) -> &'a str {
        let start = script.find(&format!("!macro {name}")).unwrap_or_else(|| panic!("no {name}"));
        let rest = &script[start..];
        &rest[..rest.find("!macroend").expect("no !macroend")]
    }

    #[test]
    fn installer_registers_and_unregisters_the_aumid() {
        let script = include_str!("../nsis-hooks.nsh");
        let install = macro_body(script, "NSIS_HOOK_POSTINSTALL");
        assert!(install.contains(
            r#"WriteRegStr HKCU "Software\Classes\AppUserModelId\${BUNDLEID}" "DisplayName" "${PRODUCTNAME}""#
        ));
        let uninstall = macro_body(script, "NSIS_HOOK_POSTUNINSTALL");
        let guard = uninstall.find("${If} $UpdateMode <> 1").expect("no update guard");
        let guarded = &uninstall[guard..uninstall.find("${EndIf}").expect("no ${EndIf}")];
        assert!(guarded.contains(r#"DeleteRegKey HKCU "Software\Classes\AppUserModelId\${BUNDLEID}""#));
        // The existing start-at-login removal stays.
        assert!(guarded.contains(r#"DeleteRegValue HKCU "Software\Microsoft\Windows\CurrentVersion\Run" "Flick""#));
    }

    /// Read-only: prints whether the AUMID is registered and Windows' setting
    /// for its toasts. Shows nothing. Run by hand:
    /// `cargo test --lib real_registration_reads -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn real_registration_reads() {
        println!("registered: {}", registered());
        let setting = with_com(|| {
            ToastNotificationManager::CreateToastNotifierWithId(&HSTRING::from(AUMID)).and_then(|n| n.Setting())
        });
        match setting {
            Ok(s) => println!("setting: {}", setting_name(s)),
            Err(e) => println!("setting error: {:?}", e.code()),
        }
    }

    fn setting_name(s: windows::UI::Notifications::NotificationSetting) -> &'static str {
        use windows::UI::Notifications::NotificationSetting as N;
        match s {
            N::Enabled => "Enabled",
            N::DisabledForApplication => "DisabledForApplication",
            N::DisabledForUser => "DisabledForUser",
            N::DisabledByGroupPolicy => "DisabledByGroupPolicy",
            N::DisabledByManifest => "DisabledByManifest",
            _ => "Other",
        }
    }
}
