// Custom font preferences ([docs/chat-and-interface](../../../docs/chat-and-interface.md)): sanitize / override building / localStorage round-trip / <html> application
import { describe, it, expect, beforeEach } from "vitest";
import {
  applyInitialFonts, buildFontOverride, previewFontFamily, readStoredFonts, reconcileFontsFromConfig,
  commitFontSizeSlot, commitFontSlot, sanitizeFontList, sanitizeFontLevel, storeFonts,
} from "../utils/fonts";

const defaults = { sans: "", content: "", mono: "", fontSizeSans: "14", fontSizeContent: "14", fontSizeMono: "12" } as const;

beforeEach(() => { localStorage.clear(); document.documentElement.removeAttribute("style"); });

describe("sanitizeFontList", () => {
  it("按逗号拆分并裁剪空白，丢弃空段", () => {
    expect(sanitizeFontList(" Microsoft YaHei , , Segoe UI ,, ")).toBe("Microsoft YaHei, Segoe UI");
  });

  it("剥离可逃逸 CSS 字面量的字符（引号/反斜杠/花括号/分号/尖括号）", () => {
    expect(sanitizeFontList('"a;b{}<>c\\", plain')).toBe("abc, plain");
  });

  it("剥离控制字符但保留中文等非 ASCII 名称", () => {
    expect(sanitizeFontList("微\u0000软\u200B雅黑")).toBe("微软雅黑");
  });

  it("折叠段内连续空白（带引号名匹配会失败）", () => {
    expect(sanitizeFontList("Jet  Brains\t Mono")).toBe("Jet Brains Mono");
  });

  it("全空输入 → 空串（= 默认链）", () => {
    expect(sanitizeFontList('"" , \t,')).toBe("");
  });
});

describe("buildFontOverride / previewFontFamily", () => {
  it("用户链引导默认链兜底", () => {
    expect(buildFontOverride("HarmonyOS Sans, 微软雅黑", "sans")).toBe(
      '"HarmonyOS Sans", "微软雅黑", var(--ws-font-sans-fallback)',
    );
    expect(buildFontOverride("Cascadia Mono", "mono")).toBe(
      '"Cascadia Mono", var(--ws-font-mono-fallback)',
    );
  });

  it("空列表 → 空串（清除覆盖）；预览空草稿时回落默认链", () => {
    expect(buildFontOverride("", "sans")).toBe("");
    expect(previewFontFamily("", "sans")).toBe("var(--ws-font-sans-fallback)");
    expect(previewFontFamily("Cascadia Mono", "mono")).toBe(
      '"Cascadia Mono", var(--ws-font-mono-fallback)',
    );
  });
});

describe("持久化与应用", () => {
  beforeEach(() => {
    localStorage.clear();
    document.documentElement.style.removeProperty("--ws-font-sans");
    document.documentElement.style.removeProperty("--ws-font-mono");
  });

  it("storeFonts：sanitize 后写 localStorage 并内联到 <html>，返回值回填草稿", () => {
    const saved = storeFonts({ ...defaults, sans: ' Map"le ', mono: "  ", fontSizeSans: "14", fontSizeMono: "12" });
    expect(saved).toEqual({ ...defaults, sans: "Maple", mono: "", fontSizeSans: "14", fontSizeMono: "12" });
    expect(localStorage.getItem("ws_font_sans")).toBe("Maple");
    expect(localStorage.getItem("ws_font_mono")).toBeNull(); // empty = key removed
    const style = document.documentElement.style;
    expect(style.getPropertyValue("--ws-font-sans")).toBe('"Maple", var(--ws-font-sans-fallback)');
    expect(style.getPropertyValue("--ws-font-mono")).toBe(""); // not overridden
  });

  it("applyInitialFonts：读取持久化并在挂载前应用（防 FOUC 路径）", () => {
    localStorage.setItem("ws_font_mono", "Consolas, 微软雅黑");
    applyInitialFonts();
    expect(document.documentElement.style.getPropertyValue("--ws-font-mono")).toBe(
      '"Consolas", "微软雅黑", var(--ws-font-mono-fallback)',
    );
    expect(readStoredFonts().mono).toBe("Consolas, 微软雅黑");
  });

  it("端到端防御：localStorage 预置恶意串，applyInitialFonts 输出必已剥离", () => {
    localStorage.setItem("ws_font_sans", 'Red; } body { display:none');
    applyInitialFonts();
    // Escape characters are all stripped into a literal font name; the injection structure (; and braces) is gone
    expect(document.documentElement.style.getPropertyValue("--ws-font-sans")).toBe(
      '"Red body display:none", var(--ws-font-sans-fallback)',
    );
  });

  it("恢复默认：storeFonts 空值移除覆盖与存储键", () => {
    storeFonts({ ...defaults, sans: "Inter", mono: "JetBrains Mono", fontSizeSans: "14", fontSizeMono: "12" });
    storeFonts({ ...defaults, sans: "", mono: "", fontSizeSans: "14", fontSizeMono: "12" });
    expect(readStoredFonts()).toEqual({ ...defaults, sans: "", mono: "", fontSizeSans: "14", fontSizeMono: "12" });
    expect(document.documentElement.style.getPropertyValue("--ws-font-sans")).toBe("");
    expect(document.documentElement.style.getPropertyValue("--ws-font-mono")).toBe("");
  });
});

