// run:inject 通知行显示注入正文（[docs/steer-run-inject](../../../docs/steer-run-inject.md)）：
// 载荷带 texts（用户从运行队列「↑ 立即」steer 进来的真实消息）时，通知行显示「注入 N 条消息：」并逐条渲染正文；
// texts 缺省 / 为空 / 全空白（ask 方案批准注入、旧后端）时逐字回落旧的单行通知。
//
// 两层断言：① handler 层（runHandlers 的 run:inject 落什么进 store）；② 渲染层（ChatMessages 怎么画）。
// 文案一律经 i18n.t 取期望值（不硬编码中英文），语言切换不会让用例失效（沿用历史状态通知测试的写法）。
import { describe, it, expect, vi, beforeEach, afterEach } from "vitest";
import { render, cleanup, screen } from "@testing-library/react";
import { App as AntApp } from "antd";
import { i18n } from "../i18n";
import { useRun } from "../stores/run";
import { useSessions } from "../stores/sessions";
import type { UiItem } from "../stores/run.types";
import { itemSig } from "../utils/scrollAnchor";

vi.mock("@tauri-apps/api/core", () => ({
  invoke: vi.fn(async () => null),
  Channel: class {
    onmessage: ((f: any) => void) | null = null;
  },
}));

import ChatMessages from "../features/chat/ChatMessages";

type Notice = Extract<UiItem, { kind: "notice" }>;

const SESSION = "s-inject-notice";

/** 事件 handler：唯一的 run:inject 入口（29 键事件面的成员） */
function inject(payload: any) {
  useRun.getState().bindGlobalHandlers()["run:inject"](payload);
}

/** 会话桶里的通知项 */
function notices(session = SESSION): Notice[] {
  return (useRun.getState().tabs[session]?.items ?? []).filter(
    (i) => i.kind === "notice",
  ) as Notice[];
}

function tabOf(session: string) {
  return useRun.getState().tabs[session]!;
}

// ---------- 夹具 ----------

function seedHandlerTab(session = SESSION) {
  useRun.setState((s) => {
    s.tabs[session] = {
      items: [],
      running: true,
      streamGen: 0,
      ask: null,
      breakdown: null,
      todos: [],
      suggestions: [],
      subs: [],
      subStreams: {},
      subDrawer: { open: false, subId: null },
      gitEntries: null,
      writeTick: 0,
      queue: [],
      pendingItemId: null,
      draftFromQueue: null,
      lastDoneRunId: null,
      compacting: false,
    };
  });
}

function seedRenderItem(item: UiItem, session = "s1") {
  useSessions.setState({
    tabs: [
      {
        key: session,
        sessionId: session,
        workspace: "/tmp/ws",
        title: session,
        projectId: null,
        createdAt: "2026-09-01T00:00:00Z",
        prefs: { approval_mode: "auto_edit", model_id: null, reasoning_effort: null },
      },
    ],
    activeKey: session,
    projects: [],
  });
  useRun.setState((s) => {
    s.tabs[session] = {
      items: [item],
      running: false,
      streamGen: 0,
      ask: null,
      breakdown: null,
      todos: [],
      suggestions: [],
      subs: [],
      subStreams: {},
      subDrawer: { open: false, subId: null },
      gitEntries: null,
      writeTick: 0,
      queue: [],
      pendingItemId: null,
      draftFromQueue: null,
      lastDoneRunId: null,
      compacting: false,
    };
  });
}

function renderChat() {
  return render(
    <AntApp>
      <ChatMessages />
    </AntApp>,
  );
}

/** 转录里那条通知的根节点 */
function noticeRoot(): HTMLElement {
  const el = document.querySelector(".notice-line") as HTMLElement | null;
  expect(el).not.toBeNull();
  return el!;
}

// zustand store 是模块级单例：测试间状态会残留，用例前后都重置（踩坑清单）
beforeEach(() => {
  seedHandlerTab();
});

afterEach(() => {
  cleanup();
  useRun.setState({ tabs: {}, drafts: {} });
  useSessions.setState({ tabs: [], activeKey: null, projects: [] });
});

