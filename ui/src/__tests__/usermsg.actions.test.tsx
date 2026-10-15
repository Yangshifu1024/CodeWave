// User message hover actions ([docs/chat-and-interface](../../../docs/chat-and-interface.md) addendum): copy → clipboard; edit → ws:composer-fill backfills the Composer.
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
// 原生插件封装（ipc/clipboard.ts）：单测环境非 Tauri，真函数会返回 false 自然降级到浏览器接口，
// 故 mock 插件包让 invoke 不报错即可，不改变这条「插件不可用 → 浏览器接口」的真实降级路径。
vi.mock("@tauri-apps/plugin-clipboard-manager", () => ({
  writeText: vi.fn(async () => {}),
}));

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
  writeText.mockReset();
  writeText.mockImplementation(async () => {});
  useSessions.setState({ activeKey: null });
  useRun.setState((s) => {
    s.tabs = {}; s.drafts = {};
  });
});

/** copyText 先 await 插件探测再写浏览器接口，断言前需把微任务队列排空。 */
async function flush() {
  for (let i = 0; i < 8; i++) await Promise.resolve();
}

describe("用户消息悬停操作（复制/修改）", () => {
  it("文本消息渲染复制与修改按钮", () => {
    seedItems([{ kind: "user", text: "标题左侧留出间距" }]);
    renderMsgs();
    expect(document.querySelector('button[aria-label="复制"]')).toBeTruthy();
    expect(document.querySelector('button[aria-label="修改"]')).toBeTruthy();
  });

  it("复制：内容写入剪贴板", async () => {
    seedItems([{ kind: "user", text: "复制我" }]);
    renderMsgs();
    fireEvent.click(document.querySelector('button[aria-label="复制"]')!);
    await flush();
    expect(writeText).toHaveBeenCalledWith("复制我");
  });

  it("复制：写入失败时按钮显失败态，不静默（回归钉子）", async () => {
    writeText.mockRejectedValueOnce(new Error("NotAllowedError"));
    seedItems([{ kind: "user", text: "复制失败我" }]);
    renderMsgs();
    fireEvent.click(document.querySelector('button[aria-label="复制"]')!);
    // 三级链路全失败 → 返回 false → 显失败图标，而不是静默无事发生
    await vi.waitFor(() => {
      expect(document.querySelector(".user-msg-actions .anticon-close")).toBeTruthy();
    });
  });

  it("复制：navigator.clipboard 不存在时不抛未捕获异常（旧写法的病根）", async () => {
    Object.defineProperty(navigator, "clipboard", { value: undefined, configurable: true });
    seedItems([{ kind: "user", text: "无 clipboard" }]);
    renderMsgs();
    expect(() => fireEvent.click(document.querySelector('button[aria-label="复制"]')!)).not.toThrow();
    await flush();
    Object.defineProperty(navigator, "clipboard", { value: { writeText }, configurable: true });
  });

  it("修改：派发 ws:composer-fill 携带原文与附件（Composer 侧监听回填）", () => {
    seedItems([{ kind: "user", text: "回填我", images: [{ mediaType: "image/png", data: "AAAA" }] }]);
    const received: unknown[] = [];
    const onFill = (e: Event) => received.push((e as CustomEvent).detail);
    window.addEventListener("ws:composer-fill", onFill);
    renderMsgs();
    fireEvent.click(document.querySelector('button[aria-label="修改"]')!);
    window.removeEventListener("ws:composer-fill", onFill);
    expect(received).toEqual([{ text: "回填我", images: [{ mediaType: "image/png", data: "AAAA" }] }]);
  });

  it("修改：纯文本消息派发 ws:composer-fill 的 images 为 undefined（Composer 侧覆盖清空现有附件）", () => {
    seedItems([{ kind: "user", text: "纯文本" }]);
    const received: unknown[] = [];
    const onFill = (e: Event) => received.push((e as CustomEvent).detail);
    window.addEventListener("ws:composer-fill", onFill);
    renderMsgs();
    fireEvent.click(document.querySelector('button[aria-label="修改"]')!);
    window.removeEventListener("ws:composer-fill", onFill);
    expect(received).toEqual([{ text: "纯文本", images: undefined }]);
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
