// 文件引用入口（[docs/chat-and-interface](../../../docs/chat-and-interface.md)）：
// 附件按钮选文件 → 非图片进草稿 refs 并以 chip 展示（正文里不再出现路径）；图片走附件通道；
// 项目外路径先弹放行确认框；发送前一刻才把引用合成 `@路径` 追加到正文末尾。
// Composer 独立挂载（不依赖 AppShell），与 composer.paste 同一套环境种子。
import { describe, it, expect, vi, beforeAll, beforeEach, afterEach } from "vitest";
import { render, screen, fireEvent, waitFor, cleanup, act } from "@testing-library/react";
import { App as AntApp } from "antd";
import "../i18n"; // Composer 独立挂载必须先初始化 i18next（文案全走 t()）

const ipcMock = vi.hoisted(() => ({
  selectDocumentFiles: vi.fn(async () => [] as string[]),
  checkExternalPath: vi.fn(async (_sid: string, _p: string) => ({ inside: true, dir: "", ref: "" })),
  allowExternalDir: vi.fn(async () => [] as string[]),
  readWorkspaceFileBase64: vi.fn(async () => ({ path: "", size: 0, content: "" })),
  listSkills: vi.fn(async () => []),
  searchWorkspacePaths: vi.fn(async (_sid: string, _q: string, _limit?: number) => [] as string[]),
  compactSession: vi.fn(async () => null),
  startChat: vi.fn(async (_sid: string, _text: string, _images: { mime: string; data: string }[], _ch?: unknown) => "ok"),
  listAgents: vi.fn(async () => []),
  cancelRun: vi.fn(async () => null),
}));

vi.mock("@tauri-apps/api/core", () => ({
  invoke: vi.fn(async () => null),
  Channel: class {
    onmessage: any = null;
  },
}));

vi.mock("../ipc/client", () => ({ ipc: ipcMock }));
const drop = vi.hoisted(() => ({ current: null as null | ((paths: string[]) => void) }));
vi.mock("../ipc/dragdrop", () => ({ listenFileDrop: vi.fn(async (handlers) => {
  drop.current = handlers.onDrop;
  return () => { if (drop.current === handlers.onDrop) drop.current = null; };
}) }));

import Composer from "../features/chat/Composer";
import { useSessions } from "../stores/sessions";
import { useRun } from "../stores/run";
import { useSettings } from "../stores/settings";
import { useUi } from "../stores/ui";

const MODEL_BASE = {
  id: "m1", model: "test-model", max_tokens: 32768, context_window: 128000,
  reasoning_effort: null, vision: true, video: false,
};

function seedEnv() {
  useSettings.setState({
    config: {
      schema_version: 2,
      providers: [
        {
          id: "p1", name: "Test Provider", api_format: "openai_chat" as const,
          base_url: "https://api.example.com/v1", keys: ["***abcd"],
          models: [{ ...MODEL_BASE }],
          headers: [],
        },
      ],
      active_model_id: "m1",
      proxy: null,
      network: { allow_private_network: false },
      compact_threshold: 0.6,
      compact_timeout_seconds: 180,
      approval: { enabled: true, confirm_outside_create: true, confirm_git_push: true, auto_confirm: false, command_allowlist: [] },
      post_write_check: { enabled: false, command: "", timeout_seconds: 30, tail_chars: 3000 },
      ui: { font_size: 15, accent: "cyan", language: "zh-CN" },
      custom_prompt: null,
      disabled_skills: [],
      log: { level: "info", session_verbose: false },
    },
    loaded: true,
  });
  useSessions.setState({
    tabs: [{
      key: "s1", sessionId: "s1", workspace: "/tmp/ws", title: "冒烟会话",
      projectId: null, createdAt: "2026-08-30T00:00:00Z",
      prefs: { approval_mode: "auto_edit", model_id: null, reasoning_effort: null },
    }],
    activeKey: "s1",
  });
  useRun.setState((s) => {
    s.tabs["s1"] = {
      items: [], running: false, streamGen: 0, ask: null, breakdown: null,
      todos: [], suggestions: [], subs: [], subStreams: {}, subDrawer: { open: false, subId: null },
      gitEntries: null, writeTick: 0, queue: [], pendingItemId: null, draftFromQueue: null, lastDoneRunId: null,
      compacting: false,
    } as (typeof s.tabs)[string];
    s.drafts = { s1: { text: "", images: [], refs: [] } };
  });
}

/** 打开「+」菜单并点「添加附件」；选文件的行为由 ipc.selectDocumentFiles 的 mock 决定。 */
async function clickAttach() {
  fireEvent.click(document.querySelector(".toolbar-left .ant-btn") as HTMLElement);
  const item = await waitFor(() => {
    const el = Array.from(document.querySelectorAll(".ant-dropdown-menu-item")).find((o) =>
      (o.textContent ?? "").includes("附件"),
    );
    expect(el).toBeTruthy();
    return el as HTMLElement;
  });
  fireEvent.click(item);
}


