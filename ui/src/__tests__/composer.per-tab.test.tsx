// Composer 草稿按 Tab 隔离（drafts 平行分桶，key 同 tabs）：切会话各自保留、发送只清发送方 Tab、
// 关 Tab 草稿随 dispose 丢弃、切 Tab 收起提及菜单、编辑回填（队列编辑/消息修改）只落目标 Tab、
// ask 覆盖恢复草稿、队列出队不触碰草稿。
import { describe, it, expect, vi, beforeAll, afterEach } from "vitest";
import { render, screen, fireEvent, waitFor, cleanup, act } from "@testing-library/react";
import { App as AntApp } from "antd";
import "../i18n"; // Composer mounted standalone must init i18next explicitly (no global entry outside App.tsx)

vi.mock("@tauri-apps/api/core", () => ({
  invoke: vi.fn(async () => null),
  Channel: class {
    onmessage: any = null;
  },
}));

vi.mock("../ipc/client", () => {
  return {
    ipc: {
      listSkills: vi.fn(async () => []),
      searchWorkspacePaths: vi.fn(async () => []),
      listAgents: vi.fn(async () => []),
      compactSession: vi.fn(async () => null),
      startChat: vi.fn(async () => "ok"),
      cancelRun: vi.fn(async () => null),
    },
  };
});

import Composer from "../features/chat/Composer";
import { ipc } from "../ipc/client";
import { useSessions } from "../stores/sessions";
import { useRun, type PendingImage } from "../stores/run";
import { useSettings } from "../stores/settings";
import { useUi } from "../stores/ui";

const MODEL_BASE = {
  id: "m1", model: "test-model", max_tokens: 32768, context_window: 128000,
  reasoning_effort: null, vision: true, video: false,
};

function blankTab(items: any[] = []) {
  return {
    items, running: false, streamGen: 0, ask: null, breakdown: null,
    todos: [], suggestions: [], subs: [], subStreams: {}, subDrawer: { open: false, subId: null },
    gitEntries: null, writeTick: 0, queue: [], pendingItemId: null,
    draftFromQueue: null, lastDoneRunId: null, compacting: false,
  };
}

const IMG: PendingImage = { id: "img1", name: "a.png", mime: "image/png", data: "AAAA", dataUrl: "data:image/png;base64,AAAA" };

/** 双 Tab 环境（s1 活跃）：带模型配置（发送守卫用）与两个空运行态桶；草稿桶惰性创建，初始为空 */
function seedTwoTabs() {
  useSettings.setState({
    config: {
      schema_version: 2,
      providers: [
        {
          id: "p1", name: "Test Provider", api_format: "openai_chat" as const,
          base_url: "https://api.example.com/v1", keys: ["***abcd"],
          models: [{ ...MODEL_BASE }],
        },
      ],
      active_model_id: "m1",
      proxy: null,
      network: { allow_private_network: false },
      compact_threshold: 0.6,
      compact_timeout_seconds: 180,
      approval: { enabled: true, confirm_outside_create: true, confirm_git_push: true, auto_confirm: false, command_allowlist: [] },
      validation: { python: true, rust: true, typescript: true, go: true, json: true },
      ui: { font_size: 15, accent: "cyan", language: "zh-CN" },
      custom_prompt: null,
      disabled_skills: [],
      log: { level: "info", session_verbose: false },
    },
    loaded: true,
  });
  useSessions.setState({
    tabs: [
      { key: "s1", sessionId: "s1", workspace: "/tmp/ws1", title: "会话一", projectId: null, createdAt: "2026-09-01T00:00:00Z", prefs: { approval_mode: "auto_edit", model_id: null, reasoning_effort: null } },
      { key: "s2", sessionId: "s2", workspace: "/tmp/ws2", title: "会话二", projectId: null, createdAt: "2026-09-01T00:00:00Z", prefs: { approval_mode: "auto_edit", model_id: null, reasoning_effort: null } },
    ],
    activeKey: "s1",
  });
  useRun.setState((s) => {
    s.tabs["s1"] = blankTab();
    s.tabs["s2"] = blankTab();
  });
}

function switchTab(key: string) {
  act(() => {
    useSessions.setState({ activeKey: key });
  });
}

function mountComposer() {
  return render(
    <AntApp>
      <Composer />
    </AntApp>,
  );
}

