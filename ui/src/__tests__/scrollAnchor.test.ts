// 滚动锚点（会话保存与恢复优化 · 批1）：
// 一、纯函数不变量：itemSig 稳定性与区分度 / isAtBottom 阈值边界 / computeAnchor 定位与偏移 /
//     pickTarget 命中与漂移回退 / collectNodes 打标收集 / restoreAnchor 命中与降级贴底 / capture → restore 往返。
// 二、ChatMessages 接线（DOM 层）：消息行 data-key/data-sig 标注 → 激活时按锚点还原 → 懒加载二次校正。
//     批2 P3 追加：分页前插更早内容后视口锚点不漂（稳定键不变 ⇒ 位置不漂）。
//     滚动记录走 uiState.scheduleAnchor（200ms 节流），用假计时器跑完整链路。
//
// happy-dom 没有真实布局：getBoundingClientRect / scrollHeight / clientHeight 全部手写桩。
// 桩里必须保持真实浏览器的这条关系——「节点视口 top = 内容偏移 - scrollTop，容器视口 top 恒为 0」——
// 否则测的是桩自己而不是 scrollAnchor 的算法。
import { afterAll, afterEach, beforeAll, describe, expect, it, vi } from "vitest";
import { createElement } from "react";
import { act, cleanup, render, waitFor } from "@testing-library/react";
import { App } from "antd";
import "../i18n";
import ChatMessages from "../features/chat/ChatMessages";
import { MAX_PAGED_PAGES, useRun } from "../stores/run";
import { useSessions } from "../stores/sessions";
import type { TimelineSeg, UiItem } from "../stores/run.types";
import type { Message } from "../ipc/types";
import {
  BOTTOM_EPS,
  bottomScrollTarget,
  captureAnchor,
  collectNodes,
  computeAnchor,
  isAtBottom,
  isSelfScroll,
  itemKeysOf,
  itemSig,
  pickTarget,
  restoreAnchor,
} from "../utils/scrollAnchor";
import type { AnchorGeometry, AnchorNode, ScrollAnchor } from "../utils/scrollAnchor";
import { getScrollAnchor, reset as resetUiState, setScrollAnchor } from "../utils/uiState";

// 唯一 invoke 入口必须 mock（不直接 mock @tauri-apps/api/core）——现场态落盘经由 uiState → ipc 走这里
const ipcMock = vi.hoisted(() => ({
  setUiState: vi.fn(async (_state: unknown): Promise<void> => {}),
  // 分页前翻（批2 P3）：默认「没有更早内容」，用例按需覆盖
  loadSessionEarlier: vi.fn(async (): Promise<{ messages: Message[]; from_seq: number; has_more: boolean }> => ({ messages: [], from_seq: 0, has_more: false })),
  loadToolOutcomes: vi.fn(async () => []),
}));
vi.mock("../ipc/client", () => ({ ipc: ipcMock }));

// ---------- 布局桩（happy-dom 无布局） ----------

/** 只填 top 的 DOMRect 桩：锚点只读 top，其余字段给齐以免类型断言外扩 */
function rect(top: number): DOMRect {
  return { top, y: top, bottom: top, left: 0, right: 0, width: 0, height: 0, x: 0, toJSON: () => ({}) } as DOMRect;
}

/** 覆写实例上的只读属性（happy-dom 的 scrollHeight/clientHeight 恒为 0，不覆写读不到东西） */
function defineSize(el: HTMLElement, prop: "scrollHeight" | "clientHeight", value: number): void {
  Object.defineProperty(el, prop, { configurable: true, writable: true, value });
}

/** rect 每次调用现算：还原改了 scrollTop 之后读数必须跟着变，否则往返测试是假绿 */
function defineRect(el: HTMLElement, topOf: () => number): void {
  Object.defineProperty(el, "getBoundingClientRect", { configurable: true, value: () => rect(topOf()) });
}

interface NodeSpec {
  /** 稳定键（批2 P3 起锚点身份；ChatMessages 打在 data-key 上） */
  key: string;
  sig: string;
  /** 该消息在滚动内容里的偏移 */
  top: number;
}

interface ScrollerSpec {
  scrollTop: number;
  scrollHeight: number;
  clientHeight: number;
  nodes: NodeSpec[];
  /** 未打标的兄弟节点（子代理卡等外部组件）：必须被 collectNodes 跳过 */
  untaggedTops?: number[];
}

/** 造一个「有布局」的滚动容器：容器视口顶恒在 0，节点视口 top = 内容偏移 - scrollTop */
function makeScroller(spec: ScrollerSpec): HTMLDivElement {
  const el = document.createElement("div");
  defineSize(el, "scrollHeight", spec.scrollHeight);
  defineSize(el, "clientHeight", spec.clientHeight);
  defineRect(el, () => 0);
  for (const top of spec.untaggedTops ?? []) {
    const child = document.createElement("div"); // 无 data-key：不可锚定
    defineRect(child, () => top - el.scrollTop);
    el.appendChild(child);
  }
  for (const n of spec.nodes) {
    const child = document.createElement("div");
    child.dataset.sig = n.sig;
    child.dataset.key = n.key;
    defineRect(child, () => n.top - el.scrollTop);
    el.appendChild(child);
  }
  el.scrollTop = spec.scrollTop; // happy-dom 的 scrollTop 可写且不钳制
  return el;
}

