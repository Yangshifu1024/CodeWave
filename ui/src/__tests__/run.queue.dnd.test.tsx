// 运行队列 DOM 层拖拽集成测试（[fix/queue-panel-width-and-drag]）：
// 之前 run.queue.test.ts 只覆盖 store 层 reorderQueue，DOM 层 HTML5 DnD 未测。
// 这条测试同时也是「draggable={draggingId === q.id}」首改拖死锁的回归守护。
import { describe, it, expect, vi, beforeEach, beforeAll, afterEach } from "vitest";
import { render, cleanup, fireEvent } from "@testing-library/react";
import { App as AntApp } from "antd";
import "../i18n";

const ipcMock = vi.hoisted(() => ({
  selectDocumentFiles: vi.fn(async () => [] as string[]),
  checkExternalPath: vi.fn(async (_sid: string, _p: string) => ({ inside: true, dir: "", ref: "" })),
  allowExternalDir: vi.fn(async () => [] as string[]),
  readWorkspaceFileBase64: vi.fn(async () => ({ path: "", size: 0, content: "" })),
  listSkills: vi.fn(async () => []),
  searchWorkspacePaths: vi.fn(async (_sid: string, _q: string, _limit?: number) => [] as string[]),
  compactSession: vi.fn(async () => null),
  startChat: vi.fn(async () => "ok"),
  cancelRun: vi.fn(async () => null),
}));

vi.mock("@tauri-apps/api/core", () => ({
  invoke: vi.fn(async () => null),
  Channel: class {
    onmessage: any = null;
  },
}));

vi.mock("../ipc/client", () => ({ ipc: ipcMock }));

import Composer from "../features/chat/Composer";
import { useSessions } from "../stores/sessions";
import { useRun } from "../stores/run";
import { useSettings } from "../stores/settings";
import { useUi } from "../stores/ui";

const MODEL_BASE = {
  id: "m1", model: "test-model", max_tokens: 32768, context_window: 128000,
  reasoning_effort: null, vision: true, video: false,
};

/** 已 running=true 的会话，队列里塞三条任务。Composer 会在 send 失败时把任务入队而不调 start_chat。 */
function seed() {
  useSettings.setState({
    config: {
      schema_version: 2,
      providers: [{
        id: "p1", name: "Test Provider", api_format: "openai_chat" as const,
        base_url: "https://api.example.com/v1", keys: ["***abcd"],
        models: [{ ...MODEL_BASE }], headers: [],
      }],
      active_model_id: "m1",
      proxy: null,
      network: { allow_private_network: false },
      compact_threshold: 0.6,
      compact_timeout_seconds: 180,
      approval: { enabled: true, confirm_outside_create: true, confirm_git_push: true, auto_confirm: false, command_allowlist: [] },
      post_write_check: { enabled: false, command: "", timeout_seconds: 30, tail_chars: 3000 },
      ui: { font_size: 15, accent: "cyan", language: "zh-CN" },
      custom_prompt: null,
      disabled_skills: [],
      log: { level: "info", session_verbose: false },
    },
    loaded: true,
  });
  useSessions.setState({
    tabs: [{
      key: "s1", sessionId: "s1", workspace: "/tmp/ws", title: "s1",
      projectId: null, createdAt: "2026-09-01T00:00:00Z",
      prefs: { approval_mode: "auto_edit", model_id: null, reasoning_effort: null },
    }],
    activeKey: "s1",
  });
  useRun.setState((s) => {
    s.tabs["s1"] = {
      items: [], running: true, streamGen: 0, ask: null, breakdown: null,
      todos: [], suggestions: [], subs: [], subStreams: {}, subDrawer: { open: false, subId: null },
      gitEntries: null, writeTick: 0,
      queue: [
        { id: "qA", text: "任务A", images: [] },
        { id: "qB", text: "任务B", images: [] },
        { id: "qC", text: "任务C", images: [] },
      ],
      pendingItemId: null, draftFromQueue: null, lastDoneRunId: null, compacting: false,
    };
    s.drafts = { s1: { text: "", images: [], refs: [] } };
  });
}

const items = () => Array.from(document.querySelectorAll(".queue-item")) as HTMLElement[];

beforeAll(() => {
  Element.prototype.scrollTo = (Element.prototype as any).scrollTo ?? (() => {});
});

