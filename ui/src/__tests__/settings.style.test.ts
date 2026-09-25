// Shared settings layout contract ([docs/settings-ui-unification](../../../docs/settings-ui-unification.md)).
// Vitest does not load CSS, so assert the single shared source and its wiring directly.
import { describe, it, expect } from "vitest";
import { readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

const here = dirname(fileURLToPath(import.meta.url));
const theme = readFileSync(join(here, "../features/panels/settings/SettingsTheme.tsx"), "utf8");
const css = readFileSync(join(here, "../features/panels/settings/settings-theme.css"), "utf8");
const appCss = readFileSync(join(here, "../theme/app.css"), "utf8");

describe("设置页共享视觉契约（settings-ui-unification）", () => {
  it("所有面板共享一个 antd 主题层与左右式 Form", () => {
    expect(theme).toMatch(/export function SettingsThemeProvider/);
    expect(theme).toMatch(/layout="horizontal"/);
    expect(theme).toMatch(/colon=\{false\}/);
    expect(theme).toMatch(/componentSize="medium"/);
    expect(css).toMatch(/\.settings-theme-root \.settings-form \.settings-row/);
  });

  it("设置表单使用紧凑行距，必填星号与标签同一行", () => {
    expect(theme).toMatch(/rowPaddingBlock:\s*10/);
    expect(css).toMatch(/label\.ant-form-item-required\s*\{[^}]*display:\s*flex/);
    expect(css).toMatch(/label\.ant-form-item-required::before\s*\{[^}]*flex:\s*none/);
  });

  it("模型编辑器用双列网格并在窄弹框下自动改单列", () => {
    expect(css).toMatch(/\.settings-provider-model-form\s*\{[^}]*grid-template-columns:\s*repeat\(2, minmax\(0, 1fr\)\)/);
    expect(css).toMatch(/\.settings-provider-model-field-wide\s*\{\s*grid-column:\s*1 \/ -1/);
    expect(css).toMatch(/@container settings-content \(max-width: 680px\)[\s\S]*?\.settings-provider-model-form\s*\{\s*grid-template-columns:\s*minmax\(0, 1fr\)/);
  });

  it("主题预览铺满卡片，字体输入与各自预览占同一半宽设置行", () => {
    expect(css).toMatch(/\.settings-theme-root \.settings-theme-picker\s*\{\s*width:\s*100%/);
    expect(css).toMatch(/\.settings-row\.settings-row-full \.ant-form-item-control\s*\{[^}]*flex:\s*0 0 100%/);
    expect(css).toMatch(/\.settings-row\.settings-font-row \.ant-form-item-control\s*\{[^}]*flex:\s*0 0 50%/);
    expect(css).toMatch(/\.settings-row\.settings-font-row \.ant-form-item-label\s*\{[^}]*margin-inline-end:\s*var\(--settings-row-gap\)/);
    expect(css).toMatch(/\.settings-theme-root \.settings-font-control\s*\{[^}]*flex-direction:\s*column/);
  });

  it("内容宽度、行卡片、控制宽度和窄屏堆叠由公共常量与样式管理", () => {
    expect(theme).toMatch(/contentWidth:\s*960/);
    expect(theme).toMatch(/controlWidth:\s*\{\s*narrow:\s*180,\s*mid:\s*240,\s*wide:\s*360/);
    expect(css).toMatch(/\.settings-section-card\s*\{/);
    expect(css).toMatch(/@container settings-content \(max-width: 680px\)/);
    expect(css).toMatch(/\.ant-form-item-row\s*\{\s*display:\s*flex;\s*flex-direction:\s*column/);
  });

  it("MCP 的键值编辑项仍保持标签在上方", () => {
    expect(appCss).toMatch(/\.mcp-entry-row\s*\{[^}]*flex-direction:\s*column/);
    expect(appCss).not.toMatch(/\.mcp-entry-row \.mcp-label\s*\{[^}]*width:/);
  });
});
