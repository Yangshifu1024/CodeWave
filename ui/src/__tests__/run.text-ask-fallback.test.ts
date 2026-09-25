// 文本形态 ask 兜底的前端补剥（[docs/text-form-ask-fallback](../../../docs/text-form-ask-fallback.md)）：
// 某 BYOK 端点偶发把 ask 工具调用**当正文 XML 透传**（本回合无 tool_use，正文末尾漂着完整的 `<ask>…</ask>`）。
// 后端把它恢复成等价 ask 调用（走既有 AskTool 通道）并在**落盘历史**里剥掉那段块；但流式帧早已把这段
// 协议原文送进**当轮气泡**且无法回收 → 后端在 `ask:opened` 里带上 `text_recovered`（被剥离的原文），
// 前端据此从最近一条 assistant 气泡文本里做子串移除 + 末尾 trim，使流式与定稿（重开会话后）表现一致。
//
// **剥离不能只做一次**：正文经 **64ms 节流**下发（后端 `stream_flush_loop`），而 ask:opened 在流结束后几毫秒
// 就到——最后那个节流窗口里的尾巴（往往正是 `</ask>`）会在 ask:opened **之后**才作为 delta_text 到达。
// 故待剥字符串作为**当轮状态**留在 tab 上（`tab.textRecovered`），由每帧文本落地处（runFrames.applyFrameToTab
// → stripRecoveredInTab）幂等补剥，`run:done` 清空。本文件除「帧先到齐 → ask:opened」的理想序外，
// 还专门钉了「后到帧也剥」那一序（同类先例：segments.tsx 的 stripReportMarkers 也在标记必然完整之后才剥）。
//
// 本文件断言的是 **store 状态**（气泡文本 + `tab.ask`），不碰渲染；事件喂法照抄 run.ask-mode.test.ts /
// run.streaming-indicator.test.ts（zustand store 是模块级单例：逐用例用独立 session 键播种，互不干扰）。
import { describe, expect, it, vi } from "vitest";
import { applyFrameToTab, blank } from "../stores/runFrames";
import { useRun } from "../stores/run";
import { useSessions } from "../stores/sessions";

vi.mock("@tauri-apps/api/core", () => ({
  invoke: vi.fn(async () => null),
  Channel: class {
    onmessage: any = null;
  },
}));

/** 块前散文（后端剥块后正文里逐字留下的就是它） */
const PROSE = "要生成域名，得先确认你用的是哪一家。";

/** 被透传的协议原文（后端 `text_recovered` 带回的就是这段子串，含 `<ask>` / `</ask>` 标记） */
const BLOCK = [
  "<ask>",
  "  <questions>",
  "    <item>",
  "      <id>cn_domain_source</id>",
  "      <question>这个域名是照着哪一家改的？</question>",
  "      <single>true</single>",
  "      <options>",
  "        <item><id>typo</id><label>拼写纠正</label></item>",
  "      </options>",
  "    </item>",
  "  </questions>",
  "</ask>",
].join("\n");

/** ask:opened 载荷里的题目（与 BLOCK 内容对应；前端只按字符串剥原文，与载荷解析无关） */
const QUESTIONS = [
  {
    id: "cn_domain_source",
    question: "这个域名是照着哪一家改的？",
    single: true,
    options: [{ id: "typo", label: "拼写纠正" }],
  },
];

function seedTab(session: string) {
  useRun.setState((s) => {
    s.tabs[session] = blank();
    // delta 帧只在运行中才落进转录（applyFrameToTab 的「运行结束后的迟到帧」守卫）
    s.tabs[session]!.running = true;
  });
  useSessions.setState({ activeKey: session });
}

/** 帧驱动必须走 setState（immer 草稿）：直接对冻结快照调用会抛 read only */
function delta(session: string, text: string) {
  useRun.setState((s) =>
    applyFrameToTab(s.tabs[session]!, { type: "delta_text", gen: 0, text } as any),
  );
}

/** 气泡文本 = 该 Tab 全部 assistant 项 text 段的拼接（段按到达序呈现，此处只关心文本全量） */
function bubbleText(session: string): string {
  let out = "";
  for (const it of useRun.getState().tabs[session]!.items) {
    if (it.kind !== "assistant") continue;
    for (const seg of it.timeline) if (seg.kind === "text") out += seg.text;
  }
  return out;
}

const handlers = () => useRun.getState().bindGlobalHandlers();

