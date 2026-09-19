// Custom font preferences ([docs/custom-font-and-titlebar](../../../docs/custom-font-and-titlebar.md)): sanitize / override building / localStorage round-trip / <html> application
import { describe, it, expect, beforeEach } from "vitest";
import {
  applyInitialFonts, buildFontOverride, previewFontFamily, readStoredFonts, reconcileFontsFromConfig,
  sanitizeFontList, storeFonts,
} from "../utils/fonts";

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
    const saved = storeFonts({ sans: ' Map"le ', mono: "  " });
    expect(saved).toEqual({ sans: "Maple", mono: "" });
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
    storeFonts({ sans: "Inter", mono: "JetBrains Mono" });
    storeFonts({ sans: "", mono: "" });
    expect(readStoredFonts()).toEqual({ sans: "", mono: "" });
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
    const r = reconcileFontsFromConfig({ sans: "PingFang SC", mono: "JetBrains Mono" });
    expect(r).toEqual({ prefs: { sans: "PingFang SC", mono: "JetBrains Mono" }, migrate: false });
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
    expect(r.prefs).toEqual({ sans: "", mono: "Consolas" });
    expect(document.documentElement.style.getPropertyValue("--ws-font-mono")).toBe(
      '"Consolas", var(--ws-font-mono-fallback)',
    );
  });

  it("两侧都空 → 默认链且不迁移（顺带清掉脏缓存键）", () => {
    localStorage.setItem("ws_font_sans", ' "" ; } ');
    const r = reconcileFontsFromConfig({ sans: "", mono: "" });
    expect(r).toEqual({ prefs: { sans: "", mono: "" }, migrate: false });
    expect(localStorage.getItem("ws_font_sans")).toBeNull();
    expect(document.documentElement.style.getPropertyValue("--ws-font-sans")).toBe("");
  });
});
