//! Paired Bluetooth audio devices and saved Wi-Fi networks behind traits.
//! The real implementations talk to Windows (the audio endpoints' kernel
//! streaming filters and the WLAN API); tests use the fakes in [`testing`]
//! and never construct `KsBtAudio` or `WlanWifi`.

#[derive(Clone, Debug, PartialEq)]
pub struct BtDevice {
    /// The KS filter's device id (the first one, when a device has several).
    pub id: String,
    pub name: String,
    pub connected: bool,
}

pub trait BtAudio: Send + Sync {
    fn list(&self) -> Result<Vec<BtDevice>, String>;
    fn connect(&self, id: &str) -> Result<(), String>;
    fn disconnect(&self, id: &str) -> Result<(), String>;
}

#[derive(Clone, Debug, PartialEq)]
pub struct WifiProfile {
    pub name: String,
    pub connected: bool,
}

pub trait Wifi: Send + Sync {
    fn profiles(&self) -> Result<Vec<WifiProfile>, String>;
    fn connect(&self, name: &str) -> Result<(), String>;
}

/// One audio endpoint as read: the KS filter it hangs off and what it is called.
#[derive(Clone, Debug, PartialEq)]
struct Seen {
    filter: String,
    /// The filter device's own name (what Bluetooth settings shows).
    filter_name: Option<String>,
    /// The endpoint's name, `Headphones (…)`: only a fallback.
    endpoint_name: Option<String>,
    active: bool,
}

/// Whether a KS filter id belongs to a Bluetooth audio device.
fn is_bluetooth(filter: &str) -> bool {
    let id = filter.to_ascii_uppercase();
    id.contains("BTHENUM") || id.contains("BTHHFENUM")
}

/// The endpoints' Bluetooth filters as devices, each with every filter id it
/// owns. Endpoints sharing a filter (a headset's stereo and hands-free ones)
/// make one device; filters whose own device name (never a fallback name) is
/// the same are one device too, so a headset never shows twice. Connected
/// when any endpoint is active.
fn group(seen: &[Seen]) -> Vec<(BtDevice, Vec<String>)> {
    // Each device with its filters and whether its name is the filter's own.
    let mut out: Vec<(BtDevice, Vec<String>, bool)> = Vec::new();
    for s in seen.iter().filter(|s| is_bluetooth(&s.filter)) {
        if let Some((device, _, _)) = out.iter_mut().find(|(_, filters, _)| filters.contains(&s.filter)) {
            device.connected |= s.active;
            continue;
        }
        // Only a name read from the filter device itself joins filters: a
        // fallback name says nothing about which device it is.
        let own_name = s.filter_name.clone().filter(|n| !n.trim().is_empty());
        if let Some(own) = &own_name {
            if let Some((device, filters, _)) = out.iter_mut().find(|(d, _, named)| *named && d.name == *own) {
                device.connected |= s.active;
                filters.push(s.filter.clone());
                continue;
            }
        }
        let named = own_name.is_some();
        let name = own_name
            .or_else(|| s.endpoint_name.clone().filter(|n| !n.trim().is_empty()))
            .unwrap_or_else(|| "Bluetooth audio device".to_string());
        out.push((BtDevice { id: s.filter.clone(), name, connected: s.active }, vec![s.filter.clone()], named));
    }
    out.into_iter().map(|(d, f, _)| (d, f)).collect()
}

/// The filter ids of the device `id` names (its own id or any of its filters).
fn filters_of(groups: &[(BtDevice, Vec<String>)], id: &str) -> Option<Vec<String>> {
    groups.iter().find(|(d, f)| d.id == id || f.iter().any(|x| x == id)).map(|(_, f)| f.clone())
}

/// `primary`, or `fallback` when it fails; both errors when both do.
fn first_working<T>(
    primary: impl FnOnce() -> Result<T, String>,
    fallback: impl FnOnce() -> Result<T, String>,
) -> Result<T, String> {
    primary().or_else(|first| fallback().map_err(|second| format!("{first}; {second}")))
}