describe("文本形态 ask 兜底：气泡里的协议原文补剥", () => {
  it("带 text_recovered：块从当轮气泡剔除、块前散文保留、ask 卡照常就位", () => {
    const session = "s-text-recovered";
    seedTab(session);
    // 理想序（帧先到齐 → ask:opened）：散文 + 协议原文 + 块后换行（真实帧里块就是正文尾部那一段）
    delta(session, `${PROSE}\n\n${BLOCK}\n`);

    handlers()["ask:opened"]({
      session,
      ask_id: "a1",
      kind: "ask",
      questions: QUESTIONS as any,
      text_recovered: BLOCK,
    });

    const text = bubbleText(session);
    expect(text).not.toContain("<ask>"); // 协议原文已从气泡消失
    expect(text).not.toContain("cn_domain_source");
    // 块前散文逐字保留；尾部只剩剥离残留的空白（与后端 text_ask::strip_block 的 trim_end 同口径）
    expect(text).toContain(PROSE);
    expect(text.trimEnd()).toBe(PROSE);

    // ask 卡数据照旧来自载荷：题目与选项一字不动，且不新增任何来源标注
    const ask = useRun.getState().tabs[session]!.ask!;
    expect(ask.askId).toBe("a1");
    expect(ask.kind).toBe("ask");
    expect(ask.questions![0].id).toBe("cn_domain_source");
    expect(ask.questions![0].question).toBe("这个域名是照着哪一家改的？");
    expect(ask.questions![0].options![0].id).toBe("typo");
  });

  it("不带 text_recovered（真实工具调用路径）：气泡文本一字不动", () => {
    const session = "s-no-recovered";
    seedTab(session);
    delta(session, `${PROSE}\n\n${BLOCK}\n`);
    const before = bubbleText(session);

    handlers()["ask:opened"]({
      session,
      ask_id: "a2",
      kind: "ask",
      questions: QUESTIONS as any,
    });

    expect(bubbleText(session)).toBe(before); // 零影响
    expect(bubbleText(session)).toContain("<ask>");
    expect(useRun.getState().tabs[session]!.ask!.askId).toBe("a2");
    // 普通 ask 不进「待剥」状态：否则后续增量会拿一个不属于本轮的字符串去剥
    expect(useRun.getState().tabs[session]!.textRecovered ?? null).toBeNull();
  });

  it("带 text_recovered 但气泡里此刻没有该块（整块还没到 / 已被裁剪）：当时原样、不报错", () => {
    const session = "s-block-missing";
    seedTab(session);
    delta(session, PROSE); // 只有散文，没有块

    expect(() =>
      handlers()["ask:opened"]({
        session,
        ask_id: "a3",
        kind: "ask",
        questions: QUESTIONS as any,
        text_recovered: BLOCK,
      }),
    ).not.toThrow();

    expect(bubbleText(session)).toBe(PROSE);
    expect(useRun.getState().tabs[session]!.ask!.askId).toBe("a3");
    // 待剥状态照旧保留（这是「当轮」语义）：块若在后到的帧里补齐，仍会被剥
    expect(useRun.getState().tabs[session]!.textRecovered).toBe(BLOCK);
  });

  it("M-1 守卫不变：桶不存在时既不重建桶、也不动任何文本", () => {
    const session = "s-no-bucket";
    useRun.setState((s) => {
      delete s.tabs[session];
    });

    expect(() =>
      handlers()["ask:opened"]({
        session,
        ask_id: "a4",
        kind: "ask",
        questions: QUESTIONS as any,
        text_recovered: BLOCK,
      }),
    ).not.toThrow();

    expect(useRun.getState().tabs[session]).toBeUndefined();
  });

  it("后到的增量也剥（64ms 节流把 `</ask>` 尾巴推到 ask:opened 之后）：气泡最终无残留", () => {
    const session = "s-late-tail";
    seedTab(session);
    // 首帧：散文 + 块的其余全部，**唯独缺 `</ask>`**——节流窗口把块切在两帧之间
    const cut = BLOCK.indexOf("</ask>");
    delta(session, `${PROSE}\n\n${BLOCK.slice(0, cut)}`);

    handlers()["ask:opened"]({
      session,
      ask_id: "a5",
      kind: "ask",
      questions: QUESTIONS as any,
      text_recovered: BLOCK,
    });
    // 此刻块还不完整：剥不到东西（这一断言正是本用例要钉的「只剥一次必然漏」）
    expect(bubbleText(session)).toContain("<ask>");
    expect(useRun.getState().tabs[session]!.textRecovered).toBe(BLOCK);

    // 尾帧补齐全块（真实场景里就是最后那个节流窗口里的 `</ask>`）
    delta(session, `${BLOCK.slice(cut)}\n`);

    const text = bubbleText(session);
    expect(text).not.toContain("<ask>"); // 回归钉：后到的增量也被剥了
    expect(text).not.toContain("</ask>");
    expect(text).not.toContain("cn_domain_source");
    expect(text).toContain(PROSE); // 散文一字不动
    expect(text.trimEnd()).toBe(PROSE);
  });

  it("run:done 清空待剥状态：下一轮的新增量不再被剥", () => {
    const session = "s-cleared-on-done";
    seedTab(session);
    delta(session, PROSE);
    handlers()["ask:opened"]({
      session,
      ask_id: "a6",
      kind: "ask",
      questions: QUESTIONS as any,
      text_recovered: BLOCK,
    });
    expect(useRun.getState().tabs[session]!.textRecovered).toBe(BLOCK);

    handlers()["run:done"]({ session, run_id: "r1" });
    expect(useRun.getState().tabs[session]!.textRecovered ?? null).toBeNull();

    // 下一轮（模拟新一轮起跑：running 重新为真）里再出现同样的协议原文 → 不再被剥
    useRun.setState((s) => {
      s.tabs[session]!.running = true;
    });
    delta(session, `\n\n${BLOCK}\n`);
    expect(bubbleText(session)).toContain("</ask>");
  });

  it("块被 thinking 增量切成两个 text 段（跨段）：不剥、不报错（行为记录）", () => {
    const session = "s-split-segments";
    seedTab(session);
    const cut = BLOCK.indexOf("</ask>");
    delta(session, `${PROSE}\n\n${BLOCK.slice(0, cut)}`); // 段 1 尾部是块的开头
    handlers()["ask:opened"]({
      session,
      ask_id: "a7",
      kind: "ask",
      questions: QUESTIONS as any,
      text_recovered: BLOCK,
    });
    // thinking 增量把正文切成两段：完整块从此不可能出现在同一段里
    useRun.setState((s) =>
      applyFrameToTab(s.tabs[session]!, { type: "delta_thinking", gen: 0, text: "想一下" } as any),
    );
    delta(session, `${BLOCK.slice(cut)}\n`);

    // 记录当前行为：跨段不剥（本来就不该，也没报错）；ask 卡照常就位
    expect(() => bubbleText(session)).not.toThrow();
    expect(bubbleText(session)).toContain("</ask>");
    expect(useRun.getState().tabs[session]!.ask!.askId).toBe("a7");
  });
});
