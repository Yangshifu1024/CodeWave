import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { ipc } from "../ipc/client";
import type { AskOpenedEvent, ToolResultEvent } from "../ipc/types";
import { useRun } from "../stores/run";
import { blank } from "../stores/runFrames";
import { useSessions } from "../stores/sessions";

vi.mock("@tauri-apps/api/core", () => ({
  invoke: vi.fn(async () => null),
  Channel: class { onmessage: unknown = null; },
}));

vi.mock("../ipc/client", () => ({
  ipc: {
    resolveAsk: vi.fn(async () => {}),
    gitStatus: vi.fn(async () => []),
  },
}));

function deferred() {
  let resolve!: () => void;
  let reject!: (reason: Error) => void;
  const promise = new Promise<void>((res, rej) => { resolve = res; reject = rej; });
  return { promise, resolve, reject };
}

const handlers = () => useRun.getState().bindGlobalHandlers();
const tab = (session = "s1") => useRun.getState().tabs[session]!;

function askEvent(overrides: Partial<AskOpenedEvent> = {}): AskOpenedEvent {
  return {
    session: "s1", ask_id: "a1", kind: "ask",
    plan_body: "## 方案\n实现预览按钮并验证生命周期。",
    can_preview_plan: true,
    questions: [{ id: "q1", question: "是否执行？", options: [{ id: "approve", label: "执行", mode: "auto_edit" }] }],
    ...overrides,
  };
}

function seedTab(session = "s1", askId = "a1", runId = "r1") {
  useRun.setState((s) => {
    s.tabs[session] = blank();
    s.tabs[session].running = true;
    s.tabs[session].runId = runId;
  });
  useRun.getState().restoreAsk(askEvent({ session, ask_id: askId }));
}

function renderResult(overrides: Partial<ToolResultEvent> = {}): ToolResultEvent {
  return {
    session: "s1", run_id: "r1", batch_id: "b1", call_index: 0, call_key: "b1:0",
    tool: "render_html", args_preview: "{}",
    outcome: { ok: true, data: { html: "<h1>预览</h1>" } }, duration_ms: 12,
    ...overrides,
  };
}

function resetStores() {
  useSessions.setState({ tabs: [], activeKey: null, projects: [] });
  useRun.setState((s) => { s.tabs = {}; s.drafts = {}; });
}

beforeEach(() => {
  vi.clearAllMocks();
  vi.mocked(ipc.resolveAsk).mockResolvedValue(undefined);
  vi.spyOn(document, "hasFocus").mockReturnValue(true);
  resetStores();
  seedTab();
  useSessions.setState({ activeKey: "s1" });
});

afterEach(() => {
  resetStores();
  vi.restoreAllMocks();
});

describe("previewPlan request contract", () => {
  it("显式指定会话和 ask，只发送宿主 action 与原备注，不混入选项或切档字段", async () => {
    seedTab("s2", "a2", "r2");
    useSessions.setState({ activeKey: "s2" });
    const note = "  展示窄屏布局\n保留此备注  ";
    const otherTab = tab("s2");

    await useRun.getState().previewPlan("s1", "a1", note);

    expect(ipc.resolveAsk).toHaveBeenCalledExactlyOnceWith("s1", "a1", { action: "preview_plan", note });
    expect(tab().ask).toMatchObject({ askId: "a1", previewPending: true });
    expect(tab().widgetAutoOpen).toEqual({ armed: true, requestId: expect.any(String), askId: "a1", runId: "r1" });
    expect(tab("s2")).toBe(otherTab);
    expect(useSessions.getState().activeKey).toBe("s2");
  });

  it("有 capability 与正文即可预览，不依赖预览选项、批准形标记或成功保存文件", async () => {
    expect(tab().ask?.planFile).toBeNull();
    expect(tab().ask?.approval).toBeUndefined();
    expect(tab().ask?.questions?.[0].options).not.toContainEqual(expect.objectContaining({ id: "preview" }));

    await useRun.getState().previewPlan("s1", "a1");

    expect(ipc.resolveAsk).toHaveBeenCalledExactlyOnceWith("s1", "a1", { action: "preview_plan", note: "" });
  });

  it.each([
    ["缺少 capability", { can_preview_plan: undefined }],
    ["明确禁止 capability", { can_preview_plan: false }],
    ["缺少正文", { plan_body: undefined }],
    ["空正文", { plan_body: "" }],
    ["空白正文", { plan_body: " \n\t " }],
  ] satisfies [string, Partial<AskOpenedEvent>][])('%s 时拒绝请求且不置位', async (_name, overrides) => {
    useRun.getState().restoreAsk(askEvent(overrides));

    await expect(useRun.getState().previewPlan("s1", "a1")).rejects.toThrow();

    expect(ipc.resolveAsk).not.toHaveBeenCalled();
    expect(tab().ask?.previewPending).toBeFalsy();
    expect(tab().widgetAutoOpen).toBeUndefined();
  });

  it("未知会话、已关闭 ask、旧 askId 均不提交，也不重建状态桶", async () => {
    await expect(useRun.getState().previewPlan("missing", "a1")).rejects.toThrow();
    await expect(useRun.getState().previewPlan("s1", "old-ask")).rejects.toThrow();
    handlers()["ask:closed"]({ session: "s1", ask_id: "a1" });
    await expect(useRun.getState().previewPlan("s1", "a1")).rejects.toThrow();

    expect(ipc.resolveAsk).not.toHaveBeenCalled();
    expect(useRun.getState().tabs.missing).toBeUndefined();
    expect(tab().widgetAutoOpen).toBeUndefined();
  });
});