/// The real thing, through the audio endpoints' KS filters: the one-shot
/// reconnect/disconnect that Windows' own Bluetooth settings use. Never use
/// in tests.
pub struct KsBtAudio;

impl BtAudio for KsBtAudio {
    fn list(&self) -> Result<Vec<BtDevice>, String> {
        crate::launch::with_com(|| {
            let (seen, _, _) = ks::read()?;
            Ok(group(&seen).into_iter().map(|(d, _)| d).collect())
        })
    }

    fn connect(&self, id: &str) -> Result<(), String> {
        crate::launch::with_com(|| ks::oneshot(id, ks::ONESHOT_RECONNECT))
    }

    fn disconnect(&self, id: &str) -> Result<(), String> {
        crate::launch::with_com(|| ks::oneshot(id, ks::ONESHOT_DISCONNECT))
    }
}

mod ks {
    use super::{filters_of, first_working, group, Seen};
    use std::collections::HashMap;
    use std::ffi::c_void;
    use windows::core::{Interface, GUID, PCWSTR, PWSTR};
    use windows::Win32::Devices::FunctionDiscovery::PKEY_Device_FriendlyName;
    use windows::Win32::Media::Audio::{
        eAll, IDeviceTopology, IMMDevice, IMMDeviceEnumerator, IPart, MMDeviceEnumerator, DEVICE_STATE, DEVICE_STATE_ACTIVE,
        DEVICE_STATE_UNPLUGGED,
    };
    use windows::Win32::Media::KernelStreaming::{
        IKsControl, KSIDENTIFIER, KSIDENTIFIER_0, KSIDENTIFIER_0_0, KSPROPERTY_TYPE_GET,
    };
    use windows::Win32::System::Com::{CoCreateInstance, CoTaskMemFree, CLSCTX_ALL, CLSCTX_INPROC_SERVER, STGM_READ};
    use windows::Win32::System::Com::StructuredStorage::PropVariantToStringAlloc;

    /// `KSPROPSETID_BtAudio`.
    const KSPROPSETID_BT_AUDIO: GUID = GUID::from_u128(0x7fa06c40_b8f6_4c7e_8556_e8c33a12e54d);
    /// `KSPROPERTY_ONESHOT_RECONNECT`.
    pub const ONESHOT_RECONNECT: u32 = 0;
    /// `KSPROPERTY_ONESHOT_DISCONNECT`.
    pub const ONESHOT_DISCONNECT: u32 = 1;

    /// A string COM handed out, copied and freed.
    ///
    /// # Safety
    /// `p` is null or a NUL-terminated string from `CoTaskMemAlloc`.
    unsafe fn take(p: PWSTR) -> Option<String> {
        if p.is_null() {
            return None;
        }
        let s = unsafe { p.to_string() }.ok();
        unsafe { CoTaskMemFree(Some(p.as_ptr() as *const c_void)) };
        s
    }

    /// The device's `PKEY_Device_FriendlyName`.
    ///
    /// # Safety
    /// COM is initialised on this thread.
    unsafe fn friendly_name(device: &IMMDevice) -> Option<String> {
        unsafe {
            let store = device.OpenPropertyStore(STGM_READ).ok()?;
            let value = store.GetValue(&PKEY_Device_FriendlyName).ok()?;
            take(PropVariantToStringAlloc(&value).ok()?).filter(|s| !s.is_empty())
        }
    }

    /// The KS filter an endpoint is connected to: its device id and part.
    ///
    /// # Safety
    /// COM is initialised on this thread.
    unsafe fn filter_of(endpoint: &IMMDevice) -> Option<(String, IPart)> {
        unsafe {
            let topology: IDeviceTopology = endpoint.Activate(CLSCTX_ALL, None).ok()?;
            let connector = topology.GetConnector(0).ok()?;
            let part: IPart = connector.GetConnectedTo().ok()?.cast().ok()?;
            let id = take(part.GetTopologyObject().ok()?.GetDeviceId().ok()?)?;
            Some((id, part))
        }
    }