function getTextarea() {
  return screen.getByPlaceholderText(/CodeWave/) as HTMLTextAreaElement;
}

function fireKey(el: HTMLElement, key: string, init: KeyboardEventInit = {}) {
  fireEvent(el, new KeyboardEvent("keydown", { key, bubbles: true, cancelable: true, ...init }));
}

beforeAll(() => {
  Element.prototype.scrollTo = (Element.prototype as any).scrollTo ?? (() => {});
});

afterEach(() => {
  cleanup();
  // zustand module-level singletons: clear leftovers (panel toggles + tabs/config + run tabs & drafts)
  useUi.setState({ settingsOpen: false, tasksOpen: false, statsOpen: false, rbTab: "info" });
  useSessions.setState({ tabs: [], activeKey: null, projects: [], sessions: [] });
  useSettings.setState({ config: null, loaded: false });
  useRun.setState({ tabs: {}, drafts: {} });
  vi.clearAllMocks();
});

describe("Composer 草稿按 Tab 隔离", () => {
  it("D1 文本草稿隔离：s1 输入 → 切 s2 为空 → 切回 s1 原样保留", () => {
    seedTwoTabs();
    mountComposer();
    const ta = getTextarea();
    fireEvent.change(ta, { target: { value: "草稿 A" } });
    expect(ta.value).toBe("草稿 A");
    switchTab("s2");
    expect(ta.value).toBe(""); // s2 是自己的空草稿，不再串到别 Tab 的内容
    switchTab("s1");
    expect(ta.value).toBe("草稿 A"); // 切回恢复
  });

  it("D2 附件随同一草稿桶隔离：s1 贴图 → s2 无缩略图 → 切回恢复", () => {
    seedTwoTabs();
    mountComposer();
    act(() => {
      useRun.getState().setDraftImages([IMG]);
    });
    expect(document.querySelector(".attach-chip")).toBeTruthy();
    switchTab("s2");
    expect(document.querySelector(".attach-chip")).toBeFalsy();
    switchTab("s1");
    expect(document.querySelector(".attach-chip")).toBeTruthy();
  });

  it("D3 发送只清发送方 Tab：s1 发送后清空，s2 草稿不受影响", async () => {
    seedTwoTabs();
    mountComposer();
    // 预置 s2 草稿（setDraftText 缺省写当前活跃 Tab）
    switchTab("s2");
    act(() => {
      useRun.getState().setDraftText("草稿 B");
    });
    switchTab("s1");
    const ta = getTextarea();
    fireEvent.change(ta, { target: { value: "发送我" } });
    fireKey(ta, "Enter");
    await waitFor(() => expect(ipc.startChat).toHaveBeenCalledWith("s1", "发送我", [], expect.anything()));
    await waitFor(() => expect(ta.value).toBe("")); // s1 发送接受后清空
    expect(useRun.getState().drafts["s2"]!.text).toBe("草稿 B"); // s2 原样
  });

  it("D4 关 Tab 草稿随 dispose 丢弃：closeTab 删平行桶，重开会话草稿为空", () => {
    seedTwoTabs();
    act(() => {
      useRun.getState().setDraftText("将被丢弃");
    });
    act(() => {
      useSessions.getState().closeTab("s1");
    });
    expect(useRun.getState().tabs["s1"]).toBeUndefined();
    expect(useRun.getState().drafts["s1"]).toBeUndefined();
    act(() => {
      useRun.getState().initTab("s1"); // 模拟重开会话（草稿桶惰性创建）
    });
    expect(useRun.getState().drafts["s1"]).toBeUndefined(); // 回退共享空草稿，composer 为空
  });

  it("D5 切 Tab 收起提及菜单：@ 候选不残留进新会话", async () => {
    seedTwoTabs();
    mountComposer();
    const ta = getTextarea();
    fireEvent.change(ta, { target: { value: "@" } });
    await waitFor(() => expect(document.querySelector(".ant-popover:not(.ant-popover-hidden)")).toBeTruthy());
    switchTab("s2");
    // happy-dom 不执行 CSS 动画：rc-motion 的 leave 分阶段推进（prepare → active → 结束事件），
    // 在轮询里反复派发 motion 结束事件，直到 leave 完成落下 hidden 态
    await waitFor(() => {
      document.querySelectorAll(".ant-popover").forEach((el) => {
        fireEvent.transitionEnd(el);
        fireEvent.animationEnd(el);
      });
      expect(document.querySelector(".ant-popover:not(.ant-popover-hidden)")).toBeFalsy();
    });
  });

  it("D6 队列编辑回填只落点击所在 Tab：文本与附件进 s1 草稿，s2 不受染", async () => {
    seedTwoTabs();
    mountComposer();
    act(() => {
      useRun.setState((s) => {
        s.tabs["s1"]!.queue = [{ id: "q1", text: "队列任务", images: [{ mime: "image/png", data: "AAAA" }] }];
      });
    });
    act(() => {
      useRun.getState().editQueueItem("s1", "q1");
    });
    await waitFor(() => expect(getTextarea().value).toBe("队列任务"));
    await waitFor(() => expect(document.querySelector(".attach-chip")).toBeTruthy());
    switchTab("s2");
    expect(getTextarea().value).toBe("");
    expect(document.querySelector(".attach-chip")).toBeFalsy();
    switchTab("s1");
    expect(getTextarea().value).toBe("队列任务");
  });

  it("D7 消息「修改」fill：带图回显缩略图，无图覆盖清空现有附件", async () => {
    seedTwoTabs();
    mountComposer();
    const ta = getTextarea();
    act(() => {
      window.dispatchEvent(
        new CustomEvent("ws:composer-fill", { detail: { text: "修改我", images: [{ mediaType: "image/png", data: "AAAA" }] } }),
      );
    });
    await waitFor(() => expect(ta.value).toBe("修改我"));
    await waitFor(() => expect(document.querySelector(".attach-chip")).toBeTruthy());
    act(() => {
      window.dispatchEvent(new CustomEvent("ws:composer-fill", { detail: { text: "改这段" } }));
    });
    await waitFor(() => expect(ta.value).toBe("改这段"));
    await waitFor(() => expect(document.querySelector(".attach-chip")).toBeFalsy()); // 无图 fill 清空附件
  });

  it("D8 技能「使用」insert（updater 形态）落活跃 Tab：s1 追加，s2 无草稿桶", () => {
    seedTwoTabs();
    mountComposer();
    const ta = getTextarea();
    fireEvent.change(ta, { target: { value: "前缀" } });
    act(() => {
      window.dispatchEvent(new CustomEvent("ws:composer-insert", { detail: { text: "/repo-index" } }));
    });
    expect(ta.value).toBe("前缀 /repo-index"); // 空格分隔防粘连
    expect(useRun.getState().drafts["s2"]).toBeUndefined(); // 未输入过的 Tab 不建桶
    switchTab("s2");
    expect(ta.value).toBe("");
  });

  it("D9 ask 弹出覆盖输入区、回答后草稿原样恢复", () => {
    seedTwoTabs();
    mountComposer();
    const ta = getTextarea();
    fireEvent.change(ta, { target: { value: "ask前的草稿" } });
    act(() => {
      useRun.setState((s) => {
        s.tabs["s1"]!.ask = { askId: "a1", kind: "ask", title: "问题", questions: [] } as any;
      });
    });
    expect(screen.queryByPlaceholderText(/CodeWave/)).toBeNull(); // 提问卡覆盖输入区
    act(() => {
      useRun.setState((s) => {
        s.tabs["s1"]!.ask = null; // ask:closed
      });
    });
    expect(getTextarea().value).toBe("ask前的草稿"); // 草稿存 store 桶，子树卸载不丢
  });

  it("D10 队列出队不触碰草稿：runQueueNext 走定向 send，活跃 Tab 草稿保留", async () => {
    seedTwoTabs();
    mountComposer();
    act(() => {
      useRun.getState().setDraftText("草稿 A");
    });
    act(() => {
      useRun.setState((s) => {
        s.tabs["s1"]!.queue = [{ id: "q1", text: "队列任务" }];
      });
    });
    await act(async () => {
      await useRun.getState().runQueueNext("s1");
    });
    expect(ipc.startChat).toHaveBeenCalledWith("s1", "队列任务", [], expect.anything());
    expect(useRun.getState().drafts["s1"]!.text).toBe("草稿 A");
    expect(useRun.getState().drafts["s2"]).toBeUndefined();
  });
});
