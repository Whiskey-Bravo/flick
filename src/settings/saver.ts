import { useEffect, useRef, useState } from "preact/hooks";
import { settingsSet, type Setting, type SettingsState } from "./api";
import { sameConfig } from "./sameConfig";

/**
 * Applies one setting at once. A failure is kept under the control's `field`
 * until a `settings:changed` brings a different config. A reload that leaves the config
 * as it was (a failed hotkey restores the old one) keeps the message.
 */
export function useSaver(state: SettingsState) {
  const [errors, setErrors] = useState<Record<string, string>>({});
  const last = useRef(state.config);
  useEffect(() => {
    if (!sameConfig(last.current, state.config)) setErrors({});
    last.current = state.config;
  }, [state]);
  /** Saves one setting; resolves true when it was saved. */
  const save = async (field: string, setting: Setting): Promise<boolean> => {
    try {
      await settingsSet(setting);
      setErrors((e) => (e[field] ? { ...e, [field]: "" } : e));
      return true;
    } catch (err) {
      setErrors((e) => ({ ...e, [field]: String(err) }));
      return false;
    }
  };
  return { errors, save };
}
