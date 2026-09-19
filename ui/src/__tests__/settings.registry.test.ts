// 设置项注册表契约（8 页重划 / 注册表 [docs/settings-ia](../../../docs/settings-ia.md)）：
// 手法同 titlebar.style.test.ts —— node fs 直读源码做「引用闭包」断言，其余断言直接校验注册表数据。
//   1. 引用闭包：`ui/src/features/panels/*.tsx`（扫描目录，非硬编码文件表）里的 t("settings.X") /
//      t('settings.X') / t(`settings.X`) 必须在注册表（项 / 分组 / 页名）内或显式豁免；
//   2. 键存在性：注册表里每个键在 zh-CN 与 en-US 都存在（与 i18n.keys.test.ts 双侧同步守护叠加）；
//   3. 页合法性：每项的 page ∈ PAGE_ORDER，id 全表唯一（同 id 登记两页即冲突）；
//   4. 分组完备：PAGE_GROUPS 不重不漏覆盖 PAGE_ORDER；
//   5. PAGE_FIELDS 覆盖全部 PageKey（防「漏一页 → 该页脏点永不亮」）；
//   6. 字段归属唯一：同一字段路径不得出现在两个页的 PAGE_FIELDS 里；
//   7. 项 ↔ 页字段双向闭合：每条设置项的 id 必须在其所属页的字段清单里（`app.*` 动作项除外），
//      PAGE_FIELDS 里每条路径也必须有对应项（例外见 PAGE_FIELD_EXCEPTIONS）——
//      这条才是「删掉一条字段登记仍全绿」那个盲区的封口。
import { describe, it, expect } from "vitest";
import { readFileSync, readdirSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import zh from "../i18n/zh-CN";
import en from "../i18n/en-US";
import { LSP_LANGUAGES } from "../ipc/types";
import {
  DEFAULT_PAGE,
  INSTANT_APPLY_FIELD_IDS,
  MCP_FIELD_ID,
  PAGE_FIELD_EXCEPTIONS,
  PAGE_FIELDS,
  PAGE_GROUPS,
  PAGE_LABEL_KEY,
  PAGE_ORDER,
  SETTINGS_ITEMS,
  SHELL_SETTING_KEYS,
  normalizePageKey,
} from "../features/panels/settingsRegistry";

const SRC = join(dirname(fileURLToPath(import.meta.url)), "..");

/** 展开嵌套字典为点号路径键集合 */
function keyPaths(node: unknown, prefix = ""): string[] {
  if (node === null || typeof node !== "object" || Array.isArray(node)) return [prefix];
  const out: string[] = [];
  for (const [key, value] of Object.entries(node as Record<string, unknown>)) {
    out.push(...keyPaths(value, prefix ? `${prefix}.${key}` : key));
  }
  return out;
}

const zhKeys = new Set(keyPaths(zh));
const enKeys = new Set(keyPaths(en));

/** 设置页目录（闭包扫描范围：页体全在这里，新增页体不必改测试） */
const PANELS_DIR = join(SRC, "features/panels");

/**
 * 组件源码里出现的 t("settings.X") / t('settings.X') / t(`settings.X`) 字面键。
 * 三种引号都认：只用双引号的正则会漏掉单引号 / 反引号写法（改了也全绿 = 假绿）。
 * 动态键（如 t(LANG_LABEL_KEY[lang])）不在此列，由注册表的项覆盖。
 */
function literalSettingKeys(file: string): string[] {
  const src = readFileSync(join(PANELS_DIR, file), "utf8");
  const out = new Set<string>();
  for (const m of src.matchAll(/t\(\s*(["'`])settings\.([A-Za-z0-9_]+)\1/g)) out.add(m[2]);
  return [...out];
}

/**
 * 闭包范围 = 设置页目录下**全部**页体组件（扫描而非硬编码文件表：
 * 「关于」页 AboutSettings.tsx 与未来新增页体自动纳入，不再出现「第 8 页不在闭包内」的盲区）。
 */
const CLOSURE_FILES = readdirSync(PANELS_DIR)
  .filter((f) => f.endsWith(".tsx"))
  .sort();

/** 确定含字面 settings.* 键的页体（正则失效守卫用；目录里其余组件本就没有这类键） */
const KNOWN_KEY_BEARING_FILES = ["SettingsPage.tsx", "ProvidersPanel.tsx", "FontSettings.tsx", "AboutSettings.tsx"];

describe("设置项注册表：引用闭包", () => {
  it("组件里的 t(\"settings.X\") 必须落在注册表（项 / 分组 / 页名）或显式豁免清单内", () => {
    const known = new Set<string>([
      ...SETTINGS_ITEMS.map((i) => i.labelKey),
      ...SETTINGS_ITEMS.map((i) => i.group).filter((g): g is string => !!g),
      ...Object.values(PAGE_LABEL_KEY),
      ...PAGE_GROUPS.map((g) => g.titleKey),
      ...SHELL_SETTING_KEYS,
    ].map((k) => k.replace(/^settings\./, "")));

    // 正则失效守卫：确定含字面键的页体必须各取到 ≥1 个键（AboutSettings 是「第 8 页」的回归点）
    for (const file of KNOWN_KEY_BEARING_FILES) {
      expect(CLOSURE_FILES, `${file} 未被闭包扫描覆盖`).toContain(file);
      expect(literalSettingKeys(file).length, `${file} 未取到任何 settings.* 键（正则失效？）`).toBeGreaterThan(0);
    }

    for (const file of CLOSURE_FILES) {
      const keys = literalSettingKeys(file);
      const unknown = keys.filter((k) => !known.has(k));
      expect(unknown, `${file} 里有未登记 / 未豁免的设置键：${unknown.join("、")}`).toEqual([]);
    }
  });

  it("豁免清单与注册表不重叠（一项设置不得既登记又豁免）", () => {
    const registered = new Set([
      ...SETTINGS_ITEMS.map((i) => i.labelKey.replace(/^settings\./, "")),
      ...SETTINGS_ITEMS.map((i) => i.group).filter((g): g is string => !!g).map((g) => g.replace(/^settings\./, "")),
      ...Object.values(PAGE_LABEL_KEY).map((k) => k.replace(/^settings\./, "")),
    ]);
    const overlap = SHELL_SETTING_KEYS.filter((k) => registered.has(k));
    expect(overlap).toEqual([]);
  });
});

describe("设置项注册表：键与页合法", () => {
  it("注册表里每个键在 zh-CN 与 en-US 都存在", () => {
    const keys = [
      ...SETTINGS_ITEMS.map((i) => i.labelKey),
      ...SETTINGS_ITEMS.map((i) => i.group).filter((g): g is string => !!g),
      ...Object.values(PAGE_LABEL_KEY),
      ...PAGE_GROUPS.map((g) => g.titleKey),
      ...SHELL_SETTING_KEYS.map((k) => `settings.${k}`),
    ];
    const missingZh = keys.filter((k) => !zhKeys.has(k));
    const missingEn = keys.filter((k) => !enKeys.has(k));
    expect({ missingZh, missingEn }).toEqual({ missingZh: [], missingEn: [] });
  });

  it("每项的 page ∈ PAGE_ORDER，id 全表唯一（防一项登记两页 / 复制粘贴重 id）", () => {
    for (const item of SETTINGS_ITEMS) {
      expect(PAGE_ORDER, `${item.id} 的 page 非法：${item.page}`).toContain(item.page);
    }
    const ids = SETTINGS_ITEMS.map((i) => i.id);
    const dup = ids.filter((id, i) => ids.indexOf(id) !== i);
    expect(dup, `重复的设置项 id：${dup.join("、")}`).toEqual([]);
    // keywords 是直字符串（不进 i18n）：不得写成 settings.* 键
    for (const item of SETTINGS_ITEMS) {
      for (const kw of item.keywords ?? []) {
        expect(kw.startsWith("settings."), `${item.id} 的 keyword 误写成 i18n 键：${kw}`).toBe(false);
      }
    }
  });

  it("分组不重不漏覆盖 PAGE_ORDER，且默认页合法、旧页 key 全部归一", () => {
    const grouped = PAGE_GROUPS.flatMap((g) => g.pages);
    expect([...grouped].sort()).toEqual([...PAGE_ORDER].sort());
    expect(new Set(grouped).size).toBe(grouped.length); // 无重复
    for (const g of PAGE_GROUPS) expect(g.pages.length).toBeGreaterThan(0);
    expect(PAGE_ORDER).toContain(DEFAULT_PAGE);
    // 旧页 key（含未登记值）都必须归一到合法页，绝不落到非法 key
    for (const raw of ["general", "appearance", "providers", "security", "network", "mcp", "skills", "nope", ""]) {
      expect(PAGE_ORDER).toContain(normalizePageKey(raw));
    }
    expect(normalizePageKey("mcp")).toBe("tools");
    expect(normalizePageKey(undefined)).toBe(DEFAULT_PAGE);
  });
});

describe("设置项注册表：页字段归属（脏标记数据源）", () => {
  it("PAGE_FIELDS 覆盖全部 PageKey 且每页至少一个字段（防「漏一页 → 该页脏点永不亮」）", () => {
    expect(Object.keys(PAGE_FIELDS).sort()).toEqual([...PAGE_ORDER].sort());
    for (const page of PAGE_ORDER) {
      expect(PAGE_FIELDS[page].length, `${page} 页没有登记任何字段`).toBeGreaterThan(0);
    }
  });

  it("同一字段路径不得归属两页（字段归属唯一）", () => {
    const seen = new Map<string, string>();
    const conflicts: string[] = [];
    for (const page of PAGE_ORDER) {
      for (const path of PAGE_FIELDS[page]) {
        const owner = seen.get(path);
        if (owner) conflicts.push(`${path}（${owner} 与 ${page}）`);
        else seen.set(path, page);
      }
    }
    expect(conflicts, `字段归属冲突：${conflicts.join("、")}`).toEqual([]);
  });

  it("即时生效项与 MCP 必须在 PAGE_FIELDS 内登记（防路径写错导致打点语义漂移）", () => {
    const all = new Set(PAGE_ORDER.flatMap((p) => PAGE_FIELDS[p]));
    // 即时生效项：登记但比较时跳过（它们改完立即生效，纳入就会永远显示「未保存」）
    for (const id of INSTANT_APPLY_FIELD_IDS) {
      expect(all.has(id as never), `即时生效项未登记：${id}`).toBe(true);
    }
    // MCP 不在 config 内：脏判定走独立 mcp.json 的文本基线（挂在其拥有页上）
    expect(PAGE_FIELDS.tools).toContain(MCP_FIELD_ID as never);
  });

  /**
   * 不产生页字段的设置项（显式规则，不算「漏登记」）：
   * `app.*` = 动作 / 只读身份信息（如「检查更新」按钮、版本号），没有可保存的 config 字段。
   * 其余项（含 `ui.*` 即时生效偏好）**都**要落在 PAGE_FIELDS 里：即时生效项虽不参与比较，
   * 但它们同样挂在页上、由 PAGE_FIELDS 决定「这一页有没有可改动的东西」。
   */
  const NON_FIELD_ITEM_PREFIXES = ["app."];

  it("每条设置项的 id 必须登记进其所属页的字段清单（app.* 动作项除外）——防漏登记致该页脏点永不亮", () => {
    const missing = SETTINGS_ITEMS.filter(
      (item) =>
        !NON_FIELD_ITEM_PREFIXES.some((p) => item.id.startsWith(p)) &&
        !(PAGE_FIELDS[item.page] as string[]).includes(item.id),
    ).map((item) => `${item.id}（page=${item.page}）`);
    expect(missing, `设置项未登记进 PAGE_FIELDS[page]：${missing.join("、")}`).toEqual([]);
  });

  it("PAGE_FIELDS 里每条路径都有对应设置项且归属页一致（例外仅 PAGE_FIELD_EXCEPTIONS）", () => {
    const byId = new Map(SETTINGS_ITEMS.map((i) => [i.id, i]));
    const orphans: string[] = [];
    const misplaced: string[] = [];
    for (const page of PAGE_ORDER) {
      for (const path of PAGE_FIELDS[page] as string[]) {
        const item = byId.get(path);
        if (!item) {
          if (!PAGE_FIELD_EXCEPTIONS.includes(path)) orphans.push(`${path}（page=${page}）`);
          continue;
        }
        if (item.page !== page) misplaced.push(`${path}：项登记在 ${item.page}，字段登记在 ${page}`);
      }
    }
    expect({ orphans, misplaced }).toEqual({ orphans: [], misplaced: [] });
    // 豁免清单不得与设置项重叠（同一条路径既登记项又标豁免 = 清单在掩盖真问题）
    const overlapping = PAGE_FIELD_EXCEPTIONS.filter((p) => byId.has(p));
    expect(overlapping, `豁免路径已有设置项，应从 PAGE_FIELD_EXCEPTIONS 移除：${overlapping.join("、")}`).toEqual([]);
  });

  it("六语言校验项齐备且各带独立 labelKey（SettingsPage 的语言行标签从本表派生）", () => {
    const items = LSP_LANGUAGES.map((lang) => SETTINGS_ITEMS.find((i) => i.id === `validation.${lang}`));
    const missing = LSP_LANGUAGES.filter((_, i) => !items[i]);
    expect(missing, `注册表缺少 language 项：${missing.join("、")}`).toEqual([]);
    for (const [i, lang] of LSP_LANGUAGES.entries()) {
      expect(items[i]!.page, `validation.${lang} 不在工具与集成页`).toBe("tools");
    }
    // 六个 labelKey 互不相同：否则语言行的展示名会串（改错一个也不会有界面上的表现差异之外的报错）
    const labelKeys = items.map((i) => i!.labelKey);
    expect(new Set(labelKeys).size, `语言行 labelKey 有重复：${labelKeys.join("、")}`).toBe(labelKeys.length);
  });
});
