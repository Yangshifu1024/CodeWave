// 流式等待指示不变量回归测试：同一 Tab 至多一个 streaming assistant 项（＝至多一条等待指示），且运行结束后必须归零。
//
// 缺陷背景（无对应 docs 文档，此处直接描述）：run:inject / run:retry / sub:error 会往 items 末尾 push 一条 notice，
// 使「流式 assistant 项」不再是末项；下一帧 delta 经 currentAssistantIm 便新建第二个流式项 → 聊天里同时出现两条等待指示，
// 而收尾三兄弟（run:done / run:error / run:cancelled）只翻末项 → 旧指示永久残留。
// 修复：currentAssistantIm 新建前收尾遗留流式项（唯一守卫）+ 收尾三兄弟全量扫（兜底）。
//
// 本文件断言的是**状态**（streaming 项计数），不是 DOM：渲染层已从自绘方块字符换成 antd 加载图标
// （[docs/chat-loading-indicator](../../../docs/chat-loading-indicator.md)，DOM 级断言见 chat.streaming-indicator.test.tsx），
// 计数不变量与渲染实现无关，照旧守住。
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

/** 当前 streaming 的 assistant 项数量 = 屏幕上的等待指示数量 */
function indicatorCount(session: string) {
  return tabOf(session).items.filter((i) => i.kind === "assistant" && (i as any).streaming).length;
}

function kindsOf(session: string) {
  return tabOf(session).items.map((i) => i.kind);
}

describe("等待指示不变量（streaming assistant 唯一且收尾归零）", () => {
  const session = "s-indicator";

  beforeEach(() => {
    // 焦点态下不触发系统通知分支（避免插件动态 import）
    Object.defineProperty(document, "hasFocus", { value: () => true, configurable: true, writable: true });
    seedTab(session);
  });

  it("① run:inject 插队后仍在同一流式项续写：只 1 条等待指示，顺序 assistant | notice | assistant", () => {
    const h = handlers();
    delta(session, "第一段");
    h["run:inject"]({ session, count: 2 });
    delta(session, "第二段");

    expect(kindsOf(session)).toEqual(["assistant", "notice", "assistant"]);
    expect(indicatorCount(session)).toBe(1);
    const t = tabOf(session);
    // 时间顺序保持：新文本渲染在 notice 下方，等待指示只在最底部一项
    expect((t.items[0] as any).timeline[0].text).toBe("第一段");
    expect((t.items[2] as any).timeline[0].text).toBe("第二段");
    expect((t.items[2] as any).streaming).toBe(true);
    // 遗留项已收尾且思考时长被冻结（与收尾三兄弟行为一致）
    expect((t.items[0] as any).streaming).toBe(false);
  });

  it("② run:inject + run:done：残留等待指示数 = 0（修复前为 1，本缺陷最关键断言）", () => {
    const h = handlers();
    delta(session, "第一段");
    h["run:inject"]({ session, count: 1 });
    delta(session, "第二段");
    expect(indicatorCount(session)).toBe(1);

    h["run:done"]({ session });

    expect(indicatorCount(session)).toBe(0);
    expect(tabOf(session).running).toBe(false);
  });

  it("③ run:retry：清场后新尝试仍只 1 条等待指示", () => {
    const h = handlers();
    delta(session, "旧尝试");
    h["run:retry"]({ session, attempt: 2, gen: 1 });
    delta(session, "新尝试", 1);

    expect(kindsOf(session)).toEqual(["assistant", "notice", "assistant"]);
    expect(indicatorCount(session)).toBe(1);
    const t = tabOf(session);
    // retry 原有语义保留：被找到的旧流式项清空 timeline/toolsMap，且不再持有等待指示
    expect((t.items[0] as any).timeline).toEqual([]);
    expect((t.items[0] as any).streaming).toBe(false);
    expect((t.items[2] as any).timeline.map((s: any) => s.text)).toEqual(["新尝试"]);
  });

  it("④ sub:error：子代理失败后主流继续，仍只 1 条等待指示", () => {
    const h = handlers();
    delta(session, "第一段");
    h["sub:error"]({ session, sub_id: "sub_1", error: "boom" });
    delta(session, "第二段");

    expect(kindsOf(session)).toEqual(["assistant", "notice", "assistant"]);
    expect(indicatorCount(session)).toBe(1);
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
    expect(t.items[t.items.length - 1]).toBe(assistants[2]); // 新项在末尾（等待指示只在最底部）
    expect(indicatorCount(session)).toBe(1);
    // 思考计时冻结：被收尾项的未定格 thinking 段补上 durationMs
    expect(typeof assistants[0].timeline[0].durationMs).toBe("number");
  });

  it("⑤b 末项已是流式项时，其前方残留的流式项同样被收尾（等待指示唯一）", () => {
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
    expect(indicatorCount(session)).toBe(1);
    expect((t.items[0] as any).streaming).toBe(false);
    expect((t.items[2] as any).streaming).toBe(true);
  });

  it("⑥ run:error 之后残留等待指示数 = 0", () => {
    const h = handlers();
    delta(session, "第一段");
    h["sub:error"]({ session, sub_id: "sub_1", error: "boom" });
    delta(session, "第二段");

    h["run:error"]({ session, error: "provider down", kind: "auth" });

    expect(indicatorCount(session)).toBe(0);
    expect(tabOf(session).items[kindsOf(session).length - 1].kind).toBe("error");
  });

  it("⑦ run:cancelled 之后残留等待指示数 = 0", () => {
    const h = handlers();
    delta(session, "第一段");
    h["run:inject"]({ session, count: 1 });
    delta(session, "第二段");

    h["run:cancelled"]({ session });

    expect(indicatorCount(session)).toBe(0);
    expect(tabOf(session).items[kindsOf(session).length - 1].kind).toBe("notice");
  });

  it("⑧ 兜底路径：notice 插队后未再收到 delta 即 run:done，等待指示也必须归零（R1 不生效、靠 R2 扫全量）", () => {
    const h = handlers();
    delta(session, "唯一一段");
    h["run:inject"]({ session, count: 1 });
    // 关键：没有后续 delta，故 currentAssistantIm 不会被再次调用；末项是 notice → 旧逻辑只翻末项就会漏
    h["run:done"]({ session });

    expect(indicatorCount(session)).toBe(0);
    expect(kindsOf(session)).toEqual(["assistant", "notice"]);
  });
});
