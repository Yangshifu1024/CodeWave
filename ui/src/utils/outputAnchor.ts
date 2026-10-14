import { captureAnchor, isAtBottom, restoreAnchor, type ScrollAnchor } from "./scrollAnchor";

export interface OutputAnchor {
  fallback: ScrollAnchor;
  messageKey: string | null;
  segmentKey: string | null;
  segmentType?: "summary" | "content";
  offset: number;
  atBottom: boolean;
}

function visibleSegments(container: HTMLElement): HTMLElement[] {
  return [...container.querySelectorAll<HTMLElement>("[data-output-key]")]
    .filter((el) => !el.closest("[hidden]") && el.getBoundingClientRect().height > 0);
}

/** 保住消息内部正在阅读的段落，而非仅保住整条长消息的旧像素偏移。 */
export function captureOutputAnchor(container: HTMLElement): OutputAnchor {
  const fallback = captureAnchor(container);
  const atBottom = isAtBottom({ scrollTop: container.scrollTop, scrollHeight: container.scrollHeight, clientHeight: container.clientHeight });
  const result: OutputAnchor = { fallback, messageKey: null, segmentKey: null, offset: 0, atBottom };
  if (atBottom) return result;
  const top = container.getBoundingClientRect().top;
  const segments = visibleSegments(container);
  const node = segments.find((el) => el.getBoundingClientRect().bottom > top);
  if (node) {
    result.segmentKey = node.dataset.outputKey ?? null;
    result.segmentType = node.dataset.outputGroup ? "summary" : "content";
    result.messageKey = node.dataset.outputMessageKey ?? node.closest<HTMLElement>("[data-key]")?.dataset.key ?? null;
    result.offset = top - node.getBoundingClientRect().top;
  }
  return result;
}

export function restoreOutputAnchor(container: HTMLElement, anchor: OutputAnchor): void {
  if (anchor.segmentKey) {
    const nodes = visibleSegments(container);
    const ownedNodes = nodes.filter((el) => {
      const owner = el.dataset.outputMessageKey ?? el.closest<HTMLElement>("[data-key]")?.dataset.key ?? null;
      if (owner !== anchor.messageKey) {
        try {
          const owners: unknown = JSON.parse(el.dataset.outputMessageKeys ?? "[]");
          if (!Array.isArray(owners) || !owners.includes(anchor.messageKey)) return false;
        } catch { return false; }
      }
      return true;
    });
    // The summary shares its first child's key; preserve which one was being read.
    const capturedSummary = anchor.segmentType === "summary";
    const node = ownedNodes.find(el => el.dataset.outputKey === anchor.segmentKey && !!el.dataset.outputGroup === capturedSummary)
      ?? ownedNodes.find(el => el.dataset.outputKey === anchor.segmentKey)
      ?? ownedNodes.find(el => {
        // 简洁模式可能将当前的已完成工具压入一组；落在该组摘要上。
        try {
          const keys: unknown = JSON.parse(el.dataset.outputGroup ?? "[]");
          return Array.isArray(keys) && keys.includes(anchor.segmentKey);
        } catch { return false; }
      });
    if (node) {
      const offset = node.dataset.outputKey === anchor.segmentKey && !!node.dataset.outputGroup === capturedSummary ? anchor.offset : 0;
      container.scrollTop += node.getBoundingClientRect().top - container.getBoundingClientRect().top + offset;
      return;
    }
  }
  restoreAnchor(container, anchor.fallback);
}
