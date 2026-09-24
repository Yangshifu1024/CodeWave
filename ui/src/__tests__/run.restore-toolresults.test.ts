// 会话恢复保真（[docs/session-restore-fidelity](../../../docs/session-restore-fidelity.md)）：
// ① 历史里那份模型侧文本被截断时，按 provider 侧 tool_use id 从后端 sidecar 批量回填完整出参；
// ② 「无结果 / 被中断」的调用按 E_INTERRUPTED 呈现（不再谎报「已使用」）；
// ③ 子代理卡（report 被父历史截断）与子代理过程抽屉内的工具卡走同一条回填通路。
import { beforeEach, describe, expect, it, vi } from "vitest";

const calls: { cmd: string; args: any }[] = [];
/** call_id → 后端要返回的记录（测试用例按需填充） */
const sidecar: Record<string, any> = {};

/** sub_id → 子代理过程历史（抽屉用例按需填充；缺省空数组 = 后端无落盘） */
const subHistory: Record<string, any[]> = {};

vi.mock("@tauri-apps/api/core", () => ({
  invoke: vi.fn(async (cmd: string, args?: any) => {
    calls.push({ cmd, args });
    if (cmd === "load_tool_outcomes") {
      return (args?.callIds ?? []).map((id: string) => sidecar[id]).filter(Boolean);
    }
    if (cmd === "load_subagent_history") return subHistory[args?.subId] ?? [];
    return null;
  }),
  Channel: class {
    onmessage: ((f: any) => void) | null = null;
  },
}));

import { lossyToolKeys, useRun } from "../stores/run";

const session = "s-restore";

/** 头尾截断后的真实形态：中间插了截断提示、JSON 结构被破坏（JSON.parse 必然失败）。 */
const TRUNCATED = '{"title":"Demo","html":"<div>\n…[已截断 40000 字节]…\n","chars":40000';

function seed() {
  useRun.setState((s) => {
    s.tabs[session] = {
      items: [], running: false, streamGen: 0, ask: null, breakdown: null, todos: [], suggestions: [],
      subs: [], subStreams: {}, subDrawer: { open: false, subId: null },
      gitEntries: null, writeTick: 0, queue: [], pendingItemId: null, draftFromQueue: null,
      lastDoneRunId: null, compacting: false,
    };
  });
}

function card(key: string): any {
  const item = useRun.getState().tabs[session]!.items.find((i) => i.kind === "assistant") as any;
  return item?.toolsMap?.[key];
}

function tab(): any {
  return useRun.getState().tabs[session];
}

/** 子代理过程流（immer 每次 set 都会换对象，用取值函数而非缓存引用） */
function subStream(id: string): any {
  return useRun.getState().tabs[session]!.subStreams[id];
}

/** 一条 assistant(tool_use) + 一条 tool(结果) 的历史（结果文本按用例给定，null = 结果缺失）。 */
function history(tool: string, result: string | null, isError = false) {
  const assistant = {
    role: "assistant",
    content: [{ type: "tool_use", id: "call-1", name: tool, args: { title: "Demo" } }],
  };
  const msgs: any[] = [assistant];
  if (result !== null) {
    msgs.push({
      role: "tool",
      content: [{ type: "tool_result", tool_use_id: "call-1", content: result, is_error: isError }],
    });
  }
  return msgs;
}

beforeEach(() => {
  calls.length = 0;
  for (const k of Object.keys(sidecar)) delete sidecar[k];
  for (const k of Object.keys(subHistory)) delete subHistory[k];
  seed();
});