describe("与后端配置对账（reconcileFontsFromConfig：后端为真源、缓存为首帧镜像）", () => {
  beforeEach(() => {
    localStorage.clear();
    document.documentElement.style.removeProperty("--ws-font-sans");
    document.documentElement.style.removeProperty("--ws-font-mono");
  });

  it("后端有值 → 以后端为准：应用并回写缓存（换版本/清缓存后不丢）", () => {
    localStorage.setItem("ws_font_sans", "Maple");
    const r = reconcileFontsFromConfig({ ...defaults, sans: "PingFang SC", mono: "JetBrains Mono", font_size_sans: 15, font_size_mono: 13 });
    expect(r).toEqual({ prefs: { ...defaults, sans: "PingFang SC", mono: "JetBrains Mono", fontSizeSans: "15", fontSizeMono: "13" }, migrate: false });
    expect(localStorage.getItem("ws_font_sans")).toBe("PingFang SC");
    expect(localStorage.getItem("ws_font_mono")).toBe("JetBrains Mono");
    const style = document.documentElement.style;
    expect(style.getPropertyValue("--ws-font-sans")).toBe('"PingFang SC", var(--ws-font-sans-fallback)');
    expect(style.getPropertyValue("--ws-font-mono")).toBe('"JetBrains Mono", var(--ws-font-mono-fallback)');
  });

  it("后端为空、缓存有值（老版本只存 localStorage）→ 保留缓存并回报待迁移", () => {
    localStorage.setItem("ws_font_mono", "Consolas");
    const r = reconcileFontsFromConfig({});
    expect(r.migrate).toBe(true);
    expect(r.prefs).toEqual({ ...defaults, sans: "", mono: "Consolas", fontSizeSans: "14", fontSizeMono: "12" });
    expect(document.documentElement.style.getPropertyValue("--ws-font-mono")).toBe(
      '"Consolas", var(--ws-font-mono-fallback)',
    );
  });

  it("两侧都空 → 默认链且不迁移（顺带清掉脏缓存键）", () => {
    localStorage.setItem("ws_font_sans", ' "" ; } ');
    const r = reconcileFontsFromConfig({ ...defaults, sans: "", mono: "" });
    expect(r).toEqual({ prefs: { ...defaults, sans: "", mono: "", fontSizeSans: "14", fontSizeMono: "12" }, migrate: false });
    expect(localStorage.getItem("ws_font_sans")).toBeNull();
    expect(document.documentElement.style.getPropertyValue("--ws-font-sans")).toBe("");
  });

  it("serde's default sizes do not erase legacy cached font families", () => {
    localStorage.setItem("ws_font_sans", "Existing Sans");
    localStorage.setItem("ws_font_mono", "Existing Mono");
    const result = reconcileFontsFromConfig({ ...defaults, sans: "", mono: "", font_size_sans: 14, font_size_mono: 12 });
    expect(result).toEqual({ prefs: {
      ...defaults, sans: "Existing Sans", mono: "Existing Mono", fontSizeSans: "14", fontSizeMono: "12",
    }, migrate: true });
    expect(readStoredFonts()).toEqual(result.prefs);
  });

  // The same discrete whitelist applies to every slot.
  it.each([0, -1, 11, 21, 12.5, 100000, Number.NaN, Number.POSITIVE_INFINITY])("unknown backend size %s falls back consistently", (size) => {
    localStorage.setItem("ws_font_size_sans", "xlarge");
    localStorage.setItem("ws_font_size_mono", "small");
    const { prefs } = reconcileFontsFromConfig({ font_size_sans: size, font_size_content: size, font_size_mono: size });
    expect(prefs.fontSizeSans).toBe("14");
    expect(prefs.fontSizeMono).toBe("12");
    expect(prefs.fontSizeContent).toBe("14");
    expect(document.documentElement.style.getPropertyValue("--ws-font-size-sans")).toBe("");
    expect(document.documentElement.style.getPropertyValue("--ws-font-size-mono")).toBe("");
  });

  it.each([12, 13, 14, 15, 16, 17, 18, 19, 20])("backend size %s inside the 9 档 is preserved", (size) => {
    const { prefs } = reconcileFontsFromConfig({ font_size_sans: size, font_size_content: size, font_size_mono: size });
    expect(prefs.fontSizeSans).toBe(String(size));
    expect(prefs.fontSizeMono).toBe(String(size));
    expect(prefs.fontSizeContent).toBe(String(size));
  });

  it("size slots apply from cache before mount, stay independent and reset without stale overrides", () => {
    // 预置旧语义键：既验证首帧应用正确（→17px/13px），也验证缓存被回写为新格式数字串
    localStorage.setItem("ws_font_size_sans", "large");
    localStorage.setItem("ws_font_size_mono", "small");
    applyInitialFonts();
    const style = document.documentElement.style;
    expect(style.getPropertyValue("--ws-font-size-sans")).toBe("17px");
    expect(style.getPropertyValue("--ws-font-size-mono")).toBe("13px");
    // applyInitialFonts 只应用到 CSS、不回写缓存（回写发生在 storeFonts）；旧键在下次读取时仍被翻译
    expect(localStorage.getItem("ws_font_size_sans")).toBe("large");
    expect(localStorage.getItem("ws_font_size_mono")).toBe("small");
    expect(readStoredFonts().fontSizeSans).toBe("17");
    expect(readStoredFonts().fontSizeMono).toBe("13");
    commitFontSizeSlot("mono", "19");
    expect(style.getPropertyValue("--ws-font-size-sans")).toBe("17px");
    expect(style.getPropertyValue("--ws-font-size-mono")).toBe("19px");
    commitFontSizeSlot("mono", "12");
    expect(style.getPropertyValue("--ws-font-size-mono")).toBe("");
    expect(localStorage.getItem("ws_font_size_mono")).toBeNull();
    // commitFontSizeSlot 走 storeFonts，会把另一槽的缓存也归一为新格式数字串
    expect(localStorage.getItem("ws_font_size_sans")).toBe("17");
  });
});