    /// What [`read`] finds: the endpoints, the filters' parts by id, and the enumerator.
    pub type Read = (Vec<Seen>, HashMap<String, IPart>, IMMDeviceEnumerator);

    /// Every active or unplugged endpoint with its Bluetooth filter, the
    /// filters' parts by id, and the enumerator that found them. COM must be
    /// initialised on this thread.
    pub fn read() -> Result<Read, String> {
        // SAFETY: COM objects and strings are released before returning
        // (drops and `take`); the caller runs inside `with_com`.
        unsafe {
            let enumerator: IMMDeviceEnumerator =
                CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL).map_err(|e| e.to_string())?;
            let endpoints = enumerator
                .EnumAudioEndpoints(eAll, DEVICE_STATE(DEVICE_STATE_ACTIVE.0 | DEVICE_STATE_UNPLUGGED.0))
                .map_err(|e| e.to_string())?;
            let count = endpoints.GetCount().map_err(|e| e.to_string())?;
            let mut seen = Vec::new();
            let mut parts: HashMap<String, IPart> = HashMap::new();
            let mut names: HashMap<String, Option<String>> = HashMap::new();
            for i in 0..count {
                let Ok(endpoint) = endpoints.Item(i) else { continue };
                let Some((filter, part)) = filter_of(&endpoint) else { continue };
                if !super::is_bluetooth(&filter) {
                    continue;
                }
                let active = endpoint.GetState().is_ok_and(|s| s == DEVICE_STATE_ACTIVE);
                let filter_name = names
                    .entry(filter.clone())
                    .or_insert_with(|| {
                        let wide: Vec<u16> = filter.encode_utf16().chain(Some(0)).collect();
                        enumerator.GetDevice(PCWSTR(wide.as_ptr())).ok().and_then(|d| friendly_name(&d))
                    })
                    .clone();
                seen.push(Seen { filter: filter.clone(), filter_name, endpoint_name: friendly_name(&endpoint), active });
                parts.entry(filter).or_insert(part);
            }
            Ok((seen, parts, enumerator))
        }
    }

    /// Sends the one-shot `property` to every filter of the device `id`;
    /// fine when any filter takes it. COM must be initialised on this thread.
    pub fn oneshot(id: &str, property: u32) -> Result<(), String> {
        let (seen, parts, enumerator) = read()?;
        let filters = filters_of(&group(&seen), id).ok_or_else(|| "device not found".to_string())?;
        let (mut sent, mut error) = (false, "device not found".to_string());
        for filter in filters {
            let Some(part) = parts.get(&filter) else { continue };
            // The connector part's KS control first; the filter device's own
            // when the part does not hand one out (it is not in the part's
            // documented list).
            // SAFETY: the part and enumerator are alive on this thread.
            let result = first_working(
                || unsafe { send(&part_control(part)?, property) },
                || unsafe { send(&device_control(&enumerator, &filter)?, property) },
            );
            match result {
                Ok(()) => sent = true,
                Err(e) => error = e,
            }
        }
        if sent {
            Ok(())
        } else {
            Err(error)
        }
    }

    /// The connector part's KS control.
    ///
    /// # Safety
    /// COM is initialised on this thread.
    unsafe fn part_control(part: &IPart) -> Result<IKsControl, String> {
        unsafe {
            let mut raw: *mut c_void = std::ptr::null_mut();
            part.Activate(CLSCTX_INPROC_SERVER.0, &IKsControl::IID, Some(&mut raw)).map_err(|e| e.to_string())?;
            if raw.is_null() {
                return Err("no KS control".to_string());
            }
            Ok(IKsControl::from_raw(raw))
        }
    }

    /// The KS control of the filter device itself.
    ///
    /// # Safety
    /// COM is initialised on this thread.
    unsafe fn device_control(enumerator: &IMMDeviceEnumerator, filter: &str) -> Result<IKsControl, String> {
        let wide: Vec<u16> = filter.encode_utf16().chain(Some(0)).collect();
        unsafe {
            let device = enumerator.GetDevice(PCWSTR(wide.as_ptr())).map_err(|e| e.to_string())?;
            device.Activate::<IKsControl>(CLSCTX_ALL, None).map_err(|e| e.to_string())
        }
    }

    /// Sends the one-shot `property` through `control`.
    ///
    /// # Safety
    /// COM is initialised on this thread.
    unsafe fn send(control: &IKsControl, property: u32) -> Result<(), String> {
        unsafe {
            let ks = KSIDENTIFIER {
                Anonymous: KSIDENTIFIER_0 {
                    Anonymous: KSIDENTIFIER_0_0 { Set: KSPROPSETID_BT_AUDIO, Id: property, Flags: KSPROPERTY_TYPE_GET },
                },
            };
            let mut returned = 0u32;
            control
                .KsProperty(&ks, std::mem::size_of::<KSIDENTIFIER>() as u32, std::ptr::null_mut(), 0, &mut returned)
                .map_err(|e| e.to_string())
        }
    }
}

