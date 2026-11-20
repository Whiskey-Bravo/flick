import type { ComponentChildren } from "preact";
import { useEffect, useRef } from "preact/hooks";
import type { Choice } from "./listEditorLogic";
import type { ListEditor } from "./useListEditor";

/** The list on the left, and the editor (or a hint while none is open) on the right. */
export function ListLayout(p: {
  listClass?: string;
  addLabel: string;
  onAdd: () => void;
  /** The list rows. */
  rows: ComponentChildren;
  /** How many rows there are; none shows `emptyHint`. */
  count: number;
  emptyHint: string;
  /** Below the rows, inside the list. */
  listEnd?: ComponentChildren;
  open: boolean;
  /** The right side while no entry is open. */
  idle: ComponentChildren;
  /** The open editor. */
  children: ComponentChildren;
}) {
  return (
    <div class="sc">
      <div class={p.listClass ? `sc-list ${p.listClass}` : "sc-list"}>
        <button class="sc-add" onClick={p.onAdd}>
          {p.addLabel}
        </button>
        {p.rows}
        {p.count === 0 && <p class="sc-hint">{p.emptyHint}</p>}
        {p.listEnd}
      </div>

      {!p.open ? <div class="sc-editor sc-empty">{p.idle}</div> : <div class="sc-editor">{p.children}</div>}
    </div>
  );
}

function LeaveAsk({ noun, answer, canSave }: { noun: string; answer: (c: Choice) => void; canSave: boolean }) {
  const ref = useRef<HTMLDivElement>(null);
  // The question can sit above the scrolled editor: bring it into view.
  useEffect(() => {
    ref.current?.scrollIntoView({ block: "nearest" });
  }, []);
  return (
    <div class="sc-ask" role="alertdialog" ref={ref}>
      <span>{`This ${noun} has unsaved changes.`}</span>
      <button disabled={!canSave} onClick={() => answer("save")}>
        Save
      </button>
      <button onClick={() => answer("discard")}>Discard</button>
      <button onClick={() => answer("stay")}>Stay</button>
    </div>
  );
}

/** The top of an open editor: the Save / Discard / Stay question and the file-changed note. */
export function EditorNotices({ ed, noun, valid }: { ed: ListEditor; noun: string; valid: boolean }) {
  return (
    <>
      {ed.asking && <LeaveAsk noun={noun} answer={ed.asking} canSave={valid && !ed.saving} />}
      {ed.changedNote && <p class="sc-note">The config file changed. Your unsaved edit is kept.</p>}
    </>
  );
}

/** An error line under a field or the form. */
export function ErrorLine({ children }: { children: ComponentChildren }) {
  return (
    <p class="sc-error" role="alert">
      {children}
    </p>
  );
}

/** Save, and (with `deleteLabel`) Delete with its confirm for an entry already in the file. */
export function EditorActions({ ed, valid, deleteLabel }: { ed: ListEditor; valid: boolean; deleteLabel?: string }) {
  return (
    <div class="sc-actions">
      <button class="primary" disabled={!valid || !ed.dirty || ed.saving} onClick={() => void ed.doSave()}>
        Save
      </button>
      {deleteLabel !== undefined &&
        ed.original !== null &&
        (ed.confirmDelete ? (
          <>
            <span>Delete "{ed.original}"?</span>
            <button class="danger" onClick={() => void ed.remove()}>
              Delete
            </button>
            <button onClick={() => ed.setConfirmDelete(false)}>Cancel</button>
          </>
        ) : (
          <button onClick={() => ed.setConfirmDelete(true)}>{deleteLabel}</button>
        ))}
    </div>
  );
}
