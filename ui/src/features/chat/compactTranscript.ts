import type { AssistantItem, UiItem } from "../../stores/run.types";
import { itemSig } from "../../utils/scrollAnchor";
import { canGroupTool, type ToolOrigin } from "../tools/toolPresentation";

interface Member { key: string; item: AssistantItem }
export interface TranscriptEntry {
  key: string;
  index: number;
  item: UiItem;
  members: Member[];
  origins?: Record<string, ToolOrigin>;
}

// Immutable historical members keep the same projected item during live updates.
const cache = new WeakMap<AssistantItem, TranscriptEntry>();

/** 思考段恒可入组（docs/chat-and-interface 5.2.x：连续思考 + 工具折叠为一行）。
 *  正文（非空白）、子代理卡等一律不可——它们是必须看见的分隔。 */
function safeToolOnly(item: AssistantItem): boolean {
  return !item.streaming && item.timeline.some(seg => seg.kind === "tool" || seg.kind === "thinking")
    && item.timeline.every(seg => seg.kind === "thinking"
      ? true
      : seg.kind === "text" ? seg.deferred || !seg.text.trim()
        : seg.kind === "tool" && canGroupTool(item.toolsMap[seg.callKey]));
}

/** 尾部可被下一条消息延伸：最后一个可见段是工具（可分组）或思考。 */
function safeTail(item: AssistantItem): boolean {
  for (let i = item.timeline.length - 1; i >= 0; i--) {
    const seg = item.timeline[i];
    if (seg.kind === "text" && (seg.deferred || !seg.text.trim())) continue;
    return seg.kind === "thinking" || (seg.kind === "tool" && canGroupTool(item.toolsMap[seg.callKey]));
  }
  return false;
}

/** Display projection only: never modify messages, call keys, history or wire. */
export function compactTranscript(items: readonly UiItem[], keys: readonly string[],
  boundaries: ReadonlyMap<number, unknown>, protectedKeys: ReadonlySet<string>, groupRoots?: ReadonlySet<string>): TranscriptEntry[] {
  const entries: TranscriptEntry[] = [];
  const usedKeys = new Map<TranscriptEntry, Set<string>>();
  for (let index = 0; index < items.length; index++) {
    const item = items[index];
    const key = keys[index];
    const previous = entries[entries.length - 1];
    const previousItem = previous?.members[previous.members.length - 1]?.item ?? previous?.item;
    const merge = item.kind === "assistant" && safeToolOnly(item) && !protectedKeys.has(key) && !groupRoots?.has(key)
      && previousItem?.kind === "assistant" && !previousItem.streaming && safeTail(previousItem)
      // Every appended member already passed protection; only the entry's first key needs checking.
      && !protectedKeys.has(previous.key) && !boundaries.has(index)
      && !Object.keys(item.toolsMap).some(callKey => usedKeys.get(previous)?.has(callKey));
    if (merge && item.kind === "assistant" && previousItem?.kind === "assistant") {
      previous.members.push({ key, item });
      for (const callKey of Object.keys(item.toolsMap)) usedKeys.get(previous)!.add(callKey);
    } else {
      const entry = { key, index, item, members: item.kind === "assistant" ? [{ key, item }] : [] };
      entries.push(entry);
      if (item.kind === "assistant") usedKeys.set(entry, new Set(Object.keys(item.toolsMap)));
    }
  }
  for (const entry of entries) {
    if (entry.members.length < 2) continue;
    const first = entry.members[0].item;
    const cached = cache.get(first);
    if (cached && cached.members.length === entry.members.length
      && cached.members.every((member, index) => member.key === entry.members[index].key && member.item === entry.members[index].item)) {
      entry.item = cached.item;
      entry.members = cached.members;
      entry.origins = cached.origins;
    } else {
      entry.item = { ...first, timeline: entry.members.flatMap(member => member.item.timeline),
        toolsMap: Object.assign({}, ...entry.members.map(member => member.item.toolsMap)) };
      entry.origins = {};
      // 思考段也要记源消息：跨消息组里思考可能来自另一条消息，
      // 漏了它会让组摘要的定位别名只覆盖工具那几条，滚动定位掉回上一条消息。
      // 键与 data-output-key 同构（thinking:<摊平后的段下标>），下标按摊平顺序累加。
      let segIndex = 0;
      for (const member of entry.members) {
        const origin = { key: member.key, sig: itemSig(member.item), createdAt: member.item.createdAt };
        for (const callKey of Object.keys(member.item.toolsMap)) entry.origins[callKey] = origin;
        for (const seg of member.item.timeline) {
          if (seg.kind === "thinking") entry.origins[`thinking:${segIndex}`] = origin;
          segIndex++;
        }
      }
      cache.set(first, { ...entry });
    }
  }
  return entries;
}
