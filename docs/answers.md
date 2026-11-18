# Quick answers

Sums, units, currencies, times, dates and weather, plus timers, reminders, translating and definitions.

[Back to the overview](../README.md)

## Quick answers

Type a question and the answer appears as the first row, above apps. Enter copies the answer (the bar closes); on a weather answer, Enter opens a web search for the forecast instead. Numbers are shown in your Windows number format, and you may type `,` or `.` as the decimal mark.

| You type | You get |
|---|---|
| `12*7`, `15% of 80`, `sqrt(2)`, `2pi` | The result |
| `5.5 km in m`, `70 f in c`, `3 cups in ml` | The conversion |
| `20 usd in eur`, `$20 in gbp` | The conversion at the European Central Bank's latest rates |
| `20 usd` | The same, into your home currency |
| `time` | The time and weekday here, with today's date |
| `time tokyo` | The time and weekday there, and how far ahead or behind you it is |
| `in 45 days`, `3 weeks ago`, `days until 24 dec` | The date, or the number of days |
| `weather london` | The weather there now; Enter opens a web search for it |
| `weather` | The weather in your `weather_city` |

Only text that is clearly a question gets an answer: a bare number or an app name is never read as maths. The answer comes first, and your other matches follow below as usual. A bare `time`, `weather` or `20 usd` could also be an app's name, so its answer is ranked like one instead of always coming first.

A currency or weather answer needs the internet. The row says `Getting rates…` or `Getting weather for London…` for a moment and turns into the answer by itself. If a city is not found, or you are offline, the row says so. Answers are never saved: not in past searches, not as recent launches, not as pins.

**Web search.** Four searches are built in. Type the name, a space, then what to search for; Enter opens it in your browser.

| You type | Searches |
|---|---|
| `? text` (also `?text`) | Your search engine (DuckDuckGo unless you set `search_engine`) |
| `g text` | Google |
| `yt text` | YouTube |
| `gh text` | GitHub |

**Ask Claude.** `claude text` opens the Claude app in a new chat that starts with your text. Without the app it opens claude.ai in your browser instead. Flick looks for the app when it starts and whenever the config is reloaded, so after installing Claude, pick Reload in the tray menu (or change any setting). A shortcut of your own named `claude` replaces this one.

To add another engine, add a shortcut that opens its search page with `{arg}` where the text goes (see [Shortcuts](shortcuts.md#shortcuts)):

```toml
[[shortcut]]
name = "w"
about = "Search Wikipedia"
open = "https://en.wikipedia.org/w/index.php?search={arg}"
```

A shortcut of yours with the same name (or alias) as a built-in one replaces it. Like all shortcut arguments, what you type after the name is never saved.

The config keys for quick answers, all optional:

- `online`: `false` stops every request; currency and weather rows then say `Online answers are off`. The default is `true`.
- `weather_city`: the city for a bare `weather`, for example `"London"`.
- `currency`: the currency for a bare `20 usd`, for example `"EUR"`. Leave it out to use the currency of your Windows region.
- `temperature_unit`: `"c"` or `"f"`. Leave it out to follow your Windows region.
- `search_engine`: the search page `?` uses (and Enter on a weather answer), starting with `https://` and containing `{arg}`.

## Timers and reminders

- `timer 5m`, `timer 90s`, `timer 1h30m` (a bare number is minutes): the bar shows `Timer 5 min — rings at 14:32`; Enter starts it, counting from that moment.
- `remind 20m pizza`, `remind 18:30 call mum` (today, or tomorrow when that time has passed), `remind tomorrow 9:00 dentist`: Enter sets it. Up to 7 days ahead.
- When one is due, Windows shows a notification from Flick with the note (`Timer done` for a timer).
- `timers` (also `reminders`) lists what is pending with the time left; Enter on one, then Enter again, cancels it.

Flick must be running for a timer or reminder to ring. It starts at login, so a reminder for tomorrow rings as long as you are signed in. One that fell due while Flick was not running shows once at the next start, as `Missed: call mum`.

## Translate and define

`tr guten Morgen` translates between German and English (whichever way fits the text) through MyMemory; `def serendipity` shows up to three meanings per language from English Wiktionary, for English and German words. Enter copies the row. A lookup starts only once you pause typing, and only while `online` is on; with it off the row says `Online answers are off` and Enter opens Settings. Answers are kept in memory only, until Flick quits.