/// The real thing, through the WLAN API. Never use in tests.
pub struct WlanWifi;

impl Wifi for WlanWifi {
    fn profiles(&self) -> Result<Vec<WifiProfile>, String> {
        let client = wlan::Client::open()?;
        let read = client
            .interfaces()?
            .iter()
            .map(|interface| (client.profiles(interface), client.current(interface)))
            .collect();
        merge_profiles(read)
    }

    fn connect(&self, name: &str) -> Result<(), String> {
        let client = wlan::Client::open()?;
        join_first(&client.interfaces()?, name, |i| client.profiles(i), |i| client.connect(i, name))
    }
}

/// One interface's saved profile names (or why they could not be read),
/// and the profile it is connected with.
type InterfaceProfiles = (Result<Vec<String>, String>, Option<String>);

/// Every interface's saved profiles (and the one it is connected with) as
/// one list, deduplicated by name. An interface whose list cannot be read is
/// skipped; an error only when every interface failed.
fn merge_profiles(per_interface: Vec<InterfaceProfiles>) -> Result<Vec<WifiProfile>, String> {
    let mut out: Vec<WifiProfile> = Vec::new();
    let (mut read_any, mut error) = (per_interface.is_empty(), None);
    for (profiles, current) in per_interface {
        let names = match profiles {
            Ok(names) => names,
            Err(e) => {
                error = Some(e);
                continue;
            }
        };
        read_any = true;
        for name in names {
            let connected = current.as_deref() == Some(name.as_str());
            match out.iter_mut().find(|p| p.name == name) {
                Some(p) => p.connected |= connected,
                None => out.push(WifiProfile { name, connected }),
            }
        }
    }
    match (read_any, error) {
        (false, Some(e)) => Err(e),
        _ => Ok(out),
    }
}

/// Joins `name` on the first interface that holds it and takes the join;
/// an interface whose list or join fails is skipped.
fn join_first<I>(
    interfaces: &[I],
    name: &str,
    profiles: impl Fn(&I) -> Result<Vec<String>, String>,
    connect: impl Fn(&I) -> Result<(), String>,
) -> Result<(), String> {
    let mut error = "no such saved network".to_string();
    for interface in interfaces {
        match profiles(interface) {
            Ok(names) if names.iter().any(|n| n == name) => match connect(interface) {
                Ok(()) => return Ok(()),
                Err(e) => error = e,
            },
            Ok(_) => {}
            Err(e) => error = e,
        }
    }
    Err(error)
}

mod wlan {
    use std::ffi::c_void;
    use windows::core::{GUID, PCWSTR};
    use windows::Win32::Foundation::HANDLE;
    use windows::Win32::NetworkManagement::WiFi::{
        dot11_BSS_type_infrastructure, wlan_connection_mode_profile, wlan_interface_state_connected,
        wlan_intf_opcode_current_connection, WlanCloseHandle, WlanConnect, WlanEnumInterfaces, WlanFreeMemory,
        WlanGetProfileList, WlanOpenHandle, WlanQueryInterface, WLAN_CONNECTION_ATTRIBUTES,
        WLAN_CONNECTION_PARAMETERS, WLAN_INTERFACE_INFO_LIST, WLAN_PROFILE_INFO_LIST,
    };

