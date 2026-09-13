// Composer image paste + vision soft block (Composer mounted standalone, no AppShell dependency). Covers: image paste
// shows a thumbnail / non-image file hint / plain text pastes by default / vision===false blocks / missing vision passes.
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

// Model entry under a provider ([docs/provider-management-refactor](../../../docs/provider-management-refactor.md) nested providers structure)
const MODEL_BASE = {
  id: "m1", model: "test-model", max_tokens: 32768, context_window: 128000,
  reasoning_effort: null, vision: true, video: false,
};

function seedEnv(modelExtra?: Record<string, unknown>) {
  useSettings.setState({
    config: {
      schema_version: 2,
      providers: [
        {
          id: "p1", name: "Test Provider", api_format: "openai_chat" as const,
          base_url: "https://api.example.com/v1", keys: ["***abcd"],
          models: [{ ...MODEL_BASE, ...modelExtra }],
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
      items: [], running: false, streamGen: 0, ask: null, breakdown: null,
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

async function pasteFile(file: File) {
  const textarea = screen.getByPlaceholderText(/CodeWave/) as HTMLTextAreaElement;
  firePaste(textarea, [file]);
  await new Promise((r) => setTimeout(r, 50));
  return textarea;
}

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

describe("Composer 粘贴图片与 vision 软阻断", () => {
  it("粘贴图片文件：出现附件缩略图", async () => {
    seedEnv();
    mountComposer();
    await pasteFile(new File(["x"], "a.png", { type: "image/png" }));
    await waitFor(() => expect(document.querySelector(".composer-attachments")).toBeTruthy());
    expect(document.querySelectorAll(".attach-chip").length).toBe(1);
    expect(document.body.textContent ?? "").toContain("a.png");
  });

  it("粘贴非图片文件：不产生附件，提示用 @ 引用", async () => {
    seedEnv();
    mountComposer();
    await pasteFile(new File(["data"], "note.txt", { type: "text/plain" }));
    expect(document.querySelector(".attach-chip")).toBeFalsy();
    expect(document.body.textContent ?? "").toContain("仅支持图片");
  });

  it("粘贴纯文本：无附件，文本进入输入框（默认粘贴不被拦截）", () => {
    seedEnv();
    mountComposer();
    const textarea = screen.getByPlaceholderText(/CodeWave/) as HTMLTextAreaElement;
    firePaste(textarea, [], "hello");
    expect(document.querySelector(".attach-chip")).toBeFalsy();
    fireEvent.change(textarea, { target: { value: "hello" } });
    expect(textarea.value).toBe("hello");
  });

  it("vision=false 且有附件：软阻断，不调用 startChat，输入框不清空", async () => {
    seedEnv({ vision: false });
    mountComposer();
    const textarea = await pasteFile(new File(["x"], "a.png", { type: "image/png" }));
    await waitFor(() => expect(document.querySelector(".attach-chip")).toBeTruthy());
    fireEvent.change(textarea, { target: { value: "看这张图" } });
    fireEvent.keyDown(textarea, { key: "Enter", shiftKey: false });
    await new Promise((r) => setTimeout(r, 80));
    expect(ipc.startChat).not.toHaveBeenCalled();
    expect(textarea.value).toBe("看这张图"); // not accepted, so not cleared
    expect(document.body.textContent ?? "").toContain("不支持图片");
  });

  it("vision=true：不拦截，send 正常走且输入清空（docs/provider-management-refactor 起为硬布尔）", async () => {
    seedEnv({ vision: true });
    mountComposer();
    const textarea = await pasteFile(new File(["x"], "a.png", { type: "image/png" }));
    await waitFor(() => expect(document.querySelector(".attach-chip")).toBeTruthy());
    fireEvent.change(textarea, { target: { value: "看这张图" } });
    fireEvent.keyDown(textarea, { key: "Enter", shiftKey: false });
    await waitFor(() => expect(useRun.getState().tabs["s1"]?.running).toBe(true));
    expect(ipc.startChat).toHaveBeenCalledTimes(1);
    expect(textarea.value).toBe("");
  });
});
