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

function safeToolOnly(item: AssistantItem): boolean {
  return !item.streaming && item.timeline.some(seg => seg.kind === "tool")
    && item.timeline.every(seg => seg.kind === "text" ? !seg.text.trim()
      : seg.kind === "tool" && canGroupTool(item.toolsMap[seg.callKey]));
}

function safeTail(item: AssistantItem): boolean {
  for (let i = item.timeline.length - 1; i >= 0; i--) {
    const seg = item.timeline[i];
    if (seg.kind === "text" && !seg.text.trim()) continue;
    return seg.kind === "tool" && canGroupTool(item.toolsMap[seg.callKey]);
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
      for (const member of entry.members) {
        const origin = { key: member.key, sig: itemSig(member.item), createdAt: member.item.createdAt };
        for (const callKey of Object.keys(member.item.toolsMap)) entry.origins[callKey] = origin;
      }
      cache.set(first, { ...entry });
    }
  }
  return entries;
}
