export type Tab = "general" | "appearance" | "search" | "answers" | "shortcuts" | "snippets" | "workspaces" | "hotkeys" | "updates";

/** The tab names as the window shows them; the key order is the tab order. */
export const TAB_LABELS: Record<Tab, string> = {
  general: "General",
  appearance: "Appearance",
  search: "Search",
  answers: "Quick answers",
  shortcuts: "Shortcuts",
  snippets: "Snippets",
  workspaces: "Workspaces",
  hotkeys: "Hotkeys",
  updates: "Updates",
};

export const TABS = Object.keys(TAB_LABELS) as Tab[];

/** The tab `step` places after `tab` (Ctrl+Tab is +1, Ctrl+Shift+Tab is -1), wrapping at the ends. */
export function nextTab(tab: Tab, step: 1 | -1): Tab {
  const i = TABS.indexOf(tab);
  return TABS[(i + step + TABS.length) % TABS.length];
}