describe("previewPlan single-flight and IPC ordering", () => {
  it("预览在途时拒绝重复预览和普通回答；IPC 成功后同一 ask 仍保持单飞", async () => {
    const pending = deferred();
    vi.mocked(ipc.resolveAsk).mockReturnValueOnce(pending.promise);
    const first = useRun.getState().previewPlan("s1", "a1");

    expect(tab().ask?.previewPending).toBe(true);
    await expect(useRun.getState().previewPlan("s1", "a1")).rejects.toThrow();
    await expect(useRun.getState().resolveAsk("a1", { selections: { q1: ["approve"] } }, "s1")).rejects.toThrow();
    expect(ipc.resolveAsk).toHaveBeenCalledTimes(1);
    pending.resolve();
    await first;
    await expect(useRun.getState().previewPlan("s1", "a1")).rejects.toThrow();
    expect(ipc.resolveAsk).toHaveBeenCalledTimes(1);
  });

  it("普通回答先提交时预览也被单飞门拦住", async () => {
    const pending = deferred();
    vi.mocked(ipc.resolveAsk).mockReturnValueOnce(pending.promise);
    const first = useRun.getState().resolveAsk("a1", { selections: { q1: ["approve"] } }, "s1");

    await expect(useRun.getState().previewPlan("s1", "a1")).rejects.toThrow();
    expect(ipc.resolveAsk).toHaveBeenCalledTimes(1);
    expect(tab().widgetAutoOpen).toBeUndefined();
    pending.resolve();
    await first;
  });

  it("render_html 先于 IPC 回执时仍锁定，成功回执不重复置位", async () => {
    const pending = deferred();
    vi.mocked(ipc.resolveAsk).mockReturnValueOnce(pending.promise);
    const request = useRun.getState().previewPlan("s1", "a1");
    expect(tab().widgetAutoOpen).toMatchObject({ armed: true });

    handlers()["ask:closed"]({ session: "s1", ask_id: "a1" });
    useRun.getState().onToolResult("s1", renderResult(), true);
    expect(tab().widgetAutoOpen).toEqual({ callKey: "b1:0" });
    useRun.getState().consumeWidgetAutoOpen("s1", "b1:0");
    pending.resolve();
    await request;

    expect(tab().widgetAutoOpen).toBeNull();
    expect(tab().ask).toBeNull();
  });

  it("失败回滚当前待请求并允许重试", async () => {
    vi.mocked(ipc.resolveAsk).mockRejectedValueOnce(new Error("preview IPC failed"));

    await expect(useRun.getState().previewPlan("s1", "a1")).rejects.toThrow("preview IPC failed");

    expect(tab().ask?.previewPending).toBe(false);
    expect(tab().widgetAutoOpen).toBeNull();
    await useRun.getState().previewPlan("s1", "a1", "retry");
    expect(ipc.resolveAsk).toHaveBeenCalledTimes(2);
    expect(tab().ask?.previewPending).toBe(true);
    expect(tab().widgetAutoOpen).toMatchObject({ armed: true, requestId: expect.any(String), askId: "a1" });
  });

  it("失败回执不清除已锁定的 widget", async () => {
    const pending = deferred();
    vi.mocked(ipc.resolveAsk).mockReturnValueOnce(pending.promise);
    const request = useRun.getState().previewPlan("s1", "a1");
    const rejected = expect(request).rejects.toThrow("late failure");
    useRun.getState().onToolResult("s1", renderResult(), true);

    pending.reject(new Error("late failure"));
    await rejected;

    expect(tab().widgetAutoOpen).toEqual({ callKey: "b1:0" });
    expect(tab().ask?.previewPending).toBe(false);
  });

  it("旧请求失败不清除新 ask 的 pending 或新预览信号", async () => {
    const oldPending = deferred();
    const newPending = deferred();
    vi.mocked(ipc.resolveAsk).mockReturnValueOnce(oldPending.promise).mockReturnValueOnce(newPending.promise);
    const oldRequest = useRun.getState().previewPlan("s1", "a1");
    const rejected = expect(oldRequest).rejects.toThrow("old failure");
    handlers()["ask:opened"](askEvent({ ask_id: "a2" }));
    const newRequest = useRun.getState().previewPlan("s1", "a2");

    oldPending.reject(new Error("old failure"));
    await rejected;

    expect(tab().ask).toMatchObject({ askId: "a2", previewPending: true });
    expect(tab().widgetAutoOpen).toEqual({ armed: true, requestId: expect.any(String), askId: "a2", runId: "r1" });
    newPending.resolve();
    await newRequest;
  });

  it("切换 Tab 后的失败仅回滚原会话，不动当前 Tab 的请求", async () => {
    const oldPending = deferred();
    vi.mocked(ipc.resolveAsk).mockReturnValueOnce(oldPending.promise);
    const oldRequest = useRun.getState().previewPlan("s1", "a1");
    const rejected = expect(oldRequest).rejects.toThrow("old tab failure");
    seedTab("s2", "a2", "r2");
    useSessions.setState({ activeKey: "s2" });
    await useRun.getState().previewPlan("s2", "a2");
    const otherTab = tab("s2");

    oldPending.reject(new Error("old tab failure"));
    await rejected;

    expect(tab().ask?.previewPending).toBe(false);
    expect(tab().widgetAutoOpen).toBeNull();
    expect(tab("s2")).toBe(otherTab);
    expect(tab("s2").widgetAutoOpen).toMatchObject({ armed: true, requestId: expect.any(String), askId: "a2" });
  });

  it("关闭 Tab 后的失败不复活状态桶", async () => {
    const pending = deferred();
    vi.mocked(ipc.resolveAsk).mockReturnValueOnce(pending.promise);
    const request = useRun.getState().previewPlan("s1", "a1");
    const rejected = expect(request).rejects.toThrow("closed tab");
    useRun.getState().dispose("s1");

    pending.reject(new Error("closed tab"));
    await rejected;

    expect(useRun.getState().tabs.s1).toBeUndefined();
  });
});

