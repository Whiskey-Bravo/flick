//! Argument quoting, placeholder binding and the command lines that run a
//! command in cmd.exe or WSL. Pure: nothing here starts a process.

// ---- paths -----------------------------------------------------------------

pub(crate) fn windows_dir() -> String {
    let root = std::env::var("SystemRoot").unwrap_or_else(|_| r"C:\Windows".into());
    root.trim_end_matches('\\').to_string()
}

/// `%SystemRoot%\System32\<exe>`: an absolute path, so nothing on `PATH` or
/// next to Flick is run in its place.
pub(crate) fn system32(exe: &str) -> String {
    format!(r"{}\System32\{exe}", windows_dir())
}

// ---- quoting ---------------------------------------------------------------

const UNSAFE_ARGUMENT: &str = "argument contains characters that cannot be passed safely";
/// [`check_windows_value`]'s refusal of the characters a user can type.
const QUOTE_OR_PERCENT: &str = r#"cannot pass text containing " or % to this command"#;

/// `arg` as one argument for `CommandLineToArgvW` (and the MSVC runtime), for
/// a program started directly, with no cmd.exe in between: plain when
/// possible, else `"..."` with embedded `"` as `\"` and backslashes doubled
/// before any `"`.
pub fn quote_argv(arg: &str) -> String {
    if !arg.is_empty() && !arg.chars().any(|c| matches!(c, ' ' | '\t' | '\n' | '\x0b' | '"')) {
        return arg.to_string();
    }
    let mut out = String::from("\"");
    let mut slashes = 0usize;
    for c in arg.chars() {
        match c {
            '\\' => slashes += 1,
            '"' => {
                out.extend(std::iter::repeat_n('\\', slashes * 2 + 1));
                out.push('"');
                slashes = 0;
            }
            _ => {
                out.extend(std::iter::repeat_n('\\', slashes));
                out.push(c);
                slashes = 0;
            }
        }
    }
    out.extend(std::iter::repeat_n('\\', slashes * 2));
    out.push('"');
    out
}

/// `arg` as one POSIX shell word: single-quoted, each `'` as `'\''`.
pub fn quote_bash(arg: &str) -> String {
    format!("'{}'", arg.replace('\'', r"'\''"))
}

/// `C:\Users\a b` -> `/mnt/c/Users/a b`. Spaces and other characters are kept
/// (quoting is the caller's job). None for UNC, `\\?\`, relative and
/// drive-relative (`C:x`) paths.
pub fn to_wsl_path(windows_path: &str) -> Option<String> {
    let mut chars = windows_path.chars();
    let drive = chars.next().filter(char::is_ascii_alphabetic)?;
    if chars.next() != Some(':') {
        return None;
    }
    let rest = chars.as_str();
    if !(rest.is_empty() || rest.starts_with(['\\', '/'])) {
        return None;
    }
    Some(format!("/mnt/{}{}", drive.to_ascii_lowercase(), rest.replace('\\', "/")))
}

// ---- binding ---------------------------------------------------------------
//
// Values never become shell text. A template's `{x}` is rewritten to a
// reference to the variable `FLICK_X`, and the value travels in that
// variable: an environment variable for cmd.exe, an assignment at the start
// of the script for bash.

/// `FLICK_` + the upper-cased placeholder name (ASCII letters only).
pub(crate) fn variable(name: &str) -> Result<String, String> {
    if name.is_empty() || !name.chars().all(|c| c.is_ascii_alphabetic()) {
        return Err(format!("invalid placeholder name {name:?}"));
    }
    Ok(format!("FLICK_{}", name.to_ascii_uppercase()))
}

/// `{name}` at the start of `rest` when `name` is one of `names`: (name,
/// length of the placeholder in chars).
fn placeholder<'a>(rest: &[char], names: &[&'a str]) -> Option<(&'a str, usize)> {
    if rest.first() != Some(&'{') {
        return None;
    }
    let close = rest.iter().position(|&c| c == '}')?;
    let name: String = rest[1..close].iter().collect();
    names.iter().find(|n| **n == name).map(|&n| (n, close + 1))
}

/// A value bound for cmd.exe ([`bind_windows`]) must not contain `"`, `%`, a
/// line break or NUL:
/// - `"`: the target program's own argv parsing would split there, and a
///   re-parse (see [`bind_windows`]) would see it end the quotes;
/// - `%`: constructs that re-run percent expansion after delayed expansion
///   (`call`, `for /f`, internal commands in a pipe) would expand it, and
///   dynamic variables such as `%CMDCMDLINE:~-1%` can rebuild a `"`;
/// - line breaks and NUL cannot travel in a command line or environment.
pub fn check_windows_value(v: &str) -> Result<(), String> {
    if v.contains(['"', '%']) {
        return Err(QUOTE_OR_PERCENT.into());
    }
    if v.contains(['\n', '\r', '\0']) {
        return Err(UNSAFE_ARGUMENT.into());
    }
    Ok(())
}

/// The environment value for a [`bind_windows`] reference: trailing
/// backslashes doubled. The reference is usually followed by a `"` (the
/// quotes `bind_windows` adds or the author's own), and the program's
/// `CommandLineToArgvW` would read `\"` as a literal quote; `\\"` is one
/// backslash and the closing quote. The launcher applies this; callers pass
/// plain values. It is right only for a program started directly with the
/// reference followed by `"`.
pub(crate) fn windows_env_value(v: &str) -> String {
    let trailing = v.len() - v.trim_end_matches('\\').len();
    format!("{v}{}", "\\".repeat(trailing))
}

