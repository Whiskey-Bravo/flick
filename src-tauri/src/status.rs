//! System status for the bar: battery, CPU, memory, disks and IP addresses.
//!
//! The query path reads memory only: the store answers from its cache and takes
//! each sample on its own background thread.

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use crate::net::Fetch;

/// Where the public address is read from (only when `online`).
const PUBLIC_IP_URL: &str = "https://api.ipify.org";
/// A cached value younger than this is served as is.
const FRESH: Duration = Duration::from_secs(3);
/// The public address is asked for only once `ip` stayed typed this long
/// (the same pause the weather lookup waits for).
const IP_PAUSE: Duration = Duration::from_millis(400);
const GIB: f64 = 1024.0 * 1024.0 * 1024.0;

#[derive(Clone, Debug, PartialEq)]
pub struct Battery {
    pub percent: u8,
    /// Taking charge (not held by a charge limit).
    pub charging: bool,
    /// On AC power, charging or not.
    pub plugged_in: bool,
    pub minutes_left: Option<u32>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Memory {
    pub used: u64,
    pub total: u64,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Disk {
    /// Drive as `C:`.
    pub root: String,
    pub free: u64,
    pub total: u64,
}

/// Cumulative CPU times as `GetSystemTimes` reports them: kernel includes idle.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct CpuTimes {
    pub idle: u64,
    pub kernel: u64,
    pub user: u64,
}

/// Reads of the machine's state; blocking, so only called off the query path.
pub trait StatusSource: Send + Sync {
    fn battery(&self) -> Option<Battery>;
    fn memory(&self) -> Memory;
    fn disks(&self) -> Vec<Disk>;
    fn cpu_times(&self) -> CpuTimes;
    fn local_ips(&self) -> Vec<String>;
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum StatusKind {
    Battery,
    Cpu,
    Ram,
    Disk,
    Ip,
}

#[derive(Clone, Debug, PartialEq)]
pub enum StatusValue {
    /// The first sample is still running.
    Pending,
    /// `(title, copy text)`; an empty copy text means nothing to copy.
    Lines(Vec<(String, String)>),
}

/// `1 h 20 min`, `45 min`, `2 h`.
fn duration_text(minutes: u32) -> String {
    match (minutes / 60, minutes % 60) {
        (0, m) => format!("{m} min"),
        (h, 0) => format!("{h} h"),
        (h, m) => format!("{h} h {m} min"),
    }
}

fn battery_line(b: Option<Battery>) -> (String, String) {
    let Some(b) = b else {
        return ("No battery".into(), String::new());
    };
    let mut text = format!("{}%", b.percent);
    if (b.charging && b.percent >= 100) || (b.plugged_in && !b.charging) {
        text.push_str(" · plugged in");
    } else {
        if b.charging {
            text.push_str(" · charging");
        }
        if let Some(m) = b.minutes_left.filter(|m| *m > 0) {
            text.push_str(&format!(" · about {} left", duration_text(m)));
        }
    }
    (text.clone(), text)
}

fn memory_line(m: Memory) -> (String, String) {
    let percent = if m.total == 0 { 0.0 } else { m.used as f64 / m.total as f64 * 100.0 };
    let text = format!("{:.1} of {:.0} GB used ({:.0}%)", m.used as f64 / GIB, m.total as f64 / GIB, percent);
    (text.clone(), text)
}

fn disk_lines(disks: Vec<Disk>) -> Vec<(String, String)> {
    disks
        .into_iter()
        .map(|d| {
            let text = format!("{} {:.0} GB free of {:.0} GB", d.root, d.free as f64 / GIB, d.total as f64 / GIB);
            (text.clone(), text)
        })
        .collect()
}

fn cpu_line(a: CpuTimes, b: CpuTimes) -> (String, String) {
    let idle = b.idle.saturating_sub(a.idle) as f64;
    let total = b.kernel.saturating_sub(a.kernel) as f64 + b.user.saturating_sub(a.user) as f64;
    let percent = if total <= 0.0 { 0.0 } else { (100.0 * (1.0 - idle / total)).clamp(0.0, 100.0) };
    let text = format!("CPU {:.0}%", percent);
    (text.clone(), text)
}

/// Local addresses always; the public one only when `online`, and fetched only when `fetch_public`
/// (else a placeholder row until the typing pause is over).
fn ip_lines(src: &dyn StatusSource, fetch: &dyn Fetch, online: bool, fetch_public: bool) -> Vec<(String, String)> {
    let mut lines: Vec<(String, String)> = src.local_ips().into_iter().map(|ip| (ip.clone(), ip)).collect();
    if online && !fetch_public {
        lines.push(("Public IP: checking…".into(), String::new()));
    } else if online {
        let public = fetch
            .get(PUBLIC_IP_URL)
            .ok()
            .and_then(|body| String::from_utf8(body).ok())
            .map(|s| s.trim().to_string())
            .filter(|s| {
                !s.is_empty() && s.len() <= 45 && s.chars().all(|c| c.is_ascii_hexdigit() || c == '.' || c == ':')
            });
        match public {
            Some(ip) => lines.push((format!("Public IP {ip}"), ip)),
            None => lines.push(("Public IP unavailable".into(), String::new())),
        }
    }
    if lines.is_empty() {
        lines.push(("No network connection".into(), String::new()));
    }
    lines
}

#[derive(Default)]
struct State {
    values: HashMap<StatusKind, (StatusValue, Instant)>,
    running: HashSet<StatusKind>,
    /// The cached `Ip` value includes the public address lookup.
    ip_public: bool,
    /// Since when the text has stayed `ip` (a timer re-asks once the pause is over).
    ip_since: Option<Instant>,
}

/// Cached samples, refreshed in the background.
pub struct StatusStore {
    src: Arc<dyn StatusSource>,
    fetch: Arc<dyn Fetch>,
    on_ready: Arc<dyn Fn() + Send + Sync>,
    cpu_gap: Duration,
    state: Arc<Mutex<State>>,
}

impl StatusStore {
    pub fn new(
        src: Arc<dyn StatusSource>,
        fetch: Arc<dyn Fetch>,
        on_ready: Arc<dyn Fn() + Send + Sync>,
        cpu_gap: Duration,
    ) -> Arc<Self> {
        Arc::new(StatusStore { src, fetch, on_ready, cpu_gap, state: Arc::new(Mutex::new(State::default())) })
    }

