export const SEARCH_ENGINES = [
  { label: "DuckDuckGo", url: "https://duckduckgo.com/?q={arg}" },
  { label: "Google", url: "https://www.google.com/search?q={arg}" },
  { label: "Bing", url: "https://www.bing.com/search?q={arg}" },
] as const;

export type EngineChoice = "DuckDuckGo" | "Google" | "Bing" | "Custom";

/** Which dropdown entry a saved `search_engine` address belongs to (empty is the default, DuckDuckGo). */
export function engineChoice(url: string): EngineChoice {
  if (url === "") return "DuckDuckGo";
  return SEARCH_ENGINES.find((e) => e.url === url)?.label ?? "Custom";
}

/** Same rule as `config::validate` for `search_engine`; null when fine. */
export function engineError(url: string): string | null {
  return url.startsWith("https://") && url.includes("{arg}") ? null : "Must start with https:// and contain {arg}";
}

export const TEMPERATURE: ReadonlyArray<readonly [string, string]> = [
  ["", "From Windows region"],
  ["c", "°C"],
  ["f", "°F"],
];
