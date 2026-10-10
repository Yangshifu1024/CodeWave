// P4.8 扩 css 契约组：hljs token var / 用户气泡 md 重置 / table-wrap / 标题 / 链接 / blockquote / hr。
// 与 markdown.user.test.ts 配套：css + 渲染器双侧守门。
import { describe, it, expect } from "vitest";
import { readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

const appCss = readFileSync(join(dirname(fileURLToPath(import.meta.url)), "../theme/app.css"), "utf8");

describe("markdown 主题感（docs/markdown-style-refresh）", () => {
  it("hljs token 全部用 --ws-hl-* 主题感变量，取代硬编码 #hex", () => {
    // 共享组内任一 token 出现必须 var(--ws-hl-*)
    const tokenVars = ["--ws-hl-keyword", "--ws-hl-string", "--ws-hl-number", "--ws-hl-name", "--ws-hl-built", "--ws-hl-comment"];
    for (const v of tokenVars) {
      expect(appCss).toContain(`var(${v})`);
    }
  });

  it("共享组包含 .sub-drawer-stream .md（子代理过程流也吃这套契约）", () => {
    expect(appCss).toMatch(/:is\(\.assistant \.md, \.sub-drawer-stream \.md, \.plan-body\.md, \.updater-notes-body\.md\)/);
  });

  it("表格 th 底色 + 行 hover + .table-wrap 横向滚动", () => {
    expect(appCss).toMatch(/:is\([^*]+?\)\s+\.table-wrap\s*\{\s*overflow-x:\s*auto/);
    expect(appCss).toMatch(/:is\([^*]+?\)\s+th\s*\{\s*background:\s*var\(--ws-bg-nav\)/);
    expect(appCss).toMatch(/:is\([^*]+?\)\s+tr:hover\s+td\s*\{\s*background:\s*var\(--ws-bg-nav\)/);
  });

  it("链接走 var(--ws-accent) + 下划线 + 偏移", () => {
    expect(appCss).toMatch(/:is\([^*]+?\)\s+a\s*\{[^}]*color:\s*var\(--ws-accent\)/);
    expect(appCss).toMatch(/text-underline-offset:\s*3px/);
  });

  it("标题梯度 h1-h6 + 行高 + 颜色 token", () => {
    expect(appCss).toMatch(/:is\([^*]+?\)\s+:is\(h1,\s*h2,\s*h3,\s*h4,\s*h5,\s*h6\)\s*\{[^}]*line-height:\s*1\.4/);
    expect(appCss).toMatch(/:is\([^*]+?\)\s+h1\s*\{[^}]*font-size:\s*18\.5px/);
    expect(appCss).toMatch(/:is\([^*]+?\)\s+h3\s*\{[^}]*font-size:\s*15px/);
  });

  it("段落 line-height 1.7（[docs/markdown-style-refresh] 阅读感升级）", () => {
    expect(appCss).toMatch(/:is\([^*]+?\)\s+p\s*\{[^}]*line-height:\s*1\.7/);
  });

  it("blockquote 左侧 accent 条 + dim 文字", () => {
    expect(appCss).toMatch(/:is\([^*]+?\)\s+blockquote\s*\{[^}]*border-left:\s*3px solid var\(--ws-accent\)/);
    expect(appCss).toMatch(/:is\([^*]+?\)\s+blockquote\s*\{[^}]*color:\s*var\(--ws-text-2\)/);
  });

  it("hr 1px 主题感分隔线", () => {
    expect(appCss).toMatch(/:is\([^*]+?\)\s+hr\s*\{[^}]*border-top:\s*1px solid var\(--ws-border\)/);
  });

  it("del / mark 修饰元素有规则", () => {
    expect(appCss).toMatch(/:is\([^*]+?\)\s+del\s*\{[^}]*line-through/);
    expect(appCss).toMatch(/:is\([^*]+?\)\s+mark\s*\{[^}]*color-mix/);
  });

  it(".user-bubble.md 重置 white-space 与 p margin，避免 markdown 双换行", () => {
    expect(appCss).toMatch(/\.user-bubble\.md\s*\{\s*white-space:\s*normal/);
    expect(appCss).toMatch(/\.user-bubble\.md\s*>\s*p\s*\{[^}]*line-height:\s*1\.6/);
    expect(appCss).toMatch(/\.user-bubble\.md\s*>\s*p:first-child\s*\{\s*margin-top:\s*0/);
    expect(appCss).toMatch(/\.user-bubble\.md\s*>\s*p:last-child\s*\{\s*margin-bottom:\s*0/);
  });

  it(".user-bubble.md 含基础 markdown 元素（h1-h3 / ul / blockquote / a / strong / em / code）", () => {
    expect(appCss).toMatch(/\.user-bubble\.md\s*:is\(h1,\s*h2,\s*h3,\s*h4,\s*h5,\s*h6\)\s*\{[^}]*font-weight:\s*600/);
    expect(appCss).toMatch(/\.user-bubble\.md\s*blockquote\s*\{[^}]*border-left:\s*3px solid var\(--ws-accent\)/);
    expect(appCss).toMatch(/\.user-bubble\.md\s*:not\(pre\)\s*>\s*code\s*\{[^}]*background:\s*var\(--ws-bg-nav\)/);
    expect(appCss).toMatch(/\.user-bubble\.md\s*a\s*\{[^}]*color:\s*var\(--ws-accent\)/);
  });

  it("bridge.tsx 输出 --ws-hl-* 与 --ws-code-block-* token", () => {
    const bridgeTsx = readFileSync(
      join(dirname(fileURLToPath(import.meta.url)), "../theme/bridge.tsx"),
      "utf8",
    );
    for (const v of [
      "--ws-hl-keyword",
      "--ws-hl-string",
      "--ws-hl-number",
      "--ws-hl-name",
      "--ws-hl-built",
      "--ws-hl-comment",
      "--ws-hl-attr",
      "--ws-code-block-bg",
      "--ws-code-block-border",
      "--ws-code-block-text",
    ]) {
      expect(bridgeTsx).toContain(v);
    }
  });
});
