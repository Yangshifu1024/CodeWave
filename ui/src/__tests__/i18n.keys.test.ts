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
      "rightbar.quotaEmpty",
      "rightbar.quotaGoSettings",
      "rightbar.quotaUnsupported",
      "rightbar.quotaReasonNoAdapter",
      "rightbar.quotaReasonEmptyUrl",
      "rightbar.quotaNoKey",
      "rightbar.quotaRejected",
      "rightbar.quotaCollapsed",
      "rightbar.quotaLastOk",
      "rightbar.quotaNeverOk",
      "rightbar.quotaAgoJustNow",
      "rightbar.quotaAgoMinutes",
      "rightbar.quotaAgoHours",
      "rightbar.quotaAgoDays",
      "rightbar.quotaStatusRateLimited",
      "rightbar.quotaKeySourceKeyring",
      "rightbar.quotaKeySourceConfig",
      "rightbar.window.rolling",
      "rightbar.window.weekly",
      "rightbar.window.monthly",
      "app.resizeLeft",
      "app.resizeRight",
    ]) {
      expect(zhKeys.has(key), `zh 缺键：${key}`).toBe(true);
      expect(enKeys.has(key), `en 缺键：${key}`).toBe(true);
    }
    // 已移除的键不得复活（数据目录行 / 会话段 / 旧凭证链语义）
    for (const gone of [
      "rightbar.dataDir",
      "rightbar.startedAt",
      "rightbar.session",
      "rightbar.quotaSource",
      "rightbar.quotaNoCredential",
    ]) {
      expect(zhKeys.has(gone), `zh 残留旧键：${gone}`).toBe(false);
      expect(enKeys.has(gone), `en 残留旧键：${gone}`).toBe(false);
    }
  });

  it("批④ 删除的键在 zh-CN 与 en-US 双侧都不得复活", () => {
    const zhKeys = new Set(keyPaths(zh));
    const enKeys = new Set(keyPaths(en));
    // 批④（[docs/settings-terminology](../../../docs/settings-terminology.md)）：同义键收敛到 common.*、
    // 借用键拆成 settings.*、about 段整体迁入 settings.about* —— 这些旧键不得再出现
    for (const gone of [
      "settings.remove",
      "settings.save",
      "settings.saved",
      "settings.cancel",
      "nav.delete",
      "nav.save",
      "nav.saved",
      "nav.cancel",
      "sessions.delete",
      "sessions.empty",
      "tasks.delete",
      "queue.delete",
      "rightbar.skillBuiltin",
      "lsp.confirmCost",
      "about.slogan",
      "about.appData",
      "about.repo",
    ]) {
      expect(zhKeys.has(gone), `zh 残留已删键：${gone}`).toBe(false);
      expect(enKeys.has(gone), `en 残留已删键：${gone}`).toBe(false);
    }
    // `about` 段整体退役：不得再有任何 about.* 键（全部迁入 settings.about*）
    expect([...zhKeys].filter((k) => k.startsWith("about."))).toEqual([]);
    expect([...enKeys].filter((k) => k.startsWith("about."))).toEqual([]);
  });

  it("批④ 新增键全量对偶（段级 / 前缀级闭包，不再是抽样名单）", () => {
    const zhKeys = new Set(keyPaths(zh));
    const enKeys = new Set(keyPaths(en));
    // ① 段级：`common` 是批④ 新增的段 → 整段对偶，且键集恰为这 5 把。
    //    少一把（收敛漏改）或多一把（半途改名）都要有人过一眼，故期望集合写死。
    const COMMON_KEYS = ["builtin", "cancel", "delete", "save", "saved"];
    expect(Object.keys(zh.common).sort()).toEqual(COMMON_KEYS);
    expect(Object.keys(en.common).sort()).toEqual(COMMON_KEYS);
    // ② 前缀级：`settings.about*` 是本批迁入 / 新增的一族 → 两侧集合逐把相等且数量写死
    //    （原来只抽样 6 把：aboutVersionHint / aboutOpen* 等漏改另一语言不会红）。
    const aboutKeys = (keys: Set<string>) => [...keys].filter((k) => k.startsWith("settings.about")).sort();
    expect(aboutKeys(zhKeys)).toEqual(aboutKeys(enKeys));
    expect(aboutKeys(zhKeys)).toHaveLength(15);
    // ③ 两个拆出的键由注册表 SHELL_SETTING_KEYS 收口：settings.registry.test.ts 已断言该清单逐把双侧存在，
    //    这里只点名，不再手工维护「15 把 about* + 2 把拆出键」的名单。
    for (const key of ["settings.skillsEmpty", "settings.aiLanguagePlaceholder"]) {
      expect(zhKeys.has(key), `zh 缺新键：${key}`).toBe(true);
      expect(enKeys.has(key), `en 缺新键：${key}`).toBe(true);
    }
  });

  it("速率段与统计总览的新键双侧对偶（[docs/composer-token-rate]）", () => {
    const zhKeys = new Set(keyPaths(zh));
    const enKeys = new Set(keyPaths(en));
    for (const key of [
      "composer.rateRunning",
      "composer.rateTitle",
      "composer.rateTtft",
      "composer.rateOutput",
      "composer.rateGenMs",
      "composer.rateToolWait",
      "stats.avgRate",
      "stats.avgStepMs",
      "stats.avgTtft",
    ]) {
      expect(zhKeys.has(key), `zh 缺键：${key}`).toBe(true);
      expect(enKeys.has(key), `en 缺键：${key}`).toBe(true);
    }
  });

  it("会话保留期与清理的新键双侧对偶且数量写死（[docs/session-cleanup]）", () => {
    const zhKeys = new Set(keyPaths(zh));
    const enKeys = new Set(keyPaths(en));
    // `settings.cleanup*` 是本批新增的一族（选项 / 禁用原因 / 确认框 / 完成提示 / 上次清理回显）：
    // 两侧集合逐把相等且数量写死——半途改名或只补一侧都会被这条拦住。
    const cleanup = (keys: Set<string>) => [...keys].filter((k) => k.startsWith("settings.cleanup")).sort();
    expect(cleanup(zhKeys)).toEqual(cleanup(enKeys));
    expect(cleanup(zhKeys)).toHaveLength(23);
    // 保留期本身的项名与说明另算（不合 cleanup* 前缀）
    for (const key of ["settings.sessionRetention", "settings.sessionRetentionHint"]) {
      expect(zhKeys.has(key), `zh 缺新键：${key}`).toBe(true);
      expect(enKeys.has(key), `en 缺新键：${key}`).toBe(true);
    }
    // 本批不得借 `settings.about*` 族（那一族的数量被上一用例写死为 15）
    expect(cleanup(enKeys).some((k) => k.includes("about"))).toBe(false);
  });
});
