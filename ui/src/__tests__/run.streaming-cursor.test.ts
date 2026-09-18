// 打字光标不变量回归测试：同一 Tab 至多一个 streaming assistant 项（＝一个光标），且运行结束后必须归零。
//
// 缺陷背景（无对应 docs 文档，此处直接描述）：run:inject / run:retry / sub:error 会往 items 末尾 push 一条 notice，
// 使「流式 assistant 项」不再是末项；下一帧 delta 经 currentAssistantIm 便新建第二个流式项 → 聊天里两个光标同时闪，
// 而收尾三兄弟（run:done / run:error / run:cancelled）只翻末项 → 旧光标永久残留。
// 修复：currentAssistantIm 新建前收尾遗留流式项（唯一守卫）+ 收尾三兄弟全量扫（兜底）。
import { beforeEach, describe, expect, it, vi } from "vitest";
import { applyFrameToTab, blank, currentAssistantIm } from "../stores/runFrames";
import { useRun } from "../stores/run";
import { useSessions } from "../stores/sessions";

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

function tabOf(session: string) {
  return useRun.getState().tabs[session]!;
}

/** 帧驱动必须走 setState（immer 草稿），直接对冻结快照调用会抛 read only */
function delta(session: string, text: string, gen = 0) {
  useRun.setState((s) => applyFrameToTab(s.tabs[session]!, { type: "delta_text", gen, text } as any));
}

/** 当前 streaming 的 assistant 项数量 = 屏幕上的闪烁光标数 */
function cursorCount(session: string) {
  return tabOf(session).items.filter((i) => i.kind === "assistant" && (i as any).streaming).length;
}

function kindsOf(session: string) {
  return tabOf(session).items.map((i) => i.kind);
}

