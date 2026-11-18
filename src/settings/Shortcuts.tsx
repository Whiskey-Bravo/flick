import { useEffect, useRef, useState } from "preact/hooks";
import { serverUrl } from "../icons";
import {
  settingsCheckShortcut,
  settingsDeleteShortcut,
  settingsPickFolder,
  settingsPickIcon,
  settingsSaveShortcut,
  type FieldError,
  type SettingsState,
  type ShortcutDef,
  type ShortcutField,
} from "./api";
import { ItemPicker } from "./ItemPicker";
import { disabledReasons, emptyForm, iconFromRow, isDirty, toDef, toForm, type Form, type Kind } from "./shortcutForm";
import "./shortcuts.css";

/** A shortcut's own icon; nothing shows while it is empty or can't be read. */
function OwnIcon({ icon }: { icon: string }) {
  const value = icon.trim();
  if (value === "") return <span class="sc-icon" />;
  return (
    <img
      // A new value is a new image: a hidden one (unreadable) must not stay hidden.
      key={value}
      class="sc-icon"
      src={serverUrl(`file:${value}`, window.devicePixelRatio || 1)}
      alt=""
      onError={(e) => {
        e.currentTarget.style.visibility = "hidden";
      }}
    />
  );
}

/** Asks the editor whether the tab may be left; resolves false when the user chose Stay. */
export type LeaveGuard = { current: (() => Promise<boolean>) | null };

type Choice = "save" | "discard" | "stay";

const TARGET_LABEL: Record<Kind, string> = {
  open: "Website, file, folder or program",
  run: "Windows command",
  wsl: "WSL command",
};

const KIND_LABEL: Record<Kind, string> = {
  open: "Open a website or file",
  run: "Run a Windows command",
  wsl: "Run a WSL command",
};

const message = (e: unknown) => (typeof e === "string" ? e : e instanceof Error ? e.message : "Could not save.");

