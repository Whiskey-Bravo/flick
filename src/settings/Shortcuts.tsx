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
import { EditorActions, EditorNotices, ErrorLine, ListLayout } from "./ListEditor";
import { errorMessage, type LeaveGuard } from "./listEditorLogic";
import { disabledReasons, emptyForm, iconFromRow, isDirty, toDef, toForm, type Form, type Kind } from "./shortcutForm";
import { useListEditor } from "./useListEditor";
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

export function Shortcuts({ state, guard }: { state: SettingsState; guard: LeaveGuard }) {
  const defs = state.config.shortcut;
  const wsl = state.distros !== null;
  const [appIcon, setAppIcon] = useState(false);
  const ed = useListEditor<Form, ShortcutDef, string | FieldError, FieldError>({
    state,
    guard,
    defs,
    keyOf: (d) => d.name,
    formKey: (f) => f.name,
    empty: emptyForm,
    toForm,
    isDirty,
    write: (original, form) => settingsSaveShortcut(original, toDef(form)),
    normalize: (form) => toForm(toDef(form)),
    // A rule of one field shows under that field; anything else under the form.
    saveError: (e) => (typeof e === "object" && e !== null && "field" in e ? (e as FieldError) : errorMessage(e)),
    check: (original, form) => settingsCheckShortcut(original, toDef(form)),
    remove: settingsDeleteShortcut,
    onShow: () => setAppIcon(false),
  });
  const { open, form, saveError } = ed;

  const valid = ed.checkPassed && form.name.trim() !== "";

  const set = <K extends keyof Form>(k: K, v: Form[K]) => ed.patch({ [k]: v } as Partial<Form>);

  const reasons = disabledReasons(form, wsl);
  const shown = (field: ShortcutField) => {
    const e = saveError && typeof saveError === "object" ? saveError : ed.checkError;
    return e && e.field === field ? <ErrorLine>{e.message}</ErrorLine> : null;
  };

  const moreRef = useRef<HTMLDetailsElement>(null);
  const errField = saveError && typeof saveError === "object" ? saveError.field : ed.checkError?.field;
  useEffect(() => {
    if (moreRef.current && errField && ["admin", "pick", "choices"].includes(errField)) {
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
    <ListLayout
      addLabel="+ Add shortcut"
      onAdd={() => ed.select(null)}
      rows={defs.map((d) => (
        <button key={d.name} class={ed.isOpen(d.name) ? "sc-row active" : "sc-row"} onClick={() => ed.select(d)}>
          {someIcon && <OwnIcon icon={d.icon ?? ""} />}
          <span class="sc-text">
            <span class="sc-name">{d.name}</span>
            <span class="sc-about">{d.about || d.run || d.wsl || d.open}</span>
          </span>
        </button>
      ))}
      count={defs.length}
      emptyHint="No shortcuts yet."
      open={open}
      idle="Pick a shortcut, or add a new one."
    >
      <EditorNotices ed={ed} noun="shortcut" valid={valid} />

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
          <button onClick={() => setAppIcon(!appIcon)}>From an app...</button>
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
        <small>A program, a shortcut, or an .ico, .png or .jpg file.</small>
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

      {typeof saveError === "string" && <ErrorLine>{saveError}</ErrorLine>}

      <EditorActions ed={ed} valid={valid} deleteLabel="Delete" />
    </ListLayout>
  );
}
