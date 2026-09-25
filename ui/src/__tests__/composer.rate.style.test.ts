// 速率段样式契约（[docs/composer-token-rate](../../docs/composer-token-rate.md)）：
// 上下文/命中挪到进度圈 hover 的 Popover 后，.toolbar-info 只剩速率段；
// 速率保持中性次级灰、不做分档着色；「在跑」点必须尊重 prefers-reduced-motion。
// happy-dom 不做布局 → 断言源码 CSS（同 ask.style.test.ts）。
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

describe("Composer 速率段样式契约", () => {
  it("用主题 token 的中性灰，不新增硬编码色值", () => {
    const body = ruleBody(".composer-toolbar .toolbar-info .ctx-rate");
    expect(body).toMatch(/color:\s*var\(--ws-dim\)/);
    expect(body).not.toMatch(/#[0-9a-fA-F]{3,6}/);
  });

  it("不做分档着色（色彩强度只映射风险等级）", () => {
    expect(appCss).not.toMatch(/\.ctx-rate\.(warn|danger|ok|yellow|medium|high)/);
  });

  it("「在跑」点是极淡跳动点，且 prefers-reduced-motion 下静止", () => {
    const body = ruleBody(".composer-toolbar .toolbar-info .rate-dot");
    expect(body).toMatch(/animation:/);
    expect(body).toMatch(/opacity:\s*0?\.\d+/);
    // 动效只是心跳提示：减弱动效偏好下必须停在静止形态（信息不依赖动效）
    expect(appCss).toMatch(
      /@media \(prefers-reduced-motion: reduce\)[\s\S]{0,200}?\.composer-toolbar \.toolbar-info \.rate-dot\s*\{\s*animation:\s*none/,
    );
  });
});