/** 造锚点拓扑：节点 i 的稳定键 k<i>、指纹 u<i>，内容偏移取自 tops（顺序即 DOM 顺序） */
function tops(list: number[]): AnchorNode[] {
  return list.map((top, i) => ({ key: `k${i}`, sig: `u${i}`, top }));
}

const geo = (scrollTop: number): AnchorGeometry => ({ scrollTop, scrollHeight: 1000, clientHeight: 400 });

const ISO = "2026-09-01T10:00:00Z";

function user(text: string, createdAt?: string): UiItem {
  return { kind: "user", text, createdAt };
}

function assistant(timeline: TimelineSeg[], createdAt?: string): UiItem {
  return { kind: "assistant", timeline, toolsMap: {}, streaming: false, createdAt };
}

describe("itemSig 消息指纹", () => {
  it("同一份 item 反复求值稳定（锚点靠跨渲染比较它）", () => {
    expect(itemSig(user("你好", ISO))).toBe(itemSig(user("你好", ISO)));
    expect(itemSig(assistant([{ kind: "text", text: "在" }], ISO))).toBe(
      itemSig(assistant([{ kind: "text", text: "在" }], ISO)),
    );
    expect(itemSig({ kind: "notice", text: "已压缩" })).toBe(itemSig({ kind: "notice", text: "已压缩" }));
  });

  it("不同条目指纹不同：文本 / 时间 / timeline 长度 / 末段内容", () => {
    expect(itemSig(user("A", ISO))).not.toBe(itemSig(user("B", ISO)));
    expect(itemSig(user("A", ISO))).not.toBe(itemSig(user("A", "2026-09-01T10:00:01Z")));
    expect(itemSig(assistant([{ kind: "text", text: "x" }], ISO))).not.toBe(
      itemSig(assistant([{ kind: "text", text: "y" }], ISO)),
    );
    expect(itemSig(assistant([{ kind: "text", text: "x" }], ISO))).not.toBe(
      itemSig(
        assistant(
          [
            { kind: "text", text: "x" },
            { kind: "text", text: "x" },
          ],
          ISO,
        ),
      ),
    );
  });

  it("assistant 指纹跟 timeline 末段走：工具卡/子代理卡取锚点 id，无段时为 0 段", () => {
    expect(itemSig({ kind: "assistant", timeline: [{ kind: "tool", callKey: "call-1" }], toolsMap: {}, streaming: true })).toBe(
      "a::1:t:call-1",
    );
    expect(itemSig({ kind: "assistant", timeline: [{ kind: "sub", subId: "sub-1" }], toolsMap: {}, streaming: true })).toBe(
      "a::1:s:sub-1",
    );
    expect(itemSig({ kind: "assistant", timeline: [], toolsMap: {}, streaming: true })).toBe("a::0:");
  });

  it("user 指纹只看「长度 + 前 24 字符」：前缀相同的等长消息会撞指纹（已知取舍，由 (sig, idx) 精确命中兜住）", () => {
    const head = "x".repeat(24);
    expect(itemSig(user(`${head}AAAA`, ISO))).toBe(itemSig(user(`${head}BBBB`, ISO)));
    expect(itemSig(user(`${head}AAAA`, ISO))).not.toBe(itemSig(user(`${head}AAAAA`, ISO)));
  });

  it("跨 kind 不混同：notice / error / sub / user / assistant 前缀各异", () => {
    const sigs = [
      itemSig({ kind: "notice", text: "同文本" }),
      itemSig({ kind: "error", text: "同文本" }),
      itemSig({ kind: "sub", subId: "同文本" }),
      itemSig(user("同文本")),
      itemSig(assistant([{ kind: "text", text: "同文本" }])),
    ];
    expect(sigs[0].startsWith("n:")).toBe(true);
    expect(sigs[1].startsWith("e:")).toBe(true);
    expect(sigs[2]).toBe("s:同文本");
    expect(new Set(sigs).size).toBe(sigs.length);
  });
});

describe("isAtBottom 贴底阈值（BOTTOM_EPS）", () => {
  it("离底 39 视为贴底；40 / 41 视为离开（严格小于阈值）", () => {
    expect(BOTTOM_EPS).toBe(40);
    expect(isAtBottom(geo(561))).toBe(true); // 1000 - 561 - 400 = 39
    expect(isAtBottom(geo(560))).toBe(false); // 40：阈值内不含边界
    expect(isAtBottom(geo(559))).toBe(false); // 41
  });

  it("内容不足一屏 / 恰好到底都算贴底（离底为非正数）", () => {
    expect(isAtBottom({ scrollTop: 0, scrollHeight: 300, clientHeight: 400 })).toBe(true);
    expect(isAtBottom(geo(600))).toBe(true);
  });
});

