// Unit tests for the pure frame reducers ([docs/fence-hardening-and-powershell-ast](../../../docs/fence-hardening-and-powershell-ast.md) refactor): the run.ts reduction pipeline
// (applyFrameToTab / applyFrameToSub / messagesToSubStream) without React or store setup.
import { describe, expect, it } from "vitest";
import {
  applyFrameToSub,
  applyFrameToTab,
  applyToolStart,
  blank,
  closeRunningTools,
  messagesToSubStream,
  settleRunningTools,
  updateToolFromStart,
} from "../stores/runFrames";
import type { Message } from "../ipc/types";
import type { ToolView } from "../stores/run.types";

describe("applyFrameToTab", () => {
  it("appends deltas in arrival order and merges same-kind tails", () => {
    const t = blank();
    t.running = true;
    applyFrameToTab(t, { type: "delta_text", gen: 0, text: "A" } as any);
    applyFrameToTab(t, { type: "delta_text", gen: 0, text: "B" } as any);
    applyFrameToTab(t, { type: "delta_thinking", gen: 0, text: "T" } as any);
    applyFrameToTab(t, { type: "delta_text", gen: 0, text: "C" } as any);
    const last = t.items[t.items.length - 1] as any;
    expect(last.kind).toBe("assistant");
    expect(last.timeline.map((s: any) => [s.kind, s.text])).toEqual([
      ["text", "AB"],
      ["thinking", "T"],
      ["text", "C"],
    ]);
  });

  it("drops late frames after run end (ghost indicator guard, review C1)", () => {
    const t = blank();
    t.running = false;
    applyFrameToTab(t, { type: "delta_text", gen: 0, text: "ghost" } as any);
    expect(t.items).toHaveLength(0);
  });

  it("drops frames below streamGen after retry (review C2) and advances the threshold", () => {
    const t = blank();
    t.running = true;
    applyFrameToTab(t, { type: "delta_text", gen: 3, text: "new" } as any);
    applyFrameToTab(t, { type: "delta_text", gen: 2, text: "old-half" } as any);
    const last = t.items[t.items.length - 1] as any;
    expect(last.timeline).toHaveLength(1);
    expect(last.timeline[0].text).toBe("new");
    expect(t.streamGen).toBe(3);
  });

  it("lands tool_progress on a tool anchor keyed by batch:index", () => {
    const t = blank();
    t.running = true;
    applyFrameToTab(t, { type: "tool_progress", gen: 0, batch: "b1", index: 2, chunk: "..." } as any);
    const last = t.items[t.items.length - 1] as any;
    expect(last.timeline[0]).toEqual({ kind: "tool", callKey: "b1:2" });
    expect(last.toolsMap["b1:2"].progressTail).toBe("...");
  });

  it('「开始」帧（空 chunk + 工具名）立刻建运行中卡片——修「调用结束才显示」', () => {
    const t = blank();
    t.running = true;
    // 后端在工具真正执行前发的帧：chunk 为空、name = 真工具名
    applyFrameToTab(t, { type: "tool_progress", batch: "b9", index: 3, chunk: "", name: "read" } as any);
    const last = t.items[t.items.length - 1] as any;
    expect(last.timeline[0]).toEqual({ kind: "tool", callKey: "b9:3" });
    const tool = last.toolsMap["b9:3"];
    expect(tool).toMatchObject({ tool: "read", status: "running", progressTail: "" });

    // 后续输出帧照常填进度尾部；状态仍为运行中，直到 tool:result 落定
    applyFrameToTab(t, { type: "tool_progress", batch: "b9", index: 3, chunk: "file body", name: "read" } as any);
    expect(tool.progressTail).toBe("file body");
    expect(tool.status).toBe("running");
  });

  it("backfills the running placeholder card with the frame's tool name (running-name fix)", () => {
    const t = blank();
    t.running = true;
    applyFrameToTab(t, { type: "tool_progress", batch: "b1", index: 0, chunk: "out", name: "command" } as any);
    const last = t.items[t.items.length - 1] as any;
    expect(last.toolsMap["b1:0"].tool).toBe("command");
  });

  it('keeps the "?" placeholder for legacy nameless frames (compat fallback)', () => {
    const t = blank();
    t.running = true;
    applyFrameToTab(t, { type: "tool_progress", batch: "b1", index: 0, chunk: "out" } as any);
    const last = t.items[t.items.length - 1] as any;
    expect(last.toolsMap["b1:0"].tool).toBe("?");
  });

  it("never overwrites a real name backfilled by tool:result (late-frame idempotency)", () => {
    const t = blank();
    t.running = true;
    const last = () => t.items[t.items.length - 1] as any;
    applyFrameToTab(t, { type: "tool_progress", batch: "b1", index: 0, chunk: "out", name: "command" } as any);
    last().toolsMap["b1:0"].tool = "command"; // tool:result 已回填真名（onToolResult 同写 tool 字段）
    last().toolsMap["b1:0"].status = "ok";
    // 迟到的 tool_progress 帧：progressTail 照常更新，但名字不被覆盖
    applyFrameToTab(t, { type: "tool_progress", batch: "b1", index: 0, chunk: "late", name: "grep" } as any);
    expect(last().toolsMap["b1:0"].tool).toBe("command");
    expect(last().toolsMap["b1:0"].progressTail).toBe("late");
  });

  it("同 key 重复帧：不新增锚点（timeline 中 tool 段与 toolsMap 键数均不变）", () => {
    const t = blank();
    t.running = true;
    applyFrameToTab(t, { type: "tool_progress", batch: "b1", index: 0, chunk: "a", name: "read" } as any);
    applyFrameToTab(t, { type: "tool_progress", batch: "b1", index: 0, chunk: "b", name: "read" } as any);
    expect(t.items).toHaveLength(1);
    const a = t.items[0] as any;
    expect(a.timeline.filter((s: any) => s.kind === "tool")).toHaveLength(1);
    expect(Object.keys(a.toolsMap)).toEqual(["b1:0"]);
    expect(a.toolsMap["b1:0"].progressTail).toBe("b");
  });

  it("notice 插队后同 key 帧跳回原 assistant 项：不新建项/锚点/卡片", () => {
    const t = blank();
    t.running = true;
    applyFrameToTab(t, { type: "tool_progress", batch: "b1", index: 0, chunk: "first", name: "command" } as any);
    // run:inject / sub:error 会在 items 末尾 push 一条 notice：末项不再是流式项
    t.items.push({ kind: "notice", text: "injected" } as any);
    applyFrameToTab(t, { type: "tool_progress", batch: "b1", index: 0, chunk: "second", name: "command" } as any);
    expect(t.items).toHaveLength(2); // 未另建 assistant 项（修前为 3）
    const a = t.items[0] as any;
    expect(a.timeline.filter((s: any) => s.kind === "tool")).toHaveLength(1);
    expect(Object.keys(a.toolsMap)).toEqual(["b1:0"]);
    expect(a.toolsMap["b1:0"].progressTail).toBe("second");
  });
});