    /// The cached value (memory only). When it is missing or older than 3 s,
    /// one background sample starts (never two for a kind) and `on_ready` is
    /// called once when it is done; meanwhile the old value or `Pending` is returned.
    pub fn get(&self, kind: StatusKind, online: bool) -> StatusValue {
        let mut st = self.state.lock().unwrap();
        // The public address waits for the typing pause; a timer re-asks when it is over.
        let mut fetch_public = false;
        if kind == StatusKind::Ip && online {
            match st.ip_since {
                Some(since) => fetch_public = since.elapsed() >= IP_PAUSE,
                None => {
                    st.ip_since = Some(Instant::now());
                    let (state, on_ready) = (self.state.clone(), self.on_ready.clone());
                    std::thread::spawn(move || {
                        std::thread::sleep(IP_PAUSE);
                        if state.lock().unwrap().ip_since.is_some() {
                            on_ready();
                        }
                    });
                }
            }
        }
        let cached = st.values.get(&kind).cloned();
        if let Some((v, at)) = &cached {
            let wants_public = kind == StatusKind::Ip && fetch_public && !st.ip_public;
            if at.elapsed() < FRESH && !wants_public {
                return v.clone();
            }
        }
        if st.running.insert(kind) {
            let (src, fetch, on_ready, gap) = (self.src.clone(), self.fetch.clone(), self.on_ready.clone(), self.cpu_gap);
            let state = self.state.clone();
            std::thread::spawn(move || {
                let lines = match kind {
                    StatusKind::Battery => vec![battery_line(src.battery())],
                    StatusKind::Ram => vec![memory_line(src.memory())],
                    StatusKind::Disk => disk_lines(src.disks()),
                    StatusKind::Cpu => {
                        let a = src.cpu_times();
                        std::thread::sleep(gap);
                        vec![cpu_line(a, src.cpu_times())]
                    }
                    StatusKind::Ip => ip_lines(src.as_ref(), fetch.as_ref(), online, fetch_public),
                };
                {
                    let mut st = state.lock().unwrap();
                    if kind == StatusKind::Ip {
                        st.ip_public = online && fetch_public;
                    }
                    st.values.insert(kind, (StatusValue::Lines(lines), Instant::now()));
                    st.running.remove(&kind);
                }
                on_ready();
            });
        }
        cached.map_or(StatusValue::Pending, |(v, _)| v)
    }

