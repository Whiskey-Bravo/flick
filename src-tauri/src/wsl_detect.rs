//! Find the WSL distro to run commands in, from `wsl.exe -l -v`.

use std::path::PathBuf;
use std::time::Duration;

use crate::launch::valid_distro;
use crate::process::{run_hidden, RunError};

const LIST_TIMEOUT: Duration = Duration::from_secs(10);

/// Outcome of [`detect`].
#[derive(Debug, PartialEq)]
pub enum Detection {
    /// WSL answered (or is not installed): the distro to use, if any.
    Found(Option<String>),
    /// `wsl.exe` timed out; the caller keeps its previous state and retries later.
    Failed,
}

/// Decode `wsl.exe` output: UTF-16LE (with or without BOM) as it normally
/// writes to a pipe, else UTF-8.
fn decode(raw: &[u8]) -> String {
    let raw = raw.strip_prefix(&[0xFF, 0xFE]).unwrap_or(raw);
    let first_line = raw.split(|&b| b == b'\n').next().unwrap_or(&[]);
    let utf16 = raw.len() % 2 == 0
        && first_line.len() >= 2
        && first_line.iter().skip(1).step_by(2).all(|&b| b == 0);
    if utf16 {
        let units: Vec<u16> = raw.chunks_exact(2).map(|c| u16::from_le_bytes([c[0], c[1]])).collect();
        String::from_utf16_lossy(&units)
    } else {
        String::from_utf8_lossy(raw).into_owned()
    }
}

/// `(name, is_default)` for each row of `wsl -l -v`. The header (first
/// non-empty line) is skipped; a row needs a name, a state and a numeric
/// version, so the "no installed distributions" message yields nothing.
/// A leading `*` marks the default; names failing `valid_distro` are dropped.
pub fn parse_list(raw: &[u8]) -> Vec<(String, bool)> {
    decode(raw)
        .lines()
        .filter(|l| !l.trim().is_empty())
        .skip(1)
        .filter_map(|line| {
            let line = line.trim();
            let (default, rest) = match line.strip_prefix('*') {
                Some(rest) => (true, rest),
                None => (false, line),
            };
            let tokens: Vec<&str> = rest.split_whitespace().collect();
            let version_ok = tokens.last().is_some_and(|v| v.chars().all(|c| c.is_ascii_digit()));
            (tokens.len() >= 3 && version_ok && valid_distro(tokens[0]))
                .then(|| (tokens[0].to_string(), default))
        })
        .collect()
}

fn is_docker(name: &str) -> bool {
    name.to_ascii_lowercase().starts_with("docker-desktop")
}

/// The default distro, unless it is one of Docker's internal ones; then the
/// first other distro. `None` when only Docker's remain.
pub fn choose(list: &[(String, bool)]) -> Option<String> {
    list.iter()
        .find(|(n, d)| *d && !is_docker(n))
        .or_else(|| list.iter().find(|(n, _)| !is_docker(n)))
        .map(|(n, _)| n.clone())
}

/// Ask `wsl.exe` for its distros.
pub fn detect() -> Detection {
    let root = std::env::var_os("SystemRoot").unwrap_or_else(|| r"C:\Windows".into());
    let exe = PathBuf::from(root).join(r"System32\wsl.exe");
    from_run(run_hidden(&exe, &["-l", "-v"], LIST_TIMEOUT), exe.exists())
}

/// No WSL only when `wsl.exe` is missing, or ran and failed (how it reports
/// "not installed" and "no distros"). A timeout, or a `wsl.exe` that exists
/// but could not be started, is `Failed` and gets retried.
fn from_run(res: Result<Vec<u8>, RunError>, exe_exists: bool) -> Detection {
    match res {
        Ok(out) => Detection::Found(choose(&parse_list(&out))),
        Err(RunError::Exit) => Detection::Found(None),
        Err(RunError::Spawn) if !exe_exists => Detection::Found(None),
        Err(RunError::Spawn | RunError::Timeout) => Detection::Failed,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_a_missing_or_failing_wsl_means_no_wsl() {
        assert!(matches!(from_run(Err(RunError::Exit), true), Detection::Found(None)));
        assert!(matches!(from_run(Err(RunError::Spawn), false), Detection::Found(None)));
        assert!(matches!(from_run(Err(RunError::Spawn), true), Detection::Failed));
        assert!(matches!(from_run(Err(RunError::Timeout), true), Detection::Failed));
    }

    const LIST: &str = "  NAME              STATE           VERSION\r\n* Ubuntu            Stopped         2\r\n  docker-desktop    Stopped         2\r\n  Debian            Running         2\r\n";

    fn utf16(s: &str, bom: bool) -> Vec<u8> {
        let mut out = if bom { vec![0xFF, 0xFE] } else { Vec::new() };
        for u in s.encode_utf16() {
            out.extend_from_slice(&u.to_le_bytes());
        }
        out
    }

    #[test]
    fn parses_utf16_and_utf8() {
        let want = vec![
            ("Ubuntu".to_string(), true),
            ("docker-desktop".into(), false),
            ("Debian".into(), false),
        ];
        assert_eq!(parse_list(&utf16(LIST, true)), want);
        assert_eq!(parse_list(&utf16(LIST, false)), want);
        assert_eq!(parse_list(LIST.as_bytes()), want);
    }

    #[test]
    fn chooses_default() {
        assert_eq!(choose(&parse_list(LIST.as_bytes())), Some("Ubuntu".into()));
    }

    #[test]
    fn docker_default_falls_back_to_first_other() {
        let l = vec![("docker-desktop".into(), true), ("Ubuntu".into(), false)];
        assert_eq!(choose(&l), Some("Ubuntu".into()));
    }

    #[test]
    fn only_docker_is_no_wsl() {
        let l = vec![("docker-desktop".into(), true), ("docker-desktop-data".into(), false)];
        assert_eq!(choose(&l), None);
    }

    #[test]
    fn no_distros_message_parses_to_empty() {
        let msg = "Windows Subsystem for Linux has no installed distributions.\r\nUse 'wsl.exe --list --online' to list available distributions\r\n";
        assert!(parse_list(&utf16(msg, true)).is_empty());
    }
}