describe("applyFrameToSub", () => {
  it("lazily creates the sub stream and routes envelope-independent frames", () => {
    const t = blank();
    applyFrameToSub(t, "s1", { type: "delta_text", gen: 0, text: "hello" } as any);
    expect(t.subStreams["s1"].timeline[0]).toEqual({ kind: "text", text: "hello" });
    expect(t.subStreams["s1"].status).toBe("running");
  });

  it("drops late frames after the stream is closed out", () => {
    const t = blank();
    t.subStreams["s1"] = { timeline: [], toolsMap: {}, status: "done", gen: 0, loaded: false };
    applyFrameToSub(t, "s1", { type: "delta_text", gen: 0, text: "late" } as any);
    expect(t.subStreams["s1"].timeline).toHaveLength(0);
  });

  it("backfills the sub stream placeholder card with the frame's tool name", () => {
    const t = blank();
    applyFrameToSub(t, "s1", { type: "tool_progress", batch: "b1", index: 0, chunk: "out", name: "command" } as any);
    expect(t.subStreams["s1"].toolsMap["b1:0"].tool).toBe("command");
  });
});

describe("messagesToSubStream", () => {
  it("rebuilds the process stream: text/thinking in order, tool_use anchored with result backfilled", () => {
    const msgs: Message[] = [
      { role: "user", content: [{ type: "text", text: "task" }] } as any,
      { role: "assistant", content: [{ type: "text", text: "doing" }] } as any,
      { role: "assistant", content: [{ type: "tool_use", id: "u1", name: "read", args: { path: "a" } }] } as any,
      { role: "tool", content: [{ type: "tool_result", tool_use_id: "u1", content: '{"ok":1}', is_error: false }] } as any,
    ];
    const { timeline, toolsMap } = messagesToSubStream(msgs);
    expect(timeline.map((s) => s.kind)).toEqual(["text", "tool"]);
    const tool = toolsMap["u1"];
    expect(tool.status).toBe("ok");
    expect(tool.outcome).toEqual({ ok: true, data: { ok: 1 } });
    expect(tool.argsPreview).toBe(JSON.stringify({ path: "a" }));
  });

  it("缺配对结果的 tool_use：默认保持 running（兼容既有行为），settleRunning=true 时落定「已中断」", () => {
    const msgs: Message[] = [
      { role: "assistant", content: [{ type: "tool_use", id: "u2", name: "edit", args: { files: [] } }] } as any,
    ];
    expect(messagesToSubStream(msgs).toolsMap["u2"].status).toBe("running");

    const settled = messagesToSubStream(msgs, true).toolsMap["u2"];
    expect(settled.status).toBe("error");
    expect(settled.outcome).toEqual({ ok: false, data: null, error: { code: "E_INTERRUPTED", message: "" } });
    expect(settled.progressTail).toBe("");
  });
});