    /// The text is no longer `ip`: the next `ip` starts a new typing pause.
    pub fn leave_ip(&self) {
        self.state.lock().unwrap().ip_since = None;
    }
}

/// The battery from `SYSTEM_POWER_STATUS` fields; `None` for no battery or an
/// unknown state. AC power alone is not charging: a charge limit (battery care)
/// can hold a plugged-in laptop at 80%.
fn power_battery(ac_line: u8, flag: u8, percent: u8, seconds_left: u32) -> Option<Battery> {
    // BatteryFlag: 8 = charging, 128 = no battery, 255 = unknown; BatteryLifePercent 255 = unknown.
    if flag & 128 != 0 || flag == 255 || percent > 100 {
        return None;
    }
    Some(Battery {
        percent,
        charging: flag & 8 != 0,
        plugged_in: ac_line == 1,
        minutes_left: (seconds_left != u32::MAX).then_some(seconds_left / 60),
    })
}

/// The real machine, through Win32.
pub struct SystemStatus;

impl StatusSource for SystemStatus {
    fn battery(&self) -> Option<Battery> {
        use windows::Win32::System::Power::{GetSystemPowerStatus, SYSTEM_POWER_STATUS};
        let mut s = SYSTEM_POWER_STATUS::default();
        // SAFETY: `s` is a valid out-pointer for the call.
        unsafe { GetSystemPowerStatus(&mut s) }.ok()?;
        power_battery(s.ACLineStatus, s.BatteryFlag, s.BatteryLifePercent, s.BatteryLifeTime)
    }

    fn memory(&self) -> Memory {
        use windows::Win32::System::SystemInformation::{GlobalMemoryStatusEx, MEMORYSTATUSEX};
        let mut m = MEMORYSTATUSEX { dwLength: std::mem::size_of::<MEMORYSTATUSEX>() as u32, ..Default::default() };
        // SAFETY: `m` is a valid out-pointer with dwLength set.
        if unsafe { GlobalMemoryStatusEx(&mut m) }.is_err() {
            return Memory { used: 0, total: 0 };
        }
        Memory { used: m.ullTotalPhys.saturating_sub(m.ullAvailPhys), total: m.ullTotalPhys }
    }

    fn disks(&self) -> Vec<Disk> {
        use windows::core::PCWSTR;
        use windows::Win32::Storage::FileSystem::{GetDiskFreeSpaceExW, GetDriveTypeW, GetLogicalDrives};
        const DRIVE_FIXED: u32 = 3;
        // SAFETY: plain query without arguments.
        let mask = unsafe { GetLogicalDrives() };
        let mut out = Vec::new();
        for i in 0..26u32 {
            if mask & (1 << i) == 0 {
                continue;
            }
            let letter = (b'A' + i as u8) as char;
            let root: Vec<u16> = format!("{letter}:\\").encode_utf16().chain(Some(0)).collect();
            // SAFETY: `root` is NUL-terminated and outlives the calls.
            if unsafe { GetDriveTypeW(PCWSTR(root.as_ptr())) } != DRIVE_FIXED {
                continue;
            }
            let (mut free, mut total) = (0u64, 0u64);
            // SAFETY: the out-pointers are valid; the unused one is optional.
            let ok = unsafe { GetDiskFreeSpaceExW(PCWSTR(root.as_ptr()), Some(&mut free), Some(&mut total), None) };
            if ok.is_ok() && total > 0 {
                out.push(Disk { root: format!("{letter}:"), free, total });
            }
        }
        out
    }

    fn cpu_times(&self) -> CpuTimes {
        use windows::Win32::Foundation::FILETIME;
        use windows::Win32::System::Threading::GetSystemTimes;
        let (mut idle, mut kernel, mut user) = (FILETIME::default(), FILETIME::default(), FILETIME::default());
        // SAFETY: the three out-pointers are valid.
        let _ = unsafe { GetSystemTimes(Some(&mut idle), Some(&mut kernel), Some(&mut user)) };
        let n = |f: FILETIME| (u64::from(f.dwHighDateTime) << 32) | u64::from(f.dwLowDateTime);
        CpuTimes { idle: n(idle), kernel: n(kernel), user: n(user) }
    }