// ---------- A. handler 层 ----------

describe("run:inject 通知行：注入正文（handler 层）", () => {
  it("A1 带 texts（1 条）：通知文案走 notice.injectedText，injectedTexts 即该数组", () => {
    inject({ session: SESSION, count: 1, texts: ["先别动索引，帮我看看迁移脚本"] });
    const list = notices();
    expect(list).toHaveLength(1);
    expect(list[0].text).toBe(i18n.t("notice.injectedText", { n: 1 }));
    expect(list[0].injectedTexts).toEqual(["先别动索引，帮我看看迁移脚本"]);
    // 与回落文案不同（不是「注入 N 条消息」那一句）
    expect(list[0].text).not.toBe(i18n.t("notice.injected", { n: 1 }));
  });

  it("A2 带 texts（2 条）：injectedTexts 长度 2 且顺序与载荷一致（FIFO，不排序不反转）", () => {
    const first = "第一条：接着上一轮继续";
    const second = "第二条：把结果贴到通知里";
    inject({ session: SESSION, count: 2, texts: [first, second] });
    const list = notices();
    expect(list).toHaveLength(1);
    expect(list[0].injectedTexts).toHaveLength(2);
    expect(list[0].injectedTexts).toEqual([first, second]);
    expect(list[0].text).toBe(i18n.t("notice.injectedText", { n: 2 }));
  });

  it("A3 无 texts 字段（旧后端 / ask 批准注入路径）：回落旧文案且不带 injectedTexts", () => {
    inject({ session: SESSION, count: 2 });
    const list = notices();
    expect(list).toHaveLength(1);
    expect(list[0].text).toBe(i18n.t("notice.injected", { n: 2 }));
    expect(list[0].injectedTexts).toBeUndefined();
    // 键本身都不该带进来（Object.keys 里无该字段，而非 undefined 值）
    expect(Object.keys(list[0])).not.toContain("injectedTexts");
  });

  it("A4 texts 为空数组：同样回落旧文案，不带 injectedTexts", () => {
    inject({ session: SESSION, count: 1, texts: [] });
    const list = notices();
    expect(list).toHaveLength(1);
    expect(list[0].text).toBe(i18n.t("notice.injected", { n: 1 }));
    expect(list[0].injectedTexts).toBeUndefined();
  });

  it("A5a texts 含空串 / 纯空白串 / 非字符串：过滤后仍渲染剩余正文，顺序不变", () => {
    inject({ session: SESSION, count: 4, texts: ["  ", "第一条有效", "", 42, null, "第二条有效  "] });
    const list = notices();
    expect(list).toHaveLength(1);
    expect(list[0].injectedTexts).toEqual(["第一条有效", "第二条有效  "]);
    expect(list[0].text).toBe(i18n.t("notice.injectedText", { n: 4 }));
  });

  it("A5b texts 过滤后全空：整体回落旧文案（不产生悬挂冒号的空正文通知）", () => {
    inject({ session: SESSION, count: 2, texts: ["", "   ", "\n\t"] });
    const list = notices();
    expect(list).toHaveLength(1);
    expect(list[0].text).toBe(i18n.t("notice.injected", { n: 2 }));
    expect(list[0].injectedTexts).toBeUndefined();
  });

  it("A6 count 缺失但有 texts：条数用 texts.length 兜底，文案里不出现 undefined", () => {
    const texts = ["甲", "乙"];
    inject({ session: SESSION, texts });
    const list = notices();
    expect(list[0].injectedTexts).toEqual(texts);
    expect(list[0].text).toBe(i18n.t("notice.injectedText", { n: texts.length }));
    expect(list[0].text).not.toContain("undefined");
    expect(list[0].text).not.toContain("NaN");
  });

  it("A7 载荷缺 session / 载荷本身为 undefined：静默 return，不抛异常也不落通知", () => {
    expect(() => inject(undefined)).not.toThrow();
    expect(() => inject(null)).not.toThrow();
    expect(() => inject({})).not.toThrow();
    expect(() => inject({ texts: ["孤儿正文"], count: 1 })).not.toThrow();
    expect(tabOf(SESSION).items).toHaveLength(0);
  });
});

