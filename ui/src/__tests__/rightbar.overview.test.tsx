import { afterEach, expect, it, vi } from "vitest";
import { act, cleanup, fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import { App } from "antd";
import "../i18n";
import RightBar from "../features/shell/RightBar";
import { useSessions } from "../stores/sessions";
import { useRun } from "../stores/run";
import type { SubView } from "../stores/run";
import { useUi } from "../stores/ui";
import { COLLAPSED_SECTIONS_KEY } from "../utils/rightbarPrefs";

const { invoke } = vi.hoisted(() => ({ invoke: vi.fn() }));
vi.mock("@tauri-apps/api/core", () => ({ invoke }));
vi.mock("../features/files/FileViewerModal", async importOriginal => ({
  ...await importOriginal<typeof import("../features/files/FileViewerModal")>(),
  default: ({ path }: { path: string | null }) => path ? <div role="dialog">{path}</div> : null,
}));
vi.mock("../features/shell/SkillDetailModal", () => ({ default: ({ skill }: { skill: { name: string } | null }) => skill ? <div role="dialog">{skill.name}</div> : null }));
const artifact = { path: "/ws/report.md", first_op: "create", last_op: "create", first_at: "2026-10-14T00:00:00Z", last_at: "2026-10-14T00:00:00Z", count: 1, exists: true, size: 10 };
const gitFile = { path: "app.ts", status: "modified", additions: 42, deletions: 18, patch: "" };

function seed() {
  localStorage.clear();
  useSessions.setState({ tabs: ["s1", "s2"].map(id => ({ key: id, sessionId: id, workspace: "/ws", title: id, projectId: "p1", createdAt: "2026-10-14T00:00:00Z", prefs: { approval_mode: "auto_edit" as const, model_id: null, reasoning_effort: null } })), activeKey: "s1", projects: [{ id: "p1", name: "CodeWave", directory: "/ws", data_dir: "/ws/.codewave", created_at: "2026-10-14T00:00:00Z" }] });
  useRun.setState({ tabs: {} });
  useRun.getState().initTab("s1");
  useRun.getState().initTab("s2");
  useUi.setState({ rightBarOpen: true, rbTab: "info" });
  invoke.mockImplementation(async (cmd: string) => {
    if (cmd === "git_diff") return { files: [gitFile], truncated: false };
    if (cmd === "list_session_files") return [artifact, { ...artifact, path: "/ws/gone.md", exists: false }];
    if (cmd === "list_skills" || cmd === "list_editors" || cmd === "quota_snapshots" || cmd === "list_log_files") return [];
    if (cmd === "read_session_log") return { content: "", truncated: false };
    return null;
  });
}
const renderBar = () => render(<App><RightBar /></App>);
// happy-dom does not run CSS leave animations; finish rc-motion before asserting portal removal.
async function finishModalClose() {
  await waitFor(() => {
    document.querySelectorAll(".ant-modal, .ant-modal-wrap, .ant-modal-mask").forEach(el => {
      fireEvent.transitionEnd(el);
      fireEvent.animationEnd(el);
    });
    expect(screen.queryByRole("dialog")).toBeNull();
  });
}
afterEach(() => {
  cleanup(); vi.useRealTimers(); vi.resetAllMocks(); localStorage.clear();
  useSessions.setState({ tabs: [], activeKey: null, projects: [] });
  useRun.setState({ tabs: {} });
  useUi.setState({ rightBarOpen: true, rbTab: "info" });
});

it("概览和变更弹窗共用查询结果，手动刷新后汇总同步更新", async () => {
  seed(); renderBar();
  await screen.findByText("+42");
  expect(screen.getByRole("region", { name: "会话概览" })).toBeTruthy();
  fireEvent.click(screen.getByRole("button", { name: "查看工作区变更" }));
  const dialog = await screen.findByRole("dialog", { name: /工作区变更/ });
  expect(within(dialog).getByRole("menuitem", { name: /app.ts/ })).toBeTruthy();
  expect(invoke.mock.calls.filter(([cmd]) => cmd === "git_diff")).toHaveLength(1);
  invoke.mockImplementation(async (cmd: string) => cmd === "git_diff" ? { files: [{ ...gitFile, additions: 10 }], truncated: false } : []);
  fireEvent.click(within(dialog).getByRole("button", { name: "刷新变更" }));
  await waitFor(() => expect(invoke.mock.calls.filter(([cmd]) => cmd === "git_diff")).toHaveLength(2));
  fireEvent.click(within(dialog).getByRole("button", { name: "Close" }));
  await finishModalClose();
  expect((screen.getByRole("radio", { name: "信息" }) as HTMLInputElement).checked).toBe(true);
  await within(screen.getByRole("region", { name: "会话概览" })).findByText("+10");
  expect(invoke.mock.calls.filter(([cmd]) => cmd === "git_diff")).toHaveLength(2);
});

it("顶部只有三项视图，按根目录区分同名文件，重开弹窗保留选择", async () => {
  seed();
  const original = invoke.getMockImplementation()!;
  invoke.mockImplementation(async (cmd: string, args?: unknown) => cmd === "git_diff" ? { files: [
    { ...gitFile, root: "/ws/first", patch: "+first-root-line" },
    { ...gitFile, root: "/ws/second", patch: "+second-root-line" },
  ], truncated: false } : original(cmd, args));
  renderBar();
  const nav = screen.getByRole("radiogroup", { name: "右侧栏视图" });
  expect(within(nav).getAllByRole("radio")).toHaveLength(3);
  expect(within(nav).queryByRole("radio", { name: "变更" })).toBeNull();
  fireEvent.click(screen.getByRole("button", { name: "查看工作区变更" }));
  const dialog = await screen.findByRole("dialog", { name: /工作区变更/ });
  await within(dialog).findByText("+first-root-line");
  fireEvent.click(within(dialog).getAllByRole("menuitem", { name: /app.ts/ })[1]);
  expect(within(dialog).getByText("+second-root-line")).toBeTruthy();
  expect(within(dialog).queryByText("+first-root-line")).toBeNull();
  fireEvent.click(within(dialog).getByRole("button", { name: "Close" }));
  await finishModalClose();
  fireEvent.click(screen.getByRole("button", { name: "查看工作区变更" }));
  expect(within(await screen.findByRole("dialog", { name: /工作区变更/ })).getByText("+second-root-line")).toBeTruthy();
});

it("外部调用变更仍可打开弹窗，关闭后恢复日志视图和原有范围", async () => {
  seed(); renderBar();
  fireEvent.click(screen.getByRole("radio", { name: "日志" }));
  fireEvent.click(screen.getByRole("radio", { name: "全局" }));
  act(() => useUi.getState().showChanges());
  expect(useUi.getState().rbTab).toBe("changes");
  // rc-util uses a fixed test id, which collides with the global-log Select's id.
  const dialog = await screen.findByRole("dialog");
  expect(within(dialog).getByText("工作区变更（vs HEAD）")).toBeTruthy();
  fireEvent.click(within(dialog).getByRole("button", { name: "Close" }));
  await finishModalClose();
  expect((screen.getByRole("radio", { name: "日志" }) as HTMLInputElement).checked).toBe(true);
  expect((screen.getByRole("radio", { name: "全局" }) as HTMLInputElement).checked).toBe(true);
});

it("切换会话或隐藏右栏关闭变更弹窗，返回时不会复活", async () => {
  seed(); renderBar();
  fireEvent.click(screen.getByRole("button", { name: "查看工作区变更" }));
  await screen.findByRole("dialog", { name: /工作区变更/ });
  act(() => useSessions.setState({ activeKey: "s2" }));
  await finishModalClose();
  act(() => useSessions.setState({ activeKey: "s1" }));
  expect(screen.queryByRole("dialog")).toBeNull();
  expect(useUi.getState().rbTab).toBe("info");
  fireEvent.click(screen.getByRole("button", { name: "查看工作区变更" }));
  await screen.findByRole("dialog", { name: /工作区变更/ });
  act(() => useUi.setState({ rightBarOpen: false }));
  await finishModalClose();
  act(() => useUi.setState({ rightBarOpen: true }));
  expect(screen.queryByRole("dialog")).toBeNull();
  expect(useUi.getState().rbTab).toBe("info");
});

it("文件摘要可折叠，复用查看器，删除项禁用，数量进入原文件页签且不切换折叠状态", async () => {
  seed(); renderBar();
  const report = await screen.findByRole("button", { name: "查看 report.md" });
  const header = () => screen.getByText("会话文件").closest(".ant-collapse-header")!;
  expect(header().getAttribute("aria-expanded")).toBe("true");
  expect(screen.queryByRole("button", { name: "查看全部" })).toBeNull();
  fireEvent.click(header());
  expect(header().getAttribute("aria-expanded")).toBe("false");
  expect(useUi.getState().rbTab).toBe("info");
  fireEvent.click(header());
  expect((screen.getByRole("button", { name: "查看 gone.md" }) as HTMLButtonElement).disabled).toBe(true);
  fireEvent.click(report);
  expect(screen.getByRole("dialog").textContent).toBe("/ws/report.md");
  act(() => useSessions.setState({ activeKey: "s2" }));
  expect(screen.queryByRole("dialog")).toBeNull();
  fireEvent.click(await screen.findByRole("button", { name: "文件 2" }));
  expect((screen.getByRole("radio", { name: "文件 2" }) as HTMLInputElement).checked).toBe(true);
  act(() => useUi.setState({ rbTab: "info" }));
  expect(header().getAttribute("aria-expanded")).toBe("true");
});

it("只汇总当前会话子代理，partial/budget 不能算成正常完成，并复用真实过程抽屉", async () => {
  seed();
  const base = { role: "reviewer", description: "审查", step: 1, maxSteps: 10, tokens: 0, lastTools: [] };
  useRun.setState(state => { state.tabs.s1.subs = [{ ...base, subId: "report", status: "done", ended: "report" }, { ...base, subId: "partial", role: "partial-agent", status: "done", ended: "partial" }, { ...base, subId: "budget", role: "budget-agent", status: "done", ended: "budget" }, { ...base, subId: "unknown", role: "unknown-agent", status: "done", ended: "future-ending" as SubView["ended"] }]; state.tabs.s2.subs = [{ ...base, subId: "other", role: "other-session-agent", status: "running" }]; });
  renderBar();
  const agents = screen.getByRole("region", { name: "子代理" });
  expect(within(agents).getByText("1 已完成")).toBeTruthy();
  expect(within(agents).getByText("2 需注意")).toBeTruthy();
  expect(within(agents).getByText("1 异常结束")).toBeTruthy();
  expect(screen.queryByText("other-session-agent")).toBeNull();
  fireEvent.click(screen.getByRole("button", { name: "查看 reviewer 的过程" }));
  expect(useRun.getState().tabs.s1.subDrawer).toEqual({ open: true, subId: "report" });
});

it("新增额度折叠保留技能/计划偏好", async () => {
  seed();
  localStorage.setItem(COLLAPSED_SECTIONS_KEY, JSON.stringify(["plan"]));
  useRun.setState(state => { state.tabs.s1.todos = [{ title: "已完成任务", status: "completed" }]; });
  renderBar();
  const header = (name: string) => screen.getByText(name).closest(".ant-collapse-header")!;
  expect(header("当前计划").getAttribute("aria-expanded")).toBe("false");
  fireEvent.click(header("额度与余额"));
  await waitFor(() => expect(JSON.parse(localStorage.getItem(COLLAPSED_SECTIONS_KEY)!)).toEqual(["plan", "quota"]));
  fireEvent.click(header("当前计划"));
  await screen.findByText("已完成任务");
  expect(document.querySelector(".rb-todo-done")).toBeTruthy();
});

it("技能详情随会话切换关闭，切回原会话不能复活旧弹层", async () => {
  seed();
  const original = invoke.getMockImplementation()!;
  invoke.mockImplementation(async (cmd: string, args?: unknown) => cmd === "list_skills" ? [{ name: "demo", origin: "<builtin>", description: "示例", whenToUse: "" }] : original(cmd, args));
  renderBar();
  fireEvent.click(await screen.findByText("demo"));
  expect(screen.getByRole("dialog").textContent).toBe("demo");
  act(() => useSessions.setState({ activeKey: "s2" }));
  expect(screen.queryByRole("dialog")).toBeNull();
  act(() => useSessions.setState({ activeKey: "s1" }));
  expect(screen.queryByRole("dialog")).toBeNull();
});

it("额度区折叠停止轮询，重新展开只发一次查询", async () => {
  seed(); renderBar();
  await waitFor(() => expect(invoke.mock.calls.filter(([cmd]) => cmd === "quota_snapshots")).toHaveLength(1));
  vi.useFakeTimers();
  fireEvent.click(screen.getByText("额度与余额").closest(".ant-collapse-header")!);
  act(() => { vi.advanceTimersByTime(5 * 60 * 1000); });
  expect(invoke.mock.calls.filter(([cmd]) => cmd === "quota_snapshots")).toHaveLength(1);
  await act(async () => { fireEvent.click(screen.getByText("额度与余额").closest(".ant-collapse-header")!); });
  expect(invoke.mock.calls.filter(([cmd]) => cmd === "quota_snapshots")).toHaveLength(2);
});
