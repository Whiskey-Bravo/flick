//! HTTPS GET for quick answers. The only place Flick talks to the network.

use std::time::{Duration, Instant};

/// Why a fetch did not produce a body.
#[derive(Debug, Clone, PartialEq)]
pub enum FetchError {
    /// No connection: name not resolved, connection refused or reset, timeout.
    Offline,
    /// The server answered with something other than 200.
    Status(u16),
    /// Anything else (bad URL, TLS failure, body too large, ...).
    Other(String),
}

/// How long one fetch may take, all steps included. A job of several requests
/// (weather: geocode, then forecast) shares one such budget.
pub const TIMEOUT: Duration = Duration::from_secs(5);

/// Downloads a URL. Implementations block; callers run them on a background thread.
pub trait Fetch: Send + Sync {
    /// Downloads `url`, giving up (as offline) at `deadline`.
    fn get_until(&self, url: &str, deadline: Instant) -> Result<Vec<u8>, FetchError>;

    /// Downloads `url` within [`TIMEOUT`] from now.
    fn get(&self, url: &str) -> Result<Vec<u8>, FetchError> {
        self.get_until(url, Instant::now() + TIMEOUT)
    }
}

/// HTTPS GET through Windows' own WinHTTP stack: system proxy settings, every step
/// capped by the deadline, User-Agent `Flick`, no cookies, no credentials, 200 only,
/// body ≤ 2 MB.
pub struct WinHttp;

impl Fetch for WinHttp {
    fn get_until(&self, url: &str, deadline: Instant) -> Result<Vec<u8>, FetchError> {
        let (host, port, path) = split_https(url)?;
        winhttp::get(&host, port, &path, deadline)
    }
}

/// Splits `https://host[:port]/path?query` into its parts. Anything else
/// (other schemes, user info, IP-v6 literals, odd characters) is refused.
fn split_https(url: &str) -> Result<(String, u16, String), FetchError> {
    let bad = || FetchError::Other(format!("refused URL: {url}"));
    let rest = url.strip_prefix("https://").ok_or_else(bad)?;
    let rest = rest.split('#').next().unwrap_or("");
    let cut = rest.find(['/', '?']).unwrap_or(rest.len());
    let (authority, path) = rest.split_at(cut);
    let (host, port) = match authority.split_once(':') {
        Some((h, p)) => (h, p.parse::<u16>().ok().filter(|p| *p != 0).ok_or_else(bad)?),
        None => (authority, 443),
    };
    let host_ok = !host.is_empty()
        && host.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'.');
    let path_ok = path.bytes().all(|b| b.is_ascii_graphic());
    if !host_ok || !path_ok {
        return Err(bad());
    }
    let path = if path.starts_with('/') { path.to_string() } else { format!("/{path}") };
    Ok((host.to_string(), port, path))
}

mod winhttp {
    use std::ffi::c_void;
    use std::time::Instant;

    use windows::core::{w, Error, HSTRING, HRESULT, PCWSTR};
    use windows::Win32::Networking::WinHttp::*;

    use super::FetchError;

    const MAX_BODY: usize = 2 * 1024 * 1024;

    /// A WinHTTP handle, closed on drop.
    struct Handle(*mut c_void);

    impl Handle {
        fn new(raw: *mut c_void) -> Result<Self, FetchError> {
            if raw.is_null() {
                Err(map(Error::from_win32()))
            } else {
                Ok(Handle(raw))
            }
        }
    }

    impl Drop for Handle {
        fn drop(&mut self) {
            // SAFETY: a non-null handle from WinHttpOpen/Connect/OpenRequest, closed once.
            unsafe {
                let _ = WinHttpCloseHandle(self.0);
            }
        }
    }

    fn map(e: Error) -> FetchError {
        let offline = [
            ERROR_WINHTTP_NAME_NOT_RESOLVED,
            ERROR_WINHTTP_CANNOT_CONNECT,
            ERROR_WINHTTP_CONNECTION_ERROR,
            ERROR_WINHTTP_TIMEOUT,
        ];
        if offline.iter().any(|c| e.code() == HRESULT::from_win32(*c)) {
            FetchError::Offline
        } else {
            FetchError::Other(e.message())
        }
    }

    /// Milliseconds left until `deadline`, or None when it has passed.
    pub(super) fn remaining_ms(deadline: Instant, now: Instant) -> Option<i32> {
        let left = deadline.saturating_duration_since(now).as_millis();
        (left > 0).then(|| i32::try_from(left).unwrap_or(i32::MAX))
    }

    /// Caps every timeout of the next blocking call at the time left; past the deadline
    /// the fetch counts as offline.
    fn arm(request: &Handle, deadline: Instant) -> Result<(), FetchError> {
        let ms = remaining_ms(deadline, Instant::now()).ok_or(FetchError::Offline)?;
        // SAFETY: a live request handle.
        unsafe { WinHttpSetTimeouts(request.0, ms, ms, ms, ms) }.map_err(map)
    }

