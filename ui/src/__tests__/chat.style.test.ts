// Chat area scrollbar style contract ([docs/workspace-explorer-removal-and-chat-scrollbar](../../../docs/workspace-explorer-removal-and-chat-scrollbar.md)): transparent track matching the content background + theme-token thumb,
// replacing the jarring default WebView scrollbar. Asserts the raw source via node fs (same approach as titlebar.style.test.ts).
import { describe, it, expect } from "vitest";
import { readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

const appCss = readFileSync(join(dirname(fileURLToPath(import.meta.url)), "../theme/app.css"), "utf8");

describe("聊天区滚动条样式契约（docs/workspace-explorer-removal-and-chat-scrollbar）", () => {
  it(".chat-messages 轨道透明：滚动条背景与内容区一致", () => {
    expect(appCss).toMatch(/\.chat-messages::-webkit-scrollbar-track\s*\{[^}]*background:\s*transparent/);
  });

  it("thumb 走主题 token 且圆角，hover 加深", () => {
    expect(appCss).toMatch(
      /\.chat-messages::-webkit-scrollbar-thumb\s*\{[^}]*background:\s*var\(--ws-border\)[^}]*border-radius/,
    );
    expect(appCss).toMatch(/\.chat-messages::-webkit-scrollbar-thumb:hover\s*\{[^}]*background:\s*var\(--ws-dim\)/);
  });

  it("Firefox 语法兜底存在（scrollbar-width thin + 透明轨道）", () => {
    expect(appCss).toMatch(/\.chat-messages\s*\{[^}]*scrollbar-width:\s*thin[^}]*scrollbar-color:\s*var\(--ws-border\) transparent/);
  });
});

describe("用户消息悬停操作样式契约（docs/titlebar-content-batch 追加）", () => {
  it(".user-msg-actions 默认隐藏（visibility 兜底防透明误点），悬停消息行浮现", () => {
    expect(appCss).toMatch(/\.user-msg-actions\s*\{[^}]*visibility:\s*hidden[^}]*opacity:\s*0/);
    expect(appCss).toMatch(/\.msg\.user:hover \.user-msg-actions\s*\{[^}]*visibility:\s*visible[^}]*opacity:\s*1/);
  });
});