/** 按去空白后的文本找按钮：antd 会在两个字的按钮里插空格（「取 消」）。 */
function buttonByText(label: string): HTMLElement {
  const el = Array.from(document.querySelectorAll("button")).find(
    (b) => (b.textContent ?? "").replace(/\s/g, "") === label,
  );
  expect(el, `按钮「${label}」未找到`).toBeTruthy();
  return el as HTMLElement;
}

/** 在输入框里写正文（chip 不在 textarea 里，正文只放人写的内容）。
 *  用 placeholder 定位：antd TextArea 的 autoSize 会在 DOM 里另放一个测量用 textarea，
 *  直接 querySelector("textarea") 可能拿到那个非受控的替身。 */
function typeBody(v: string) {
  const ta = screen.getByPlaceholderText(/CodeWave/) as HTMLTextAreaElement;
  fireEvent.change(ta, { target: { value: v } });
}

beforeAll(() => {
  Element.prototype.scrollTo = (Element.prototype as any).scrollTo ?? (() => {});
});

afterEach(() => {
  cleanup();
  useUi.setState({ settingsOpen: false, tasksOpen: false, statsOpen: false, rbTab: "info" });
  useSessions.setState({ tabs: [], activeKey: null, projects: [], sessions: [] });
  useSettings.setState({ config: null, loaded: false });
  useRun.setState({ tabs: {}, drafts: {}, draftIdentities: {}, draftSends: {} });
  vi.clearAllMocks();
  vi.unstubAllGlobals();
  ipcMock.selectDocumentFiles.mockResolvedValue([]);
  ipcMock.checkExternalPath.mockImplementation(async () => ({ inside: true, dir: "", ref: "" }));
});


function deferred<T>() { let resolve!: (value: T) => void; const promise = new Promise<T>((r) => { resolve = r; }); return { promise, resolve }; }
function mount() { return render(<AntApp><Composer /></AntApp>); }
function toB() {
  act(() => {
    const source = useSessions.getState().tabs[0]!;
    useSessions.setState({ tabs: [source, { ...source, key: "s2", sessionId: "s2" }], activeKey: "s2" });
    useRun.getState().initTab("s2");
    useRun.getState().setDraftText("B text", "s2");
    useRun.getState().setDraftRefs(["B.pdf"], "s2");
  });
}
function paste(name = "a.png", size = 1) {
  const file = new File([new Uint8Array(size)], name, { type: "image/png" });
  fireEvent.paste(screen.getByPlaceholderText(/CodeWave/), { clipboardData: { files: [file], items: [] } });
}
const reads: { onload: (() => void) | null; result: string; readAsDataURL: () => void }[] = [];
beforeEach(() => {
  reads.length = 0;
  vi.stubGlobal("FileReader", class {
    onload = null; result = "";
    readAsDataURL() { reads.push(this); }
  });
  ipcMock.checkExternalPath.mockImplementation(async (_sid, path) => ({ inside: true, dir: "", ref: path }));
});
async function finishRead(index: number, value: string) { await act(async () => { reads[index]!.result = `data:image/png;base64,${value}`; reads[index]!.onload?.(); }); }

