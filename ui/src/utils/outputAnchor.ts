import { captureAnchor, isAtBottom, restoreAnchor, type ScrollAnchor } from "./scrollAnchor";

export interface OutputAnchor {
  fallback: ScrollAnchor;
  messageKey: string | null;
  segmentKey: string | null;
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
    result.messageKey = node.closest<HTMLElement>("[data-key]")?.dataset.key ?? null;
    result.offset = top - node.getBoundingClientRect().top;
  }
  return result;
}

export function restoreOutputAnchor(container: HTMLElement, anchor: OutputAnchor): void {
  if (anchor.segmentKey) {
    const nodes = visibleSegments(container);
    const node = nodes.find((el) => {
      if ((el.closest<HTMLElement>("[data-key]")?.dataset.key ?? null) !== anchor.messageKey) return false;
      if (el.dataset.outputKey === anchor.segmentKey) return true;
      // 简洁模式可能将当前的已完成工具压入一组；落在该组摘要上。
      try {
        const keys: unknown = JSON.parse(el.dataset.outputGroup ?? "[]");
        return Array.isArray(keys) && keys.includes(anchor.segmentKey);
      } catch { return false; }
    });
    if (node) {
      const offset = node.dataset.outputKey === anchor.segmentKey ? anchor.offset : 0;
      container.scrollTop += node.getBoundingClientRect().top - container.getBoundingClientRect().top + offset;
      return;
    }
  }
  restoreAnchor(container, anchor.fallback);
}
