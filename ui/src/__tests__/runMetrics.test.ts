// 本轮（单 run）计数（[docs/composer-token-rate](../../docs/composer-token-rate.md)）：
// runMetrics 的惰性建立 / 累加 / 归零，以及「工具等待只算本轮」「无数据不造桶」「迟到 usage 帧仍累加」三条纪律。
// store 直驱（不经 UI）：帧归一走 applyFrameToTab / applyUsageFrame，工具结果走 onToolResult。
import { describe, it, expect, vi, beforeEach } from "vitest";
import { applyFrameToTab, applyUsageFrame, blank } from "../stores/runFrames";
import { useRun } from "../stores/run";
import { useSessions } from "../stores/sessions";
import { tokPerSec } from "../features/chat/composerMetrics";
import type { ToolResultEvent } from "../ipc/types";

vi.mock("@tauri-apps/api/core", () => ({
  invoke: vi.fn(async () => null),
  Channel: class {
    onmessage: ((f: any) => void) | null = null;
  },
}));

const PREFS = { approval_mode: "auto_edit" as const, model_id: null, reasoning_effort: null };

function seed() {
  useSessions.setState({
    tabs: [{
      key: "s1", sessionId: "s1", workspace: "/tmp/ws", title: "s1",
      projectId: null, createdAt: "2026-09-21T00:00:00Z", prefs: PREFS,
    }],
    activeKey: "s1",
    projects: [],
  });
  useRun.setState((s) => {
    s.tabs = { s1: blank() };
    s.drafts = { s1: { text: "", images: [], refs: [] } };
  });
}

function toolResult(durationMs: number, callKey = "b1:0"): ToolResultEvent {
  return {
    session: "s1", run_id: "r1", batch_id: "b1", call_index: 0, call_key: callKey,
    tool: "read", args_preview: "{}", outcome: { ok: true, data: null }, duration_ms: durationMs,
  };
}

beforeEach(() => {
  useSessions.setState({ tabs: [], activeKey: null, projects: [] });
  useRun.setState((s) => {
    s.tabs = {};
    s.drafts = {};
  });
});

describe("runMetrics 惰性建立与累加", () => {
  it("blank() 不带 runMetrics（缺省即「无数据」，速率段据此隐藏）", () => {
    expect(blank().runMetrics).toBeUndefined();
    expect("runMetrics" in blank()).toBe(false);
  });

  it("usage 帧惰性建桶：只有被计入的帧（duration_ms > 0）才累加 output / 耗时 / 步数", () => {
    const t = blank();
    expect(t.runMetrics).toBeUndefined();
    applyUsageFrame(t, { output: 300, duration_ms: 4000, ttft_ms: 500 });
    applyUsageFrame(t, { output: 200, duration_ms: 6000, ttft_ms: 700 });
    expect(t.runMetrics).toEqual({ output: 500, genMs: 10000, steps: 2, ttftMs: 500, toolMs: 0 });
    expect(tokPerSec(t.runMetrics)).toBeCloseTo(50, 10); // 500 tokens / 10 s
  });

  it("缺字段 / null / 0 / 负的 duration_ms：整帧不计（output 也不进分子），与后端 add_step 同口径", () => {
    const t = blank();
    applyUsageFrame(t, { output: 100 });
    applyUsageFrame(t, { output: 100, duration_ms: null });
    applyUsageFrame(t, { output: 100, duration_ms: 0 });
    applyUsageFrame(t, { output: 100, duration_ms: -5 });
    // 分子分母同域：不计时的帧连 output 都不进分子——否则「带 usage 却不带计时」的帧会虚高速率（AC-17）
    expect(t.runMetrics).toEqual({ output: 0, genMs: 0, steps: 0, ttftMs: null, toolMs: 0 });
    expect(tokPerSec(t.runMetrics)).toBeNull(); // 无耗时 = 无速率 → 显示端整段隐藏
    // 会话级 usage（命中率数据源）仍是「收到就累加」，本计数器的「整帧不计」不影响它
    expect(t.usage!.output).toBe(400);
  });

  it("TTFT 只取首个非空值（不是求和，也不是最后一步的值）；未被计入的帧不参与", () => {
    const t = blank();
    applyUsageFrame(t, { output: 1, duration_ms: 1000 });
    expect(t.runMetrics!.ttftMs).toBeNull();
    applyUsageFrame(t, { output: 1, ttft_ms: 99 }); // 无耗时 → 该步不计，其 TTFT 也不采用
    expect(t.runMetrics!.ttftMs).toBeNull();
    applyUsageFrame(t, { output: 1, duration_ms: 1000, ttft_ms: 250 });
    applyUsageFrame(t, { output: 1, duration_ms: 1000, ttft_ms: 900 });
    expect(t.runMetrics!.ttftMs).toBe(250);
    expect(t.runMetrics).toEqual({ output: 3, genMs: 3000, steps: 3, ttftMs: 250, toolMs: 0 });
  });

  it("各 Tab 独立：一个桶收帧不影响另一个桶（AC-10）", () => {
    const a = blank();
    const b = blank();
    applyUsageFrame(a, { output: 100, duration_ms: 1000 });
    expect(a.runMetrics!.genMs).toBe(1000);
    expect(b.runMetrics).toBeUndefined();
  });

  it("运行收尾（running=false）后迟到的 usage 帧仍累加，但不重建流式项（review C1 守卫不破）", () => {
    const t = blank();
    t.running = false;
    applyFrameToTab(t, {
      type: "usage", input: 0, output: 400, cache_read: 0, cache_write: 0,
      duration_ms: 8000, ttft_ms: 300,
    });
    expect(t.runMetrics).toEqual({ output: 400, genMs: 8000, steps: 1, ttftMs: 300, toolMs: 0 });
    expect(t.items).toEqual([]);
  });
});

