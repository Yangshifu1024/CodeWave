// i18n 双语键集合守护：新增/删除键必须双侧同步
//（[docs/rightbar-info-refactor-and-subscription-quota](../../../docs/rightbar-info-refactor-and-subscription-quota.md)）。
// 此前仓库无此守护，漏改另一语言不会红；本用例把「中英对称」变成硬约束。
import { describe, it, expect } from "vitest";
import zh from "../i18n/zh-CN";
import en from "../i18n/en-US";

/** 展开嵌套字典为点号路径键集合（右栏这类键在实现里就是按点号调用的） */
function keyPaths(node: unknown, prefix = ""): string[] {
  if (node === null || typeof node !== "object" || Array.isArray(node)) return [prefix];
  const out: string[] = [];
  for (const [key, value] of Object.entries(node as Record<string, unknown>)) {
    out.push(...keyPaths(value, prefix ? `${prefix}.${key}` : key));
  }
  return out;
}

describe("i18n 键集合契约", () => {
  it("zh-CN 与 en-US 的键集合完全一致", () => {
    const zhKeys = keyPaths(zh).sort();
    const enKeys = keyPaths(en).sort();
    // 失败时打印差异，便于直接补键
    const missingInEn = zhKeys.filter((k) => !enKeys.includes(k));
    const missingInZh = enKeys.filter((k) => !zhKeys.includes(k));
    expect({ missingInEn, missingInZh }).toEqual({ missingInEn: [], missingInZh: [] });
  });

  it("额度段与编辑器下拉的新键在两侧都存在", () => {
    const zhKeys = new Set(keyPaths(zh));
    const enKeys = new Set(keyPaths(en));
    for (const key of [
      "rightbar.openInEditor",
      "rightbar.openInEditorFailed",
      "rightbar.quota",
      "rightbar.quotaRefresh",
      "rightbar.quotaNoCredential",
      "rightbar.window.rolling",
      "rightbar.window.weekly",
      "rightbar.window.monthly",
      "app.resizeLeft",
      "app.resizeRight",
    ]) {
      expect(zhKeys.has(key), `zh 缺键：${key}`).toBe(true);
      expect(enKeys.has(key), `en 缺键：${key}`).toBe(true);
    }
    // 已移除的键不得复活（数据目录行 / 会话段）
    for (const gone of ["rightbar.dataDir", "rightbar.startedAt", "rightbar.session"]) {
      expect(zhKeys.has(gone), `zh 残留旧键：${gone}`).toBe(false);
      expect(enKeys.has(gone), `en 残留旧键：${gone}`).toBe(false);
    }
  });
});
