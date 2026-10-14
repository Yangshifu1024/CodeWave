import { afterEach, expect, it, vi } from "vitest";
import { cleanup, fireEvent, render, screen, within } from "@testing-library/react";
import "../i18n";
import ChangesModal from "../features/workspace/ChangesModal";
import type { WorkspaceChanges } from "../features/workspace/useWorkspaceChanges";

const file = { path: "a.ts", status: "modified", additions: 1, deletions: 0, patch: "+first-line" };
const changes = (overrides: Partial<WorkspaceChanges> = {}): WorkspaceChanges => ({
  files: [file], loading: false, error: "", truncated: false, refresh: vi.fn(), ...overrides,
});
const view = (data: WorkspaceChanges, sessionId: string | null = "s1") => <ChangesModal open sessionId={sessionId} changes={data} onClose={vi.fn()} />;
afterEach(cleanup);

it("初次加载不显示无变更，失败明确报错并允许重试", async () => {
  const { rerender } = render(view(changes({ files: null, loading: true })));
  const dialog = await screen.findByRole("dialog");
  expect(within(dialog).queryByText("无变更")).toBeNull();
  expect(dialog.querySelector(".ant-skeleton")).toBeTruthy();
  const retry = vi.fn();
  rerender(view(changes({ files: null, error: "offline", refresh: retry })));
  expect(within(dialog).getByText("offline")).toBeTruthy();
  expect(within(dialog).queryByText("无变更")).toBeNull();
  fireEvent.click(within(dialog).getByRole("button", { name: /重\s*试/ }));
  expect(retry).toHaveBeenCalledOnce();
});

it("无会话、空变更和没有文本 diff 分别显示对应状态", async () => {
  const { rerender } = render(view(changes({ files: null }), null));
  const dialog = await screen.findByRole("dialog");
  expect(within(dialog).getByText("无活动会话")).toBeTruthy();
  expect((within(dialog).getByRole("button", { name: "刷新变更" }) as HTMLButtonElement).disabled).toBe(true);
  rerender(view(changes({ files: [] })));
  expect(within(dialog).getByText("无变更")).toBeTruthy();
  rerender(view(changes({ files: [{ ...file, patch: "" }] })));
  expect(within(dialog).getByRole("menuitem", { name: /a.ts/ })).toBeTruthy();
  expect(within(dialog).getByText("暂无可显示的文本 diff")).toBeTruthy();
  expect(within(dialog).queryByText("无变更")).toBeNull();
});

it("后端截断标记和刷新失败提示不遮掉最近成功的 diff", async () => {
  render(view(changes({ truncated: true, error: "refresh failed" })));
  const dialog = await screen.findByRole("dialog");
  expect(within(dialog).getByText("变更内容过长，当前仅显示部分结果")).toBeTruthy();
  expect(within(dialog).getByText("refresh failed")).toBeTruthy();
  expect(within(dialog).getByText("+first-line")).toBeTruthy();
});

it("文件选择按身份保留，不受刷新排序影响，换会话后重置", async () => {
  const second = { ...file, path: "b.ts", patch: "+second-line" };
  const { rerender } = render(view(changes({ files: [file, second] })));
  const dialog = await screen.findByRole("dialog");
  fireEvent.click(within(dialog).getByRole("menuitem", { name: /b.ts/ }));
  expect(within(dialog).getByText("+second-line")).toBeTruthy();
  rerender(view(changes({ files: [second, file] })));
  expect(within(dialog).getByText("+second-line")).toBeTruthy();
  rerender(view(changes({ files: [file, second] }), "s2"));
  expect(within(dialog).getByText("+first-line")).toBeTruthy();
  rerender(view(changes({ files: [file, second] }), "s1"));
  expect(within(dialog).getByText("+first-line")).toBeTruthy();
});
