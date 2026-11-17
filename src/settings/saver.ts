import { useEffect, useState } from "preact/hooks";
import { settingsSet, type Setting, type SettingsState } from "./api";

/**
 * Applies one setting at once. A failure is kept under the control's `field`
 * until the next `settings:changed` (a new `state`) brings the saved value back.
 */
export function useSaver(state: SettingsState) {
  const [errors, setErrors] = useState<Record<string, string>>({});
  useEffect(() => setErrors({}), [state]);
  const save = async (field: string, setting: Setting) => {
    try {
      await settingsSet(setting);
      setErrors((e) => (e[field] ? { ...e, [field]: "" } : e));
    } catch (err) {
      setErrors((e) => ({ ...e, [field]: String(err) }));
    }
  };
  return { errors, save };
}