describe("computeAnchor 计算锚点", () => {
  it("贴底 ⇒ 贴底标记，不白算消息锚点", () => {
    expect(computeAnchor(geo(600), tops([0, 100, 200, 300]))).toEqual({ kind: "bottom" });
  });

  it("无节点且未贴底 ⇒ 仍为贴底标记（空列表没有可锚目标）", () => {
    expect(computeAnchor(geo(250), [])).toEqual({ kind: "bottom" });
  });

  it("中途 ⇒ 取视口顶端之上最近的一条消息 + 段内像素偏移", () => {
    expect(computeAnchor(geo(250), tops([0, 100, 200, 300]))).toEqual({
      kind: "item",
      key: "k2",
      sig: "u2",
      offset: 50,
    });
  });

  it("视口顶端恰好落在某条消息上 ⇒ 偏移 0", () => {
    expect(computeAnchor(geo(200), tops([0, 100, 200, 300]))).toEqual({ kind: "item", key: "k2", sig: "u2", offset: 0 });
  });

  it("消息全在视口下方（overscroll 到顶）⇒ 取第一条，偏移为负（由浏览器钳到 0）", () => {
    expect(computeAnchor(geo(0), tops([20, 120]))).toEqual({ kind: "item", key: "k0", sig: "u0", offset: -20 });
  });

  it("分页前插更早内容：按稳定键重定位到同一条消息的同一相对位置（AC-13）", () => {
    const before: AnchorNode[] = [
      { key: "s1:0", sig: "u0", top: 0 },
      { key: "s1:1", sig: "u1", top: 100 },
    ];
    const anchor = computeAnchor({ scrollTop: 130, scrollHeight: 1000, clientHeight: 600 }, before);
    expect(anchor).toEqual({ kind: "item", key: "s1:1", sig: "u1", offset: 30 });
    if (anchor.kind !== "item") throw new Error("预期消息锚点");

    // 前插两段更早内容（+200px）：键不变，只有内容偏移变大
    const after: AnchorNode[] = [
      { key: "s0:0", sig: "x0", top: 0 },
      { key: "s0:1", sig: "x1", top: 100 },
      { key: "s1:0", sig: "u0", top: 200 },
      { key: "s1:1", sig: "u1", top: 300 },
    ];
    const target = pickTarget(after, anchor)!;
    expect(target.key).toBe("s1:1");
    // 旧位置 130 + 前插高度 200 = 330：视口相对该消息的偏移仍为 30px，没有漂
    expect(target.top + anchor.offset).toBe(330);
  });
});

describe("itemKeysOf 稳定渲染键（AC-14）", () => {
  it("恢复前缀用「段号 + 段内序号」，其余按到达顺序取 live 序数", () => {
    const keys = itemKeysOf([user("a"), user("b"), user("c")], ["s2:0", "s2:1"]);
    expect(keys).toEqual(["s2:0", "s2:1", "live:0"]);
  });

  it("无恢复前缀（新会话 / 未分页）⇒ 全部 live 序数，与下标一致且不重复", () => {
    const keys = itemKeysOf([user("a"), user("b")]);
    expect(keys).toEqual(["live:0", "live:1"]);
    expect(new Set(keys).size).toBe(2);
  });

  it("前插更早内容（AC-14）：已有项的键一个不变（数组下标会整体漂移）", () => {
    const older = [user("更早一"), user("更早二")];
    const current = [user("第一句"), user("第二句")];
    const before = itemKeysOf(current, ["s2:0", "s2:1"]);
    // 前插：键表头部接上更早一段，已有项保持在原位
    const after = itemKeysOf([...older, ...current], ["s1:0", "s1:1", "s2:0", "s2:1"]);
    expect(after.slice(older.length)).toEqual(before);
    expect(new Set(after).size).toBe(after.length);
    // 反证：按下标作 key 时，同一条消息的 key 从 0/1 漂到了 2/3
    expect(after.slice(older.length)).not.toEqual(["0", "1"]);
  });

  it("尾部追加（流式）：已有项的键同样不变", () => {
    const items = [user("第一句"), user("第二句")];
    const before = itemKeysOf(items, ["s0:0", "s0:1"]);
    const after = itemKeysOf([...items, user("第三句")], ["s0:0", "s0:1"]);
    expect(after.slice(0, 2)).toEqual(before);
    expect(after[2]).toBe("live:0");
  });

  it("恢复前缀短于 items（尾部的 live 项）时前缀键仍逐位对应", () => {
    expect(itemKeysOf([user("a"), user("b"), user("c")], ["s0:0"])).toEqual(["s0:0", "live:0", "live:1"]);
  });
});

describe("pickTarget 定位锚点目标", () => {
  it("稳定键精确命中优先：同内容消息撞指纹时靠它区分", () => {
    const a = { key: "kA", sig: "dup", top: 0 };
    const b = { key: "kB", sig: "dup", top: 100 };
    expect(pickTarget([a, b], { kind: "item", key: "kB", sig: "dup", offset: 3 })).toBe(b);
  });

  it("键已不存在（分页前插后键漂、或随「收起更早的」被丢弃）⇒ 退回指纹命中", () => {
    const nodes = tops([0, 100]);
    expect(pickTarget(nodes, { kind: "item", key: "k9", sig: "u0", offset: 3 })).toBe(nodes[0]);
  });

  it("批1 旧快照锚点（有 idx、无 key）：key 缺失不误命中，按指纹兜底", () => {
    const nodes = tops([0, 100]);
    const legacy = { kind: "item", sig: "u1", offset: 3 } as unknown as ScrollAnchor;
    expect(pickTarget(nodes, legacy)).toBe(nodes[1]);
  });

  it("键与指纹都已不存在（被裁/被删）⇒ null，调用方据此降级贴底", () => {
    expect(pickTarget(tops([0, 100]), { kind: "item", key: "k9", sig: "gone", offset: 3 })).toBeNull();
  });

  it("贴底锚点 / 空列表 ⇒ null", () => {
    expect(pickTarget(tops([0, 100]), { kind: "bottom" })).toBeNull();
    expect(pickTarget([], { kind: "item", key: "k0", sig: "u0", offset: 0 })).toBeNull();
  });
});

