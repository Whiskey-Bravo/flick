//! System chores: the Recycle Bin, removable drives, the DNS cache and
//! Explorer. Everything Windows-facing sits behind [`Chores`] so tests use
//! [`testing::FakeChores`] and never touch the real thing.

use std::path::PathBuf;
use std::time::Duration;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BinInfo {
    pub items: u64,
    pub bytes: u64,
}

/// A drive that can be ejected safely.
#[derive(Clone, Debug, PartialEq)]
pub struct Drive {
    pub letter: char,
    /// The volume label; empty when it has none.
    pub label: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EjectError {
    /// Windows refused: something has a file open on it.
    InUse,
    Failed,
}

pub trait Chores: Send + Sync {
    fn bin(&self) -> Result<BinInfo, String>;
    /// Empties every drive's Recycle Bin with no dialog, progress or sound.
    fn empty_bin(&self) -> Result<(), String>;
    /// Removable drives and USB disks that have a volume.
    fn drives(&self) -> Result<Vec<Drive>, String>;
    fn eject(&self, letter: char) -> Result<(), EjectError>;
    fn flush_dns(&self) -> Result<(), String>;
    fn restart_explorer(&self) -> Result<(), String>;
}

/// The real thing. Never constructed in tests.
pub struct SystemChores;

const DRIVE_REMOVABLE: u32 = 2;
const DRIVE_FIXED: u32 = 3;
const FLUSH_TIMEOUT: Duration = Duration::from_secs(10);

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

fn windows_dir() -> PathBuf {
    std::env::var_os("WINDIR")
        .or_else(|| std::env::var_os("SystemRoot"))
        .map(PathBuf::from)
        .unwrap_or_else(|| r"C:\Windows".into())
}

mod win {
    use std::mem::size_of;
    use std::time::{Duration, Instant};

    use windows::core::{PCWSTR, PWSTR};
    use windows::Win32::Devices::DeviceAndDriverInstallation::{
        CM_Get_Parent, CM_Request_Device_EjectW, SetupDiDestroyDeviceInfoList, SetupDiEnumDeviceInterfaces,
        SetupDiGetClassDevsW, SetupDiGetDeviceInterfaceDetailW, CR_REMOVE_VETOED, CR_SUCCESS, DIGCF_DEVICEINTERFACE,
        DIGCF_PRESENT, HDEVINFO, PNP_VetoTypeUnknown, SP_DEVICE_INTERFACE_DATA, SP_DEVICE_INTERFACE_DETAIL_DATA_W,
        SP_DEVINFO_DATA,
    };
    use windows::Win32::Foundation::{CloseHandle, ERROR_SUCCESS, HANDLE, WIN32_ERROR};
    use windows::Win32::Storage::FileSystem::{
        BusTypeMmc, BusTypeSd, BusTypeUsb, CreateFileW, GetDriveTypeW, GetLogicalDrives, GetVolumeInformationW,
        FILE_FLAGS_AND_ATTRIBUTES, FILE_SHARE_READ, FILE_SHARE_WRITE, OPEN_EXISTING, STORAGE_BUS_TYPE,
    };
    use windows::Win32::System::Diagnostics::ToolHelp::{
        CreateToolhelp32Snapshot, Process32FirstW, Process32NextW, PROCESSENTRY32W, TH32CS_SNAPPROCESS,
    };
    use windows::Win32::System::Ioctl::{
        PropertyStandardQuery, StorageDeviceProperty, GUID_DEVINTERFACE_DISK, IOCTL_STORAGE_GET_DEVICE_NUMBER,
        IOCTL_STORAGE_QUERY_PROPERTY, STORAGE_DEVICE_DESCRIPTOR, STORAGE_DEVICE_NUMBER, STORAGE_PROPERTY_QUERY,
    };
    use windows::Win32::System::IO::DeviceIoControl;
    use windows::Win32::System::RestartManager::{
        RmEndSession, RmForceShutdown, RmRegisterResources, RmRestart, RmShutdown, RmStartSession, CCH_RM_SESSION_KEY,
    };
    use windows::Win32::UI::Shell::{
        SHEmptyRecycleBinW, SHQueryRecycleBinW, SHERB_NOCONFIRMATION, SHERB_NOPROGRESSUI, SHERB_NOSOUND, SHQUERYRBINFO,
    };