    /// WLAN API version 2 (Vista and later).
    const CLIENT_VERSION: u32 = 2;

    fn text(raw: &[u16]) -> String {
        let end = raw.iter().position(|&c| c == 0).unwrap_or(raw.len());
        String::from_utf16_lossy(&raw[..end])
    }

    /// Memory the WLAN API handed out, freed on drop.
    struct Owned<T>(*mut T);

    impl<T> Drop for Owned<T> {
        fn drop(&mut self) {
            if !self.0.is_null() {
                // SAFETY: the pointer came from the WLAN API and is freed once.
                unsafe { WlanFreeMemory(self.0 as *const c_void) };
            }
        }
    }

    /// An open WLAN client handle, closed on drop.
    pub struct Client(HANDLE);

    impl Drop for Client {
        fn drop(&mut self) {
            // SAFETY: the handle came from WlanOpenHandle and is closed once.
            unsafe { WlanCloseHandle(self.0, None) };
        }
    }

    impl Client {
        pub fn open() -> Result<Client, String> {
            let (mut version, mut handle) = (0u32, HANDLE::default());
            // SAFETY: both out pointers are valid locals.
            let r = unsafe { WlanOpenHandle(CLIENT_VERSION, None, &mut version, &mut handle) };
            if r != 0 {
                return Err(format!("WlanOpenHandle failed: {r}"));
            }
            Ok(Client(handle))
        }

        pub fn interfaces(&self) -> Result<Vec<GUID>, String> {
            let mut list: *mut WLAN_INTERFACE_INFO_LIST = std::ptr::null_mut();
            // SAFETY: the list is freed by `Owned`; items are read within its count.
            unsafe {
                let r = WlanEnumInterfaces(self.0, None, &mut list);
                let list = Owned(list);
                if r != 0 || list.0.is_null() {
                    return Err(format!("WlanEnumInterfaces failed: {r}"));
                }
                let n = (*list.0).dwNumberOfItems as usize;
                let items = std::slice::from_raw_parts((*list.0).InterfaceInfo.as_ptr(), n);
                Ok(items.iter().map(|i| i.InterfaceGuid).collect())
            }
        }

        /// The interface's saved profile names.
        pub fn profiles(&self, interface: &GUID) -> Result<Vec<String>, String> {
            let mut list: *mut WLAN_PROFILE_INFO_LIST = std::ptr::null_mut();
            // SAFETY: the list is freed by `Owned`; items are read within its count.
            unsafe {
                let r = WlanGetProfileList(self.0, interface, None, &mut list);
                let list = Owned(list);
                if r != 0 || list.0.is_null() {
                    return Err(format!("WlanGetProfileList failed: {r}"));
                }
                let n = (*list.0).dwNumberOfItems as usize;
                let items = std::slice::from_raw_parts((*list.0).ProfileInfo.as_ptr(), n);
                Ok(items.iter().map(|p| text(&p.strProfileName)).filter(|n| !n.is_empty()).collect())
            }
        }

        /// The profile the interface is connected with, if any.
        pub fn current(&self, interface: &GUID) -> Option<String> {
            let (mut size, mut data) = (0u32, std::ptr::null_mut::<c_void>());
            // SAFETY: the data is freed by `Owned` and read only when its size fits.
            unsafe {
                let r = WlanQueryInterface(
                    self.0,
                    interface,
                    wlan_intf_opcode_current_connection,
                    None,
                    &mut size,
                    &mut data,
                    None,
                );
                let data = Owned(data as *mut WLAN_CONNECTION_ATTRIBUTES);
                if r != 0 || data.0.is_null() || (size as usize) < std::mem::size_of::<WLAN_CONNECTION_ATTRIBUTES>() {
                    return None;
                }
                let attrs = &*data.0;
                (attrs.isState == wlan_interface_state_connected)
                    .then(|| text(&attrs.strProfileName))
                    .filter(|n| !n.is_empty())
            }
        }

