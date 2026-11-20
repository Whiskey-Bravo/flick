import {
  settingsCheckSnippet,
  settingsDeleteSnippet,
  settingsSaveSnippet,
  type SettingsState,
  type SnippetDef,
} from "./api";
import { EditorActions, EditorNotices, ErrorLine, ListLayout } from "./ListEditor";
import type { LeaveGuard } from "./listEditorLogic";
import {
  emptySnippetForm,
  isSnippetDirty,
  preview,
  toSnippetDef,
  toSnippetForm,
  type SnippetForm,
} from "./snippetForm";
import { useListEditor } from "./useListEditor";
import "./shortcuts.css";

export function Snippets({ state, guard }: { state: SettingsState; guard: LeaveGuard }) {
  const defs = state.config.snippet;
  const ed = useListEditor<SnippetForm, SnippetDef>({
    state,
    guard,
    defs,
    keyOf: (d) => d.name,
    formKey: (f) => f.name,
    empty: emptySnippetForm,
    toForm: toSnippetForm,
    isDirty: isSnippetDirty,
    write: (original, form) => settingsSaveSnippet(original, toSnippetDef(form)),
    normalize: (form) => toSnippetForm(toSnippetDef(form)),
    check: (original, form) => settingsCheckSnippet(original, toSnippetDef(form)),
    remove: settingsDeleteSnippet,
  });
  const { form, patch } = ed;

  /** Nothing typed yet: no complaint about the empty fields. */
  const blank = form.name === "" && form.text === "";
  // Unlike the other pages, an empty name is left to the live check.
  const valid = ed.checkPassed;
  const shownCheck = blank ? null : ed.checkError;

  return (
    <ListLayout
      addLabel="+ Add snippet"
      onAdd={() => ed.select(null)}
      rows={defs.map((d) => (
        <button key={d.name} class={ed.isOpen(d.name) ? "sc-row active" : "sc-row"} onClick={() => ed.select(d)}>
          <span class="sc-name">{d.name}</span>
          <span class="sc-about">{preview(d.text)}</span>
        </button>
      ))}
      count={defs.length}
      emptyHint="No snippets yet."
      open={ed.open}
      idle="A snippet is a text you use often: an address, a signature, a reply. Type its name in the bar and press Enter to paste it into the window you were in."
    >
      <EditorNotices ed={ed} noun="snippet" valid={valid} />

      <label class="sc-field">
        <span>Name</span>
        <input value={form.name} onInput={(e) => patch({ name: e.currentTarget.value })} />
        <small>One word. Type it in the bar and press Enter to paste the text; Ctrl+Enter only copies it.</small>
      </label>
      <label class="sc-field">
        <span>Text</span>
        <textarea class="sn-text" rows={8} value={form.text} onInput={(e) => patch({ text: e.currentTarget.value })} />
        <small>Saved in your config file as plain text, so not the place for passwords.</small>
      </label>

      {shownCheck !== null && <ErrorLine>{shownCheck}</ErrorLine>}
      {ed.saveError !== null && <ErrorLine>{ed.saveError}</ErrorLine>}

      <EditorActions ed={ed} valid={valid} deleteLabel="Delete snippet" />
    </ListLayout>
  );
}
