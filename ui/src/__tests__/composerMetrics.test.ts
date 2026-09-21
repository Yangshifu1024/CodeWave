// 速率与耗时的展示口径（[docs/composer-token-rate](../../docs/composer-token-rate.md)）：
// 工具条速率段与统计面板总览三项共用同一份纯函数——位数自适应、工具等待与速率解耦、
// 「无数据」一律 null（绝不返回 0 / NaN / Infinity，显示端据此隐藏）。
import { describe, it, expect } from "vitest";
import { avgStepMs, formatInt, formatMs, formatRate, tokPerSec, type RateInput } from "../features/chat/composerMetrics";

const base: RateInput = { output: 1820, genMs: 100_000, steps: 3, ttftMs: 420, toolMs: 5000 };

describe("formatRate：<10 两位 / 10–99 一位 / ≥100 取整 / 0<c<0.01 → <0.01", () => {
  it.each([
    [9.994, "9.99"],
    [9.995, "9.99"], // toFixed 的二进制舍入：9.995 实际略小于 9.995
    [0.004, "<0.01"],
    [0, "0.00"],
    [9.5, "9.50"],
    [10, "10.0"],
    [99.94, "99.9"],
    [100.4, "100"],
    [1234.6, "1235"],
  ])("%s → %s", (n, want) => {
    expect(formatRate(n)).toBe(want);
  });

  it("负值与非有限值不产出文本（调用方本就不该走到这里）", () => {
    expect(formatRate(-3)).toBe("");
    for (const n of [NaN, Infinity, -Infinity]) expect(formatRate(n)).toBe("");
  });
});

describe("tokPerSec：Σoutput ÷ (Σ生成耗时 / 1000)", () => {
  it("1820 tokens / 100 s → 18.2 tok/s", () => {
    expect(tokPerSec(base)).toBeCloseTo(18.2, 10);
  });

  it("工具耗时只是等待：换 toolMs 数值结果不变", () => {
    for (const toolMs of [0, 5000, 999_999]) {
      expect(tokPerSec({ ...base, toolMs })).toBeCloseTo(18.2, 10);
    }
  });

  it("缺字段 / 分母 0 / 分母负 / 无输出 / 非有限 → null（不是 0，也不出 NaN）", () => {
    expect(tokPerSec(undefined)).toBeNull();
    expect(tokPerSec(null)).toBeNull();
    expect(tokPerSec({ output: 100, genMs: 0, steps: 0 })).toBeNull();
    expect(tokPerSec({ output: 100, genMs: -1, steps: 1 })).toBeNull();
    expect(tokPerSec({ output: 0, genMs: 1000, steps: 1 })).toBeNull();
    expect(tokPerSec({ output: NaN, genMs: 1000, steps: 1 })).toBeNull();
    expect(tokPerSec({ output: 100, genMs: Infinity, steps: 1 })).toBeNull();
  });
});

describe("avgStepMs：Σ生成耗时 ÷ 步数", () => {
  it("100 s / 3 步", () => {
    expect(avgStepMs(base)).toBeCloseTo(100_000 / 3, 10);
  });

  it("步数 0 / 负 / 缺字段 / 无耗时 → null", () => {
    expect(avgStepMs({ output: 100, genMs: 10_000, steps: 0 })).toBeNull();
    expect(avgStepMs({ output: 100, genMs: 10_000, steps: -2 })).toBeNull();
    expect(avgStepMs({ output: 100, genMs: 0, steps: 2 })).toBeNull();
    expect(avgStepMs(undefined)).toBeNull();
  });
});

describe("formatMs：<1s 整毫秒 / ≥1s 一位小数秒", () => {
  it.each([
    [0, "0 ms"],
    [420, "420 ms"],
    [999, "999 ms"],
    [1000, "1.0 s"],
    [2500, "2.5 s"],
    [100_000, "100.0 s"],
  ])("%s → %s", (ms, want) => {
    expect(formatMs(ms)).toBe(want);
  });

  it("null / undefined / 负 / 非有限 → 空串", () => {
    expect(formatMs(null)).toBe("");
    expect(formatMs(undefined)).toBe("");
    expect(formatMs(-1)).toBe("");
    expect(formatMs(NaN)).toBe("");
  });
});

describe("formatInt：M/k 缩写（口径与统计面板既有 fmt() 一致）", () => {
  it.each([
    [0, "0"],
    [999, "999"],
    [1000, "1.0k"],
    [1820, "1.8k"],
    [1_000_000, "1.0M"],
    [1_234_567, "1.2M"],
  ])("%s → %s", (n, want) => {
    expect(formatInt(n)).toBe(want);
  });
});