export function Shortcuts({ state, guard }: { state: SettingsState; guard: LeaveGuard }) {
  const defs = state.config.shortcut;
  const wsl = state.distros !== null;

  /** The name the open shortcut has in the file; null while adding a new one. */
  const [original, setOriginal] = useState<string | null>(null);
  const [open, setOpen] = useState(false);
  const [form, setForm] = useState<Form>(emptyForm());
  const [saved, setSaved] = useState<Form | null>(null);
  const [check, setCheck] = useState<{ key: string; error: FieldError | null } | null>(null);
  const [saveError, setSaveError] = useState<string | FieldError | null>(null);
  const [changedNote, setChangedNote] = useState(false);
  const [asking, setAsking] = useState<((c: Choice) => void) | null>(null);
  const [confirmDelete, setConfirmDelete] = useState(false);
  const [appIcon, setAppIcon] = useState(false);
  const askRef = useRef<HTMLDivElement>(null);
  // The question can sit above the scrolled editor: bring it into view.
  useEffect(() => {
    if (asking) askRef.current?.scrollIntoView({ block: "nearest" });
  }, [asking !== null]);

  const dirty = open && isDirty(saved, form);
  const key = JSON.stringify(form);

  // Live check, debounced.
  useEffect(() => {
    if (!open) return;
    let stale = false;
    const t = setTimeout(() => {
      settingsCheckShortcut(original, toDef(form)).then(
        (error) => !stale && setCheck({ key, error }),
        () => {},
      );
    }, 200);
    return () => {
      stale = true;
      clearTimeout(t);
    };
  }, [open, original, key]);

  // The file changed: follow it unless there is an unsaved edit.
  const latest = useRef({ open, dirty, saved, original });
  latest.current = { open, dirty, saved, original };
  useEffect(() => {
    const { open, dirty, saved, original } = latest.current;
    if (!open || original === null) return;
    const def = defs.find((d) => d.name === original);
    const next = def ? toForm(def) : null;
    if (next && saved && !isDirty(saved, next)) return;
    if (dirty) {
      setChangedNote(true);
    } else if (next) {
      setSaved(next);
      setForm(next);
    } else {
      setOpen(false);
      setOriginal(null);
    }
  }, [state]);

  const doSave = async (): Promise<boolean> => {
    setSaveError(null);
    try {
      await settingsSaveShortcut(original, toDef(form));
      const norm = toForm(toDef(form));
      setSaved(norm);
      setForm(norm);
      setOriginal(norm.name);
      setChangedNote(false);
      return true;
    } catch (e) {
      setSaveError(typeof e === "object" && e !== null && "field" in e ? (e as FieldError) : message(e));
      return false;
    }
  };

  const valid = check?.key === key && check.error === null && form.name.trim() !== "";

  /** Resolves true when the editor may be left or replaced. */
  // A second leave request while the question is open (a second click on
  // the window's X) waits for the same answer instead of replacing it.
  const pending = useRef<Promise<boolean> | null>(null);
  const mayLeave = (): Promise<boolean> => {
    if (!dirty) return Promise.resolve(true);
    if (pending.current) return pending.current;
    const p = (async () => {
      const c = await new Promise<Choice>((resolve) => setAsking(() => resolve));
      setAsking(null);
      if (c === "stay") return false;
      if (c === "save") return await doSave();
      return true;
    })();
    pending.current = p;
    void p.finally(() => (pending.current = null));
    return p;
  };
  guard.current = mayLeave;
  useEffect(() => () => void (guard.current = null), []);

  const show = (def: ShortcutDef | null) => {
    const f = def ? toForm(def) : emptyForm();
    setOpen(true);
    setOriginal(def ? def.name : null);
    setSaved(def ? f : null);
    setForm(f);
    setCheck(null);
    setSaveError(null);
    setAppIcon(false);
    setChangedNote(false);
    setConfirmDelete(false);
  };

  const select = async (def: ShortcutDef | null) => {
    if (def && open && original === def.name) return;
    if (await mayLeave()) show(def);
  };

  const remove = async () => {
    if (original === null) return;
    try {
      await settingsDeleteShortcut(original);
      setOpen(false);
      setOriginal(null);
      setSaved(null);
      setForm(emptyForm());
    } catch (e) {
      setSaveError(message(e));
    }
    setConfirmDelete(false);
  };

  const set = <K extends keyof Form>(k: K, v: Form[K]) => {
    setForm((f) => ({ ...f, [k]: v }));
    setSaveError(null);
  };

  const reasons = disabledReasons(form, wsl);
  const shown = (field: ShortcutField) => {
    const e = saveError && typeof saveError === "object" ? saveError : check?.key === key ? check.error : null;
    return e && e.field === field ? (
      <p class="sc-error" role="alert">
        {e.message}
      </p>
    ) : null;
  };

  const moreRef = useRef<HTMLDetailsElement>(null);
  const errField = saveError && typeof saveError === "object" ? saveError.field : check?.key === key ? check.error?.field : undefined;
  useEffect(() => {
    if (moreRef.current && errField && ["admin", "terminal", "pick", "choices"].includes(errField)) {
      moreRef.current.open = true;
    }
  }, [errField, open]);

  const pickFolder = () => {
    settingsPickFolder().then(
      (p) => p && set("pick", p),
      () => {},
    );
  };

  const pickIcon = () => {
    settingsPickIcon().then(
      (p) => p && set("icon", p),
      () => {},
    );
  };
  const someIcon = defs.some((d) => (d.icon ?? "").trim() !== "");

  return (
    <div class="sc">
      <div class="sc-list">
        <button class="sc-add" onClick={() => select(null)}>
          + Add shortcut
        </button>
        {defs.map((d) => (
          <button
            key={d.name}
            class={open && original === d.name ? "sc-row active" : "sc-row"}
            onClick={() => select(d)}
          >
            {someIcon && <OwnIcon icon={d.icon ?? ""} />}
            <span class="sc-text">
              <span class="sc-name">{d.name}</span>
              <span class="sc-about">{d.about || d.run || d.wsl || d.open}</span>
            </span>
          </button>
        ))}
        {defs.length === 0 && <p class="sc-hint">No shortcuts yet.</p>}
      </div>

      {!open ? (
        <div class="sc-editor sc-empty">Pick a shortcut, or add a new one.</div>
      ) : (
        <div class="sc-editor">
          {asking && (
            <div class="sc-ask" role="alertdialog" ref={askRef}>
              <span>This shortcut has unsaved changes.</span>
              <button disabled={!valid} onClick={() => asking("save")}>
                Save
              </button>
              <button onClick={() => asking("discard")}>Discard</button>
              <button onClick={() => asking("stay")}>Stay</button>
            </div>
          )}
          {changedNote && <p class="sc-note">The config file changed. Your unsaved edit is kept.</p>}

          <label class="sc-field">
            <span>Name</span>
            <input value={form.name} onInput={(e) => set("name", e.currentTarget.value)} />
            {shown("name")}
          </label>
          <label class="sc-field">
            <span>Also answers to</span>
            <input value={form.aliases} onInput={(e) => set("aliases", e.currentTarget.value)} />
            <small>Other words that find it, separated by spaces or commas.</small>
            {shown("aliases")}
          </label>
          <label class="sc-field">
            <span>Description</span>
            <input value={form.about} onInput={(e) => set("about", e.currentTarget.value)} />
          </label>

          <div class="sc-field">
            <span>Icon</span>
            <div class="sc-pick">
              <OwnIcon icon={form.icon} />
              <input
                aria-label="Icon"
                value={form.icon}
                placeholder="None: the usual icon"
                onInput={(e) => set("icon", e.currentTarget.value)}
              />
              <button onClick={pickIcon}>Browse...</button>
              <button onClick={() => setAppIcon(!appIcon)}>Use an app's icon</button>
              <button disabled={form.icon === ""} onClick={() => set("icon", "")}>
                Clear
              </button>
            </div>
            {appIcon && (
              <ItemPicker
                forWorkspace={false}
                placeholder="Search for the app whose icon to use"
                only={(row) => iconFromRow(row.icon) !== null}
                onPick={(row) => {
                  const icon = iconFromRow(row.icon);
                  if (icon !== null) set("icon", icon);
                  setAppIcon(false);
                }}
              />
            )}
            <small>A program, a shortcut, or an .ico or .png file.</small>
          </div>

          <div class="sc-field" role="radiogroup" aria-label="What it does">
            <span>What it does</span>
            {(["open", "run", "wsl"] as Kind[]).map((k) => (
              <label key={k} class="sc-radio">
                <input
                  type="radio"
                  name="kind"
                  checked={form.kind === k}
                  disabled={k === "wsl" && !wsl && form.kind !== "wsl"}
                  onChange={() => set("kind", k)}
                />
                {KIND_LABEL[k]}
                {k === "wsl" && !wsl && <small> ({reasons.wsl})</small>}
              </label>
            ))}
          </div>

          <label class="sc-field">
            <span>{TARGET_LABEL[form.kind]}</span>
            <input value={form.target} onInput={(e) => set("target", e.currentTarget.value)} />
            <small>{"{arg}"} is what you type after the name</small>
            {shown("target")}
          </label>

          <details class="sc-more" ref={moreRef}>
            <summary>More options</summary>
            <label class="sc-check">
              <input
                type="checkbox"
                checked={form.admin}
                disabled={!!reasons.admin}
                onChange={(e) => set("admin", e.currentTarget.checked)}
              />
              Run as administrator
              {reasons.admin && <small> ({reasons.admin})</small>}
            </label>
            {shown("admin")}

            <label class="sc-field">
              <span>Show a terminal</span>
              <select
                value={form.terminal === null ? "" : form.terminal ? "yes" : "no"}
                disabled={!!reasons.terminal}
                onChange={(e) => {
                  const v = e.currentTarget.value;
                  set("terminal", v === "" ? null : v === "yes");
                }}
              >
                <option value="">Default for this kind</option>
                <option value="yes">Yes</option>
                <option value="no">No</option>
              </select>
              {reasons.terminal && <small>({reasons.terminal})</small>}
              {shown("terminal")}
            </label>

            <div class="sc-field">
              <span>Choices</span>
              <label class="sc-radio">
                <input type="radio" name="list" checked={form.listMode === "none"} onChange={() => set("listMode", "none")} />
                None
              </label>
              <label class="sc-radio">
                <input
                  type="radio"
                  name="list"
                  checked={form.listMode === "pick"}
                  disabled={!!reasons.pick}
                  onChange={() => set("listMode", "pick")}
                />
                Choose from a folder's sub-folders
                {reasons.pick && <small> ({reasons.pick})</small>}
              </label>
              {form.listMode === "pick" && (
                <div class="sc-pick">
                  <input value={form.pick} onInput={(e) => set("pick", e.currentTarget.value)} />
                  <button onClick={pickFolder}>Browse...</button>
                </div>
              )}
              {shown("pick")}
              <label class="sc-radio">
                <input
                  type="radio"
                  name="list"
                  checked={form.listMode === "choices"}
                  disabled={!!reasons.choices}
                  onChange={() => set("listMode", "choices")}
                />
                Choose from a list
                {reasons.choices && <small> ({reasons.choices})</small>}
              </label>
              {form.listMode === "choices" && (
                <textarea
                  rows={4}
                  value={form.choices}
                  placeholder="One per line"
                  onInput={(e) => set("choices", e.currentTarget.value)}
                />
              )}
              {shown("choices")}
              {form.listMode !== "none" && (
                <small>Use {"{name}"} (or {"{path}"} for a folder) in the target above.</small>
              )}
            </div>
          </details>

          {typeof saveError === "string" && (
            <p class="sc-error" role="alert">
              {saveError}
            </p>
          )}

          <div class="sc-actions">
            <button class="primary" disabled={!valid || !dirty} onClick={() => void doSave()}>
              Save
            </button>
            {original !== null &&
              (confirmDelete ? (
                <>
                  <span>Delete "{original}"?</span>
                  <button class="danger" onClick={() => void remove()}>
                    Delete
                  </button>
                  <button onClick={() => setConfirmDelete(false)}>Cancel</button>
                </>
              ) : (
                <button onClick={() => setConfirmDelete(true)}>Delete</button>
              ))}
          </div>
        </div>
      )}
    </div>
  );
}