describe("previewPlan ask and run lifecycle", () => {
  it.each(["restore", "event"] as const)("%s 同一 ask 时保留 pending 与 armed，禁止恢复重复提交", async (route) => {
    await useRun.getState().previewPlan("s1", "a1");
    const signal = tab().widgetAutoOpen;
    const payload = askEvent({ plan_body: "更新后的正文" });

    if (route === "restore") useRun.getState().restoreAsk(payload);
    else handlers()["ask:opened"](payload);

    expect(tab().ask).toMatchObject({ askId: "a1", previewPending: true, canPreviewPlan: true, planBody: "更新后的正文" });
    expect(tab().widgetAutoOpen).toEqual(signal);
    await expect(useRun.getState().previewPlan("s1", "a1")).rejects.toThrow();
    expect(ipc.resolveAsk).toHaveBeenCalledTimes(1);
  });

  it("新 ask 清空尚未满足的 armed 并恢复可预览状态", async () => {
    await useRun.getState().previewPlan("s1", "a1");

    useRun.getState().restoreAsk(askEvent({ ask_id: "a2" }));

    expect(tab().ask?.previewPending).toBeFalsy();
    expect(tab().widgetAutoOpen).toBeNull();
    await useRun.getState().previewPlan("s1", "a2");
    expect(ipc.resolveAsk).toHaveBeenLastCalledWith("s1", "a2", { action: "preview_plan", note: "" });
  });

  it("新 ask 不抢走已锁定 widget 的一次性弹出机会", async () => {
    await useRun.getState().previewPlan("s1", "a1");
    useRun.getState().onToolResult("s1", renderResult(), true);

    useRun.getState().restoreAsk(askEvent({ ask_id: "a2" }));

    expect(tab().ask?.previewPending).toBeFalsy();
    expect(tab().widgetAutoOpen).toEqual({ callKey: "b1:0" });
  });

  it("只接受发起预览时同一 run 的 render_html", async () => {
    await useRun.getState().previewPlan("s1", "a1");

    useRun.getState().onToolResult("s1", renderResult({ run_id: "r-old", call_key: "old:0" }), true);
    expect(tab().widgetAutoOpen).toMatchObject({ armed: true, runId: "r1" });
    useRun.getState().onToolResult("s1", renderResult(), true);
    expect(tab().widgetAutoOpen).toEqual({ callKey: "b1:0" });
  });

  it.each(["run:done", "run:error", "run:cancelled"])("%s 清空 ask 与待弹状态，迟到成功不重置", async (event) => {
    const pending = deferred();
    vi.mocked(ipc.resolveAsk).mockReturnValueOnce(pending.promise);
    const request = useRun.getState().previewPlan("s1", "a1");

    handlers()[event]({ session: "s1", run_id: "r1", error: "run failed" });
    pending.resolve();
    await request;
    useRun.getState().onToolResult("s1", renderResult(), true);

    expect(tab().running).toBe(false);
    expect(tab().ask).toBeNull();
    expect(tab().widgetAutoOpen).toBeNull();
  });

  it("取消后启动新 run，旧 IPC 拒绝和旧 run:error 均不清新信号", async () => {
    const oldPending = deferred();
    vi.mocked(ipc.resolveAsk).mockReturnValueOnce(oldPending.promise);
    const oldRequest = useRun.getState().previewPlan("s1", "a1");
    const rejected = expect(oldRequest).rejects.toThrow("old run failure");
    handlers()["run:cancelled"]({ session: "s1", run_id: "r1" });
    seedTab("s1", "a2", "r2");
    await useRun.getState().previewPlan("s1", "a2");

    oldPending.reject(new Error("old run failure"));
    await rejected;
    handlers()["run:error"]({ session: "s1", run_id: "r1", error: "stale terminal" });

    expect(tab().running).toBe(true);
    expect(tab().ask).toMatchObject({ askId: "a2", previewPending: true });
    expect(tab().widgetAutoOpen).toEqual({ armed: true, requestId: expect.any(String), askId: "a2", runId: "r2" });
  });

  it("新 run 即使复用 askId，旧失败也不能回滚新请求", async () => {
    const oldPending = deferred();
    vi.mocked(ipc.resolveAsk).mockReturnValueOnce(oldPending.promise);
    const oldRequest = useRun.getState().previewPlan("s1", "a1");
    const rejected = expect(oldRequest).rejects.toThrow("old generation failure");
    const oldRequestId = tab().widgetAutoOpen?.requestId;
    handlers()["run:cancelled"]({ session: "s1", run_id: "r1" });
    seedTab("s1", "a1", "r2");
    await useRun.getState().previewPlan("s1", "a1");
    const newSignal = tab().widgetAutoOpen;
    expect(newSignal?.requestId).not.toBe(oldRequestId);

    oldPending.reject(new Error("old generation failure"));
    await rejected;

    expect(tab().ask).toMatchObject({ askId: "a1", previewPending: true });
    expect(tab().widgetAutoOpen).toEqual(newSignal);
  });
});

