// 清理相关的 IPC 封装契约（[docs/session-cleanup](../../docs/session-cleanup.md) §6）：
// 命令名与参数键名必须与后端逐字一致（一个字符改动就是运行时「命令找不到」），
// 返回值原样透传（设置页据此弹确认框/报完成提示），saveConfig 的 skipCleanup 默认为 false。
// 这一层测的就是 client.ts 自己，所以只能 mock 底层的 @tauri-apps/api/core（唯一 invoke 实现）。
import { beforeEach, describe, expect, it, vi } from "vitest";
import type { CleanupOutcome, CleanupPreview, CleanupStatus, ConfigState } from "../ipc/types";

const invokeMock = vi.hoisted(() =>
  vi.fn(async (_cmd: string, _args?: unknown): Promise<unknown> => null),
);

vi.mock("@tauri-apps/api/core", () => ({ invoke: invokeMock }));

import { ipc } from "../ipc/client";

beforeEach(() => {
  invokeMock.mockReset().mockResolvedValue(null);
});

describe("会话清理三个新命令", () => {
  it("previewSessionCleanup：命令名 preview_session_cleanup，参数键 days（可选档位与 null 都原样传）", async () => {
    const preview: CleanupPreview = { count: 3, titles: ["a", "b", "c"], orphan_count: 1 };
    invokeMock.mockResolvedValueOnce(preview);

    await expect(ipc.previewSessionCleanup(7)).resolves.toEqual(preview);
    expect(invokeMock).toHaveBeenLastCalledWith("preview_session_cleanup", { days: 7 });

    await ipc.previewSessionCleanup(null);
    expect(invokeMock).toHaveBeenLastCalledWith("preview_session_cleanup", { days: null });
  });

  it("runSessionCleanup：命令名 run_session_cleanup，无参数，返回被删 id 列表与成败条数", async () => {
    const outcome: CleanupOutcome = { ids: ["s1"], deleted: 1, failed: 0 };
    invokeMock.mockResolvedValueOnce(outcome);

    await expect(ipc.runSessionCleanup()).resolves.toEqual(outcome);
    expect(invokeMock).toHaveBeenLastCalledWith("run_session_cleanup");
  });

  it("getCleanupStatus：命令名 get_cleanup_status，无参数，返回上次清理记录", async () => {
    const status: CleanupStatus = { last_run_at: "2026-09-19T08:00:00Z", last_deleted: 2, last_failed: 1 };
    invokeMock.mockResolvedValueOnce(status);

    await expect(ipc.getCleanupStatus()).resolves.toEqual(status);
    expect(invokeMock).toHaveBeenLastCalledWith("get_cleanup_status");
  });
});

describe("saveConfig 的新签名", () => {
  const cfg = { schema_version: 2 } as ConfigState;

  it("不传选项 → skipCleanup 为 false；传 { skipCleanup: true } 原样透传", async () => {
    await ipc.saveConfig(cfg);
    expect(invokeMock).toHaveBeenLastCalledWith("save_config", { config: cfg, skipCleanup: false });

    await ipc.saveConfig(cfg, { skipCleanup: true });
    expect(invokeMock).toHaveBeenLastCalledWith("save_config", { config: cfg, skipCleanup: true });

    await ipc.saveConfig(cfg, {});
    expect(invokeMock).toHaveBeenLastCalledWith("save_config", { config: cfg, skipCleanup: false });
  });

  it("返回值原样透传：有清理结果给结果，保留期未变/本次跳过给 null", async () => {
    const outcome: CleanupOutcome = { ids: ["s1", "s2"], deleted: 2, failed: 1 };
    invokeMock.mockResolvedValueOnce(outcome);
    await expect(ipc.saveConfig(cfg)).resolves.toBe(outcome);

    invokeMock.mockResolvedValueOnce(null);
    await expect(ipc.saveConfig(cfg)).resolves.toBeNull();
  });
});
