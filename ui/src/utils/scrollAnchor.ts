// 会话滚动锚点（会话保存与恢复优化 · 批1）：记录「视口顶端那条消息 + 段内像素偏移」，重开会话/切回 Tab 时还原。
// 为什么不用 scrollTop 绝对值：消息列表在恢复后需要重新渲染（markdown/代码块/图片异步撑高），
// 绝对滚动位置必然错位；锚在「某条消息 + 相对偏移」上，撑高后仍指向同一条消息的同一相对位置。
//
// 批2 P3（分段分页）后锚点身份改用**稳定键**（`key` = 段号 + 段内序号，见 itemKeysOf）：
// 分页会把更早的段**前插**到列表头部，一切基于数组下标的定位都会整体漂移，而稳定键在前插与尾部追加下都不变。
// `sig`（内容指纹）降为**兼容兜底**：批1 写进 ui-state.json 的旧锚点没有 key 字段，
// 或锚点项已随「收起更早的」被丢弃时，按指纹就近命中；两者都没有 ⇒ 调用方降级为贴底。
import type { UiItem } from "../stores/run.types";
import type { HistoryBoundary } from "../ipc/types";

/** 贴底判定阈值（与 ChatMessages 的跟随阈值一致）；也是程序化滚动豁免的容差 */
export const BOTTOM_EPS = 40;

/** 滚动锚点：bottom = 贴底（重开后自动贴底）；item = 顶端消息的**稳定键**（+ 内容指纹兼容位）+ 段内偏移 */
export type ScrollAnchor =
  | { kind: "bottom" }
  | { kind: "item"; key: string; sig: string; offset: number };

