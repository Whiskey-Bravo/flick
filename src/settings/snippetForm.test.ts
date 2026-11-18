import { describe, expect, it } from "vitest";
import { emptySnippetForm, isSnippetDirty, preview, toSnippetDef, toSnippetForm } from "./snippetForm";

describe("snippet form", () => {
  it("trims the name and keeps the text as typed", () => {
    expect(toSnippetDef({ name: "  sig ", text: " Best regards,\nSam\n" })).toEqual({
      name: "sig",
      text: " Best regards,\nSam\n",
    });
    expect(toSnippetForm({ name: "sig", text: "a\nb" })).toEqual({ name: "sig", text: "a\nb" });
  });

  it("is dirty only when the name or the text differs", () => {
    expect(isSnippetDirty(null, emptySnippetForm())).toBe(false);
    expect(isSnippetDirty(null, { name: "a", text: "" })).toBe(true);
    const saved = { name: "sig", text: "Bye" };
    expect(isSnippetDirty(saved, { ...saved })).toBe(false);
    expect(isSnippetDirty(saved, { name: "sig", text: "Bye " })).toBe(true);
    expect(isSnippetDirty(saved, { name: "sig2", text: "Bye" })).toBe(true);
  });

  it("previews the text on one line", () => {
    expect(preview("  Best regards,\n\nSam ")).toBe("Best regards, Sam");
    expect(preview("x".repeat(50), 10)).toBe("x".repeat(10) + "…");
    expect(preview("")).toBe("");
  });
});
