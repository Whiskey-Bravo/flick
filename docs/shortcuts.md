# Shortcuts, WSL and workspaces

Your own commands, commands in WSL, workspaces and direct hotkeys.

[Back to the overview](../README.md)

## Shortcuts

Shortcuts are your own commands, defined in the config as `[[shortcut]]` tables. Type the shortcut's name (or one of its aliases) and press Enter. Each shortcut has exactly one of `run` (a Windows command), `wsl` (a command in WSL) or `open` (a file, folder, program or URL).

A shortcut can have its own icon: add `icon = "..."` with the path of a program, a shortcut file or an `.ico` file (the row shows that file's icon) or of a `.png` file up to 1 MB (the row shows the picture). In Settings, the Icon field also lets you pick an app and use its icon. An icon that can't be read falls back to the usual one.

Search the web with what you type (`search rust traits`):

```toml
[[shortcut]]
name = "search"
aliases = ["s"]
about = "Search the web"
open = "https://duckduckgo.com/?q={arg}"
```

Pick a project folder from a list, then open it in VS Code:

```toml
[[shortcut]]
name = "proj"
about = "Open a project in VS Code"
pick = "C:/Users/you/Projects"
run = "code {path}"
```

Run a command as administrator:

```toml
[[shortcut]]
name = "flushdns"
about = "Flush the DNS cache"
run = "ipconfig /flushdns"
admin = true
```

<details>
<summary>All shortcut keys</summary>

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

</details>

<details>
<summary>Placeholders and lists</summary>

- `{arg}`: the text you type after the name. Type the name, then a space, then the text (`search rust traits`). Pressing Enter on the name alone puts `search ` in the bar for you to finish. `{arg}` cannot be combined with `pick` or `choices`.
- `{name}`: with `pick` or `choices`, the chosen entry.
- `{path}`: with `pick`, the full path of the chosen sub-folder (for `wsl` shortcuts, as a `/mnt/c/...` path).

In an `open` link, the text you type is encoded for the web address.

A shortcut with `pick` or `choices` opens a list when you press Enter on it; type to filter it, Enter runs the shortcut with that entry, Esc goes back. You can also type the name and part of an entry directly (`proj fli`).

Folder lists are re-read every 5 minutes, and in the background when you open the list (the list shows what was read last).

</details>

<details>
<summary>More examples</summary>

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

</details>

<details>
<summary>Limits of placeholders</summary>

In `run` and `wsl` shortcuts, what you type is never pasted into the command. Flick passes each value as a variable (`FLICK_ARG`, `FLICK_NAME`, `FLICK_PATH`) and the command refers to that variable, so `&`, `|`, `;` or `$( )` in what you type are just text. To keep that safe, some commands cannot take placeholders; Flick reports this when it reads the config. `open` shortcuts insert the text into the target itself (no shell is involved), percent-encoded for links (`https://…` and app links such as `claude://…` or `obsidian://…`).

- `run` shortcuts: a value containing `"` or `%` is refused ("cannot pass text containing " or % to this command"). A command cannot combine a placeholder with `!` or `^`.
- `run` shortcuts whose program is a `.bat` or `.cmd` file (such as `code`, VS Code's launcher) read their arguments a second time as command text; `&`, `|` or `^` in a value may then not arrive exactly as typed.
- `wsl` shortcuts: a command cannot combine a placeholder with arithmetic (`(( ))`, `$(( ))`, `let`), `[[ ]]`, an array index, or builtins that take a variable name (`declare`, `local`, `read`, `unset`, `printf -v` and similar); the message names the one found. These words are refused anywhere in the command, even inside quoted text (`echo please read {arg}`). Use `[ … ]` for tests. A placeholder inside single quotes must be the whole quoted text (`'{arg}'`), and a placeholder cannot sit inside `${…}`.
- `wsl` shortcuts: do not copy a placeholder into a variable that is then used in arithmetic, an array index or a substring offset (`n={arg}; echo "${s:n}"`): bash evaluates the value there, and Flick cannot see it.
- Shortcuts that run as administrator cannot take placeholders.
- A value starting with `-` or `/` is read as an option by the program it is given to. Put `--` before the placeholder if the program supports it.
- Commands that hand their arguments to another shell evaluate the value there: `ssh host …`, `watch …`, `su -c`, `wsl …` without `--exec`, `powershell -c`. Do not put a placeholder into those.
- Values inserted into an `http://` or `https://` `open` target are percent-encoded. Other `open` targets get the value as it is.
- Shortcut names are one word, without `:` and not starting with `>`.

</details>

<details>
<summary>Where commands start</summary>

- Apps, files, `open` shortcuts and `run` shortcuts start in your user folder (`%USERPROFILE%`), unless an app's own shortcut names another start folder.
- `>` commands and `wsl` shortcuts start in your Linux home folder.
- A shortcut with `pick` starts in the folder you picked (`wsl` shortcuts see it as `/mnt/c/...`).
- Anything run as administrator starts in `C:\Windows\System32`: Windows decides this.

</details>

## WSL commands

Type `>` followed by a command, for example `> htop -d 10`, and press Enter. The command runs in a new terminal window in your default WSL distro (or the one set by `wsl_distro` in the config). The window stays open afterwards. Commands are never saved or learned.

Without WSL installed, `>` commands are not offered, and a `wsl` shortcut says "WSL is not installed".

<details>
<summary>How matching and ranking work</summary>

**Apps, Settings pages and shortcuts** match by:

- prefix, word prefix and substring;
- initials (`vsc` finds Visual Studio Code);
- initials followed by the start of the next word (`vscode` and `vs code` find Visual Studio Code).

Hyphens, dots and spaces in names are ignored when matching from the start, so `7zip` finds "7-Zip File Manager" and `wifi` finds "Wi-Fi". For queries of three letters or more, a dropped letter or two inside one word is tolerated (`blnder` finds Blender). Swapped letters such as `chorme` do not match. Letters scattered across unrelated words do not match.

Apps are also found by their program or package name: `winword` finds Word, `msedge` finds Edge, `calc` finds Calculator.

**Files and folders** match more strictly:

- by their exact name (or a file's name without its extension);
- from three letters, by the start of the name or the start of a word in it (every typed word the start of a word);
- from four letters, by any part of the name.

Initials and dropped letters never find a file.

**Ranking.** When several kinds of result match equally well, your shortcuts come first, then apps and Settings pages. Files and folders always come below an app, shortcut or Settings page whose name matches, folders above files, and deeper ones lower. While other results exist, files and folders take at most five of the eight rows; rows left over after everything else go to further files. Power actions rank below an app matching the same text.

Results you pick often move up, a file you pick often even above apps. Power actions are never learned. See Privacy for what is stored.

</details>

## Workspaces and hotkeys

Both are set up in Settings, on the Workspaces and Hotkeys tabs. You pick their items with the search box there, which finds what the bar finds: apps, files, folders, games, bookmarks, shortcuts, Settings pages, Bluetooth devices and Wi-Fi networks (`connect headphones`, `wifi home`), chores and more. Answers, `tr`/`def` and timers are not offered there, so nothing typed in Settings goes online.

**Workspaces.** A workspace is a name for a set of items (`work`, `gaming`). Type its name and the bar shows `Open workspace work`; Enter opens every item in order, with a short pause between them, and the bar hides. An item that no longer exists, one that asks for a second Enter (`empty bin`, power actions), or a shortcut that needs something typed after it is skipped (the Workspaces tab does not offer those). If any item could not open, the bar comes back with the workspace's name and, for example, `1 of 3 could not open`. Right after you sign in, a workspace waits a few seconds for Flick to finish finding your apps and games; if they are still not ready it says `Flick is still starting`. A workspace cannot be named after something Flick answers to itself (`timer`, `tr`, `connect`, `v`, `snip`…). Workspaces do not arrange or close windows.

**Hotkeys.** A hotkey opens one item from anywhere, without the bar. It needs two modifiers (like Ctrl+Alt) or Win: plain keys and single-modifier combos are refused. A combo that another program holds says `Taken by another program` next to it in Settings, and your other hotkeys keep working. Flick's own bar hotkey always wins: a hotkey with the same keys is never registered, also when you later change the bar hotkey to those keys. A hotkey on an item that asks for a second Enter (`empty bin`, `restart explorer`, shut down…) opens the bar with that row ready, so one deliberate Enter is still needed. A hotkey on a shortcut that takes an argument opens the bar with the shortcut typed, ready for the rest; one with a pick list opens the bar on that list. If the item is gone, Settings says `Not found` next to it. A hotkey works everywhere, so it can hide an app's own shortcut with the same keys.