describe("会话恢复：工具结果 sidecar 回填", () => {
  it("历史文本被截断（解析失败）→ 一次批量 IPC 按 call id 拉回完整出参并回填卡片", async () => {
    // 后端 sidecar 存的是整套 ToolOutcome 信封
    sidecar["call-1"] = {
      call_id: "call-1",
      outcome: { ok: true, data: { title: "Demo", html: "<div>full</div>", chars: 12 } },
      duration_ms: 42,
    };
    useRun.getState().restoreFromMessages(session, history("render_html", TRUNCATED) as any);

    // 先落占位（不阻塞首帧）
    expect(card("call-1").outcome.data).toEqual({ restored: true });

    await vi.waitFor(() => expect(card("call-1").outcome.data.html).toBe("<div>full</div>"));
    // 只发一次 IPC，且只带失败的调用 id
    const fetches = calls.filter((c) => c.cmd === "load_tool_outcomes");
    expect(fetches).toHaveLength(1);
    expect(fetches[0].args.callIds).toEqual(["call-1"]);
    expect(card("call-1").status).toBe("ok");
    expect(card("call-1").durationMs).toBe(42); // 顺带回填耗时
  });

  it("失败但有出参的调用（sidecar 里 ok=false）：状态一并还原为失败，不是「已使用」", async () => {
    sidecar["call-1"] = {
      call_id: "call-1",
      outcome: { ok: false, error: { code: "E_EXIT_CODE", message: "命令退出码 3" }, data: { exit_code: 3, output: "boom" } },
      duration_ms: null,
    };
    useRun.getState().restoreFromMessages(session, history("command", "[error E_EXIT_CODE: 命令退出码 3]\n{\"exit_code\":3}") as any);
    await vi.waitFor(() => expect(card("call-1").status).toBe("error"));
    expect(card("call-1").outcome.error.code).toBe("E_EXIT_CODE");
    expect(card("call-1").outcome.data.output).toBe("boom");
  });

  it("兼容只存了 data 的旧备份：按成功包裹", async () => {
    sidecar["call-1"] = { call_id: "call-1", outcome: { html: "<i>legacy</i>" } };
    useRun.getState().restoreFromMessages(session, history("render_html", TRUNCATED) as any);
    await vi.waitFor(() => expect(card("call-1").outcome.data.html).toBe("<i>legacy</i>"));
    expect(card("call-1").status).toBe("ok");
  });

  it("后端没有备份（旧会话）→ 保持占位，不报错", async () => {
    useRun.getState().restoreFromMessages(session, history("render_html", TRUNCATED) as any);
    await vi.waitFor(() => expect(calls.some((c) => c.cmd === "load_tool_outcomes")).toBe(true));
    expect(card("call-1").outcome.data).toEqual({ restored: true });
  });

  it("能正常解析的结果不触发任何回读（小卡片零开销）", async () => {
    useRun.getState().restoreFromMessages(
      session,
      history("calculate", JSON.stringify({ expression: "1+1", result: 2 })) as any,
    );
    await Promise.resolve();
    expect(calls.some((c) => c.cmd === "load_tool_outcomes")).toBe(false);
    expect(card("call-1").outcome.data.result).toBe(2);
  });

  it("无结果 / 被中断的调用：落 E_INTERRUPTED（不再显示为「已使用」），且不回读", async () => {
    // 后端 repair 对悬空 tool_use 补的文本
    useRun.getState().restoreFromMessages(
      session,
      history("command", "[interrupted] 工具调用被中断，未产生结果", true) as any,
    );
    const c = card("call-1");
    expect(c.status).toBe("error");
    expect(c.outcome).toMatchObject({ ok: false, error: { code: "E_INTERRUPTED" } });
    expect(calls.some((x) => x.cmd === "load_tool_outcomes")).toBe(false);
  });

  it("整条结果缺失（旧会话悬空调用）：同样按已中断呈现", async () => {
    useRun.getState().restoreFromMessages(session, history("read", null) as any);
    expect(card("call-1").status).toBe("error");
    expect(card("call-1").outcome.error.code).toBe("E_INTERRUPTED");
  });

  it("真实失败（[error E_XXX: 说明]）：从文本还原错误码与说明，不是「已使用 + 空数据」", async () => {
    useRun.getState().restoreFromMessages(
      session,
      history("command", "[error E_EXIT_CODE: 命令退出码 1]", true) as any,
    );
    const c = card("call-1");
    expect(c.status).toBe("error");
    expect(c.outcome.error).toEqual({ code: "E_EXIT_CODE", message: "命令退出码 1" });
    // 这段文本同样解析不出 JSON（与 restoredToolData 同判据）→ 照例问一次 sidecar；
    // 失败结果本就无备份（data 为空，后端落盘门槛跳过）→ 返回空，卡片保持上面的还原结果
    await vi.waitFor(() => expect(calls.some((x) => x.cmd === "load_tool_outcomes")).toBe(true));
    expect(c.status).toBe("error");
    expect(c.outcome.error).toEqual({ code: "E_EXIT_CODE", message: "命令退出码 1" });
  });

  it("lossyToolKeys：解析不出 JSON 的才收，跳过 [interrupted] 与无结果项，去重保序", () => {
    const msgs: any[] = [
      {
        role: "assistant",
        content: [
          { type: "tool_use", id: "a", name: "read", args: {} },
          { type: "tool_use", id: "b", name: "read", args: {} },
          { type: "tool_use", id: "c", name: "read", args: {} },
          { type: "tool_use", id: "d", name: "read", args: {} },
          { type: "tool_use", id: "a", name: "read", args: {} },
        ],
      },
      {
        role: "tool",
        content: [
          { type: "tool_result", tool_use_id: "a", content: TRUNCATED },
          { type: "tool_result", tool_use_id: "b", content: "[interrupted] 工具调用被中断，未产生结果" },
          { type: "tool_result", tool_use_id: "d", content: '{"ok":1}' },
        ],
      },
    ];
    // a = 截断文本；b = 中断；c = 无结果；d = 可解析；重复的 a 只收一次
    expect(lossyToolKeys(msgs)).toEqual(["a"]);
  });

  it("子代理卡（report 被父历史截断）：合成 key 改名真实 sub_id、补回完整报告、流桶跟着搬", async () => {
    // 父历史里子代理 outcome 的模型侧文本被头尾截断 → 解析不出 sub_id / report
    const SUB_TRUNCATED = '{"sub_id":"sub-real","report":"第一段…[已截断 20000 字节]…';
    sidecar["call-1"] = {
      call_id: "call-1",
      outcome: { ok: true, data: { sub_id: "sub-real", report: "完整报告正文" } },
      duration_ms: 1234,
    };
    useRun.getState().restoreFromMessages(session, [
      {
        role: "assistant",
        content: [
          {
            type: "tool_use",
            id: "call-1",
            name: "subagent",
            args: { role: "explore", task: "看看代码", description: "调研" },
          },
        ],
      },
      { role: "tool", content: [{ type: "tool_result", tool_use_id: "call-1", content: SUB_TRUNCATED }] },
    ] as any);

    // 先降级：合成 key + 无报告（抽屉只能展示 task + 最终报告）
    expect(tab().subs[0].subId).toBe("restored:call-1");
    expect(tab().subs[0].report).toBeUndefined();

    await vi.waitFor(() => expect(tab().subs[0].subId).toBe("sub-real"));
    expect(tab().subs[0].report).toBe("完整报告正文");
    // timeline 锚点同步改名，流桶搬过去（旧合成 key 清掉），抽屉据此才能拉真实过程流
    const item = tab().items.find((i: any) => i.kind === "assistant") as any;
    expect(item.timeline).toEqual([{ kind: "sub", subId: "sub-real" }]);
    expect(Object.keys(tab().subStreams)).toEqual(["sub-real"]);
    expect(tab().subStreams["sub-real"].loaded).toBe(false);
  });

  it("子代理过程抽屉：抽屉内工具卡的出参同样从 sidecar 回填", async () => {
    subHistory["sub-real"] = [
      { role: "assistant", content: [{ type: "tool_use", id: "c-9", name: "read", args: { path: "a.ts" } }] },
      { role: "tool", content: [{ type: "tool_result", tool_use_id: "c-9", content: TRUNCATED }] },
    ];
    sidecar["c-9"] = {
      call_id: "c-9",
      outcome: { ok: true, data: { path: "a.ts", content: "完整文件内容" } },
      duration_ms: 7,
    };
    useRun.setState((s) => {
      s.tabs[session]!.subs.push({
        subId: "sub-real",
        role: "explore",
        description: "调研",
        step: 0,
        maxSteps: 25,
        tokens: 0,
        lastTools: [],
        status: "done",
      } as any);
      s.tabs[session]!.subStreams["sub-real"] = { timeline: [], toolsMap: {}, status: "done", gen: 0, loaded: false };
    });

    await useRun.getState().openSubDrawer(session, "sub-real");

    // 过程流先按瘦身文本落卡（解析失败 → tail 占位）
    expect(subStream("sub-real").timeline).toEqual([{ kind: "tool", callKey: "c-9" }]);
    await vi.waitFor(() => expect(subStream("sub-real").toolsMap["c-9"].outcome.data.content).toBe("完整文件内容"));
    expect(subStream("sub-real").toolsMap["c-9"].status).toBe("ok");
    expect(subStream("sub-real").toolsMap["c-9"].durationMs).toBe(7);
    // 回填走的是同一个 IPC，且会话 id 用根会话（子代理结果存在根会话的 sidecar 下）
    const fetches = calls.filter((c) => c.cmd === "load_tool_outcomes");
    expect(fetches).toHaveLength(1);
    expect(fetches[0].args).toEqual({ sessionId: session, callIds: ["c-9"] });
  });
});
