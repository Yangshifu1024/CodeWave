// 「先看预览」后的 widget 自动弹框信号（[docs/preview-skill](../../../docs/preview-skill.md)）的信号层：
// 置位（armWidgetAutoOpen）/ 锁定（onToolResult 的主会话 render_html 判定）/ 消费（consumeWidgetAutoOpen）/ 收尾清空。
//
// 断言的是 **store 状态**，不碰渲染（UI 消费链由 AskPanel / ToolCallCard 侧的测试覆盖）：
// 本文件钉的是「信号什么时候该在、什么时候不该在」——尤其两条防呆：
//   ① 不合格的工具结果（失败 / 别的工具 / 空 html）**不消费**信号，用户点了预览就还得等下一张；
//   ② 子代理的 render_html 结果**不得**触发弹框（子代理渲染是过程流水，不该打断主视图）。
// 事件喂法照抄 run.text-ask-fallback.test.ts / run.streaming-indicator.test.ts。
import { beforeEach, describe, expect, it, vi } from "vitest";
import { blank } from "../stores/runFrames";
import { useRun } from "../stores/run";
import { useSessions } from "../stores/sessions";
import type { ToolResultEvent } from "../ipc/types";

vi.mock("@tauri-apps/api/core", () => ({
  invoke: vi.fn(async () => null),
  Channel: class {
    onmessage: any = null;
  },
}));

vi.mock("../ipc/client", () => ({
  ipc: {
    gitStatus: vi.fn(async () => []),
  },
}));

const handlers = () => useRun.getState().bindGlobalHandlers();

function seedTab(session: string) {
  useRun.setState((s) => {
    s.tabs[session] = blank();
    s.tabs[session]!.running = true;
  });
  useSessions.setState({ activeKey: session });
}

function signalOf(session: string) {
  return useRun.getState().tabs[session]!.widgetAutoOpen;
}

/** 主会话 render_html 的结果事件（默认带可预览的非空 html） */
function renderResult(over: Partial<ToolResultEvent> = {}): ToolResultEvent {
  return {
    session: "s1",
    run_id: "r1",
    batch_id: "b1",
    call_index: 0,
    call_key: "b1:0",
    tool: "render_html",
    args_preview: "{}",
    outcome: { ok: true, data: { html: "<h1>hi</h1>" } },
    duration_ms: 12,
    ...over,
  } as ToolResultEvent;
}

beforeEach(() => {
  // 焦点态下不走系统通知分支（避免插件动态 import）
  Object.defineProperty(document, "hasFocus", { value: () => true, configurable: true, writable: true });
  useSessions.setState({ tabs: [], activeKey: null, projects: [] });
  useRun.setState((s) => {
    s.tabs = {};
    s.drafts = {};
  });
});

describe("armWidgetAutoOpen / consumeWidgetAutoOpen", () => {
  it("置位为 {armed:true}；重复调用不累积（仍是单份 armed，无旧 callKey）", () => {
    seedTab("s1");
    useRun.getState().armWidgetAutoOpen("s1");
    expect(signalOf("s1")).toEqual({ armed: true });

    useRun.getState().armWidgetAutoOpen("s1");
    expect(signalOf("s1")).toEqual({ armed: true });

    // 已锁定 callKey 后再置位 = 用户又点了一次预览 → 整体替换，旧锁定作废
    useRun.setState((s) => {
      s.tabs.s1!.widgetAutoOpen = { callKey: "b1:0" };
    });
    useRun.getState().armWidgetAutoOpen("s1");
    expect(signalOf("s1")).toEqual({ armed: true });
  });

  it("消费按 callKey 命中：命中才清空，不匹配则保持原样（A 卡的信号不被 B 卡吃掉）", () => {
    seedTab("s1");
    useRun.setState((s) => {
      s.tabs.s1!.widgetAutoOpen = { callKey: "b1:0" };
    });

    useRun.getState().consumeWidgetAutoOpen("s1", "b1:9"); // 不匹配 → 什么都不做
    expect(signalOf("s1")).toEqual({ callKey: "b1:0" });

    useRun.getState().consumeWidgetAutoOpen("s1", "b1:0"); // 命中 → 清空
    expect(signalOf("s1") ?? null).toBeNull();

    // 幂等：重复消费不再报错、也不改其它状态
    useRun.getState().consumeWidgetAutoOpen("s1", "b1:0");
    expect(signalOf("s1") ?? null).toBeNull();
  });

  it("对不存在的 Tab 不新建状态桶（M-1：纯 UI 信号不凭空造状态）", () => {
    seedTab("s1");
    useRun.getState().armWidgetAutoOpen("s-unknown");
    useRun.getState().consumeWidgetAutoOpen("s-unknown", "b1:0");
    expect(useRun.getState().tabs["s-unknown"]).toBeUndefined();
  });
});

