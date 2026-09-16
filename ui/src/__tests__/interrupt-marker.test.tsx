// 会话行中断标记（会话保存与恢复优化 · 批1）：上次异常退出/退出前中止的标记由后端落盘在 SessionMeta.interrupted，
// 左栏会话行展示橙档徽标（需注意；无彩色=默认、红=危险），徽标兼作「清除标记」入口
// （点击 → ipc.clearSessionInterrupt + 本地就地撤标记，不用重拉列表）。
// vitest 不加载 CSS：直接渲染组件 + useSessions.setState 灌数据；ipc 统一 mock ui/src/ipc/client（前端唯一 invoke 入口）。
import { describe, it, expect, vi, afterEach } from "vitest";
import { render, cleanup, fireEvent, waitFor } from "@testing-library/react";
import { App } from "antd";
import "../i18n"; // 断言中文文案：显式初始化 i18next（组件独立挂载路径）
import ProjectNav from "../features/shell/ProjectNav";
import { ipc } from "../ipc/client";
import { useSessions } from "../stores/sessions";
import { useRun } from "../stores/run";
import { useUi } from "../stores/ui";
import type { SessionMeta } from "../ipc/types";

vi.mock("../ipc/client", () => ({
  ipc: {
    listScheduledTasks: vi.fn(async () => []),
    clearSessionInterrupt: vi.fn(async () => undefined),
  },
}));

function meta(partial: Partial<SessionMeta> & { id: string; title: string }): SessionMeta {
  return {
    workspace: "/tmp/ws",
    model_id: null,
    created_at: "2026-09-01T00:00:00Z",
    updated_at: "2026-09-01T00:00:00Z",
    message_count: 0,
    project_id: null,
    roots: ["/tmp/ws"],
    running: false,
    interrupted: null,
    ...partial,
  };
}

const CRASH = { kind: "crash", at: "2026-09-20T01:00:00Z" } as const;
const QUIT = { kind: "quit", at: "2026-09-20T02:00:00Z" } as const;

function renderNav(sessions: SessionMeta[]) {
  useSessions.setState({ tabs: [], sessions, activeKey: null, projects: [], unread: {} });
  return render(
    <App>
      <ProjectNav />
    </App>,
  );
}

/** 按标题定位会话行（行顺序受排序影响，不按索引取） */
function rowByTitle(title: string): HTMLElement {
  const row = Array.from(document.querySelectorAll(".session-nav-row")).find(
    (r) => r.querySelector(".session-title")?.textContent === title,
  );
  expect(row).toBeTruthy();
  return row as HTMLElement;
}

function badgeOf(title: string): HTMLElement {
  const badge = rowByTitle(title).querySelector(".session-interrupt");
  expect(badge).toBeTruthy();
  return badge as HTMLElement;
}

afterEach(() => {
  cleanup();
  useSessions.setState({ tabs: [], activeKey: null, sessions: [], projects: [], unread: {} });
  useRun.setState((s) => {
    s.tabs = {}; s.drafts = {};
  });
  useUi.setState({ tasksOpen: false, closeTabRequest: null });
  vi.clearAllMocks();
});

describe("会话行中断标记（会话保存与恢复优化 · 批1）", () => {
  it("有 interrupted 的会话行渲染徽标，无标记的行不渲染", () => {
    renderNav([
      meta({ id: "s1", title: "上次崩过", interrupted: CRASH }),
      meta({ id: "s2", title: "干净会话" }),
    ]);

    expect(rowByTitle("上次崩过").querySelector(".session-interrupt")).toBeTruthy();
    expect(rowByTitle("干净会话").querySelector(".session-interrupt")).toBeNull();
    expect(document.querySelectorAll(".session-interrupt")).toHaveLength(1);
  });

  it("徽标 title 按中断类型给语义，并写明动作提示", () => {
    renderNav([
      meta({ id: "s1", title: "崩溃会话", interrupted: CRASH }),
      meta({ id: "s2", title: "中止会话", interrupted: QUIT }),
    ]);

    const crash = badgeOf("崩溃会话").getAttribute("title") ?? "";
    expect(crash).toContain("异常退出");
    expect(crash).toContain("清除中断标记"); // 徽标本身即清除入口，title 里说明点击后果
    expect(badgeOf("中止会话").getAttribute("title") ?? "").toContain("正常退出前中止");
  });

  it("点击徽标 → 调 clearSessionInterrupt(sessionId) 并就地撤掉标记", async () => {
    renderNav([meta({ id: "s1", title: "崩溃会话", interrupted: CRASH })]);

    fireEvent.click(badgeOf("崩溃会话"));

    expect(ipc.clearSessionInterrupt).toHaveBeenCalledWith("s1");
    await waitFor(() => expect(document.querySelector(".session-interrupt")).toBeNull());
    await waitFor(() => expect(document.body.textContent).toContain("已清除中断标记"));
  });

  it("清除失败（后端报错）：标记保留，给出失败提示不静默", async () => {
    vi.mocked(ipc.clearSessionInterrupt).mockRejectedValueOnce(new Error("boom"));
    renderNav([meta({ id: "s1", title: "崩溃会话", interrupted: CRASH })]);

    fireEvent.click(badgeOf("崩溃会话"));

    await waitFor(() => expect(document.body.textContent).toContain("清除中断标记失败"));
    expect(document.querySelector(".session-interrupt")).toBeTruthy();
  });
});
