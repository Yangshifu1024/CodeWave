// 流式等待指示的 DOM 契约（[docs/chat-loading-indicator](../../../docs/chat-loading-indicator.md)）：
//  ① 聊天消息在「正在输出」时渲染 antd 加载图标（.anticon-loading），且旧的自绘方块字符类名 .cursor 已彻底不存在；
//  ② 子代理过程抽屉的运行中流同样渲染加载图标（两处共用同一个语义化类名 .ws-streaming-indicator）；
//  ③ 非输出状态（streaming 为假 / 子代理非 running）不渲染该指示。
import { describe, it, expect, vi, beforeEach, afterEach } from "vitest";
import { render, cleanup } from "@testing-library/react";
import { App as AntApp } from "antd";
import "../i18n"; // 直接挂载的组件必须自行初始化 i18next（否则 t() 返回原始 key）

vi.mock("@tauri-apps/api/core", () => ({
  invoke: vi.fn(async () => null),
  Channel: class {
    onmessage: ((f: any) => void) | null = null;
  },
}));

import ChatMessages from "../features/chat/ChatMessages";
import SubagentDrawer from "../features/subagent/SubagentDrawer";
import { useRun, type SubStream, type SubView } from "../stores/run";
import { useSessions } from "../stores/sessions";

/** 聊天消息里的等待指示（样式与语义都在这个类名上） */
const indicator = () => document.querySelector(".ws-streaming-indicator");
/** antd 加载图标（@ant-design/icons 的 LoadingOutlined spin 渲染出的类名） */
const spinner = () => document.querySelector(".ws-streaming-indicator .anticon-loading");

function seedChat(streaming: boolean) {
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
      items: [{ kind: "assistant", timeline: [{ kind: "text", text: "正在输出的正文" }], toolsMap: {}, streaming }],
      running: streaming, streamGen: 0, ask: null, breakdown: null,
      todos: [], suggestions: [], subs: [], subStreams: {}, subDrawer: { open: false, subId: null },
      gitEntries: null, writeTick: 0, queue: [], pendingItemId: null, draftFromQueue: null,
      lastDoneRunId: null, compacting: false,
    } as any;
  });
}

function subView(over: Partial<SubView> = {}): SubView {
  return {
    subId: "sub_1", role: "explore", name: "explore", description: "探索代码",
    task: "调研标题栏实现", step: 3, maxSteps: 25, tokens: 1200, lastTools: [],
    status: "running", ...over,
  };
}

function subStream(over: Partial<SubStream> = {}): SubStream {
  return {
    timeline: [{ kind: "text", text: "子代理过程文本" }],
    toolsMap: {}, status: "running", gen: 0, loaded: true, ...over,
  };
}

function seedDrawer(status: SubStream["status"]) {
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
      items: [{ kind: "assistant", timeline: [{ kind: "sub", subId: "sub_1" }], toolsMap: {}, streaming: false }],
      running: status === "running", streamGen: 0, ask: null, breakdown: null,
      todos: [], suggestions: [],
      subs: [subView({ status: status === "running" ? "running" : "done" })],
      subStreams: { sub_1: subStream({ status }) },
      subDrawer: { open: true, subId: "sub_1" },
      gitEntries: null, writeTick: 0, queue: [], pendingItemId: null, draftFromQueue: null,
      lastDoneRunId: null, compacting: false,
    } as any;
  });
}

beforeEach(() => {
  useRun.setState({ tabs: {}, drafts: {} });
});

afterEach(() => {
  cleanup();
  useRun.setState({ tabs: {}, drafts: {} });
  useSessions.setState({ tabs: [], activeKey: null, projects: [] });
});

describe("聊天窗口：等待指示 = antd 加载图标（docs/chat-loading-indicator）", () => {
  it("正在输出：末条助手消息渲染 .anticon-loading，且旧类名 .cursor 已不存在", () => {
    seedChat(true);
    render(<ChatMessages />);
    expect(spinner()).not.toBeNull();
    // 位置语义不变：指示挂在同一条助手的 .msg.assistant 内（正文段之后）
    const msg = document.querySelector(".msg.assistant") as HTMLElement;
    expect(msg).not.toBeNull();
    expect(msg.querySelector(".ws-streaming-indicator")).not.toBeNull();
    // 旧实现（自绘方块字符 + CSS 闪烁）的类名必须彻底消失，防止残留动画回归
    expect(document.querySelector(".cursor")).toBeNull();
  });

  it("已结束（streaming=false）：不渲染任何等待指示", () => {
    seedChat(false);
    render(<ChatMessages />);
    expect(indicator()).toBeNull();
    expect(document.querySelector(".anticon-loading")).toBeNull();
    expect(document.querySelector(".cursor")).toBeNull();
  });

  it("仍在输出但正文为空时不报错（指示照常渲染）", () => {
    seedChat(true);
    useRun.setState((s) => {
      s.tabs["s1"]!.items[0] = { kind: "assistant", timeline: [], toolsMap: {}, streaming: true } as any;
    });
    render(<ChatMessages />);
    expect(spinner()).not.toBeNull();
  });
});

describe("子代理过程抽屉：同一套等待指示（docs/chat-loading-indicator）", () => {
  it("流运行中：过程流末尾渲染 .anticon-loading", () => {
    seedDrawer("running");
    render(
      <AntApp>
        <SubagentDrawer />
      </AntApp>,
    );
    const stream = document.querySelector(".sub-drawer-stream") as HTMLElement;
    expect(stream).not.toBeNull();
    expect(stream.querySelector(".ws-streaming-indicator")).not.toBeNull();
    expect(stream.querySelector(".anticon-loading")).not.toBeNull();
    expect(document.querySelector(".cursor")).toBeNull();
  });

  it("流已结束（status=done）：过程流仍在但不再渲染等待指示", () => {
    seedDrawer("done");
    render(
      <AntApp>
        <SubagentDrawer />
      </AntApp>,
    );
    const stream = document.querySelector(".sub-drawer-stream") as HTMLElement;
    expect(stream).not.toBeNull();
    expect(stream.querySelector(".ws-streaming-indicator")).toBeNull();
    expect(document.querySelector(".anticon-loading")).toBeNull();
  });
});
