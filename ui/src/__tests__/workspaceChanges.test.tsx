import { afterEach, expect, it, vi } from "vitest";
import { act, cleanup, renderHook, waitFor } from "@testing-library/react";
import "../i18n";
import { ipc } from "../ipc/client";
import { useWorkspaceChanges } from "../features/workspace/useWorkspaceChanges";

vi.mock("../ipc/client", () => ({ ipc: { gitDiff: vi.fn() } }));
const gitDiff = vi.mocked(ipc.gitDiff);
const file = { path: "a.ts", status: "modified", additions: 42, deletions: 18, patch: "" };
const response = (path = "a.ts") => ({ files: [{ ...file, path }], truncated: false });

function deferred() {
  let resolve!: (value: ReturnType<typeof response>) => void;
  const promise = new Promise<ReturnType<typeof response>>(r => { resolve = r; });
  return { promise, resolve };
}

afterEach(() => { cleanup(); vi.resetAllMocks(); });

it("只在可见且有会话时请求，隐藏和重新显示不接受过期出参", async () => {
  const old = deferred();
  gitDiff.mockReturnValueOnce(old.promise).mockResolvedValueOnce(response("fresh.ts"));
  const { result, rerender } = renderHook(({ id, visible }) => useWorkspaceChanges(id, visible), { initialProps: { id: null as string | null, visible: true } });
  expect(gitDiff).not.toHaveBeenCalled();
  rerender({ id: "s1", visible: false });
  expect(gitDiff).not.toHaveBeenCalled();
  rerender({ id: "s1", visible: true });
  expect(gitDiff).toHaveBeenCalledTimes(1);
  rerender({ id: "s1", visible: false });
  await act(async () => { old.resolve(response("old.ts")); });
  expect(result.current.files).toBeNull();
  rerender({ id: "s1", visible: true });
  await waitFor(() => expect(result.current.files?.[0].path).toBe("fresh.ts"));
});

it("刷新单飞；切换会话立即屏蔽旧文件，迟到响应也不能覆盖新会话", async () => {
  const first = deferred();
  const stale = deferred();
  const next = deferred();
  gitDiff.mockReturnValueOnce(first.promise).mockReturnValueOnce(stale.promise).mockReturnValueOnce(next.promise);
  const { result, rerender } = renderHook(({ id }) => useWorkspaceChanges(id, true), { initialProps: { id: "s1" } });
  act(() => { result.current.refresh(); result.current.refresh(); });
  expect(gitDiff).toHaveBeenCalledTimes(1);
  await act(async () => { first.resolve(response("s1.ts")); });
  expect(result.current.files?.[0].path).toBe("s1.ts");
  act(() => result.current.refresh());
  rerender({ id: "s2" });
  expect(result.current.files).toBeNull();
  await act(async () => { next.resolve(response("s2.ts")); stale.resolve(response("late-s1.ts")); });
  expect(result.current.files?.[0].path).toBe("s2.ts");
  expect(gitDiff).toHaveBeenLastCalledWith("s2");
});

it("请求失败保持同会话的最近成功结果，错误与真正空变更分开", async () => {
  gitDiff.mockResolvedValueOnce(response()).mockRejectedValueOnce(new Error("offline")).mockResolvedValueOnce({ files: [], truncated: false });
  const { result } = renderHook(() => useWorkspaceChanges("s1", true));
  await waitFor(() => expect(result.current.files).toHaveLength(1));
  act(() => result.current.refresh());
  await waitFor(() => expect(result.current.error).toBe("offline"));
  expect(result.current.files).toHaveLength(1);
  act(() => result.current.refresh());
  await waitFor(() => expect(result.current.files).toEqual([]));
  expect(result.current.error).toBe("");
});

it("非法出参不能伪装成无变更", async () => {
  gitDiff.mockResolvedValueOnce(null as unknown as Awaited<ReturnType<typeof ipc.gitDiff>>);
  const { result } = renderHook(() => useWorkspaceChanges("s1", true));
  await waitFor(() => expect(result.current.error).toContain("无效数据"));
  expect(result.current.files).toBeNull();
});
