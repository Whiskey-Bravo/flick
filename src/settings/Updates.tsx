import { useEffect, useState } from "preact/hooks";
import { onSettingsChanged, settingsSet, settingsState } from "./api";
import "./updates.css";
import {
  firstLines,
  lastCheckedText,
  settingsCheckNow,
  settingsInstallUpdate,
  settingsUpdateStatus,
  type UpdateStatus,
} from "./updatesText";

export function Updates() {
  const [status, setStatus] = useState<UpdateStatus | null>(null);
  const [auto, setAuto] = useState(true);
  const [checking, setChecking] = useState(false);
  /** Result of the last Check now press: null until pressed. */
  const [result, setResult] = useState<{ ok: true } | { ok: false; reason: string } | null>(null);

  useEffect(() => {
    const load = () => {
      settingsUpdateStatus().then(setStatus, () => {});
      settingsState().then((s) => setAuto(s.config.check_updates), () => {});
    };
    load();
    const unlisten = onSettingsChanged(load);
    return () => {
      unlisten.then((f) => f());
    };
  }, []);

  const checkNow = () => {
    setChecking(true);
    setResult(null);
    settingsCheckNow().then(
      () => setResult({ ok: true }),
      (e) => setResult({ ok: false, reason: String(e) }),
    ).finally(() => {
      setChecking(false);
      settingsUpdateStatus().then(setStatus, () => {});
    });
  };

  const state = status?.state;
  return (
    <div class="updates">
      <div>Flick {status?.current ?? ""} is installed.</div>
      <div class="muted">{lastCheckedText(status?.last_check ?? null)}</div>
      <label class="row">
        <input
          type="checkbox"
          role="switch"
          checked={auto}
          onChange={(e) => {
            const value = (e.currentTarget as HTMLInputElement).checked;
            setAuto(value);
            settingsSet({ key: "check_updates", value }).catch(() => setAuto(!value));
          }}
        />
        Check automatically
      </label>
      <div class="row">
        <button disabled={checking} onClick={checkNow}>
          Check now
        </button>
        {result?.ok && state?.state !== "available" && <span class="muted">You have the latest version.</span>}
        {result && !result.ok && <span class="error">Could not check: {result.reason}</span>}
      </div>
      {(state?.state === "available" || state?.state === "failed" || state?.state === "downloading") && (
        <div class="update-box">
          <button
            class="primary"
            disabled={state.state === "downloading"}
            onClick={() => settingsInstallUpdate().catch(() => {})}
          >
            {state.state === "downloading" ? "Downloading update…" : `Update to ${state.version}`}
          </button>
          {state.state === "failed" && <div class="error">Update failed: {state.reason}</div>}
          {state.state === "available" && (
            <ul class="notes">
              {firstLines(state.notes).map((l, i) => (
                <li key={i}>{l}</li>
              ))}
            </ul>
          )}
        </div>
      )}
    </div>
  );
}
