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

  it("描述与标签同行时自动占满剩余宽度，列宽不足则整段落下一行", () => {
    expect(ruleBody(".ask-opt")).toMatch(/flex-wrap:\s*wrap/);
    expect(ruleBody(".ask-opt .desc")).toMatch(/flex:\s*1 1 240px/);
    expect(ruleBody(".ask-opt .desc")).toMatch(/min-width:\s*0/);
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