describe("tool:start 落卡（applyToolStart）", () => {
  const anchor = () => ({ timeline: [] as any[], toolsMap: {} as any });

  it("waiting 相：建出「等待确认」卡（status=waiting）并带 argsPreview", () => {
    const a = anchor();
    applyToolStart(a, { call_key: "b1:0", tool: "edit", args_preview: '{"files":[]}', phase: "waiting" });
    expect(a.timeline[0]).toEqual({ kind: "tool", callKey: "b1:0" });
    expect(a.toolsMap["b1:0"]).toMatchObject({ tool: "edit", status: "waiting", argsPreview: '{"files":[]}' });
  });

  it("running 相：同一 key 把 waiting 翻成 running（卡不重建）", () => {
    const a = anchor();
    applyToolStart(a, { call_key: "b1:0", tool: "edit", args_preview: "{}", phase: "waiting" });
    applyToolStart(a, { call_key: "b1:0", tool: "edit", args_preview: "", phase: "running" });
    expect(a.timeline).toHaveLength(1);
    expect(a.toolsMap["b1:0"].status).toBe("running");
    expect(a.toolsMap["b1:0"].argsPreview).toBe("{}"); // 已有值不被空值覆盖
  });

  it("只读工具：只收一次 running，直接建出运行中的卡", () => {
    const a = anchor();
    applyToolStart(a, { call_key: "b2:1", tool: "read", args_preview: '{"files":[]}', phase: "running" });
    expect(a.toolsMap["b2:1"]).toMatchObject({ tool: "read", status: "running" });
  });

  it("迟到守卫：已落定（ok/error）的卡不被翻回在途，仅回填空的 argsPreview", () => {
    const a = anchor();
    applyToolStart(a, { call_key: "b3:0", tool: "edit", args_preview: "", phase: "waiting" });
    a.toolsMap["b3:0"].status = "error";
    a.toolsMap["b3:0"].outcome = { ok: false, data: null, error: { code: "E_DENIED", message: "拒绝" } };
    // 审批门被拒 → tool:error 先到，随后才到的 running 相不得把卡翻回在途
    applyToolStart(a, { call_key: "b3:0", tool: "edit", args_preview: '{"files":[1]}', phase: "running" });
    expect(a.toolsMap["b3:0"].status).toBe("error");
    expect(a.toolsMap["b3:0"].outcome.error.code).toBe("E_DENIED");
    expect(a.toolsMap["b3:0"].argsPreview).toBe('{"files":[1]}');
  });
});

