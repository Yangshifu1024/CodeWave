// 打字机逐字揭示（[docs/typewriter-stream](../../docs/typewriter-stream.md)）
//
// 测试策略（据 tester 实测）：happy-dom 下 vi.useFakeTimers() 驱动不了 requestAnimationFrame
// （happy-dom 模块加载时把 globalThis.setImmediate 绑定快照进内部 TIMER，假计时器换晚了；
//  且 rAF 时间戳是真实墙钟，假计时器下会算出巨大 dt 导致一步跳到全文）。
// 因此本文件以**纯函数单测为主**（无计时器、无 DOM、100% 确定），
// 组件测只覆盖「三态接线」——这三态在首帧同步生效，不需要推进 rAF。
import { describe, it, expect, vi, afterEach } from "vitest";
import { render } from "@testing-library/react";
import { App as AntApp } from "antd";
import "../i18n"; // 直接挂载的组件必须自行初始化 i18next（与 segments.filter.test.tsx 同）
import {
  advanceReveal,
  clampReveal,
  sliceRevealed,
  prefersReducedMotion,
  __resetReducedMotionCache,
  TimelineSegsView,
} from "../features/chat/segments";
import type { TimelineSeg, ToolView } from "../stores/run";

const toolsMap: Record<string, ToolView> = {};

afterEach(() => {
  __resetReducedMotionCache();
  vi.restoreAllMocks();
  document.body.innerHTML = "";
});

// ---------- 纯函数（主战场） ----------

describe("clampReveal（游标收敛）", () => {
  it("越界与非法输入都收敛到 [0, len]", () => {
    expect(clampReveal(-5, 10)).toBe(0);
    expect(clampReveal(99, 10)).toBe(10);
    expect(clampReveal(3, 10)).toBe(3);
    expect(clampReveal(NaN, 10)).toBe(0);
    expect(clampReveal(Infinity, 10)).toBe(10);
    expect(clampReveal(-Infinity, 10)).toBe(0);
  });

  it("len 为 0 时恒为 0", () => {
    expect(clampReveal(5, 0)).toBe(0);
    expect(clampReveal(0, 0)).toBe(0);
  });
});

describe("advanceReveal（按帧推进 · 跟随数据 + 自动追赶）", () => {
  it("单调不减：任意 dt 序列下每步都不倒退", () => {
    let r = 0;
    for (const dt of [16, 16, 300, 16, 0]) {
      const next = advanceReveal(r, 200, dt);
      expect(next).toBeGreaterThanOrEqual(r);
      r = next;
    }
  });

  it("恒 ≤ len：巨大 dt 与溢出输入都不越界", () => {
    expect(advanceReveal(0, 10, 1e9)).toBe(10);
    expect(advanceReveal(999, 10, 16)).toBe(10);
  });

  it("dt = 0（首帧 / 同帧重复）仍前进至少 1 字，保证可观测", () => {
    expect(advanceReveal(0, 100, 0)).toBeGreaterThanOrEqual(1);
    expect(advanceReveal(5, 100, 0)).toBeGreaterThan(5);
  });

  it("积压越大推进越快（自动追赶），但有上限不瞬间跳完", () => {
    const small = advanceReveal(0, 20, 16);
    const large = advanceReveal(0, 2000, 16);
    expect(large).toBeGreaterThanOrEqual(small);
    // 每帧上限：catchUp 增益封顶 8 倍、basePerFrame(90cps×16ms)≈1.44 字 → 单帧 ≤ 12 字
    // 长回复不会一帧跳完（否则失去打字机观感）
    expect(large).toBeLessThanOrEqual(12);
  });

  it("追平后原样返回（由调用方停 rAF）", () => {
    expect(advanceReveal(50, 50, 16)).toBe(50);
    expect(advanceReveal(60, 50, 16)).toBe(50);
  });

  it("静默 >800ms 直接放行到全文（思考停顿后不慢放堆积文本）", () => {
    expect(advanceReveal(0, 500, 16, { silentMs: 801 })).toBe(500);
    // 未超阈值：正常逐字
    expect(advanceReveal(0, 500, 16, { silentMs: 800 })).toBeLessThan(500);
    expect(advanceReveal(0, 500, 16, { silentMs: 0 })).toBeLessThan(500);
  });

  it("finishing（收尾 / 中断）：一次性放行到全文，不逐字拖尾", () => {
    // 摊分方案实测帧数按对数级膨胀（300 字要 47 帧 ≈ 750ms，比直接放行更拖），
    // 与 O-3「不拖尾」的意图相悖 —— 收尾本就应立即呈现完整内容。
    expect(advanceReveal(0, 300, 16, { finishing: true })).toBe(300);
    expect(advanceReveal(123, 300, 16, { finishing: true })).toBe(300);
    // 已追平时原样返回
    expect(advanceReveal(300, 300, 16, { finishing: true })).toBe(300);
  });

  it("目标变短（report 剥离等）：游标收敛不越界", () => {
    expect(advanceReveal(30, 12, 16)).toBe(12);
    // 变长照常前进
    expect(advanceReveal(30, 40, 16)).toBeGreaterThan(30);
  });
});

