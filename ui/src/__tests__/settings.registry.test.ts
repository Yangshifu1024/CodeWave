// 设置项注册表契约（8 页重划 / 注册表 [docs/settings-ia](../../../docs/settings-ia.md)）：
// 手法同 titlebar.style.test.ts —— node fs 直读源码做「引用闭包」断言，其余断言直接校验注册表数据。
//   1. 引用闭包：`ui/src/features/panels/*.tsx`（扫描目录，非硬编码文件表）里的 t("settings.X") /
//      t('settings.X') / t(`settings.X`) 必须在注册表（项 / 分组 / 页名）内或显式豁免；
//      批④ 起另加两道（见文件末尾 §批④ 返工）：
//      a) 覆盖面从 `panels/*.tsx` 扩到 `features/**/*.tsx`，非 panels 文件按「文件 → 允许段」白名单逐条登记；
//      b) 变量拼出的键名（t(key)）必须进 DYNAMIC_KEY_CALLS 豁免清单，否则静默逃出闭包。
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

/**
 * `t(...)` 调用点三分类（批④ 返工的「变量键名」补丁，口径见 [docs/settings-terminology](../../../docs/settings-terminology.md) §5 守门用例 ②）：
 * - `literal` —— `t("seg.key")`：首个非空字符是引号且引号内是完整键（闭包正则的直接目标）；
 * - `inline`  —— `t(cond ? "a.b" : "c.d")`：首个字符不是引号，但参数区里有字面键（键纳入守护）；
 * - `dynamic` —— `t(key)` / `t(LANG_LABEL_KEY[lang])`：键名由变量拼出，正则看不见 → 必须显式登记。
 * 参数区取 `t(` 之后到首个 `)`（限 130 字符窗口）：足以覆盖本仓库的写法，也不会把兄弟代码的键卷进来。
 */
