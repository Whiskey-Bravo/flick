import { invoke } from "@tauri-apps/api/core";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";

export type ShortcutDef = {
  name: string;
  aliases: string[];
  about: string;
  run?: string;
  wsl?: string;
  open?: string;
  terminal?: boolean;
  admin: boolean;
  pick?: string;
  choices: string[];
};

export type Config = {
  hotkey: string;
  wsl_distro: string;
  file_roots: string[];
  history: boolean;
  online: boolean;
  weather_city: string;
  currency: string;
  temperature_unit: string;
  search_engine: string;
  check_updates: boolean;
  shortcut: ShortcutDef[];
};

export type SettingsState = {
  /** The last good config. */
  config: Config;
  /** Error of the file as it is now, if it does not parse or validate. */
  file_error: string | null;
  /** Usable WSL distros; null when WSL is absent or not detected yet. */
  distros: string[] | null;
  start_at_login: boolean;
  version: string;
};

/** One setting; an empty string or list means "unset". */
export type Setting =
  | { key: "hotkey"; value: string }
  | { key: "wsl_distro"; value: string }
  | { key: "file_roots"; value: string[] }
  | { key: "history"; value: boolean }
  | { key: "online"; value: boolean }
  | { key: "weather_city"; value: string }
  | { key: "currency"; value: string }
  | { key: "temperature_unit"; value: string }
  | { key: "search_engine"; value: string }
  | { key: "check_updates"; value: boolean };

export type ShortcutField = "name" | "aliases" | "target" | "terminal" | "admin" | "pick" | "choices";

export type FieldError = { field: ShortcutField; message: string };

/** Why a shortcut could not be saved: a rule of one field, or the file itself. */
export type SaveError = FieldError | string;

export type PinRow = {
  provider: string;
  id: string;
  title: string;
  subtitle: string;
  icon: string;
  /** False when the provider can no longer rebuild the item; `title` is then the stored id. */
  found: boolean;
};

export const settingsState = () => invoke<SettingsState>("settings_state");
export const settingsSet = (setting: Setting) => invoke<void>("settings_set", { setting });
export const settingsSaveShortcut = (original: string | null, def: ShortcutDef) =>
  invoke<void>("settings_save_shortcut", { original, def });
export const settingsCheckShortcut = (original: string | null, def: ShortcutDef) =>
  invoke<FieldError | null>("settings_check_shortcut", { original, def });
export const settingsDeleteShortcut = (name: string) => invoke<void>("settings_delete_shortcut", { name });
export const settingsSetStartAtLogin = (on: boolean) => invoke<boolean>("settings_set_start_at_login", { on });
export const settingsClearHistory = () => invoke<void>("settings_clear_history");
export const settingsPickFolder = () => invoke<string | null>("settings_pick_folder");
export const settingsPins = () => invoke<PinRow[]>("settings_pins");
export const settingsMovePin = (provider: string, id: string, up: boolean) =>
  invoke<void>("settings_move_pin", { provider, id, up });
/** Resolves to the city's label ("Paris, France"); rejects with a short message. */
export const settingsCheckCity = (city: string) => invoke<string>("settings_check_city", { city });
/** The currencies `currency` accepts. */
export const settingsCurrencies = () => invoke<string[]>("settings_currencies");
/** Pins or unpins an item; resolves to whether it is now pinned. */
export const togglePin = (provider: string, id: string) => invoke<boolean>("toggle_pin", { provider, id });
export const settingsOpenFile = () => invoke<void>("settings_open_file");

/** Runs `handler` after every config reload. */
export const onSettingsChanged = (handler: () => void): Promise<UnlistenFn> =>
  listen("settings:changed", handler);
