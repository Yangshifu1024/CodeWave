// 配置保存链路上的清理结果透传（[docs/sessions-and-projects](../../../docs/sessions-and-projects.md) 第 12/16 条）：
// 设置页要拿「本次清理删了几个会话」来弹确认框/报完成提示，所以 store 的 save 必须带 skipCleanup 参数
// 并把后端返回的清理结果原样交回调用方——既不吞返回值，也不吞异常（保存没成功就不入 store）。
// 惯例：唯一 invoke 入口 ui/src/ipc/client 必须 mock（不直接 mock @tauri-apps/api/core）。
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import type { CleanupOutcome, ConfigSaveResult, ConfigState } from "../ipc/types";

import { configSnapshot, configSaved } from "./configFixtures";
import { rebaseSubmittedDraft } from "../utils/configDraft";

const ipcMock = vi.hoisted(() => ({
  saveConfig: vi.fn(async (_config: unknown, _opts?: unknown): Promise<unknown> => null),
  getConfigOperation: vi.fn(async (_operationId: string): Promise<unknown> => { throw new Error("unknown operation"); }),
  setSubagentModel: vi.fn(async (..._args: unknown[]): Promise<unknown> => null),
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
    post_write_check: { enabled: false, command: "", timeout_seconds: 30, tail_chars: 3000 },
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
  ipcMock.getConfig.mockReset().mockResolvedValue(configSnapshot(makeConfig()));
  ipcMock.getConfigOperation.mockReset().mockRejectedValue(new Error("unknown operation"));
  ipcMock.setSubagentModel.mockReset();
  ipcMock.setFontPrefs.mockReset().mockResolvedValue(undefined);
  useSettings.setState({ config: null, snapshot: null, loaded: false, rolePending: {}, roleErrors: {}, roleRetry: {} });
});

afterEach(() => {
  useSettings.setState({ config: null, snapshot: null, loaded: false, rolePending: {}, roleErrors: {}, roleRetry: {} });
});

describe("settings.save：清理结果与「跳过清理」透传", () => {
  it("save(config)：不带选项时 skipCleanup 按 false 传，清理结果原样返回给调用方", async () => {
    const outcome: CleanupOutcome = { ids: ["s1", "s2"], deleted: 2, failed: 0 };
    const cfg = makeConfig();
    const receipt = configSaved(cfg, outcome);
    ipcMock.saveConfig.mockResolvedValue(receipt);
    await useSettings.getState().load();

    await expect(useSettings.getState().save(cfg)).resolves.toEqual(receipt);
    expect(ipcMock.saveConfig).toHaveBeenCalledTimes(1);
    expect(ipcMock.saveConfig.mock.calls[0][0]).toBe(cfg);
    expect(ipcMock.saveConfig.mock.calls[0][1]).toMatchObject({ expectedVersion: { boot_id: "test-boot", sequence: 0 }, operationId: expect.any(String), credentialEdits: {}, skipCleanup: false });
    expect(useSettings.getState().config).toBe(cfg);
  });

  it("save(config, { skipCleanup: true })：取消清理时配置照常落盘，返回 null（保留期未变/本次跳过）", async () => {
    const cfg = makeConfig();
    const receipt = configSaved(cfg);
    ipcMock.saveConfig.mockResolvedValue(receipt);
    await useSettings.getState().load();

    await expect(useSettings.getState().save(cfg, { skipCleanup: true })).resolves.toEqual(receipt);
    expect(ipcMock.saveConfig).toHaveBeenCalledWith(cfg, expect.objectContaining({ skipCleanup: true }));
    expect(useSettings.getState().config).toEqual(cfg);
  });

  it("保存失败：异常浮出且配置不入 store（不吞异常、不假装成功）", async () => {
    ipcMock.saveConfig.mockRejectedValue(new Error("保存失败"));
    const cfg = makeConfig();
    await useSettings.getState().load();
    const previous = useSettings.getState().config;

    await expect(useSettings.getState().save(cfg)).rejects.toThrow("保存失败");
    expect(useSettings.getState().config).toBe(previous);
  });

  it("IPC 不可用时的回退默认配置带保留期字段（null = 不清理）", async () => {
    ipcMock.getConfig.mockRejectedValue(new Error("no ipc"));

    await useSettings.getState().load();

    expect(useSettings.getState().config?.sessions).toEqual({ retention_days: null });
  });
});

function deferred<T>() {
  let resolve!: (value: T) => void;
  let reject!: (error: unknown) => void;
  const promise = new Promise<T>((res, rej) => { resolve = res; reject = rej; });
  return { promise, resolve, reject };
}

describe("versioned settings receipts and independent writers", () => {
  it("adopts the sanitized backend snapshot rather than the submitted secret-bearing object", async () => {
    await useSettings.getState().load();
    const draft = { ...makeConfig(), custom_prompt: "  submitted  " };
    const receipt = configSaved({ ...draft, custom_prompt: "submitted" });
    ipcMock.saveConfig.mockResolvedValue(receipt);
    const credentialEdits = { p1: { kind: "append" as const, new_keys: ["test-new-key"] } };
    await useSettings.getState().save(draft, { credentialEdits, operationId: "append-op" });
    expect(useSettings.getState().config).toBe(receipt.config);
    expect(useSettings.getState().config).not.toBe(draft);
    expect(ipcMock.saveConfig).toHaveBeenCalledWith(draft, expect.objectContaining({ credentialEdits, operationId: "append-op" }));
  });

  it("different-role responses may arrive backwards without regressing the snapshot", async () => {
    await useSettings.getState().load();
    const first = deferred<ConfigSaveResult>();
    const second = deferred<ConfigSaveResult>();
    ipcMock.setSubagentModel.mockReturnValueOnce(first.promise).mockReturnValueOnce(second.promise);
    const a = useSettings.getState().setSubagentModel("backend-dev", "m1");
    const b = useSettings.getState().setSubagentModel("frontend-dev", "m2");
    expect(useSettings.getState().rolePending).toMatchObject({ "backend-dev": true, "frontend-dev": true });
    const both = { ...configSaved({ ...makeConfig(), subagent_models: { "backend-dev": "m1", "frontend-dev": "m2" } }),
      version: { boot_id: "test-boot", sequence: 2 }, page_version: 0, role_versions: { "backend-dev": 1, "frontend-dev": 1 } };
    second.resolve(both);
    await b;
    first.resolve({ ...configSaved({ ...makeConfig(), subagent_models: { "backend-dev": "m1" } }), page_version: 0 });
    await a;
    expect(useSettings.getState().snapshot).toBe(both);
    expect(useSettings.getState().config?.subagent_models).toEqual({ "backend-dev": "m1", "frontend-dev": "m2" });
    expect(ipcMock.saveConfig).not.toHaveBeenCalled();
  });

  it("serializes a role and preserves its retry choice after failure", async () => {
    await useSettings.getState().load();
    const request = deferred<ConfigSaveResult>();
    ipcMock.setSubagentModel.mockReturnValueOnce(request.promise);
    const saving = useSettings.getState().setSubagentModel("backend-dev", "m1");
    const rejected = expect(saving).rejects.toEqual({ code: "ConfigConflict", message: "changed" });
    await useSettings.getState().setSubagentModel("backend-dev", "m2");
    expect(ipcMock.setSubagentModel).toHaveBeenCalledTimes(1);
    request.reject({ code: "ConfigConflict", message: "changed" });
    await rejected;
    expect(useSettings.getState().config?.subagent_models).toBeUndefined();
    expect(useSettings.getState().rolePending["backend-dev"]).toBe(false);
    expect(useSettings.getState().roleErrors["backend-dev"]).toBe("changed");
    expect(useSettings.getState().roleRetry["backend-dev"]).toBe("m1");
  });

  it("only an explicit load can change boots; late saves cannot switch back", async () => {
    await useSettings.getState().load();
    const pending = deferred<ConfigSaveResult>();
    ipcMock.saveConfig.mockReturnValueOnce(pending.promise);
    const save = useSettings.getState().save(makeConfig());
    const restarted = configSnapshot({ ...makeConfig(), custom_prompt: "new boot" }, { version: { boot_id: "new-boot", sequence: 0 } });
    ipcMock.getConfig.mockResolvedValueOnce(restarted);
    await useSettings.getState().load();
    pending.resolve(configSaved({ ...makeConfig(), custom_prompt: "stale" }));
    await save;
    expect(useSettings.getState().snapshot).toBe(restarted);
    expect(useSettings.getState().adoptSnapshot(configSnapshot(makeConfig(), { version: { boot_id: "unknown", sequence: 999 } }))).toBe(false);
  });

  it("uses the page baseline token even if independent roles advance global sequence", async () => {
    const initial = configSnapshot(makeConfig(), { page_version: 7, version: { boot_id: "test-boot", sequence: 15 } });
    ipcMock.getConfig.mockResolvedValueOnce(initial);
    await useSettings.getState().load();
    ipcMock.saveConfig.mockResolvedValue(configSaved(makeConfig()));
    await useSettings.getState().save(makeConfig());
    expect(ipcMock.saveConfig.mock.calls[0][1]).toMatchObject({ expectedVersion: { boot_id: "test-boot", sequence: 7 } });
  });

  it("reconciles a lost response with its operation receipt instead of repeating a commit", async () => {
    await useSettings.getState().load();
    const receipt = { ...configSaved(makeConfig(), { ids: ["s1"], deleted: 1, failed: 0 }), replayed: true };
    ipcMock.saveConfig.mockRejectedValueOnce(new Error("connection closed"));
    ipcMock.getConfigOperation.mockResolvedValueOnce(receipt);
    await expect(useSettings.getState().save(makeConfig(), { operationId: "lost-response" })).resolves.toBe(receipt);
    expect(ipcMock.saveConfig).toHaveBeenCalledTimes(1);
    expect(ipcMock.getConfigOperation).toHaveBeenCalledWith("lost-response");
    expect(useSettings.getState().snapshot).toBe(receipt);
  });

  it("rejects page conflicts without adopting the submitted draft", async () => {
    await useSettings.getState().load();
    const before = useSettings.getState().config;
    const conflict = { code: "ConfigConflict", message: "Please reload" };
    ipcMock.saveConfig.mockRejectedValueOnce(conflict);
    await expect(useSettings.getState().save({ ...makeConfig(), custom_prompt: "mine" },
      { expectedVersion: { boot_id: "test-boot", sequence: 0 } })).rejects.toBe(conflict);
    expect(useSettings.getState().config).toBe(before);
    expect(ipcMock.getConfigOperation).not.toHaveBeenCalled();
  });

  it("rebase retains in-flight edits while accepting normalized fields and independent role/font state", () => {
    const submitted = { ...makeConfig(), custom_prompt: " submitted ", subagent_models: { "backend-dev": "old" } };
    const current = { ...submitted, log: { level: "debug", session_verbose: true }, ui: { ...submitted.ui, ai_language: "English" } };
    const saved = { ...submitted, custom_prompt: "submitted", subagent_models: { "backend-dev": "new" },
      ui: { ...submitted.ui, font_sans: "Server font" } };
    const merged = rebaseSubmittedDraft(submitted, current, saved);
    expect(merged.custom_prompt).toBe("submitted");
    expect(merged.log).toEqual(current.log);
    expect(merged.ui).toMatchObject({ ai_language: "English", font_sans: "Server font" });
    expect(merged.subagent_models).toEqual(saved.subagent_models);
    expect(submitted.custom_prompt).toBe(" submitted ");
  });
});

describe("late loads and uncertain commits", () => {
  it("does not regress a committed sequence when an earlier same-boot read arrives late", async () => {
    await useSettings.getState().load();
    const staleRead = deferred<ReturnType<typeof configSnapshot>>();
    ipcMock.getConfig.mockReturnValueOnce(staleRead.promise);
    const loading = useSettings.getState().load();
    const committed = configSaved({ ...makeConfig(), custom_prompt: "new" });
    ipcMock.saveConfig.mockResolvedValueOnce(committed);
    await useSettings.getState().save(makeConfig());
    staleRead.resolve(configSnapshot(makeConfig()));
    await loading;
    expect(useSettings.getState().snapshot).toBe(committed);
  });

  it("ignores an older explicit load after a newer load established a new boot", async () => {
    const first = deferred<ReturnType<typeof configSnapshot>>();
    ipcMock.getConfig.mockReturnValueOnce(first.promise);
    const loading = useSettings.getState().load();
    const newest = configSnapshot(makeConfig(), { version: { boot_id: "newest-boot", sequence: 0 } });
    ipcMock.getConfig.mockResolvedValueOnce(newest);
    await useSettings.getState().load();
    first.resolve(configSnapshot(makeConfig()));
    await loading;
    expect(useSettings.getState().snapshot).toBe(newest);
  });

  it("queries a typed OperationUncertain receipt because it can follow a real commit", async () => {
    await useSettings.getState().load();
    const committed = configSaved(makeConfig());
    ipcMock.saveConfig.mockRejectedValueOnce({ code: "OperationUncertain", message: "Worker response lost" });
    ipcMock.getConfigOperation.mockResolvedValueOnce(committed);
    await expect(useSettings.getState().save(makeConfig(), { operationId: "uncertain" })).resolves.toBe(committed);
    expect(ipcMock.getConfigOperation).toHaveBeenCalledWith("uncertain");
  });

  it("retains new provider edits while replacing all read-only credential metadata from the receipt", () => {
    const provider = { id: "p", name: "Old name", api_format: "openai_chat" as const, base_url: "https://example.com",
      keys: ["***old"], credential_ref: "old-generation", credential_count: 1, headers: [], models: [] };
    const submitted = { ...makeConfig(), providers: [provider] };
    const current = { ...submitted, providers: [{ ...provider, name: "Later name" }] };
    const saved = { ...submitted, providers: [{ ...provider, keys: ["***new"], credential_ref: "new-generation", credential_count: 2 }] };
    expect(rebaseSubmittedDraft(submitted, current, saved).providers[0]).toEqual({
      ...saved.providers[0], name: "Later name",
    });
  });
});
