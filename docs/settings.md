# Settings, appearance and config

The Settings window, themes and animations, updates, and the config file.

[Back to the overview](../README.md)

## Settings

Open the Settings window from the tray menu (Settings), by typing `settings` in the bar, or with the Open Settings row on the first-run welcome. Every setting is there, so you never have to open the config file. Changes in General, Search, Quick answers and Updates apply at once; a shortcut, workspace or hotkey is saved when you press Save.

- **General**: the hotkey (click the box and press the keys), Win key opens Flick (see [Win key](windows.md#win-key)), Start at login, remembering past searches and launches (with a Clear history button), and the WSL distro.
- **Search**: the folders to search (add or remove them) and your pins, which you can reorder or unpin.
- **Quick answers**: going online, your home city, currency and temperature unit, and the search engine.
- **Shortcuts**: your list of shortcuts, with an editor that checks each one as you type.
- **Workspaces**: your workspaces and their items (see [Workspaces and hotkeys](shortcuts.md#workspaces-and-hotkeys)).
- **Hotkeys**: your hotkeys, each with whether it works (see [Workspaces and hotkeys](shortcuts.md#workspaces-and-hotkeys)).
- **Updates**: the installed version, Check automatically, Check now, and the update waiting for you, if any.

The config file still works and stays the one source of truth. Edit it by hand if you like; the window follows your changes. Changes made in the window keep your comments, the order of the file and the shortcuts you did not touch. If the file has an error, the window shows it and stays read-only until you fix it.

## Appearance

Settings has an Appearance tab for how the bar looks and moves. The same choices are `theme`, `custom_theme` and `animations` in the config.

**Themes.** Dark is the default. The others are Light, Midnight, Frost, Ember, Ocean, Forest, Grape, Rose, Sand, Mint, Sakura and Glass. The Settings window follows the theme.

**Your own colours.** Pick Custom and set eight colours with the colour pickers: background, selected row, border, text, muted text, accent, error and warning. "Start from" copies a built-in theme to begin with. Settings warns when text would be hard to read. In the config: `theme = "custom"` and `custom_theme = { background = "#101418", text = "#e8eef4", accent = "#4fd1c5" }`; a colour you leave out is taken from Dark.

**Glass.** The bar looks like a pane of glass: what is behind it shows through, softened, and bends at the rim. To do that, Flick takes a picture of the screen area behind the bar each time the bar opens.

- Only that rectangle is read. The picture is kept in memory while the bar is open and dropped when it closes; it is never saved or sent anywhere. Other themes take no picture.
- The picture is still: a video playing behind the bar is frozen inside the pane until you close the bar.
- The text is dark over a light background and white over a dark one.
- The Settings window uses the Dark colours under Glass.

**Animations.** Minimal (the default) changes everything at once. Medium fades the bar in, glides the highlight between rows and eases the list to its new height. High also lets the bar rise into place, gives the highlight a little bounce and fades new rows in one after another. Wobble gives things weight: the highlight and the list are moved by a small physics simulation, so they carry their speed from one move into the next, overshoot a little and settle softly. Closing is instant at every level, and typing is never held up. If Windows' animation effects are switched off, nothing moves whatever is chosen.

A wrong `theme`, `animations` or colour is a config error like any other: Flick says so and keeps the last working config.

## Updates

Flick looks for a new version 10 seconds after it starts and then once a day. When one is found, the first row of the empty bar reads "Update to x.y" with "Enter to install and restart" under it, and the tray menu gets an Update to x.y item. Enter downloads it, installs it and restarts Flick; nothing installs unless you say so. Start at login and your settings are kept. If it fails, the bar says "Update failed" with the reason and the current version keeps running. Only updates signed with the project's key are installed.

Set `check_updates = false` to stop the automatic checks (`online = false` stops them too). Check now in Settings, Updates still works.

Versions 0.3.0 and older have no updater, so 0.4.0 has to be installed by hand once, as in Install.

## Config

The config file is `%APPDATA%\Flick\config.toml`. It is created with commented defaults on first start. Most people can use the Settings window instead (see [Settings](#settings)). Type `flick config` in the bar (or use the tray menu) to open the file.

```toml
hotkey = "Alt+Space"
wsl_distro = "Ubuntu"
file_roots = ["C:/Users/you/Desktop", "C:/Users/you/Documents", "C:/Users/you/Downloads"]
history = true

[[shortcut]]
name = "docs"
open = "https://docs.rs"
```

- `hotkey`: the global shortcut that shows the bar.
- `wsl_distro`: the WSL distro for `>` commands and `wsl` shortcuts. Leave it out to use WSL's default distro.
- `file_roots`: the folders searched for files. Leave it out to search your whole user folder.
- `history`: set to `false` to stop saving past searches and recent launches (see [Past searches](typing.md#past-searches)). A new install starts with `history = false` in its config; a config without the line counts as `true`.
- `online`, `weather_city`, `currency`, `temperature_unit`, `search_engine`: see Quick answers.
- `check_updates`: set to `false` to stop Flick looking for new versions (see [Updates](#updates)). The default is `true`.
- `win_key`: set to `true` to open the bar with the Win key on its own (see [Win key](windows.md#win-key)). The default is `false`.
- `[[shortcut]]`: see Shortcuts.
- `[[workspace]]` and `[[direct_hotkey]]`: your workspaces and hotkeys (see [Workspaces and hotkeys](shortcuts.md#workspaces-and-hotkeys)). Settings writes them for you.

<details>
<summary>When changes take effect, and errors</summary>

Changes are picked up as soon as you save the file; changed `file_roots` are searched again in the background. If the file has an error, Flick keeps the last good settings and shows the error in the bar. If the hotkey is not a valid shortcut or is in use by another program, the bar and the tray tooltip say so. Without WSL, `>` commands are not offered, whatever `wsl_distro` says.

</details>
