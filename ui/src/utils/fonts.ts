// 自定义字体偏好（[docs/chat-and-interface](../../../docs/chat-and-interface.md)）：
// 双槽位——sans（界面）/ mono（代码与日志）；值为逗号分隔的已安装字体名，空 = 默认链。
//
// 持久化（2026-09-19 修正）：**真源在后端配置文件**（`config.ui.font_sans` / `font_mono`，经
// `set_font_prefs` 即时落盘）；localStorage 降级为「首帧防闪变缓存」——启动时先按缓存应用到 <html>
// （挂载前，防 FOUC），配置到手后再用 [`reconcileFontsFromConfig`] 对账（后端优先，老版本只存缓存的自动迁移）。
/** 字体槽位：sans = 界面 / mono = 代码与日志 */
export type FontSlot = "sans" | "mono";

/** 字号档位：与同槽位的字体族（sans/mono）正交叠加。离散 4 档 + 还原默认。 */
export type FontSizeLevel = "small" | "default" | "large" | "xlarge";

/** 字号档位 → CSS px 映射。antd `token.fontSize` 走 sans 字号（与 `font_sans` 同源），
 *  所以此表的 4 档在设置页/全 UI 可见。空字符串 `""` = 还原为默认 15 */
export const FONT_SIZE_PX: Record<FontSizeLevel, number> = {
  small: 13,
  default: 15,
  large: 17,
  xlarge: 19,
};

/** 4 个档位全枚举，给 `Select` options 用 */
export const FONT_SIZE_LEVELS: readonly FontSizeLevel[] = ["small", "default", "large", "xlarge"] as const;

/** 反向：px 数字 → 档位（未知值返 null；调用方决定如何处理） */
export function pxToLevel(px: number): FontSizeLevel | null {
  for (const lvl of FONT_SIZE_LEVELS) if (FONT_SIZE_PX[lvl] === px) return lvl;
  return null;
}