    use super::{wide, BinInfo, Drive, EjectError, DRIVE_FIXED, DRIVE_REMOVABLE};

    struct Handle(HANDLE);

    impl Drop for Handle {
        fn drop(&mut self) {
            // SAFETY: the handle came from CreateFileW and is closed once.
            let _ = unsafe { CloseHandle(self.0) };
        }
    }

    struct DevInfo(HDEVINFO);

    impl Drop for DevInfo {
        fn drop(&mut self) {
            // SAFETY: the set came from SetupDiGetClassDevsW and is freed once.
            let _ = unsafe { SetupDiDestroyDeviceInfoList(self.0) };
        }
    }

    struct RmSession(u32);

    impl Drop for RmSession {
        fn drop(&mut self) {
            // SAFETY: the session came from RmStartSession and is ended once.
            let _ = unsafe { RmEndSession(self.0) };
        }
    }

    /// A handle for device queries only (no read or write access).
    fn open(path: PCWSTR) -> Option<Handle> {
        // SAFETY: `path` is a NUL-terminated string that outlives the call.
        let h = unsafe {
            CreateFileW(
                path,
                0,
                FILE_SHARE_READ | FILE_SHARE_WRITE,
                None,
                OPEN_EXISTING,
                FILE_FLAGS_AND_ATTRIBUTES(0),
                None,
            )
        };
        h.ok().map(Handle)
    }

    fn volume_path(letter: char) -> Vec<u16> {
        wide(&format!(r"\\.\{letter}:"))
    }

    fn device_number(h: &Handle) -> Option<STORAGE_DEVICE_NUMBER> {
        let mut out = STORAGE_DEVICE_NUMBER::default();
        let mut got = 0u32;
        // SAFETY: `out` is a writable STORAGE_DEVICE_NUMBER of the size passed.
        unsafe {
            DeviceIoControl(
                h.0,
                IOCTL_STORAGE_GET_DEVICE_NUMBER,
                None,
                0,
                Some(&mut out as *mut _ as *mut _),
                size_of::<STORAGE_DEVICE_NUMBER>() as u32,
                Some(&mut got),
                None,
            )
        }
        .ok()?;
        Some(out)
    }

    /// The bus the volume's disk sits on, if Windows says.
    fn bus_type(letter: char) -> Option<STORAGE_BUS_TYPE> {
        let path = volume_path(letter);
        let h = open(PCWSTR(path.as_ptr()))?;
        let query = STORAGE_PROPERTY_QUERY {
            PropertyId: StorageDeviceProperty,
            QueryType: PropertyStandardQuery,
            ..Default::default()
        };
        // u64s keep the buffer aligned for the descriptor read below.
        let mut buf = [0u64; 128];
        let mut got = 0u32;
        // SAFETY: `query` and `buf` are valid for the sizes passed.
        let ok = unsafe {
            DeviceIoControl(
                h.0,
                IOCTL_STORAGE_QUERY_PROPERTY,
                Some(&query as *const _ as *const _),
                size_of::<STORAGE_PROPERTY_QUERY>() as u32,
                Some(buf.as_mut_ptr() as *mut _),
                size_of_val(&buf) as u32,
                Some(&mut got),
                None,
            )
        }
        .is_ok();
        if !ok || (got as usize) < size_of::<STORAGE_DEVICE_DESCRIPTOR>() {
            return None;
        }
        // SAFETY: the buffer is aligned, large enough and was filled above.
        let desc = unsafe { std::ptr::read(buf.as_ptr() as *const STORAGE_DEVICE_DESCRIPTOR) };
        Some(desc.BusType)
    }

    /// Kept from `SetThreadErrorMode` until dropped, so reading an empty
    /// card slot can't show Windows' "No disk" dialog.
    struct QuietErrors(u32);

