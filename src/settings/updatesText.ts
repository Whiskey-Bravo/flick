import { invoke } from "@tauri-apps/api/core";

export type UpdateState =
  | { state: "idle" }
  | { state: "available"; version: string; notes: string }
  | { state: "downloading"; version: string }
  | { state: "failed"; version: string; reason: string };

export type UpdateStatus = {
  state: UpdateState;
  /** RFC 3339, null until a check has succeeded. */
  last_check: string | null;
  current: string;
};

export const settingsUpdateStatus = () => invoke<UpdateStatus>("settings_update_status");
/** Rejects with the reason when the check could not be made. */
export const settingsCheckNow = () => invoke<void>("settings_check_now");
export const settingsInstallUpdate = () => invoke<void>("settings_install_update");

/** The first `max` non-empty lines of release notes. */
export function firstLines(notes: string, max = 3): string[] {
  return notes
    .split(/\r?\n/)
    .map((l) => l.trim())
    .filter((l) => l !== "")
    .slice(0, max);
}

/** "Last checked: <local time>" or "Not checked yet". */
export function lastCheckedText(iso: string | null): string {
  if (iso === null) return "Not checked yet";
  const d = new Date(iso);
  return Number.isNaN(d.getTime()) ? "Not checked yet" : `Last checked: ${d.toLocaleString()}`;
}