describe("Composer deferred attachment races", () => {
  it("binds paste before FileReader, preserving all B fields", async () => {
    seedEnv(); mount(); paste(); toB(); const b = useRun.getState().drafts.s2;
    await finishRead(0, "AAAA");
    expect(useRun.getState().drafts.s1.images[0].data).toBe("AAAA");
    expect(useRun.getState().drafts.s2).toBe(b);
  });
  it("captures native-picker identity before both the dialog and base64 awaits", async () => {
    seedEnv(); const picker = deferred<string[]>(); const read = deferred<{ path: string; size: number; content: string }>();
    ipcMock.selectDocumentFiles.mockReturnValueOnce(picker.promise); ipcMock.readWorkspaceFileBase64.mockReturnValueOnce(read.promise);
    mount(); await clickAttach(); toB();
    await act(async () => { picker.resolve(["/tmp/a.png", "/tmp/a.pdf"]); });
    expect(ipcMock.readWorkspaceFileBase64).toHaveBeenCalledWith("s1", "/tmp/a.png");
    await act(async () => { read.resolve({ path: "/tmp/a.png", size: 3, content: "AAAA" }); });
    expect(useRun.getState().drafts.s1.images[0].data).toBe("AAAA");
    expect(useRun.getState().drafts.s1.refs).toEqual(["/tmp/a.pdf"]);
    expect(useRun.getState().drafts.s2).toMatchObject({ text: "B text", refs: ["B.pdf"], images: [] });
  });
  it("binds native drop to original session", async () => {
    seedEnv(); const read = deferred<{ path: string; size: number; content: string }>(); ipcMock.readWorkspaceFileBase64.mockReturnValueOnce(read.promise);
    mount(); await act(async () => { drop.current?.(["/tmp/a.png"]); }); toB();
    await act(async () => { read.resolve({ path: "/tmp/a.png", size: 3, content: "AAAA" }); });
    expect(useRun.getState().drafts.s1.images).toHaveLength(1); expect(useRun.getState().drafts.s2.images).toHaveLength(0);
  });
  it("merges reverse completions and does not resurrect a removed item", async () => {
    seedEnv(); mount(); paste("x.png"); paste("y.png");
    await finishRead(1, "YYYY");
    act(() => useRun.getState().removeDraftImage(useRun.getState().drafts.s1.images[0].id, "s1"));
    paste("z.png"); await finishRead(2, "ZZZZ"); await finishRead(0, "XXXX");
    expect(useRun.getState().drafts.s1.images.map((i) => i.data)).toEqual(["ZZZZ", "XXXX"]);
  });
  it.each(["clear", "replace", "reopen"])("discards deferred FileReader after %s", async (kind) => {
    seedEnv(); mount(); paste();
    act(() => {
      const run = useRun.getState();
      if (kind === "clear") run.clearDraft("s1");
      if (kind === "replace") window.dispatchEvent(new CustomEvent("ws:composer-fill", { detail: { text: "replacement" } }));
      if (kind === "reopen") { run.dispose("s1"); run.initTab("s1"); }
    });
    await finishRead(0, "AAAA"); expect(useRun.getState().drafts.s1?.images ?? []).toEqual([]);
  });
  it("admission sends only snapshot and preserves new text/refs/imported image", async () => {
    seedEnv(); const admission = deferred<string>(); ipcMock.startChat.mockReturnValueOnce(admission.promise);
    mount(); const input = screen.getByPlaceholderText(/CodeWave/); typeBody("old"); paste("old.png"); await finishRead(0, "OLD");
    paste("new.png");
    fireEvent.keyDown(screen.getByPlaceholderText(/CodeWave/), { key: "Enter" });
    await waitFor(() => expect(ipcMock.startChat).toHaveBeenCalled());
    expect(ipcMock.startChat.mock.calls[0].slice(0, 3)).toEqual(["s1", "old", [{ mime: "image/png", data: "OLD" }]]);
    fireEvent.change(input, { target: { value: "new text" } }); act(() => useRun.getState().setDraftRefs(["new.pdf"], "s1")); await finishRead(1, "NEW");
    await act(async () => { admission.resolve("accepted"); });
    expect(useRun.getState().drafts.s1).toMatchObject({ text: "new text", refs: ["new.pdf"] });
    expect(useRun.getState().drafts.s1.images.map((i) => i.data)).toEqual(["NEW"]);
  });
  it("settles two external-directory questions sequentially and cancels invalid targets", async () => {
    seedEnv(); ipcMock.checkExternalPath.mockImplementation(async (_sid, path) => ({ inside: false, dir: path, ref: path }));
    mount(); await act(async () => { drop.current?.(["/one/a.pdf"]); drop.current?.(["/two/b.pdf"]); });
    expect(screen.getByText(/\/one\/a\.pdf/)).toBeTruthy();
    fireEvent.click(buttonByText("取消"));
    await waitFor(() => expect(screen.getByText(/\/two\/b\.pdf/)).toBeTruthy());
    act(() => useRun.getState().clearDraft("s1"));
    await waitFor(() => expect(useRun.getState().draftIdentities.s1.imports).toEqual([]));
    // happy-dom does not deliver the CSS exit animation's completion event.
    const modal = document.querySelector(".ant-modal")!;
    expect(modal.className).toContain("ant-zoom-leave");
    fireEvent.animationEnd(modal);
    expect(ipcMock.allowExternalDir).not.toHaveBeenCalled(); expect(useRun.getState().drafts.s1.refs).toEqual([]);
  });
  it.each([5 * 1024 * 1024, 5 * 1024 * 1024 + 1])("checks single-image byte boundary %i before reading", async (size) => {
    seedEnv(); mount(); paste("boundary.png", size);
    expect(reads.length).toBe(size === 5 * 1024 * 1024 ? 1 : 0);
    if (reads.length) await finishRead(0, "AAAA");
    expect(useRun.getState().drafts.s1.images.length).toBe(size === 5 * 1024 * 1024 ? 1 : 0);
  });
  it.each(["unmount", "reopen"])("settles pending external question on %s without granting permission", async (kind) => {
    seedEnv(); ipcMock.checkExternalPath.mockImplementation(async (_sid, path) => ({ inside: false, dir: path, ref: path }));
    const view = mount(); await act(async () => { drop.current?.(["/outside/a.pdf"]); });
    expect(screen.getByRole("dialog")).toBeTruthy();
    if (kind === "unmount") view.unmount();
    else act(() => { useRun.getState().dispose("s1"); useRun.getState().initTab("s1"); });
    await waitFor(() => expect(useRun.getState().draftIdentities.s1?.imports ?? []).toEqual([]));
    expect(ipcMock.allowExternalDir).not.toHaveBeenCalled(); expect(useRun.getState().drafts.s1?.refs ?? []).toEqual([]);
  });

});
