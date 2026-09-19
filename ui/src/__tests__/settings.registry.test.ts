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
  ADVANCED_ITEM_IDS,
  DEFAULT_PAGE,
  INSTANT_APPLY_FIELD_IDS,
  MCP_FIELD_ID,
  PAGE_FIELD_EXCEPTIONS,
  PAGE_FIELDS,
  PAGE_GROUPS,
  PAGE_LABEL_KEY,
  PAGE_ORDER,
  SETTINGS_ADVANCED_PREF_KEY,
  SETTINGS_ITEMS,
  SHELL_SETTING_KEYS,
  WIDTH_CLASS,
  WIDTH_EXEMPT_ITEM_IDS,
  WIDTH_TIERS,
  advancedCountByPage,
  isAdvancedOnlyGroup,
  matchSettings,
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

// ---------- 批③：宽度档 / 搜索 / 进阶折叠（[docs/settings-search-and-advanced](../../../docs/settings-search-and-advanced.md)） ----------

/** 用 zh-CN 的实值模拟 i18n 的 t（键存在性由上面的用例守护，这里只取样值） */
function zhT(key: string): string {
  const dict = zh.settings as Record<string, unknown>;
  return String(dict[key.replace(/^settings\./, "")] ?? key);
}

/** 用 en-US 的实值模拟 i18n 的 t（英文显示名首字母大写，是 haystack 归一的作用对象） */
function enT(key: string): string {
  const dict = en.settings as Record<string, unknown>;
  return String(dict[key.replace(/^settings\./, "")] ?? key);
}

/** 读页体源码（宽度档禁用像素内联 width 的正则断言用） */
function panelSrc(file: string): string {
  return readFileSync(join(PANELS_DIR, file), "utf8");
}

describe("设置项注册表：宽度档与豁免（批③）", () => {
  it("每项要么标注合法 width、要么在豁免清单内", () => {
    const tiers = new Set<string>(WIDTH_TIERS);
    const exempt = new Set(WIDTH_EXEMPT_ITEM_IDS);
    const unclassified = SETTINGS_ITEMS.filter((i) => !i.width && !exempt.has(i.id)).map((i) => i.id);
    expect(unclassified, `未标注 width 也未豁免：${unclassified.join("、")}`).toEqual([]);
    const illegal = SETTINGS_ITEMS.filter((i) => i.width && !tiers.has(i.width)).map((i) => `${i.id}=${i.width}`);
    expect(illegal, `width 取值非法：${illegal.join("、")}`).toEqual([]);
  });

  it("豁免清单不重叠、不悬空，且与 width 项相加恰好覆盖全表", () => {
    const byId = new Map(SETTINGS_ITEMS.map((i) => [i.id, i]));
    const overlap = WIDTH_EXEMPT_ITEM_IDS.filter((id) => byId.get(id)?.width);
    expect(overlap, `既标了 width 又豁免（清单过时）：${overlap.join("、")}`).toEqual([]);
    const stale = WIDTH_EXEMPT_ITEM_IDS.filter((id) => !byId.has(id));
    expect(stale, `豁免清单里有未登记的 id：${stale.join("、")}`).toEqual([]);
    expect(new Set(WIDTH_EXEMPT_ITEM_IDS).size, "豁免清单有重复项").toBe(WIDTH_EXEMPT_ITEM_IDS.length);
    expect(SETTINGS_ITEMS.filter((i) => i.width).length + WIDTH_EXEMPT_ITEM_IDS.length).toBe(SETTINGS_ITEMS.length);
  });

  it("WIDTH_CLASS 三档齐备且类名互不相同（与 app.css 的类名同源）", () => {
    expect(Object.keys(WIDTH_CLASS).sort()).toEqual([...WIDTH_TIERS].sort());
    expect(WIDTH_TIERS.map((tier) => WIDTH_CLASS[tier])).toEqual(["w-narrow", "w-mid", "w-wide"]);
  });
});

