import type { RunDisplay } from "../../ipc/types";
import type { AssistantItem, SubView, TimelineSeg, UiItem } from "../../stores/run.types";
import { itemSig } from "../../utils/scrollAnchor";
import type { TranscriptEntry } from "./compactTranscript";

export type TaskTranscriptEntry = TranscriptEntry | {
  kind: "task";
  key: string;
  run: RunDisplay;
  process: TranscriptEntry[];
  visible: TranscriptEntry[];
  /** An unverified or absent final answer must never cause prose to disappear. */
  canCollapse: boolean;
  finalKey?: string;
};

interface SourceSegment { seg: TimelineSeg; index: number }

/** A live assistant spans several LLM steps. Split only the authoritative final text suffix,
 *  including a boundary inside an already merged text segment. Keep every original character. */
function finalSuffix(item: AssistantItem, answer: string): { process: SourceSegment[]; answer: SourceSegment[] } | null {
  const text = item.timeline.flatMap(seg => seg.kind === "text" ? [seg.text] : []).join("");
  if (!answer || !text.endsWith(answer)) return null;
  const boundary = text.length - answer.length;
  let offset = 0;
  const process: SourceSegment[] = [];
  const final: SourceSegment[] = [];
  item.timeline.forEach((seg, index) => {
    if (seg.kind !== "text") { process.push({ seg, index }); return; }
    const prefixLength = Math.min(seg.text.length, Math.max(0, boundary - offset));
    if (prefixLength > 0) process.push({ seg: prefixLength === seg.text.length ? seg : { ...seg, text: seg.text.slice(0, prefixLength) }, index });
    if (prefixLength < seg.text.length) final.push({ seg: prefixLength === 0 ? seg : {
      ...seg, text: seg.text.slice(prefixLength), outputKey: `${seg.outputKey ?? `text:${index}`}:final`,
    }, index });
    offset += seg.text.length;
  });
  return { process, answer: final };
}

function splitEntry(entry: TranscriptEntry, parts: SourceSegment[], suffix: string): TranscriptEntry {
  if (entry.item.kind !== "assistant") return entry;
  const item = entry.item;
  const origin = { key: entry.key, sig: itemSig(item), createdAt: item.createdAt };
  const origins = { ...entry.origins };
  const timeline = parts.map(({ seg, index: sourceIndex }, index) => {
    if (seg.kind === "tool") origins[seg.callKey] ??= origin;
    if (seg.kind === "sub") origins[`sub:${seg.subId}`] ??= origin;
    if (seg.kind === "thinking" || seg.kind === "text") origins[`${seg.kind}:${index}`] = entry.origins?.[`${seg.kind}:${sourceIndex}`] ?? origin;
    return seg.kind === "text" ? { ...seg, outputKey: seg.outputKey ?? `text:${sourceIndex}` } : seg;
  });
  return { ...entry, key: `${entry.key}${suffix}`, item: { ...item, timeline }, members: [], origins };
}

function attention(item: UiItem, subs: readonly SubView[]): boolean {
  if (item.kind === "error" || (item.kind === "notice" && !item.injectedTexts?.length)) return true;
  if (item.kind === "sub") {
    const sub = subs.find(sub => sub.subId === item.subId);
    return !sub || sub.status === "error" || (sub.ended !== undefined && sub.ended !== "report");
  }
  return false;
}

/** Pure display projection. A matching start message and complete final assistant are required;
 *  pages beginning midway through a run keep their ordinary transcript until the start is loaded. */
export function taskTranscript(entries: readonly TranscriptEntry[], runs: readonly RunDisplay[], subs: readonly SubView[]): TaskTranscriptEntry[] {
  const output: TaskTranscriptEntry[] = [];
  const byUser = new Map(runs.filter(run => Number.isFinite(run.duration_ms) && run.duration_ms >= 0)
    .map(run => [run.user_created_at, run]));
  for (let cursor = 0; cursor < entries.length; cursor++) {
    const start = entries[cursor];
    output.push(start);
    if (start.item.kind !== "user" || !start.item.createdAt) continue;
    const run = byUser.get(start.item.createdAt);
    if (!run) continue;
    const expectsFinal = run.status === "done" && !!run.final_answer?.trim();
    let end = cursor + 1;
    let suffix: ReturnType<typeof finalSuffix> = null;
    if (expectsFinal) {
      if (!run.final_answer_created_at) continue;
      for (; end < entries.length; end++) {
        const item = entries[end].item;
        if (item.kind === "user" && item.createdAt && byUser.get(item.createdAt)?.run_id !== run.run_id
          && byUser.has(item.createdAt)) break;
        if (item.kind === "assistant" && !item.streaming && item.createdAt === run.final_answer_created_at) {
          suffix = finalSuffix(item, run.final_answer!);
          if (suffix) { end++; break; }
        }
      }
      if (!suffix) continue; // The final message is absent from this page or its text cannot be verified.
    } else {
      // Without a final message identity we cannot distinguish an injected user from a new question.
      while (end < entries.length && entries[end].item.kind !== "user") end++;
    }
    const range = entries.slice(cursor + 1, end);
    if (!range.length || range.some(entry => entry.item.kind === "assistant" && entry.item.streaming)) continue;
    if (run.status === "done" && !suffix) {
      output.push({ kind: "task", key: `task:${run.run_id}`, run, process: [...range], visible: [], canCollapse: false });
      cursor = end - 1;
      continue;
    }
    const process: TranscriptEntry[] = [];
    const visible: TranscriptEntry[] = [];
    for (let i = 0; i < range.length; i++) {
      const entry = range[i];
      const item = entry.item;
      if (attention(item, subs)) { visible.push(entry); continue; }
      if (item.kind !== "assistant") { process.push(entry); continue; }
      const finalText = suffix && i === range.length - 1 ? suffix.answer : [];
      const source = suffix && i === range.length - 1 ? suffix.process : item.timeline.map((seg, index) => ({ seg, index }));
      const exposed = source.filter(({ seg }) => seg.kind === "sub"
        ? attention({ kind: "sub", subId: seg.subId }, subs)
        : seg.kind === "tool" && (() => {
        const tool = item.toolsMap[seg.callKey];
        return tool?.status === "error" || !!tool?.outcome?.warnings?.length;
      })());
      const kept = new Set(exposed);
      const hidden = source.filter(part => !kept.has(part));
      if (hidden.length) process.push(splitEntry(entry, hidden, finalText.length || exposed.length ? ":process" : ""));
      if (exposed.length) visible.push(splitEntry(entry, exposed, ":attention"));
      if (finalText.length) visible.push(splitEntry(entry, finalText, ""));
    }
    output.push({ kind: "task", key: `task:${run.run_id}`, run, process, visible,
      canCollapse: run.status !== "done" || !!suffix, finalKey: suffix ? range[range.length - 1].key : undefined });
    cursor = end - 1;
  }
  return output;
}

export function taskDuration(ms: number, language: string): string {
  const seconds = Math.floor(ms / 1000);
  const hours = Math.floor(seconds / 3600);
  const minutes = Math.floor(seconds / 60) % 60;
  const rest = seconds % 60;
  if (language.startsWith("zh")) return [hours ? `${hours} 小时` : "", minutes ? `${minutes} 分` : "", `${rest} 秒`].filter(Boolean).join(" ");
  return [hours ? `${hours}h` : "", minutes ? `${minutes}m` : "", `${rest}s`].filter(Boolean).join(" ");
}
