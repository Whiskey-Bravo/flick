import { describe, it, expect } from "vitest";
import { builtinName, fallbackName, serverUrl } from "./icons";

describe("icons", () => {
  it("builds the server url from the encoded key and scaled size", () => {
    expect(serverUrl("ext:.pdf", 1.5)).toBe("http://flick-icon.localhost/ext%3A.pdf?px=48");
    expect(serverUrl("file:C:\\a b\\p.exe", 1)).toBe("http://flick-icon.localhost/file%3AC%3A%5Ca%20b%5Cp.exe?px=32");
  });
  it("only builtin keys are served locally", () => {
    expect(builtinName("builtin:wsl")).toBe("wsl");
    expect(builtinName("folder")).toBeNull();
    expect(builtinName("file:C:\\q")).toBeNull();
  });
  it("falls back by kind", () => {
    expect(fallbackName({ icon: "folder", provider: "files" })).toBe("folder");
    expect(fallbackName({ icon: "ext:.txt", provider: "files" })).toBe("file");
    expect(fallbackName({ icon: "file:C:\\a.exe", provider: "apps" })).toBe("app");
    expect(fallbackName({ icon: "file:C:\\a.txt", provider: "files" })).toBe("file");
  });
});