describe("collectNodes 收集可锚定节点", () => {
  it("只收带 data-key 的节点，未打标项（子代理卡）自然跳过；top 相对内容顶部，与当前 scrollTop 无关", () => {
    const el = makeScroller({
      scrollTop: 250,
      scrollHeight: 1000,
      clientHeight: 400,
      nodes: [
        { key: "s1:0", sig: "u0", top: 0 },
        { key: "s1:1", sig: "a1", top: 120 },
      ],
      untaggedTops: [60],
    });
    const at250 = collectNodes(el);
    expect(at250).toEqual([
      { key: "s1:0", sig: "u0", top: 0 },
      { key: "s1:1", sig: "a1", top: 120 },
    ]);
    el.scrollTop = 0; // 滚回顶部：内容坐标不该跟着漂
    expect(collectNodes(el)).toEqual(at250);
  });
});

describe("captureAnchor 读现场锚点", () => {
  it("中途 ⇒ 消息锚点；贴底 ⇒ 贴底标记", () => {
    const el = makeScroller({
      scrollTop: 250,
      scrollHeight: 1000,
      clientHeight: 400,
      nodes: [
        { key: "k0", sig: "u0", top: 0 },
        { key: "k1", sig: "u1", top: 100 },
        { key: "k2", sig: "u2", top: 200 },
        { key: "k3", sig: "u3", top: 300 },
      ],
      untaggedTops: [150],
    });
    expect(captureAnchor(el)).toEqual({ kind: "item", key: "k2", sig: "u2", offset: 50 });
    el.scrollTop = 600;
    expect(captureAnchor(el)).toEqual({ kind: "bottom" });
  });
});

describe("restoreAnchor 还原锚点", () => {
  const scroller = () =>
    makeScroller({
      scrollTop: 0,
      scrollHeight: 1000,
      clientHeight: 400,
      nodes: [
        { key: "k0", sig: "u0", top: 0 },
        { key: "k1", sig: "u1", top: 100 },
        { key: "k2", sig: "u2", top: 200 },
        { key: "k3", sig: "u3", top: 300 },
      ],
    });

  it("命中消息锚 ⇒ 定位到「该消息 + 段内偏移」并返回 true", () => {
    const el = scroller();
    expect(restoreAnchor(el, { kind: "item", key: "k2", sig: "u2", offset: 50 })).toBe(true);
    expect(el.scrollTop).toBe(250);
  });

  it("锚点消息已被裁/被删 ⇒ 返回 false 并降级贴底（content 已变，位置不再可信）", () => {
    const el = scroller();
    const stale: ScrollAnchor = { kind: "item", key: "k9", sig: "gone", offset: 12 };
    expect(restoreAnchor(el, stale)).toBe(false);
    expect(el.scrollTop).toBe(el.scrollHeight);
  });

  it("null / 贴底锚点 ⇒ 返回 true 并贴底（贴底与无锚同语义，调用方无需分叉）", () => {
    const el = scroller();
    expect(restoreAnchor(el, null)).toBe(true);
    expect(el.scrollTop).toBe(el.scrollHeight);
    el.scrollTop = 0;
    expect(restoreAnchor(el, { kind: "bottom" })).toBe(true);
    expect(el.scrollTop).toBe(el.scrollHeight);
  });

  it("capture → restore 往返幂等：位置回到原处，再还原一次不动", () => {
    const el = scroller();
    el.scrollTop = 250;
    const anchor = captureAnchor(el);
    expect(anchor).toEqual({ kind: "item", key: "k2", sig: "u2", offset: 50 });
    el.scrollTop = 0; // 模拟切走后被别人滚过
    expect(restoreAnchor(el, anchor)).toBe(true);
    expect(el.scrollTop).toBe(250);
    expect(restoreAnchor(el, anchor)).toBe(true); // 幂等：重放不叠加偏移
    expect(el.scrollTop).toBe(250);
  });
});

