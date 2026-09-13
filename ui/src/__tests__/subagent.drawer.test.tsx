// Subagent interaction batch ([docs/subagent-interaction-drawer](../../../docs/subagent-interaction-drawer.md)) component-level tests: chat card single-row clickable / process drawer render and close-without-destroy / Composer run indicator.
import { describe, it, expect, vi, beforeEach, afterEach } from "vitest";
import { render, screen, fireEvent, cleanup } from "@testing-library/react";
import { App as AntApp } from "antd";
import "../i18n"; // directly mounted components must explicitly init i18next (no global entry outside App.tsx)

vi.mock("@tauri-apps/api/core", () => ({
  invoke: vi.fn(async () => null),
  Channel: class {
    onmessage: any = null;
  },
}));

import { invoke } from "@tauri-apps/api/core";
import SubagentItemCard from "../features/subagent/SubagentItemCard";
import SubagentDrawer from "../features/subagent/SubagentDrawer";
import Composer from "../features/chat/Composer";
import { useRun, type SubView, type SubStream } from "../stores/run";
import { useSessions } from "../stores/sessions";
import { useSettings } from "../stores/settings";

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

function seedTab(session: string, subs: SubView[], streams: Record<string, SubStream>, drawer: { open: boolean; subId: string | null }) {
  useRun.setState((s) => {
    s.tabs[session] = {
      items: [{ kind: "assistant", timeline: [{ kind: "sub", subId: subs[0]?.subId ?? "sub_1" }], toolsMap: {}, streaming: false }] as any,
      running: true,
      streamGen: 0,
      ask: null,
      breakdown: null,
      todos: [],
      suggestions: [],
      subs, subStreams: streams, subDrawer: drawer,
      gitEntries: null, writeTick: 0, queue: [], pendingItemId: null, draftFromQueue: null, lastDoneRunId: null, compacting: false,
    };
  });
  useSessions.setState({
    tabs: [{
      key: session, sessionId: session, workspace: "/tmp/ws", title: "t",
      projectId: null, createdAt: "2026-09-01T00:00:00Z",
      prefs: { approval_mode: "auto_edit", model_id: null, reasoning_effort: null },
    }],
    activeKey: session,
  });
}

beforeEach(() => {
  useRun.setState({ tabs: {} });
});

afterEach(() => {
  cleanup();
  useRun.setState({ tabs: {} });
});

describe("SubagentItemCard（docs/subagent-interaction-drawer）", () => {
  it("单行渲染「子智能体 <名称> · <任务>」与状态徽标，点击打开过程抽屉", () => {
    seedTab("s1", [subView()], { sub_1: subStream() }, { open: false, subId: null });
    render(<SubagentItemCard subId="sub_1" />);
    const card = document.querySelector(".sub-card") as HTMLElement;
    expect(card).not.toBeNull();
    expect(card.textContent).toContain("子智能体");
    expect(card.textContent).toContain("explore");
    expect(card.textContent).toContain("探索代码");
    expect(card.textContent).toContain("3/25");
    fireEvent.click(card);
    expect(useRun.getState().tabs["s1"]!.subDrawer).toEqual({ open: true, subId: "sub_1" });
  });

  it("结束后仍可点击（归档回看）", () => {
    seedTab("s1", [subView({ status: "done" })], { sub_1: subStream({ status: "done" }) }, { open: false, subId: null });
    render(<SubagentItemCard subId="sub_1" />);
    fireEvent.click(document.querySelector(".sub-card") as HTMLElement);
    const drawer = useRun.getState().tabs["s1"]!.subDrawer;
    expect(drawer).toEqual({ open: true, subId: "sub_1" });
  });
});

