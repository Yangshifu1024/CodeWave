// 代理模式卡片样式契约（[docs/proxy-mode-title-invisible](../../docs/proxy-mode-title-invisible.md)）：
// antd 6 在 `.ant-radio-group` 上设了 `font-size: 0`（消除 inline-block 之间的空白间隙），代理卡片整棵
// 子树都挂在该容器内 —— 只要 `.proxy-mode-list` 不再还原字号，未显式声明 font-size 的
// `.proxy-mode-title` 就会继承成 0 号字：DOM 里有文本（因此 `cardByTitle` 之类 DOM 断言全绿），
// 但真实渲染里完全看不见。vitest 不加载 CSS（css:false），故用 node fs 直读源码做契约断言
// （与 titlebar.style.test.ts 同法）。
import { describe, it, expect } from "vitest";
import { readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

const here = dirname(fileURLToPath(import.meta.url));
const appCss = readFileSync(join(here, "../theme/app.css"), "utf8");
const bridge = readFileSync(join(here, "../theme/bridge.tsx"), "utf8");

describe("代理模式卡片样式契约", () => {
  it(".proxy-mode-list 必须还原字号（否则 .ant-radio-group 的 font-size:0 会让标题隐形）", () => {
    expect(appCss).toMatch(/\.proxy-mode-list\s*\{[^}]*font-size:\s*var\(--ws-font-size\)/);
  });

  it("ThemeBridge 必须桥接 --ws-font-size（取 antd token.fontSize，不硬编码像素）", () => {
    expect(bridge).toContain('s.setProperty("--ws-font-size"');
    expect(bridge).toContain("token.fontSize");
  });
});