/// Rewrite each `{x}` (for `x` in `names`) of a cmd.exe command template to
/// `!FLICK_X!`, expanded by cmd.exe's delayed expansion (run the result with
/// `command_line(.., delayed_expansion = true, ..)` and the values in the
/// environment). Delayed expansion happens after the line is parsed, so in a
/// plain command a value is never scanned for `&|<>^%"`:
/// - outside quotes the reference is quoted, `"!FLICK_X!"`, so a value with
///   spaces stays one argument;
/// - inside the author's quotes (`"{x}"` or `"a {x} b"`) it is bare.
///
/// Refused when at least one placeholder is bound: a template with `!` of its
/// own (delayed expansion would eat it) or with `^` (a caret escape is read
/// differently when a construct parses the line a second time). Unknown
/// `{...}` stay as they are; values are checked by [`check_windows_value`].
///
/// Limits:
/// - `call`, `for /f ... in ('...')` and a pipe with an internal command or a
///   `( ... )` block on either side parse the expanded value again as
///   cmd.exe text. Safety there rests on refusing `"` and `%` in values and
///   `^` and `!` in templates; the value is not round-trip exact in those
///   constructs (a `!` in it may be expanded again, for example).
/// - a `.bat`/`.cmd` target re-parses its arguments too.
/// - a value ending in `\` used in the middle of a longer quoted string
///   (`"a {x} b"`) arrives with its trailing backslashes doubled (see
///   `windows_env_value`).
/// - a value beginning with `-` or `/` is read as an option by the target
///   program: argument injection, not execution. Put `--` before the
///   placeholder where the program supports it.
/// - `set /a` with a placeholder can assign environment variables for the
///   rest of the line.
pub fn bind_windows(template: &str, names: &[&str]) -> Result<String, String> {
    let chars: Vec<char> = template.chars().collect();
    let mut out = String::new();
    let mut quoted = false; // cmd.exe's quote state
    let mut bound = false;
    let mut i = 0;
    while i < chars.len() {
        if let Some((name, len)) = placeholder(&chars[i..], names) {
            let var = variable(name)?;
            if quoted {
                out.push_str(&format!("!{var}!"));
            } else {
                open_quote(&mut out);
                out.push_str(&format!("!{var}!\""));
            }
            bound = true;
            i += len;
            continue;
        }
        if chars[i] == '"' {
            quoted = !quoted;
        }
        out.push(chars[i]);
        i += 1;
    }
    if bound && template.contains('!') {
        return Err("this command cannot use ! together with a placeholder".into());
    }
    if bound && template.contains('^') {
        return Err("this command cannot use ^ together with a placeholder".into());
    }
    Ok(out)
}

/// Opens the quotes [`bind_windows`] puts around an unquoted reference. Right
/// after a `\` (as in `C:\notes\{x}`) a `"` would be read by the program's
/// `CommandLineToArgvW` as a literal quote, so the quotes start at the
/// beginning of the word instead (`"C:\notes\!FLICK_X!"`). When that word has
/// quotes of its own the backslashes are doubled instead: `\\"` is one
/// backslash and an opening quote.
fn open_quote(out: &mut String) {
    let slashes = out.len() - out.trim_end_matches('\\').len();
    if slashes == 0 {
        out.push('"');
        return;
    }
    let delimiter = out.rfind([' ', '\t', '&', '|', '<', '>', '(', ')', '"']);
    match delimiter {
        Some(at) if out.as_bytes()[at] == b'"' => out.push_str(&format!("{}\"", "\\".repeat(slashes))),
        Some(at) => out.insert(at + 1, '"'),
        None => out.insert(0, '"'),
    }
}

/// The refusal of a placeholder used together with `construct`.
fn cannot_use(construct: &str) -> String {
    format!("this command cannot use {construct} together with a placeholder")
}

/// `word` in `t` as a command word: at the start or after a character that
/// cannot be part of a word, and followed by a blank or the end.
fn has_word(t: &str, word: &str) -> bool {
    let bytes = t.as_bytes();
    let part_of_word = |b: u8| b.is_ascii_alphanumeric() || b == b'_' || b == b'-';
    t.match_indices(word).any(|(at, _)| {
        let end = at + word.len();
        (at == 0 || !part_of_word(bytes[at - 1])) && (end == bytes.len() || matches!(bytes[end], b' ' | b'\t'))
    })
}

/// `word` in `t` as a command word (see [`has_word`]) with `option` among the
/// options right after it, separated by any run of blanks: `test  -v`,
/// `wait -n -p`.
fn has_option(t: &str, word: &str, option: &str) -> bool {
    let bytes = t.as_bytes();
    let part_of_word = |b: u8| b.is_ascii_alphanumeric() || b == b'_' || b == b'-';
    t.match_indices(word).any(|(at, _)| {
        if at > 0 && part_of_word(bytes[at - 1]) {
            return false;
        }
        let rest = &t[at + word.len()..];
        if !rest.starts_with([' ', '\t']) {
            return false;
        }
        rest.split([' ', '\t']).filter(|w| !w.is_empty()).take_while(|w| w.starts_with('-')).any(|w| w == option)
    })
}

/// The first construct of `template` where bash would evaluate a value as an
/// arithmetic expression or a variable name (whose array subscript is
/// arithmetic): `((`, `$[`, `[[`, an array subscript holding a placeholder
/// ("an array index"), the builtins `let`, `declare`, `typeset`, `local`,
/// `unset`, `read`, `mapfile`, `readarray`, `getopts`, and `test -v` / `[ -v` /
/// `printf -v` / `wait -p`. The builtins are matched as words anywhere, quoted text
/// included (`echo please read {x}` is refused too): conservative on purpose.
fn arithmetic_context(template: &str, names: &[&str]) -> Option<&'static str> {
    let texts = ["((", "$[", "[["];
    if let Some(t) = texts.into_iter().find(|s| template.contains(s)) {
        return Some(t);
    }
    let options = [("[", "-v", "[ -v"), ("test", "-v", "test -v"), ("printf", "-v", "printf -v"), ("wait", "-p", "wait -p")];
    if let Some((_, _, t)) = options.into_iter().find(|(w, o, _)| has_option(template, w, o)) {
        return Some(t);
    }
    let words = ["let", "declare", "typeset", "local", "unset", "read", "mapfile", "readarray", "getopts"];
    if let Some(w) = words.into_iter().find(|w| has_word(template, w)) {
        return Some(w);
    }
    // name[ ... {x} ... ]
    let chars: Vec<char> = template.chars().collect();
    let index = (1..chars.len()).any(|j| {
        let prev = chars[j - 1];
        if chars[j] != '[' || !(prev.is_ascii_alphanumeric() || prev == '_') {
            return false;
        }
        let end = chars[j..].iter().position(|&c| c == ']').map_or(chars.len(), |p| j + p);
        (j..end).any(|k| placeholder(&chars[k..], names).is_some())
    });
    index.then_some("an array index")
}

