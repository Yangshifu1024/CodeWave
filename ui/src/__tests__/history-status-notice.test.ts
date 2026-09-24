// 历史未完整保存的会话内提示（[docs/session-history-limits](../../../docs/session-history-limits.md)）：
//   ① `run:done` 载荷的可选字段 `history_save`（后端**仅当保存不干净时**才带上）→ 转录补一条 notice；
//   ② `SessionMeta.history_status`（挂在会话索引上，重启后仍在）→ 恢复历史时补同一条 notice。
// 口径：干净路径零打扰；只要状态在就每次打开都显示（不做「已读」交互、不加本地持久化）。
// 惯例：唯一 invoke 入口 ui/src/ipc/client 必须 mock（不直接 mock @tauri-apps/api/core）；
// zustand store 是模块级单例，逐用例重建态桶（踩坑清单）。
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { waitFor } from "@testing-library/react";

const ipcMock = vi.hoisted(() => ({
  loadSession: vi.fn(async (): Promise<unknown[]> => []),
  listSessions: vi.fn(async (): Promise<unknown[]> => []),
  listProjects: vi.fn(async (): Promise<unknown[]> => []),
  getSessionPrefs: vi.fn(async () => ({ approval_mode: "auto_edit", model_id: null, reasoning_effort: null })),
  sessionRunning: vi.fn(async () => false),
  gitStatus: vi.fn(async () => ({ repo: false, entries: [] })),
  getTokenBreakdown: vi.fn(async () => null),
  mcpConnect: vi.fn(async () => undefined),
  getUiState: vi.fn(async () => null),
  setUiState: vi.fn(async () => undefined),
}));

vi.mock("../ipc/client", () => ({ ipc: ipcMock }));

import { i18n } from "../i18n";
import { DEFAULT_PREFS } from "../ipc/types";
import type { Message, SessionMeta } from "../ipc/types";
import { useRun } from "../stores/run";
import type { Tab } from "../stores/sessions";
import { useSessions } from "../stores/sessions";

/** 历史重建用的最小消息集（一条 user + 一条 assistant；恢复路径本身不关心内容细节） */
const MSGS = [
  { role: "user", content: [{ type: "text", text: "你好" }] },
  { role: "assistant", content: [{ type: "text", text: "你好！" }] },
] as unknown as Message[];

function metaOf(id: string, extra: Partial<SessionMeta> = {}): SessionMeta {
  return {
    id,
    title: id,
    workspace: `/tmp/${id}`,
    model_id: null,
    created_at: "2026-09-01T00:00:00Z",
    updated_at: "2026-09-01T00:00:00Z",
    last_opened_at: null,
    message_count: 0,
    project_id: null,
    roots: [`/tmp/${id}`],
    running: false,
    interrupted: null,
    ...extra,
  };
}

/** Tab 构造（骨架 Tab 的字段与 restoreTabs 产出一致） */
function tabWith(id: string, loaded = true): Tab {
  return {
    key: id,
    sessionId: id,
    workspace: `/tmp/${id}`,
    title: id,
    projectId: null,
    createdAt: "2026-09-01T00:00:00Z",
    prefs: { ...DEFAULT_PREFS },
    loaded,
  };
}

/** 运行态桶（字段照 run.queue.test.ts 的 seed；缺字段会直接炸出 undefined） */
function seedRun(session: string, running = true) {
  useRun.setState((s) => {
    s.tabs[session] = {
      items: [], running, streamGen: 0, ask: null, breakdown: null,
      todos: [], suggestions: [], subs: [], subStreams: {}, subDrawer: { open: false, subId: null },
      gitEntries: null, writeTick: 0, queue: [], pendingItemId: null, draftFromQueue: null,
      lastDoneRunId: null, compacting: false,
    };
  });
}

/** 转录里的 notice 文案（顺序即显示顺序） */
function noticeTexts(session: string): string[] {
  return useRun
    .getState()
    .tabs[session]!.items.filter((i) => i.kind === "notice")
    .map((i) => (i as { text: string }).text);
}