describe("SubagentDrawer（docs/subagent-interaction-drawer）", () => {
  it("渲染头部（左上角关闭 + 名称 · 任务 + 状态）与任务块/过程流", () => {
    seedTab("s1", [subView()], { sub_1: subStream() }, { open: true, subId: "sub_1" });
    render(
      <AntApp>
        <SubagentDrawer />
      </AntApp>,
    );
    const head = document.querySelector(".sub-drawer-head") as HTMLElement;
    expect(head).not.toBeNull();
    // close button comes first in the header (top-left)
    expect((head.firstChild as HTMLElement).className).toContain("sub-drawer-close");
    expect(head.textContent).toContain("子智能体");
    expect(head.textContent).toContain("explore");
    expect(head.textContent).toContain("探索代码");
    expect(head.textContent).toContain("运行中");
    const body = document.querySelector(".sub-drawer-body") as HTMLElement;
    expect(body.textContent).toContain("调研标题栏实现");
    expect(body.textContent).toContain("子代理过程文本");
  });

  it("关闭不销毁：close 后流数据保留，可再次打开", () => {
    seedTab("s1", [subView()], { sub_1: subStream() }, { open: true, subId: "sub_1" });
    render(
      <AntApp>
        <SubagentDrawer />
      </AntApp>,
    );
    fireEvent.click(document.querySelector(".sub-drawer-close")!.firstElementChild as HTMLElement);
    const t = useRun.getState().tabs["s1"]!;
    expect(t.subDrawer.open).toBe(false);
    expect(t.subStreams.sub_1.timeline).toHaveLength(1);
  });

  it("分配的任务默认收起为单行预览，点击标签/预览可展开与收起（docs/subdrawer-scroll-hardening §6）", () => {
    seedTab("s1", [subView()], { sub_1: subStream() }, { open: true, subId: "sub_1" });
    render(
      <AntApp>
        <SubagentDrawer />
      </AntApp>,
    );
    const toggle = document.querySelector(".sub-drawer-task-toggle") as HTMLElement;
    expect(toggle.getAttribute("aria-expanded")).toBe("false"); // 默认收起
    expect(document.querySelector(".sub-drawer-task-preview")).not.toBeNull();
    expect(document.querySelector(".sub-drawer-task-preview")!.textContent).toContain("调研标题栏实现");
    fireEvent.click(toggle); // 展开
    expect(toggle.getAttribute("aria-expanded")).toBe("true");
    expect(document.querySelector(".sub-drawer-task-preview")).toBeNull();
    expect(document.querySelector(".sub-drawer-task .user-bubble")).not.toBeNull();
    fireEvent.click(toggle); // 收起
    expect(document.querySelector(".sub-drawer-task-preview")).not.toBeNull();
    // 点击预览气泡也可展开
    fireEvent.click(document.querySelector(".sub-drawer-task-preview")!);
    expect(document.querySelector(".sub-drawer-task-preview")).toBeNull();
  });

  it("antd6 DOM 契约探针：面板为 .ant-drawer-section（无 .ant-drawer-content），body 包在 .ant-drawer-body 内且是 section 直下二层", () => {
    // antd 6.6 drawer 已换 @rc-component/drawer 1.4.2：面板类名 content→section，.sub-drawer 直接落在面板本体上，
    // 内容外还有一层 antd .ant-drawer-body 包裹。app.css 滚动链（wrapper 钉视口 → section 定高不滚 → .ant-drawer-body
    // 透传 flex → .sub-drawer-body 唯一滚动体）依赖该结构，antd 再漂移时在此显式失败而非静默回归。
    seedTab("s1", [subView()], { sub_1: subStream() }, { open: true, subId: "sub_1" });
    render(
      <AntApp>
        <SubagentDrawer />
      </AntApp>,
    );
    const root = document.querySelector(".ant-drawer.sub-drawer-root");
    expect(root).not.toBeNull(); // rootClassName：wrapper 钉视口规则（.sub-drawer-root > wrapper）的挂载点
    const wrapper = document.querySelector(".sub-drawer-root > .ant-drawer-content-wrapper") as HTMLElement;
    expect(wrapper).not.toBeNull(); // 与 app.css 钉视口选择器同形，防 DOM 层级漂移使规则脱靶
    const section = document.querySelector(".ant-drawer-section.sub-drawer");
    expect(section).not.toBeNull();
    expect(section!.parentElement).toBe(wrapper);
    expect(document.querySelector(".ant-drawer-content")).toBeNull();
    const antBody = document.querySelector(".ant-drawer-body") as HTMLElement;
    expect(antBody.parentElement).toBe(section); // section 直下 = header + antBody 两层
    const header = document.querySelector(".ant-drawer-section.sub-drawer > .ant-drawer-header") as HTMLElement;
    expect(header).not.toBeNull(); // flex:none 规则与视口钳制的 header 查询都依赖该类名与层级
    expect(header.parentElement).toBe(section);
    const body = document.querySelector(".sub-drawer-body") as HTMLElement;
    expect(body.parentElement).toBe(antBody); // 我们的内容包在 antd body 内
  });
});

