// 自定义字体偏好（[docs/custom-font-and-titlebar](../../../docs/custom-font-and-titlebar.md)）：
// 双槽位——sans（界面）/ mono（代码与日志）；值为逗号分隔的已安装字体名，空 = 默认链。
// 经 localStorage 持久化（纯 UI 偏好，不入后端配置）；挂载前应用以防 FOUC。
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
