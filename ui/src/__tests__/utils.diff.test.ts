// lineDiff / collapseDiff: lightweight LCS diff used by tool cards ([docs/oss-prep-batch](../../../docs/oss-prep-batch.md) coverage batch)
import { describe, it, expect } from "vitest";
import { lineDiff, collapseDiff, type DiffLine } from "../utils/diff";

function kinds(lines: DiffLine[]): string {
  return lines.map((l) => l.type).join(",");
}

describe("utils/diff lineDiff", () => {
  it("marks everything as same for identical text", () => {
    const out = lineDiff("a\nb\nc", "a\nb\nc");
    expect(out.every((l) => l.type === "same")).toBe(true);
    expect(out.map((l) => l.text)).toEqual(["a", "b", "c"]);
  });

  it("detects pure addition at the end", () => {
    expect(lineDiff("a\nb", "a\nb\nc")).toEqual([
      { type: "same", text: "a" },
      { type: "same", text: "b" },
      { type: "add", text: "c" },
    ]);
  });

  it("detects pure deletion at the end", () => {
    expect(lineDiff("a\nb\nc", "a\nb")).toEqual([
      { type: "same", text: "a" },
      { type: "same", text: "b" },
      { type: "del", text: "c" },
    ]);
  });

  it("detects a middle change as del+add pair", () => {
    const out = lineDiff("a\nold\nc", "a\nnew\nc");
    expect(kinds(out)).toBe("same,del,add,same");
    expect(out[1]).toMatchObject({ type: "del", text: "old" });
    expect(out[2]).toMatchObject({ type: "add", text: "new" });
  });

  it("treats empty string as a single empty line", () => {
    expect(lineDiff("", "a")).toEqual([
      { type: "del", text: "" },
      { type: "add", text: "a" },
    ]);
    expect(lineDiff("a", "")).toEqual([
      { type: "del", text: "a" },
      { type: "add", text: "" },
    ]);
    expect(lineDiff("", "")).toEqual([{ type: "same", text: "" }]);
  });

  it("falls back to prefix + bulk del/add when input exceeds the LCS size limit", () => {
    // 20x20 lines is under the limit; 25x25 (625 lines each side > 400*400 op cap? no —
    // n*m counts lines: use 401 lines each side so n*m > 400*400)
    const oldText = Array.from({ length: 401 }, (_, i) => `old-${i}`).join("\n");
    const newText = Array.from({ length: 401 }, (_, i) => `new-${i}`).join("\n");
    const out = lineDiff(oldText, newText);
    // Fallback keeps nothing as same (no common prefix) → bulk del + add
    expect(out.filter((l) => l.type === "del")).toHaveLength(401);
    expect(out.filter((l) => l.type === "add")).toHaveLength(401);
    expect(out.every((l) => l.type !== "same")).toBe(true);
  });

  it("fallback keeps the common prefix when inputs share one", () => {
    const common = Array.from({ length: 401 }, (_, i) => `same-${i}`);
    const oldText = [...common, "x"].join("\n");
    const newText = [...common, "y"].join("\n");
    const out = lineDiff(oldText, newText);
    expect(out.filter((l) => l.type === "same")).toHaveLength(401);
    expect(out.filter((l) => l.type === "del")).toHaveLength(1);
    expect(out.filter((l) => l.type === "add")).toHaveLength(1);
  });
});

describe("utils/diff collapseDiff", () => {
  function seq(): DiffLine[] {
    return [
      { type: "same", text: "c1" },
      { type: "same", text: "c2" },
      { type: "same", text: "c3" },
      { type: "del", text: "old" },
      { type: "add", text: "new" },
      { type: "same", text: "c4" },
      { type: "same", text: "c5" },
      { type: "same", text: "c6" },
    ];
  }

  it("keeps context lines around changes and drops distant same lines", () => {
    const out = collapseDiff(seq(), 2);
    // context=2：变更行在 idx 3/4 → 保留 idx 1..6；c1(idx0) 与 c6(idx7) 距变更 3 行被折叠
    expect(out.some((l) => l.text === "c1")).toBe(false);
    expect(out.some((l) => l.text === "c6")).toBe(false);
    expect(out.some((l) => l.text === "c2")).toBe(true);
    expect(out.some((l) => l.text === "c5")).toBe(true);
    expect(out.some((l) => l.text === "old")).toBe(true);
  });

  it("inserts a single ellipsis marker for collapsed regions", () => {
    const out = collapseDiff(seq(), 1);
    expect(out.filter((l) => l.text === "…")).toHaveLength(1);
  });

  it("returns everything when context covers the whole input", () => {
    const out = collapseDiff(seq(), 10);
    expect(out.some((l) => l.text === "…")).toBe(false);
    expect(out).toHaveLength(seq().length);
  });
});