describe("runMetrics 归零与工具等待", () => {
  it("send() 归零本轮计数；会话级 usage 不受影响（命中率需要跨 run 累加）", async () => {
    seed();
    useRun.setState((s) => {
      s.tabs.s1.runMetrics = { output: 999, genMs: 1000, steps: 1, ttftMs: 10, toolMs: 20 };
      s.tabs.s1.usage = { input: 100, output: 5, cacheRead: 7, cacheWrite: 0 };
    });
    await useRun.getState().send("第二轮");
    const t = useRun.getState().tabs.s1;
    expect(t.running).toBe(true);
    expect(t.runMetrics).toEqual({ output: 0, genMs: 0, steps: 0, ttftMs: null, toolMs: 0 });
    expect(t.usage).toEqual({ input: 100, output: 5, cacheRead: 7, cacheWrite: 0 });
  });

  it("onToolResult 把本轮工具卡耗时累加进 toolMs，且速率数值不受影响", () => {
    seed();
    useRun.setState((s) => {
      s.tabs.s1.running = true;
      s.tabs.s1.runMetrics = { output: 1820, genMs: 100_000, steps: 1, ttftMs: 400, toolMs: 0 };
    });
    useRun.getState().onToolResult("s1", toolResult(5000), true);
    useRun.getState().onToolResult("s1", toolResult(2500, "b1:1"), true);
    const m = useRun.getState().tabs.s1.runMetrics!;
    expect(m.toolMs).toBe(7500);
    expect(m.genMs).toBe(100_000); // 工具等待不进分母
    expect(m.steps).toBe(1);
    expect(tokPerSec(m)).toBeCloseTo(18.2, 10);
  });

  it("本轮未发过消息（没有计数桶）时，工具结果不会凭空造桶（「无数据」不得变成「数据为 0」）", () => {
    seed();
    useRun.getState().onToolResult("s1", toolResult(5000), true);
    const t = useRun.getState().tabs.s1;
    expect(t.runMetrics).toBeUndefined();
    expect(t.items[0]).toMatchObject({ kind: "assistant" }); // 工具卡本身照常落定
  });

  it("历史（会话恢复）的工具卡不计入本轮：新一轮只累加本轮 duration", async () => {
    seed();
    useRun.getState().restoreFromMessages("s1", [
      { role: "assistant", content: [{ type: "tool_use", id: "old:0", name: "read", args: {} }] },
    ] as any);
    // 恢复路径不造本轮桶（历史卡由 restoreFromMessages 直接构造、不经 onToolResult）
    expect(useRun.getState().tabs.s1.runMetrics).toBeUndefined();
    // 历史卡即使带着耗时留在转录里，也不参与新一轮的「本轮工具等待」
    useRun.setState((s) => {
      s.tabs.s1.items = [{
        kind: "assistant", streaming: false, timeline: [{ kind: "tool", callKey: "old:0" }],
        toolsMap: { "old:0": { callKey: "old:0", tool: "read", status: "ok", durationMs: 9999, progressTail: "" } },
      }];
    });
    await useRun.getState().send("新任务");
    useRun.getState().onToolResult("s1", toolResult(5000, "b2:0"), true);
    const m = useRun.getState().tabs.s1.runMetrics!;
    expect(m.toolMs).toBe(5000); // 不含历史卡的 9999
    expect(tokPerSec(m)).toBeNull(); // 工具耗时单独存在不足以显示速率
  });
});
