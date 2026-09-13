// Settings form vertical style contract ([docs/settings-forms-vertical](../../../docs/settings-forms-vertical.md)): the antd Form part is directly guaranteed by layout="vertical",
// here we guard that the custom .mcp-entry-row does not regress to the horizontal "fixed-width left label" form.
// vitest does not load CSS (css:false); same approach as titlebar.style.test.ts: read the source with node fs and assert.
import { describe, it, expect } from "vitest";
import { readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

const appCss = readFileSync(join(dirname(fileURLToPath(import.meta.url)), "../theme/app.css"), "utf8");

describe("设置表单 vertical 样式契约（docs/settings-forms-vertical）", () => {
  it(".mcp-entry-row：纵向排列（label 在上），label 不再固定左宽", () => {
    expect(appCss).toMatch(/\.mcp-entry-row\s*\{[^}]*display:\s*flex[^}]*flex-direction:\s*column/);
    expect(appCss).not.toMatch(/\.mcp-entry-row \.mcp-label\s*\{[^}]*width:/);
  });
});
