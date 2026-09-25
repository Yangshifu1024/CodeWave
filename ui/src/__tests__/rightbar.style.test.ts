// 辅助信息可见性版式契约（[docs/ui-affordance-visibility-audit](../../../docs/ui-affordance-visibility-audit.md)）：
// happy-dom 不做布局，故与 ask.style.test.ts / chat.style.test.ts 同路——直接断言源码 CSS。
// 这里钉两类不变量：① 辅助信息不许被 auto margin 推到看不见处；② 颜色只走 --ws-* token，不硬编码色值。
import { describe, it, expect } from "vitest";
import { readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

const appCss = readFileSync(join(dirname(fileURLToPath(import.meta.url)), "../theme/app.css"), "utf8");
const aboutSrc = readFileSync(join(dirname(fileURLToPath(import.meta.url)), "../features/panels/AboutSettings.tsx"), "utf8");

/** 取出选择器对应的规则体（[^}] 恰好停在规则右括号；选择器含空格时逐字面匹配） */
function ruleBody(selector: string): string {
  const esc = selector.replace(/[.*+?^${}()|[\]\\]/g, "\\$&");
  const m = new RegExp(`${esc}\\s*\\{([^}]*)\\}`).exec(appCss);
  expect(m, `未找到规则：${selector}`).toBeTruthy();
  return m![1];
}

describe("辅助信息可见性：版式契约", () => {
  it("右栏技能来源紧跟技能名（不再 auto margin 推到行尾），字号与设置页同一档", () => {
    const body = ruleBody(".rb-skill-origin");
    expect(body).not.toMatch(/margin-left:\s*auto/);
    expect(body).toMatch(/font-size:\s*11px/);
    expect(body).toMatch(/color:\s*var\(--ws-dim\)/);
  });

  it("设置·关于的自动更新说明位于共享表单标签，不被行尾边距推走", () => {
    expect(aboutSrc).toContain('label={t("settings.autoUpdateCheckbox")}');
    expect(aboutSrc).not.toContain("settings-update-label");
    expect(appCss).not.toContain(".settings-update-label");
  });

  it("「即时生效」行内标注及关于页标题后缀均不再出现", () => {
    expect(appCss).not.toContain(".settings-instant");
    expect(aboutSrc).not.toContain("instantApplySuffix");
  });

  it("统计摘要：容器可读（--ws-text-2），只有标签降级到 --ws-dim", () => {
    expect(ruleBody(".stats-summary")).toMatch(/color:\s*var\(--ws-text-2\)/);
  });

  it("右栏日志的警告/错误与子代理提前退出原因走 token，不硬编码色值", () => {
    for (const sel of [".rb-log .warn", ".rb-log .err", ".sub-card .sub-card-warn"]) {
      const body = ruleBody(sel);
      expect(body).toMatch(/var\(--ws-(warn|err)\)/);
      expect(body).not.toMatch(/#[0-9a-fA-F]{3,6}/);
    }
  });
});