// ---------- B. 渲染层 ----------

describe("run:inject 通知行：注入正文（ChatMessages 渲染层）", () => {
  it("B8 带 injectedTexts：通知行先出文案、再逐条渲染注入正文", () => {
    const texts = ["第一条：接着上一轮继续", "第二条：把结果贴到通知里"];
    const text = i18n.t("notice.injectedText", { n: 2 });
    seedRenderItem({ kind: "notice", text, injectedTexts: texts });
    renderChat();

    // 每条正文都可见（Tooltip 浮层按需挂载，不断言浮层，只断言触发元素的文本）
    expect(screen.getByText("第一条：接着上一轮继续")).toBeTruthy();
    expect(screen.getByText("第二条：把结果贴到通知里")).toBeTruthy();

    const root = noticeRoot();
    // 结构：外层 = [文案行, 正文容器]；正文容器里每条一个 div
    expect(root.children.length).toBe(2);
    expect(root.children[0].textContent).toBe(`· ${text}`);
    const body = root.children[1] as HTMLElement;
    expect(body.children.length).toBe(2);
    expect(body.children[0].textContent).toBe(texts[0]);
    expect(body.children[1].textContent).toBe(texts[1]);
    // 渲染顺序 = 载荷顺序
    expect(root.textContent).toContain(`${texts[0]}${texts[1]}`);
    // 长正文截断样式挂在每条上（悬停看全文由 Tooltip 承担）
    const line = body.children[0] as HTMLElement;
    expect(line.style.overflow).toBe("hidden");
    expect(line.style.textOverflow).toBe("ellipsis");
    expect(line.style.whiteSpace).toBe("nowrap");
  });

  it("B9 不带 injectedTexts：仍是单行「· {text}」，与改动前逐字一致（回归基线）", () => {
    const text = i18n.t("notice.injected", { n: 1 });
    seedRenderItem({ kind: "notice", text });
    renderChat();

    const root = noticeRoot();
    expect(root.textContent).toBe(`· ${text}`);
    // 单行形态：无任何子元素节点（旧实现就是单 div）
    expect(root.children.length).toBe(0);
    expect(root.querySelectorAll("div").length).toBe(0);
  });

  it("B10 injectedTexts 全为空串 / 纯空白：退化为单行，不渲染空的正文行", () => {
    const text = i18n.t("notice.injectedText", { n: 2 });
    seedRenderItem({ kind: "notice", text, injectedTexts: ["", "   "], });
    renderChat();

    const root = noticeRoot();
    expect(root.textContent).toBe(`· ${text}`);
    expect(root.children.length).toBe(0);
    expect(root.querySelectorAll("div").length).toBe(0);
  });

  it("B11 通知根节点带 data-key / data-sig（滚动锚点依赖，两种形态都不能丢）", () => {
    const withTexts = { kind: "notice" as const, text: i18n.t("notice.injectedText", { n: 1 }), injectedTexts: ["锚点正文"] };
    seedRenderItem(withTexts);
    renderChat();
    let root = noticeRoot();
    // 首项的稳定键 = itemKeysOf 的 live 序数；指纹 = itemSig（notice → n:{text}）
    expect(root.getAttribute("data-key")).toBe("live:0");
    expect(root.getAttribute("data-sig")).toBe(itemSig(withTexts));
    expect(root.getAttribute("data-key")).toBeTruthy();
    expect(root.getAttribute("data-sig")).toBeTruthy();
    cleanup();

    // 回落形态同样打锚点标
    const bare = { kind: "notice" as const, text: i18n.t("notice.injected", { n: 1 }) };
    seedRenderItem(bare);
    renderChat();
    root = noticeRoot();
    expect(root.getAttribute("data-key")).toBe("live:0");
    expect(root.getAttribute("data-sig")).toBe(itemSig(bare));
  });
});