    // Declared by hand: the windows crate only has it under a feature this
    // build does not enable (Win32_System_Diagnostics_Debug).
    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn SetThreadErrorMode(new_mode: u32, old_mode: *mut u32) -> i32;
    }

    const SEM_FAILCRITICALERRORS: u32 = 1;

    impl QuietErrors {
        fn new() -> Self {
            let mut old = 0u32;
            // SAFETY: `old` is writable; this thread's mode only.
            unsafe { SetThreadErrorMode(SEM_FAILCRITICALERRORS, &mut old) };
            Self(old)
        }
    }

    impl Drop for QuietErrors {
        fn drop(&mut self) {
            // SAFETY: restores the mode read in `new`.
            unsafe { SetThreadErrorMode(self.0, std::ptr::null_mut()) };
        }
    }

    fn explorer_running() -> bool {
        let Ok(snapshot) = (unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) }) else {
            // Unknown: assume it runs rather than start a second shell.
            return true;
        };
        let snapshot = Handle(snapshot);
        let mut entry = PROCESSENTRY32W { dwSize: size_of::<PROCESSENTRY32W>() as u32, ..Default::default() };
        // SAFETY: a valid snapshot; `entry` is sized above.
        let mut more = unsafe { Process32FirstW(snapshot.0, &mut entry) }.is_ok();
        while more {
            let len = entry.szExeFile.iter().position(|&c| c == 0).unwrap_or(entry.szExeFile.len());
            if String::from_utf16_lossy(&entry.szExeFile[..len]).eq_ignore_ascii_case("explorer.exe") {
                return true;
            }
            // SAFETY: as above.
            more = unsafe { Process32NextW(snapshot.0, &mut entry) }.is_ok();
        }
        false
    }

    /// The volume's label, or `None` when it has no readable volume (an
    /// empty card slot).
    fn label(root: &[u16]) -> Option<String> {
        let mut name = [0u16; 261];
        // SAFETY: `root` is NUL-terminated; `name` is writable.
        unsafe { GetVolumeInformationW(PCWSTR(root.as_ptr()), Some(&mut name), None, None, None, None) }.ok()?;
        let len = name.iter().position(|&c| c == 0).unwrap_or(name.len());
        Some(String::from_utf16_lossy(&name[..len]).trim().to_string())
    }

    pub fn bin() -> Result<BinInfo, String> {
        let mut info = SHQUERYRBINFO { cbSize: size_of::<SHQUERYRBINFO>() as u32, ..Default::default() };
        // SAFETY: a null root means every drive; `info` is sized above.
        unsafe { SHQueryRecycleBinW(PCWSTR::null(), &mut info) }.map_err(|e| e.message())?;
        Ok(BinInfo { items: info.i64NumItems.max(0) as u64, bytes: info.i64Size.max(0) as u64 })
    }

    pub fn empty_bin() -> Result<(), String> {
        // SAFETY: no window, a null root (every drive), plain flags.
        let r = unsafe {
            SHEmptyRecycleBinW(None, PCWSTR::null(), SHERB_NOCONFIRMATION | SHERB_NOPROGRESSUI | SHERB_NOSOUND)
        };
        match r {
            Ok(()) => Ok(()),
            // Windows reports an error for a bin that was already empty.
            Err(_) if bin().is_ok_and(|b| b.items == 0) => Ok(()),
            Err(e) => Err(e.message()),
        }
    }

    pub fn drives() -> Result<Vec<Drive>, String> {
        let _quiet = QuietErrors::new();
        // SAFETY: no arguments.
        let mask = unsafe { GetLogicalDrives() };
        let mut out = Vec::new();
        for i in 0..26u8 {
            if mask & (1 << i) == 0 {
                continue;
            }
            let letter = (b'A' + i) as char;
            let root = wide(&format!(r"{letter}:\"));
            // SAFETY: `root` is NUL-terminated.
            let kind = unsafe { GetDriveTypeW(PCWSTR(root.as_ptr())) };
            // Removable volumes on USB (sticks; a built-in USB card reader
            // can't be told apart and is kept) or SD/MMC; fixed disks on USB.
            let bus = bus_type(letter);
            let wanted = match kind {
                DRIVE_REMOVABLE => bus.is_some_and(|b| b == BusTypeUsb || b == BusTypeSd || b == BusTypeMmc),
                DRIVE_FIXED => bus == Some(BusTypeUsb),
                _ => false,
            };
            if !wanted {
                continue;
            }
            if let Some(label) = label(&root) {
                out.push(Drive { letter, label });
            }
        }
        Ok(out)
    }

    /// The devnode of the disk whose device number matches the volume's.
    fn disk_devnode(number: &STORAGE_DEVICE_NUMBER) -> Option<u32> {
        // SAFETY: a valid interface GUID; no enumerator or window.
        let set = unsafe {
            SetupDiGetClassDevsW(Some(&GUID_DEVINTERFACE_DISK), PCWSTR::null(), None, DIGCF_PRESENT | DIGCF_DEVICEINTERFACE)
        }
        .ok()?;
        let set = DevInfo(set);
        for index in 0.. {
            let mut iface = SP_DEVICE_INTERFACE_DATA { cbSize: size_of::<SP_DEVICE_INTERFACE_DATA>() as u32, ..Default::default() };
            // SAFETY: `set` is valid; `iface` is sized above.
            if unsafe { SetupDiEnumDeviceInterfaces(set.0, None, &GUID_DEVINTERFACE_DISK, index, &mut iface) }.is_err() {
                return None;
            }
            let mut needed = 0u32;
            // SAFETY: a size query: no buffer, only `needed` is written.
            let _ = unsafe { SetupDiGetDeviceInterfaceDetailW(set.0, &iface, None, 0, Some(&mut needed), None) };
            if (needed as usize) < size_of::<SP_DEVICE_INTERFACE_DETAIL_DATA_W>() {
                continue;
            }
            // u64s keep the detail struct aligned.
            let mut buf = vec![0u64; (needed as usize).div_ceil(8)];
            let detail = buf.as_mut_ptr() as *mut SP_DEVICE_INTERFACE_DETAIL_DATA_W;
            let mut dev = SP_DEVINFO_DATA { cbSize: size_of::<SP_DEVINFO_DATA>() as u32, ..Default::default() };
            // SAFETY: `detail` points at `needed` writable, aligned bytes whose
            // cbSize is set to the fixed part's size, as the API requires.
            let ok = unsafe {
                (*detail).cbSize = size_of::<SP_DEVICE_INTERFACE_DETAIL_DATA_W>() as u32;
                SetupDiGetDeviceInterfaceDetailW(set.0, &iface, Some(detail), needed, None, Some(&mut dev))
            }
            .is_ok();
            if !ok {
                continue;
            }
            // SAFETY: DevicePath is a NUL-terminated string inside `buf`.
            let path = unsafe { PCWSTR(std::ptr::addr_of!((*detail).DevicePath) as *const u16) };
            let Some(h) = open(path) else { continue };
            if device_number(&h).is_some_and(|n| n.DeviceType == number.DeviceType && n.DeviceNumber == number.DeviceNumber) {
                return Some(dev.DevInst);
            }
        }
        None
    }

    pub fn eject(letter: char) -> Result<(), EjectError> {
        let path = volume_path(letter);
        let number = {
            let h = open(PCWSTR(path.as_ptr())).ok_or(EjectError::Failed)?;
            device_number(&h).ok_or(EjectError::Failed)?
        };
        let disk = disk_devnode(&number).ok_or(EjectError::Failed)?;
        let mut parent = 0u32;
        // SAFETY: `parent` is writable.
        if unsafe { CM_Get_Parent(&mut parent, disk, 0) } != CR_SUCCESS {
            return Err(EjectError::Failed);
        }
        let mut veto = PNP_VetoTypeUnknown;
        // A veto name buffer keeps Windows from showing its own dialog.
        let mut veto_name = [0u16; 260];
        // SAFETY: `veto` and `veto_name` are writable.
        let r = unsafe { CM_Request_Device_EjectW(parent, Some(&mut veto), Some(&mut veto_name), 0) };
        if r == CR_SUCCESS && veto == PNP_VetoTypeUnknown {
            Ok(())
        } else if r == CR_REMOVE_VETOED || veto != PNP_VetoTypeUnknown {
            Err(EjectError::InUse)
        } else {
            Err(EjectError::Failed)
        }
    }

    fn check(r: WIN32_ERROR, what: &str) -> Result<(), String> {
        if r == ERROR_SUCCESS {
            Ok(())
        } else {
            Err(format!("{what} failed: {}", r.0))
        }
    }

    pub fn restart_explorer(explorer: &str) -> Result<(), String> {
        let mut handle = 0u32;
        let mut key = [0u16; CCH_RM_SESSION_KEY as usize + 1];
        // SAFETY: `handle` and `key` are writable; the key holds CCH_RM_SESSION_KEY + 1 chars.
        check(unsafe { RmStartSession(&mut handle, None, PWSTR(key.as_mut_ptr())) }, "RmStartSession")?;
        let session = RmSession(handle);
        let exe = wide(explorer);
        let files = [PCWSTR(exe.as_ptr())];
        // SAFETY: `files` holds one NUL-terminated path that outlives the call.
        check(unsafe { RmRegisterResources(session.0, Some(&files), None, None) }, "RmRegisterResources")?;
        // Once a shutdown has been tried, Explorer may be gone, so from here
        // on nothing returns early. Order: 1) RmRestart whatever RmShutdown
        // said; 2) if no explorer.exe runs, start it ourselves; 3) Err only
        // when Explorer still isn't running.
        // SAFETY: a started session; no callback.
        let shutdown = check(unsafe { RmShutdown(session.0, RmForceShutdown.0 as u32, None) }, "RmShutdown");
        // SAFETY: a started session; no callback.
        let restart = check(unsafe { RmRestart(session.0, None, None) }, "RmRestart");
        drop(session);
        if wait_for_explorer(Duration::from_secs(2)) {
            return Ok(());
        }
        let _ = std::process::Command::new(explorer).spawn();
        if wait_for_explorer(Duration::from_secs(5)) {
            Ok(())
        } else {
            Err(shutdown.and(restart).err().unwrap_or_else(|| "Explorer did not come back".to_string()))
        }
    }

    /// Polls until an explorer.exe process exists or `limit` passes.
    fn wait_for_explorer(limit: Duration) -> bool {
        let deadline = Instant::now() + limit;
        loop {
            if explorer_running() {
                return true;
            }
            if Instant::now() >= deadline {
                return false;
            }
            std::thread::sleep(Duration::from_millis(100));
        }
    }
}

impl Chores for SystemChores {
    fn bin(&self) -> Result<BinInfo, String> {
        win::bin()
    }

    fn empty_bin(&self) -> Result<(), String> {
        win::empty_bin()
    }

    fn drives(&self) -> Result<Vec<Drive>, String> {
        win::drives()
    }

    fn eject(&self, letter: char) -> Result<(), EjectError> {
        win::eject(letter.to_ascii_uppercase())
    }

    fn flush_dns(&self) -> Result<(), String> {
        let exe = windows_dir().join(r"System32\ipconfig.exe");
        crate::process::run_hidden(&exe, &["/flushdns"], FLUSH_TIMEOUT)
            .map(|_| ())
            .map_err(|e| format!("ipconfig: {e:?}"))
    }

    fn restart_explorer(&self) -> Result<(), String> {
        let exe = windows_dir().join("explorer.exe");
        win::restart_explorer(&exe.to_string_lossy())
    }
}

#[cfg(test)]
pub mod testing {
    //! A scripted [`Chores`] that records every call.

    use std::sync::{Condvar, Mutex};

    use super::{BinInfo, Chores, Drive, EjectError};

    pub struct FakeChores {
        bin: Mutex<Result<BinInfo, String>>,
        empty: Mutex<Result<(), String>>,
        drives: Mutex<Result<Vec<Drive>, String>>,
        eject: Mutex<Result<(), EjectError>>,
        flush: Mutex<Result<(), String>>,
        restart: Mutex<Result<(), String>>,
        calls: Mutex<Vec<String>>,
        blocked: Mutex<bool>,
        unblocked: Condvar,
    }

    impl Default for FakeChores {
        fn default() -> Self {
            Self {
                bin: Mutex::new(Ok(BinInfo { items: 0, bytes: 0 })),
                empty: Mutex::new(Ok(())),
                drives: Mutex::new(Ok(Vec::new())),
                eject: Mutex::new(Ok(())),
                flush: Mutex::new(Ok(())),
                restart: Mutex::new(Ok(())),
                calls: Mutex::new(Vec::new()),
                blocked: Mutex::new(false),
                unblocked: Condvar::new(),
            }
        }
    }

    impl FakeChores {
        pub fn set_bin(&self, r: Result<BinInfo, String>) {
            *self.bin.lock().unwrap() = r;
        }
        pub fn set_empty(&self, r: Result<(), String>) {
            *self.empty.lock().unwrap() = r;
        }
        pub fn set_drives(&self, r: Result<Vec<Drive>, String>) {
            *self.drives.lock().unwrap() = r;
        }
        pub fn set_eject(&self, r: Result<(), EjectError>) {
            *self.eject.lock().unwrap() = r;
        }
        pub fn set_flush(&self, r: Result<(), String>) {
            *self.flush.lock().unwrap() = r;
        }
        pub fn set_restart(&self, r: Result<(), String>) {
            *self.restart.lock().unwrap() = r;
        }
        /// Every call so far, in order: `bin`, `empty_bin`, `drives`,
        /// `eject:E`, `flush_dns`, `restart_explorer`.
        pub fn calls(&self) -> Vec<String> {
            self.calls.lock().unwrap().clone()
        }
        /// `bin` and `drives` wait until [`FakeChores::release`].
        pub fn block(&self) {
            *self.blocked.lock().unwrap() = true;
        }
        pub fn release(&self) {
            *self.blocked.lock().unwrap() = false;
            self.unblocked.notify_all();
        }
        fn record(&self, call: String) {
            self.calls.lock().unwrap().push(call);
        }
        fn wait(&self) {
            let mut blocked = self.blocked.lock().unwrap();
            while *blocked {
                blocked = self.unblocked.wait(blocked).unwrap();
            }
        }
    }

    impl Chores for FakeChores {
        fn bin(&self) -> Result<BinInfo, String> {
            self.record("bin".into());
            self.wait();
            self.bin.lock().unwrap().clone()
        }
        fn empty_bin(&self) -> Result<(), String> {
            self.record("empty_bin".into());
            self.empty.lock().unwrap().clone()
        }
        fn drives(&self) -> Result<Vec<Drive>, String> {
            self.record("drives".into());
            self.wait();
            self.drives.lock().unwrap().clone()
        }
        fn eject(&self, letter: char) -> Result<(), EjectError> {
            self.record(format!("eject:{letter}"));
            *self.eject.lock().unwrap()
        }
        fn flush_dns(&self) -> Result<(), String> {
            self.record("flush_dns".into());
            self.flush.lock().unwrap().clone()
        }
        fn restart_explorer(&self) -> Result<(), String> {
            self.record("restart_explorer".into());
            self.restart.lock().unwrap().clone()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Reads only: prints the Recycle Bin's item count and how many
    /// removable drives there are. Empties, ejects and flushes nothing.
    #[test]
    #[ignore]
    fn real_chores_read() {
        let chores = SystemChores;
        match chores.bin() {
            Ok(b) => println!("recycle bin items: {}", b.items),
            Err(_) => println!("recycle bin: could not read"),
        }
        match chores.drives() {
            Ok(d) => println!("removable drives: {}", d.len()),
            Err(_) => println!("removable drives: could not read"),
        }
    }
}
