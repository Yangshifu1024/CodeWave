// Left nav session ordering contract: new sessions (not yet checkpointed, existing only in open tabs) go first;
// the inline time column reflects last activity only, blank when inactive (creation time never masquerades as activity).
// vitest loads no CSS, so render the component directly + seed data via useSessions.setState (invoke returns [] across the board, covering mount-time list_scheduled_tasks).
import { describe, it, expect, vi, afterEach } from "vitest";
import { render, cleanup, screen } from "@testing-library/react";
import { App } from "antd";
import ProjectNav from "../features/shell/ProjectNav";
import { useSessions, type Tab } from "../stores/sessions";
import { DEFAULT_PREFS, type ProjectEntry, type SessionMeta } from "../ipc/types";

vi.mock("@tauri-apps/api/core", () => ({
  invoke: vi.fn(async () => []),
}));

function meta(partial: Partial<SessionMeta> & { id: string; title: string }): SessionMeta {
  return {
    workspace: "/tmp/ws",
    model_id: null,
    created_at: "",
    updated_at: "",
    message_count: 0,
    project_id: null,
    roots: ["/tmp/ws"],
    running: false,
    interrupted: null,
    ...partial,
  };
}

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

const project: ProjectEntry = {
  id: "p1",
  name: "项目A",
  directory: "/tmp/ws",
  created_at: "2026-08-01T00:00:00Z",
};

function renderNav(state: {
  tabs?: Tab[];
  sessions?: SessionMeta[];
  projects?: ProjectEntry[];
  activeKey?: string | null;
}) {
  useSessions.setState({
    tabs: state.tabs ?? [],
    sessions: state.sessions ?? [],
    projects: state.projects ?? [],
    activeKey: state.activeKey ?? null,
  });
  return render(
    <App>
      <ProjectNav />
    </App>,
  );
}

function rowTitles(): (string | undefined)[] {
  return [...document.querySelectorAll(".session-nav-row .session-title")].map((e) => e.textContent);
}

afterEach(() => {
  cleanup();
  useSessions.setState({ tabs: [], activeKey: null, sessions: [], projects: [] });
});

describe("左栏会话排序：新会话置顶（更新时间倒序 + 创建时间兜底）", () => {
  it("项目分组：未落盘的新会话排第 1 行且行内时间留空，老会话按最后活跃倒序", () => {
    renderNav({
      projects: [project],
      sessions: [
        meta({ id: "s-old-1", title: "老会话一", project_id: "p1", created_at: "2026-08-30T09:00:00Z", updated_at: "2026-08-30T10:00:00Z" }),
        meta({ id: "s-old-2", title: "老会话二", project_id: "p1", created_at: "2026-08-31T09:00:00Z", updated_at: "2026-08-31T10:00:00Z" }),
      ],
      tabs: [tab({ sessionId: "s-new", title: "新会话", projectId: "p1", createdAt: "2026-09-03T09:00:00Z" })],
      activeKey: "s-new",
    });
    const rows = [...document.querySelectorAll(".session-nav-row")];
    expect(rows.map((r) => r.querySelector(".session-title")?.textContent)).toEqual([
      "新会话",
      "老会话二",
      "老会话一",
    ]);
    const times = rows.map((r) => r.querySelector(".session-time")?.textContent);
    expect(times[0]).toBe(""); // inactive: no time shown, creation time not masquerading as activity
    expect(times[1]).not.toBe("");
    expect(times[2]).not.toBe("");
  });

  it("项目会话超过 5 条：新会话不被「显示更多」折叠（截断截尾部老会话）", () => {
    const sessions = Array.from({ length: 6 }, (_, i) =>
      meta({
        id: `s-${i}`,
        title: `老会话${i}`,
        project_id: "p1",
        updated_at: `2026-08-2${i}T10:00:00Z`,
      }),
    );
    renderNav({
      projects: [project],
      sessions,
      tabs: [tab({ sessionId: "s-new", title: "新会话", projectId: "p1" })],
      activeKey: "s-new",
    });
    const titles = rowTitles();
    expect(titles[0]).toBe("新会话");
    expect(titles).toHaveLength(5); // PREVIEW_COUNT
    expect(titles).toContain("老会话5"); // most recently active old session stays in the preview
    expect(screen.getByText("显示更多")).toBeTruthy();
  });

  it("临时会话区：多个未落盘新会话按创建时间新者在先", () => {
    renderNav({
      tabs: [
        tab({ sessionId: "t1", title: "临时甲", createdAt: "2026-09-03T08:00:00Z" }),
        tab({ sessionId: "t2", title: "临时乙", createdAt: "2026-09-03T09:00:00Z" }),
      ],
      activeKey: "t2",
    });
    expect(rowTitles()).toEqual(["临时乙", "临时甲"]);
  });
});