    fn set_u32(handle: &Handle, option: u32, value: u32) -> Result<(), FetchError> {
        // SAFETY: a live handle and a 4-byte buffer, as these options expect.
        unsafe { WinHttpSetOption(Some(handle.0), option, Some(&value.to_ne_bytes())) }.map_err(map)
    }

    /// The whole fetch (resolve, connect, send, headers, body) must finish by `deadline`.
    pub(super) fn get(host: &str, port: u16, path: &str, deadline: Instant) -> Result<Vec<u8>, FetchError> {
        let ms = remaining_ms(deadline, Instant::now()).ok_or(FetchError::Offline)?;
        // Declared in this order so they drop request → connection → session.
        // SAFETY (all calls below): every handle passed is live and owned by a guard;
        // string arguments outlive the calls; out-pointers point at locals of the right size.
        let session = Handle::new(unsafe {
            WinHttpOpen(w!("Flick"), WINHTTP_ACCESS_TYPE_AUTOMATIC_PROXY, PCWSTR::null(), PCWSTR::null(), 0)
        })?;
        unsafe { WinHttpSetTimeouts(session.0, ms, ms, ms, ms) }
            .map_err(map)?;
        let host = HSTRING::from(host);
        let connection = Handle::new(unsafe { WinHttpConnect(session.0, &host, port, 0) })?;
        let path = HSTRING::from(path);
        let request = Handle::new(unsafe {
            WinHttpOpenRequest(
                connection.0,
                w!("GET"),
                &path,
                PCWSTR::null(),
                PCWSTR::null(),
                std::ptr::null(),
                WINHTTP_FLAG_SECURE,
            )
        })?;
        set_u32(&request, WINHTTP_OPTION_DISABLE_FEATURE, WINHTTP_DISABLE_COOKIES | WINHTTP_DISABLE_AUTHENTICATION)?;
        set_u32(&request, WINHTTP_OPTION_AUTOLOGON_POLICY, WINHTTP_AUTOLOGON_SECURITY_LEVEL_HIGH)?;
        set_u32(&request, WINHTTP_OPTION_REDIRECT_POLICY, WINHTTP_OPTION_REDIRECT_POLICY_DISALLOW_HTTPS_TO_HTTP)?;

        arm(&request, deadline)?;
        unsafe { WinHttpSendRequest(request.0, None, None, 0, 0, 0) }.map_err(map)?;
        arm(&request, deadline)?;
        unsafe { WinHttpReceiveResponse(request.0, std::ptr::null_mut()) }.map_err(map)?;

        let mut status: u32 = 0;
        let mut len = std::mem::size_of::<u32>() as u32;
        unsafe {
            WinHttpQueryHeaders(
                request.0,
                WINHTTP_QUERY_STATUS_CODE | WINHTTP_QUERY_FLAG_NUMBER,
                PCWSTR::null(),
                Some(&mut status as *mut u32 as *mut c_void),
                &mut len,
                std::ptr::null_mut(),
            )
        }
        .map_err(map)?;
        if status != 200 {
            return Err(FetchError::Status(u16::try_from(status).unwrap_or(u16::MAX)));
        }

        let mut body = Vec::new();
        let mut buf = vec![0u8; 64 * 1024];
        loop {
            arm(&request, deadline)?;
            let mut read: u32 = 0;
            unsafe { WinHttpReadData(request.0, buf.as_mut_ptr() as *mut c_void, buf.len() as u32, &mut read) }
                .map_err(map)?;
            if read == 0 {
                break;
            }
            if body.len() + read as usize > MAX_BODY {
                return Err(FetchError::Other("response larger than 2 MB".into()));
            }
            body.extend_from_slice(&buf[..read as usize]);
        }
        drop(request);
        drop(connection);
        drop(session);
        Ok(body)
    }
}

/// Test fetcher: records requested URLs in order and answers from a script.
#[cfg(test)]
pub(crate) struct FakeFetch {
    urls: std::sync::Mutex<Vec<String>>,
    deadlines: std::sync::Mutex<Vec<Instant>>,
    responses: std::collections::HashMap<String, Result<Vec<u8>, FetchError>>,
    default: Result<Vec<u8>, FetchError>,
    delay: Option<Duration>,
}

#[cfg(test)]
impl FakeFetch {
    /// Answers every URL with `default` unless scripted otherwise.
    pub(crate) fn new(default: Result<Vec<u8>, FetchError>) -> Self {
        FakeFetch {
            urls: std::sync::Mutex::new(Vec::new()),
            deadlines: std::sync::Mutex::new(Vec::new()),
            responses: std::collections::HashMap::new(),
            default,
            delay: None,
        }
    }

    /// Every URL fails with `FetchError::Offline`.
    pub(crate) fn offline() -> Self {
        Self::new(Err(FetchError::Offline))
    }

    /// Answers `url` with `response`.
    pub(crate) fn respond(mut self, url: &str, response: Result<Vec<u8>, FetchError>) -> Self {
        self.responses.insert(url.to_string(), response);
        self
    }