/// A bash script that runs `template` with each `{x}` bound to its value:
/// `FLICK_X=<quote_bash(value)>; ...; <template with references>`. The value
/// appears only in that leading assignment, in plain context; the template
/// gets a reference:
/// - `"{x}"` -> `"${FLICK_X}"` (the author's quotes kept);
/// - `'{x}'` -> `"${FLICK_X}"` (single quotes would not expand it);
/// - inside a longer single-quoted string -> an error;
/// - inside a double-quoted string -> `${FLICK_X}`;
/// - anywhere else -> `"${FLICK_X}"`.
///
/// The quote tracker follows `'...'`, `"..."`, `$( )` / `( )` nesting and
/// `${...}` (nested ones too). Guarantee: in the contexts this function accepts, the value is
/// never parsed as code; if the tracker misjudges a context the worst outcome
/// is a literal `${FLICK_X}` or the value being word-split or globbed.
///
/// Refused when at least one placeholder is bound, because bash would
/// evaluate the value as arithmetic (where `a[$(cmd)]` runs `cmd`): `((`,
/// `$((`, `$[`, `[[`, `let`, `declare`, `typeset`, `local`, array subscripts
/// holding a placeholder, a placeholder inside `${...}` (substring offsets
/// are arithmetic), and builtins that take a variable name (`unset`, `read`,
/// `mapfile`, `readarray`, `test -v`, `[ -v`, `printf -v`, `getopts`, `wait -p`). Plain `[ ... ]`
/// tests stay allowed. Unknown `{...}` stay as they are. A value with NUL is
/// refused. A value beginning with `-` is read as an option by the target
/// program (argument injection, not execution); put `--` before the
/// placeholder where the program supports it.
pub fn bind_bash(template: &str, values: &[(&str, &str)]) -> Result<String, String> {
    #[derive(Clone, Copy, PartialEq)]
    enum State {
        Plain,
        Single,
        Double,
        Param,
    }
    fn set(stack: &mut [State], s: State) {
        if let Some(t) = stack.last_mut() {
            *t = s;
        }
    }
    let mut script = String::new();
    for (name, value) in values {
        if value.contains('\0') {
            return Err(UNSAFE_ARGUMENT.into());
        }
        script.push_str(&format!("{}={}; ", variable(name)?, quote_bash(value)));
    }
    let names: Vec<&str> = values.iter().map(|(n, _)| *n).collect();
    let chars: Vec<char> = template.chars().collect();
    let bound = (0..chars.len()).any(|i| placeholder(&chars[i..], &names).is_some());
    if let Some(construct) = arithmetic_context(template, &names).filter(|_| bound) {
        return Err(cannot_use(construct));
    }
    let mut out = String::new();
    let mut stack = vec![State::Plain];
    let mut opened_at = usize::MAX; // out.len() right after the last opening single quote
    let mut i = 0;
    while i < chars.len() {
        let top = *stack.last().unwrap_or(&State::Plain);
        if let Some((name, len)) = placeholder(&chars[i..], &names) {
            let var = variable(name)?;
            match top {
                State::Plain => out.push_str(&format!("\"${{{var}}}\"")),
                State::Double => out.push_str(&format!("${{{var}}}")),
                State::Param => return Err(format!("put {{{name}}} outside ${{…}} in this command")),
                State::Single => {
                    if opened_at != out.len() || chars.get(i + len) != Some(&'\'') {
                        return Err(format!("put {{{name}}} outside single quotes in this command"));
                    }
                    out.pop(); // the author's opening single quote
                    out.push_str(&format!("\"${{{var}}}\""));
                    i += 1; // and the closing one
                    set(&mut stack, State::Plain);
                }
            }
            i += len;
            continue;
        }
        let c = chars[i];
        let next = chars.get(i + 1).copied();
        out.push(c);
        i += 1;
        match (top, c) {
            (State::Plain | State::Double, '\\') => {
                if let Some(n) = next {
                    out.push(n);
                    i += 1;
                }
            }
            (State::Plain | State::Double, '$') if next == Some('(') => {
                out.push('(');
                i += 1;
                stack.push(State::Plain);
            }
            (State::Plain | State::Double | State::Param, '$')
                if next == Some('{') && placeholder(&chars[i..], &names).is_none() =>
            {
                out.push('{');
                i += 1;
                stack.push(State::Param);
            }
            (State::Plain, '(') => stack.push(State::Plain),
            (State::Plain, ')') if stack.len() > 1 => {
                stack.pop();
            }
            (State::Plain, '\'') => {
                set(&mut stack, State::Single);
                opened_at = out.len();
            }
            (State::Plain, '"') => set(&mut stack, State::Double),
            (State::Single, '\'') | (State::Double, '"') => set(&mut stack, State::Plain),
            (State::Param, '}') if stack.len() > 1 => {
                stack.pop();
            }
            _ => {}
        }
    }
    script.push_str(&out);
    Ok(script)
}

// ---- command lines ---------------------------------------------------------

#[derive(Clone, Debug, PartialEq)]
pub enum Shell {
    Windows,
    Wsl { distro: String },
}

#[derive(Debug, PartialEq)]
pub struct CommandLine {
    pub program: String,
    pub params: String,
    pub hidden: bool,
}

/// WSL distro names that can go on a command line unquoted: ASCII letters,
/// digits, `.`, `_` and `-`, not empty and not starting with `-`.
pub fn valid_distro(name: &str) -> bool {
    !name.is_empty()
        && !name.starts_with('-')
        && name.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
}