describe("Composer 子代理运行指示器（docs/subagent-interaction-drawer）", () => {
  function seedComposerEnv(subs: SubView[]) {
    useSettings.setState({
      config: {
        schema_version: 2,
        providers: [{
          id: "p1", name: "P", api_format: "openai_chat" as const, base_url: "https://api.example.com/v1",
          keys: ["***abcd"], models: [{
            id: "m1", model: "test-model", max_tokens: 32768, context_window: 128000,
            reasoning_effort: null, vision: true, video: false,
          }],
        }],
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
    seedTab("s1", subs, Object.fromEntries(subs.map((s) => [s.subId, subStream({ status: s.status })])), { open: false, subId: null });
  }

  it("无运行中子代理时指示器隐藏", () => {
    seedComposerEnv([subView({ status: "done" })]);
    render(
      <AntApp>
        <Composer />
      </AntApp>,
    );
    expect(document.querySelector(".subs-indicator")).toBeNull();
  });

  it("单个运行中：显示机器人 + 数量 1，点击直达抽屉", () => {
    seedComposerEnv([subView()]);
    render(
      <AntApp>
        <Composer />
      </AntApp>,
    );
    const btn = document.querySelector(".subs-indicator") as HTMLElement;
    expect(btn).not.toBeNull();
    expect(btn.textContent).toContain("1");
    fireEvent.click(btn);
    expect(useRun.getState().tabs["s1"]!.subDrawer).toEqual({ open: true, subId: "sub_1" });
  });

  it("多个运行中：数量为 2，向上菜单选择后打开对应抽屉", () => {
    seedComposerEnv([subView(), subView({ subId: "sub_2", role: "backend-dev", name: "backend-dev", description: "实现功能" })]);
    render(
      <AntApp>
        <Composer />
      </AntApp>,
    );
    const btn = document.querySelector(".subs-indicator") as HTMLElement;
    expect(btn.textContent).toContain("2");
    fireEvent.click(btn);
    // antd Dropdown menu mounts on body (pitfall list: find menu-item by text, then click)
    const items = document.querySelectorAll(".ant-dropdown-menu-item");
    expect(items.length).toBe(2);
    const dev = Array.from(items).find((el) => el.textContent?.includes("dev"));
    fireEvent.click(dev!);
    expect(useRun.getState().tabs["s1"]!.subDrawer).toEqual({ open: true, subId: "sub_2" });
  });
});

// ---------- 停止按钮（[docs/subagent-file-isolation]）：运行中右侧单独停止，主代理收到 E_SUBAGENT_STOPPED 询问是否重派 ----------

describe("SubagentItemCard 停止按钮", () => {
  afterEach(() => {
    cleanup();
    useRun.setState({ tabs: {} });
  });

  it("运行中显示停止按钮：点击单独停止该子代理（stop_subagent），且不打开抽屉", () => {
    seedTab("s1", [subView()], { sub_1: subStream() }, { open: false, subId: null });
    render(<SubagentItemCard subId="sub_1" />);
    const stopBtn = document.querySelector(".sub-card-stop") as HTMLElement;
    expect(stopBtn).not.toBeNull();
    fireEvent.click(stopBtn);
    expect(vi.mocked(invoke)).toHaveBeenCalledWith("stop_subagent", { sessionId: "s1", subId: "sub_1" });
    // stopPropagation：卡片 onClick 不触发（抽屉保持关闭）
    expect(useRun.getState().tabs["s1"]!.subDrawer).toEqual({ open: false, subId: null });
  });

  it("结束后不显示停止按钮；卡片点击仍打开抽屉", () => {
    seedTab("s1", [subView({ status: "done" })], { sub_1: subStream({ status: "done" }) }, { open: false, subId: null });
    render(<SubagentItemCard subId="sub_1" />);
    expect(document.querySelector(".sub-card-stop")).toBeNull();
    fireEvent.click(document.querySelector(".sub-card") as HTMLElement);
    expect(useRun.getState().tabs["s1"]!.subDrawer).toEqual({ open: true, subId: "sub_1" });
  });
});
