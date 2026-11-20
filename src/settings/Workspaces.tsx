import { useEffect, useState } from "preact/hooks";
import {
  settingsCheckWorkspace,
  settingsDeleteWorkspace,
  settingsItemsFound,
  settingsSaveWorkspace,
  type SettingsState,
  type WorkspaceDef,
} from "./api";
import { ItemPicker } from "./ItemPicker";
import { EditorActions, EditorNotices, ErrorLine, ListLayout } from "./ListEditor";
import type { LeaveGuard } from "./listEditorLogic";
import { useListEditor } from "./useListEditor";
import {
  addItem,
  emptyWsForm,
  isWsDirty,
  moveItem,
  removeItem,
  toWsDef,
  toWsForm,
  type WsForm,
} from "./workspaceForm";
import "./shortcuts.css";
import "./workspaces.css";

export function Workspaces({ state, guard }: { state: SettingsState; guard: LeaveGuard }) {
  const defs = state.config.workspace;
  const ed = useListEditor<WsForm, WorkspaceDef>({
    state,
    guard,
    defs,
    keyOf: (d) => d.name,
    formKey: (f) => f.name,
    empty: emptyWsForm,
    toForm: toWsForm,
    isDirty: isWsDirty,
    write: (original, form) => settingsSaveWorkspace(original, toWsDef(form)),
    normalize: (form) => toWsForm(toWsDef(form)),
    check: (original, form) => settingsCheckWorkspace(original, toWsDef(form)),
    remove: settingsDeleteWorkspace,
  });
  const { open, form, patch } = ed;
  const [found, setFound] = useState<{ key: string; list: boolean[] } | null>(null);
  const itemsKey = JSON.stringify(form.items);

  // Which items can still be rebuilt.
  useEffect(() => {
    if (!open || form.items.length === 0) {
      setFound(null);
      return;
    }
    let stale = false;
    settingsItemsFound(form.items).then(
      (list) => !stale && setFound({ key: itemsKey, list }),
      () => {},
    );
    return () => {
      stale = true;
    };
  }, [open, itemsKey, state]);

  const valid = ed.checkPassed && form.name.trim() !== "";
  const foundList = found?.key === itemsKey ? found.list : null;

  return (
    <ListLayout
      addLabel="+ Add workspace"
      onAdd={() => ed.select(null)}
      rows={defs.map((d) => (
        <button key={d.name} class={ed.isOpen(d.name) ? "sc-row active" : "sc-row"} onClick={() => ed.select(d)}>
          <span class="sc-name">{d.name}</span>
          <span class="sc-about">{d.items.length === 1 ? "1 item" : `${d.items.length} items`}</span>
        </button>
      ))}
      count={defs.length}
      emptyHint="No workspaces yet."
      open={open}
      idle="Pick a workspace, or add a new one."
    >
      <EditorNotices ed={ed} noun="workspace" valid={valid} />

      <label class="sc-field">
        <span>Name</span>
        <input value={form.name} onInput={(e) => patch({ name: e.currentTarget.value })} />
        <small>Type its name in the bar and press Enter to open every item.</small>
      </label>
      <label class="sc-field">
        <span>Other names</span>
        <input value={form.aliases} onInput={(e) => patch({ aliases: e.currentTarget.value })} />
      </label>

      <div class="sc-field">
        <span>Items</span>
        {form.items.length > 0 && (
          <ul class="ws-items">
            {form.items.map((it, i) => (
              <li key={it.provider + "\u0000" + it.id} class="ws-item">
                <span class="ws-title">{it.title}</span>
                {foundList && foundList[i] === false && <span class="ws-missing">Not found</span>}
                <button disabled={i === 0} onClick={() => patch({ items: moveItem(form.items, i, true) })}>
                  Up
                </button>
                <button
                  disabled={i === form.items.length - 1}
                  onClick={() => patch({ items: moveItem(form.items, i, false) })}
                >
                  Down
                </button>
                <button onClick={() => patch({ items: removeItem(form.items, i) })}>Remove</button>
              </li>
            ))}
          </ul>
        )}
        <ItemPicker forWorkspace={true} onPick={(row) => patch({ items: addItem(form.items, row) })} />
      </div>

      {ed.checkError !== null && <ErrorLine>{ed.checkError}</ErrorLine>}
      {ed.saveError !== null && <ErrorLine>{ed.saveError}</ErrorLine>}

      <EditorActions ed={ed} valid={valid} deleteLabel="Delete workspace" />
    </ListLayout>
  );
}
