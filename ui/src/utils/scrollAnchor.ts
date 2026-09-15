// 会话滚动锚点（会话保存与恢复优化 · 批1）：记录「视口顶端那条消息 + 段内像素偏移」，重开会话/切回 Tab 时还原。
// 为什么不用 scrollTop 绝对值：消息列表在恢复后需要重新渲染（markdown/代码块/图片异步撑高），
// 绝对滚动位置必然错位；锚在「某条消息 + 相对偏移」上，撑高后仍指向同一条消息的同一相对位置。
// 为什么锚点带 sig 指纹：items 数组里没有稳定 id（UiItem 无 id 字段），index 在列表被裁/被删时会漂移，
// 因此以内容指纹为准、index 只作精确定位的提示；指纹在列表中已不存在 ⇒ 调用方降级为贴底。
import type { UiItem } from "../stores/run.types";

/** 贴底判定阈值（与 ChatMessages 的跟随阈值一致） */
export const BOTTOM_EPS = 40;

/** 滚动锚点：bottom = 贴底（重开后自动贴底）；item = 顶端消息指纹 + 段内偏移 */
export type ScrollAnchor =
  | { kind: "bottom" }
  | { kind: "item"; idx: number; sig: string; offset: number };

/** 参与锚点计算的消息元素（由 DOM 读出的相对内容顶部的像素位置） */
export interface AnchorNode {
  idx: number;
  sig: string;
  /** 相对滚动内容顶部的像素位置（= 元素离滚动容器内容顶的偏移） */
  top: number;
}

/** 滚动容器几何（happy-dom 无布局，纯函数化以便单测直接构造） */
export interface AnchorGeometry {
  scrollTop: number;
  scrollHeight: number;
  clientHeight: number;
}

/** 消息内容指纹：kind + 时间 + 结构/文本特征。同内容消息可能撞指纹 —— 定位时先按 (sig, idx) 精确命中，再退回 sig。 */
export function itemSig(item: UiItem): string {
  switch (item.kind) {
    case "user":
      return `u:${item.createdAt ?? ""}:${item.text.length}:${item.text.slice(0, 24)}`;
    case "assistant": {
      const last = item.timeline[item.timeline.length - 1];
      const tail =
        !last ? "" : last.kind === "tool" ? `t:${last.callKey}` : last.kind === "sub" ? `s:${last.subId}` : `${last.text.length}:${last.text.slice(-24)}`;
      return `a:${item.createdAt ?? ""}:${item.timeline.length}:${tail}`;
    }
    case "sub":
      return `s:${item.subId}`;
    case "notice":
      return `n:${item.text}`;
    case "error":
      return `e:${item.text.slice(0, 40)}`;
  }
}

/** 视口顶端是否贴底（阈值内视为贴底） */
export function isAtBottom(g: AnchorGeometry): boolean {
  return g.scrollHeight - g.scrollTop - g.clientHeight < BOTTOM_EPS;
}

/** 计算当前锚点：视口贴底 ⇒ 贴底标记；否则取「顶端之前最近的一条消息」+ 段内偏移。
 *  消息全在视口下方（overscroll 到顶）时取第一条，偏移可能为负 —— 还原时由浏览器钳到 0。 */
export function computeAnchor(g: AnchorGeometry, nodes: AnchorNode[]): ScrollAnchor {
  if (isAtBottom(g)) return { kind: "bottom" };
  if (nodes.length === 0) return { kind: "bottom" };
  let best = nodes[0];
  for (const n of nodes) {
    if (n.top <= g.scrollTop + 1) best = n;
    else break;
  }
  return { kind: "item", idx: best.idx, sig: best.sig, offset: g.scrollTop - best.top };
}

/** 定位锚点目标消息：先按 (sig, idx) 精确命中，再退回 sig（列表增删导致 index 漂移）；
 *  都没有（消息被裁/被删/指纹不匹配）⇒ null，调用方降级为贴底。 */
export function pickTarget(nodes: AnchorNode[], anchor: ScrollAnchor): AnchorNode | null {
  if (anchor.kind !== "item") return null;
  return (
    nodes.find((n) => n.sig === anchor.sig && n.idx === anchor.idx) ??
    nodes.find((n) => n.sig === anchor.sig) ??
    null
  );
}

/** 从滚动容器读消息节点：消息根元素由 ChatMessages 打上 data-sig / data-idx；
 *  未打标的项（子代理卡等外部组件）自然被跳过，锚点落到其上一条消息，位置仍然正确。 */
export function collectNodes(el: HTMLElement): AnchorNode[] {
  const base = el.getBoundingClientRect().top - el.scrollTop;
  const out: AnchorNode[] = [];
  for (const node of Array.from(el.querySelectorAll<HTMLElement>("[data-sig]"))) {
    // top 相对「滚动内容顶部」：元素视口位置 - 容器视口位置 + 当前滚动量
    const top = node.getBoundingClientRect().top - base;
    out.push({ idx: Number(node.dataset.idx ?? "0"), sig: node.dataset.sig ?? "", top });
  }
  return out;
}

/** 读当前锚点（滚动防抖记录用） */
export function captureAnchor(el: HTMLElement): ScrollAnchor {
  return computeAnchor(
    { scrollTop: el.scrollTop, scrollHeight: el.scrollHeight, clientHeight: el.clientHeight },
    collectNodes(el),
  );
}

/** 还原锚点：返回是否命中消息锚（false = 已降级为贴底）。
 *  只负责「定位 + 叠加偏移」，二次校正（异步撑高后重定位）由调用方按帧重试本函数。 */
export function restoreAnchor(el: HTMLElement, anchor: ScrollAnchor | null | undefined): boolean {
  if (!anchor || anchor.kind === "bottom") {
    el.scrollTop = el.scrollHeight;
    return true;
  }
  const target = pickTarget(collectNodes(el), anchor);
  if (!target) {
    el.scrollTop = el.scrollHeight;
    return false;
  }
  el.scrollTop = target.top + anchor.offset;
  return true;
}
