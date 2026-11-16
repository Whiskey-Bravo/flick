# Flick

Flick is a small keyboard launcher for Windows 11, built for personal use with Tauri 2 (a Rust core and a plain TypeScript front end). Press a global hotkey, type a few letters, press Enter.

## What it does today

- Finds and opens installed apps (Start-menu shortcuts and Store apps).
- Finds files and folders by name in your Desktop, Documents and Downloads (or the folders you choose).
- Runs your own shortcuts from the config: commands, commands that take what you type, links, and lists to pick from.
- Runs commands in WSL: type `>` and the command.
- Opens Windows Settings pages (for example "Bluetooth & devices" or "Installed apps").
- Runs power actions: lock, sleep, restart, shut down, sign out.
- Runs an app as administrator. This applies to apps only, both shortcuts and Store apps. A Store app that cannot be elevated says "this app cannot be run as administrator" instead of opening normally. Cancelling the UAC prompt just closes the bar.
- Lists everything it can do: type `help` or press F1.
- Opens its own config file: type `flick config`.

Apps, Settings pages and shortcuts match by prefix, word prefix, initials (`vsc` finds Visual Studio Code), initials followed by the start of the next word (`vscode` and `vs code` find Visual Studio Code) and substring. Hyphens, dots and spaces in names are ignored when matching from the start, so `7zip` finds "7-Zip File Manager" and `wifi` finds "Wi-Fi". For queries of three letters or more, a dropped letter or two inside one word is tolerated (`blnder` finds Blender); swapped letters such as `chorme` do not match. Letters scattered across unrelated words do not match.