describe("onToolResult 里的锁定判定（主会话）", () => {
  beforeEach(() => seedTab("s1"));

  it("已 armed + 主会话 render_html 成功且 html 非空 → callKey 落位、armed 消失（一次性）", () => {
    useRun.getState().armWidgetAutoOpen("s1");
    useRun.getState().onToolResult("s1", renderResult({ call_key: "b1:3" }), true);

    const sig = signalOf("s1")!;
    expect(sig.callKey).toBe("b1:3");
    expect(sig.armed).toBeUndefined();
    // 工具卡照常落定（信号层不干扰既有工具卡链路）
    expect(useRun.getState().tabs.s1!.items[0]).toMatchObject({ kind: "assistant" });
  });

  it("render_html 失败（tool:error）→ 信号仍为 {armed:true}，不消费", () => {
    useRun.getState().armWidgetAutoOpen("s1");
    useRun.getState().onToolResult("s1", renderResult({ outcome: { ok: false, data: null, error: { code: "E_X", message: "boom" } } }), false);
    expect(signalOf("s1")).toEqual({ armed: true });
  });

  it("render_html 成功但 html 为空串 / 缺失 → 信号仍为 {armed:true}，不消费", () => {
    useRun.getState().armWidgetAutoOpen("s1");
    useRun.getState().onToolResult("s1", renderResult({ call_key: "b1:1", outcome: { ok: true, data: { html: "" } } }), true);
    expect(signalOf("s1")).toEqual({ armed: true });

    useRun.getState().onToolResult("s1", renderResult({ call_key: "b1:2", outcome: { ok: true, data: {} } }), true);
    expect(signalOf("s1")).toEqual({ armed: true });

    // 随后一张合格的 widget 仍能把它锁上（信号在整个 run 内一直有效）
    useRun.getState().onToolResult("s1", renderResult({ call_key: "b1:3" }), true);
    expect(signalOf("s1")!.callKey).toBe("b1:3");
  });

  it("别的工具成功（command）→ 信号仍为 {armed:true}，不消费", () => {
    useRun.getState().armWidgetAutoOpen("s1");
    useRun.getState().onToolResult("s1", renderResult({ call_key: "b1:1", tool: "command" }), true);
    expect(signalOf("s1")).toEqual({ armed: true });
  });

  it("未 armed（信号缺省）时收到 render_html 成功结果 → 不产生信号（普通 render_html 不自动弹框）", () => {
    expect(signalOf("s1")).toBeUndefined();
    useRun.getState().onToolResult("s1", renderResult(), true);
    expect(signalOf("s1")).toBeUndefined();
  });

  it("子代理的 render_html 结果：owner Tab 的信号不被置位（子代理渲染不触发弹框）", () => {
    useRun.getState().armWidgetAutoOpen("s1");
    // 子代理流已在位（sub:spawn 的副产物），但 sessionId 传的是 sub_id → 走子代理路由分支
    useRun.setState((s) => {
      s.tabs.s1!.subStreams["sub-1"] = { timeline: [], toolsMap: {}, status: "running", gen: 0, loaded: false };
    });
    useRun.getState().onToolResult("sub-1", renderResult({ session: "sub-1" }), true);

    expect(signalOf("s1")).toEqual({ armed: true }); // 主会话信号纹丝未动
    // 结果确实落进了子代理流（证明走的是子代理分支而不是主会话分支）
    const st = useRun.getState().tabs.s1!.subStreams["sub-1"]!;
    expect(Object.values(st.toolsMap)[0]!.tool).toBe("render_html");
  });

  it("无对应 Tab 也无 subStreams（迟到的未知 session）→ 早退，不动任何已有信号", () => {
    useRun.getState().armWidgetAutoOpen("s1");
    useRun.getState().onToolResult("s-unknown-sub", renderResult({ session: "s-unknown-sub" }), true);
    expect(signalOf("s1")).toEqual({ armed: true });
  });
});

describe("收尾三兄弟清空待弹信号（不跨轮补弹）", () => {
  beforeEach(() => seedTab("s1"));

  it("run:done → 清空（armed 与已锁定的 callKey 都不跨轮）", () => {
    useRun.getState().armWidgetAutoOpen("s1");
    handlers()["run:done"]({ session: "s1", run_id: "r1" });
    expect(signalOf("s1") ?? null).toBeNull();

    seedTab("s2");
    useRun.setState((s) => {
      s.tabs.s2!.widgetAutoOpen = { callKey: "b1:0" };
    });
    handlers()["run:done"]({ session: "s2", run_id: "r2" });
    expect(signalOf("s2") ?? null).toBeNull();
  });

  it("run:error → 清空", () => {
    useRun.getState().armWidgetAutoOpen("s1");
    handlers()["run:error"]({ session: "s1", error: "provider down" });
    expect(signalOf("s1") ?? null).toBeNull();
  });

  it("run:cancelled → 清空", () => {
    useRun.getState().armWidgetAutoOpen("s1");
    handlers()["run:cancelled"]({ session: "s1" });
    expect(signalOf("s1") ?? null).toBeNull();
  });
});