        pub fn connect(&self, interface: &GUID, profile: &str) -> Result<(), String> {
            let wide: Vec<u16> = profile.encode_utf16().chain(Some(0)).collect();
            let params = WLAN_CONNECTION_PARAMETERS {
                wlanConnectionMode: wlan_connection_mode_profile,
                strProfile: PCWSTR(wide.as_ptr()),
                pDot11Ssid: std::ptr::null_mut(),
                pDesiredBssidList: std::ptr::null_mut(),
                dot11BssType: dot11_BSS_type_infrastructure,
                dwFlags: 0,
            };
            // SAFETY: `params` and `wide` outlive the call.
            let r = unsafe { WlanConnect(self.0, interface, &params, None) };
            if r != 0 {
                return Err(format!("WlanConnect failed: {r}"));
            }
            Ok(())
        }
    }
}

#[cfg(test)]
pub mod testing {
    use super::*;
    use std::sync::{Condvar, Mutex, MutexGuard};
    use std::thread::ThreadId;

    fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
        m.lock().unwrap_or_else(|e| e.into_inner())
    }

    struct State<T> {
        list: Result<Vec<T>, String>,
        calls: Vec<String>,
        fail_next: bool,
        blocked: bool,
        /// The thread of every list read, recorded before any wait on the gate.
        reads: Vec<ThreadId>,
    }

    /// Scripted list, recorded calls, a gate on reads.
    struct Script<T> {
        state: Mutex<State<T>>,
        gate: Condvar,
    }

    impl<T: Clone> Script<T> {
        fn new(list: Result<Vec<T>, String>) -> Self {
            Script {
                state: Mutex::new(State { list, calls: Vec::new(), fail_next: false, blocked: false, reads: Vec::new() }),
                gate: Condvar::new(),
            }
        }

        fn read(&self) -> Result<Vec<T>, String> {
            let mut state = lock(&self.state);
            state.reads.push(std::thread::current().id());
            while state.blocked {
                state = self.gate.wait(state).unwrap_or_else(|p| p.into_inner());
            }
            state.list.clone()
        }

        fn call(&self, what: String) -> Result<(), String> {
            let mut state = lock(&self.state);
            state.calls.push(what);
            if std::mem::take(&mut state.fail_next) {
                return Err("failed".to_string());
            }
            Ok(())
        }

        fn block(&self) {
            lock(&self.state).blocked = true;
        }

        fn release(&self) {
            lock(&self.state).blocked = false;
            self.gate.notify_all();
        }
    }

    /// A scripted Bluetooth list: `connect <id>` / `disconnect <id>` recorded.
    pub struct FakeBt(Script<BtDevice>);

    impl FakeBt {
        pub fn new(devices: Vec<BtDevice>) -> Self {
            FakeBt(Script::new(Ok(devices)))
        }
        pub fn failing() -> Self {
            FakeBt(Script::new(Err("failed".to_string())))
        }
        pub fn calls(&self) -> Vec<String> {
            lock(&self.0.state).calls.clone()
        }
        /// The next connect or disconnect fails.
        pub fn fail_next(&self) {
            lock(&self.0.state).fail_next = true;
        }
        pub fn block(&self) {
            self.0.block()
        }
        pub fn release(&self) {
            self.0.release()
        }
        /// `list` calls started so far.
        pub fn lists(&self) -> usize {
            lock(&self.0.state).reads.len()
        }
        /// What `list` answers from now on (a device was paired meanwhile).
        pub fn set(&self, devices: Vec<BtDevice>) {
            lock(&self.0.state).list = Ok(devices);
        }
        /// `list` calls started on `thread`.
        pub fn lists_on(&self, thread: ThreadId) -> usize {
            lock(&self.0.state).reads.iter().filter(|t| **t == thread).count()
        }
    }

    impl BtAudio for FakeBt {
        fn list(&self) -> Result<Vec<BtDevice>, String> {
            self.0.read()
        }
        fn connect(&self, id: &str) -> Result<(), String> {
            self.0.call(format!("connect {id}"))
        }
        fn disconnect(&self, id: &str) -> Result<(), String> {
            self.0.call(format!("disconnect {id}"))
        }
    }

    /// Scripted saved networks: `join <name>` recorded.
    pub struct FakeWifi(Script<WifiProfile>);

    impl FakeWifi {
        pub fn new(profiles: Vec<WifiProfile>) -> Self {
            FakeWifi(Script::new(Ok(profiles)))
        }
        pub fn failing() -> Self {
            FakeWifi(Script::new(Err("failed".to_string())))
        }
        pub fn calls(&self) -> Vec<String> {
            lock(&self.0.state).calls.clone()
        }
        /// The next join fails.
        pub fn fail_next(&self) {
            lock(&self.0.state).fail_next = true;
        }
        pub fn block(&self) {
            self.0.block()
        }
        pub fn release(&self) {
            self.0.release()
        }
        /// `profiles` calls started so far.
        pub fn lists(&self) -> usize {
            lock(&self.0.state).reads.len()
        }
    }

    impl Wifi for FakeWifi {
        fn profiles(&self) -> Result<Vec<WifiProfile>, String> {
            self.0.read()
        }
        fn connect(&self, name: &str) -> Result<(), String> {
            self.0.call(format!("join {name}"))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn seen(filter: &str, filter_name: Option<&str>, endpoint_name: &str, active: bool) -> Seen {
        Seen {
            filter: filter.to_string(),
            filter_name: filter_name.map(str::to_string),
            endpoint_name: Some(endpoint_name.to_string()),
            active,
        }
    }

    const STEREO: &str = r"{2}.\\?\bthenum#{0000110b-0000-1000-8000-00805f9b34fb}_vid&0001#7&1&0&aabbccddeeff_c00000000#{x}\a2dp";
    const HANDS_FREE: &str = r"{2}.\\?\BTHHFENUM#BthHFPAudio#8&2&97#{x}\hfp";

    #[test]
    fn only_bluetooth_filters_count() {
        let groups = group(&[
            seen(r"{2}.\\?\hdaudio#func_01#{x}\speaker", Some("Realtek Audio"), "Speakers (Realtek Audio)", true),
            seen(STEREO, Some("WH-1000XM4"), "Headphones (WH-1000XM4)", false),
        ]);
        assert_eq!(groups.len(), 1);
        assert_eq!(groups[0].0, BtDevice { id: STEREO.into(), name: "WH-1000XM4".into(), connected: false });
    }

    #[test]
    fn endpoints_of_one_filter_are_one_device_connected_when_any_is_active() {
        let groups = group(&[
            seen(STEREO, Some("WH-1000XM4"), "Headphones (WH-1000XM4)", false),
            seen(STEREO, Some("WH-1000XM4"), "Headset (WH-1000XM4)", true),
        ]);
        assert_eq!(groups, vec![(BtDevice { id: STEREO.into(), name: "WH-1000XM4".into(), connected: true }, vec![STEREO.to_string()])]);
    }

    #[test]
    fn filters_of_the_same_name_are_one_device_and_all_get_the_call() {
        let groups = group(&[
            seen(STEREO, Some("WH-1000XM4"), "Headphones (WH-1000XM4)", false),
            seen(HANDS_FREE, Some("WH-1000XM4"), "Headset (WH-1000XM4)", false),
        ]);
        assert_eq!(groups.len(), 1);
        assert_eq!(filters_of(&groups, STEREO), Some(vec![STEREO.to_string(), HANDS_FREE.to_string()]));
        assert_eq!(filters_of(&groups, HANDS_FREE), Some(vec![STEREO.to_string(), HANDS_FREE.to_string()]));
        assert_eq!(filters_of(&groups, "other"), None);
    }

    #[test]
    fn the_endpoint_name_is_only_a_fallback() {
        let groups = group(&[seen(STEREO, None, "Headphones (Speaker)", true)]);
        assert_eq!(groups[0].0.name, "Headphones (Speaker)");
    }

    #[test]
    fn unnamed_devices_stay_apart() {
        let groups = group(&[
            seen(STEREO, None, "Headphones", false),
            seen(HANDS_FREE, None, "Headphones", true),
        ]);
        assert_eq!(groups.len(), 2);
        let generic = group(&[
            Seen { filter: STEREO.into(), filter_name: None, endpoint_name: None, active: false },
            Seen { filter: HANDS_FREE.into(), filter_name: None, endpoint_name: None, active: false },
        ]);
        assert_eq!(generic.len(), 2);
        assert!(generic.iter().all(|(d, _)| d.name == "Bluetooth audio device"));
    }

    #[test]
    fn an_own_name_never_joins_a_fallback_name() {
        let groups = group(&[
            seen(STEREO, None, "Speaker", false),
            seen(HANDS_FREE, Some("Speaker"), "Headset (Speaker)", false),
        ]);
        assert_eq!(groups.len(), 2);
    }

    #[test]
    fn the_fallback_runs_only_when_the_first_way_fails() {
        let ran = std::cell::Cell::new(false);
        assert_eq!(first_working(|| Ok(1), || { ran.set(true); Ok(2) }), Ok(1));
        assert!(!ran.get());
        assert_eq!(first_working(|| Err::<u32, _>("part".to_string()), || Ok(2)), Ok(2));
        assert_eq!(
            first_working(|| Err::<u32, _>("part".to_string()), || Err("device".to_string())),
            Err("part; device".to_string())
        );
    }

    fn names(list: &[&str]) -> Result<Vec<String>, String> {
        Ok(list.iter().map(|s| s.to_string()).collect())
    }

    #[test]
    fn a_failing_interface_is_skipped_when_reading_profiles() {
        let merged = merge_profiles(vec![
            (Err("denied".to_string()), None),
            (names(&["Home-5G", "Office"]), Some("Office".to_string())),
            (names(&["Office"]), None),
        ]);
        assert_eq!(
            merged,
            Ok(vec![WifiProfile { name: "Home-5G".into(), connected: false }, WifiProfile { name: "Office".into(), connected: true }])
        );
        assert_eq!(merge_profiles(vec![(Err("a".into()), None), (Err("b".into()), None)]), Err("b".to_string()));
        assert_eq!(merge_profiles(vec![]), Ok(vec![]));
    }

    #[test]
    fn joining_skips_failing_interfaces() {
        let joined = std::cell::RefCell::new(Vec::new());
        let lists = |i: &u32| match i {
            0 => Err("denied".to_string()),
            1 | 2 => names(&["Home-5G"]),
            _ => names(&[]),
        };
        let connect = |i: &u32| {
            joined.borrow_mut().push(*i);
            if *i == 1 { Err("busy".to_string()) } else { Ok(()) }
        };
        assert_eq!(join_first(&[0, 3, 1, 2], "Home-5G", lists, connect), Ok(()));
        assert_eq!(*joined.borrow(), vec![1, 2]);
        assert_eq!(join_first(&[0, 1], "Home-5G", lists, connect), Err("busy".to_string()));
        assert_eq!(join_first(&[0], "Home-5G", lists, connect), Err("denied".to_string()));
        assert_eq!(join_first(&[3], "Home-5G", lists, connect), Err("no such saved network".to_string()));
    }

    /// By hand: `cargo test --lib real_devices_read -- --ignored --nocapture`.
    /// Reads only; prints counts and connected flags, never names.
    #[test]
    #[ignore]
    fn real_devices_read() {
        match KsBtAudio.list() {
            Ok(devices) => {
                let flags: Vec<bool> = devices.iter().map(|d| d.connected).collect();
                println!("bluetooth audio devices: {} connected flags: {flags:?}", devices.len());
            }
            Err(e) => println!("bluetooth audio devices: error {e}"),
        }
        match WlanWifi.profiles() {
            Ok(profiles) => println!(
                "saved wi-fi profiles: {} connected: {}",
                profiles.len(),
                profiles.iter().filter(|p| p.connected).count()
            ),
            Err(e) => println!("saved wi-fi profiles: error {e}"),
        }
    }
}
