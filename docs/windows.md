# Windows controls

System toggles, devices and chores, the Win key, the taskbar, and the tray menu.

[Back to the overview](../README.md)

## Control Windows

Flick can switch to a window, close apps, and steer the PC without the mouse. Rows for open windows rank above the app's own row.

| You type | What happens |
|---|---|
| Part of a window title or app name | Lists the matching open windows; Enter brings one to the front |
| `close chrome` | Closes all windows of that app politely (they may ask to save) |
| `kill chrome` | Forces the app to end (press Enter twice); unsaved work is lost |
| `quit all` | Closes every open window except Explorer (press Enter twice) |
| `vol 30`, `vol up`, `vol down` | Sets or steps the volume |
| `mute`, `unmute` | Mutes or unmutes the sound |
| `next song`, `previous song`, `play`, `pause` | Sends the media key to the player |
| `dark mode`, `light mode` | Switches the Windows theme |
| `wifi off`, `wifi on`, `bluetooth off`, `bluetooth on` | Switches the radio (if Windows will not allow it, the Settings page opens) |
| `night light` | Opens the Night light settings page |
| `battery`, `cpu`, `ram`, `disk`, `ip` | Shows the value on top; Enter copies it |

Apps that run as administrator cannot be closed or ended from Flick and say so. Explorer can be closed but never ended. Window rows and status values are never learned or saved.

## Devices and chores

- `connect headphones` / `disconnect headphones`: paired Bluetooth **audio** devices (headphones, earbuds, speakers) by name; bare `connect` or `disconnect` lists them. Other kinds of Bluetooth device are not listed, and pairing a new device still happens in Windows' Settings. Connecting and disconnecting has not been verified on a real device yet.
- `wifi home`: joins a **saved** Wi-Fi network by name. Networks you have never joined are not offered, and Flick never asks for a password. `wifi on` and `wifi off` switch the radio as before.
- `empty bin`: shows how many items it holds and how much space; Enter twice empties the Recycle Bin, without Windows' own question.
- `eject` lists removable drives (`eject E:` picks one); Enter ejects it safely. A drive with a file open on it says `Drive is in use`.
- `flush dns`: one Enter clears Windows' DNS cache.
- `restart explorer`: Enter twice restarts the taskbar and desktop.

Of these, `empty bin` and `restart explorer` ask for the second Enter.

## Win key

Flick can take over the Win key: tap it on its own and the bar opens instead of the Start menu; tap it again to close the bar. It is off by default. Switch it on in Settings, General (Win key opens Flick), or set `win_key = true` in the config. It applies at once, with no restart.

- Win combinations keep working as before: Win+E, Win+D, Win+V, Win+Shift+S and the rest. Holding Win and clicking or scrolling counts as a combination too, so the bar stays closed.
- The Start menu is still there: press Ctrl+Esc, or type `start` in the bar and pick the Start menu row. The row can't open Start when a window running as administrator was in front (Windows blocks Flick's keys there); Ctrl+Esc typed by hand still works.
- Windows keeps the Win key while a window running as administrator is in front, while an exclusive full-screen game is in front, while Sticky Keys is on (a tap of Win then holds it for the next key), and while Settings is waiting for you to press a hotkey. A tap then opens Start as usual.
- While it's on, Task Manager shows a second flick.exe: it watches the Win key, and ends when you switch the setting off or quit Flick. If it ends some other way, Flick starts it again within about 10 seconds.
- If Flick itself stops responding, the Win key goes back to Windows after 2 seconds, so Start is never out of reach.
- If Windows doesn't let Flick watch the Win key, Settings says "The Win key isn't available right now; Alt+Space still works." (with your bar hotkey). The bar hotkey is not affected by this setting.

## Showing the taskbar

If your taskbar hides by itself, Flick can bring it up while the bar is open, the way the Start menu does. Switch on **Show the taskbar while Flick is open** in Settings, General, or set `show_taskbar = true` in the config. It is off by default.

- While the bar is open, a Flick button sits on the taskbar; that button is what keeps the taskbar up. It lights up for a moment as the taskbar rises.
- Closing the bar removes the button, and the taskbar hides again about half a second later. Clicking the button closes the bar, like a click anywhere else.
- No Windows setting is changed, and it does nothing when the taskbar doesn't hide by itself.

## Tray menu

- Open
- Settings
- Edit config (opens the file in its associated editor, or in Notepad if `.toml` has none)
- Reload (reads the config again and refreshes apps, games, bookmarks, files and shortcut lists)
- Start at login (switched on the first time a release build starts)
- Quit

## Hotkey conflict with PowerToys Run

PowerToys Run also uses Alt+Space by default. Only one program can own a hotkey, so change one of the two (set `hotkey` in Flick's config, or change the shortcut in PowerToys). When Flick cannot register its hotkey and PowerToys Run is running, the bar and the tray tooltip name PowerToys Run as the likely cause.
