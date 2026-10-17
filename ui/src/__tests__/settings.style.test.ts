// Shared settings layout contract ([docs/settings-guide](../../../docs/settings-guide.md)).
// Vitest does not load CSS, so assert the single shared source and its wiring directly.
import { describe, it, expect } from "vitest";
import { readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

const here = dirname(fileURLToPath(import.meta.url));
const theme = readFileSync(join(here, "../features/panels/settings/SettingsTheme.tsx"), "utf8");
const css = readFileSync(join(here, "../features/panels/settings/settings-theme.css"), "utf8");
const fontSettings = readFileSync(join(here, "../features/panels/FontSettings.tsx"), "utf8");

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

  it("主题预览铺满卡片，字体行左列为定宽窄列、行内双控件占满右侧", () => {
    expect(css).toMatch(/\.settings-theme-root \.settings-theme-picker\s*\{\s*width:\s*100%/);
    expect(css).toMatch(/\.settings-row\.settings-row-full \.ant-form-item-control\s*\{[^}]*flex:\s*0 0 100%/);
    // 三槽合并成三行后左列只剩「界面 / 内容 / 代码」两字标签：定宽窄列，右侧控件吃掉余量
    expect(css).toMatch(/\.settings-row\.settings-font-row \.ant-form-item-label\s*\{[^}]*flex:\s*0 0 var\(--settings-font-slot-width\)/);
    expect(css).toMatch(/\.settings-row\.settings-font-row \.ant-form-item-control\s*\{[^}]*flex:\s*1 1 auto/);
    expect(css).toMatch(/\.settings-row\.settings-font-row \.ant-form-item-label\s*\{[^}]*align-items:\s*center/);
    // 三个尺寸都进 SETTINGS_VISUAL，页体不写字面量
    expect(theme).toMatch(/fontSlotLabelWidth:\s*72/);
    expect(theme).toMatch(/fontSizeSelectWidth:\s*88/);
    expect(theme).toMatch(/fontResetWidth:\s*32/);
    expect(css).toMatch(/\.settings-theme-root \.settings-font-control\s*\{[^}]*flex-direction:\s*column/);
  });

  it("字体行三个控件等高居中，行末重置按钮独立成列", () => {
    // Select 不能靠 size="small" 缩高（会比同行 Input 矮一截、下边线对不齐），
    // 重置也不能再藏在 suffixIcon 里——两者都已移到行末的整槽重置按钮
    const sizeSelect = fontSettings.match(/<Select<FontSizeLevel>[\s\S]*?\/>/)?.[0] ?? "";
    expect(sizeSelect).not.toBe("");
    expect(sizeSelect).not.toMatch(/size="small"/);
    expect(sizeSelect).not.toMatch(/suffixIcon/);
    expect(css).toMatch(/\.settings-font-pair\s*\{[^}]*grid-template-columns:[^}]*var\(--settings-font-size-select-width\)[^}]*var\(--settings-font-reset-width\)/);
    expect(css).toMatch(/\.settings-font-pair\s*\{[^}]*align-items:\s*center/);
    expect(css).toMatch(/\.settings-theme-root \.settings-font-reset\s*\{[^}]*align-items:\s*center/);
  });

  it("内容宽度、行卡片、控制宽度和窄屏堆叠由公共常量与样式管理", () => {
    expect(theme).toMatch(/contentWidth:\s*960/);
    expect(theme).toMatch(/controlWidth:\s*\{\s*narrow:\s*180,\s*mid:\s*240,\s*wide:\s*360/);
    expect(css).toMatch(/\.settings-section-card\s*\{/);
    expect(css).toMatch(/@container settings-content \(max-width: 680px\)/);
    expect(css).toMatch(/\.ant-form-item-row\s*\{\s*display:\s*flex;\s*flex-direction:\s*column/);
  });

  it("MCP 编辑弹框字段标签在上方，窄屏键值行自动堆叠", () => {
    expect(css).toMatch(/\.mcp-editor-field,\s*\.settings-theme-root \.mcp-entry-row\s*\{[^}]*flex-direction:\s*column/);
    expect(css).toMatch(/@container settings-content \(max-width: 540px\)[\s\S]*?\.mcp-kv-table \.mcp-table-row\s*\{[^}]*grid-template-columns:\s*minmax\(0, 1fr\) 34px/);
  });
});
