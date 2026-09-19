// Unit tests for the pure frame reducers ([docs/fence-hardening-and-powershell-ast](../../../docs/fence-hardening-and-powershell-ast.md) refactor): the run.ts reduction pipeline
// (applyFrameToTab / applyFrameToSub / messagesToSubStream) without React or store setup.
import { describe, expect, it } from "vitest";
import {
  applyFrameToSub,
  applyFrameToTab,
  blank,
  messagesToSubStream,
} from "../stores/runFrames";
import type { Message } from "../ipc/types";

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

  it("drops late frames after run end (ghost cursor guard, review C1)", () => {
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
});