describe("宽度档：app.css 类与页体（批③）", () => {
  const css = readFileSync(join(SRC, "theme/app.css"), "utf8");

  it("app.css 定三档类，各带 max-width:100%（窄窗不横向溢出）", () => {
    for (const [tier, px] of [
      ["w-narrow", 180],
      ["w-mid", 240],
      ["w-wide", 360],
    ] as const) {
      const rule = css.match(new RegExp(`\\.${tier}\\s*\\{[^}]*\\}`))?.[0];
      expect(rule, `app.css 缺 .${tier} 规则`).toBeTruthy();
      expect(rule, `.${tier} 宽度不是 ${px}px`).toContain(`width: ${px}px`);
      expect(rule, `.${tier} 缺 max-width:100%`).toMatch(/max-width:\s*100%/);
    }
  });

  it("搜索框整行：导航头可换行 + .settings-search 占满一行", () => {
    expect(css).toMatch(/\.settings-nav-head\s*\{[^}]*flex-wrap:\s*wrap/);
    expect(css).toMatch(/\.settings-search\s*\{[^}]*flex:\s*1 0 100%/);
  });

  it("设置页体不再出现像素内联 width（SettingsPage / ProvidersPanel / FontSettings）", () => {
    for (const file of ["SettingsPage.tsx", "ProvidersPanel.tsx", "FontSettings.tsx"]) {
      const offenders = panelSrc(file).match(/\bwidth:\s*\d+/g) ?? [];
      expect(offenders, `${file} 仍有像素内联 width：${offenders.join("、")}`).toEqual([]);
    }
  });

  it("三档在页体里各至少用一处", () => {
    const src = ["SettingsPage.tsx", "ProvidersPanel.tsx", "FontSettings.tsx"].map(panelSrc).join("\n");
    for (const cls of WIDTH_TIERS.map((tier) => WIDTH_CLASS[tier])) {
      expect(src, `${cls} 未在页体里使用`).toContain(`"${cls}"`);
    }
  });

  it("进阶折叠的隐藏类在 app.css 里是 display:none（只加类、不搬 DOM）", () => {
    expect(css).toMatch(/\.settings-advanced-hidden\s*\{\s*display:\s*none/);
  });
});

describe("设置项注册表：搜索 matchSettings（批③）", () => {
  const ids = (q: string) => matchSettings(q, zhT).map((i) => i.id);

  it("空串 / 仅空白返回空数组（调用方据此回到常规导航）", () => {
    expect(matchSettings("", zhT)).toEqual([]);
    expect(matchSettings("   ", zhT)).toEqual([]);
    expect(matchSettings("\t \n", zhT)).toEqual([]);
  });

  it("中文显示名 / 中文关键词 / 英文关键词 / 大小写都命中", () => {
    expect(ids("代理模式")).toContain("network.proxy"); // 显示名
    expect(ids("代理")).toContain("network.proxy"); // 中文关键词
    expect(ids("proxy")).toContain("network.proxy"); // 英文关键词
    expect(ids("PROXY")).toContain("network.proxy"); // 小写归一
    expect(ids("lsp")).toContain("validation.lsp.max_chars"); // 关键词补的 lsp
    expect(ids("日志")).toContain("log.level");
    expect(ids("主题")).toContain("ui.theme");
  });

  it("页名与组名参与命中", () => {
    const budget = ids("全局预算");
    expect(budget.length).toBe(7); // 预算组 7 项全命中
    expect(budget).toEqual([
      "validation.lsp.sync_window_ms",
      "validation.lsp.max_diagnostics",
      "validation.lsp.max_chars",
      "validation.lsp.idle_ttl_ms",
      "validation.lsp.max_servers",
      "validation.lsp.max_file_bytes",
      "validation.lsp.dedupe_limit",
    ]);
    // 页名命中整页（工具与集成页含 MCP / 技能）
    expect(ids("工具与集成")).toContain("mcp.servers");
    expect(ids("工具与集成")).toContain("disabled_skills");
  });

  it("多词 AND：每个词都要命中同一项（词序无关）", () => {
    expect(ids("诊断 毫秒")).toEqual(["validation.lsp.sync_window_ms"]);
    expect(ids("毫秒 诊断")).toEqual(["validation.lsp.sync_window_ms"]);
    expect(ids("诊断 zzz")).toEqual([]);
    expect(ids("proxy 代理")).toContain("network.proxy");
  });

  it("haystack 大小写归一：小写 query 命中「首字母大写 / 全大写缩写」的英文显示名（keywords 里没这个写法）", () => {
    // en 显示名 "Server idle TTL (ms)"：query 用小写缩写 ttl。该词的**大写形式**只存在于显示名里
    // （keywords 只有 lsp/idle/回收/闲置，页面与组名也没有）——删掉 haystack 的 .toLowerCase() 后
    // haystack 里只剩 "TTL"，本用例必红（keywords 全小写只能偶然盖住其它项，盖不住这一项）。
    expect(matchSettings("ttl", enT).map((i) => i.id)).toContain("validation.lsp.idle_ttl_ms");
    // 同形第二例：句首大写的 "Confirm writes creating paths outside workspace"
    expect(matchSettings("confirm", enT).map((i) => i.id)).toContain("approval.confirm_outside_create");
  });

  it("结果稳定排序：页序 → 组序 → 注册表原序", () => {
    // 页序在前：同一次查询里 appearance 的命中排在 tools 之前
    const pageRanks = matchSettings("a", zhT).map((i) => PAGE_ORDER.indexOf(i.page));
    expect([...pageRanks].sort((x, y) => x - y)).toEqual(pageRanks);
    // 多次调用结果一致（纯函数、无隐藏状态）
    expect(ids("lsp")).toEqual(ids("lsp"));
    // 组序：预算组（lspBudget）在发现组（lspDiscovery）之前
    const lspIds = ids("lsp");
    expect(lspIds.indexOf("validation.lsp.sync_window_ms")).toBeLessThan(lspIds.indexOf("validation.lsp.extra_roots"));
  });
});

describe("设置项注册表：进阶项派生（批③）", () => {
  it("进阶项共 11 项（范围本批不变），与注册表 advanced 标记同源", () => {
    expect(ADVANCED_ITEM_IDS.length).toBe(11);
    expect(new Set(ADVANCED_ITEM_IDS).size).toBe(ADVANCED_ITEM_IDS.length);
    expect(new Set(ADVANCED_ITEM_IDS)).toEqual(new Set(SETTINGS_ITEMS.filter((i) => i.advanced).map((i) => i.id)));
  });

  it("advancedCountByPage 逐页统计，拾起来恰好 11", () => {
    expect(PAGE_ORDER.map((p) => advancedCountByPage(p)).reduce((a, b) => a + b, 0)).toBe(11);
    expect(advancedCountByPage("tools")).toBe(8); // LSP 预算 7 + JDK 路径
    expect(advancedCountByPage("security")).toBe(1); // 命令白名单
    expect(advancedCountByPage("providers")).toBe(1); // 活跃模型
    expect(advancedCountByPage("logs")).toBe(1); // 会话详细日志
    expect(advancedCountByPage("appearance")).toBe(0);
    expect(advancedCountByPage("network")).toBe(0);
    expect(advancedCountByPage("about")).toBe(0);
  });

  it("整组皆为进阶项：只有 LSP 预算组（发现组含非进阶的额外 SDK 根目录）", () => {
    expect(isAdvancedOnlyGroup("tools", "settings.lspBudget")).toBe(true);
    expect(isAdvancedOnlyGroup("tools", "settings.validation")).toBe(false);
    expect(isAdvancedOnlyGroup("tools", "settings.lspDiscovery")).toBe(false);
    expect(isAdvancedOnlyGroup("tools", "settings.mcp")).toBe(false);
    expect(isAdvancedOnlyGroup("tools", "settings.skills")).toBe(false);
    expect(isAdvancedOnlyGroup("tools", "settings.nope")).toBe(false);
  });

  it("折叠偏好键固定为 ws_settings_show_advanced（全局单一偏好，不得静默改名）", () => {
    expect(SETTINGS_ADVANCED_PREF_KEY).toBe("ws_settings_show_advanced");
  });
});