/// What to start for `command` in `shell`. Pure apart from reading
/// `%SystemRoot%`. Hidden unless `keep_open`; a visible console started
/// directly opens in the user's default terminal app. The distro name must
/// pass [`valid_distro`] (the caller checks).
///
/// cmd.exe gets `/D` (no AutoRun), `/V:ON` when `delayed_expansion` (for
/// [`bind_windows`] references) else `/V:OFF` (no `!` expansion whatever the
/// registry says), and `/S` with the command in one extra pair of quotes, so
/// cmd.exe removes exactly that pair and parses `command` as written.
///
/// WSL ignores `delayed_expansion` and gets `--exec bash -lic <script>`:
/// `--exec` runs bash itself rather than through the distro's login shell,
/// and the script is one `CommandLineToArgvW` argument, which is how wsl.exe
/// builds the Linux argv (checked on a real machine by
/// `quoting_survives_real_shells`). To keep the window open the script is
/// `eval '<command>'; exec bash`, so a command ending in `&`, `;` or a `#`
/// comment still reaches `exec bash`; a command that calls `exit` or runs
/// under `set -e` and fails can still close the window.
///
/// With `home` the WSL parameters start with `--cd ~` (wsl.exe's own option;
/// the `~` is literal, no shell reads it), so the command starts in the Linux
/// home; without it wsl.exe starts in its own working directory, translated.
/// cmd.exe ignores `home`: its directory is an API parameter.
pub fn command_line(shell: &Shell, command: &str, keep_open: bool, delayed_expansion: bool, home: bool) -> CommandLine {
    let (program, params) = match shell {
        Shell::Windows => {
            let switch = if keep_open { "/K" } else { "/C" };
            let v = if delayed_expansion { "ON" } else { "OFF" };
            (system32("cmd.exe"), format!("/D /V:{v} /S {switch} \"{command}\""))
        }
        Shell::Wsl { distro } => {
            let script = if keep_open {
                format!("eval {}; exec bash", quote_bash(command))
            } else {
                command.to_string()
            };
            let cd = if home { "--cd ~ " } else { "" };
            (system32("wsl.exe"), format!("{cd}-d {distro} --exec bash -lic {}", quote_argv(&script)))
        }
    };
    CommandLine { program, params, hidden: !keep_open }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::windows::process::CommandExt;
    use std::path::Path;
    use std::process::Command;
    use windows::core::PCWSTR;

    fn wide(s: &str) -> Vec<u16> {
        s.encode_utf16().chain(std::iter::once(0)).collect()
    }

    /// The real `CommandLineToArgvW`, minus the program name.
    fn argv(args: &str) -> Vec<String> {
        use windows::Win32::Foundation::{LocalFree, HLOCAL};
        use windows::Win32::UI::Shell::CommandLineToArgvW;
        let line = wide(&format!("prog.exe {args}"));
        let mut n = 0i32;
        // SAFETY: `line` is NUL-terminated; the returned block is freed once.
        unsafe {
            let list = CommandLineToArgvW(PCWSTR(line.as_ptr()), &mut n);
            assert!(!list.is_null());
            let out = (1..n as usize).map(|i| (*list.add(i)).to_string().unwrap()).collect();
            LocalFree(Some(HLOCAL(list as _)));
            out
        }
    }

    // ---- quote_argv / quote_bash / paths ---------------------------------

    #[test]
    fn quote_argv_round_trips_through_command_line_to_argv() {
        assert_eq!(quote_argv("plain"), "plain");
        assert_eq!(quote_argv("a b"), "\"a b\"");
        assert_eq!(quote_argv(""), "\"\"");
        assert_eq!(quote_argv(r#"say "hi""#), r#""say \"hi\"""#);
        assert_eq!(quote_argv(r"a b\"), r#""a b\\""#);
        assert_eq!(quote_argv("a&b%PATH%^"), "a&b%PATH%^"); // no cmd.exe involved
        for arg in [
            "plain", "a b", "", r#"""#, r#"say "hi""#, r"a b\", r"trailing\", r#"a\"b"#, r#"a\\"b"#,
            r"C:\dir with space\", "a  b", "tab\there", "line\nbreak", "%COMSPEC% & | ^", "Résumé", "it's",
            r#"bash 'x "y"' \z"#,
        ] {
            let got = argv(&format!("before {} after", quote_argv(arg)));
            assert_eq!(got, vec!["before", arg, "after"], "{arg:?}");
        }
    }

    #[test]
    fn quote_bash_is_single_quoted() {
        assert_eq!(quote_bash("ls -la"), "'ls -la'");
        assert_eq!(quote_bash("it's"), "'it'\\''s'");
        assert_eq!(quote_bash("a; rm -rf ~ $(x)"), "'a; rm -rf ~ $(x)'");
        assert_eq!(quote_bash(""), "''");
    }

    #[test]
    fn wsl_path_conversion() {
        assert_eq!(to_wsl_path(r"C:\Users\a b\Résumé").as_deref(), Some("/mnt/c/Users/a b/Résumé"));
        assert_eq!(to_wsl_path(r"\\server\share"), None);
        assert_eq!(to_wsl_path(r"d:\").as_deref(), Some("/mnt/d/"));
        assert_eq!(to_wsl_path("E:/x/y").as_deref(), Some("/mnt/e/x/y"));
        assert_eq!(to_wsl_path(r"C:"), Some("/mnt/c".to_string()));
        assert_eq!(to_wsl_path(r"relative\path"), None);
        assert_eq!(to_wsl_path(r"C:relative"), None);
        assert_eq!(to_wsl_path(r"\rooted"), None);
        assert_eq!(to_wsl_path(r"\\?\C:\x"), None);
        assert_eq!(to_wsl_path(""), None);
    }

    // ---- bind_windows ----------------------------------------------------

    #[test]
    fn bind_windows_rewrites_to_delayed_references() {
        let file = &["file"];
        assert_eq!(bind_windows(r#"prog "{file}""#, file).unwrap(), r#"prog "!FLICK_FILE!""#);
        assert_eq!(bind_windows("prog {file}", file).unwrap(), r#"prog "!FLICK_FILE!""#);
        assert_eq!(bind_windows(r#"echo "x {a} y""#, &["a"]).unwrap(), r#"echo "x !FLICK_A! y""#);
        assert_eq!(
            bind_windows("p {a} {b} {a} {zz}", &["a", "b"]).unwrap(),
            r#"p "!FLICK_A!" "!FLICK_B!" "!FLICK_A!" {zz}"#
        );
    }

    #[test]
    fn bind_windows_after_a_backslash_keeps_the_path_whole() {
        let name = &["name"];
        assert_eq!(bind_windows(r"notepad C:\notes\{name}.txt", name).unwrap(), r#"notepad "C:\notes\!FLICK_NAME!".txt"#);
        assert_eq!(bind_windows(r"x&C:\a\\{name}", name).unwrap(), r#"x&"C:\a\\!FLICK_NAME!""#);
        assert_eq!(bind_windows(r"\{name}", name).unwrap(), r#""\!FLICK_NAME!""#);
        // A word with quotes of its own: the backslashes are doubled.
        assert_eq!(bind_windows(r#"p "a b"\{name}"#, name).unwrap(), r#"p "a b"\\"!FLICK_NAME!""#);
        // What the program sees once cmd.exe has put the value in.
        for (template, want) in [
            (r"notepad C:\notes\{name}.txt", r"C:\notes\My Notes.txt"),
            (r"notepad C:\a\\{name}", r"C:\a\\My Notes"),
            (r#"notepad "C:\a b"\{name}"#, r"C:\a b\My Notes"),
            (r"notepad {name}\{name}", r"My Notes\My Notes"),
        ] {
            let line = bind_windows(template, name).unwrap().replace("!FLICK_NAME!", &windows_env_value("My Notes"));
            assert_eq!(argv(&line), vec!["notepad", want], "{template}");
        }
    }

    #[test]
    fn bind_windows_refuses_caret_with_a_placeholder() {
        let refused = Err("this command cannot use ^ together with a placeholder".to_string());
        assert_eq!(bind_windows(r#"echo ^"{a} | findstr ."#, &["a"]), refused);
        assert_eq!(bind_windows(r#"echo "a^b" {a}"#, &["a"]), refused);
        // Without a bound placeholder the template is returned unchanged.
        assert_eq!(bind_windows("echo a^&b", &["a"]).unwrap(), "echo a^&b");
        assert_eq!(bind_windows("echo a^&b {zz}", &["a"]).unwrap(), "echo a^&b {zz}");
    }

    #[test]
    fn bind_windows_refuses_bang_with_a_placeholder() {
        assert_eq!(
            bind_windows("echo hi! {a}", &["a"]),
            Err("this command cannot use ! together with a placeholder".to_string())
        );
        assert_eq!(bind_windows("echo hi!", &["a"]).unwrap(), "echo hi!");
        assert_eq!(bind_windows("echo hi! {other}", &["a"]).unwrap(), "echo hi! {other}");
    }

    #[test]
    fn windows_values_refuse_quotes_and_line_breaks() {
        let refused = Err(UNSAFE_ARGUMENT.to_string());
        assert_eq!(UNSAFE_ARGUMENT, "argument contains characters that cannot be passed safely");
        for bad in ["a\nb", "a\rb", "a\0b"] {
            assert_eq!(check_windows_value(bad), refused, "{bad:?}");
        }
        // `"` and `%` are the ones a user can type: the message names them.
        let refused = Err(r#"cannot pass text containing " or % to this command"#.to_string());
        for bad in [
            r#"x" & calc & ""#,
            r#"say "hi""#,
            "%COMSPEC%",
            "100%",
            "x%CMDCMDLINE:~-1% & echo INJECTED & rem ",
        ] {
            assert_eq!(check_windows_value(bad), refused, "{bad:?}");
        }
        for ok in ["a & b", "!COMSPEC!", "a^b", "!x!", "Résumé & more", "it's", r"trailing\", "a  b", ""] {
            assert_eq!(check_windows_value(ok), Ok(()), "{ok}");
        }
    }

    #[test]
    fn windows_env_values_double_trailing_backslashes() {
        assert_eq!(windows_env_value("plain"), "plain");
        assert_eq!(windows_env_value(r"a\b"), r"a\b");
        assert_eq!(windows_env_value(r"trailing\"), r"trailing\\");
        assert_eq!(windows_env_value(r"C:\dir\\"), r"C:\dir\\\\");
        assert_eq!(windows_env_value(""), "");
        // What the program sees for `prog "!FLICK_A!"` after expansion.
        for v in [r"trailing\", r"C:\", r"a b\\", "x"] {
            assert_eq!(argv(&format!("\"{}\"", windows_env_value(v))), vec![v], "{v}");
        }
    }

    // ---- bind_bash -------------------------------------------------------

    #[test]
    fn bind_bash_assigns_then_references() {
        assert_eq!(
            bind_bash(r"printf '%s\n' {v}", &[("v", "it's")]).unwrap(),
            r#"FLICK_V='it'\''s'; printf '%s\n' "${FLICK_V}""#
        );
        assert_eq!(bind_bash("echo '{x}'", &[("x", "v")]).unwrap(), r#"FLICK_X='v'; echo "${FLICK_X}""#);
        assert_eq!(bind_bash("echo \"{x}\"", &[("x", "v")]).unwrap(), r#"FLICK_X='v'; echo "${FLICK_X}""#);
        assert_eq!(bind_bash("echo \"pre {x} post\"", &[("x", "v")]).unwrap(), r#"FLICK_X='v'; echo "pre ${FLICK_X} post""#);
        assert_eq!(
            bind_bash("cp {src} {dst} {zz}", &[("src", "a b"), ("dst", "c")]).unwrap(),
            r#"FLICK_SRC='a b'; FLICK_DST='c'; cp "${FLICK_SRC}" "${FLICK_DST}" {zz}"#
        );
        assert_eq!(
            bind_bash("echo 'pre {x} post'", &[("x", "v")]),
            Err("put {x} outside single quotes in this command".to_string())
        );
        assert_eq!(bind_bash("echo {x}", &[("x", "a\0b")]), Err(UNSAFE_ARGUMENT.to_string()));
    }

    /// The hostile value appears only once, in the leading assignment, and
    /// nowhere in the rewritten template.
    fn value_only_in_assignment(script: &str, var: &str, value: &str) {
        let assignment = format!("{var}={}; ", quote_bash(value));
        assert!(script.starts_with(&assignment), "{script}");
        assert!(!script[assignment.len()..].contains(value), "{script}");
    }

    #[test]
    fn bind_bash_reviewer_cases_never_put_the_value_in_the_template() {
        // `${v}` used to give ANSI-C quoting ($'...') with live backslashes.
        let v = r"\'; echo INJECTED; #";
        let s = bind_bash(r"printf '%s\n' ${v}", &[("v", v)]).unwrap();
        value_only_in_assignment(&s, "FLICK_V", v);
        assert!(s.ends_with(r#"printf '%s\n' $"${FLICK_V}""#), "{s}");
        // `"` inside "$( )" opens a new context.
        let v = "$(echo INJECTED)";
        let s = bind_bash(r#"ls "$(dirname "{file}")""#, &[("file", v)]).unwrap();
        value_only_in_assignment(&s, "FLICK_FILE", v);
        assert!(s.ends_with(r#"ls "$(dirname "${FLICK_FILE}")""#), "{s}");
    }

    #[test]
    fn bind_bash_tracks_command_substitution() {
        let one = &[("name", "v")];
        assert_eq!(
            bind_bash(r#"echo "$(basename "{name}")""#, one).unwrap(),
            r#"FLICK_NAME='v'; echo "$(basename "${FLICK_NAME}")""#
        );
        assert_eq!(
            bind_bash(r#"echo "$(dirname "$(basename "{name}")")""#, one).unwrap(),
            r#"FLICK_NAME='v'; echo "$(dirname "$(basename "${FLICK_NAME}")")""#
        );
        // Unquoted inside $( ): quoted reference; back in the outer string: bare.
        assert_eq!(
            bind_bash(r#"echo "$(basename {name}) and {name}""#, one).unwrap(),
            r#"FLICK_NAME='v'; echo "$(basename "${FLICK_NAME}") and ${FLICK_NAME}""#
        );
    }

    #[test]
    fn bind_bash_refuses_arithmetic_contexts() {
        let n = &[("n", "1")];
        // Each refusal names the construct that refused it.
        for (template, construct) in [
            ("sleep $(( {n} * 60 ))", "(("),
            ("(( {n} > 3 )) && echo big", "(("),
            ("for ((i=0; i<{n}; i++)); do echo $i; done", "(("),
            ("[[ {n} -gt 0 ]] && echo yes", "[["),
            (r#"[[ -n "{n}" ]]"#, "[["),
            ("let x={n}+1", "let"),
            ("declare -i x={n}", "declare"),
            ("typeset -i x={n}", "typeset"),
            ("local -i x={n}", "local"),
            ("echo ${arr[{n}]}", "an array index"),
            ("arr[{n}]=x", "an array index"),
            ("echo $[ {n} + 1 ]", "$["),
            ("unset {n}", "unset"),
            ("read {n} < /dev/null", "read"),
            ("echo please read {n}", "read"),
            ("[ -v {n} ]", "[ -v"),
            ("test -v {n}", "test -v"),
            ("printf -v {n} x", "printf -v"),
            ("mapfile {n} < /dev/null", "mapfile"),
            ("readarray {n} < /dev/null", "readarray"),
            ("getopts ab {n}", "getopts"),
            ("[  -v {n} ]", "[ -v"),
            ("[\t-v {n} ]", "[ -v"),
            ("test \t -v {n}", "test -v"),
            ("printf  -v {n} x", "printf -v"),
            ("sleep 1 & wait -p {n}", "wait -p"),
            ("sleep 1 & wait -n -p {n}", "wait -p"),
        ] {
            let refused = Err(format!("this command cannot use {construct} together with a placeholder"));
            assert_eq!(bind_bash(template, n), refused, "{template}");
        }
        // Inside `${...}` (substring offsets are arithmetic).
        for template in ["echo ${x:{n}:2}", "echo ${x:-{n}}"] {
            assert_eq!(bind_bash(template, n), Err("put {n} outside ${…} in this command".to_string()), "{template}");
        }
        // Allowed: single-bracket tests, ordinary words containing the names.
        assert_eq!(
            bind_bash(r#"[ -d "{n}" ] && ls "{n}""#, n).unwrap(),
            r#"FLICK_N='1'; [ -d "${FLICK_N}" ] && ls "${FLICK_N}""#
        );
        assert_eq!(bind_bash("[ {n} -gt 0 ]", n).unwrap(), r#"FLICK_N='1'; [ "${FLICK_N}" -gt 0 ]"#);
        assert!(bind_bash("readlink -f {n}; outlet {n}; grep -v {n} x", n).is_ok());
        assert!(bind_bash("wait; echo {n}; latest -v {n}", n).is_ok());
        // Without a bound placeholder nothing is refused.
        assert_eq!(bind_bash("echo $(( 1 + 2 ))", n).unwrap(), "FLICK_N='1'; echo $(( 1 + 2 ))");
    }

    #[test]
    fn bind_bash_tracks_nested_parameter_expansions() {
        // The inner `}` closes only the inner `${`: the placeholder is still inside the outer one.
        assert_eq!(
            bind_bash("echo ${v:${n}{name}}", &[("name", "x")]),
            Err("put {name} outside ${…} in this command".to_string())
        );
        // Both closed: the placeholder after them is plain.
        assert_eq!(
            bind_bash("echo ${a:-${b}}{x}", &[("x", "v")]).unwrap(),
            r#"FLICK_X='v'; echo ${a:-${b}}"${FLICK_X}""#
        );
    }

    // ---- command lines -------------------------------------------------

    fn debian() -> Shell {
        Shell::Wsl { distro: "Debian".into() }
    }

    fn cl(program: &str, params: &str, hidden: bool) -> CommandLine {
        CommandLine { program: program.into(), params: params.into(), hidden }
    }

    #[test]
    fn command_line_shapes() {
        let cmd = system32("cmd.exe");
        let wsl = system32("wsl.exe");
        assert!(cmd.ends_with(r"\System32\cmd.exe") && Path::new(&cmd).is_absolute(), "{cmd}");
        assert!(wsl.ends_with(r"\System32\wsl.exe") && Path::new(&wsl).is_absolute(), "{wsl}");
        assert_eq!(command_line(&Shell::Windows, "dir", false, false, false), cl(&cmd, r#"/D /V:OFF /S /C "dir""#, true));
        assert_eq!(command_line(&Shell::Windows, "dir", true, false, false), cl(&cmd, r#"/D /V:OFF /S /K "dir""#, false));
        assert_eq!(command_line(&Shell::Windows, "dir", false, true, false), cl(&cmd, r#"/D /V:ON /S /C "dir""#, true));
        assert_eq!(command_line(&Shell::Windows, "dir", true, true, false), cl(&cmd, r#"/D /V:ON /S /K "dir""#, false));
        for delayed in [false, true] {
            assert_eq!(
                command_line(&debian(), "ls -la", false, delayed, false),
                cl(&wsl, r#"-d Debian --exec bash -lic "ls -la""#, true)
            );
            assert_eq!(
                command_line(&debian(), "ls -la", true, delayed, false),
                cl(&wsl, r#"-d Debian --exec bash -lic "eval 'ls -la'; exec bash""#, false)
            );
        }
        // In the Linux home: `--cd ~` first, the `~` literal (no shell reads it).
        assert_eq!(
            command_line(&debian(), "ls -la", false, false, true),
            cl(&wsl, r#"--cd ~ -d Debian --exec bash -lic "ls -la""#, true)
        );
        assert_eq!(
            command_line(&debian(), "ls -la", true, false, true),
            cl(&wsl, r#"--cd ~ -d Debian --exec bash -lic "eval 'ls -la'; exec bash""#, false)
        );
        assert_eq!(argv(&command_line(&debian(), "ls", true, false, true).params)[..2], ["--cd", "~"]);
        // cmd.exe has no such option: its directory is an API parameter.
        assert_eq!(command_line(&Shell::Windows, "dir", true, false, true), command_line(&Shell::Windows, "dir", true, false, false));
    }

    #[test]
    fn command_line_with_both_quote_kinds() {
        let c = r#"echo "it's" 'x'"#;
        let (cmd, wsl) = (system32("cmd.exe"), system32("wsl.exe"));
        assert_eq!(
            command_line(&Shell::Windows, c, false, false, false),
            cl(&cmd, r#"/D /V:OFF /S /C "echo "it's" 'x'""#, true)
        );
        assert_eq!(
            command_line(&debian(), c, false, false, false),
            cl(&wsl, r#"-d Debian --exec bash -lic "echo \"it's\" 'x'""#, true)
        );
        let open = command_line(&debian(), c, true, false, false);
        assert_eq!(open.params, r#"-d Debian --exec bash -lic "eval 'echo \"it'\''s\" '\''x'\'''; exec bash""#);
        // The script reaches bash as exactly one argv element.
        let args = argv(&open.params);
        assert_eq!(args[..4], ["-d", "Debian", "--exec", "bash"]);
        assert_eq!(args[4..], ["-lic", r#"eval 'echo "it'\''s" '\''x'\'''; exec bash"#]);
    }

    #[test]
    fn distro_names() {
        assert!(valid_distro("Debian") && valid_distro("Ubuntu-22.04") && valid_distro("my_distro"));
        for bad in ["", "-", "-d", "--exec", "a b", "debian\"", "x&y", "a;b", "ü", "a\nb"] {
            assert!(!valid_distro(bad), "{bad:?}");
        }
    }

    // ---- real shells (opt-in) --------------------------------------------

    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    const PROBE_ENV: &str = "FLICK_ARGV_PROBE";
    const MARK: &str = "FLICK_ARGV_MARK";

    /// Argv printer for `quoting_survives_real_shells`: prints every argument
    /// after MARK as `ARG[...]`. Does nothing unless the probe env var is set.
    #[test]
    #[ignore]
    fn print_argv_helper() {
        if std::env::var_os(PROBE_ENV).is_none() {
            return;
        }
        let args: Vec<String> = std::env::args().collect();
        if let Some(at) = args.iter().position(|a| a == MARK) {
            println!(); // libtest prints the test name without a line break
            for a in &args[at + 1..] {
                println!("ARG[{a}]");
            }
        }
    }

    fn run(line: &CommandLine, env: &[(String, String)]) -> (String, String) {
        run_in(line, env, None)
    }

    /// [`run`] started in `dir` (as `lpDirectory` / `lpCurrentDirectory` do in production).
    fn run_in(line: &CommandLine, env: &[(String, String)], dir: Option<&Path>) -> (String, String) {
        let mut command = Command::new(&line.program);
        if let Some(dir) = dir {
            command.current_dir(dir);
        }
        let out = command
            .raw_arg(&line.params)
            .env(PROBE_ENV, "1")
            .envs(env.iter().map(|(k, v)| (k, v)))
            .creation_flags(CREATE_NO_WINDOW)
            .output()
            .expect("start");
        (String::from_utf8_lossy(&out.stdout).into_owned(), String::from_utf8_lossy(&out.stderr).into_owned())
    }

    fn lines(s: &str) -> Vec<&str> {
        s.lines().map(|l| l.trim_end_matches('\r')).collect()
    }

    fn no_injection(out: &str, err: &str, what: &str) {
        let injected = |s: &str| s.lines().any(|l| l.trim() == "INJECTED");
        assert!(!injected(out) && !injected(err), "{what}\n{out}\n{err}");
    }

    /// Opt-in check of the production shapes on the real machine. It starts
    /// only these programs, all with CREATE_NO_WINDOW and captured output:
    /// `%SystemRoot%\System32\cmd.exe`, `%SystemRoot%\System32\wsl.exe`,
    /// `%SystemRoot%\System32\findstr.exe` (the pipe case) and this test
    /// executable itself as the argv printer. Payloads are echo/printf only.
    /// Production starts bound Windows commands with `CreateProcessW` (see
    /// `launch::create_process`); this test uses `std::process::Command` with
    /// piped output instead, to capture what comes back, with the same
    /// program, parameters and environment values.
    #[test]
    #[ignore]
    fn quoting_survives_real_shells() {
        // Windows: bind_windows + /V:ON + values in the environment.
        let exe = std::env::current_exe().unwrap();
        let printer = format!(
            "{} --exact quote::tests::print_argv_helper --ignored --nocapture --test-threads=1 -- {MARK}",
            quote_argv(&exe.to_string_lossy())
        );
        let findstr = quote_argv(&system32("findstr.exe"));
        let values = [
            "a & echo INJECTED & b",
            "!COMSPEC!",
            "a^b",
            "!x!",
            "it's",
            "a  b",
            r"trailing\",
            r"C:\",
            "Résumé & more",
            "a|b",
            "a>b",
            "x | echo INJECTED",
        ];
        let templates = [
            ("plain", format!("{printer} {{a}}")),
            ("quoted", format!("{printer} \"{{a}}\"")),
            ("pipe", format!("{printer} {{a}} | {findstr} ARG")),
        ];
        for v in values {
            check_windows_value(v).unwrap();
            for (kind, template) in &templates {
                let c = bind_windows(template, &["a"]).unwrap();
                let line = command_line(&Shell::Windows, &c, false, true, false);
                // As SystemLauncher::run_command sets it.
                let env = [("FLICK_A".to_string(), windows_env_value(v))];
                let (out, err) = run(&line, &env);
                let got: Vec<&str> =
                    lines(&out).into_iter().filter_map(|l| l[l.find("ARG[")? + 4..].strip_suffix(']')).collect();
                println!("windows {kind:6} {v:30} -> {got:?}");
                assert_eq!(got, vec![v], "windows {kind} {v}\n{} {}\n{out}\n{err}", line.program, line.params);
                no_injection(&out, &err, v);
            }
        }

        // Constructs that parse the expanded value a second time: only "no
        // injection" is required; what comes back is recorded.
        let reparse = [
            format!("echo {{x}} | {findstr} ."),
            format!("(echo {{x}}) | {findstr} ."),
            "call echo \"{x}\"".to_string(),
            "for /f \"delims=\" %i in ('echo \"{x}\"') do @echo %i".to_string(),
        ];
        for v in ["a & echo INJECTED & b", "!COMSPEC!", "a b"] {
            check_windows_value(v).unwrap();
            for template in &reparse {
                let c = bind_windows(template, &["x"]).unwrap();
                let line = command_line(&Shell::Windows, &c, false, true, false);
                let env = [("FLICK_X".to_string(), windows_env_value(v))];
                let (out, err) = run(&line, &env);
                println!("reparse {template:60} {v:24} -> {:?} {:?}", lines(&out), lines(&err));
                no_injection(&out, &err, v);
            }
        }

        // WSL: bind_bash.
        let wsl = system32("wsl.exe");
        let list = Command::new(&wsl).args(["-l", "-q"]).creation_flags(CREATE_NO_WINDOW).output().unwrap();
        let utf16: Vec<u16> = list.stdout.chunks_exact(2).map(|b| u16::from_le_bytes([b[0], b[1]])).collect();
        let distros = String::from_utf16_lossy(&utf16);
        // The first installed distro that is not Docker's own.
        let Some(distro) = distros
            .lines()
            .map(|l| l.trim().trim_matches('\0').to_string())
            .find(|d| !d.is_empty() && !d.starts_with("docker-desktop"))
        else {
            println!("SKIPPED the WSL half: no WSL distro is installed ({distros:?})");
            return;
        };
        let probe_shell = Shell::Wsl { distro };
        let values = [
            "it's",
            r#"a "b" c"#,
            "$(echo INJECTED)",
            "`echo INJECTED`",
            r"\'; echo INJECTED; #",
            "; echo INJECTED ;",
            "%USERNAME%",
            "a  b",
            "Résumé",
            "*",
            r"trailing\",
        ];
        let templates = [
            (true, r"printf '%s\n' {v}"),
            (true, r#"printf '%s\n' "{v}""#),
            (true, r"printf '%s\n' '{v}'"),
            (false, r"printf '%s\n' ${v}"),
            (true, r#"echo "$(printf '%s' "{v}")""#),
        ];
        for v in values {
            for (well_formed, template) in templates {
                let script = bind_bash(template, &[("v", v)]).unwrap();
                for keep_open in [false, true] {
                    let mut line = command_line(&probe_shell, &script, keep_open, false, false);
                    if keep_open {
                        assert!(line.params.contains("; exec bash"));
                        line.params = line.params.replace("; exec bash", "; true");
                    }
                    let (out, err) = run(&line, &[]);
                    println!("wsl keep={keep_open:5} {template:34} {v:24} -> {:?}", lines(&out));
                    if well_formed {
                        assert!(lines(&out).contains(&v), "wsl {template} {v}\n{}\n{out}\n{err}", line.params);
                    }
                    no_injection(&out, &err, v);
                }
            }
        }
        // Arithmetic-looking templates that bind_bash still accepts.
        let v = "a[$(echo INJECTED >&2)]";
        for template in ["[ {n} -gt 0 ]", "sleep {n}"] {
            let script = bind_bash(template, &[("n", v)]).unwrap();
            let line = command_line(&probe_shell, &script, false, false, false);
            let (out, err) = run(&line, &[]);
            println!("wsl arith {template:24} {v} -> {:?} {:?}", lines(&out), lines(&err));
            no_injection(&out, &err, v);
        }
        for (c, want) in [("echo END1 &", "END1"), ("echo END2;", "END2"), ("echo END3 # comment", "END3")] {
            for keep_open in [false, true] {
                let mut line = command_line(&probe_shell, c, keep_open, false, false);
                if keep_open {
                    line.params = line.params.replace("; exec bash", "; echo AFTER");
                }
                let (out, err) = run(&line, &[]);
                println!("wsl keep={keep_open:5} {c:34} -> {:?}", lines(&out));
                assert!(lines(&out).contains(&want), "{c}\n{}\n{out}\n{err}", line.params);
                if keep_open {
                    assert!(lines(&out).contains(&"AFTER"), "the keep-open tail did not run: {c}\n{out}\n{err}");
                }
            }
        }

        // Working directory. `--cd ~` (home = true): the Linux home, whatever
        // directory wsl.exe itself was started in.
        let show_pwd = r#"printf '%s\n' "PWD=$PWD" "HOME=$HOME""#;
        let elsewhere = tempfile::tempdir().unwrap();
        let line = command_line(&probe_shell, show_pwd, false, false, true);
        let (out, err) = run_in(&line, &[], Some(elsewhere.path()));
        let out_lines = lines(&out);
        let field = |key: &str| out_lines.iter().find_map(|l| l.strip_prefix(key)).map(str::to_string);
        println!("wsl --cd ~ -> {out_lines:?}");
        assert!(field("PWD=").is_some() && field("PWD=") == field("HOME="), "{}\n{out}\n{err}", line.params);
        // Without `--cd` wsl.exe starts in its own directory, translated:
        // a picked folder passed as lpDirectory arrives as /mnt/...
        let line = command_line(&probe_shell, show_pwd, false, false, false);
        let (out, err) = run_in(&line, &[], Some(elsewhere.path()));
        let out_lines = lines(&out);
        let pwd = out_lines.iter().find_map(|l| l.strip_prefix("PWD=")).map(str::to_string);
        let want = to_wsl_path(&elsewhere.path().to_string_lossy());
        println!("wsl in {} -> {out_lines:?}", elsewhere.path().display());
        assert!(
            pwd.as_deref().map(str::to_lowercase) == want.as_deref().map(str::to_lowercase),
            "{pwd:?} != {want:?}\n{out}\n{err}"
        );
    }
}
