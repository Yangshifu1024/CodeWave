// 转录里的两类「历史可见性」（批2 P2，plan §2 / §5-P2 承诺项）：
// ① 上下文压缩边界分隔线：插在「段号 ≥ 边界 seq 的第一条消息」之前；多次压缩各就各位；
//    随 loadSessionEarlier 拿到的更早段一起生效（前插后位置仍正确）；无 boundaries 字段（旧后端 / legacy）
//    时不渲染、不抛错。
// ② 坏段可见提示：paging.bad_segments > 0 时在转录顶部出一条轻量告警；0 / 缺字段时零打扰。
// 位置一律按 DOM 兄弟顺序断言（Fragment 不产生节点，故顺序 = 转录顺序）。
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { act, cleanup, render } from "@testing-library/react";
import { App } from "antd";
import "../i18n";
import ChatMessages from "../features/chat/ChatMessages";
import { useRun } from "../stores/run";
import { useSessions } from "../stores/sessions";
import type { TabRunState, UiItem } from "../stores/run.types";
import type { HistoryBoundary, Message, SessionPaging } from "../ipc/types";
import zh from "../i18n/zh-CN";
import en from "../i18n/en-US";

const ISO = "2026-01-02T03:04:05.000Z";

/** 唯一 invoke 入口必须 mock（ChatMessages 的锚点落盘与分页前翻都走它） */
const ipcMock = vi.hoisted(() => ({
  setUiState: vi.fn(async (_state: unknown): Promise<void> => {}),
  loadSessionEarlier: vi.fn(async (): Promise<{ messages: Message[]; from_seq: number; has_more: boolean }> => ({
    messages: [],
    from_seq: 0,
    has_more: false,
  })),
  loadToolOutcomes: vi.fn(async () => []),
}));
vi.mock("../ipc/client", () => ({ ipc: ipcMock }));

const user = (text: string): UiItem => ({ kind: "user", text, createdAt: ISO });
const bd = (seq: number, source: "compact" | "shrink" = "compact"): HistoryBoundary => ({ seq, source, at: ISO });
const paging = (over: Partial<SessionPaging> = {}): SessionPaging => ({
  format: "new",
  loaded_from_seq: 1,
  segment_count: 1,
  total_messages: 0,
  bytes: 0,
  has_more: false,
  ...over,
});

/** 播种一个 Tab（不挂载）：用 store 直填，避开 IPC 链路（与 scrollAnchor.test.ts 同口径） */
function seed(
  items: UiItem[],
  opts: { keys?: string[]; boundaries?: HistoryBoundary[]; page?: SessionPaging } = {},
): void {
  useSessions.setState({ activeKey: "s1" });
  useRun.setState((s) => {
    const tab = {
      items,
      itemKeys: opts.keys,
      running: false,
      streamGen: 0,
      ask: null,
      breakdown: null,
      todos: [],
      suggestions: [],
      subs: [],
      subStreams: {},
      subDrawer: { open: false, subId: null },
      gitEntries: null,
      writeTick: 0,
      queue: [],
      pendingItemId: null,
      draftFromQueue: null,
      lastDoneRunId: null,
      compacting: false,
      paging: opts.page
        ? {
            format: opts.page.format,
            loadedFromSeq: opts.page.loaded_from_seq,
            firstLoadedSeq: opts.page.loaded_from_seq,
            hasMore: opts.page.has_more,
            totalMessages: opts.page.total_messages,
            segmentCount: opts.page.segment_count,
            loadedPages: 1,
            loading: false,
            badSegments: opts.page.bad_segments ?? 0,
          }
        : undefined,
      boundaries: opts.boundaries,
    } satisfies TabRunState;
    s.tabs.s1 = tab;
  });
}

/** 转录顺序标签：msg:<稳定键> / bd:<边界段号>；分页条与告警行等「非转录项」过滤掉 */
function layout(container: HTMLElement): string[] {
  return [...container.querySelectorAll<HTMLElement>(".chat-messages > *")]
    .map((n) =>
      n.classList.contains("msg")
        ? `msg:${n.dataset.key}`
        : n.dataset.boundarySeq
          ? `bd:${n.dataset.boundarySeq}`
          : "other",
    )
    .filter((x) => x !== "other");
}

const mount = () => render(<App><ChatMessages /></App>);