describe("打字光标不变量（streaming assistant 唯一且收尾归零）", () => {
  const session = "s-cursor";

  beforeEach(() => {
    // 焦点态下不触发系统通知分支（避免插件动态 import）
    Object.defineProperty(document, "hasFocus", { value: () => true, configurable: true, writable: true });
    seedTab(session);
  });

  it("① run:inject 插队后仍在同一流式项续写：只 1 个光标，顺序 assistant | notice | assistant", () => {
    const h = handlers();
    delta(session, "第一段");
    h["run:inject"]({ session, count: 2 });
    delta(session, "第二段");

    expect(kindsOf(session)).toEqual(["assistant", "notice", "assistant"]);
    expect(cursorCount(session)).toBe(1);
    const t = tabOf(session);
    // 时间顺序保持：新文本渲染在 notice 下方，光标只在最底部一项
    expect((t.items[0] as any).timeline[0].text).toBe("第一段");
    expect((t.items[2] as any).timeline[0].text).toBe("第二段");
    expect((t.items[2] as any).streaming).toBe(true);
    // 遗留项已收尾且思考时长被冻结（与收尾三兄弟行为一致）
    expect((t.items[0] as any).streaming).toBe(false);
  });

  it("② run:inject + run:done：残留光标数 = 0（修复前为 1，本缺陷最关键断言）", () => {
    const h = handlers();
    delta(session, "第一段");
    h["run:inject"]({ session, count: 1 });
    delta(session, "第二段");
    expect(cursorCount(session)).toBe(1);

    h["run:done"]({ session });

    expect(cursorCount(session)).toBe(0);
    expect(tabOf(session).running).toBe(false);
  });

  it("③ run:retry：清场后新尝试仍只 1 个光标", () => {
    const h = handlers();
    delta(session, "旧尝试");
    h["run:retry"]({ session, attempt: 2, gen: 1 });
    delta(session, "新尝试", 1);

    expect(kindsOf(session)).toEqual(["assistant", "notice", "assistant"]);
    expect(cursorCount(session)).toBe(1);
    const t = tabOf(session);
    // retry 原有语义保留：被找到的旧流式项清空 timeline/toolsMap，且不再持有光标
    expect((t.items[0] as any).timeline).toEqual([]);
    expect((t.items[0] as any).streaming).toBe(false);
    expect((t.items[2] as any).timeline.map((s: any) => s.text)).toEqual(["新尝试"]);
  });

  it("④ sub:error：子代理失败后主流继续，仍只 1 个光标", () => {
    const h = handlers();
    delta(session, "第一段");
    h["sub:error"]({ session, sub_id: "sub_1", error: "boom" });
    delta(session, "第二段");

    expect(kindsOf(session)).toEqual(["assistant", "notice", "assistant"]);
    expect(cursorCount(session)).toBe(1);
  });

  it("⑤ 历史脏数据：两个遗留 streaming 项全部收尾，新项唯一且为末尾", () => {
    useRun.setState((s) => {
      const t = s.tabs[session]!;
      t.items.push(
        { kind: "assistant", timeline: [{ kind: "thinking", text: "旧思考", startedAt: Date.now() - 50 }], toolsMap: {}, streaming: true },
        { kind: "notice", text: "插队一" },
        { kind: "assistant", timeline: [], toolsMap: {}, streaming: true },
        { kind: "notice", text: "插队二" },
      );
      currentAssistantIm(t);
    });
    const t = tabOf(session);
    const assistants = t.items.filter((i) => i.kind === "assistant") as any[];
    expect(assistants).toHaveLength(3);
    expect(assistants[0].streaming).toBe(false);
    expect(assistants[1].streaming).toBe(false);
    expect(assistants[2].streaming).toBe(true);
    expect(t.items[t.items.length - 1]).toBe(assistants[2]); // 新项在末尾（光标只在最底部）
    expect(cursorCount(session)).toBe(1);
    // 思考计时冻结：被收尾项的未定格 thinking 段补上 durationMs
    expect(typeof assistants[0].timeline[0].durationMs).toBe("number");
  });

  it("⑤b 末项已是流式项时，其前方残留的流式项同样被收尾（光标唯一）", () => {
    useRun.setState((s) => {
      const t = s.tabs[session]!;
      t.items.push(
        { kind: "assistant", timeline: [], toolsMap: {}, streaming: true },
        { kind: "notice", text: "插队" },
        { kind: "assistant", timeline: [], toolsMap: {}, streaming: true },
      );
      currentAssistantIm(t);
    });
    const t = tabOf(session);
    expect(cursorCount(session)).toBe(1);
    expect((t.items[0] as any).streaming).toBe(false);
    expect((t.items[2] as any).streaming).toBe(true);
  });

  it("⑥ run:error 之后残留光标数 = 0", () => {
    const h = handlers();
    delta(session, "第一段");
    h["sub:error"]({ session, sub_id: "sub_1", error: "boom" });
    delta(session, "第二段");

    h["run:error"]({ session, error: "provider down", kind: "auth" });

    expect(cursorCount(session)).toBe(0);
    expect(tabOf(session).items[kindsOf(session).length - 1].kind).toBe("error");
  });

  it("⑦ run:cancelled 之后残留光标数 = 0", () => {
    const h = handlers();
    delta(session, "第一段");
    h["run:inject"]({ session, count: 1 });
    delta(session, "第二段");

    h["run:cancelled"]({ session });

    expect(cursorCount(session)).toBe(0);
    expect(tabOf(session).items[kindsOf(session).length - 1].kind).toBe("notice");
  });

  it("⑧ 兜底路径：notice 插队后未再收到 delta 即 run:done，光标也必须归零（R1 不生效、靠 R2 扫全量）", () => {
    const h = handlers();
    delta(session, "唯一一段");
    h["run:inject"]({ session, count: 1 });
    // 关键：没有后续 delta，故 currentAssistantIm 不会被再次调用；末项是 notice → 旧逻辑只翻末项就会漏
    h["run:done"]({ session });

    expect(cursorCount(session)).toBe(0);
    expect(kindsOf(session)).toEqual(["assistant", "notice"]);
  });
});