function handlers() {
  return useRun.getState().bindGlobalHandlers();
}

/** 三种保存结果载荷（后端只在第 2、3 种时才把 history_save 放进 run:done） */
const rejected = { saved: false, stripped_images: 0, dropped_rounds: 0, bytes: 0 };
const degraded = { saved: true, stripped_images: 2, dropped_rounds: 1, bytes: 4096 };
const clean = { saved: true, stripped_images: 0, dropped_rounds: 0, bytes: 1024 };

beforeEach(() => {
  vi.clearAllMocks();
  ipcMock.loadSession.mockResolvedValue(MSGS);
  useSessions.setState({ tabs: [], activeKey: null, sessions: [], projects: [] });
  useRun.setState((s) => {
    s.tabs = {};
    s.drafts = {};
  });
});

afterEach(() => {
  useSessions.setState({ tabs: [], activeKey: null, sessions: [], projects: [] });
  useRun.setState((s) => {
    s.tabs = {};
    s.drafts = {};
  });
});

describe("run:done.history_save（当次保存结果）", () => {
  it("saved=false（超限拒存）→ 转录末尾一条「历史未能保存」提示，其余语义不动", () => {
    seedRun("s1");
    handlers()["run:done"]({ session: "s1", run_id: "r1", history_save: rejected });
    expect(noticeTexts("s1")).toEqual([i18n.t("notice.historyRejected")]);
    expect(useRun.getState().tabs["s1"]!.running).toBe(false); // 既有 done 语义照旧
  });

  it("saved=true 且有降级计数 → 「历史未完整保存」提示带图片数与轮数", () => {
    seedRun("s1");
    handlers()["run:done"]({ session: "s1", run_id: "r1", history_save: degraded });
    const texts = noticeTexts("s1");
    expect(texts).toEqual([i18n.t("notice.historyDegraded", { images: 2, rounds: 1 })]);
    expect(texts[0]).toContain("2");
    expect(texts[0]).toContain("1");
  });

  it("干净保存结果（saved=true、无降级计数）→ 零打扰，不产生提示", () => {
    seedRun("s1");
    handlers()["run:done"]({ session: "s1", run_id: "r1", history_save: clean });
    expect(noticeTexts("s1")).toEqual([]);
  });

  it("载荷不带 history_save（干净路径）→ 不产生任何提示", () => {
    seedRun("s1");
    handlers()["run:done"]({ session: "s1", run_id: "r1" });
    expect(noticeTexts("s1")).toEqual([]);
  });

  it("同一 run_id 的迟到 done 不重复提示（run_id 幂等守卫照旧）", () => {
    seedRun("s1");
    const h = handlers();
    h["run:done"]({ session: "s1", run_id: "r1", history_save: rejected });
    useRun.setState((s) => {
      s.tabs["s1"]!.running = true; // 迟到 done 场景：running 已被后续运行翻回 true
    });
    h["run:done"]({ session: "s1", run_id: "r1", history_save: rejected });
    expect(noticeTexts("s1")).toHaveLength(1);
  });

  it("连续两次 run:done 都降级（同文案、不同 run_id）→ 转录里只留一条提示（跨 run 去重）", () => {
    seedRun("s1");
    const h = handlers();
    h["run:done"]({ session: "s1", run_id: "r1", history_save: degraded });
    useRun.setState((s) => {
      s.tabs["s1"]!.running = true; // 新一轮运行开始（上一次 notice 已是转录末项）
    });
    h["run:done"]({ session: "s1", run_id: "r2", history_save: degraded });
    expect(noticeTexts("s1")).toEqual([i18n.t("notice.historyDegraded", { images: 2, rounds: 1 })]);
  });

  it("两次降级之间隔着用户消息与助手回复（非 notice 项）→ 仍只一条；隔着别的 notice 则重新提示一次", () => {
    seedRun("s1");
    const h = handlers();
    h["run:done"]({ session: "s1", run_id: "r1", history_save: rejected });
    // 模拟用户继续对话：真实转录里两次 done 之间必然夹着用户消息与助手回复
    useRun.setState((s) => {
      s.tabs["s1"]!.items.push(
        { kind: "user", text: "继续" },
        { kind: "assistant", timeline: [], toolsMap: {}, streaming: false },
      );
      s.tabs["s1"]!.running = true;
    });
    h["run:done"]({ session: "s1", run_id: "r2", history_save: rejected });
    expect(noticeTexts("s1")).toEqual([i18n.t("notice.historyRejected")]);

    // 口径边界（有意为之）：最近一条 notice 是别的提示（取消/重试/压缩等）→ 视为被别的事打断，再越限时重新提示
    useRun.setState((s) => {
      s.tabs["s1"]!.items.push({ kind: "notice", text: i18n.t("notice.cancelled") });
      s.tabs["s1"]!.running = true;
    });
    h["run:done"]({ session: "s1", run_id: "r3", history_save: rejected });
    expect(noticeTexts("s1")).toEqual([
      i18n.t("notice.historyRejected"),
      i18n.t("notice.cancelled"),
      i18n.t("notice.historyRejected"),
    ]);
  });
});

