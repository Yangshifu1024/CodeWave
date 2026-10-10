// 项目条目契约测试：ProjectEntry 的字段集合前后端必须双向一致。
// 根治「后端加了字段、前端忘声明 → 组装 entry 时漏抄 → 静默丢数据」这一类缺陷
// （allowed_dirs 长期丢失即此因：后端 struct 里有，前端类型里没有，保存时自然带不上）。
// 手法同 [events.contract.test.ts]：正则抽字段名，两端对拍 + 字段数硬锚点。
import { describe, expect, it } from "vitest";
import { readFileSync } from "node:fs";
import { join, dirname } from "node:path";
import { fileURLToPath } from "node:url";

const uiSrc = join(dirname(fileURLToPath(import.meta.url)), "..");

const srcTauri = join(dirname(fileURLToPath(import.meta.url)), "../../../src-tauri/src");

const rustPath = join(srcTauri, "core/projects.rs");

const tsPath = join(uiSrc, "ipc/types.ts");

/** 抽 Rust struct 本体（从 `pub struct X {` 到行首 `}` 为止，struct 内不会缩进闭合括号，
 *  故行首 `}` 即边界；`#[serde(default)]` / 文档注释一并跳过）。
 *
 *  刻意**不**用 indexOf 取首个匹配：本文件里紧挨着还有一个入参 DTO `ProjectSaveInput`，
 *  其字段名是 `ProjectEntry` 的真子集。若将来出现 `ProjectSaveInputV2` 之类命名，
 *  首匹配策略会把另一个 struct 的字段当成本 struct 的，契约测试就会假绿。
 *  故这里扫描**全部**同名 struct 起点并逐个校验，任一处不一致即失败。 */
function rustFields(src: string, structName: string): string[] {
  const marker = `pub struct ${structName} {`;
  // 精确匹配整词（`X` 不应命中 `Xxx`）：marker 前后必须是词边界
  const re = new RegExp(`(?:^|\\W)${marker.replace(/[.*+?^${}()|[\]\\]/g, "\\$&")}(?=\\W|$)`, "g");
  const starts: number[] = [];
  for (const m of src.matchAll(re)) {
    const at = m.index! + (m[0].startsWith(" ") || m[0].startsWith("\n") || m[0].startsWith("\t") ? 1 : 0);
    if (src.slice(at, at + marker.length) === marker) starts.push(at);
  }
  expect(starts.length, `projects.rs 里找不到 ${structName}`).toBeGreaterThan(0);
  const all = starts.map((start) => {
    const body = src.slice(start);
    const end = body.indexOf("\n}");
    expect(end, `${structName} 未闭合`).toBeGreaterThan(-1);
    const inner = body.slice(0, end);
    return [...inner.matchAll(/^\s*pub ([a-z_]+):/gm)].map((m) => m[1]);
  });
  // 全部同名 struct 的字段并集：任一处多字段都会被对拍抓出来
  return [...new Set(all.flat())];
}

/** 抽 TS interface 本体（`export interface X {` 到首个行首 `}` 之间的 `  name: type;` 形态）。 */
function tsFields(src: string, ifaceName: string): string[] {
  const start = src.indexOf(`export interface ${ifaceName} {`);
  expect(start, `types.ts 里找不到 ${ifaceName}`).toBeGreaterThan(-1);
  const body = src.slice(start);
  const end = body.indexOf("\n}");
  expect(end, `${ifaceName} 未闭合`).toBeGreaterThan(-1);
  const inner = body.slice(0, end);
  return [...inner.matchAll(/^\s*([A-Za-z_][A-Za-z0-9_]*)\??:/gm)].map((m) => m[1]);
}

describe("项目契约：ProjectEntry 字段集合前后端一致", () => {
  it("两端字段集合相等（双向：无遗漏、无幽灵字段）", () => {
    const rs = rustFields(readFileSync(rustPath, "utf8"), "ProjectEntry");
    const ts = tsFields(readFileSync(tsPath, "utf8"), "ProjectEntry");

    const missingInTs = rs.filter((f) => !ts.includes(f));
    const missingInRust = ts.filter((f) => !rs.includes(f));

    expect(
      missingInTs,
      `后端 ProjectEntry 有、前端 types.ts 无（组装 entry 时会漏抄 → 静默丢数据）: ${missingInTs.join(", ")}`,
    ).toEqual([]);
    expect(
      missingInRust,
      `前端 ProjectEntry 有、后端 Rust 无（多声明的幽灵字段）: ${missingInRust.join(", ")}`,
    ).toEqual([]);
  });

  // 字段数硬锚点：后端加字段必须同步改这里（且上面那条双向对拍也要过）
  it("ProjectEntry 字段数为 6", () => {
    const rs = rustFields(readFileSync(rustPath, "utf8"), "ProjectEntry");
    const ts = tsFields(readFileSync(tsPath, "utf8"), "ProjectEntry");
    // id / name / directory / data_dir / created_at / allowed_dirs
    expect(rs.sort()).toEqual(["allowed_dirs", "created_at", "data_dir", "directory", "id", "name"]);
    expect(ts.sort()).toEqual(["allowed_dirs", "created_at", "data_dir", "directory", "id", "name"]);
    expect(rs.length).toBe(6);
    expect(ts.length).toBe(6);
  });
});