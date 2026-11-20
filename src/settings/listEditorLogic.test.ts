import { describe, expect, it } from "vitest";
import { askToLeave, errorMessage, followFile, type Choice } from "./listEditorLogic";

/** A leave question the test answers by hand. */
function question() {
  const log: string[] = [];
  let answer = (_: Choice) => {};
  let asked = 0;
  const ask = () => {
    asked++;
    log.push("ask");
    return new Promise<Choice>((r) => (answer = r));
  };
  const answered = () => log.push("answered");
  let saves = 0;
  let saveResult = true;
  const save = async () => {
    saves++;
    log.push("save");
    return saveResult;
  };
  return {
    log,
    ask,
    answered,
    save,
    answer: (c: Choice) => answer(c),
    get asked() {
      return asked;
    },
    get saves() {
      return saves;
    },
    failSave: () => (saveResult = false),
  };
}

describe("askToLeave", () => {
  it("lets a clean editor go without asking", async () => {
    const q = question();
    expect(await askToLeave({ current: null }, false, q.ask, q.answered, q.save)).toBe(true);
    expect(q.asked).toBe(0);
  });

  it("Stay keeps the editor and saves nothing", async () => {
    const q = question();
    const p = askToLeave({ current: null }, true, q.ask, q.answered, q.save);
    q.answer("stay");
    expect(await p).toBe(false);
    expect(q.saves).toBe(0);
  });

  it("Discard leaves without saving", async () => {
    const q = question();
    const p = askToLeave({ current: null }, true, q.ask, q.answered, q.save);
    q.answer("discard");
    expect(await p).toBe(true);
    expect(q.saves).toBe(0);
  });

  it("Save closes the question first, then leaves only when the save went through", async () => {
    const q = question();
    const p = askToLeave({ current: null }, true, q.ask, q.answered, q.save);
    q.answer("save");
    expect(await p).toBe(true);
    expect(q.log).toEqual(["ask", "answered", "save"]);

    const failing = question();
    failing.failSave();
    const f = askToLeave({ current: null }, true, failing.ask, failing.answered, failing.save);
    failing.answer("save");
    expect(await f).toBe(false);
  });

  it("a second request while the question is open waits for the same answer", async () => {
    const q = question();
    const pending = { current: null as Promise<boolean> | null };
    const first = askToLeave(pending, true, q.ask, q.answered, q.save);
    const second = askToLeave(pending, true, q.ask, q.answered, q.save);
    expect(second).toBe(first);
    expect(q.asked).toBe(1);
    q.answer("discard");
    expect(await second).toBe(true);
    // Once answered, the next request asks again.
    await Promise.resolve();
    expect(pending.current).toBeNull();
    const third = askToLeave(pending, true, q.ask, q.answered, q.save);
    expect(q.asked).toBe(2);
    q.answer("stay");
    expect(await third).toBe(false);
  });
});

type Def = { name: string; text: string };
type Form = { name: string; text: string };
const rules = {
  keyOf: (d: Def) => d.name,
  toForm: (d: Def): Form => ({ ...d }),
  isDirty: (saved: Form | null, f: Form) => JSON.stringify(saved ?? { name: "", text: "" }) !== JSON.stringify(f),
};
const a: Def = { name: "a", text: "one" };

describe("followFile", () => {
  it("does nothing while closed or adding a new entry", () => {
    expect(followFile({ open: false, original: "a", dirty: false, saved: a }, [a], rules)).toEqual({ kind: "none" });
    expect(followFile({ open: true, original: null, dirty: true, saved: null }, [a], rules)).toEqual({ kind: "none" });
  });

  it("does nothing when the file still holds what was saved, even with an edit", () => {
    expect(followFile({ open: true, original: "a", dirty: false, saved: a }, [a], rules)).toEqual({ kind: "none" });
    expect(followFile({ open: true, original: "a", dirty: true, saved: a }, [a], rules)).toEqual({ kind: "none" });
  });

  it("keeps an unsaved edit and shows the note, changed or gone", () => {
    const changed = { name: "a", text: "two" };
    expect(followFile({ open: true, original: "a", dirty: true, saved: a }, [changed], rules)).toEqual({ kind: "note" });
    expect(followFile({ open: true, original: "a", dirty: true, saved: a }, [], rules)).toEqual({ kind: "note" });
  });

  it("reloads a clean form from the file", () => {
    const changed = { name: "a", text: "two" };
    expect(followFile({ open: true, original: "a", dirty: false, saved: a }, [changed], rules)).toEqual({
      kind: "reload",
      form: changed,
    });
  });

  it("closes a clean form whose entry left the file", () => {
    expect(followFile({ open: true, original: "a", dirty: false, saved: a }, [{ name: "b", text: "" }], rules)).toEqual({
      kind: "close",
    });
  });
});

describe("errorMessage", () => {
  it("reads a string, an Error, or falls back", () => {
    expect(errorMessage("Name is taken")).toBe("Name is taken");
    expect(errorMessage(new Error("boom"))).toBe("boom");
    expect(errorMessage({})).toBe("Could not save.");
  });
});