/** 参与锚点计算的消息元素（由 DOM 读出的相对内容顶部的像素位置） */
export interface AnchorNode {
  /** 稳定键（ChatMessages 打在 data-key 上；= itemKeysOf 的产物） */
  key: string;
  /** 内容指纹（兼容位：旧锚点没有 key 时按它就近命中） */
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

/** 转录项的稳定渲染键（批2 P3 AC-14）——React 渲染 key 与滚动锚点**共用同一套**身份。
 *
 *  `restored` = 恢复自磁盘的「前缀键」（`s<段号>:<段内序>`，由 run.ts 的 buildTranscript 生成），
 *  与 items 头部对齐；其余（本次运行新产生的项）按到达顺序取 `live:<序数>`。
 *
 *  为什么这样就稳定：分页**只在头部插入**（前缀变长，已有项的相对次序不变），
 *  运行期**只在尾部追加**（live 序数递增，已有项的序数不变）——两种情况都不改动任何已有键。
 *  数组下标两头都会漂移（这正是 AC-14 要修的问题）；内容指纹（itemSig）会随流式增量每帧变化，
 *  当 key 会让流式项反复重建，故也不可用。
 *
 *  调用方须与 items 同序使用：`itemKeysOf(items, keys)[i]` ↔ `items[i]`。 */
export function itemKeysOf(items: readonly UiItem[], restored?: readonly string[] | null): string[] {
  const fixed = restored ?? [];
  const out: string[] = [];
  let live = 0;
  for (let i = 0; i < items.length; i++) {
    const key = i < fixed.length ? fixed[i] : "";
    out.push(key || `live:${live++}`);
  }
  return out;
}

/** 稳定渲染键的段号（`s<段号>:<段内序>`）；`live:<序数>`（本次运行新产生）与畸形键 → null。 */
export function keySegment(key: string): number | null {
  const m = /^s(\d+):/.exec(key);
  if (!m) return null;
  const n = Number(m[1]);
  return Number.isFinite(n) ? n : null;
}

/** 压缩边界 → 「插在第几条 item 之前」的下标表（批2 P2 转录里的分隔线）。
 *
 *  位置 = **第一条段号 ≥ `seq` 的项**之前（段号取自稳定键，见 itemKeysOf）：分页前插不改任何已有键，
 *  所以「更早一段带来的边界」插进去后位置自然正确，store 侧无需维护任何下标。
 *
 *  两道守卫：
 *  ① `seq` 必须 ≥ 已渲染项里**最小的段号**——「收起更早的」把前翻的段丢掉后，留在 state 里的旧边界
 *     不该在转录顶端伪造成一条分隔线；
 *  ② 找不到那样的项（边界段号超出已覆盖段范围）就跳过。
 *  边界缺失 / 脏数据（非有限数字）同样跳过——旧后端与 legacy 会话一个字都不渲染，也不抛错。
 *
 *  返回 Map<下标, 边界[]>：同一位置可叠多条（例如同一段上 compact 与 shrink 各一条，按 seq 升序）。 */
export function boundaryAtIndexes(
  keys: readonly string[],
  boundaries: readonly HistoryBoundary[] | null | undefined,
): Map<number, HistoryBoundary[]> {
  const out = new Map<number, HistoryBoundary[]>();
  const list = boundaries ?? [];
  if (!list.length || !keys.length) return out;
  const segs = keys.map(keySegment);
  const finite = segs.filter((s): s is number => s !== null);
  if (!finite.length) return out; // 全是 live 键：无从判断段号，不渲染
  const minSeg = Math.min(...finite);
  for (const b of list) {
    if (!b || typeof b.seq !== "number" || !Number.isFinite(b.seq) || b.seq < minSeg) continue;
    const at = segs.findIndex((s) => s !== null && s >= b.seq);
    if (at < 0) continue;
    const bucket = out.get(at);
    if (bucket) bucket.push(b);
    else out.set(at, [b]);
  }
  return out;
}

/** 视口顶端是否贴底（阈值内视为贴底） */
export function isAtBottom(g: AnchorGeometry): boolean {
  return g.scrollHeight - g.scrollTop - g.clientHeight < BOTTOM_EPS;
}

/** 程序化跳底的**目标 scrollTop = 浏览器实际会落到的位置**：scrollTop 会被钳到 scrollHeight - clientHeight（最小 0）。
 *
 *  为什么必须单独算：把未钳的 scrollHeight 当作目标去比对自家滚动事件时，
 *  差值恒等于 clientHeight（几百像素），远超过 40px 容差 —— 豁免判定永远不成立，
 *  于是每一次程序化跳底产生的 scroll 事件都会被当成「用户滚走了」。
 *  在流式场景下内容往往在事件派发前又长了一截，几何判定当场得出「未贴底」⇒ 跟随被关掉且不再自愈
 *  （[docs/chat-autoscroll-regression-fix](./chat-autoscroll-regression-fix.md)）。 */
export function bottomScrollTarget(g: { scrollHeight: number; clientHeight: number }): number {
  return Math.max(0, g.scrollHeight - g.clientHeight);
}

/** 程序化滚动的「自家事件」判定：位置落在 [from, target] 区间（各留 BOTTOM_EPS 容差）内即视为自家滚动。
 *
 *  用区间而不是单点，是为覆盖两种自家滚动：
 *  ① 平滑滚动（scrollTo behavior:"smooth"）动画期间位置在 from→target 之间逐帧移动；
 *  ② 连续多次跳底 —— scroll 事件在下一帧才派发，期间可能又跳了一次（目标已变）。
 *  位置越过区间（用户上滚 / 拖滚动条 / PageUp）⇒ 非自家事件，交出控制权。
 *  （from 恒 ≤ target 不成立：内容变矮时落点可能小于发起位置，故取 min/max。） */
export function isSelfScroll(scrollTop: number, from: number, target: number): boolean {
  return (
    scrollTop >= Math.min(from, target) - BOTTOM_EPS &&
    scrollTop <= Math.max(from, target) + BOTTOM_EPS
  );
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
  return { kind: "item", key: best.key, sig: best.sig, offset: g.scrollTop - best.top };
}

/** 定位锚点目标消息：先按**稳定键**命中（分页前插/列表增删都不影响身份），
 *  再退回内容指纹（批1 旧锚点没有 key，或键已随「收起更早的」被丢弃）；
 *  都没有（消息被裁/被删）⇒ null，调用方降级为贴底。 */
export function pickTarget(nodes: AnchorNode[], anchor: ScrollAnchor): AnchorNode | null {
  if (anchor.kind !== "item") return null;
  return (
    (anchor.key ? nodes.find((n) => n.key === anchor.key) : undefined) ??
    nodes.find((n) => n.sig === anchor.sig) ??
    null
  );
}

/** 从滚动容器读消息节点：消息根元素由 ChatMessages 打上 data-key / data-sig；
 *  未打标的项（子代理卡、错误行等外部组件）自然被跳过，锚点落到其上一条消息，位置仍然正确。 */
export function collectNodes(el: HTMLElement): AnchorNode[] {
  const base = el.getBoundingClientRect().top - el.scrollTop;
  const out: AnchorNode[] = [];
  for (const node of Array.from(el.querySelectorAll<HTMLElement>("[data-key]"))) {
    // top 相对「滚动内容顶部」：元素视口位置 - 容器视口位置 + 当前滚动量
    const top = node.getBoundingClientRect().top - base;
    out.push({ key: node.dataset.key ?? "", sig: node.dataset.sig ?? "", top });
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