describe("legacy preview resolveAsk lifecycle", () => {
  const legacyValue = { answers: { q1: { selections: ["preview"], note: "先看窄屏布局" } } };

  it("旧预览选项保留 answers 载荷，并在 IPC 返回前捕获快速 widget", async () => {
    const pending = deferred();
    vi.mocked(ipc.resolveAsk).mockReturnValueOnce(pending.promise);
    useRun.getState().restoreAsk(askEvent({ can_preview_plan: undefined }));
    const request = useRun.getState().resolveAsk("a1", legacyValue, "s1", true);

    expect(ipc.resolveAsk).toHaveBeenCalledExactlyOnceWith("s1", "a1", legacyValue);
    expect(tab().ask?.previewPending).toBe(true);
    expect(tab().widgetAutoOpen).toEqual({ armed: true, requestId: expect.any(String), askId: "a1", runId: "r1" });
    await expect(useRun.getState().resolveAsk("a1", legacyValue, "s1", true)).rejects.toThrow();
    expect(ipc.resolveAsk).toHaveBeenCalledTimes(1);
    handlers()["ask:closed"]({ session: "s1", ask_id: "a1" });
    useRun.getState().onToolResult("s1", renderResult(), true);
    expect(tab().widgetAutoOpen).toEqual({ callKey: "b1:0" });

    pending.resolve();
    await request;

    expect(tab().widgetAutoOpen).toEqual({ callKey: "b1:0" });
    useRun.getState().consumeWidgetAutoOpen("s1", "b1:0");
    expect(tab().widgetAutoOpen).toBeNull();
  });

  it("旧预览失败仅回滚当前 armed 与 pending，重试生成新 token", async () => {
    const pending = deferred();
    vi.mocked(ipc.resolveAsk).mockReturnValueOnce(pending.promise);
    const request = useRun.getState().resolveAsk("a1", legacyValue, "s1", true);
    const rejected = expect(request).rejects.toThrow("legacy preview failed");
    const oldRequestId = tab().widgetAutoOpen?.requestId;

    pending.reject(new Error("legacy preview failed"));
    await rejected;

    expect(tab().ask?.previewPending).toBe(false);
    expect(tab().widgetAutoOpen).toBeNull();
    await useRun.getState().resolveAsk("a1", legacyValue, "s1", true);
    expect(tab().ask?.previewPending).toBe(true);
    expect(tab().widgetAutoOpen?.requestId).not.toBe(oldRequestId);
  });

  it("旧预览的失败回执保留已锁定 widget", async () => {
    const pending = deferred();
    vi.mocked(ipc.resolveAsk).mockReturnValueOnce(pending.promise);
    const request = useRun.getState().resolveAsk("a1", legacyValue, "s1", true);
    const rejected = expect(request).rejects.toThrow("legacy late failure");
    useRun.getState().onToolResult("s1", renderResult(), true);

    pending.reject(new Error("legacy late failure"));
    await rejected;

    expect(tab().ask?.previewPending).toBe(false);
    expect(tab().widgetAutoOpen).toEqual({ callKey: "b1:0" });
  });

  it("旧预览跨 run 的迟到失败不回滚复用 askId 的新预览", async () => {
    const pending = deferred();
    vi.mocked(ipc.resolveAsk).mockReturnValueOnce(pending.promise);
    const oldRequest = useRun.getState().resolveAsk("a1", legacyValue, "s1", true);
    const rejected = expect(oldRequest).rejects.toThrow("legacy stale run");
    const oldRequestId = tab().widgetAutoOpen?.requestId;
    handlers()["run:cancelled"]({ session: "s1", run_id: "r1" });
    seedTab("s1", "a1", "r2");
    await useRun.getState().resolveAsk("a1", legacyValue, "s1", true);
    const newSignal = tab().widgetAutoOpen;
    expect(newSignal?.requestId).not.toBe(oldRequestId);

    pending.reject(new Error("legacy stale run"));
    await rejected;

    expect(tab().ask?.previewPending).toBe(true);
    expect(tab().widgetAutoOpen).toEqual(newSignal);
    expect(tab().widgetAutoOpen?.runId).toBe("r2");
  });
});

