// Sidebar collapsed style contract (finalized in [docs/sidebar-toggle-buttons](../../../docs/sidebar-toggle-buttons.md); [docs/titlebar-logo-toggle](../../../docs/titlebar-logo-toggle.md) removed the toggle button row and consolidated the toggle entry to the titlebar Logo):
// guard the narrow-rail width/border from being dropped (would lose separation from the main area); vitest does not load CSS (css:false),
// same approach as titlebar.style.test.ts: read the source with node fs and assert.
import { describe, it, expect } from "vitest";
import { readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

const appCss = readFileSync(join(dirname(fileURLToPath(import.meta.url)), "../theme/app.css"), "utf8");

describe("侧栏折叠样式契约（docs/sidebar-toggle-buttons）", () => {
  it(".sider-rail 窄轨退役（docs/sidebar-collapse-animation-and-titlebar-blend）：折叠 0 宽完全隐藏后，窄轨布局样式必须整体移除", () => {
    expect(appCss).not.toMatch(/\.sider-rail\s*\{/);
  });

  it(".right-bar：0.2s 过渡 + 折叠裁切收缩（docs/sidebar-collapse-animation-and-titlebar-blend：隐藏不再卸载，常驻挂载后宽度/内边距收零 + visibility 延迟隐藏）", () => {
    expect(appCss).toMatch(/\.right-bar\s*\{[^}]*transition:\s*all 0\.2s/);
    expect(appCss).toMatch(/\.right-bar\s*\{[^}]*overflow-x:\s*hidden/);
    // [docs/sidebar-collapse-animation-and-titlebar-blend](../../../docs/sidebar-collapse-animation-and-titlebar-blend.md) collapsed clip: width/padding collapse to 0, border melts, visibility hides after the animation; no narrow-rail substitute
    expect(appCss).toMatch(/\.right-bar\.right-bar-closed\s*\{[^}]*width:\s*0[^}]*visibility:\s*hidden/);
    expect(appCss).not.toMatch(/\.right-rail\s*\{/);
  });

  it(".sider-nav 占满侧栏体：flex:1 + min-height:0（顶部按钮行已随 docs/titlebar-logo-toggle 移除，导航从 0 起占满）", () => {
    expect(appCss).toMatch(/\.sider-nav\s*\{[^}]*flex:\s*1;[^}]*min-height:\s*0/);
    expect(appCss).toMatch(/\.sider-body\s*\{[^}]*display:\s*flex[^}]*flex-direction:\s*column/);
  });

  it(".sider-nav 横向裁剪：overflow-x hidden，任何内容超宽不出横向滚动条", () => {
    expect(appCss).toMatch(/\.sider-nav\s*\{[^}]*overflow-x:\s*hidden/);
  });

  it(".session-title 可收缩：flex:1 + min-width:0 + ellipsis，空间不足时截断标题而非挤出滚动条", () => {
    expect(appCss).toMatch(/\.session-nav-row \.session-title\s*\{[^}]*flex:\s*1[^}]*min-width:\s*0[^}]*text-overflow:\s*ellipsis/);
    expect(appCss).not.toMatch(/\.session-nav-row \.session-title\s*\{[^}]*flex:\s*none/);
  });

  it(".session-unread-dot：12px 运行槽位内的圆形 accent 点（docs/ask-ink-accent-and-composer-cover 未读点，不影响标题对齐）", () => {
    expect(appCss).toMatch(/\.session-nav-row \.session-unread-dot\s*\{[^}]*border-radius:\s*50%[^}]*background:\s*var\(--ws-accent\)/);
  });

  it(".rb-tabs 根高度约束：flex:1 + min-height:0（缺失则 Tabs 根被内容撑高、body-holder 永不滚动——10f11fc 回归教训，docs/rightbar-visual-batch）+ 动画期内容 300px 下限（docs/sidebar-collapse-animation-and-titlebar-blend）", () => {
    expect(appCss).toMatch(/\.rb-tabs\s*\{[^}]*flex:\s*1[^}]*min-height:\s*0/);
    expect(appCss).toMatch(/\.rb-tabs\s*\{[^}]*min-width:\s*300px/);
  });

  it(".right-bar 背景：var(--ws-bg-main)（docs/rightbar-visual-batch 侧栏分色，亮色 #f8f8f8 / 暗色回自动）", () => {
    expect(appCss).toMatch(/\.right-bar\s*\{[^}]*background:\s*var\(--ws-bg-main\)/);
  });
});
