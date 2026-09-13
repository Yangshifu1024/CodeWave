// User message hover actions ([docs/titlebar-content-batch](../../../docs/titlebar-content-batch.md) addendum): copy → clipboard; edit → ws:composer-fill backfills the Composer.
// Render ChatMessages directly + craft items via useRun setState (invoke returns [] everywhere).
import { describe, it, expect, vi, afterEach } from "vitest";
import { render, cleanup, fireEvent } from "@testing-library/react";
import { App } from "antd";
import "../i18n";
import ChatMessages from "../features/chat/ChatMessages";
import { useSessions } from "../stores/sessions";
import { useRun } from "../stores/run";

vi.mock("@tauri-apps/api/core", () => ({
  invoke: vi.fn(async () => []),
}));

const writeText = vi.fn(async () => {});
Object.defineProperty(navigator, "clipboard", { value: { writeText }, configurable: true });

function seedItems(items: any[]) {
  useSessions.setState({ activeKey: "s1" });
  useRun.setState((s) => {
    s.tabs["s1"] = {
      items, running: false, streamGen: 0, ask: null, breakdown: null,
      todos: [], suggestions: [], subs: [], subStreams: {}, subDrawer: { open: false, subId: null }, gitEntries: null, writeTick: 0,
      queue: [], pendingItemId: null, draftFromQueue: null, lastDoneRunId: null, compacting: false,
    };
  });
}

function renderMsgs() {
  return render(
    <App>
      <ChatMessages />
    </App>,
  );
}

afterEach(() => {
  cleanup();
  writeText.mockClear();
  useSessions.setState({ activeKey: null });
  useRun.setState((s) => {
    s.tabs = {};
  });
});

describe("用户消息悬停操作（复制/修改）", () => {
  it("文本消息渲染复制与修改按钮", () => {
    seedItems([{ kind: "user", text: "标题左侧留出间距" }]);
    renderMsgs();
    expect(document.querySelector('button[aria-label="复制"]')).toBeTruthy();
    expect(document.querySelector('button[aria-label="修改"]')).toBeTruthy();
  });

  it("复制：内容写入剪贴板", () => {
    seedItems([{ kind: "user", text: "复制我" }]);
    renderMsgs();
    fireEvent.click(document.querySelector('button[aria-label="复制"]')!);
    expect(writeText).toHaveBeenCalledWith("复制我");
  });

  it("修改：派发 ws:composer-fill 携带原文（Composer 侧监听回填）", () => {
    seedItems([{ kind: "user", text: "回填我" }]);
    const received: unknown[] = [];
    const onFill = (e: Event) => received.push((e as CustomEvent).detail);
    window.addEventListener("ws:composer-fill", onFill);
    renderMsgs();
    fireEvent.click(document.querySelector('button[aria-label="修改"]')!);
    window.removeEventListener("ws:composer-fill", onFill);
    expect(received).toEqual([{ text: "回填我" }]);
  });

  it("纯图片消息（无文本）不渲染操作按钮", () => {
    seedItems([{ kind: "user", text: "", images: [{ mediaType: "image/png", data: "AAAA" }] }]);
    renderMsgs();
    expect(document.querySelector('button[aria-label="复制"]')).toBeFalsy();
    expect(document.querySelector('button[aria-label="修改"]')).toBeFalsy();
    expect(document.querySelector(".user-attachments")).toBeTruthy();
  });
});

describe("消息时间戳恢复（旧档案留空，不伪造当前时刻）", () => {
  function tsTexts(): (string | undefined)[] {
    return [...document.querySelectorAll(".msg .role .ts")].map((e) => e.textContent ?? "");
  }

  it("restoreFromMessages：无 created_at 的旧消息时间行留空", () => {
    useSessions.setState({ activeKey: "s1" });
    useRun.getState().restoreFromMessages("s1", [
      { role: "user", content: [{ type: "text", text: "旧问题" }] },
      { role: "assistant", content: [{ type: "text", text: "旧回答" }] },
    ] as any);
    renderMsgs();
    const texts = tsTexts();
    expect(texts.length).toBe(2);
    for (const t of texts) expect(t).toBe(""); // never falls back to Date.now() impersonating "now"
  });

  it("restoreFromMessages：带 created_at 的新档案显示真实时间", () => {
    useSessions.setState({ activeKey: "s1" });
    useRun.getState().restoreFromMessages("s1", [
      { role: "user", content: [{ type: "text", text: "新问题" }], created_at: "2025-01-01T10:00:00Z" },
    ] as any);
    renderMsgs();
    const texts = tsTexts();
    expect(texts[0]).toContain("2025-01-01");
  });
});