describe("转录历史可见性：压缩边界分隔线与坏段提示", () => {
  beforeEach(() => {
    ipcMock.loadSessionEarlier.mockResolvedValue({ messages: [], from_seq: 0, has_more: false });
  });
  afterEach(() => {
    cleanup();
    useSessions.setState({ activeKey: null, tabs: [] });
    useRun.setState((s) => {
      s.tabs = {};
      s.drafts = {};
    });
  });

  it("压缩边界：分隔线插在第一条第段号 ≥ seq 的消息之前（多次压缩各就各位）", () => {
    const items = [user("一"), user("二"), user("三"), user("四")];
    seed(items, { keys: ["s1:0", "s2:0", "s3:0", "s4:0"], boundaries: [bd(2, "compact"), bd(4, "shrink")] });

    const { container } = mount();
    // ① 位置：s2 处（段号 2）与 s4 处（段号 4）各插一条，其余位置不受影响
    expect(layout(container)).toEqual(["msg:s1:0", "bd:2", "msg:s2:0", "msg:s3:0", "bd:4", "msg:s4:0"]);

    const dividers = [...container.querySelectorAll<HTMLElement>(".ant-divider")];
    expect(dividers).toHaveLength(2);
    // ② source 保留在 DOM 上（排障用）：compact / shrink 文案相同，只有数据不同
    expect(dividers.map((d) => d.dataset.boundarySource)).toEqual(["compact", "shrink"]);
    expect(dividers.map((d) => d.dataset.boundarySeq)).toEqual(["2", "4"]);
    expect(container.textContent).toContain("此处上下文已压缩");
    // ③ 分隔线不打 data-key：滚动锚点收集不会把它当锚点
    expect(container.querySelectorAll(".ant-divider[data-key]")).toHaveLength(0);
  });

  it("分隔线位于转录首项时也不丢（边界落在已加载的最早段）", () => {
    seed([user("一"), user("二")], { keys: ["s1:0", "s1:1"], boundaries: [bd(1)] });
    const { container } = mount();
    expect(layout(container)).toEqual(["bd:1", "msg:s1:0", "msg:s1:1"]);
  });

  it("无 boundaries 字段（旧后端 / legacy / 未压缩）：不渲染分隔线，也不抛错", () => {
    const items = [user("一"), user("二")];
    seed(items, { keys: ["s0:0", "s0:1"] }); // 缺省 = 无 boundaries
    const { container } = mount();
    expect(layout(container)).toEqual(["msg:s0:0", "msg:s0:1"]);
    expect(container.querySelectorAll(".ant-divider")).toHaveLength(0);
    expect(container.textContent).not.toContain("此处上下文已压缩");
  });

  it("空数组与脏数据（seq 非法）同样不渲染，且不影响其他边界", () => {
    const items = [user("一"), user("二")];
    seed(items, {
      keys: ["s1:0", "s2:0"],
      boundaries: [{ seq: Number.NaN, source: "compact", at: ISO } as HistoryBoundary],
    });
    const { container } = mount();
    expect(layout(container)).toEqual(["msg:s1:0", "msg:s2:0"]);

    cleanup();
    seed(items, { keys: ["s1:0", "s2:0"], boundaries: [] });
    const again = mount();
    expect(again.container.querySelectorAll(".ant-divider")).toHaveLength(0);
    expect(layout(again.container)).toEqual(["msg:s1:0", "msg:s2:0"]);
  });

  it("前翻更早段：本页带回的边界插在正确位置，并并入 store（收起后随之清掉）", async () => {
    const older: Message[] = [{ role: "user", content: [{ type: "text", text: "更早一" }] }];
    ipcMock.loadSessionEarlier.mockResolvedValue({
      messages: older,
      from_seq: 1,
      has_more: false,
      boundaries: [bd(2)],
    } as any);
    seed([user("最近一"), user("最近二")], {
      keys: ["s3:0", "s3:1"],
      boundaries: [],
      page: paging({ loaded_from_seq: 3, segment_count: 3, total_messages: 3, has_more: true }),
    });

    const { container } = mount();
    await act(async () => {
      await useRun.getState().loadEarlier("s1");
    });

    // 更早一段（键 s1:0）前插；边界 seq=2 落在它之后、s3:* 之前 —— 前插后位置仍正确
    expect(layout(container)).toEqual(["msg:s1:0", "bd:2", "msg:s3:0", "msg:s3:1"]);
    expect(useRun.getState().tabs.s1.boundaries).toEqual([bd(2)]);

    // 收起更早的：段号早于首屏的边界不再有对应内容 —— state 与界面一并清掉
    act(() => {
      useRun.getState().collapseEarlier("s1");
    });
    expect(useRun.getState().tabs.s1.boundaries).toEqual([]);
    expect(layout(container)).toEqual(["msg:s3:0", "msg:s3:1"]);
  });

  it("重复前翻同一段不叠出重复分隔线（按 seq + source 去重）", async () => {
    const older: Message[] = [{ role: "user", content: [{ type: "text", text: "更早一" }] }];
    ipcMock.loadSessionEarlier.mockResolvedValue({
      messages: older,
      from_seq: 1,
      has_more: true,
      boundaries: [bd(2), bd(2)],
    } as any);
    seed([user("最近一")], {
      keys: ["s3:0"],
      boundaries: [bd(2)],
      page: paging({ loaded_from_seq: 3, segment_count: 3, total_messages: 2, has_more: true }),
    });

    const { container } = mount();
    await act(async () => {
      await useRun.getState().loadEarlier("s1");
    });
    expect(useRun.getState().tabs.s1.boundaries).toEqual([bd(2)]);
    expect(container.querySelectorAll(".ant-divider")).toHaveLength(1);
  });

  it("坏段提示：bad_segments > 0 时出现在转录顶部；= 0 / 缺字段时零打扰", () => {
    seed([user("一")], { keys: ["s1:0"], page: paging({ bad_segments: 2, total_messages: 1 }) });
    const { container } = mount();
    expect(container.textContent).toContain("有 2 段历史未能读取，其余内容正常。");
    expect(container.querySelector(".ant-alert-warning")).toBeTruthy();

    cleanup();
    seed([user("一")], { keys: ["s1:0"], page: paging({ total_messages: 1 }) }); // 缺字段
    const zero = mount();
    expect(zero.container.textContent).not.toContain("未能读取");
    expect(zero.container.querySelector(".ant-alert")).toBeNull();

    cleanup();
    seed([user("一")], { keys: ["s1:0"], page: paging({ bad_segments: 0, total_messages: 1 }) });
    const explicitZero = mount();
    expect(explicitZero.container.textContent).not.toContain("未能读取");
    expect(explicitZero.container.querySelector(".ant-alert")).toBeNull();
  });

  it("坏段计数口径：首屏取 paging.bad_segments；前翻回传值时累加", async () => {
    // 首屏映射（restoreFromMessages 是唯一的建立点）
    useRun.getState().restoreFromMessages("s1", [], { paging: paging({ bad_segments: 1, has_more: true }) });
    expect(useRun.getState().tabs.s1.paging!.badSegments).toBe(1);

    // 前翻：后端当前不回该字段 → 沿用首屏值；回了就累加（契约补齐后无需再改前端）
    seed([user("最近一")], {
      keys: ["s3:0"],
      page: paging({ loaded_from_seq: 3, bad_segments: 1, has_more: true }),
    });
    ipcMock.loadSessionEarlier.mockResolvedValue({
      messages: [{ role: "user", content: [{ type: "text", text: "更早一" }] }],
      from_seq: 1,
      has_more: true,
      boundaries: [],
      bad_segments: 2,
    } as any);
    await act(async () => {
      await useRun.getState().loadEarlier("s1");
    });
    expect(useRun.getState().tabs.s1.paging!.badSegments).toBe(3);
    // 边界缺省（后端未给）→ 空数组，分隔线一条也不出
    expect(useRun.getState().tabs.s1.boundaries).toEqual([]);
  });

  it("i18n：分隔线与坏段提示的中英键齐备", () => {
    for (const key of ["chat.compactedBoundary", "chat.compactedBoundaryHint", "chat.badSegments"]) {
      const leaf = key.split(".")[1];
      expect((zh as any).chat[leaf], `zh 缺键 ${key}`).toBeTruthy();
      expect((en as any).chat[leaf], `en 缺键 ${key}`).toBeTruthy();
    }
    // 坏段文案带数量占位符（两语言都得有，否则 i18next 会原样吐出 {{n}}）
    expect((zh as any).chat.badSegments).toContain("{{n}}");
    expect((en as any).chat.badSegments).toContain("{{n}}");
  });
});