beforeEach(() => {
  // happy-dom 不实现 dataTransfer；fireEvent.dragStart/drop 不带时也不会 crash 但 setData 会 noop
  // 关键路径在 onDragStart 的 gripArmed 守卫 + setDraggingId + onDrop 调 reorderQueue
  // —— store 调用的副作用是「queue 数组换序」，测试只断言它
});

afterEach(() => {
  cleanup();
  useUi.setState({ settingsOpen: false, tasksOpen: false, statsOpen: false, rbTab: "info" });
  useSessions.setState({ tabs: [], activeKey: null, projects: [], sessions: [] });
  useSettings.setState({ config: null, loaded: false });
  useRun.setState({ tabs: {}, drafts: {} });
  vi.clearAllMocks();
});

describe("运行队列 DOM 层拖拽（[fix/queue-panel-width-and-drag]）", () => {
  it("item 的 draggable 恒为 true（首改拖死锁的回归：draggingId 初始 null 时仍可启动 DnD）", () => {
    seed();
    render(<AntApp><Composer /></AntApp>);

    expect(items().length).toBe(3);
    for (const it of items()) {
      // 原实现是 `draggable={draggingId === q.id}` → draggingId=null 时 draggable=false
      // happy-dom 不实现 IDL `.draggable` 属性，只同步 attribute：这里断言 attribute。
      // 死锁场景下 attribute 为 "false"，修复后为 "true"。
      expect(it.getAttribute("draggable")).toBe("true");
    }
  });

  it("按下 grip 后 dragStart 真正启动（draggingId 被设置），drop 后 queue 实际换序", () => {
    seed();
    const before = useRun.getState().tabs["s1"]!.queue.map((q) => q.id);
    expect(before).toEqual(["qA", "qB", "qC"]);

    render(<AntApp><Composer /></AntApp>);
    const list = items();
    expect(list.length).toBe(3);

    const [first, , third] = list;
    const gripA = first.querySelector(".queue-grip") as HTMLElement;
    expect(gripA).toBeTruthy();

    // 1) 按下 grip → gripArmed 置 true
    fireEvent.mouseDown(gripA);

    // 2) 从 first（qA）启动拖动 → onDragStart 守卫通过、setDraggingId(qA)
    // happy-dom dataTransfer 是占位对象，setData/effectAllowed 接受但不真正存
    const dt = { effectAllowed: "", setData: vi.fn() } as any;
    fireEvent.dragStart(first, { dataTransfer: dt });
    // draggingId 状态在 onDragStart 闭包中设置；class 应已加 dragging
    expect(first.className).toContain("dragging");

    // 3) 拖到 third（qC）上 → dragOver 阻止默认 + dropEffect
    fireEvent.dragOver(third, { dataTransfer: dt });
    expect(third.className).toContain("drop-target");

    // 4) 释放 → onDrop 调 reorderQueue(qA, qC)
    fireEvent.drop(third, { dataTransfer: dt });

    // 期望：qA 移到 qC 位置 → qB qC qA
    const after = useRun.getState().tabs["s1"]!.queue.map((q) => q.id);
    expect(after).toEqual(["qB", "qC", "qA"]);
  });

  it("不在 grip 上按下而直接 dragStart：被守卫拒，不换序（点击文本 / 按钮不误拖）", () => {
    seed();
    const before = useRun.getState().tabs["s1"]!.queue.map((q) => q.id);
    expect(before).toEqual(["qA", "qB", "qC"]);

    render(<AntApp><Composer /></AntApp>);
    const list = items();
    const [first, , third] = list;

    // 不 fireEvent.mouseDown on grip —— 直接 dragStart
    const dt = { effectAllowed: "", setData: vi.fn() } as any;
    fireEvent.dragStart(first, { dataTransfer: dt });

    // draggingId 没被设 → 没有 .dragging class（state 仍是 null）
    expect(first.className).not.toContain("dragging");
    // drop 即便发生：draggingId=null，reorderQueue 第一参数无效，no-op
    fireEvent.drop(third, { dataTransfer: dt });
    const after = useRun.getState().tabs["s1"]!.queue.map((q) => q.id);
    expect(after).toEqual(before); // 顺序不变
  });
});
