import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { act, cleanup, fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import { App } from "antd";
import "../i18n";
import { ipc } from "../ipc/client";
import { useActivity } from "../stores/activity";
import { useRun } from "../stores/run";
import { useSessions } from "../stores/sessions";
import { useUi } from "../stores/ui";
import RuntimeDashboard from "../features/dashboard/RuntimeDashboard";
import type { ActivitySnapshot, ActivitySub } from "../ipc/types";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn(async () => null), Channel: class {} }));
const sub: ActivitySub = { sub_id: "sub1", role: "tester", name: "Checks", task: "Verify restore isolation", step: 18, max_steps: 90, detail: "Running checks", last_tool: "read", status: "running", ended: null, report: null };
const makeSnapshot = (): ActivitySnapshot => ({
  revision: 1, sampled_at_ms: 10000,
  sessions: [
    { session_id: "waiting", run_id: "rw", title: "Waiting session", project_id: null, started_at_ms: 1000, activity: "Waiting for replies", waiting: [
      { ask_id: "a", kind: "approval", title: "Approve write", detail: "file" },
      { ask_id: "b", kind: "ask", title: "Choose approach", detail: "question" },
    ], subs: [] },
    { session_id: "closed", run_id: "rc", title: "Closed Tab session", project_id: null, started_at_ms: 2000, activity: "Checking code", waiting: [], subs: [sub] },
  ], recent: [],
});
function click(text: string) {
  const button = Array.from(document.querySelectorAll("button")).find((b) => b.textContent?.replace(/\s/g, "") === text.replace(/\s/g, ""));
  if (!button) throw new Error(`missing button ${text}`);
  fireEvent.click(button);
}

beforeEach(() => {
  useActivity.getState().setActive(false);
  useActivity.setState({ snapshot: makeSnapshot(), error: null, lastSyncedAt: 10000, expanded: [], recentOpen: false });
  useSessions.setState({ tabs: [], activeKey: "different", sessions: [], projects: [] });
  useRun.setState({ tabs: {}, drafts: {} });
  useUi.setState({ dashboardOpen: true });
  vi.spyOn(ipc, "getActivitySnapshot").mockImplementation(async () => useActivity.getState().snapshot!);
});
afterEach(() => { cleanup(); useActivity.getState().setActive(false); useUi.setState({ dashboardOpen: false }); vi.restoreAllMocks(); });
const mount = () => render(<App><RuntimeDashboard /></App>);

describe("runtime dashboard", () => {
  it("counts the independent snapshot and lists waiting sessions once even without open Tabs", async () => {
    mount();
    await waitFor(() => expect(useActivity.getState().refreshing).toBe(false));
    expect(screen.getAllByText("Waiting session")).toHaveLength(1);
    expect(screen.getByText("Closed Tab session")).toBeTruthy();
    const counters = Array.from(document.querySelectorAll(".ant-statistic-content-value")).map((n) => n.textContent);
    expect(counters).toEqual(["2", "2", "1"]);
    expect(screen.queryByText("主代理")).toBeNull();
    expect(useSessions.getState().activeKey).toBe("different");
    expect(useRun.getState().tabs).toEqual({});
  });

  it("preserves expansion across sections and refresh, with detail bound to the target session", async () => {
    mount();
    fireEvent.click(screen.getByText("Waiting session"));
    fireEvent.click(screen.getByText("Closed Tab session"));
    await waitFor(() => expect(useActivity.getState().expanded).toEqual(["waiting:rw", "closed:rc"]));
    expect(screen.getAllByText("主代理")).toHaveLength(2);
    click("查看详情");
    await waitFor(() => expect(document.querySelector(".ant-drawer-content-wrapper")?.getAttribute("style")).toContain("50vw"));
    const drawer = within(document.querySelector(".ant-drawer-body") as HTMLElement);
    expect(drawer.getByText("Verify restore isolation")).toBeTruthy();
    expect(drawer.getByText("18 / 90")).toBeTruthy();
    act(() => {
      const next = makeSnapshot(); next.revision = 2;
      next.sessions[1].subs[0] = { ...sub, detail: "Updated target activity", report: "Target report" };
      useActivity.setState({ snapshot: next });
    });
    expect(screen.getByText("Target report")).toBeTruthy();
    expect(useActivity.getState().expanded).toEqual(["waiting:rw", "closed:rc"]);
    const mask = document.querySelector(".ant-drawer-mask")!;
    fireEvent.click(screen.getByText("Target report"));
    expect(document.querySelector(".ant-drawer-open")).toBeTruthy();
    fireEvent.click(mask);
    await waitFor(() => expect(document.querySelector(".ant-drawer-open")).toBeNull());
  });

  it("keeps recent results folded and user cancellation neutral", async () => {
    const next = makeSnapshot();
    next.recent = [{ id: "old", kind: "subagent", session_id: "parent", run_id: "old-run", title: "Stopped child", project_id: null, status: "cancelled", ended: null, finished_at_ms: 9000,
      sub: { ...sub, status: "cancelled", report: "Saved work" } }];
    useActivity.setState({ snapshot: next });
    mount();
    expect(screen.queryByText("Stopped child")).toBeNull();
    fireEvent.click(screen.getByText("最近结束（1）"));
    await waitFor(() => expect(screen.getByText("Stopped child")).toBeTruthy());
    expect(screen.getByText("用户停止").closest(".ant-tag")?.className).not.toMatch(/warning|error/);
    click("查看详情");
    await waitFor(() => expect(screen.getByText("parent")).toBeTruthy());
    expect(screen.getByText("Saved work")).toBeTruthy();
  });
});