/** 两个槽位的字体偏好（空串 = 使用默认链；空档位 = 使用默认字号） */
export interface FontPreferences {
  sans: string;
  mono: string;
  fontSizeSans: FontSizeLevel;
  fontSizeMono: FontSizeLevel;
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
/** 字号档位在 localStorage 里也各占一个键，首帧 anti-FOUC 与 sans/mono 同构 */
const SIZE_STORAGE_KEYS: Record<"sans" | "mono", string> = {
  sans: "ws_font_size_sans",
  mono: "ws_font_size_mono",
};

/** 各槽位要覆写的 CSS 变量及其回退链变量（定义于 native.css :root） */
const FONT_PROPERTIES: Record<FontSlot, { override: string; fallback: string }> = {
  sans: { override: "--ws-font-sans", fallback: "--ws-font-sans-fallback" },
  mono: { override: "--ws-font-mono", fallback: "--ws-font-mono-fallback" },
};
/** 字号覆写变量：双槽各一个。回退链变量 = native.css 里 antd 默认 14px 下的同名字号 */
const FONT_SIZE_PROPERTIES: Record<"sans" | "mono", { override: string; fallback: string }> = {
  sans: { override: "--ws-font-size-sans", fallback: "--ws-font-size-sans-fallback" },
  mono: { override: "--ws-font-size-mono", fallback: "--ws-font-size-mono-fallback" },
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

/** 净化后的字号档位串：仅接受 4 档枚举；其他值（裁外、超范围、手改）回退 `default` */
export function sanitizeFontLevel(input: string): FontSizeLevel {
  if (input === "small" || input === "default" || input === "large" || input === "xlarge") return input;
  return "default";
}

/** 净化字号档位串为纯字串（= 4 档之一；同 `sanitizeFontLevel`，返回 string 类型供存储/读存储） */
export function sanitizeFontLevelStr(input: string): FontSizeLevel {
  return sanitizeFontLevel(input);
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

/** 写 CSS 变量：字号档位 → px 字符串 */
function applyFontSize(slot: "sans" | "mono", level: FontSizeLevel): void {
  const { override, fallback } = FONT_SIZE_PROPERTIES[slot];
  const px = FONT_SIZE_PX[level];
  // 默认档位 = 不写 CSS 变量（让 native.css 里的回退链生效），避免老用户首屏胉动
  if (level === "default") {
    document.documentElement.style.removeProperty(override);
  } else {
    document.documentElement.style.setProperty(override, `${px}px`);
  }
  // 同步 backup/fallback 变量（反香中供 antd token 使用）。默认档位不写，避免老用户被强覆盖
  if (level !== "default") {
    document.documentElement.style.setProperty(fallback, `${px}px`);
  } else {
    document.documentElement.style.removeProperty(fallback);
  }
}

/** 从 localStorage 读当前偏好（"" / 未设置 = 默认） */
export function readStoredFonts(): FontPreferences {
  if (typeof localStorage === "undefined") {
    return { sans: "", mono: "", fontSizeSans: "default", fontSizeMono: "default" };
  }
  return {
    sans: localStorage.getItem(STORAGE_KEYS.sans) ?? "",
    mono: localStorage.getItem(STORAGE_KEYS.mono) ?? "",
    fontSizeSans: sanitizeFontLevel(localStorage.getItem(SIZE_STORAGE_KEYS.sans) ?? ""),
    fontSizeMono: sanitizeFontLevel(localStorage.getItem(SIZE_STORAGE_KEYS.mono) ?? ""),
  };
}

/** 读取持久化偏好并应用到 <html>；在 React 挂载前运行（main.tsx）以防 FOUC */
export function applyInitialFonts(): void {
  const stored = readStoredFonts();
  applyFont("sans", sanitizeFontList(stored.sans));
  applyFont("mono", sanitizeFontList(stored.mono));
  applyFontSize("sans", stored.fontSizeSans);
  applyFontSize("mono", stored.fontSizeMono);
}

/** 持久化（此处做净化）并立即应用；返回净化后的值供调用方回填草稿 */
export function storeFonts(prefs: FontPreferences): FontPreferences {
  const saved: FontPreferences = {
    sans: sanitizeFontList(prefs.sans),
    mono: sanitizeFontList(prefs.mono),
    fontSizeSans: sanitizeFontLevel(prefs.fontSizeSans),
    fontSizeMono: sanitizeFontLevel(prefs.fontSizeMono),
  };
  if (typeof localStorage !== "undefined") {
    for (const slot of ["sans", "mono"] as const) {
      if (saved[slot] === "") localStorage.removeItem(STORAGE_KEYS[slot]);
      else localStorage.setItem(STORAGE_KEYS[slot], saved[slot]);
    }
    for (const slot of ["sans", "mono"] as const) {
      const key = SIZE_STORAGE_KEYS[slot];
      if (saved[slot === "sans" ? "fontSizeSans" : "fontSizeMono"] === "default") localStorage.removeItem(key);
      else localStorage.setItem(key, saved[slot === "sans" ? "fontSizeSans" : "fontSizeMono"]);
    }
  }
  applyFont("sans", saved.sans);
  applyFont("mono", saved.mono);
  applyFontSize("sans", saved.fontSizeSans);
  applyFontSize("mono", saved.fontSizeMono);
  return saved;
}

/** 提交一个字体槽位：净化 + 写缓存 + 立即应用（返回净化后的两槽值）。 */
export function commitFontSlot(slot: FontSlot, candidate: string): FontPreferences {
  return storeFonts({ ...readStoredFonts(), [slot]: candidate });
}

/** 提交一个字号槽位：净化档位 + 写缓存 + 立即应用（返回净化后的两字号+两字体值） */
export function commitFontSizeSlot(slot: "sans" | "mono", level: FontSizeLevel): FontPreferences {
  return storeFonts({
    ...readStoredFonts(),
    [slot === "sans" ? "fontSizeSans" : "fontSizeMono"]: level,
  });
}

/** 后端配置里的字体偏好（`config.ui.font_sans` / `font_mono`；字号同后端为 px 数字） */
export interface BackendFontPrefs {
  sans?: string;
  mono?: string;
  /** 后端存的字号是 px f32；为空 / 缺 / 未知 → 补默认 15 */
  font_size_sans?: number;
  font_size_mono?: number;
}

/**
 * 与后端配置对账（配置加载后调用一次）。
 *
 * 真源在后端配置，localStorage 只是首帧缓存，三者按以下规则协调：
 * 1. 后端有值 → **以后端为准**：应用 + 回写缓存（换版本 / 清缓存 / dev 与打包版之间都不会丢）；
 * 2. 后端为空、缓存有值（老用户）→ 保留缓存值并返回 `migrate = true`，由调用方写回后端（**自动迁移**）；
 * 3. 两侧都空 → 保持默认。
 */
export function reconcileFontsFromConfig(backend: BackendFontPrefs): {
  prefs: FontPreferences;
  migrate: boolean;
} {
  const backendSans = sanitizeFontList(backend.sans ?? "");
  const backendMono = sanitizeFontList(backend.mono ?? "");
  const backendSizeSans = backend.font_size_sans ?? 0;
  const backendSizeMono = backend.font_size_mono ?? 0;
  // 后端有字体或字号任意一项 → 以后端为准（同时回写缓存）
  const hasBackend =
    backendSans !== "" ||
    backendMono !== "" ||
    backendSizeSans > 0 ||
    backendSizeMono > 0;
  if (hasBackend) {
    const prefs: FontPreferences = {
      sans: backendSans,
      mono: backendMono,
      fontSizeSans: pxToLevel(backendSizeSans) ?? "default",
      fontSizeMono: pxToLevel(backendSizeMono) ?? "default",
    };
    storeFonts(prefs);
    return { prefs, migrate: false };
  }
  const cached = readStoredFonts();
  const prefs: FontPreferences = {
    sans: sanitizeFontList(cached.sans),
    mono: sanitizeFontList(cached.mono),
    fontSizeSans: cached.fontSizeSans,
    fontSizeMono: cached.fontSizeMono,
  };
  storeFonts(prefs);
  return { prefs, migrate: prefs.sans !== "" || prefs.mono !== "" || prefs.fontSizeSans !== "default" || prefs.fontSizeMono !== "default" };
}