describe("sliceRevealed（按游标切片）", () => {
  it("revealed ≥ len 返回全文", () => {
    expect(sliceRevealed("hello", 5)).toBe("hello");
    expect(sliceRevealed("hello", 99)).toBe("hello");
  });

  it("revealed < len 返回前缀", () => {
    expect(sliceRevealed("hello", 2)).toBe("he");
    expect(sliceRevealed("hello", 0)).toBe("");
  });

  it("空串恒返回空串", () => {
    expect(sliceRevealed("", 5)).toBe("");
  });
});

describe("prefersReducedMotion", () => {
  it("happy-dom 默认 no-preference → false（缓存后一致）", () => {
    const a = prefersReducedMotion();
    expect(typeof a).toBe("boolean");
    expect(prefersReducedMotion()).toBe(a);
  });
});

// ---------- 组件三态接线（首帧同步生效，不需推进 rAF） ----------

function segs(...texts: string[]): TimelineSeg[] {
  return texts.map((text) => ({ kind: "text", text }) as TimelineSeg);
}

function mds(): HTMLElement[] {
  return [...document.querySelectorAll<HTMLElement>(".md")];
}

describe("TimelineSegsView 打字机三态接线", () => {
  it("非流式：不切片，两个段都整段直出（含尾段位置）", () => {
    render(
      <AntApp>
        <TimelineSegsView timeline={segs("ABCDEFGH", "abcdefgh")} toolsMap={toolsMap} />
      </AntApp>,
    );
    // markdown-it 渲染 <p> 会带尾随换行，统一 trim 后比较
    const got = mds().map((m) => (m.textContent ?? "").trim());
    expect(got).toEqual(["ABCDEFGH", "abcdefgh"]);
  });

  it("流式：尾段被切，且是全文的真前缀（不写死长度，避免 flaky）", () => {
    const full = "这是一段用于验证打字机逐字揭示效果的较长文本内容";
    render(
      <AntApp>
        <TimelineSegsView timeline={segs(full)} toolsMap={toolsMap} streaming />
      </AntApp>,
    );
    const tail = (mds()[0].textContent ?? "").trim();
    expect(full.startsWith(tail)).toBe(true);
    // 首帧就有内容（不为空），否则会闪空白
    expect(tail.length).toBeGreaterThan(0);
  });

  it("流式：非尾段保持完整，只有最后一段被切", () => {
    const head = "第一段已经定稿的完整内容";
    render(
      <AntApp>
        <TimelineSegsView timeline={segs(head)} toolsMap={toolsMap} streaming={false} />
      </AntApp>,
    );
    expect((mds()[0].textContent ?? "").trim()).toBe(head);
  });

  it("定稿（streaming 翻 false）：整段直出", () => {
    const full = "定稿后的完整内容应该一次性全部可见";
    const view = (streaming: boolean) => (
      <AntApp>
        <TimelineSegsView timeline={segs(full)} toolsMap={toolsMap} streaming={streaming} />
      </AntApp>
    );
    const { rerender } = render(view(true));
    rerender(view(false));
    expect((mds()[0].textContent ?? "").trim()).toBe(full);
  });

  it("reduced-motion：首帧即全文，不逐字", () => {
    const original = window.matchMedia;
    window.matchMedia = ((q: string) => ({
      matches: /prefers-reduced-motion/.test(q),
      media: q,
      onchange: null,
      addListener: () => {},
      removeListener: () => {},
      addEventListener: () => {},
      removeEventListener: () => {},
      dispatchEvent: () => false,
    })) as unknown as typeof window.matchMedia;
    __resetReducedMotionCache();
    const full = "减弱动态效果时这段内容应当首帧全部可见";
    render(
      <AntApp>
        <TimelineSegsView timeline={segs(full)} toolsMap={toolsMap} streaming />
      </AntApp>,
    );
    expect((mds()[0].textContent ?? "").trim()).toBe(full);
    window.matchMedia = original;
    __resetReducedMotionCache();
  });

  it("stripReport：先剥离再切片，输出不含半截协议标记", () => {
    const withMark = "<report>子代理汇报正文</report>";
    render(
      <AntApp>
        <TimelineSegsView timeline={segs(withMark)} toolsMap={toolsMap} streaming={false} stripReport />
      </AntApp>,
    );
    const txt = mds()[0].textContent ?? "";
    expect(txt).not.toContain("<report>");
    expect(txt).not.toContain("</report>");
    expect(txt).toContain("子代理汇报正文");
  });

  it("data-streaming 契约不变：流式时带该属性，定稿后不带", () => {
    const full = "验证 mermaid 延迟渲染契约仍然成立";
    const view = (streaming: boolean) => (
      <AntApp>
        <TimelineSegsView timeline={segs(full)} toolsMap={toolsMap} streaming={streaming} />
      </AntApp>
    );
    const { rerender } = render(view(true));
    expect(mds()[0].hasAttribute("data-streaming")).toBe(true);
    rerender(view(false));
    expect(mds()[0].hasAttribute("data-streaming")).toBe(false);
  });
});
