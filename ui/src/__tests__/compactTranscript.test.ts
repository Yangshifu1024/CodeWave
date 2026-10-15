import { afterEach, describe, expect, it, vi } from "vitest";
import { compactTranscript } from "../features/chat/compactTranscript";
import type { AssistantItem, UiItem } from "../stores/run.types";
import { collectNodes, restoreAnchor } from "../utils/scrollAnchor";
import { captureOutputAnchor, restoreOutputAnchor } from "../utils/outputAnchor";
import { toolTime } from "../features/tools/toolPresentation";

const command = (key: string): AssistantItem => ({
  kind: "assistant", streaming: false, createdAt: "2026-10-14T13:14:25+08:00",
  timeline: [{ kind: "tool", callKey: key }],
  toolsMap: { [key]: { callKey: key, tool: "command", status: "ok", progressTail: "",
    argsPreview: JSON.stringify({ command: `echo ${key}` }), outcome: { ok: true, data: { exit_code: 0, output: key } },
  } },
});
const project = (items: UiItem[], boundaries = new Map<number, unknown>(), protectedKeys = new Set<string>()) =>
  compactTranscript(items, items.map((_, index) => `message-${index}`), boundaries, protectedKeys);

afterEach(() => { document.body.innerHTML = ""; });

describe("cross-message compact tool projection", () => {
  it("merges a mixed message's tool tail with following successful calls without changing originals", () => {
    const first = command("a");
    first.timeline.unshift({ kind: "text", text: "日志已落盘" });
    const items = [first, command("b"), command("c")];
    const original = JSON.stringify(items);
    const entries = project(items);
    expect(entries).toHaveLength(1);
    expect(entries[0].item.kind).toBe("assistant");
    if (entries[0].item.kind !== "assistant") throw new Error("Missing projected assistant");
    expect(entries[0].item.timeline.map(seg => seg.kind)).toEqual(["text", "tool", "tool", "tool"]);
    expect(Object.keys(entries[0].item.toolsMap)).toEqual(["a", "b", "c"]);
    expect(entries[0].origins?.b.key).toBe("message-1");
    expect(entries[0].origins?.b.createdAt).toBe(items[1].createdAt);
    expect(JSON.stringify(items)).toBe(original);
    expect(project(items)[0].item).toBe(entries[0].item);
  });

  it("does not cross user, prose, subagent, error or notice boundaries", () => {
    const separators: UiItem[] = [
      { kind: "user", text: "新请求" }, { kind: "notice", text: "注入" }, { kind: "error", text: "失败" },
      { kind: "sub", subId: "child" },
      { ...command("middle"), timeline: [{ kind: "text", text: "结论" }] },
    ];
    for (const separator of separators) expect(project([command("a"), separator, command("b")])).toHaveLength(3);
    expect(project([command("a"), command("b")], new Map([[1, true]]))).toHaveLength(2);
  });

  it("keeps thinking in the group instead of splitting it (concise fold, docs 5.2.x)", () => {
    // 思考不再作分隔符：三条含思考的消息应并成一条
    const withThinking = (key: string) => ({
      ...command(key), timeline: [{ kind: "thinking" as const, text: "推理" }, { kind: "tool" as const, callKey: key }],
    });
    const entries = project([withThinking("a"), withThinking("b"), withThinking("c")]);
    expect(entries).toHaveLength(1);
    expect(entries[0].item.kind === "assistant" && entries[0].item.timeline.map(seg => seg.kind))
      .toEqual(["thinking", "tool", "thinking", "tool", "thinking", "tool"]);
    // 思考段也要带源消息映射，否则跨消息组的滚动定位会掉回上一条消息
    expect(entries[0].origins?.["thinking:0"]?.key).toBe("message-0");
    expect(entries[0].origins?.["thinking:2"]?.key).toBe("message-1");
    expect(entries[0].origins?.["thinking:4"]?.key).toBe("message-2");
  });

  it("keeps running, waiting, failed, warned and open-detail calls separate", () => {
    for (const status of ["running", "waiting", "error"] as const) {
      const current = command("b"); current.toolsMap.b.status = status;
      expect(project([command("a"), current])).toHaveLength(2);
    }
    const warned = command("b"); warned.toolsMap.b.outcome.warnings = ["检查失败"];
    expect(project([command("a"), warned])).toHaveLength(2);
    const nonzero = command("b"); nonzero.toolsMap.b.outcome.data.exit_code = 1;
    expect(project([command("a"), nonzero])).toHaveLength(2);
    expect(project([command("a"), { ...command("b"), streaming: true }])).toHaveLength(2);
    expect(project([command("a"), command("b")], new Map(), new Set(["message-1"]))).toHaveLength(2);
    expect(project([command("a"), command("b")], new Map(), new Set(["message-0"]))).toHaveLength(2);
  });

  it("never overwrites repeated call keys, and preserves compaction dividers' source index", () => {
    expect(project([command("same"), command("same")])).toHaveLength(2);
    const entries = project([command("a"), command("b"), command("c")], new Map([[1, true]]));
    expect(entries.map(entry => entry.index)).toEqual([0, 1]);
    expect(entries[1].members.map(member => member.key)).toEqual(["message-1", "message-2"]);
  });

  it("checks protection linearly for long consecutive tool histories", () => {
    const items = Array.from({ length: 1000 }, (_, index) => command(`call-${index}`));
    const protectedKeys = new Set<string>();
    const has = vi.spyOn(protectedKeys, "has");
    const entries = project(items, new Map(), protectedKeys);
    expect(entries).toHaveLength(1);
    expect(entries[0].members).toHaveLength(items.length);
    expect(has.mock.calls.length).toBeLessThanOrEqual(items.length * 2);
  });

  it("does not invent missing or invalid per-call time", () => {
    expect(toolTime(undefined)).toBe("");
    expect(toolTime("invalid")).toBe("");
    expect(toolTime("2026-10-14T13:14:25+08:00")).toMatch(/^\d{2}:\d{2}:\d{2}$/);
  });
});

