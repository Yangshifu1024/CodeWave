// 「子代理设置区与上方容器间距」样式契约（plan fix/settings-section-provider-gap）：
// 回归保护 — providers section 不能把 line 170 默认的 32px section-gap 抹零。
// tester 静态分析已确认双重抹零（line 427 抹零 section 自身 + line 196 :last-child
// 抹零其内 section-card）；本测试钉死修复后的契约：section 自身无 margin-bottom: 0 抹零
// + section-card 显式声明 32px，避免任一处再被改回去而无人发现。
//
// 走 CSS 源码断言（happy-dom 不做布局），与 settings.style.test.ts 风格一致。
import { describe, it, expect } from "vitest";
import { readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

const css = readFileSync(
  join(dirname(fileURLToPath(import.meta.url)), "../features/panels/settings/settings-theme.css"),
  "utf8",
);

/** 取出选择器对应的规则体（[^}] 恰好停在规则右括号；选择器含空格时逐字面匹配） */
function ruleBody(selector: string): string {
  const esc = selector.replace(/[.*+?^${}()|[\]\\]/g, "\\$&");
  const m = new RegExp(`${esc}\\s*\\{([^}]*)\\}`).exec(css);
  expect(m, `未找到规则：${selector}`).toBeTruthy();
  return m![1];
}

describe("providers section 间距契约（避免「Subagent Models 标题顶到上方容器底部」回归）", () => {
  it("line 170 仍提供 32px section-gap（默认 margin-bottom）", () => {
    const body = ruleBody(".settings-theme-root .settings-section");
    expect(body).toMatch(/margin-bottom:\s*var\(--settings-section-gap\)/);
  });

  it(".settings-provider-section 不再被单独抹零 margin-bottom", () => {
    // 修复前 line 427 把 .settings-provider-section 强制 margin-bottom: 0，导致
    // SubagentModels section 标题紧贴 provider 卡片底边。修复后这条选择器不应再带
    // margin-bottom: 0。
    const providerRule = new RegExp(
      "\\.settings-theme-root\\s+\\.settings-provider-section\\s*\\{[^}]*\\}",
    ).exec(css);
    if (providerRule) {
      expect(providerRule[0], "providers section 不应再单独定义 margin-bottom: 0").not.toMatch(
        /margin-bottom:\s*0/,
      );
    }
    // 选择器不存在（被合并到下方 > 规则）也算合规，但若存在则不允许抹零。
  });

  it(".settings-provider-section > .settings-section-card 显式保留 32px，覆盖 :last-child 抹零", () => {
    const body = ruleBody(".settings-theme-root .settings-provider-section > .settings-section-card");
    // 必须显式声明 margin-bottom 为 section-gap（默认 32px），否则会被
    // line 196 .settings-section-card:last-child { margin-bottom: 0 } 覆盖，
    // 再次触发 0 间距。
    expect(body).toMatch(/margin-bottom:\s*var\(--settings-section-gap\)/);
    // 视觉外壳（providers section 内层不需要卡片边框）继续由这条规则清零。
    expect(body).toMatch(/padding:\s*0/);
    expect(body).toMatch(/border:\s*0/);
    expect(body).toMatch(/background:\s*transparent/);
  });
});
