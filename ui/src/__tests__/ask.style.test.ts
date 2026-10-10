// AskPanel 选项描述排版契约（[docs/ask-option-desc-wrap](../../../docs/ask-option-desc-wrap.md)）：
// 描述必须是**完整折行**显示——用户反馈「选项说明只显示半句，看不到选它会怎样」。
// happy-dom 不做布局，故与 chat.style.test.ts / titlebar.style.test.ts 同路：直接断言源码 CSS。
import { describe, it, expect } from "vitest";
import { readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

const appCss = readFileSync(join(dirname(fileURLToPath(import.meta.url)), "../theme/app.css"), "utf8");

/** 取出选择器对应的规则体（[^}] 恰好停在规则右括号；选择器含空格时逐字面匹配） */
function ruleBody(selector: string): string {
  const esc = selector.replace(/[.*+?^${}()|[\]\\]/g, "\\$&");
  const m = new RegExp(`${esc}\\s*\\{([^}]*)\\}`).exec(appCss);
  expect(m, `未找到规则：${selector}`).toBeTruthy();
  return m![1];
}

describe("AskPanel 选项描述排版契约", () => {
  it("描述允许折行：不再有 nowrap / ellipsis，且长串可任意断行", () => {
    const body = ruleBody(".ask-opt .desc");
    expect(body).not.toMatch(/white-space:\s*nowrap/);
    expect(body).not.toMatch(/text-overflow:\s*ellipsis/);
    expect(body).not.toMatch(/overflow:\s*hidden/);
    expect(body).toMatch(/white-space:\s*normal/);
    expect(body).toMatch(/overflow-wrap:\s*anywhere/);
  });

  it("描述独占下一行：由 .opt-body 列容器承载换行，且左边界与标题对齐", () => {
    // 缩进对齐（第二行对齐到标签起点）纯 CSS 做不到，必须靠 JSX 加一层列容器——
    // 故这里断言容器存在且为列向，而不是断言某个 flex 基准宽度。
    const body = ruleBody(".ask-opt .opt-body");
    expect(body).toMatch(/flex:\s*1/);
    expect(body).toMatch(/flex-direction:\s*column/);
    expect(body).toMatch(/min-width:\s*0/);
    // 标题行：标签与推荐 chip 同行、基线对齐；flex-wrap 保留在**这一层**（超长标题自身仍可折行）
    const head = ruleBody(".ask-opt .opt-head");
    expect(head).toMatch(/display:\s*flex/);
    expect(head).toMatch(/align-items:\s*baseline/);
    expect(head).toMatch(/gap:\s*8px/);
    // 描述已是列容器内的块级项：不得再有同行 flex 基准宽度（旧实现是 flex: 1 1 240px）
    expect(ruleBody(".ask-opt .desc")).not.toMatch(/flex:\s*1 1/);
    expect(ruleBody(".ask-opt .desc")).toMatch(/min-width:\s*0/);
    // 换行职责已从 .ask-opt 移交列容器：行本身不再需要 wrap
    expect(ruleBody(".ask-opt")).not.toMatch(/flex-wrap:\s*wrap/);
    expect(ruleBody(".ask-opt")).toMatch(/align-items:\s*flex-start/);
  });

  it("多行下指示物对齐标题首行（不随描述变长而漂移）", () => {
    expect(ruleBody(".ask-opt .opt-box")).toMatch(/align-self:\s*flex-start/);
    // 首行行盒高度显式声明，不依赖上游继承
    expect(ruleBody(".ask-opt")).toMatch(/line-height:\s*18px/);
  });

  it("关注中的选项行显示左侧墨条（inset 阴影，不占布局宽度）", () => {
    // 分三条写：ruleBody 是逐字面正则匹配，逗号合并选择器取不到规则体
    for (const sel of [".ask-opt:hover", ".ask-opt.kb", ".ask-opt.picked"]) {
      expect(ruleBody(sel)).toMatch(/box-shadow:\s*inset 2px 0 0 var\(--ws-accent\)/);
    }
  });

  it("推荐 chip 走 accent 同色系半透明底（暗色下不翻成亮块），且无硬编码色值", () => {
    const body = ruleBody(".ask-opt .rec-pill");
    // 不能用 accent 实心底：--ws-accent 是 antd colorPrimaryText 派生的**前景色**、暗色下为提亮灰，
    // 当底会翻成亮块。半透明底 + accent 字属同色系明度分层，两主题都可读。
    expect(body).toMatch(/background:\s*color-mix\(in srgb, var\(--ws-accent\) 16%, transparent\)/);
    expect(body).toMatch(/color:\s*var\(--ws-accent\)/);
    expect(body).not.toMatch(/#[0-9a-fA-F]{3,6}/);
    expect(body).not.toMatch(/border:\s*1px/);
  });

  it("长标签不再把描述挤到看不见（标签可收缩并可断行）", () => {
    expect(ruleBody(".ask-opt .label")).toMatch(/flex:\s*0 1 auto/);
    expect(ruleBody(".ask-opt .label")).toMatch(/overflow-wrap:\s*anywhere/);
  });

  it("描述走主题 token 与次级字号（不新增硬编码颜色）", () => {
    const body = ruleBody(".ask-opt .desc");
    expect(body).toMatch(/color:\s*var\(--ws-dim\)/);
    expect(body).not.toMatch(/#[0-9a-fA-F]{3,6}/);
  });
});
