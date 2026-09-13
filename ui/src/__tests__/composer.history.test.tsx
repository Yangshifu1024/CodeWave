// Composer ↑↓ history recall: empty input + ↑ recalls the latest user message / browsing mode ↑↓ walks history (stops at
// the oldest, past the newest restores the draft) / Esc exits / typing exits / image recall / IME ignored / no-op without history / Enter sends.
import { describe, it, expect, vi, beforeAll, afterEach } from "vitest";
import { render, screen, fireEvent, waitFor, cleanup } from "@testing-library/react";
import { App as AntApp } from "antd";
import "../i18n"; // Composer mounted standalone must init i18next explicitly (no global entry outside App.tsx)

vi.mock("@tauri-apps/api/core", () => ({
  invoke: vi.fn(async () => null),
  Channel: class {
    onmessage: any = null;
  },
}));

vi.mock("../ipc/client", () => {
  return {
    ipc: {
      listSkills: vi.fn(async () => []),
      searchWorkspacePaths: vi.fn(async () => []),
      compactSession: vi.fn(async () => null),
      startChat: vi.fn(async () => "ok"),
      cancelRun: vi.fn(async () => null),
    },
  };
});

import Composer from "../features/chat/Composer";
import { ipc } from "../ipc/client";
import { useSessions } from "../stores/sessions";
import { useRun } from "../stores/run";
import { useSettings } from "../stores/settings";
import { useUi } from "../stores/ui";

const MODEL_BASE = {
  id: "m1", model: "test-model", max_tokens: 32768, context_window: 128000,
  reasoning_effort: null, vision: true, video: false,
};

type UserItem = { kind: "user"; text: string; images?: { mediaType: string; data: string }[] };

function seedEnv(items: (UserItem | Record<string, unknown>)[] = []) {
  useSettings.setState({
    config: {
      schema_version: 2,
      providers: [
        {
          id: "p1", name: "Test Provider", api_format: "openai_chat" as const,
          base_url: "https://api.example.com/v1", keys: ["***abcd"],
          models: [{ ...MODEL_BASE }],
        },
      ],
      active_model_id: "m1",
      proxy: null,
      network: { allow_private_network: false },
      compact_threshold: 0.6,
      compact_timeout_seconds: 180,
      approval: { enabled: true, confirm_outside_create: true, confirm_git_push: true, auto_confirm: false, command_allowlist: [] },
      validation: { python: true, rust: true, typescript: true, go: true, json: true },
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
      items: items as any,
      running: false, streamGen: 0, ask: null, breakdown: null,
      todos: [], suggestions: [], subs: [], subStreams: {}, subDrawer: { open: false, subId: null }, gitEntries: null, writeTick: 0, queue: [], pendingItemId: null, draftFromQueue: null, lastDoneRunId: null, compacting: false,
    };
  });
}

function mountComposer() {
  return render(
    <AntApp>
      <Composer />
    </AntApp>,
  );
}

function getTextarea() {
  return screen.getByPlaceholderText(/CodeWave/) as HTMLTextAreaElement;
}

/** Dispatch a native KeyboardEvent (can carry isComposing; React onKeyDown catches it via delegation) */
function fireKey(el: HTMLElement, key: string, init: KeyboardEventInit = {}) {
  fireEvent(el, new KeyboardEvent("keydown", { key, bubbles: true, cancelable: true, ...init }));
}

/** Build a paste event carrying clipboardData (happy-dom's ClipboardEvent has no clipboardData) */
function firePaste(textarea: HTMLElement, files: File[], text = "") {
  const pasteEvent = new Event("paste", { bubbles: true, cancelable: true }) as any;
  pasteEvent.clipboardData = {
    files,
    items: files.map((f) => ({ kind: "file", type: f.type, getAsFile: () => f })),
    getData: () => text,
  };
  fireEvent(textarea, pasteEvent);
}

const TWO = (): (UserItem | Record<string, unknown>)[] => [
  { kind: "user", text: "hello1" },
  { kind: "user", text: "hello2" },
];

beforeAll(() => {
  Element.prototype.scrollTo = (Element.prototype as any).scrollTo ?? (() => {});
});