describe("ask request epoch isolation", () => {
  it.each(["previewPlan", "legacyPreview", "ordinaryAnswer"] as const)("%s 同 runId、同 askId 但新 epoch 时，旧失败不清 pending", async (kind) => {
    const value = { answers: { q1: { selections: [kind === "ordinaryAnswer" ? "approve" : "preview"], note: "" } } };
    const requestCurrent = () => kind === "previewPlan"
      ? useRun.getState().previewPlan("s1", "a1")
      : useRun.getState().resolveAsk("a1", value, "s1", kind === "legacyPreview");
    const pending = deferred();
    vi.mocked(ipc.resolveAsk).mockReturnValueOnce(pending.promise);
    const oldEpoch = tab().runEpoch;
    const oldRequest = requestCurrent();
    const rejected = expect(oldRequest).rejects.toThrow("stale epoch failure");
    const oldRequestId = tab().widgetAutoOpen?.requestId;

    useRun.getState().dispose("s1");
    seedTab("s1", "a1", "r1");
    expect(tab().runEpoch).not.toBe(oldEpoch);
    await requestCurrent();
    const newSignal = tab().widgetAutoOpen;
    if (kind !== "ordinaryAnswer") expect(newSignal?.requestId).not.toBe(oldRequestId);

    pending.reject(new Error("stale epoch failure"));
    await rejected;

    expect(tab().ask).toMatchObject({ askId: "a1", previewPending: true });
    expect(tab().widgetAutoOpen).toEqual(newSignal);
    await expect(requestCurrent()).rejects.toThrow();
    expect(ipc.resolveAsk).toHaveBeenCalledTimes(2);
  });
});
