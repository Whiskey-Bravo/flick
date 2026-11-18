# Privacy

What Flick keeps on your PC, what goes online, and what never does.

[Back to the overview](../README.md)

Flick counts only which result you picked (an app, a Settings page, a file, a shortcut, an entry of a shortcut's list). This is stored only on this PC in `%APPDATA%\Flick\picks.json` and never shown. Power actions and system toggles (mute, Wi-Fi, dark mode and the like) are never counted or kept as recent launches.

The text of a search is saved when you open a result from it, on this PC in `%APPDATA%\Flick\history.json`, together with your recent launches. On a new install this is switched off: nothing you type is kept until you turn on "Remember past searches and launches" in Settings (or set `history = true`). Installs from before version 0.11 keep it on. It keeps the last 50 searches and the last 5 launches. Pins are saved in `%APPDATA%\Flick\pins.json`.

Flick goes online when you ask a currency or weather question, when you type `ip`, and to look for updates, and only while `online` is not `false`:

- Currency: when the saved rates are older than the latest publication, Flick downloads the European Central Bank's daily rates file (at most once an hour) and keeps it in `%APPDATA%\Flick\rates.json`. Nothing about you is sent beyond the request itself.
- Weather: Flick sends the city name you asked for (or your `weather_city`) to Open-Meteo's place search, then that place's coordinates to Open-Meteo's forecast. Never your own position, and nothing is looked up from your IP address. Only your `weather_city` and its coordinates are kept, in `%APPDATA%\Flick\weather.json`; any other place is remembered only until Flick quits.
- Flick asks GitHub once a day whether a new version exists (GitHub sees the request; nothing about you is sent). `check_updates = false` or `online = false` stops it.
- `ip` (also `my ip` and `ip address`) asks api.ipify.org for your public address, only while `online` is on, and only after the text has stayed exactly `ip` for a moment (so typing `ipconfig` sends nothing). api.ipify.org sees the request; nothing about you is sent. Without it you see just your local addresses.
- `tr` sends only the text typed after `tr` to MyMemory (`api.mymemory.translated.net`), and `def` only the word typed after `def` to Wikimedia (`en.wiktionary.org`); only after typing pauses, and never while `online` is off. Nothing else about you is sent, and the answers are not saved.
- Nothing else goes online. `online = false` stops all of the above. Web search only opens your browser; Flick itself sends nothing.

`v` reads Windows' clipboard history only while `v` is typed in the bar (a search such as `vlc` passes through `v` on the way). Flick stores none of it and forgets it when the bar closes; copies that password managers ask Windows to leave out of the history stay out. `qr` codes are drawn on your PC and nothing is sent. Passwords and UUIDs (`password`, `uuid`) are made on your PC from Windows' random numbers; they, `lorem` and `hash` are never sent, stored or logged. `v`, `qr`, `password`, `uuid`, `lorem`, `hash`, `snip`, `color` and snippet rows are never counted, kept as recent launches or saved as searches.

Paths you type, and the `dl` and `recent` rows, are never counted, kept as recent launches or saved as searches. `recent` only reads the list Windows already keeps; Flick adds nothing to it. Uninstall reads the installed-programs list on this PC and sends nothing anywhere.

The text of a timer or reminder is kept in `%APPDATA%\Flick\reminders.json` only until it rings or you cancel it; the file is removed when nothing is pending.

Flick reads Firefox's bookmarks and the game launchers' lists of installed games on this PC, and sends them nowhere.

None of `tr`, `def`, timers and reminders, devices (`connect`, `disconnect`, `wifi`) or chores (`empty bin`, `eject`, `flush dns`, `restart explorer`) are counted, kept as recent launches or saved as searches.

Never saved: answers, WSL commands (text after `>`), the text you give a shortcut or type into its list, `help`, and power actions.

To switch it off, set `history = false` in the config: Flick stops recording and deletes `history.json`. `clear history` empties it at any time.