Files and folders match more strictly: by their exact name (or a file's name without its extension), and from three letters by the start of the name or the start of a word in it (every typed word the start of a word), and from four letters by any part of the name. Initials and dropped letters never find a file.

Apps are also found by their program or package name: `winword` finds Word, `msedge` finds Edge, `calc` finds Calculator.

When several kinds of result match equally well, your shortcuts come first, then apps and Settings pages. Files and folders always come below an app, shortcut or Settings page whose name matches, folders above files, and deeper ones lower. While other results exist, files and folders take at most five of the eight rows; rows left over after everything else go to further files. Power actions rank below an app matching the same text.

Results you pick often move up, a file you pick often even above apps. Only which result you picked is counted (an app, a Settings page, a file, a shortcut, an entry of a shortcut's list), stored only on this PC in `%APPDATA%\Flick\picks.json`, and never shown. Nothing you type is saved: not WSL commands, not the text you give a shortcut. Power actions are never learned.

If you press Enter while results for what you just typed are still on their way, Flick waits for them and opens the new top result.

## Keys

| Key | Action |
|---|---|
| Alt+Space | Open the bar (configurable) |
| Enter | Open the selected result |
| Ctrl+Enter | Show the selected file or folder in Explorer |
| Ctrl+Shift+Enter | Run the selected app as administrator |
| Enter again | Confirm restart, shut down or sign out |
| Up / Down | Move the selection |
| F1 | Show the help list |
| Esc | Leave a shortcut's list; otherwise close the bar |

The hotkey does nothing while an exclusive full-screen app (such as a game) is in front.

## Help

Type `help` (in any case) to see one line for each kind of result, with an example. Type more after it to filter the list (`help file`). Picking a line puts its example in the bar, so `> htop -d 10` gives you `> ` ready to type. F1 does the same as typing `help`, and leaves a shortcut's list first.

## Files and folders

Type part of a file or folder name. Search is by name only, not by content. Two characters find only an exact name (`db`); the start of a name or word needs three, any other part of the name four.

- Enter opens the file or folder; Ctrl+Enter shows it selected in Explorer.
- The searched folders are your Desktop, Documents and Downloads, or the ones listed in `file_roots` in the config.
- Hidden and system files, names starting with `.`, and build, package or tool folders (`node_modules`, `target`, `dist`, `build`, `__pycache__`, `venv`, `AppData`, `vendor`, `site-packages`, `Pods`, `bower_components`, `obj`) are skipped.
- At most 300 000 entries are indexed (shared equally between the folders), down to eight folder levels.
- The list is built in the background when Flick starts, kept up to date as files are added, removed or renamed, and rebuilt every 30 minutes. A file that was moved since says "file not found".

## WSL commands

Type `>` followed by a command, for example `> htop -d 10`, and press Enter. The command runs in a new terminal window in your default WSL distro unless `wsl_distro` in the config says otherwise, and the window stays open afterwards. Commands are never saved or learned. Without WSL installed, `>` commands are not offered, and a `wsl` shortcut says "WSL is not installed".

## Shortcuts

Shortcuts are your own commands, defined in the config as `[[shortcut]]` tables. Type the shortcut's name (or one of its aliases) and press Enter.

Each shortcut needs exactly one of:

- `run`: a Windows command, run by `cmd.exe`;
- `wsl`: a command run in your WSL distro;
- `open`: a file, folder, program or URL to open.

Other keys:

- `name`: one word, without `:` and not starting with `>`; names and aliases must be unique and cannot be `help`.
- `aliases`: other words that find the shortcut.
- `about`: the text shown under the name (the command is shown when this is empty).
- `terminal`: keep a terminal window open. The default is `true` for `wsl` and `false` for `run`. With `false` a `run` command runs without a window.
- `admin`: run as administrator (`run` and `open` only). Such a shortcut cannot take placeholders.
- `pick`: a folder; its sub-folders are the choices.
- `choices`: a list of texts to choose from.

Placeholders:

- `{arg}`: the text you type after the name. Type the name, then a space, then the text (`search rust traits`). Pressing Enter on the name alone puts `search ` in the bar for you to finish. `{arg}` cannot be combined with `pick` or `choices`.
- `{name}`: with `pick` or `choices`, the chosen entry.
- `{path}`: with `pick`, the full path of the chosen sub-folder (for `wsl` shortcuts, as a `/mnt/c/...` path).

A shortcut with `pick` or `choices` opens a list when you press Enter on it; type to filter it, Enter runs the shortcut with that entry, Esc goes back. You can also type the name and part of an entry directly (`proj fli`).

Folder lists are re-read every 5 minutes, and in the background when you open the list (the list shows what was read last).

An `open` link that takes what you type (the text is encoded for the web address):

```toml
[[shortcut]]
name = "search"
aliases = ["s"]
about = "Search the web"
open = "https://duckduckgo.com/?q={arg}"
```

A `run` command that picks a project folder:

```toml
[[shortcut]]
name = "proj"
about = "Open a project in VS Code"
pick = "C:/Users/you/Projects"
run = "code {path}"
```

A `wsl` command that needs a terminal for `sudo` (a terminal is kept open by default):

```toml
[[shortcut]]
name = "upgrade"
about = "Upgrade Linux packages"
wsl = "sudo apt upgrade"
```

A `run` command that takes what you type:

```toml
[[shortcut]]
name = "ping"
about = "Ping a host"
run = "ping {arg}"
terminal = true
```

A `run` command as administrator:

```toml
[[shortcut]]
name = "flushdns"
about = "Flush the DNS cache"
run = "ipconfig /flushdns"
admin = true
```

### Limits of placeholders

In `run` and `wsl` shortcuts, what you type is never pasted into the command. Flick passes each value as a variable (`FLICK_ARG`, `FLICK_NAME`, `FLICK_PATH`) and the command refers to that variable, so `&`, `|`, `;` or `$( )` in what you type are just text. To keep that safe, some commands cannot take placeholders; Flick reports this when it reads the config. `open` shortcuts insert the text into the target itself (no shell is involved), percent-encoded for web links.

- `run` shortcuts: a value containing `"` or `%` is refused ("cannot pass text containing " or % to this command"). A command cannot combine a placeholder with `!` or `^`.
- `run` shortcuts whose program is a `.bat` or `.cmd` file (such as `code`, VS Code's launcher) read their arguments a second time as command text; `&`, `|` or `^` in a value may then not arrive exactly as typed.
- `wsl` shortcuts: a command cannot combine a placeholder with arithmetic (`(( ))`, `$(( ))`, `let`), `[[ ]]`, an array index, or builtins that take a variable name (`declare`, `local`, `read`, `unset`, `printf -v` and similar); the message names the one found. These words are refused anywhere in the command, even inside quoted text (`echo please read {arg}`). Use `[ … ]` for tests. A placeholder inside single quotes must be the whole quoted text (`'{arg}'`), and a placeholder cannot sit inside `${…}`.
- `wsl` shortcuts: do not copy a placeholder into a variable that is then used in arithmetic, an array index or a substring offset (`n={arg}; echo "${s:n}"`): bash evaluates the value there, and Flick cannot see it.
- Shortcuts that run as administrator cannot take placeholders.
- A value starting with `-` or `/` is read as an option by the program it is given to. Put `--` before the placeholder if the program supports it.
- Commands that hand their arguments to another shell evaluate the value there: `ssh host …`, `watch …`, `su -c`, `wsl …` without `--exec`, `powershell -c`. Do not put a placeholder into those.
- Values inserted into an `http://` or `https://` `open` target are percent-encoded. Other `open` targets get the value as it is.
- Shortcut names are one word, without `:` and not starting with `>`.

### Where commands start

- Apps, files, `open` shortcuts and `run` shortcuts start in your user folder (`%USERPROFILE%`), unless an app's own shortcut names another start folder.
- `>` commands and `wsl` shortcuts start in your Linux home folder.
- A shortcut with `pick` starts in the folder you picked (`wsl` shortcuts see it as `/mnt/c/...`).
- Anything run as administrator starts in `C:\Windows\System32`: Windows decides this.

## Config

The config file is `%APPDATA%\Flick\config.toml`. It is created with commented defaults on first start. Type `flick config` in the bar (or use the tray menu) to open it.

```toml
hotkey = "Alt+Space"
wsl_distro = "Ubuntu"
file_roots = ["C:/Users/you/Desktop", "C:/Users/you/Documents", "C:/Users/you/Downloads"]

[[shortcut]]
name = "docs"
open = "https://docs.rs"
```

- `hotkey`: the global shortcut that shows the bar.
- `wsl_distro`: the WSL distro for `>` commands and `wsl` shortcuts. Leave it out to use WSL's default distro; without WSL, `>` commands are not offered.
- `file_roots`: the folders searched for files. Leave it out for Desktop, Documents and Downloads.
- `[[shortcut]]`: see Shortcuts.

Changes are picked up as soon as you save the file; changed `file_roots` are searched again in the background. If the file has an error, Flick keeps the last good settings and shows the error in the bar. If the hotkey is not a valid shortcut or is in use by another program, the bar and the tray tooltip say so.

## Tray menu

- Open
- Edit config (opens the file in its associated editor, or in Notepad if `.toml` has none)
- Reload (reads the config again and refreshes apps, files and shortcut lists)
- Start at login (switched on the first time a release build starts)
- Quit

## Run and build

```
npm install
npm run tauri dev
npm run tauri build
```

The build makes an NSIS installer.

Tests:

```
cargo test --manifest-path src-tauri/Cargo.toml
npx vitest run
```

Do not run a bare `cargo update`: `Cargo.lock` pins `notify-types` to a version that still builds with Rust 1.90.

## Hotkey conflict with PowerToys Run

PowerToys Run also uses Alt+Space by default. Only one program can own a hotkey, so change one of the two (set `hotkey` in Flick's config, or change the shortcut in PowerToys).

## Not yet

- Calculator and unit conversions
- Web search
- Clipboard history
- Taking over the Win key
