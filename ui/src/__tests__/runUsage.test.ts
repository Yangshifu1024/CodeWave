// usage 帧累加与命中率派生（本轮新增：工具条「命中 NN%」的数据源）。
// 此前 usage 帧在 applyFrameToTab 里被直接丢弃（「usage 帧不进转录」），命中率无从计算。
import { describe, it, expect } from "vitest";
import { applyFrameToTab, applyUsageFrame, blank, cacheHitRate, contextTier, hitRateTier } from "../stores/runFrames";
import type { UsageTotals } from "../stores/run.types";

const usage = (over: Partial<UsageTotals>): UsageTotals => ({
  input: 0, output: 0, cacheRead: 0, cacheWrite: 0, ...over,
});

describe("usage 帧累加", () => {
  it("逐帧累加四字段；Tab 初值为零（帧字段为 snake_case，与 ipc/types.ts 的 Frame 契约一致）", () => {
    const t = blank();
    expect(t.usage).toEqual({ input: 0, output: 0, cacheRead: 0, cacheWrite: 0 });
    applyUsageFrame(t, { input: 100, output: 20, cache_read: 700, cache_write: 30 });
    applyUsageFrame(t, { input: 50, output: 10, cache_read: 300, cache_write: 0 });
    expect(t.usage).toEqual({ input: 150, output: 30, cacheRead: 1000, cacheWrite: 30 });
  });

  it("字段缺失按 0 计（不产生 NaN）", () => {
    const t = blank();
    applyUsageFrame(t, { input: 10 });
    applyUsageFrame(t, undefined);
    expect(t.usage).toEqual({ input: 10, output: 0, cacheRead: 0, cacheWrite: 0 });
  });

  it("applyFrameToTab 消费 usage 帧：不进 timeline，且 running=false（收尾后迟到帧）也累加", () => {
    const t = blank();
    t.running = true;
    applyFrameToTab(t, { type: "usage", input: 100, output: 5, cache_read: 700, cache_write: 0 });
    expect(t.usage!.cacheRead).toBe(700);
    expect(t.items).toHaveLength(0); // 不进转录
    // 收尾：run 已结束，最后一条 usage 帧仍须累加（否则命中率漏掉最新一轮）
    t.running = false;
    applyFrameToTab(t, { type: "usage", input: 100, output: 5, cache_read: 300, cache_write: 0 });
    expect(t.usage).toEqual({ input: 200, output: 10, cacheRead: 1000, cacheWrite: 0 });
    expect(t.items).toHaveLength(0);
  });
});

describe("cacheHitRate 边界与协议口径", () => {
  it("无数据 → null（不显示「命中」段）", () => {
    expect(cacheHitRate(blank(), "openai")).toBeNull();
    expect(cacheHitRate(undefined, "anthropic")).toBeNull();
  });

  it("有数据但零命中 → 0（与「无数据」区分）", () => {
    expect(cacheHitRate({ usage: usage({ input: 1000 }) }, "openai")).toBe(0);
  });

  it("openai 系：input 已含 cached_tokens，分母 = input", () => {
    // 700 命中 / 300 未命中 = 70%（旧统一公式会算成 41%，即系统性偏低）
    expect(cacheHitRate({ usage: usage({ input: 1000, cacheRead: 700 }) }, "openai")).toBeCloseTo(0.7, 10);
    expect(cacheHitRate({ usage: usage({ input: 1000, cacheRead: 1000 }) }, "openai")).toBe(1);
    // openai 的 cache_write 不进分母（该协议无写入计费）
    expect(cacheHitRate({ usage: usage({ input: 1000, cacheRead: 500, cacheWrite: 9999 }) }, "openai"))
      .toBeCloseTo(0.5, 10);
  });

  it("anthropic 系：input 不含缓存，分母 = input + cache_read + cache_write", () => {
    // 同一组数字在 anthropic 语义下分母多一个 cache_write
    expect(cacheHitRate({ usage: usage({ input: 200, cacheRead: 700, cacheWrite: 100 }) }, "anthropic"))
      .toBeCloseTo(0.7, 10);
    expect(cacheHitRate({ usage: usage({ input: 200, cacheRead: 700, cacheWrite: 100 }) }, "openai"))
      .toBeCloseTo(700 / 200, 10); // 同一数据两种口径不同值——证明分母确实按语义取
    expect(cacheHitRate({ usage: usage({ cacheRead: 1000 }) }, "anthropic")).toBe(1);
  });
});

describe("命中率四档（≥99% ok / ≥95% yellow / ≥90% warn / 其余 danger）", () => {
  it("边界矩阵", () => {
    expect(hitRateTier(1)).toBe("ok");
    expect(hitRateTier(0.99)).toBe("ok");
    expect(hitRateTier(0.9899)).toBe("yellow");
    expect(hitRateTier(0.95)).toBe("yellow");
    expect(hitRateTier(0.9499)).toBe("warn");
    expect(hitRateTier(0.9)).toBe("warn");
    expect(hitRateTier(0.8999)).toBe("danger");
    expect(hitRateTier(0)).toBe("danger");
  });
});

describe("上下文占用三档（相对自动压缩阈值）", () => {
  it("阈值 0.6：0.42 / 0.6 为界", () => {
    expect(contextTier(0.4199, 0.6)).toBe("low");
    expect(contextTier(0.42, 0.6)).toBe("medium"); // 阈值 × 0.7
    expect(contextTier(0.5999, 0.6)).toBe("medium");
    expect(contextTier(0.6, 0.6)).toBe("high"); // 达阈值
    expect(contextTier(0.8, 0.6)).toBe("high");
    expect(contextTier(0, 0.6)).toBe("low");
  });

  it("阈值非法（缺省 / 0 / 越界 / NaN）→ 一律 low（不臆测风险）", () => {
    for (const bad of [0, -0.1, 1.5, NaN, Infinity]) {
      expect(contextTier(0.95, bad)).toBe("low");
    }
  });
});