describe("工具卡相变迁移（updateToolFromStart）", () => {
  const container = () => ({ timeline: [] as any[], toolsMap: {} as any });
  const card = (over: Partial<ToolView> = {}): ToolView => ({
    callKey: "b1:0", tool: "edit", status: "waiting", progressTail: "", ...over,
  });

  it("waiting → running：原卡原地翻相，不重建卡片", () => {
    const a = container();
    applyToolStart(a, { call_key: "b1:0", tool: "edit", args_preview: '{"files":[]}', phase: "waiting" });
    const before = a.toolsMap["b1:0"];
    updateToolFromStart(before, { call_key: "b1:0", tool: "edit", args_preview: "", phase: "running" });
    expect(a.timeline).toHaveLength(1);
    expect(Object.keys(a.toolsMap)).toEqual(["b1:0"]);
    expect(a.toolsMap["b1:0"]).toBe(before); // 同一对象：未重建卡/未新增锚点
    expect(before.status).toBe("running");
    expect(before.argsPreview).toBe('{"files":[]}'); // 空值不回填覆盖已有预览
  });

  it("已落定（ok/error）的卡不被翻回在途，仅回填空 argsPreview", () => {
    const ok = card({ status: "ok", argsPreview: "" });
    updateToolFromStart(ok, { call_key: "b1:0", tool: "edit", args_preview: '{"files":[1]}', phase: "running" });
    expect(ok.status).toBe("ok");
    expect(ok.argsPreview).toBe('{"files":[1]}'); // 空 → 回填
    // 已有 argsPreview 时后续值不覆盖
    updateToolFromStart(ok, { call_key: "b1:0", tool: "edit", args_preview: '{"files":[2]}', phase: "waiting" });
    expect(ok.argsPreview).toBe('{"files":[1]}');
    expect(ok.status).toBe("ok");

    const err = card({ status: "error", argsPreview: "{}" });
    updateToolFromStart(err, { call_key: "b1:0", tool: "edit", args_preview: "", phase: "running" });
    expect(err.status).toBe("error");
    expect(err.argsPreview).toBe("{}");

    // 落定卡上的占位名一律不回填（真名由 tool:result 负责写入），此处只做行为记录
    const settledPlaceholder = card({ status: "ok", tool: "?" });
    updateToolFromStart(settledPlaceholder, { call_key: "b1:0", tool: "read", args_preview: "", phase: "running" });
    expect(settledPlaceholder.tool).toBe("?");
  });

  it('占位名 "?" 回填真名；已有真名不被覆盖', () => {
    const placeholder = card({ tool: "?", status: "running" });
    updateToolFromStart(placeholder, { call_key: "b1:0", tool: "grep", args_preview: "", phase: "running" });
    expect(placeholder.tool).toBe("grep");
    // waiting 相同样回填（名字回填与 phase 无关）
    const waiting = card({ tool: "?", status: "waiting" });
    updateToolFromStart(waiting, { call_key: "b1:0", tool: "edit", args_preview: "", phase: "waiting" });
    expect(waiting.tool).toBe("edit");
    // 已回填真名（如 tool:result 先到）时不被后续 start 覆盖
    const named = card({ tool: "read", status: "running" });
    updateToolFromStart(named, { call_key: "b1:0", tool: "command", args_preview: "", phase: "running" });
    expect(named.tool).toBe("read");
  });
});

describe("在途工具卡收尾（settleRunningTools / closeRunningTools）", () => {
  it("running 与 waiting 一体落定「已中断」，已落定的卡不动", () => {
    const map: any = {
      a: { callKey: "a", tool: "command", status: "running", progressTail: "tail" },
      b: { callKey: "b", tool: "edit", status: "waiting", progressTail: "" },
      c: { callKey: "c", tool: "read", status: "ok", outcome: { ok: true, data: {} }, progressTail: "" },
    };
    settleRunningTools(map);
    for (const k of ["a", "b"]) {
      expect(map[k].status).toBe("error");
      expect(map[k].outcome).toEqual({ ok: false, data: null, error: { code: "E_INTERRUPTED", message: "" } });
      expect(map[k].progressTail).toBe("");
    }
    expect(map.c.outcome).toEqual({ ok: true, data: {} });
  });

  it("closeRunningTools(subId) 只扫该子流；无 subId 扫全 Tab（含所有 assistant 项与所有子流）", () => {
    const t = blank();
    t.items.push({
      kind: "assistant", timeline: [{ kind: "tool", callKey: "m1" }],
      toolsMap: { m1: { callKey: "m1", tool: "command", status: "running", progressTail: "" } },
      streaming: true,
    } as any);
    t.subStreams.s1 = {
      timeline: [], status: "running", gen: 0, loaded: true,
      toolsMap: { s1: { callKey: "s1", tool: "read", status: "running", progressTail: "" } },
    };
    t.subStreams.s2 = {
      timeline: [], status: "running", gen: 0, loaded: true,
      toolsMap: { s2: { callKey: "s2", tool: "grep", status: "waiting", progressTail: "" } },
    };

    closeRunningTools(t, "s1");
    expect((t.items[0] as any).toolsMap.m1.status).toBe("running"); // 主会话在途卡不受子代理收尾影响
    expect(t.subStreams.s1.toolsMap.s1.status).toBe("error");
    expect(t.subStreams.s2.toolsMap.s2.status).toBe("waiting"); // 兄弟子流不受影响

    closeRunningTools(t);
    expect((t.items[0] as any).toolsMap.m1.status).toBe("error");
    expect(t.subStreams.s2.toolsMap.s2.status).toBe("error");
  });
});