describe("SessionMeta.history_status（重启后仍可见）", () => {
  it("restoreFromMessages 带 meta.history_status=rejected → 重建转录末尾补一条提示", () => {
    useRun.getState().restoreFromMessages("s2", MSGS, {
      history_status: { kind: "rejected", at: "2026-09-02T00:00:00Z", reason: "历史超过 8MB 上限，请新开会话" },
    });
    const items = useRun.getState().tabs["s2"]!.items;
    expect(items.at(-1)).toEqual({ kind: "notice", text: i18n.t("notice.historyRejected") });
  });

  it("restoreFromMessages 带 meta.history_status=degraded → 提示带计数", () => {
    useRun.getState().restoreFromMessages("s2", MSGS, {
      history_status: { kind: "degraded", stripped_images: 2, dropped_rounds: 1, at: "2026-09-02T00:00:00Z" },
    });
    const items = useRun.getState().tabs["s2"]!.items;
    expect(items.at(-1)).toEqual({
      kind: "notice",
      text: i18n.t("notice.historyDegraded", { images: 2, rounds: 1 }),
    });
    expect(noticeTexts("s2")).toHaveLength(1); // 历史重建本身不额外造 notice
  });

  it("restoreFromMessages 不带 meta / meta 无 history_status → 不产生提示", () => {
    useRun.getState().restoreFromMessages("s2", MSGS);
    expect(noticeTexts("s2")).toEqual([]);
    useRun.getState().restoreFromMessages("s3", MSGS, {});
    expect(noticeTexts("s3")).toEqual([]);
  });

  it("openSession 把 SessionMeta.history_status 带进恢复（sessions.ts 接线）", async () => {
    const meta = metaOf("s4", {
      history_status: { kind: "rejected", at: "2026-09-02T00:00:00Z", reason: "超限" },
    });
    useSessions.setState({ sessions: [meta] });
    await useSessions.getState().openSession(meta);
    expect(noticeTexts("s4")).toEqual([i18n.t("notice.historyRejected")]);
  });

  it("骨架 Tab 惰性激活时回退会话列表快照里的 history_status", async () => {
    useSessions.setState({
      tabs: [tabWith("s5", false)],
      sessions: [
        metaOf("s5", {
          history_status: { kind: "degraded", stripped_images: 1, dropped_rounds: 3, at: "2026-09-02T00:00:00Z" },
        }),
      ],
      projects: [],
    });
    useSessions.getState().activate("s5");
    await waitFor(() => expect(noticeTexts("s5")).toHaveLength(1));
    expect(noticeTexts("s5")[0]).toBe(i18n.t("notice.historyDegraded", { images: 1, rounds: 3 }));
  });

  it("openSession 的 meta 无 history_status → 不产生提示", async () => {
    const meta = metaOf("s6");
    useSessions.setState({ sessions: [meta] });
    await useSessions.getState().openSession(meta);
    expect(noticeTexts("s6")).toEqual([]);
  });
});
