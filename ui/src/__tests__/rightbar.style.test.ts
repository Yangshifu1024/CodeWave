// 辅助信息可见性版式契约（[docs/chat-and-interface](../../../docs/chat-and-interface.md)）：
// happy-dom 不做布局，故与 ask.style.test.ts / chat.style.test.ts 同路——直接断言源码 CSS。
// Keep long names shrinkable and source labels readable; colors follow theme tokens.
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
  it("右栏技能名可收缩，来源拥有固定可读槽位并保留溢出省略", () => {
    const body = ruleBody(".rb-skill-origin");
    expect(ruleBody(".rb-skill-name")).toMatch(/flex:\s*1;\s*min-width:\s*0/);
    expect(body).toMatch(/width:\s*\d+px/);
    expect(body).toMatch(/text-align:\s*right/);
    expect(body).toContain("text-overflow: ellipsis");
    expect(body).toContain("font-size: calc(11px + var(--ws-font-size-sans-offset))");
    expect(body).toMatch(/color:\s*var\(--ws-text-2\)/);
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
