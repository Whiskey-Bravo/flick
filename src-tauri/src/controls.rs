//! System controls behind a trait: audio, media keys, dark mode and radios.
//! The real implementation talks to Windows; tests use fakes and never
//! construct `SystemControls`.

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum MediaKey {
    Next,
    Previous,
    PlayPause,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum RadioKind {
    WiFi,
    Bluetooth,
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Audio {
    pub volume: u8,
    pub muted: bool,
}

pub trait Controls: Send + Sync {
    fn audio(&self) -> Result<Audio, String>;
    fn set_volume(&self, pct: u8) -> Result<(), String>;
    fn set_muted(&self, on: bool) -> Result<(), String>;
    fn media(&self, key: MediaKey) -> Result<(), String>;
    fn set_dark(&self, dark: bool) -> Result<(), String>;
    fn set_radio(&self, kind: RadioKind, on: bool) -> Result<(), String>;
}

/// The real thing. Never use in tests.
pub struct SystemControls;

mod real {
    use windows::core::{w, PCWSTR};
    use windows::Devices::Radios::{Radio, RadioAccessStatus, RadioState};
    use windows::Win32::Foundation::{LPARAM, WPARAM};
    use windows::Win32::Media::Audio::Endpoints::IAudioEndpointVolume;
    use windows::Win32::Media::Audio::{eConsole, eRender, IMMDeviceEnumerator, MMDeviceEnumerator};
    use windows::Win32::System::Com::{
        CoCreateInstance, CoInitializeEx, CoUninitialize, CLSCTX_ALL, COINIT_MULTITHREADED,
    };
    use windows::Win32::System::Registry::{RegSetKeyValueW, HKEY_CURRENT_USER, REG_DWORD};
    use windows::Win32::UI::Input::KeyboardAndMouse::{
        SendInput, INPUT, INPUT_0, INPUT_KEYBOARD, KEYBDINPUT, KEYEVENTF_KEYUP, VIRTUAL_KEY,
        VK_MEDIA_NEXT_TRACK, VK_MEDIA_PLAY_PAUSE, VK_MEDIA_PREV_TRACK,
    };
    use windows::Win32::UI::WindowsAndMessaging::{
        SendMessageTimeoutW, HWND_BROADCAST, SMTO_ABORTIFHUNG, WM_SETTINGCHANGE,
    };

    use super::{Audio, Controls, MediaKey, RadioKind, SystemControls};

    /// COM on the calling thread for the life of the guard.
    struct Com(bool);

    impl Com {
        fn init() -> Com {
            // S_FALSE (already initialised) still needs its CoUninitialize;
            // a different apartment mode (error) does not.
            // SAFETY: no pointers; balanced by Drop.
            Com(unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) }.is_ok())
        }
    }

    impl Drop for Com {
        fn drop(&mut self) {
            if self.0 {
                // SAFETY: pairs a successful CoInitializeEx on this thread.
                unsafe { CoUninitialize() };
            }
        }
    }

    fn endpoint() -> Result<IAudioEndpointVolume, String> {
        // SAFETY: plain COM calls with valid arguments on an initialised thread.
        unsafe {
            let enumerator: IMMDeviceEnumerator = CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL)
                .map_err(|_| "no sound device".to_string())?;
            let device = enumerator
                .GetDefaultAudioEndpoint(eRender, eConsole)
                .map_err(|_| "no sound device".to_string())?;
            device
                .Activate::<IAudioEndpointVolume>(CLSCTX_ALL, None)
                .map_err(|_| "can't reach the sound device".to_string())
        }
    }

    fn key_input(vk: VIRTUAL_KEY, up: bool) -> INPUT {
        INPUT {
            r#type: INPUT_KEYBOARD,
            Anonymous: INPUT_0 {
                ki: KEYBDINPUT {
                    wVk: vk,
                    dwFlags: if up { KEYEVENTF_KEYUP } else { Default::default() },
                    ..Default::default()
                },
            },
        }
    }

    fn set_dword(name: PCWSTR, value: u32) -> Result<(), String> {
        // SAFETY: `value` outlives the call and the size is that of a u32.
        let rc = unsafe {
            RegSetKeyValueW(
                HKEY_CURRENT_USER,
                w!(r"Software\Microsoft\Windows\CurrentVersion\Themes\Personalize"),
                name,
                REG_DWORD.0,
                Some(&value as *const u32 as *const _),
                std::mem::size_of::<u32>() as u32,
            )
        };
        if rc.is_ok() {
            Ok(())
        } else {
            Err("couldn't write the theme setting".to_string())
        }
    }

    fn radio_error(e: windows::core::Error) -> String {
        let m = e.message();
        if m.is_empty() {
            "the radio didn't answer".to_string()
        } else {
            m.trim().to_string()
        }
    }

    impl Controls for SystemControls {
        fn audio(&self) -> Result<Audio, String> {
            let _com = Com::init();
            let ep = endpoint()?;
            // SAFETY: the interface is valid for these calls.
            unsafe {
                let level = ep.GetMasterVolumeLevelScalar().map_err(|_| "can't read the volume".to_string())?;
                let muted = ep.GetMute().map_err(|_| "can't read the volume".to_string())?;
                Ok(Audio { volume: (level.clamp(0.0, 1.0) * 100.0).round() as u8, muted: muted.as_bool() })
            }
        }

        fn set_volume(&self, pct: u8) -> Result<(), String> {
            let _com = Com::init();
            let ep = endpoint()?;
            // SAFETY: a null event context is allowed.
            unsafe { ep.SetMasterVolumeLevelScalar(f32::from(pct.min(100)) / 100.0, std::ptr::null()) }
                .map_err(|_| "can't set the volume".to_string())
        }

        fn set_muted(&self, on: bool) -> Result<(), String> {
            let _com = Com::init();
            let ep = endpoint()?;
            // SAFETY: a null event context is allowed.
            unsafe { ep.SetMute(on, std::ptr::null()) }.map_err(|_| "can't change mute".to_string())
        }

        fn media(&self, key: MediaKey) -> Result<(), String> {
            let vk = match key {
                MediaKey::Next => VK_MEDIA_NEXT_TRACK,
                MediaKey::Previous => VK_MEDIA_PREV_TRACK,
                MediaKey::PlayPause => VK_MEDIA_PLAY_PAUSE,
            };
            let inputs = [key_input(vk, false), key_input(vk, true)];
            // SAFETY: a valid slice of fully initialised INPUTs.
            let sent = unsafe { SendInput(&inputs, std::mem::size_of::<INPUT>() as i32) };
            if sent == inputs.len() as u32 {
                Ok(())
            } else {
                Err("couldn't send the media key".to_string())
            }
        }

        fn set_dark(&self, dark: bool) -> Result<(), String> {
            let light = u32::from(!dark);
            set_dword(w!("AppsUseLightTheme"), light)?;
            set_dword(w!("SystemUsesLightTheme"), light)?;
            // SAFETY: the parameter is a NUL-terminated string that outlives the call.
            unsafe {
                SendMessageTimeoutW(
                    HWND_BROADCAST,
                    WM_SETTINGCHANGE,
                    WPARAM(0),
                    LPARAM(w!("ImmersiveColorSet").as_ptr() as isize),
                    SMTO_ABORTIFHUNG,
                    200,
                    None,
                );
            }
            Ok(())
        }

        fn set_radio(&self, kind: RadioKind, on: bool) -> Result<(), String> {
            let _com = Com::init();
            let want = match kind {
                RadioKind::WiFi => windows::Devices::Radios::RadioKind::WiFi,
                RadioKind::Bluetooth => windows::Devices::Radios::RadioKind::Bluetooth,
            };
            let access = Radio::RequestAccessAsync().map_err(radio_error)?.get().map_err(radio_error)?;
            if access != RadioAccessStatus::Allowed {
                return Err("Windows didn't allow access to the radio".to_string());
            }
            let radios = Radio::GetRadiosAsync().map_err(radio_error)?.get().map_err(radio_error)?;
            let radio = radios
                .into_iter()
                .find(|r| r.Kind().map(|k| k == want).unwrap_or(false))
                .ok_or_else(|| "no radio found".to_string())?;
            let state = if on { RadioState::On } else { RadioState::Off };
            let result = radio.SetStateAsync(state).map_err(radio_error)?.get().map_err(radio_error)?;
            if result == RadioAccessStatus::Allowed {
                Ok(())
            } else {
                Err("Windows didn't allow changing the radio".to_string())
            }
        }
    }
}
