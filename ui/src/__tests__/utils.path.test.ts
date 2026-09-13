// baseName: path tail extraction with Windows/POSIX separator normalization ([docs/oss-prep-batch](../../../docs/oss-prep-batch.md) coverage batch)
import { describe, it, expect } from "vitest";
import { baseName } from "../utils/path";

describe("utils/path baseName", () => {
  it("extracts file name from POSIX path", () => {
    expect(baseName("/home/user/project/src/main.rs")).toBe("main.rs");
  });

  it("extracts file name from Windows path with backslashes", () => {
    expect(baseName("D:\\Code\\CodeWave\\src-tauri\\Cargo.toml")).toBe("Cargo.toml");
  });

  it("handles mixed separators", () => {
    expect(baseName("D:/Code\\CodeWave/ui/src/App.tsx")).toBe("App.tsx");
  });

  it("returns directory name for directory-like path", () => {
    expect(baseName("/home/user/project/")).toBe("project");
  });

  it("returns the input unchanged when there is no separator", () => {
    expect(baseName("Cargo.toml")).toBe("Cargo.toml");
  });

  it("falls back to input when path is only separators", () => {
    // "///" splits into empty segments → pop() is undefined → falls back to the raw input
    expect(baseName("///")).toBe("///");
  });
});