describe("程序化跳底的目标与豁免判定（[docs/chat-autoscroll-regression-fix](../../../docs/chat-autoscroll-regression-fix.md)）", () => {
  it("bottomScrollTarget 取浏览器实际落点（scrollHeight - clientHeight，钳到 0）", () => {
    expect(bottomScrollTarget({ scrollHeight: 1000, clientHeight: 600 })).toBe(400);
    expect(bottomScrollTarget({ scrollHeight: 300, clientHeight: 600 })).toBe(0); // 内容不足一屏
  });

  it("落点与 scrollHeight 的差值恒为 clientHeight —— 单点比对（旧实现）必然失败", () => {
    const g = { scrollHeight: 1000, clientHeight: 600 };
    const landing = bottomScrollTarget(g);
    // 旧实现拿未钳的 scrollHeight 当目标：|400 - 1000| = 600 ≫ 40 ⇒ 自家跳底被当成用户接管
    expect(Math.abs(landing - g.scrollHeight)).toBe(g.clientHeight);
    expect(Math.abs(landing - g.scrollHeight) < BOTTOM_EPS).toBe(false);
    // 新实现：目标就是落点
    expect(isSelfScroll(landing, landing, landing)).toBe(true);
  });

  it("跳底之后内容又长：位置停在旧落点，仍须判为自家事件", () => {
    // 跳底时 1000/600（落点 400），scroll 事件派发前内容长到 1600：scrollTop 仍是 400
    expect(isSelfScroll(400, 400, 400)).toBe(true);
    // 同一时刻按几何判定「是否贴底」为 false —— 这正是缺陷链条的第二步（跟随被关掉）
    expect(isAtBottom({ scrollTop: 400, scrollHeight: 1600, clientHeight: 600 })).toBe(false);
  });

  it("平滑滚动区间内的中间位置算自家事件；越过区间（用户上滚）不算", () => {
    expect(isSelfScroll(120, 0, 400)).toBe(true); // 动画中（from=0 → target=400）
    expect(isSelfScroll(400, 0, 400)).toBe(true); // 动画结束
    expect(isSelfScroll(370, 400, 400)).toBe(true); // 容差内（BOTTOM_EPS=40）
    expect(isSelfScroll(100, 400, 400)).toBe(false); // 用户上滚 300px：交出控制权
  });
});

// ---------- ChatMessages 接线（DOM 层） ----------

/** 消息行高度（固定值：内容偏移 = idx × ROW）；容器一屏 600px、内容 1000px ⇒ 可滚 */
const ROW = 100;
const SCROLL_HEIGHT = 1000;
const CLIENT_HEIGHT = 600;