afterEach(() => {
  cleanup();
  // zustand module-level singletons: clear leftovers (panel toggles + tabs/config + run tabs)
  useUi.setState({ settingsOpen: false, tasksOpen: false, statsOpen: false, rbTab: "info" });
  useSessions.setState({ tabs: [], activeKey: null, projects: [], sessions: [] });
  useSettings.setState({ config: null, loaded: false });
  useRun.setState({ tabs: {}, drafts: {} });
  vi.clearAllMocks();
});

describe("Composer 历史消息召回（↑↓）", () => {
  it("H1 空输入按 ↑：召回最近一条用户消息", () => {
    seedEnv(TWO());
    mountComposer();
    const ta = getTextarea();
    fireKey(ta, "ArrowUp");
    expect(ta.value).toBe("hello2");
  });

  it("H2 浏览态 ↑ 前翻（hello2→hello1），最旧处停留不回绕", () => {
    seedEnv(TWO());
    mountComposer();
    const ta = getTextarea();
    fireKey(ta, "ArrowUp");
    expect(ta.value).toBe("hello2");
    fireKey(ta, "ArrowUp");
    expect(ta.value).toBe("hello1");
    fireKey(ta, "ArrowUp");
    expect(ta.value).toBe("hello1"); // stays at the oldest
  });

  it("H3 ↓ 后翻至越过最新：退出浏览态并恢复进入前草稿（空文本+附件）", async () => {
    seedEnv([
      { kind: "user", text: "hello1" },
      { kind: "user", text: "hello2" },
      { kind: "user", text: "hello3" },
    ]);
    mountComposer();
    const ta = getTextarea();
    // Pre-recall draft = empty text + pasted image (↑ only enters from empty text; a non-empty draft is unreachable, see H6)
    firePaste(ta, [new File(["x"], "a.png", { type: "image/png" })]);
    await waitFor(() => expect(document.querySelector(".attach-chip")).toBeTruthy());
    fireKey(ta, "ArrowUp"); // enter browsing: points at the newest hello3, attachments temporarily hidden
    expect(ta.value).toBe("hello3");
    expect(document.querySelector(".attach-chip")).toBeFalsy();
    fireKey(ta, "ArrowUp"); // hello3 → hello2
    expect(ta.value).toBe("hello2");
    fireKey(ta, "ArrowUp"); // hello2 → hello1 (oldest)
    expect(ta.value).toBe("hello1");
    fireKey(ta, "ArrowDown"); // hello1 → hello2
    expect(ta.value).toBe("hello2");
    fireKey(ta, "ArrowDown"); // hello2 → hello3 (newest)
    expect(ta.value).toBe("hello3");
    fireKey(ta, "ArrowDown"); // past the newest → draft restored
    expect(ta.value).toBe("");
    await waitFor(() => expect(document.querySelector(".attach-chip")).toBeTruthy()); // attachments restored
  });

  it("H4 Esc 退出浏览态：恢复进入前草稿（空文本+附件）", async () => {
    seedEnv(TWO());
    mountComposer();
    const ta = getTextarea();
    firePaste(ta, [new File(["x"], "a.png", { type: "image/png" })]);
    await waitFor(() => expect(document.querySelector(".attach-chip")).toBeTruthy());
    fireKey(ta, "ArrowUp");
    expect(ta.value).toBe("hello2");
    expect(document.querySelector(".attach-chip")).toBeFalsy();
    fireKey(ta, "Escape");
    expect(ta.value).toBe("");
    await waitFor(() => expect(document.querySelector(".attach-chip")).toBeTruthy()); // attachments restored
  });

  it("H5 召回后键入字符：退出浏览态，保留召回文本为编辑基础", () => {
    seedEnv(TWO());
    mountComposer();
    const ta = getTextarea();
    fireKey(ta, "ArrowUp");
    expect(ta.value).toBe("hello2");
    fireEvent.change(ta, { target: { value: "hello2x" } });
    expect(ta.value).toBe("hello2x"); // recalled text kept as the editing base
    fireKey(ta, "ArrowDown"); // no longer browsing: ↓ no longer restores the draft
    expect(ta.value).toBe("hello2x");
  });

  it("H7 带图用户消息召回：附件缩略图恢复 + 输入框文本正确", () => {
    seedEnv([
      { kind: "user", text: "hello1" },
      { kind: "user", text: "hello2", images: [{ mediaType: "image/png", data: "iVBORw0KGgo=" }] },
    ]);
    mountComposer();
    const ta = getTextarea();
    fireKey(ta, "ArrowUp");
    expect(ta.value).toBe("hello2");
    const chip = document.querySelector(".attach-chip");
    expect(chip).toBeTruthy();
    const img = chip?.querySelector("img");
    expect(img?.getAttribute("src")).toBe("data:image/png;base64,iVBORw0KGgo=");
    expect(document.body.textContent ?? "").toContain("历史图片 1");
  });

  it("H10 IME 组合中 ArrowUp 不触发召回", () => {
    seedEnv(TWO());
    mountComposer();
    const ta = getTextarea();
    fireKey(ta, "ArrowUp", { isComposing: true });
    expect(ta.value).toBe("");
  });

  it("IME WebKit 形态：确认候选词的 Enter（isComposing=false + keyCode 229）不触发发送，文本保留在输入框", async () => {
    seedEnv([]);
    mountComposer();
    const ta = getTextarea();
    // 模拟 WebKit IME 序列：组合开始 → 输入上屏（change） → 确认 Enter（isComposing 已为 false）
    fireEvent.compositionStart(ta);
    fireEvent.change(ta, { target: { value: "logo" } });
    fireKey(ta, "Enter", { keyCode: 229 });
    await waitFor(() => expect(ipc.startChat).not.toHaveBeenCalled());
    expect(ta.value).toBe("logo");
    // composition 结束后同一文本再按 Enter：正常发送，且全局 composing 状态未锁死
    fireEvent.compositionEnd(ta);
    fireKey(ta, "Enter");
    await waitFor(() => expect(ipc.startChat).toHaveBeenCalledWith("s1", "logo", [], expect.anything()));
  });

  it("IME 组合中 Enter（isComposing=true）不触发发送；组合正常结束发送可用（防锁死回归）", async () => {
    seedEnv([]);
    mountComposer();
    const ta = getTextarea();
    fireEvent.compositionStart(ta);
    fireEvent.change(ta, { target: { value: "你好" } });
    fireKey(ta, "Enter", { isComposing: true });
    await waitFor(() => expect(ipc.startChat).not.toHaveBeenCalled());
    expect(ta.value).toBe("你好");
    fireEvent.compositionEnd(ta);
    fireKey(ta, "Enter");
    await waitFor(() => expect(ipc.startChat).toHaveBeenCalledWith("s1", "你好", [], expect.anything()));
  });

  it("H13 无用户消息按 ↑：无动作不报错", () => {
    seedEnv([]);
    mountComposer();
    const ta = getTextarea();
    fireKey(ta, "ArrowUp");
    expect(ta.value).toBe("");
    expect(document.querySelector(".attach-chip")).toBeFalsy();
  });

  it("H6 非空且非浏览态按 ↑：不劫持方向键", () => {
    seedEnv(TWO());
    mountComposer();
    const ta = getTextarea();
    fireEvent.change(ta, { target: { value: "my draft" } });
    fireKey(ta, "ArrowUp");
    expect(ta.value).toBe("my draft"); // non-empty input not hijacked, cursor semantics preserved
  });

  it("召回后 Enter：作为新消息发送（startChat 收到召回文本），指针复位", async () => {
    seedEnv(TWO());
    mountComposer();
    const ta = getTextarea();
    fireKey(ta, "ArrowUp");
    expect(ta.value).toBe("hello2");
    fireKey(ta, "Enter");
    await waitFor(() => expect(ipc.startChat).toHaveBeenCalled());
    expect(ipc.startChat).toHaveBeenCalledWith("s1", "hello2", [], expect.anything());
    expect(ta.value).toBe(""); // cleared after acceptance
    fireKey(ta, "ArrowDown"); // pointer reset: ↓ is a no-op
    expect(ta.value).toBe("");
  });
});