function callSites(src: string): { text: string; literal: string | null; inline: string[] }[] {
  const literalAt = new Map<number, string>();
  for (const m of src.matchAll(/(?<![A-Za-z0-9_$.])t\(\s*(["'`])([A-Za-z0-9_]+(?:\.[A-Za-z0-9_]+)*)\1/g)) {
    literalAt.set(m.index!, m[2]);
  }
  const sites: { text: string; literal: string | null; inline: string[] }[] = [];
  for (const m of src.matchAll(/(?<![A-Za-z0-9_$.])t\(/g)) {
    const i = m.index!;
    const close = src.indexOf(")", i + 2);
    const end = close === -1 || close > i + 130 ? -1 : close;
    const literal = literalAt.get(i) ?? null;
    const args = end === -1 ? "" : src.slice(i + 2, end);
    const inline = [...args.matchAll(/(["'`])([A-Za-z0-9_]+(?:\.[A-Za-z0-9_]+)*)\1/g)]
      .map((k) => k[2])
      .filter((k) => k.includes(".") && k !== literal);
    sites.push({ text: end === -1 ? src.slice(i, i + 40) : src.slice(i, end + 1), literal, inline });
  }
  return sites;
}

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
 * 组件源码里的字面键（任意段）：t("seg.key") / t('seg.key') / t(`seg.key`)，三种引号都认；
 * 参数区里内联的字面键（t(cond ? "a.b" : "c.d")）同样纳入——否则这种写法能同时躲过
 * 「字面键闭包」与「纯动态键登记」两道门。
 * 批④ 起闭包从「只认 settings.*」扩到「所有字面键」——只认 settings.* 时，
 * 页面借用他段键（settings 页借 composer.effortDefault / sessions.empty）会静默逃出守护。
 * 前置否定断言 `(?<![A-Za-z0-9_$.])` 不可省：否则 `respondExitRequest("cancel")` 这类
 * 以 `t(` 结尾的调用会被误当成 `t("cancel")`（改成任意段后才会暴露的假阳性）。
 * 变量拼出的键名不在此列（看不见）：调用点必须进 DYNAMIC_KEY_CALLS 显式登记，见下面的守门用例。
 */
function literalKeysIn(src: string): string[] {
  const out = new Set<string>();
  for (const site of callSites(src)) {
    if (site.literal) out.add(site.literal);
    for (const k of site.inline) out.add(k);
  }
  return [...out];
}

/** 设置页页体的字面键（相对 panels/ 的文件名） */
function literalKeys(file: string): string[] {
  return literalKeysIn(readFileSync(join(PANELS_DIR, file), "utf8"));
}

/** 只取 settings.* 段并剥掉前缀（批② 的注册表引用闭包用例用） */
function literalSettingKeys(file: string): string[] {
  return literalKeys(file)
    .filter((k) => k.startsWith("settings."))
    .map((k) => k.slice("settings.".length));
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
    // MCP / 技能拆成独立页后：旧段名即新页 key，直达（不再被别名表折到 tools）
    expect(normalizePageKey("mcp")).toBe("mcp");
    expect(normalizePageKey("skills")).toBe("skills");
    expect(normalizePageKey("tools")).toBe("tools");
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
    // MCP 不在 config 内：脏判定走独立 mcp.json 的文本基线（拆页后拥有它的页是 mcp）
    expect(PAGE_FIELDS.mcp).toContain(MCP_FIELD_ID as never);
    // 拆页后字段归属随页面走：tools 页只剩写入后检查四项，不再拥有 MCP / 技能字段
    expect(PAGE_FIELDS.tools).not.toContain(MCP_FIELD_ID as never);
    expect(PAGE_FIELDS.skills).toContain("disabled_skills" as never);
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

  it("写入后检查四项齐备且各带独立 labelKey（SettingsPage 的控件标签从本表派生）", () => {
    const ids = [
      "post_write_check.enabled",
      "post_write_check.command",
      "post_write_check.timeout_seconds",
      "post_write_check.tail_chars",
    ];
    const items = ids.map((id) => SETTINGS_ITEMS.find((i) => i.id === id));
    expect(items.filter((i) => !i), `注册表缺少写入后检查项：${ids.filter((_, i) => !items[i]).join("、")}`).toEqual([]);
    for (const item of items) expect(item!.page, `${item!.id} 不在工具与集成页`).toBe("tools");
    // labelKey 互不相同：否则行的展示名会串
    const labelKeys = items.map((i) => i!.labelKey);
    expect(new Set(labelKeys).size, `写入后检查 labelKey 有重复：${labelKeys.join("、")}`).toBe(labelKeys.length);
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

// ---------- 动态行级锚点（额度灰行「去设置」） ----------
/**
 * 供应商行是**动态条目**（数量与 id 随配置变），不在注册表里 → 它的锚点走独立命名空间
 * `providers.<uuid>`（例外与来源见 [docs/settings-search-and-advanced](../../../docs/settings-search-and-advanced.md) §1.6）。
 * 这里钉三件事：模板串写法、命名空间不与注册表 id 重叠、settingsHit 只有一个消费方。
 */
describe("锚点契约：动态行级锚点命名空间", () => {
  it("供应商列表行带 providers.<id> 行级锚点（模板串，id 由配置决定）", () => {
    expect(panelSrc("ProvidersPanel.tsx")).toContain("data-setting-id={`providers.${p.id}`}");
  });

  it("行级命名空间不与注册表 id 重叠：容器锚点 providers 仍在册，providers.* 一律不是注册表项", () => {
    expect(SETTINGS_ITEMS.some((i) => i.id === "providers")).toBe(true);
    expect(SETTINGS_ITEMS.filter((i) => i.id.startsWith("providers.")).map((i) => i.id)).toEqual([]);
  });

  it("settingsHit 只有 SettingsPage 一个消费方（多一个消费方就会抢请求 / 重复定位）", () => {
    expect(CLOSURE_FILES.filter((f) => panelSrc(f).includes("settingsHit"))).toEqual(["SettingsPage.tsx"]);
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
    expect(ids("eslint")).toContain("post_write_check.command"); // 命令示例关键词
    expect(ids("日志")).toContain("log.level");
    expect(ids("主题")).toContain("ui.theme");
  });

  it("页名与组名参与命中", () => {
    // 组名「写入后检查」命中整组 4 项（组名 + 各项关键词）
    const group = ids("写入后检查");
    expect(group).toEqual([
      "post_write_check.enabled",
      "post_write_check.command",
      "post_write_check.timeout_seconds",
      "post_write_check.tail_chars",
    ]);
    // 页名命中整页（工具与集成页含 MCP / 技能）
    expect(ids("工具与集成")).toContain("mcp.servers");
    expect(ids("工具与集成")).toContain("disabled_skills");
  });

  it("多词 AND：每个词都要命中同一项（词序无关）", () => {
    expect(ids("输出 字符")).toEqual(["post_write_check.tail_chars"]);
    expect(ids("字符 输出")).toEqual(["post_write_check.tail_chars"]);
    expect(ids("输出 zzz")).toEqual([]);
    expect(ids("proxy 代理")).toContain("network.proxy");
  });

  it("haystack 大小写归一：小写 query 命中英文显示名（keywords 里没这个写法）", () => {
    // en 显示名 "Output tail chars handed to the model"：query 用小写 output，该词只存在于显示名里
    // （keywords 只有 tail/输出/字符/尾部/截断）——删掉 haystack 的 .toLowerCase() 后本用例必红。
    expect(matchSettings("output", enT).map((i) => i.id)).toContain("post_write_check.tail_chars");
    // 同形第二例：句首大写的 "Confirm writes creating paths outside workspace"
    expect(matchSettings("confirm", enT).map((i) => i.id)).toContain("approval.confirm_outside_create");
  });

  it("结果稳定排序：页序 → 组序 → 注册表原序", () => {
    // 页序在前：同一次查询里 appearance 的命中排在 tools 之前
    const pageRanks = matchSettings("a", zhT).map((i) => PAGE_ORDER.indexOf(i.page));
    expect([...pageRanks].sort((x, y) => x - y)).toEqual(pageRanks);
    // 多次调用结果一致（纯函数、无隐藏状态）
    expect(ids("check")).toEqual(ids("check"));
    // 组内按注册表原序：写入后检查四项的顺序稳定
    expect(ids("写入后检查")).toEqual([
      "post_write_check.enabled",
      "post_write_check.command",
      "post_write_check.timeout_seconds",
      "post_write_check.tail_chars",
    ]);
  });
});

describe("设置项注册表：进阶项派生（批③）", () => {
  it("进阶项共 4 项（写入后检查 2 + 命令白名单 + 会话详细日志），与注册表 advanced 标记同源", () => {
    expect(ADVANCED_ITEM_IDS.length).toBe(4);
    expect(new Set(ADVANCED_ITEM_IDS).size).toBe(ADVANCED_ITEM_IDS.length);
    expect(new Set(ADVANCED_ITEM_IDS)).toEqual(new Set(SETTINGS_ITEMS.filter((i) => i.advanced).map((i) => i.id)));
  });

  it("advancedCountByPage 逐页统计，拾起来恰好 4", () => {
    expect(PAGE_ORDER.map((p) => advancedCountByPage(p)).reduce((a, b) => a + b, 0)).toBe(4);
    expect(advancedCountByPage("tools")).toBe(2); // 写入后检查：超时 / 输出尾部字符
    expect(advancedCountByPage("security")).toBe(1); // 命令白名单
    // 模型与供应商页：0——该页唯一候选 active_model_id 的锚点只在编辑视图，页级开关对它无意义
    expect(advancedCountByPage("providers")).toBe(0);
    expect(advancedCountByPage("logs")).toBe(1); // 会话详细日志
    expect(advancedCountByPage("appearance")).toBe(0);
    expect(advancedCountByPage("network")).toBe(0);
    expect(advancedCountByPage("about")).toBe(0);
  });

  it("整组皆为进阶项：写入后检查组含非进阶的开关与命令，故不是整组折叠", () => {
    expect(isAdvancedOnlyGroup("tools", "settings.postWriteCheck")).toBe(false);
    expect(isAdvancedOnlyGroup("tools", "settings.mcp")).toBe(false);
    expect(isAdvancedOnlyGroup("tools", "settings.skills")).toBe(false);
    expect(isAdvancedOnlyGroup("tools", "settings.nope")).toBe(false);
  });

  it("折叠偏好键固定为 ws_settings_show_advanced（全局单一偏好，不得静默改名）", () => {
    expect(SETTINGS_ADVANCED_PREF_KEY).toBe("ws_settings_show_advanced");
  });
});

// ---------- 批④：术语与 i18n 键统一（[docs/settings-terminology](../../../docs/settings-terminology.md)） ----------

/**
 * 本目录里**不是设置页页体**的独立面板（各有自己的键段，与设置无关）。
 * 豁免粒度是「文件 → 段」而不是「整文件」：在 TasksPage.tsx 里写 t("sessions.empty") 同样会被抓住。
 */
const NON_SETTINGS_PANEL_SEGMENTS: Record<string, string[]> = {
  "TasksPage.tsx": ["tasks."],
  "TokenStatsModal.tsx": ["stats."],
  "UpdateModal.tsx": ["updater."],
};

/** 设置页页体允许引用的键段（批④ 收敛目标：跨页通用动作进 common，页面专属留页面段） */
const ALLOWED_KEY_PREFIXES = ["settings.", "common."];

describe("设置项注册表：panels 不得借他段键（批④）", () => {
  it("panels/*.tsx 里的字面键必须属于 settings.* / common.*，或在显式豁免清单内", () => {
    const offenders: string[] = [];
    for (const file of CLOSURE_FILES) {
      const exempt = NON_SETTINGS_PANEL_SEGMENTS[file] ?? [];
      for (const key of literalKeys(file)) {
        if (ALLOWED_KEY_PREFIXES.some((p) => key.startsWith(p))) continue;
        if (exempt.some((p) => key.startsWith(p))) continue;
        offenders.push(`${file}: ${key}`);
      }
    }
    expect(offenders, `设置页借用了非 settings.*/common.* 的键：${offenders.join("、")}`).toEqual([]);
  });

  it("豁免清单只覆盖非设置页面板，且不悬空 / 不整文件豁免", () => {
    for (const [file, segments] of Object.entries(NON_SETTINGS_PANEL_SEGMENTS)) {
      expect(CLOSURE_FILES, `${file} 不在闭包扫描范围`).toContain(file);
      const keys = literalKeys(file);
      expect(keys.length, `${file} 未取到任何字面键`).toBeGreaterThan(0);
      // 该文件仍必须引用豁免段内的键：否则清单已过时（或文件悄悄变成了设置页页体）
      expect(
        keys.some((k) => segments.some((s) => k.startsWith(s))),
        `${file} 已无豁免段键，清单过时`,
      ).toBe(true);
      // 豁免段不得与允许段重叠（否则豁免会掩盖真正的借键）
      expect(segments.some((s) => ALLOWED_KEY_PREFIXES.some((p) => s.startsWith(p)))).toBe(false);
    }
  });

  it("正则失效守卫：源码里出现 t( 的页体都必须被扫到字面键（防静默逃逸）", () => {
    for (const file of CLOSURE_FILES) {
      const src = readFileSync(join(PANELS_DIR, file), "utf8");
      if (!/\bt\(/.test(src)) continue;
      expect(
        literalKeys(file).length,
        `${file} 含 t( 却未取到任何字面键（正则失效？变量拼出的键请进 DYNAMIC_KEY_CALLS 登记）`,
      ).toBeGreaterThan(0);
    }
  });
});

// ---------- 批④ 返工：守门② 扩到 features 全目录 + 变量键名（[docs/settings-terminology](../../../docs/settings-terminology.md) §5） ----------

/** `features/` 根（守门② 的覆盖面从 `panels/` 扩到 `features/**`） */
const FEATURES_DIR = join(SRC, "features");

/** `features/` 下排除 `panels/`（已由上面的闭包覆盖）的全部 `.tsx`，路径相对 `features/`（POSIX 分隔符） */
function listFeatureFiles(dir: string, prefix = ""): string[] {
  return readdirSync(dir, { withFileTypes: true }).flatMap((entry) => {
    const rel = prefix ? `${prefix}/${entry.name}` : entry.name;
    if (entry.isDirectory()) return rel === "panels" ? [] : listFeatureFiles(join(dir, entry.name), rel);
    return entry.name.endsWith(".tsx") ? [rel] : [];
  });
}

const FEATURE_FILES = listFeatureFiles(FEATURES_DIR).sort();

/** 读 `features/` 下的组件源码（相对 `features/` 的路径，panels 用 `panels/X.tsx`） */
function featureSrc(rel: string): string {
  return readFileSync(join(FEATURES_DIR, rel), "utf8");
}

/** 源码里用到的 i18n 段（以 `.` 结尾，如 `queue.`） */
function usedSegments(src: string): string[] {
  return [...new Set(literalKeysIn(src).map((k) => `${k.split(".")[0]}.`))].sort();
}

/**
 * 纯动态键调用点（`t(key)` / `t(LANG_LABEL_KEY[lang])`）：键名由变量拼出，闭包正则看不见。
 * 这类调用点必须逐条登记进 DYNAMIC_KEY_CALLS，否则静默逃出守门②。
 */
function pureDynamicCalls(src: string): string[] {
  return callSites(src)
    .filter((s) => !s.literal && s.inline.length === 0)
    .map((s) => s.text);
}

/** 应用级共享段：任何文件都可引用（`app.*` 应用级通用文案；`common.*` 批④ 新增的跨页通用动作） */
const SHARED_SEGMENTS = ["app.", "common."];

/**
 * 功能目录 → 自有段：目录内的文件只允许引用自有段 + 共享段 + 登记在案的跨段借用。
 * 这道表负责机械判定「哪些段是借来的」——`settings.*` 不在任何非 panels 目录的自有段里，
 * 因此任何非设置页文件引用它都必须登记（批④ 自己新增的 `LspGuideCard → settings.lspJavaCost` 就是这一格）。
 */
const DIR_OWNED_SEGMENTS: Record<string, string[]> = {
  chat: ["chat.", "composer.", "notice.", "queue."], // 消息区 / 输入区 / 引导条 / 运行队列
  files: ["files."], // 会话产物与文件列表
  quota: [], // 额度与余额：全部文案挂在右栏 `rightbar.*`，见 CROSS_SEGMENT_BORROWINGS
  shell: ["closeTab.", "exitApp.", "git.", "nav.", "rightbar.", "sessions.", "skills.", "titlebar."], // 壳层：左导航 / 顶栏 / 右栏 / 拦截框 / git 身份条 / 技能详情
  subagent: ["subagent."], // 子代理抽屉与卡片
  tools: ["ask.", "tools."], // 审批面板与工具调用卡
  workspace: ["diff."], // 变更面板
};

/**
 * 「文件 → 允许的 i18n 段」白名单（**精确到文件，不做整目录放行**）。
 * 关掉的逃逸面：本守门原只覆盖 `features/panels/*.tsx`——在 `chat/QueuePanel.tsx` 里写 `t("sessions.empty")`、
 * 在 `shell/RightBar.tsx` 里把空态换成他段键，全量用例零变红，而看不见的段恰好最容易悄悄借错。
 * 每条的构成 = 该文件功能面的段 + 共享段（SHARED_SEGMENTS）；跨段借用另在 CROSS_SEGMENT_BORROWINGS 逐条写理由。
 * 新增文件 / 新增段：先想清楚归属再登记（未登记即判红——这正是本表的用意）。
 */
const FEATURE_FILE_SEGMENTS: Record<string, string[]> = {
  "chat/ChatMessages.tsx": ["app.", "chat.", "notice."], // chat.* 消息区；app.* 空态；notice.* 模型设置引导
  "chat/Composer.tsx": ["app.", "composer.", "settings.", "subagent."], // composer.* 自有；另两段为跨段借用（见下）
  "chat/ContextInfoBar.tsx": ["app."], // 仅 app.compact（信息条）
  "chat/ExternalDirPrompt.tsx": ["composer."], // 项目外目录放行确认框（文案归 composer 段，与附件入口同一处）
  "chat/QueuePanel.tsx": ["common.", "queue."], // queue.* 自有；common.delete 通用删除动作
  "chat/segments.tsx": ["chat."], // 流式段落状态词
  "files/FileViewerModal.tsx": ["files."], // 产物预览弹窗
  "files/FilesPanel.tsx": ["files."], // 产物登记列表
  "quota/QuotaSection.tsx": ["rightbar."], // 额度段挂在右栏信息页，沿用 rightbar.*
  "shell/AppShell.tsx": ["app.", "closeTab.", "exitApp.", "git.", "nav."], // 壳层：顶栏动作 / 两个拦截框 / git 身份条 / 中断提示
  "shell/OpenInEditorSelect.tsx": ["rightbar."], // 右栏「在编辑器中打开」下拉
  "shell/ProjectNav.tsx": ["common.", "nav.", "sessions.", "tasks."], // 左导航 nav.*；common.* 通用动作；sessions.rename 会话重命名；tasks.* 见 CROSS_SEGMENT_BORROWINGS
  "shell/RightBar.tsx": ["common.", "rightbar."], // rightbar.* 自有；common.builtin 与设置页共用的来源标签
  "shell/SkillDetailModal.tsx": ["skills."], // 技能详情弹层
  "shell/TopBar.tsx": ["app.", "titlebar."], // 顶栏：app.* 折叠/统计动作；titlebar.* 标题栏
  "subagent/SubagentDrawer.tsx": ["composer.", "subagent."], // 子代理抽屉；composer. 为档位行借用（见下）
  "subagent/SubagentItemCard.tsx": ["subagent."], // 子代理卡片
  "tools/AskPanel.tsx": ["ask."], // 审批面板
  "tools/ToolCallCard.tsx": ["tools."], // 工具调用卡
  "tools/WidgetPreviewModal.tsx": ["tools."], // render_html 大弹框预览（[docs/html-preview-modal](../../../docs/html-preview-modal.md)）
  "workspace/ChangesPanel.tsx": ["app.", "diff."], // diff.* 变更面板；app.retry 通用重试
};

/**
 * 跨段借用登记（文件 → 借来的段 → 理由）：借别的功能面的段必须逐条写理由，
 * 否则「允许段清单」会变成一张谁都可以往上加段的橡皮图章。
 */
const CROSS_SEGMENT_BORROWINGS: Record<string, Record<string, string>> = {
  "chat/Composer.tsx": {
    "settings.": "既有的跨页借键（早于批④，本批未动）：推理强度控件标题复用模型表单字段名 settings.reasoning",
    "subagent.": "既有的跨段借键（早于批④）：输入区运行中子代理计数复用 subagent.runningCount",
  },
  "shell/ProjectNav.tsx": {
    "tasks.": "左栏任务区展示的就是计划任务（与任务页共用 stores/tasks 单一数据源），状态/下次触发/历史等文案复用任务页的 tasks.* 段——两处说的是同一件事，另起一段反而会漂移",
  },
  "subagent/SubagentDrawer.tsx": {
    "composer.": "抽屉头部档位行（[docs/mode-gate-and-subagent-sync]）显示的权限档位与 Composer 胶囊是同一件事：同一档位在两处必须同名，复用 composer.mode* 键，另起 subagent.mode* 会让两套名字漂移",
  },
  "quota/QuotaSection.tsx": {
    "rightbar.": "额度段是本目录独立的组件（quota/）但渲染在右栏信息页里，文案沿用右栏段 rightbar.*",
  },
};

/**
 * 纯动态键调用点豁免清单（文件 → 调用点原文 → 理由）。
 * 键名由变量拼出，字面键扫描看不见，所以必须逐条登记（未登记即判红）；
 * 键值域由别处守护：注册表用例（labelKey / PAGE_LABEL_KEY）与 i18n 双侧键集合用例。
 */
const DYNAMIC_KEY_CALLS: Record<string, Record<string, string>> = {
  "panels/SettingsPage.tsx": {
    "t(item.labelKey)": "搜索命中行的项名由注册表 labelKey 派生",
    "t(PAGE_LABEL_KEY[item.page])": "搜索命中行的所属页名由注册表页名键派生",
    "t(group.titleKey)": "左导航组标题由 PAGE_GROUPS.titleKey 派生",
    "t(PAGE_LABEL_KEY[key])": "左导航页名由注册表页名键派生",
    "t(activePage.labelKey)": "操作条里的当前页名由注册表页名键派生",
  },
  "chat/ChatMessages.tsx": { "t(hintKey)": "错误引导文案键随消息元数据派生（各 kind 的文案键由后端回喂）" },
  "chat/Composer.tsx": { "t(modeDescKeys[mode])": "审批模式说明键由 mode 派生（modeDescKeys 表）" },
  "quota/QuotaSection.tsx": {
    "t(`rightbar.window.${k}`)": "窗口名键由 entry.key 派生（rolling/weekly/monthly 三键已由 i18n 键集合用例断言）",
    "t(reset.key, reset.params)": "重置倒计时文案的键与参数由 countdown() 组装",
    "t(updated.key, updated.params)": "额度更新时间文案的键与参数由相对时间计算组装",
  },
  "tools/ToolCallCard.tsx": {
    "t(key)": "工具动词键由 VERBS[tool.tool] 派生",
    "t(neutralErrKey)": "中性错误码（E_INTERRUPTED / E_ASK_*）→ 文案键由 NEUTRAL_ERR_KEYS 表派生（三键已由 i18n 键集合用例断言）",
  },
};

describe("features 全目录：不得跨段借键（批④ 返工 · 守门②扩面）", () => {
  it("每个用到字面键的非 panels 文件都在「文件 → 允许段」白名单内（新文件未登记即判红）", () => {
    const withKeys = FEATURE_FILES.filter((rel) => callSites(featureSrc(rel)).length > 0);
    const unregistered = withKeys.filter((rel) => !FEATURE_FILE_SEGMENTS[rel]);
    const stale = Object.keys(FEATURE_FILE_SEGMENTS).filter((rel) => !withKeys.includes(rel));
    expect({ unregistered, stale }).toEqual({ unregistered: [], stale: [] });
  });

  it("非 panels 文件的字面键必须落在本文件的允许段内（在 QueuePanel 里写 t(\"sessions.empty\") 必红）", () => {
    const offenders: string[] = [];
    for (const [rel, allowed] of Object.entries(FEATURE_FILE_SEGMENTS)) {
      for (const key of literalKeysIn(featureSrc(rel))) {
        if (!allowed.some((p) => key.startsWith(p))) offenders.push(`${rel}: ${key}`);
      }
    }
    expect(offenders, `非设置页组件借用了未允许的键段：${offenders.join("、")}`).toEqual([]);
  });

  it("白名单里的段要么是共享段 / 本目录自有段，要么登记在案：借段不能悄悄进白名单", () => {
    const unregistered: string[] = [];
    for (const [rel, segments] of Object.entries(FEATURE_FILE_SEGMENTS)) {
      const owned = DIR_OWNED_SEGMENTS[rel.split("/")[0]] ?? [];
      const borrowed = Object.keys(CROSS_SEGMENT_BORROWINGS[rel] ?? {});
      for (const seg of segments) {
        if (SHARED_SEGMENTS.includes(seg) || owned.includes(seg) || borrowed.includes(seg)) continue;
        unregistered.push(`${rel}: ${seg}`);
      }
    }
    expect(unregistered, `跨段借用未登记理由：${unregistered.join("、")}`).toEqual([]);

    // 反向：登记的借用必须在白名单里、且该文件真的还在用（清单不悬空）
    const bad: string[] = [];
    for (const [rel, borrows] of Object.entries(CROSS_SEGMENT_BORROWINGS)) {
      const allowed = FEATURE_FILE_SEGMENTS[rel] ?? [];
      const used = usedSegments(featureSrc(rel));
      for (const seg of Object.keys(borrows)) {
        if (!allowed.includes(seg)) bad.push(`${rel} 的借用段 ${seg} 不在允许段清单里`);
        else if (!used.includes(seg)) bad.push(`${rel} 已不再用 ${seg}，借用登记过时`);
      }
    }
    expect(bad, bad.join("、")).toEqual([]);
  });

  it("每个目录都有自有段登记（除 quota/ 这种整体借他段渲染的目录外，不得为空）", () => {
    const dirs = [...new Set(FEATURE_FILES.map((rel) => rel.split("/")[0]))].sort();
    const missing = dirs.filter((d) => !(d in DIR_OWNED_SEGMENTS));
    expect(missing, `目录未登记自有段：${missing.join("、")}`).toEqual([]);
    expect(DIR_OWNED_SEGMENTS.quota, "quota/ 是「全部文案借右栏段」的唯一例外，改动请连带更新说明").toEqual([]);
  });
});

describe("守门② · 变量键名（批④ 返工）", () => {
  const SCANNED = [...CLOSURE_FILES.map((f) => `panels/${f}`), ...FEATURE_FILES];

  it("每个文件的 t( 调用点三分类可对账（字面键 + 内联字面键 + 纯动态），纯动态必须显式登记", () => {
    const offenders: string[] = [];
    for (const rel of SCANNED) {
      const sites = callSites(featureSrc(rel));
      if (sites.length === 0) continue;
      const literal = sites.filter((s) => s.literal).length;
      const inline = sites.filter((s) => !s.literal && s.inline.length > 0).length;
      const dynamic = pureDynamicCalls(featureSrc(rel));
      // 对账：t( 出现次数 = 字面键调用 + 内联字面键调用 + 纯动态调用（不等即有正则失效）
      expect(literal + inline + dynamic.length, `${rel}: t( 调用点分类对账不平`).toBe(sites.length);
      const registered = DYNAMIC_KEY_CALLS[rel] ?? {};
      const unregistered = dynamic.filter((text) => !(text in registered));
      if (unregistered.length) offenders.push(`${rel}: ${unregistered.join(" + ")}`);
    }
    expect(offenders, `变量拼出的键名未登记（闭包看不见，改引他段键也不会红）：${offenders.join("；")}`).toEqual([]);
  });

  it("纯动态键豁免清单不悬空（登记了却已不存在的调用点 = 清单过时）", () => {
    const stale: string[] = [];
    for (const [rel, calls] of Object.entries(DYNAMIC_KEY_CALLS)) {
      const live = pureDynamicCalls(featureSrc(rel));
      for (const text of Object.keys(calls)) if (!live.includes(text)) stale.push(`${rel}: ${text}`);
    }
    expect(stale, `DYNAMIC_KEY_CALLS 有过时条目：${stale.join("、")}`).toEqual([]);
  });
});

/** 展开字典为「叶子键 → 字符串值」（i18n 值层面的术语扫描用） */
function leafValues(node: unknown, prefix = ""): [string, string][] {
  if (node === null || typeof node !== "object" || Array.isArray(node)) {
    return [[prefix, String(node)]];
  }
  return Object.entries(node as Record<string, unknown>).flatMap(([k, v]) =>
    leafValues(v, prefix ? `${prefix}.${k}` : k),
  );
}

describe("术语一致性：写后检查（post-write-check）", () => {
  it("i18n 双语的设置分组名与新术语同源，且全字典不再出现「语法校验」写法", () => {
    expect(zh.settings.postWriteCheck).toBe("写入后检查");
    expect(en.settings.postWriteCheck).toBe("Post-write check");
    const offenders = ([[zh, "zh-CN"], [en, "en-US"]] as const).flatMap(([dict, name]) =>
      leafValues(dict)
        .filter(([, value]) => /语法校验|syntax validation/i.test(value))
        .map(([key, value]) => `${name}: ${key} = ${value}`),
    );
    expect(offenders, `i18n 仍写旧术语：${offenders.join("、")}`).toEqual([]);
  });

  it("已同步的文档与新术语对齐（命名现状的文档整篇不得留旧写法）", () => {
    const docsDir = join(SRC, "..", "..", "docs");
    // 命名现状的文档：含 AGENTS.md 列为必读的技术基准 technical-design.md
    for (const doc of ["settings-ia.md", "settings-terminology.md", "technical-design.md"]) {
      const text = readFileSync(join(docsDir, doc), "utf8");
      expect(text, `${doc} 仍写「写入后语法校验」`).not.toContain("写入后语法校验");
      expect(text, `${doc} 没有出现新术语「写入后检查」`).toContain("写入后检查");
    }
  });
});

// ---------- 会话保留期与清理的登记（[docs/session-cleanup](../../../docs/session-cleanup.md) §3 第 18/22 条） ----------

/**
 * 清理界面的从属文案键（页内的说明 / 选项 / 提示 / 确认框；**不是**可配置项，故不进 SETTINGS_ITEMS，
 * 但必须逐把进 SHELL_SETTING_KEYS——否则页体里的 t("settings.X") 会直接判红）。
 */
const CLEANUP_SHELL_KEYS = [
  "sessionRetentionHint",
  "cleanupNever",
  "cleanupDays",
  "cleanupNeedRetention",
  "cleanupUnsavedFirst",
  "cleanupNonePending",
  "cleanupPreviewFailed",
  "cleanupConfirmTitle",
  "cleanupSaveConfirmTitle",
  "cleanupConfirmDesc",
  "cleanupConfirmListTitle",
  "cleanupOrphanDesc", // → 只删索引外残留文件（会话一条不删）时的确认框说明
  "cleanupConfirmOk",
  "cleanupSaveSkip",
  "cleanupSavedSkipped",
  "cleanupDone",
  "cleanupNeverRun",
  "cleanupLastRun",
  "cleanupLastFailed",
  "cleanupFailed", // → 清理后「N 个会话未能清理」的警示
  "cleanupOrphanExtra", // → 会话与残留数据文件同时要删时，确认框补的一句
  "cleanupDoneOrphans", // → 同一情形的完成提示后缀
];

describe("设置项注册表：会话保留期与清理的登记（[docs/session-cleanup]）", () => {
  it("保留期是 agent 页的页级保存字段：登记项 + 进 PAGE_FIELDS + narrow 档 + 不进即时生效清单", () => {
    const item = SETTINGS_ITEMS.find((i) => i.id === "sessions.retention_days");
    expect(item, "注册表缺 sessions.retention_days").toBeTruthy();
    expect(item!.page, "保留期不在工作区与智能体页").toBe("agent");
    expect(item!.width, "保留期下拉是窄档").toBe("narrow");
    expect(PAGE_FIELDS.agent as string[], "保留期未进 agent 页字段（脏点永不亮）").toContain("sessions.retention_days");
    // 走页级保存：若误登记成即时生效项，脏标记会永远显示「未保存」
    expect(INSTANT_APPLY_FIELD_IDS).not.toContain("sessions.retention_days");
  });

  it("「立即清理」与「上次清理」用 app.* 前缀（无落盘字段）且都登记了宽度豁免", () => {
    for (const id of ["app.cleanup_now", "app.cleanup_status"]) {
      const item = SETTINGS_ITEMS.find((i) => i.id === id);
      expect(item, `注册表缺 ${id}`).toBeTruthy();
      expect(item!.page, `${id} 不在工作区与智能体页`).toBe("agent");
      expect(
        (PAGE_FIELDS.agent as string[]).includes(id),
        `${id} 是 app.* 项（无落盘字段），不该进 PAGE_FIELDS`,
      ).toBe(false);
      expect(WIDTH_EXEMPT_ITEM_IDS, `${id} 未登记宽度豁免（无独立控件宽度）`).toContain(id);
    }
  });

  it("三个新项都不标进阶（进阶项总数保持 4，页级折叠不牵动清理界面）", () => {
    for (const id of ["sessions.retention_days", "app.cleanup_now", "app.cleanup_status"]) {
      expect(SETTINGS_ITEMS.find((i) => i.id === id)?.advanced, `${id} 不该标进阶`).toBeFalsy();
    }
    expect(ADVANCED_ITEM_IDS.length).toBe(4);
    expect(advancedCountByPage("agent"), "工作区与智能体页进阶项数不应因清理界面变化").toBe(0);
  });

  it("清理的从属文案键逐把登记进 SHELL_SETTING_KEYS（未登记即被引用闭包判红）", () => {
    for (const key of CLEANUP_SHELL_KEYS) {
      expect(SHELL_SETTING_KEYS, `${key} 未登记进 SHELL_SETTING_KEYS`).toContain(key);
    }
    // 三个项名键只作为「项」存在：同时进豁免清单会被「豁免与注册表不重叠」判红
    for (const labelKey of ["sessionRetention", "cleanupNow", "cleanupStatus"]) {
      expect(SHELL_SETTING_KEYS, `${labelKey} 已是设置项名，不得再进豁免清单`).not.toContain(labelKey);
    }
  });

  it("清理界面可被搜索命中（「清理」「保留期」两个问法都能找到保留期项）", () => {
    const retention = SETTINGS_ITEMS.find((i) => i.id === "sessions.retention_days")!;
    expect(matchSettings("保留期", zhT).map((i) => i.id)).toContain(retention.id);
    expect(matchSettings("清理", zhT).map((i) => i.id)).toContain("app.cleanup_now");
  });
});

// ---------- 旧格式历史清理的登记（分段 JSONL 落地后的显式入口） ----------

/**
 * 旧格式历史清理界面的从属文案键（页内说明 / 两个按钮 / 状态行 / 确认框 / 完成与保留提示）：
 * **不是**可配置项，故不进 SETTINGS_ITEMS，但必须逐把进 SHELL_SETTING_KEYS——
 * 否则页体里的 t("settings.X") 会直接判红（引用闭包）。
 */
const LEGACY_SHELL_KEYS = [
  "legacyHistoryHint",
  "legacyHistoryPreview",
  "legacyHistoryNow",
  "legacyHistoryStatusHint",
  "legacyHistoryUnknown",
  "legacyHistoryNone",
  "legacyHistoryNoneCleanable",
  "legacyHistoryPreviewLine",
  "legacyHistoryPreviewKeep",
  "legacyHistoryPreviewDone",
  "legacyHistoryConfirmTitle",
  "legacyHistoryConfirmDesc",
  "legacyHistoryConfirmKeep",
  "legacyHistoryConfirmOk",
  "legacyHistoryDone",
  "legacyHistoryKept",
  "legacyHistoryFailed",
  "legacyHistoryPreviewFailed",
];

describe("设置项注册表：旧格式历史清理的登记", () => {
  it("两项都是 app.* 动作 / 只读项（不进 PAGE_FIELDS）、带宽度豁免、不标进阶", () => {
    for (const id of ["app.legacy_history_cleanup", "app.legacy_history_status"]) {
      const item = SETTINGS_ITEMS.find((i) => i.id === id);
      expect(item, `注册表缺 ${id}`).toBeTruthy();
      expect(item!.page, `${id} 不在工作区与智能体页`).toBe("agent");
      expect(item!.labelKey.startsWith("settings."), `${id} 的 labelKey 不在 settings 段`).toBe(true);
      expect(
        (PAGE_FIELDS.agent as string[]).includes(id),
        `${id} 是 app.* 项（无落盘字段），不该进 PAGE_FIELDS`,
      ).toBe(false);
      expect(WIDTH_EXEMPT_ITEM_IDS, `${id} 未登记宽度豁免（无独立控件宽度）`).toContain(id);
      expect(item!.advanced, `${id} 不该标进阶`).toBeFalsy();
    }
    // 进阶项总数不变（页级折叠不牵动旧格式清理界面）
    expect(ADVANCED_ITEM_IDS.length).toBe(4);
    expect(advancedCountByPage("agent")).toBe(0);
  });

  it("两个项名不得再进豁免清单（既是项也是豁免 = 清单重叠）", () => {
    for (const labelKey of ["legacyHistoryCleanup", "legacyHistoryStatus"]) {
      expect(SHELL_SETTING_KEYS, `${labelKey} 已是设置项名`).not.toContain(labelKey);
    }
  });

  it("从属文案键逐把登记进 SHELL_SETTING_KEYS（未登记即被引用闭包判红）", () => {
    for (const key of LEGACY_SHELL_KEYS) {
      expect(SHELL_SETTING_KEYS, `${key} 未登记进 SHELL_SETTING_KEYS`).toContain(key);
    }
  });

  it("中英文两套文案齐备（项名 + 全部从属文案）", () => {
    for (const key of ["legacyHistoryCleanup", "legacyHistoryStatus", ...LEGACY_SHELL_KEYS]) {
      expect(zhKeys.has(`settings.${key}`), `zh-CN 缺 settings.${key}`).toBe(true);
      expect(enKeys.has(`settings.${key}`), `en-US 缺 settings.${key}`).toBe(true);
    }
  });

  it("可被搜索命中（「旧格式历史」「回收」两个问法都能找到入口）", () => {
    expect(matchSettings("旧格式历史", zhT).map((i) => i.id)).toContain("app.legacy_history_cleanup");
    expect(matchSettings("旧格式历史", zhT).map((i) => i.id)).toContain("app.legacy_history_status");
    expect(matchSettings("回收", zhT).map((i) => i.id)).toContain("app.legacy_history_cleanup");
    expect(matchSettings("legacy history", enT).map((i) => i.id)).toContain("app.legacy_history_cleanup");
  });
});
