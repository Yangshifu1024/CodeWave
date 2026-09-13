// TopBar content contract ([docs/titlebar-content-batch](../../../docs/titlebar-content-batch.md) two-segment; [docs/titlebar-logo-toggle](../../../docs/titlebar-logo-toggle.md) Logo toggles the left bar + history nav removed):
// render TopBar directly + craft data via store setState (invoke returns [] everywhere, covering unexpected calls during mount).
import { describe, it, expect, vi, afterEach } from "vitest";
import { render, cleanup, fireEvent } from "@testing-library/react";
import { App } from "antd";
import "../i18n";
import TopBar, { SIDER_W_CLOSED, SIDER_W_OPEN } from "../features/shell/TopBar";
import { useSessions, type Tab } from "../stores/sessions";
import { useRun } from "../stores/run";
import { useUi } from "../stores/ui";
import { DEFAULT_PREFS } from "../ipc/types";

vi.mock("@tauri-apps/api/core", () => ({
  invoke: vi.fn(async () => []),
}));
vi.mock("@tauri-apps/api/event", () => ({
  listen: vi.fn(async () => vi.fn()),
}));
vi.mock("@tauri-apps/api/window", () => ({
  getCurrentWindow: () => ({ setFocus: vi.fn() }),
}));

function tab(partial: Partial<Tab> & { sessionId: string; title: string }): Tab {
  return {
    key: partial.sessionId,
    workspace: "/tmp/ws",
    projectId: null,
    createdAt: "2026-09-03T09:00:00Z",
    prefs: { ...DEFAULT_PREFS },
    ...partial,
  };
}

function seedRun(sessionId: string, gitEntries: { repo: boolean; entries: any[]; branch?: string | null } | null) {
  // initTab fallback creates the bucket: avoids brittleness when run.ts fields change (review 🟡4)
  if (!useRun.getState().tabs[sessionId]) useRun.getState().initTab(sessionId);
  useRun.setState((s) => {
    s.tabs[sessionId].gitEntries = gitEntries;
  });
}

function renderBar() {
  return render(
    <App>
      <TopBar />
    </App>,
  );
}

afterEach(() => {
  cleanup();
  useSessions.setState({ tabs: [], activeKey: null, projects: [], explorerOpen: true });
  useRun.setState((s) => {
    s.tabs = {}; s.drafts = {};
  });
  useUi.setState({ rightBarOpen: true });
  localStorage.removeItem("ws_right_bar_open");
  localStorage.removeItem("ws_explorer_open");
});

describe("顶栏两段式布局（docs/titlebar-content-batch）", () => {
  it("两段容器与中段弹性区都带 drag-region；无活动会话显示占位标题、无胶囊", () => {
    renderBar();
    for (const sel of [".tb-left-seg", ".tb-main-seg", ".tb-flex"]) {
      expect(document.querySelector(sel)?.hasAttribute("data-tauri-drag-region")).toBe(true);
    }
    expect(document.querySelector(".tb-title-empty")?.textContent).toBe("CodeWave");
    expect(document.querySelectorAll(".tb-pill")).toHaveLength(0);
  });

  it("左段宽度与左栏 Sider 对齐：展开 280 / 折叠 0 完全隐藏（docs/sidebar-collapse-animation-and-titlebar-blend 窄轨退役）；Logo 开关位于右段段首（docs/titlebar-logo-right-segment），仍是左栏开合入口", () => {
    const { rerender } = renderBar();
    const left = () => document.querySelector(".tb-left-seg") as HTMLElement;
    const toggle = () => document.querySelector(".tb-main-seg .tb-logo-toggle") as HTMLButtonElement;
    expect(left().style.width).toBe(`${SIDER_W_OPEN}px`);
    expect(left().classList.contains("tb-left-closed")).toBe(false);
    expect(document.querySelector(".tb-main-seg")!.classList.contains("tb-main-cleared")).toBe(false);
    expect(toggle().getAttribute("aria-label")).toBe("折叠左侧栏");
    // the [docs/titlebar-content-batch](../../../docs/titlebar-content-batch.md) ←/→ session history nav was removed; the left segment is a pure background band ([docs/titlebar-logo-right-segment](../../../docs/titlebar-logo-right-segment.md))
    expect(document.querySelector(".tb-nav")).toBeFalsy();

    useSessions.setState({ explorerOpen: false });
    rerender(<App><TopBar /></App>);
    expect(left().style.width).toBe(`${SIDER_W_CLOSED}px`);
    // [docs/sidebar-collapse-animation-and-titlebar-blend](../../../docs/sidebar-collapse-animation-and-titlebar-blend.md): collapsed decor classes flip on — background melts into the content color, traffic-light yield migrates to the right segment
    expect(left().classList.contains("tb-left-closed")).toBe(true);
    expect(document.querySelector(".tb-main-seg")!.classList.contains("tb-main-cleared")).toBe(true);
    expect(toggle().getAttribute("aria-label")).toBe("展开左侧栏");
  });

  it("Logo 点击翻转左栏开合并同步 localStorage 记忆", () => {
    renderBar();
    const toggle = () => document.querySelector(".tb-main-seg .tb-logo-toggle") as HTMLButtonElement;
    fireEvent.click(toggle());
    expect(useSessions.getState().explorerOpen).toBe(false);
    expect(localStorage.getItem("ws_explorer_open")).toBe("0");
    fireEvent.click(toggle());
    expect(useSessions.getState().explorerOpen).toBe(true);
    expect(localStorage.getItem("ws_explorer_open")).toBe("1");
  });

  it("项目会话：标题 + 工作目录胶囊（basename + 完整路径 title）+ 分支胶囊", () => {
    useSessions.setState({
      tabs: [tab({ sessionId: "s1", title: "界面配色调整", projectId: "p1", workspace: "D:\\Code\\CodeWave" })],
      activeKey: "s1",
    });
    seedRun("s1", { repo: true, entries: [], branch: "master" });
    renderBar();
    expect(document.querySelector(".tb-title")?.textContent).toBe("界面配色调整");
    const pills = [...document.querySelectorAll(".tb-pill")];
    expect(pills).toHaveLength(2);
    expect(pills[0].querySelector(".tb-pill-text")?.textContent).toBe("CodeWave");
    expect(pills[0].getAttribute("title")).toBe("D:\\Code\\CodeWave");
    expect(pills[1].querySelector(".tb-pill-text")?.textContent).toBe("master");
  });

  it("临时会话胶囊文案；非 Git 仓库不渲染分支胶囊", () => {
    useSessions.setState({
      tabs: [tab({ sessionId: "t1", title: "临时会话", projectId: null })],
      activeKey: "t1",
    });
    seedRun("t1", { repo: false, entries: [] });
    renderBar();
    const pills = [...document.querySelectorAll(".tb-pill")];
    expect(pills).toHaveLength(1);
    expect(pills[0].textContent).toContain("临时会话");
  });
});

describe("右栏开合切换按钮", () => {
  it("点击在展开/折叠间翻转并同步 localStorage 记忆", () => {
    renderBar();
    const btn = () =>
      document.querySelector('button[aria-label="折叠右侧栏"], button[aria-label="展开右侧栏"]') as HTMLButtonElement;
    expect(btn().getAttribute("aria-label")).toBe("折叠右侧栏");
    fireEvent.click(btn());
    expect(useUi.getState().rightBarOpen).toBe(false);
    expect(localStorage.getItem("ws_right_bar_open")).toBe("0");
    fireEvent.click(btn());
    expect(useUi.getState().rightBarOpen).toBe(true);
  });
});