function blankTab(items: UiItem[], itemKeys?: string[]) {
  return {
    items,
    itemKeys,
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
}

function seedTab(items: UiItem[], key = "s1", itemKeys?: string[]): void {
  useSessions.setState({ activeKey: key });
  useRun.setState((s) => {
    s.tabs[key] = blankTab(items, itemKeys);
  });
}

/** 本文件按任务包指定为 .ts（JSX 只在 .tsx 可用）：用 createElement 搭最小挂载树 */
const mount = () => render(createElement(App, null, createElement(ChatMessages)));

describe("ChatMessages 接线：标注 / 还原 / 懒加载二次校正", () => {
  // 渲染期布局：.chat-messages 一屏 600px（内容 1000px），消息行按**兄弟顺序**每行 100px 排布。
  // 行 rect 必须随 scrollTop 现算（真实浏览器行为），否则二次校正会被自己的桩骗成「已经命中」。
  const proto = HTMLElement.prototype as unknown as Record<string, unknown>;
  const savedRect = Object.getOwnPropertyDescriptor(HTMLElement.prototype, "getBoundingClientRect");
  const savedScrollHeight = Object.getOwnPropertyDescriptor(HTMLElement.prototype, "scrollHeight");
  const savedClientHeight = Object.getOwnPropertyDescriptor(HTMLElement.prototype, "clientHeight");

  beforeAll(() => {
    Object.defineProperty(HTMLElement.prototype, "getBoundingClientRect", {
      configurable: true,
      value(this: HTMLElement) {
        // 行高按**兄弟顺序**（DOM 顺序）排布：批2 P3 起锚点身份是稳定键，从 data-key 推不出像素位置；
        // 按兄弟顺序算 = 前插更早内容后老行的内容偏移自然变大（正是真实的浏览器行为）。
        // 容器视口顶恒为 0（见文件头约定）：少了这条短路，容器自己也会按「兄弟序号 × ROW - scrollTop」
        // 编出一个假 rect，collectNodes 的 base 随之偏掉 -scrollTop，**所有消息的内容偏移整体漂移 scrollTop**
        // （锚点会落到错的项上、位置全错）。
        if (this.classList?.contains("chat-messages")) return rect(0);
        const parent = this.parentElement;
        if (parent) {
          const at = [...parent.children].indexOf(this);
          if (at >= 0) {
            const scroller = this.closest?.(".chat-messages") as HTMLElement | null;
            return rect(at * ROW - (scroller?.scrollTop ?? 0));
          }
        }
        return rect(0);
      },
    });
    Object.defineProperty(HTMLElement.prototype, "scrollHeight", {
      configurable: true,
      get(this: HTMLElement) {
        return this.classList?.contains("chat-messages") ? SCROLL_HEIGHT : 0;
      },
    });
    Object.defineProperty(HTMLElement.prototype, "clientHeight", {
      configurable: true,
      get(this: HTMLElement) {
        return this.classList?.contains("chat-messages") ? CLIENT_HEIGHT : 0;
      },
    });
  });

  afterAll(() => {
    if (savedRect) Object.defineProperty(HTMLElement.prototype, "getBoundingClientRect", savedRect);
    else delete proto.getBoundingClientRect;
    if (savedScrollHeight) Object.defineProperty(HTMLElement.prototype, "scrollHeight", savedScrollHeight);
    else delete proto.scrollHeight;
    if (savedClientHeight) Object.defineProperty(HTMLElement.prototype, "clientHeight", savedClientHeight);
    else delete proto.clientHeight;
  });

  afterEach(() => {
    cleanup();
    useSessions.setState({ activeKey: null, tabs: [] });
    useRun.setState((s) => {
      s.tabs = {};
      s.drafts = {};
    });
    resetUiState(); // uiState 是模块级单例：锚点表/计时器测试间不串味
  });

  it("消息行带 data-key/data-sig；激活时按锚点还原到「该消息 + 段内偏移」", () => {
    const items: UiItem[] = [user("第一句", ISO), assistant([{ kind: "text", text: "第二句" }], ISO)];
    seedTab(items, "s1", ["s1:0", "s1:1"]);
    // 上次离开在第 1 条消息（内容偏移 100）之下 30px
    setScrollAnchor("s1", { kind: "item", key: "s1:1", sig: itemSig(items[1]), offset: 30 });

    const { container } = mount();
    const scroller = container.querySelector<HTMLElement>(".chat-messages")!;

    // 标注：恢复用的稳定键（段号 + 段内序号）打在 data-key；data-sig 作指纹兜底
    const rows = [...container.querySelectorAll<HTMLElement>(".msg[data-key]")];
    expect(rows.map((r) => r.dataset.key)).toEqual(["s1:0", "s1:1"]);
    expect(rows.map((r) => r.dataset.sig)).toEqual([itemSig(items[0]), itemSig(items[1])]);
    expect(collectNodes(scroller)).toEqual([
      { key: "s1:0", sig: itemSig(items[0]), top: 0 },
      { key: "s1:1", sig: itemSig(items[1]), top: ROW },
    ]);

    // 还原：消息内容偏移 100 + 段内偏移 30；位置远未贴底 ⇒ 停掉跟随并显示「滚动到底部」
    expect(scroller.scrollTop).toBe(130);
    expect(container.querySelector('button[aria-label="滚动到底部"]')).toBeTruthy();
  });

  it("未分页（无 itemKeys）时回落 live 序数键，且互不重复", () => {
    const items: UiItem[] = [user("第一句", ISO), assistant([{ kind: "text", text: "第二句" }], ISO)];
    seedTab(items);
    const { container } = mount();
    const rows = [...container.querySelectorAll<HTMLElement>(".msg[data-key]")];
    expect(rows.map((r) => r.dataset.key)).toEqual(["live:0", "live:1"]);
  });

  it("分页前插更早内容（AC-13）：点「加载更早的消息」后视口仍锚在同一条消息上", async () => {
    const later: UiItem[] = [user("第一句", ISO), assistant([{ kind: "text", text: "第二句" }], ISO)];
    const earlierRow: Message[] = [{ role: "user", content: [{ type: "text", text: "更早一" }] }];
    // 前翻一页：真实后端只回消息，段号与项表由 store 生成
    ipcMock.loadSessionEarlier.mockResolvedValue({ messages: earlierRow, from_seq: 1, has_more: false });
    seedTab(later, "s1", ["s2:0", "s2:1"]);
    useRun.setState((s) => {
      s.tabs.s1.paging = {
        format: "new", loadedFromSeq: 2, firstLoadedSeq: 2, hasMore: true,
        totalMessages: 3, segmentCount: 2, loadedPages: 1, loading: false,
      };
    });

    const { container } = mount();
    const scroller = container.querySelector<HTMLElement>(".chat-messages")!;
    // 用户停在「第二句」上（内容偏移 100）之下 30px
    act(() => {
      scroller.scrollTop = 130;
      scroller.dispatchEvent(new Event("scroll"));
    });

    const btn = [...container.querySelectorAll("button")].find((b) => (b.textContent ?? "").replace(/\s/g, "").includes("加载更早的消息"))!;
    expect(btn).toBeTruthy();
    await act(async () => {
      btn.click();
    });

    // 更早一段到位：前插一条（键 s1:*），已有一段的键一字不变（s2:*）
    const rows = [...container.querySelectorAll<HTMLElement>(".msg[data-key]")];
    expect(rows.map((r) => r.dataset.key)).toEqual(["s1:0", "s2:0", "s2:1"]);
    expect(useRun.getState().tabs.s1.paging!.loadedFromSeq).toBe(1);
    // 视口没漂：同一行的内容偏移整体 +100（前插高度），scrollTop 同步 +100 ⇒ 相对偏移仍为 30px
    expect(scroller.scrollTop).toBe(230);

    // 终态组合（has_more 翻 false + 已经翻过页）：入口换成「已到最早的消息」，「收起更早的」仍在
    // —— 分页条的门只看「是不是段式会话」，hasMore 翻 false 不该让整行消失（AC-12）
    const strip = [...container.querySelectorAll("button")].map((b) => (b.textContent ?? "").replace(/\s/g, ""));
    expect(strip.some((x) => x.includes("加载更早的消息"))).toBe(false);
    expect(strip.some((x) => x.includes("收起更早的"))).toBe(true);
    expect(container.querySelector(".paging-bar")?.textContent).toContain("已到最早的消息");
  });

  it("最早段终态：has_more 为 false 时不显示入口，改显「已到最早的消息」（AC-12）", () => {
    const items: UiItem[] = [user("第一句", ISO)];
    seedTab(items, "s1", ["s1:0"]);
    useRun.setState((s) => {
      s.tabs.s1.paging = {
        format: "new", loadedFromSeq: 1, firstLoadedSeq: 1, hasMore: false,
        totalMessages: 1, segmentCount: 1, loadedPages: 1, loading: false,
      };
    });
    const { container } = mount();
    const buttons = [...container.querySelectorAll("button")].map((b) => b.textContent ?? "").join("|");
    expect(buttons).not.toContain("加载更早的消息");
    expect(container.textContent).toContain("已到最早的消息");
  });

  it("分页上限：到 MAX_PAGED_PAGES 后不再给入口，改显上限说明（仍可收起更早的）", () => {
    const items: UiItem[] = [user("第一句", ISO)];
    seedTab(items, "s1", ["s1:0"]);
    useRun.setState((s) => {
      s.tabs.s1.paging = {
        format: "new", loadedFromSeq: 1, firstLoadedSeq: 1, hasMore: true,
        totalMessages: 900, segmentCount: MAX_PAGED_PAGES + 1, loadedPages: MAX_PAGED_PAGES, loading: false,
      };
    });
    const { container } = mount();
    const buttons = [...container.querySelectorAll("button")].map((b) => b.textContent ?? "").join("|");
    expect(buttons).not.toContain("加载更早的消息");
    expect(container.textContent).toContain(`已达单次浏览上限（${MAX_PAGED_PAGES} 段）`);
    // 已翻过页（loadedPages > 1）⇒ 收起入口仍在，用户能把内存收回首屏那一段
    expect(buttons).toContain("收起更早的");
  });

  it("legacy 会话：无分页入口、无异味（行为与改动前一致）", () => {
    const items: UiItem[] = [user("第一句", ISO), assistant([{ kind: "text", text: "第二句" }], ISO)];
    seedTab(items, "s1", ["s0:0", "s0:1"]);
    useRun.setState((s) => {
      s.tabs.s1.paging = {
        format: "legacy", loadedFromSeq: 0, firstLoadedSeq: 0, hasMore: false,
        totalMessages: 2, segmentCount: 1, loadedPages: 1, loading: false,
      };
    });
    const { container } = mount();
    expect(container.querySelectorAll(".msg[data-key]")).toHaveLength(2);
    expect(container.textContent).not.toContain("加载更早的消息");
    expect(container.textContent).not.toContain("已到最早的消息");
  });

  it("未分页（无 paging / 旧后端）：不显示入口，既有转录照常渲染", () => {
    const items: UiItem[] = [user("第一句", ISO)];
    seedTab(items);
    const { container } = mount();
    expect(container.querySelectorAll(".msg[data-key]")).toHaveLength(1);
    expect(container.textContent).not.toContain("加载更早的消息");
  });

  it("前翻失败：入口仍在（可重试），已有转录一字不动（AC-12 边界）", async () => {
    const items: UiItem[] = [user("第一句", ISO)];
    ipcMock.loadSessionEarlier.mockRejectedValue(new Error("network down"));
    seedTab(items, "s1", ["s2:0"]);
    useRun.setState((s) => {
      s.tabs.s1.paging = {
        format: "new", loadedFromSeq: 2, firstLoadedSeq: 2, hasMore: true,
        totalMessages: 3, segmentCount: 2, loadedPages: 1, loading: false,
      };
    });
    const { container } = mount();
    await act(async () => {
      await useRun.getState().loadEarlier("s1");
    });
    const t = useRun.getState().tabs.s1;
    expect(t.items).toHaveLength(1);
    expect(t.itemKeys).toEqual(["s2:0"]);
    expect(t.paging!.failed).toBe(true);
    expect(t.paging!.hasMore).toBe(true); // 仍可重试
    const btn = [...container.querySelectorAll("button")].find((b) => (b.textContent ?? "").replace(/\s/g, "").includes("加载更早的消息"));
    expect(btn).toBeTruthy();
  });

  it("懒加载：首帧无内容不落位（不闪底部），消息到达后二次校正到锚点", async () => {
    const items: UiItem[] = [user("第一句", ISO), assistant([{ kind: "text", text: "第二句" }], ISO)];
    seedTab([]);
    setScrollAnchor("s1", { kind: "item", key: "s1:1", sig: itemSig(items[1]), offset: 30 });

    const { container } = mount();
    const scroller = container.querySelector<HTMLElement>(".chat-messages")!;
    // 无节点可锚 ⇒ 原地不动（若当场降级贴底会先跳到 1000 再跳回 130，白闪一下）
    expect(scroller.scrollTop).toBe(0);

    // items 0 → N：布局就位后由二次校正落位
    act(() => {
      useRun.setState((s) => {
        s.tabs.s1.items = items;
      });
    });
    await waitFor(() => expect(scroller.scrollTop).toBe(130));
  });

  it("滚动记录走 scheduleAnchor（200ms 节流）：落盘的锚点就是现场位置", async () => {
    vi.useFakeTimers();
    try {
      const items: UiItem[] = [user("第一句", ISO), assistant([{ kind: "text", text: "第二句" }], ISO)];
      // 段式会话（带恢复前缀）：锚点身份是稳定键，故这里必须给出与 items 对齐的键（段号 + 段内序号）
      seedTab(items, "s1", ["s1:0", "s1:1"]);
      const { container } = mount();
      const scroller = container.querySelector<HTMLElement>(".chat-messages")!;
      scroller.scrollTop = 160; // 用户滚到第 2 条消息（内容偏移 100）之下 60px
      act(() => {
        scroller.dispatchEvent(new Event("scroll"));
      });
      await act(async () => {
        await vi.advanceTimersByTimeAsync(250);
      });
      expect(getScrollAnchor("s1")).toEqual({ kind: "item", key: "s1:1", sig: itemSig(items[1]), offset: 60 });
    } finally {
      vi.useRealTimers();
    }
  });

  it("防抖窗口内切 Tab：旧会话不会被写上别人的几何（reader 的会话校验）", async () => {
    vi.useFakeTimers();
    try {
      const a: UiItem[] = [user("A1", ISO), assistant([{ kind: "text", text: "A2" }], ISO)];
      const b: UiItem[] = [user("B1", ISO), assistant([{ kind: "text", text: "B2" }], ISO)];
      seedTab(b, "s2");
      seedTab(a, "s1");
      const { container } = mount();
      const scroller = container.querySelector<HTMLElement>(".chat-messages")!;
      scroller.scrollTop = 160;
      act(() => {
        scroller.dispatchEvent(new Event("scroll")); // s1 的记录进入 200ms 防抖窗口
      });
      // 窗口内切到 s2：DOM 里已是 s2 的消息，若 reader 不校验会话就会把 s2 的几何写给 s1
      act(() => {
        useSessions.setState({ activeKey: "s2" });
      });
      await act(async () => {
        await vi.advanceTimersByTimeAsync(250);
      });
      expect(getScrollAnchor("s1")).toEqual({ kind: "bottom" });
    } finally {
      vi.useRealTimers();
    }
  });

  it("程序化跳底后的自家 scroll 事件不得被误判为用户接管（内容在事件派发前又长了一截）", async () => {
    // 真实浏览器的两条事实 happy-dom 都没有，必须自己搭桩，否则这条缺陷测不出来：
    // ① scrollTop 会被钳到 scrollHeight - clientHeight；
    // ② scroll 事件在下一帧才派发，期间内容可能又长了（流式场景几乎必然）。
    const items: UiItem[] = [user("第一句", ISO), assistant([{ kind: "text", text: "第二句" }], ISO)];
    seedTab(items);
    const { container } = mount();
    const scroller = container.querySelector<HTMLElement>(".chat-messages")!;

    let height = SCROLL_HEIGHT;
    let top = 0;
    const maxTop = () => Math.max(0, height - CLIENT_HEIGHT);
    Object.defineProperty(scroller, "scrollHeight", { configurable: true, get: () => height });
    Object.defineProperty(scroller, "clientHeight", { configurable: true, get: () => CLIENT_HEIGHT });
    Object.defineProperty(scroller, "scrollTop", {
      configurable: true,
      get: () => top,
      set: (v: number) => {
        top = Math.max(0, Math.min(v, maxTop()));
      },
    });
    (scroller as unknown as { scrollTo: (o: { top: number }) => void }).scrollTo = (o) => {
      top = Math.max(0, Math.min(o.top, maxTop()));
    };

    // 内容增长：贴底态 ⇒ 跟随 effect 程序化跳底（落点 = 1000 - 600 = 400）
    act(() => {
      useRun.setState((s) => {
        s.tabs.s1.items = [...items, user("第三句", ISO)];
      });
    });
    await waitFor(() => expect(top).toBe(maxTop()));

    // 事件派发前内容又长到 1600：scrollTop 仍是 400（浏览器不会因为内容变高就重新滚）
    act(() => {
      height = 1600;
    });
    act(() => {
      scroller.dispatchEvent(new Event("scroll"));
    });

    // 自家事件 ⇒ 跟随不得中断（不出现「回到底部」按钮）
    expect(container.querySelector('button[aria-label="滚动到底部"]')).toBeFalsy();
    // 反证：此刻按几何判定并不贴底 —— 正是旧实现把它当成「用户滚走了」的那一步
    expect(isAtBottom({ scrollTop: top, scrollHeight: height, clientHeight: CLIENT_HEIGHT })).toBe(false);

    // 另一半：跟随仍生效 —— 再长一次内容，视图应被重新拉到底
    act(() => {
      useRun.setState((s) => {
        // 基于当前 items 追加：直接写 [...items, …] 会把上一步的「第三句」顶掉，长度不变 ⇒ 跟随 effect 不会重跑
        s.tabs.s1.items = [...s.tabs.s1.items, user("第四句", ISO)];
      });
    });
    await waitFor(() => expect(top).toBe(maxTop()));
    expect(container.querySelector('button[aria-label="滚动到底部"]')).toBeFalsy();
  });
});