describe("three independent font slots", () => {
  it("uses 14 / 14 / 12 and keeps content independent when the old config has no content fields", () => {
    expect(readStoredFonts()).toEqual(defaults);
    const { prefs } = reconcileFontsFromConfig({ sans: "Old UI", mono: "Old Code", font_size_sans: 19, font_size_mono: 17 });
    expect(prefs).toEqual({ ...defaults, sans: "Old UI", mono: "Old Code", fontSizeSans: "19", fontSizeMono: "17" });
  });

  it("changes and restores content without changing interface or code", () => {
    commitFontSlot("sans", "UI Face");
    commitFontSlot("mono", "Code Face");
    commitFontSizeSlot("sans", "18");
    commitFontSizeSlot("mono", "16");
    commitFontSlot("content", "Prose Face");
    commitFontSizeSlot("content", "20");
    expect(readStoredFonts()).toEqual({ sans: "UI Face", content: "Prose Face", mono: "Code Face", fontSizeSans: "18", fontSizeContent: "20", fontSizeMono: "16" });
    commitFontSlot("content", "");
    commitFontSizeSlot("content", "14");
    expect(readStoredFonts()).toEqual({ ...defaults, sans: "UI Face", mono: "Code Face", fontSizeSans: "18", fontSizeMono: "16" });
    expect(document.documentElement.style.getPropertyValue("--ws-font-content")).toBe("");
    expect(document.documentElement.style.getPropertyValue("--ws-font-size-content")).toBe("");
  });

  it.each(["constructor", "toString", "__proto__", "NaN", "12.5", "11", "21"])("rejects invalid cached size %s for all slots", (value) => {
    for (const slot of ["sans", "content", "mono"] as const) localStorage.setItem(`ws_font_size_${slot}`, value);
    applyInitialFonts();
    expect(readStoredFonts()).toEqual(defaults);
    expect(sanitizeFontLevel(value, "sans")).toBe("14");
    expect(sanitizeFontLevel(value, "content")).toBe("14");
    expect(sanitizeFontLevel(value, "mono")).toBe("12");
    for (const slot of ["sans", "content", "mono"]) expect(document.documentElement.style.getPropertyValue(`--ws-font-size-${slot}`)).toBe("");
  });
});
