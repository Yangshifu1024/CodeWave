// 自定义字体偏好（[docs/custom-font-and-titlebar](../../../docs/custom-font-and-titlebar.md)）：
// 双槽位——sans（界面）/ mono（代码与日志）；值为逗号分隔的已安装字体名，空 = 默认链。
//
// 持久化（2026-09-19 修正）：**真源在后端配置文件**（`config.ui.font_sans` / `font_mono`，经
// `set_font_prefs` 即时落盘）；localStorage 降级为「首帧防闪变缓存」——启动时先按缓存应用到 <html>
// （挂载前，防 FOUC），配置到手后再用 [`reconcileFontsFromConfig`] 对账（后端优先，老版本只存缓存的自动迁移）。
/** 字体槽位：sans = 界面 / mono = 代码与日志 */
export type FontSlot = "sans" | "mono";

/** 两个槽位的字体偏好（空串 = 使用默认链） */
export interface FontPreferences {
  sans: string;
  mono: string;
}

/** 设置输入框占位文案：空时展示默认链的首个字体名（与 native.css 对应链保持同步） */
export const DEFAULT_FONT_LEADS: Record<FontSlot, string> = {
  sans: "System",
  mono: "SF Mono",
};

const STORAGE_KEYS: Record<FontSlot, string> = {
  sans: "ws_font_sans",
  mono: "ws_font_mono",
};

/** 各槽位要覆写的 CSS 变量及其回退链变量（定义于 native.css :root） */
const FONT_PROPERTIES: Record<FontSlot, { override: string; fallback: string }> = {
  sans: { override: "--ws-font-sans", fallback: "--ws-font-sans-fallback" },
  mono: { override: "--ws-font-mono", fallback: "--ws-font-mono-fallback" },
};

// 过滤可能逃逸双引号 font-family 值的字符与 Unicode「其他」类（控制/零宽/代理项）；
// 非 ASCII 名称（中文等）保留。
const FORBIDDEN_CHARS = /["'\\;{}<>]|\p{C}/gu;

/**
 * 原始输入 → 安全字体列表：按逗号拆分，逐段去除引号/反斜杠/控制字符并折叠空白，丢弃空段。
 * 空结果 = 使用默认链。
 */
export function sanitizeFontList(input: string): string {
  return input
    .split(",")
    .map((segment) => segment.replace(FORBIDDEN_CHARS, "").replace(/\s+/g, " ").trim())
    .filter((segment) => segment !== "")
    .join(", ");
}

/** 安全列表 → 引入回退链的 CSS font-family 值；空列表返回空串（= 清除覆写） */
export function buildFontOverride(sanitized: string, slot: FontSlot): string {
  if (sanitized === "") return "";
  const { fallback } = FONT_PROPERTIES[slot];
  const leading = sanitized
    .split(",")
    .map((name) => `"${name.trim()}"`)
    .join(", ");
  return `${leading}, var(${fallback})`;
}

/**
 * 设置预览用 font-family：草稿链，草稿为空时用默认链
 * （直接 inherit 会显示已应用的字体而非默认观感）。
 */
export function previewFontFamily(sanitized: string, slot: FontSlot): string {
  return buildFontOverride(sanitized, slot) || `var(${FONT_PROPERTIES[slot].fallback})`;
}

function applyFont(slot: FontSlot, sanitized: string): void {
  const { override } = FONT_PROPERTIES[slot];
  const value = buildFontOverride(sanitized, slot);
  if (value === "") {
    document.documentElement.style.removeProperty(override);
  } else {
    document.documentElement.style.setProperty(override, value);
  }
}

/** 从 localStorage 读当前偏好（"" = 默认链） */
export function readStoredFonts(): FontPreferences {
  if (typeof localStorage === "undefined") return { sans: "", mono: "" };
  return {
    sans: localStorage.getItem(STORAGE_KEYS.sans) ?? "",
    mono: localStorage.getItem(STORAGE_KEYS.mono) ?? "",
  };
}

/** 读取持久化偏好并应用到 <html>；在 React 挂载前运行（main.tsx）以防 FOUC */
export function applyInitialFonts(): void {
  const stored = readStoredFonts();
  applyFont("sans", sanitizeFontList(stored.sans));
  applyFont("mono", sanitizeFontList(stored.mono));
}

/** 持久化（此处做净化）并立即应用；返回净化后的值供调用方回填草稿 */
export function storeFonts(prefs: FontPreferences): FontPreferences {
  const saved: FontPreferences = {
    sans: sanitizeFontList(prefs.sans),
    mono: sanitizeFontList(prefs.mono),
  };
  if (typeof localStorage !== "undefined") {
    for (const slot of ["sans", "mono"] as const) {
      if (saved[slot] === "") localStorage.removeItem(STORAGE_KEYS[slot]);
      else localStorage.setItem(STORAGE_KEYS[slot], saved[slot]);
    }
  }
  applyFont("sans", saved.sans);
  applyFont("mono", saved.mono);
  return saved;
}

/** 提交一个槽位：净化 + 写缓存 + 立即应用（返回净化后的两槽值）。 */
export function commitFontSlot(slot: FontSlot, candidate: string): FontPreferences {
  return storeFonts({ ...readStoredFonts(), [slot]: candidate });
}

/** 后端配置里的字体偏好（`config.ui.font_sans` / `font_mono`；字段可缺 = 旧后端） */
export interface BackendFontPrefs {
  sans?: string;
  mono?: string;
}

/**
 * 与后端配置对账（配置加载后调用一次）。
 *
 * 真源在后端配置，localStorage 只是首帧缓存，两者按三条规则协调：
 * 1. 后端有值 → **以后端为准**：应用 + 回写缓存（换版本 / 清缓存 / dev 与打包版之间都不会丢）；
 * 2. 后端为空、缓存有值（2026-09-19 之前字体只存 localStorage 的老用户）→ 保留缓存值并返回
 *    `migrate = true`，由调用方写回后端（**自动迁移，老设置不丢**）；
 * 3. 两侧都空 → 保持默认链（顺带清掉可能的脏缓存键）。
 */
export function reconcileFontsFromConfig(backend: BackendFontPrefs): {
  prefs: FontPreferences;
  migrate: boolean;
} {
  const backendSans = sanitizeFontList(backend.sans ?? "");
  const backendMono = sanitizeFontList(backend.mono ?? "");
  if (backendSans !== "" || backendMono !== "") {
    const prefs = { sans: backendSans, mono: backendMono };
    storeFonts(prefs); // 应用 + 回写缓存（缓存只是镜像，写失败不影响后端真源）
    return { prefs, migrate: false };
  }
  const cached = readStoredFonts();
  const prefs = { sans: sanitizeFontList(cached.sans), mono: sanitizeFontList(cached.mono) };
  storeFonts(prefs);
  return { prefs, migrate: prefs.sans !== "" || prefs.mono !== "" };
}