    /// Sleeps this long before answering (after recording the URL). The answer is
    /// given even past the deadline, as from a server that replied just in time.
    pub(crate) fn with_delay(mut self, delay: Duration) -> Self {
        self.delay = Some(delay);
        self
    }

    /// The URLs requested so far, in order.
    pub(crate) fn urls(&self) -> Vec<String> {
        self.urls.lock().unwrap().clone()
    }

    /// The deadline of each request so far, in order.
    pub(crate) fn deadlines(&self) -> Vec<Instant> {
        self.deadlines.lock().unwrap().clone()
    }
}

#[cfg(test)]
impl Fetch for FakeFetch {
    fn get_until(&self, url: &str, deadline: Instant) -> Result<Vec<u8>, FetchError> {
        self.urls.lock().unwrap().push(url.to_string());
        self.deadlines.lock().unwrap().push(deadline);
        if let Some(d) = self.delay {
            std::thread::sleep(d);
        }
        self.responses.get(url).cloned().unwrap_or_else(|| self.default.clone())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_non_https_urls_before_any_request() {
        for url in ["http://www.ecb.europa.eu/", "ftp://x/", "www.ecb.europa.eu", ""] {
            assert!(matches!(WinHttp.get(url), Err(FetchError::Other(_))), "{url}");
        }
    }

    #[test]
    fn rejects_urls_with_credentials_or_no_host() {
        for url in ["https://user:pw@example.com/", "https:///path", "https://"] {
            assert!(matches!(WinHttp.get(url), Err(FetchError::Other(_))), "{url}");
        }
    }

    #[test]
    fn splits_https_urls() {
        assert_eq!(
            split_https("https://www.ecb.europa.eu/stats/eurofxref/eurofxref-daily.xml"),
            Ok(("www.ecb.europa.eu".into(), 443, "/stats/eurofxref/eurofxref-daily.xml".into()))
        );
        assert_eq!(
            split_https("https://api.example.com:8443?name=London#x"),
            Ok(("api.example.com".into(), 8443, "/?name=London".into()))
        );
        assert_eq!(split_https("https://example.com"), Ok(("example.com".into(), 443, "/".into())));
        for bad in ["https://example.com:0/", "https://example.com:x/", "https://[::1]/", "https://a b/", "https://a/b c"] {
            assert!(split_https(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn deadline_counts_down_and_expires() {
        let now = Instant::now();
        let deadline = now + Duration::from_millis(5_000);
        assert_eq!(winhttp::remaining_ms(deadline, now), Some(5_000));
        assert_eq!(winhttp::remaining_ms(deadline, now + Duration::from_millis(4_999)), Some(1));
        assert_eq!(winhttp::remaining_ms(deadline, deadline), None);
        assert_eq!(winhttp::remaining_ms(deadline, deadline + Duration::from_secs(1)), None);
    }

    #[test]
    fn fake_fetch_records_and_scripts() {
        let f = FakeFetch::offline().respond("https://a/", Ok(b"hi".to_vec()));
        assert_eq!(f.get("https://a/"), Ok(b"hi".to_vec()));
        assert_eq!(f.get("https://b/"), Err(FetchError::Offline));
        assert_eq!(f.urls(), vec!["https://a/".to_string(), "https://b/".to_string()]);
        let before = Instant::now();
        let deadline = before + Duration::from_millis(300);
        assert_eq!(f.get_until("https://a/", deadline), Ok(b"hi".to_vec()));
        let d = f.deadlines();
        assert_eq!(d[2], deadline);
        assert!(d[0] > before && d[0] <= before + TIMEOUT, "plain get: TIMEOUT from the call");
    }

    #[test]
    fn a_passed_deadline_is_offline_before_any_request() {
        let past = Instant::now() - Duration::from_millis(1);
        assert_eq!(WinHttp.get_until("https://no-such-host.invalid/", past), Err(FetchError::Offline));
    }

    /// Touches the network: run by hand with `cargo test --lib net -- --ignored`.
    #[test]
    #[ignore]
    fn probe_ecb_daily_file() {
        let body = WinHttp.get(crate::answers::rates::ECB_URL).expect("fetch");
        let rates = crate::answers::rates::parse_ecb(&body).expect("parse");
        println!("ECB rates of {}: {} currencies", rates.date, rates.per_eur.len());
        assert!(rates.per_eur.contains_key("USD"));
    }

    /// Touches the network (a DNS lookup): run by hand with `--ignored`.
    #[test]
    #[ignore]
    fn probe_unknown_host_is_offline() {
        assert_eq!(WinHttp.get("https://no-such-host.invalid/"), Err(FetchError::Offline));
    }

    /// Touches the network: run by hand with `--ignored`.
    #[test]
    #[ignore]
    fn probe_missing_page_is_status() {
        assert_eq!(
            WinHttp.get("https://www.ecb.europa.eu/stats/eurofxref/no-such-file.xml"),
            Err(FetchError::Status(404))
        );
    }
}
