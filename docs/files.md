# Files, clipboard and capture

Finding and browsing files, clipboard history, QR codes, snips, the colour picker, games and bookmarks.

[Back to the overview](../README.md)

## Files and folders

Type part of a file or folder name. Search is by name only, not by content. Two characters find only an exact name (`db`); the start of a name or word needs three, any other part of the name four. Enter opens the file or folder; Ctrl+Enter shows it selected in Explorer.

### Browsing by path

Type a path to look inside a folder: a drive (`C:\`), your user folder (`~\`, as in `~\Documents\`) or a network share (`\\server\share\`). Folders come first, then files; hidden and system entries are skipped. Typing more after the last `\` narrows the list, and Tab completes the selected name. While a folder is being read the list says `Reading…`; a folder that does not exist says `Folder not found` and one that cannot be read says `Can't open this folder`.

`dl` lists the 20 newest files in your Downloads folder, and `recent` lists what Windows' own Recent list holds. Add text after either to filter it (`dl invoice`).

The list shows up to 50 results and scrolls. Files take at most five of every eight rows, so folders, apps and other results stay in view.

<details>
<summary>What is searched, and how the list is kept up to date</summary>

- The searched folder is your whole user folder, or the ones listed in `file_roots` in the config.
- Hidden and system files, names starting with `.`, and build, package or tool folders (`node_modules`, `target`, `dist`, `build`, `__pycache__`, `venv`, `AppData`, `vendor`, `site-packages`, `Pods`, `bower_components`, `obj`) and Python distributions (`miniconda3`, `anaconda3`, `miniforge3`, `mambaforge`) are skipped.
- At most 1 000 000 entries are indexed (shared equally between the folders), down to nine folder levels. More than six levels down (counted from Desktop, Documents and the like), only the exact name finds a file or folder.
- The list is built in the background when Flick starts, kept up to date as files are added, removed or renamed, and rebuilt in the background when something changed (at most every 30 minutes, and at least every 2 hours). A file that was moved since says "file not found".

</details>

## Clipboard and capture

`v` lists the text items of Windows' clipboard history (the one Win+V shows), newest first, up to 50. Type after it to filter (`v invoice`). Enter pastes the item into the window you were in before the bar opened. When it can't be sure the paste lands right, it only copies: when that window is gone, runs as administrator or does not come to the front, when the clipboard did not take the item, or when you open the bar again in the meantime. Ctrl+Enter, Ctrl+Shift+Enter, or Copy in the row's menu, only copy. This needs Windows' Clipboard history setting; when it is off the row says `Clipboard history is off`, `Turn it on in Settings`, and Enter opens that setting.

`qr` draws the clipboard text as a QR code in the bar; `qr some text` draws the typed text instead. Enter copies the text. Save as picture in the code's menu (Tab or right-click), or Ctrl+S, opens the Windows Save window and writes the code as a PNG picture, large enough to print.

`snip` (also `screenshot`) opens Windows' snip tool; the snip lands on the clipboard.

`color` (also `colour` or `picker`) freezes the monitor under the mouse and shows a magnifier. Click to copy the colour under the cursor as `#RRGGBB`; Esc or right-click cancels.

## Games and bookmarks

Installed games show up in normal search by name: Steam games (in every Steam library, on any drive), Epic games, Rockstar games and Minecraft (the Minecraft Launcher). Enter starts the game through its launcher; Open file location in the row's menu shows the install folder. A game that is still downloading for the first time is not listed; one waiting for an update is. The lists are read in the background when Flick starts and again every 10 minutes, so a game you just installed shows up within minutes (or at once with Reload in the tray menu).

Bookmarks of Firefox's default profile show up in normal search by title and by site. Enter opens the bookmark in your default browser; Copy in the row's menu copies its address. Flick reads a copy of Firefox's bookmark database, so this works while Firefox is open, and reads it again when you open the bar and the last read is more than five minutes old. Only Firefox bookmarks are read: no open tabs, no other browsers.

Games and bookmarks are learned, pinned and kept as recent launches like apps.