    fn local_ips(&self) -> Vec<String> {
        use std::net::Ipv4Addr;
        use windows::Win32::NetworkManagement::IpHelper::{
            GetAdaptersAddresses, GAA_FLAG_SKIP_ANYCAST, GAA_FLAG_SKIP_DNS_SERVER, GAA_FLAG_SKIP_MULTICAST,
            IF_TYPE_SOFTWARE_LOOPBACK, IP_ADAPTER_ADDRESSES_LH,
        };
        use windows::Win32::NetworkManagement::Ndis::IfOperStatusUp;
        use windows::Win32::Networking::WinSock::{AF_INET, SOCKADDR_IN};

        let flags = GAA_FLAG_SKIP_ANYCAST | GAA_FLAG_SKIP_MULTICAST | GAA_FLAG_SKIP_DNS_SERVER;
        let mut size = 16 * 1024u32;
        let mut buf: Vec<u64> = Vec::new();
        for _ in 0..4 {
            buf = vec![0u64; (size as usize).div_ceil(8)];
            // SAFETY: the buffer is 8-byte aligned and at least `size` bytes long.
            let rc = unsafe {
                GetAdaptersAddresses(
                    AF_INET.0 as u32,
                    flags,
                    None,
                    Some(buf.as_mut_ptr() as *mut IP_ADAPTER_ADDRESSES_LH),
                    &mut size,
                )
            };
            match rc {
                0 => break,
                111 => continue, // ERROR_BUFFER_OVERFLOW: `size` now holds the needed size.
                _ => return vec![],
            }
        }
        let mut out = Vec::new();
        let mut adapter = buf.as_ptr() as *const IP_ADAPTER_ADDRESSES_LH;
        // SAFETY: the list was written by GetAdaptersAddresses into `buf`, which lives through the walk.
        unsafe {
            while !adapter.is_null() {
                let a = &*adapter;
                if a.OperStatus == IfOperStatusUp && a.IfType != IF_TYPE_SOFTWARE_LOOPBACK {
                    let mut u = a.FirstUnicastAddress;
                    while !u.is_null() {
                        let sa = (*u).Address.lpSockaddr;
                        if !sa.is_null() && (*sa).sa_family == AF_INET {
                            let sin = &*(sa as *const SOCKADDR_IN);
                            let ip = Ipv4Addr::from(sin.sin_addr.S_un.S_addr.to_ne_bytes());
                            if !ip.is_loopback() && !ip.is_link_local() && !ip.is_unspecified() {
                                out.push(ip.to_string());
                            }
                        }
                        u = (*u).Next;
                    }
                }
                adapter = a.Next;
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::net::{FakeFetch, FetchError};
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::mpsc;

    const GB: u64 = 1024 * 1024 * 1024;

    struct FakeStatus {
        battery: Option<Battery>,
        memory: Memory,
        disks: Vec<Disk>,
        cpu: Mutex<Vec<CpuTimes>>,
        ips: Vec<String>,
        memory_reads: AtomicUsize,
    }

    impl Default for FakeStatus {
        fn default() -> Self {
            FakeStatus {
                battery: None,
                memory: Memory { used: 0, total: 0 },
                disks: vec![],
                cpu: Mutex::new(vec![CpuTimes { idle: 0, kernel: 0, user: 0 }]),
                ips: vec![],
                memory_reads: AtomicUsize::new(0),
            }
        }
    }

    impl StatusSource for FakeStatus {
        fn battery(&self) -> Option<Battery> {
            self.battery.clone()
        }
        fn memory(&self) -> Memory {
            self.memory_reads.fetch_add(1, Ordering::SeqCst);
            std::thread::sleep(Duration::from_millis(100));
            self.memory.clone()
        }
        fn disks(&self) -> Vec<Disk> {
            self.disks.clone()
        }
        fn cpu_times(&self) -> CpuTimes {
            let mut v = self.cpu.lock().unwrap();
            if v.len() > 1 {
                v.remove(0)
            } else {
                v[0]
            }
        }
        fn local_ips(&self) -> Vec<String> {
            self.ips.clone()
        }
    }

    fn channel() -> (Arc<dyn Fn() + Send + Sync>, mpsc::Receiver<()>) {
        let (tx, rx) = mpsc::channel();
        let tx = Mutex::new(tx);
        let on_ready: Arc<dyn Fn() + Send + Sync> = Arc::new(move || {
            let _ = tx.lock().unwrap().send(());
        });
        (on_ready, rx)
    }

    fn store(src: FakeStatus, fetch: Arc<FakeFetch>) -> (Arc<StatusStore>, mpsc::Receiver<()>) {
        let (on_ready, rx) = channel();
        (StatusStore::new(Arc::new(src), fetch, on_ready, Duration::ZERO), rx)
    }

    fn ready(rx: &mpsc::Receiver<()>) {
        rx.recv_timeout(Duration::from_secs(1)).expect("on_ready within 1 s");
    }

    fn titles(v: StatusValue) -> Vec<String> {
        match v {
            StatusValue::Lines(l) => l.into_iter().map(|(t, _)| t).collect(),
            StatusValue::Pending => panic!("still pending"),
        }
    }

    #[test]
    fn first_ask_is_pending_then_ready() {
        let src = FakeStatus { memory: Memory { used: (9.1 * GIB) as u64, total: 16 * GB }, ..Default::default() };
        let (s, rx) = store(src, Arc::new(FakeFetch::offline()));
        assert_eq!(s.get(StatusKind::Ram, true), StatusValue::Pending);
        ready(&rx);
        let text = "9.1 of 16 GB used (57%)".to_string();
        assert_eq!(s.get(StatusKind::Ram, true), StatusValue::Lines(vec![(text.clone(), text)]));
    }

    #[test]
    fn cpu_percent_from_two_samples() {
        let t = |idle, kernel, user| CpuTimes { idle, kernel, user };
        // Delta idle 770, delta kernel 800 (includes idle), delta user 200: 1 - 770/1000 = 23%.
        let src = FakeStatus { cpu: Mutex::new(vec![t(100, 200, 100), t(870, 1000, 300)]), ..Default::default() };
        let (s, rx) = store(src, Arc::new(FakeFetch::offline()));
        s.get(StatusKind::Cpu, true);
        ready(&rx);
        assert_eq!(titles(s.get(StatusKind::Cpu, true)), vec!["CPU 23%"]);
    }

    #[test]
    fn charging_is_the_battery_flag_not_ac_power() {
        let b = |ac, flag| power_battery(ac, flag, 80, u32::MAX).map(|b| (b.charging, b.plugged_in));
        assert_eq!(b(1, 8 | 1), Some((true, true)));
        assert_eq!(b(1, 1), Some((false, true)), "a charge limit holds it");
        assert_eq!(b(0, 2), Some((false, false)));
        assert_eq!(b(1, 128), None);
        assert_eq!(b(1, 255), None);
        assert_eq!(power_battery(0, 0, 255, 0), None);
        assert_eq!(power_battery(0, 0, 50, 2700).and_then(|b| b.minutes_left), Some(45));
    }

    #[test]
    fn battery_formats() {
        let cases = [
            (
                Some(Battery { percent: 82, charging: true, plugged_in: true, minutes_left: Some(80) }),
                "82% · charging · about 1 h 20 min left",
            ),
            (Some(Battery { percent: 100, charging: true, plugged_in: true, minutes_left: None }), "100% · plugged in"),
            (Some(Battery { percent: 100, charging: false, plugged_in: true, minutes_left: None }), "100% · plugged in"),
            // Held at 80% by a charge limit: on AC, not charging.
            (Some(Battery { percent: 80, charging: false, plugged_in: true, minutes_left: None }), "80% · plugged in"),
            (
                Some(Battery { percent: 40, charging: false, plugged_in: false, minutes_left: Some(45) }),
                "40% · about 45 min left",
            ),
            (Some(Battery { percent: 55, charging: false, plugged_in: false, minutes_left: None }), "55%"),
            (None, "No battery"),
        ];
        for (battery, want) in cases {
            let (s, rx) = store(FakeStatus { battery, ..Default::default() }, Arc::new(FakeFetch::offline()));
            s.get(StatusKind::Battery, true);
            ready(&rx);
            assert_eq!(titles(s.get(StatusKind::Battery, true)), vec![want]);
        }
    }

    #[test]
    fn disk_rows_per_fixed_drive() {
        let disks = vec![
            Disk { root: "C:".into(), free: 120 * GB, total: 476 * GB },
            Disk { root: "D:".into(), free: 1000 * GB, total: 2000 * GB },
        ];
        let (s, rx) = store(FakeStatus { disks, ..Default::default() }, Arc::new(FakeFetch::offline()));
        s.get(StatusKind::Disk, true);
        ready(&rx);
        assert_eq!(
            titles(s.get(StatusKind::Disk, true)),
            vec!["C: 120 GB free of 476 GB", "D: 1000 GB free of 2000 GB"]
        );
    }

    #[test]
    fn offline_ip_never_fetches() {
        let fetch = Arc::new(FakeFetch::new(Ok(b"203.0.113.9".to_vec())));
        let src = FakeStatus { ips: vec!["192.168.1.20".into()], ..Default::default() };
        let (s, rx) = store(src, fetch.clone());
        s.get(StatusKind::Ip, false);
        ready(&rx);
        assert!(fetch.urls().is_empty());
        assert_eq!(titles(s.get(StatusKind::Ip, false)), vec!["192.168.1.20"]);
    }

    #[test]
    fn public_ip_is_fetched_when_online() {
        let fetch = Arc::new(FakeFetch::offline().respond(PUBLIC_IP_URL, Ok(b"203.0.113.9\n".to_vec())));
        let src = FakeStatus { ips: vec!["192.168.1.20".into()], ..Default::default() };
        let (s, rx) = store(src, fetch.clone());
        // Local lines first, the public one is only a placeholder while the pause runs.
        s.get(StatusKind::Ip, true);
        ready(&rx);
        let StatusValue::Lines(l) = s.get(StatusKind::Ip, true) else { panic!() };
        assert_eq!(l[0].0, "192.168.1.20");
        assert_eq!(l[1].0, "Public IP: checking…");
        assert!(fetch.urls().is_empty());
        // The timer re-asks after the pause; then the address is fetched.
        ready(&rx);
        s.get(StatusKind::Ip, true);
        ready(&rx);
        let StatusValue::Lines(l) = s.get(StatusKind::Ip, true) else { panic!() };
        assert_eq!(l[1], ("Public IP 203.0.113.9".to_string(), "203.0.113.9".to_string()));
        assert_eq!(fetch.urls().len(), 1);
    }

    #[test]
    fn passing_through_ip_while_typing_never_fetches() {
        let fetch = Arc::new(FakeFetch::new(Ok(b"203.0.113.9".to_vec())));
        let (s, rx) = store(FakeStatus { ips: vec!["10.0.0.2".into()], ..Default::default() }, fetch.clone());
        s.get(StatusKind::Ip, true);
        ready(&rx);
        s.leave_ip(); // "ipc": the text moved on
        std::thread::sleep(IP_PAUSE + Duration::from_millis(150));
        assert!(rx.try_recv().is_err(), "no timer wake-up after leaving");
        s.get(StatusKind::Ip, true); // "ip" typed again: a fresh pause starts
        s.leave_ip();
        std::thread::sleep(IP_PAUSE + Duration::from_millis(150));
        assert!(fetch.urls().is_empty());
    }

    #[test]
    fn public_ip_failure_says_unavailable() {
        let fetch = Arc::new(FakeFetch::new(Err(FetchError::Offline)));
        let (s, rx) = store(FakeStatus { ips: vec!["10.0.0.2".into()], ..Default::default() }, fetch);
        s.get(StatusKind::Ip, true);
        ready(&rx);
        ready(&rx);
        s.get(StatusKind::Ip, true);
        ready(&rx);
        let StatusValue::Lines(l) = s.get(StatusKind::Ip, true) else { panic!() };
        assert_eq!(l[1], ("Public IP unavailable".to_string(), String::new()));
    }

    #[test]
    fn one_sample_at_a_time() {
        let src = Arc::new(FakeStatus { memory: Memory { used: GB, total: 2 * GB }, ..Default::default() });
        let (on_ready, rx) = channel();
        let s = StatusStore::new(src.clone(), Arc::new(FakeFetch::offline()), on_ready, Duration::ZERO);
        assert_eq!(s.get(StatusKind::Ram, true), StatusValue::Pending);
        assert_eq!(s.get(StatusKind::Ram, true), StatusValue::Pending);
        ready(&rx);
        assert!(rx.recv_timeout(Duration::from_millis(300)).is_err(), "on_ready exactly once");
        assert_eq!(src.memory_reads.load(Ordering::SeqCst), 1);
    }
}
