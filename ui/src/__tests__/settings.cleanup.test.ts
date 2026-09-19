// 配置保存链路上的清理结果透传（[docs/session-cleanup](../../docs/session-cleanup.md) §3 第 12/16 条）：
// 设置页要拿「本次清理删了几个会话」来弹确认框/报完成提示，所以 store 的 save 必须带 skipCleanup 参数
// 并把后端返回的清理结果原样交回调用方——既不吞返回值，也不吞异常（保存没成功就不入 store）。
// 惯例：唯一 invoke 入口 ui/src/ipc/client 必须 mock（不直接 mock @tauri-apps/api/core）。
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import type { CleanupOutcome, ConfigState } from "../ipc/types";

const ipcMock = vi.hoisted(() => ({
  saveConfig: vi.fn(async (_config: unknown, _opts?: unknown): Promise<unknown> => null),
  getConfig: vi.fn(async (): Promise<unknown> => null),
  setFontPrefs: vi.fn(async (_sans: string, _mono: string): Promise<void> => {}),
}));

vi.mock("../ipc/client", () => ({ ipc: ipcMock }));

import { useSettings } from "../stores/settings";

function makeConfig(): ConfigState {
  return {
    schema_version: 2,
    providers: [],
    active_model_id: null,
    proxy: null,
    network: { allow_private_network: false },
    compact_threshold: 0.6,
    compact_timeout_seconds: 180,
    approval: { enabled: true, confirm_outside_create: true, confirm_git_push: true, auto_confirm: false, command_allowlist: [] },
    validation: { python: true, rust: true, typescript: true, go: true, json: true },
    ui: { font_size: 15, accent: "cyan", language: "zh-CN", font_sans: "", font_mono: "" },
    custom_prompt: null,
    disabled_skills: [],
    log: { level: "info", session_verbose: false },
    shell: { selection: null },
    sessions: { retention_days: 7 },
  };
}

beforeEach(() => {
  ipcMock.saveConfig.mockReset().mockResolvedValue(null);
  ipcMock.getConfig.mockReset().mockResolvedValue(null);
  ipcMock.setFontPrefs.mockReset().mockResolvedValue(undefined);
  useSettings.setState({ config: null, loaded: false });
});

afterEach(() => {
  useSettings.setState({ config: null, loaded: false });
});

describe("settings.save：清理结果与「跳过清理」透传", () => {
  it("save(config)：不带选项时 skipCleanup 按 false 传，清理结果原样返回给调用方", async () => {
    const outcome: CleanupOutcome = { ids: ["s1", "s2"], deleted: 2, failed: 0 };
    ipcMock.saveConfig.mockResolvedValue(outcome);
    const cfg = makeConfig();

    await expect(useSettings.getState().save(cfg)).resolves.toEqual(outcome);
    expect(ipcMock.saveConfig).toHaveBeenCalledTimes(1);
    expect(ipcMock.saveConfig.mock.calls[0][0]).toBe(cfg);
    expect(ipcMock.saveConfig.mock.calls[0][1]).toBeUndefined();
    expect(useSettings.getState().config).toBe(cfg);
  });

  it("save(config, { skipCleanup: true })：取消清理时配置照常落盘，返回 null（保留期未变/本次跳过）", async () => {
    ipcMock.saveConfig.mockResolvedValue(null);
    const cfg = makeConfig();

    await expect(useSettings.getState().save(cfg, { skipCleanup: true })).resolves.toBeNull();
    expect(ipcMock.saveConfig).toHaveBeenCalledWith(cfg, { skipCleanup: true });
    expect(useSettings.getState().config).toEqual(cfg);
  });

  it("保存失败：异常浮出且配置不入 store（不吞异常、不假装成功）", async () => {
    ipcMock.saveConfig.mockRejectedValue(new Error("保存失败"));
    const cfg = makeConfig();

    await expect(useSettings.getState().save(cfg)).rejects.toThrow("保存失败");
    expect(useSettings.getState().config).toBeNull();
  });

  it("IPC 不可用时的回退默认配置带保留期字段（null = 不清理）", async () => {
    ipcMock.getConfig.mockRejectedValue(new Error("no ipc"));

    await useSettings.getState().load();

    expect(useSettings.getState().config?.sessions).toEqual({ retention_days: null });
  });
});
