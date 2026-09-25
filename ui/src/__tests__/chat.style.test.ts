// Chat area scrollbar style contract ([docs/chat-scrollbar-overlay]):
// native 滚动条全部隐藏（Windows「始终显示滚动条」系统设置下 native widget 会画带箭头的滚动条，CSS 改不掉——见 app.css 对应章节），
// 改用 ChatScrollbar React 组件（仿 MiniMax Code / Slack / Discord overlay 范式）按 scroll 位置渲染细 thumb，滚动时显、停 1s 后隐。
// Asserts the raw source via node fs (same approach as titlebar.style.test.ts).
import { describe, it, expect } from "vitest";
import { readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

const appCss = readFileSync(join(dirname(fileURLToPath(import.meta.url)), "../theme/app.css"), "utf8");

describe("聊天区滚动条契约（docs/chat-scrollbar-overlay：自定义 overlay 替代 native）", () => {
  it(".chat-messages native 滚动条全隐藏（避免系统「始终显示滚动条」画 native widget 带箭头）", () => {
    // Firefox scrollbar-width 关闭
    expect(appCss).toMatch(/\.chat-messages\s*\{[^}]*scrollbar-width:\s*none/);
    // webkit 滚动条尺寸归零
    expect(appCss).toMatch(/\.chat-messages::-webkit-scrollbar\s*\{[^}]*width:\s*0/);
    expect(appCss).toMatch(/\.chat-messages::-webkit-scrollbar\s*\{[^}]*height:\s*0/);
    // webkit thumb/track 一并隐藏
    expect(appCss).toMatch(/\.chat-messages::-webkit-scrollbar-(track|thumb)\s*\{[^}]*display:\s*none/);
  });

  it(".chat-scrollbar 自定义 overlay：绝对定位贴右边缘、opacity 0→1 过渡（0.25s ease 贴合 macOS overlay）", () => {
    expect(appCss).toMatch(/\.chat-scrollbar\s*\{[^}]*position:\s*absolute/);
    expect(appCss).toMatch(/\.chat-scrollbar\s*\{[^}]*right:\s*4px/);
    expect(appCss).toMatch(/\.chat-scrollbar\s*\{[^}]*opacity:\s*0/);
    expect(appCss).toMatch(/\.chat-scrollbar\s*\{[^}]*transition:\s*opacity 0\.25s ease/);
    // .visible 切到 opacity 1
    expect(appCss).toMatch(/\.chat-scrollbar\.visible\s*\{[^}]*opacity:\s*1/);
  });

  it(".chat-scrollbar-thumb：细条 + 圆角 + hover 加深 + 加宽", () => {
    expect(appCss).toMatch(/\.chat-scrollbar-thumb\s*\{[^}]*border-radius:\s*3px/);
    expect(appCss).toMatch(/\.chat-scrollbar-thumb\s*\{[^}]*background:\s*var\(--ws-border\)/);
    expect(appCss).toMatch(/\.chat-scrollbar-thumb:hover\s*\{[^}]*background:\s*var\(--ws-dim\)/);
    expect(appCss).toMatch(/\.chat-scrollbar-thumb:hover\s*\{[^}]*width:\s*8px/);
  });
});

describe("用户消息悬停操作样式契约（docs/titlebar-content-batch 追加）", () => {
  it(".user-msg-actions 默认隐藏（visibility 兜底防透明误点），悬停消息行浮现", () => {
    expect(appCss).toMatch(/\.user-msg-actions\s*\{[^}]*visibility:\s*hidden[^}]*opacity:\s*0/);
    expect(appCss).toMatch(/\.msg\.user:hover \.user-msg-actions\s*\{[^}]*visibility:\s*visible[^}]*opacity:\s*1/);
  });
});