it("keeps an expanded tool's owner and restores its collapsed cross-message summary", () => {
  const container = document.createElement("div");
  container.innerHTML = '<div data-key="a" data-sig="sig-a"><button data-output-key="tool:x" data-output-group=\'["tool:x","tool:y"]\' data-output-message-keys=\'["a","b"]\' data-anchor-aliases=\'[{"key":"a","sig":"sig-a"},{"key":"b","sig":"sig-b"}]\'></button><div class="body"><div data-output-key="tool:y" data-output-message-key="b"></div></div></div>';
  document.body.append(container);
  Object.defineProperties(container, { scrollTop: { value: 150, writable: true }, scrollHeight: { value: 1000 }, clientHeight: { value: 200 } });
  container.getBoundingClientRect = () => ({ top: 0, bottom: 200, height: 200 } as DOMRect);
  container.querySelector<HTMLElement>('[data-key="a"]')!.getBoundingClientRect = () => ({ top: -container.scrollTop, bottom: 500 - container.scrollTop, height: 500 } as DOMRect);
  container.querySelector<HTMLElement>("button")!.getBoundingClientRect = () => ({ top: 100 - container.scrollTop, bottom: 132 - container.scrollTop, height: 32 } as DOMRect);
  container.querySelector<HTMLElement>('[data-output-key="tool:y"]')!.getBoundingClientRect = () => ({ top: 134 - container.scrollTop, bottom: 400 - container.scrollTop, height: 266 } as DOMRect);
  const anchor = captureOutputAnchor(container);
  expect(anchor.messageKey).toBe("b");
  expect(anchor.segmentKey).toBe("tool:y");
  restoreOutputAnchor(container, anchor);
  expect(container.scrollTop).toBe(150);
  container.querySelector<HTMLElement>(".body")!.hidden = true;
  restoreOutputAnchor(container, anchor);
  expect(container.scrollTop).toBe(100);
  expect(collectNodes(container).map(node => node.key)).toEqual(["a", "b"]);
  container.scrollTop = 250;
  expect(restoreAnchor(container, { kind: "item", key: "b", sig: "sig-b", offset: 0 })).toBe(true);
  expect(container.scrollTop).toBe(100);
});
