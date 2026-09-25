// Titlebar style contract ([docs/custom-font-and-titlebar](../../../docs/custom-font-and-titlebar.md) review R1): .titlebar-drag-zone is absolute inset:0,
// it needs a positioned ancestor (.toolbar position:relative); otherwise the containing block falls back to the initial containing block = the whole viewport,
// and the transparent drag layer would cover the middle and lower areas. vitest does not load CSS (css:false — even ?raw returns an empty string),
// so node fs is used to read the source directly for style contract assertions.
import { describe, it, expect } from "vitest";
import { readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

const appCss = readFileSync(join(dirname(fileURLToPath(import.meta.url)), "../theme/app.css"), "utf8");
const nativeCss = readFileSync(join(dirname(fileURLToPath(import.meta.url)), "../theme/native.css"), "utf8");
const tauriConfig = JSON.parse(readFileSync(join(dirname(fileURLToPath(import.meta.url)), "../../../src-tauri/tauri.conf.json"), "utf8"));

describe("自绘标题栏样式契约", () => {
  it("插件控制按钮的样式来源获 CSP 放行，层级高于设置覆盖层", () => {
    expect(tauriConfig.app.security.csp).toContain("style-src-elem 'self' 'unsafe-inline' tauri-plugin-decoration:");
    expect(nativeCss).toContain("--tauri-plugin-decoration-z-index: 25;");
  });
  it(".toolbar 必须定位（relative）——拖拽层的包含块", () => {
    expect(appCss).toMatch(/\.toolbar\s*\{[^}]*position:\s*relative/);
  });

  it(".toolbar 必须显式 padding:0——压掉 antd 6 Header 默认 0 50px（docs/titlebar-content-batch 审查 🔴1）", () => {
    expect(appCss).toMatch(/\.toolbar\s*\{[^}]*padding:\s*0\s*;/);
  });

  it(".titlebar-drag-zone 绝对定位铺满父级且垫底", () => {
    expect(appCss).toMatch(/\.titlebar-drag-zone\s*\{[^}]*position:\s*absolute[^}]*inset:\s*0[^}]*z-index:\s*0/);
  });

  it("内容层浮于拖拽层之上（.toolbar > * z-index:1）", () => {
    expect(appCss).toMatch(/\.toolbar > \*\s*\{[^}]*z-index:\s*1/);
  });

  it("native 回退与 macOS 分支规则存在且顺序在基础规则之后（后写胜出，docs/titlebar-content-batch 迁移到段容器）", () => {
    // Two-segment titlebar ([docs/titlebar-content-batch](../../../docs/titlebar-content-batch.md)): macOS traffic-light clearance / native fallback rules target .tb-left-seg / .tb-main-seg
    const nativeLeftAt = appCss.indexOf('html[data-titlebar-mode="native"] .tb-left-seg');
    const macosLeftAt = appCss.indexOf('html[data-os="macos"] .tb-left-seg');
    const baseLeftAt = appCss.indexOf(".tb-left-seg {");
    expect(nativeLeftAt).toBeGreaterThan(macosLeftAt); // native fallback overrides the macos 78px clearance
    expect(macosLeftAt).toBeGreaterThan(baseLeftAt);
    // Windows control strip clearance lives on the right segment; native fallback has an override rule too
    expect(appCss).toMatch(/\.tb-main-seg\s*\{[^}]*padding-right:\s*max\(/);
    expect(appCss).toContain('html[data-titlebar-mode="native"] .tb-main-seg');
  });

  it("两段背景契约：左段带过渡与右缘分隔线，右段弹性占满且不外溢；折叠态装饰归零与让位类（docs/sidebar-collapse-animation-and-titlebar-blend）", () => {
    expect(appCss).toMatch(/\.tb-left-seg\s*\{[^}]*transition:\s*width\s+0\.2s/);
    expect(appCss).toMatch(/\.tb-left-seg\s*\{[^}]*border-right:\s*1px\s+solid\s+var\(--ws-border\)/);
    expect(appCss).toMatch(/\.tb-main-seg\s*\{[^}]*flex:\s*1[^}]*min-width:\s*0/);
    // [docs/sidebar-collapse-animation-and-titlebar-blend](../../../docs/sidebar-collapse-animation-and-titlebar-blend.md): collapsed decor rules exist for every platform/native branch (review 🟡1: the platform rules carry a
    // higher specificity than .tb-left-closed alone, so each branch needs its own zeroing override)
    expect(appCss).toMatch(/\.tb-left-seg\.tb-left-closed\s*\{[^}]*padding-left:\s*0[^}]*border-right-color:\s*transparent[^}]*background:\s*var\(--ws-bg-main\)/);
    expect(appCss).toMatch(/html\[data-os="macos"\] \.tb-left-seg\.tb-left-closed\s*\{[^}]*padding-left:\s*0[^}]*min-width:\s*0/);
    expect(appCss).toMatch(/html\[data-titlebar-mode="native"\] \.tb-left-seg\.tb-left-closed\s*\{[^}]*padding-left:\s*0/);
    expect(appCss).toMatch(/\.tb-main-seg\s*\{[^}]*transition:\s*padding\s+0\.2s/);
    // [docs/sidebar-collapse-animation-and-titlebar-blend](../../../docs/sidebar-collapse-animation-and-titlebar-blend.md) traffic-light yield migration: collapsed right segment pads 78px on macOS, native fallback reverts to 12px
    expect(appCss).toMatch(/html\[data-os="macos"\] \.tb-main-seg\.tb-main-cleared\s*\{[^}]*padding-left:\s*max\(78px/);
    expect(appCss).toMatch(/html\[data-titlebar-mode="native"\] \.tb-main-seg\.tb-main-cleared\s*\{[^}]*padding-left:\s*12px/);
  });

  it("Logo 开合入口（docs/titlebar-logo-toggle）：悬停 Logo 淡出、箭头覆盖浮现（同位交换）", () => {
    expect(appCss).toMatch(/\.tb-logo-toggle\s*\{[^}]*position:\s*relative[^}]*cursor:\s*pointer/);
    expect(appCss).toMatch(/\.tb-logo-toggle:hover \.tb-logo\s*\{[^}]*opacity:\s*0/);
    expect(appCss).toMatch(/\.tb-logo-toggle:hover \.tb-logo-swap\s*\{[^}]*opacity:\s*1/);
  });
});
