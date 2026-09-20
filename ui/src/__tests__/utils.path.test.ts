// baseName: path tail extraction with Windows/POSIX separator normalization ([docs/oss-prep-batch](../../../docs/oss-prep-batch.md) coverage batch)
// shortestUniqueLabels: same-name files in different dirs get widened segments (tool card head summary)
import { describe, it, expect } from "vitest";
import { baseName, shortestUniqueLabels } from "../utils/path";

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

describe("utils/path shortestUniqueLabels", () => {
  it("无冲突时与旧 basename 输出逐字节一致（含尾随分隔符得空串）", () => {
    const paths = ["src-tauri/src/infrastructure/git/credentials.rs", "src-tauri\\src\\git\\remote.rs", "Cargo.toml", "a/tsconfig.json"];
    expect(shortestUniqueLabels(paths)).toEqual(paths.map((p) => String(p ?? "").split(/[\\/]/).pop() ?? ""));
    expect(shortestUniqueLabels(["src/lib/", "only"])).toEqual(["", "only"]);
  });

  it("两个不同目录同名文件：向前扩段到可区分", () => {
    expect(shortestUniqueLabels(["a/ui.ts", "b/ui.ts"])).toEqual(["a/ui.ts", "b/ui.ts"]);
    // 一层不够就继续向前（b/src/ui.ts vs b/test/ui.ts）
    expect(shortestUniqueLabels(["b/src/ui.ts", "b/test/ui.ts"])).toEqual(["src/ui.ts", "test/ui.ts"]);
    // 非冲突项不受影响
    expect(shortestUniqueLabels(["a/ui.ts", "b/ui.ts", "c/main.rs"])).toEqual(["a/ui.ts", "b/ui.ts", "main.rs"]);
  });

  it("段用尽仍相同则保持原标签；下标一一对应", () => {
    expect(shortestUniqueLabels(["ui.ts", "deep/path/ui.ts"])).toEqual(["ui.ts", "path/ui.ts"]);
    expect(shortestUniqueLabels(["same.ts", "same.ts"])).toEqual(["same.ts", "same.ts"]);
    expect(shortestUniqueLabels([])).toEqual([]);
  });

  it("空串冲突组（尾随分隔符路径）不扩段：两项都保持空串", () => {
    // "a/" / "b/" 的末段都是空串：扩段会把空标签写成非空（"a/" / "b/"），与「空串保持空串」约定相违
    expect(shortestUniqueLabels(["a/", "b/"])).toEqual(["", ""]);
    // 混合组：空串组与同名文件组互不影响（后者段用尽仍保持原标签）
    expect(shortestUniqueLabels(["a/", "b/", "ui.ts", "ui.ts"])).toEqual(["", "", "ui.ts", "ui.ts"]);
    expect(shortestUniqueLabels(["ui.ts", "ui.ts"])).toEqual(["ui.ts", "ui.ts"]);
  });

  it("反斜杠路径的冲突组：扩段后统一以 / 连接", () => {
    expect(shortestUniqueLabels(["a\\ui.ts", "b\\ui.ts"])).toEqual(["a/ui.ts", "b/ui.ts"]);
    // 一层不够就继续向前（混合分隔符一并归一）
    expect(shortestUniqueLabels(["src\\a\\ui.ts", "test\\b\\ui.ts"])).toEqual(["a/ui.ts", "b/ui.ts"]);
  });

  it("部分可区分组：仅冲突项扩段，非冲突项保持末段", () => {
    expect(shortestUniqueLabels(["a/ui.ts", "b/ui.ts", "z.css"])).toEqual(["a/ui.ts", "b/ui.ts", "z.css"]);
    // 三项同末段、到第三层才两两可区分 → 全体扩段
    expect(shortestUniqueLabels(["x/a/ui.ts", "y/a/ui.ts", "z/b/ui.ts"]))
      .toEqual(["x/a/ui.ts", "y/a/ui.ts", "z/b/ui.ts"]);
  });
});
