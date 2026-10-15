import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { act, cleanup, fireEvent, render } from "@testing-library/react";
import { App } from "antd";
import "../i18n";
import { taskDuration, taskTranscript } from "../features/chat/taskTranscript";
import ChatMessages from "../features/chat/ChatMessages";
import type { TranscriptEntry } from "../features/chat/compactTranscript";
import type { RunDisplay } from "../ipc/types";
import type { UiItem } from "../stores/run.types";
import { useRun } from "../stores/run";
import { useSessions } from "../stores/sessions";
import { useUi } from "../stores/ui";
import { ipc } from "../ipc/client";
import { collectNodes } from "../utils/scrollAnchor";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn(async () => null), Channel: class {} }));
const sid = "task-process";
const started = "2026-10-15T10:00:00Z";
const finished = "2026-10-15T10:02:18Z";
const run: RunDisplay = { run_id: "r1", started_at: started, user_created_at: started,
  finished_at: finished, duration_ms: 138200, status: "done", final_answer: "## 总结\n完成\n\n## 建议\n运行测试", final_answer_created_at: finished };
const user: UiItem = { kind: "user", text: "完成任务", createdAt: started };
const process: UiItem = { kind: "assistant", streaming: false, createdAt: started, toolsMap: {},
  timeline: [{ kind: "text", text: "正在检查源码" }, { kind: "thinking", text: "详细推理" }] };
const answer: UiItem = { kind: "assistant", streaming: false, createdAt: finished, toolsMap: {},
  timeline: [{ kind: "thinking", text: "最后核查" }, { kind: "text", text: run.final_answer! }] };
const entries = (items: UiItem[]): TranscriptEntry[] => items.map((item, index) => ({ key: `k${index}`, index, item, members: [] }));
const project = (items: UiItem[], runs = [run]) => taskTranscript(entries(items), runs, []);

beforeEach(() => {
  useSessions.setState({ tabs: [], activeKey: sid });
  useRun.setState({ tabs: {} });
  useRun.getState().initTab(sid);
  useUi.setState({ outputDisplay: "detailed" });
});
afterEach(() => {
  cleanup();
  useRun.setState({ tabs: {} });
  useSessions.setState({ tabs: [], activeKey: null });
  useUi.setState({ outputDisplay: "detailed" });
});

describe("task process projection", () => {
  const plainText = (parts: TranscriptEntry[]) => parts.flatMap(part => part.item.kind === "assistant"
    ? part.item.timeline.flatMap(seg => seg.kind === "text" ? [seg.text] : []) : []).join("");

  it("collapses a real single assistant spanning multiple tool steps and splits an exact final suffix inside one text segment", () => {
    const finalAnswer = "最终答复\n建议运行测试";
    const single: UiItem = { kind: "assistant", streaming: false, createdAt: finished,
      timeline: [{ kind: "text", text: "第一步检查\n" }, { kind: "tool", callKey: "read" },
        { kind: "text", text: `第二步完成\n${finalAnswer}` }],
      toolsMap: { read: { callKey: "read", tool: "read", status: "ok", progressTail: "" } } };
    const original = JSON.stringify(single);
    const result = project([user, single], [{ ...run, final_answer: finalAnswer }]);
    const task = result[1];
    if (!("kind" in task)) throw new Error("real multi-step assistant did not collapse");
    expect(task.canCollapse).toBe(true);
    expect(plainText(task.process)).toBe("第一步检查\n第二步完成\n");
    expect(plainText(task.visible)).toBe(finalAnswer);
    expect(task.visible[0].item.kind === "assistant" && task.visible[0].item.timeline[0]).toMatchObject({ outputKey: "text:2:final" });
    expect(JSON.stringify(single)).toBe(original);
  });

  it("splits an exact final suffix across text segments while retaining all non-text process and original indexes", () => {
    const single: UiItem = { ...answer, timeline: [
      { kind: "text", text: "过程前缀\n最终" }, { kind: "thinking", text: "分析" }, { kind: "text", text: "答复\n" },
    ] };
    const result = project([user, single], [{ ...run, final_answer: "最终答复\n" }]);
    const task = result[1];
    if (!("kind" in task)) throw new Error("missing cross-segment suffix");
    expect(plainText(task.process)).toBe("过程前缀\n");
    expect(plainText(task.visible)).toBe("最终答复\n");
    expect(task.process[0].origins?.["text:0"]?.key).toBe("k1");
    expect(task.visible[0].item.kind === "assistant" && task.visible[0].item.timeline).toMatchObject([
      { kind: "text", outputKey: "text:0:final" }, { kind: "text", outputKey: "text:2" },
    ]);
  });

  it.each([finished, undefined, "not-a-date"])("does not consume a subsequent question with ambiguous timestamp %s", createdAt => {
    const nextUser: UiItem = { kind: "user", text: "后续独立问题", createdAt };
    const nextAnswer: UiItem = { ...answer, createdAt: undefined, timeline: [{ kind: "text", text: "后续答复" }] };
    const source = [user, process, answer, nextUser, nextAnswer];
    const result = project(source);
    expect(result).toHaveLength(4);
    expect(result[2]).toEqual(entries(source)[3]);
    expect(result[3]).toEqual(entries(source)[4]);
  });

  it("stops an unverified run before the next known run even when both final answers are identical", () => {
    const secondStart = "2026-10-15T10:03:00Z";
    const secondFinal = "2026-10-15T10:04:00Z";
    const nextRun: RunDisplay = { ...run, run_id: "r2", user_created_at: secondStart, started_at: secondStart,
      finished_at: secondFinal, final_answer_created_at: secondFinal };
    const nextUser: UiItem = { ...user, createdAt: secondStart };
    const nextAnswer: UiItem = { ...answer, createdAt: secondFinal };
    const source = [user, process, nextUser, nextAnswer];
    const result = project(source, [run, nextRun]);
    expect(result).toHaveLength(4);
    expect(result[0]).toEqual(entries(source)[0]);
    expect(result[1]).toEqual(entries(source)[1]);
    const secondTask = result[3];
    expect("kind" in secondTask && secondTask.run.run_id).toBe("r2");
  });

  it("keeps two completed runs with identical final text in their own task groups", () => {
    const secondStart = "2026-10-15T10:03:00Z";
    const secondFinal = "2026-10-15T10:04:00Z";
    const common = "同一最终答复";
    const firstRun = { ...run, final_answer: common };
    const secondRun = { ...firstRun, run_id: "r2", user_created_at: secondStart, started_at: secondStart,
      finished_at: secondFinal, final_answer_created_at: secondFinal };
    const firstAnswer: UiItem = { ...answer, timeline: [{ kind: "text", text: `首轮过程\n${common}` }] };
    const secondAnswer: UiItem = { ...answer, createdAt: secondFinal, timeline: [{ kind: "text", text: `次轮过程\n${common}` }] };
    const result = project([user, firstAnswer, { ...user, createdAt: secondStart }, secondAnswer], [firstRun, secondRun]);
    expect(result).toHaveLength(4);
    const tasks = result.filter(part => "kind" in part);
    expect(tasks.map(task => task.run.run_id)).toEqual(["r1", "r2"]);
    expect(tasks.map(task => plainText(task.process))).toEqual(["首轮过程\n", "次轮过程\n"]);
    expect(tasks.map(task => plainText(task.visible))).toEqual([common, common]);
  });

  it("matches exact suffix text rather than a heading or trimmed approximation", () => {
    const single: UiItem = { ...answer, timeline: [{ kind: "text", text: "过程\n## 总结\n建议正文\n " }] };
    const metadata = { ...run, final_answer: "## 总结\n建议正文\n" };
    expect(project([user, single], [metadata])).toEqual(entries([user, single]));
  });

  it("keeps a run with no authoritative final time ordinary and bounds a no-answer shell at the next user", () => {
    const source = [user, process, answer];
    expect(project(source, [{ ...run, final_answer_created_at: undefined }])).toEqual(entries(source));
    const nextUser: UiItem = { kind: "user", text: "下一问" };
    const missingFinal: RunDisplay = { ...run, final_answer: undefined, final_answer_created_at: undefined };
    const result = project([user, process, nextUser, answer], [missingFinal]);
    expect(result).toHaveLength(4);
    const task = result[1];
    if (!("kind" in task)) throw new Error("missing expanded shell");
    expect(task.process.map(part => part.item)).toEqual([process]);
    expect(task.canCollapse).toBe(false);
    expect(result[2]).toEqual(entries([user, process, nextUser, answer])[2]);
  });

  it("splits mixed final text and thinking without mutating the transcript or duplicating the answer", () => {
    const source = [user, process, answer];
    const original = JSON.stringify(source);
    const projected = project(source);
    expect(projected).toHaveLength(2);
    const task = projected[1];
    expect("kind" in task && task.kind).toBe("task");
    if (!("kind" in task)) throw new Error("missing task");
    expect(task.process).toHaveLength(2);
    expect(task.visible).toHaveLength(1);
    expect(JSON.stringify(task.process.map(part => part.item))).not.toContain("## 总结");
    expect(JSON.stringify(task.visible)).not.toContain("最后核查");
    expect(JSON.stringify(source)).toBe(original);
  });

  it("keeps older history, partial pages and unverified final answers unchanged", () => {
    const source = [user, process, answer];
    expect(project(source, [])).toEqual(entries(source));
    expect(project([process, answer])).toEqual(entries([process, answer]));
    expect(project(source, [{ ...run, final_answer_created_at: started }])).toEqual(entries(source));
  });

  it("shows reliable elapsed time without guessing a final answer and preserves the original order", () => {
    const source = [user, process, { kind: "notice" as const, text: "请检查日志" }, answer];
    const projected = project(source, [{ ...run, final_answer: undefined, final_answer_created_at: undefined }]);
    const task = projected[1];
    if (!("kind" in task)) throw new Error("missing task");
    expect(task.canCollapse).toBe(false);
    expect(task.process).toEqual(entries(source).slice(1));
    expect(task.visible).toHaveLength(0);
  });

  it("includes restored inject users in the same run and isolates the next queued run", () => {
    const inject: UiItem = { kind: "user", text: "顺便检查边界", createdAt: "2026-10-15T10:01:00Z" };
    const nextUser: UiItem = { kind: "user", text: "下一轮", createdAt: "2026-10-15T10:03:00Z" };
    const result = project([user, process, inject, answer, nextUser, { ...process, streaming: true }]);
    expect(result).toHaveLength(4);
    const task = result[1];
    if (!("kind" in task)) throw new Error("missing task");
    expect(task.process.some(part => part.item === inject)).toBe(true);
    expect(result[2]).toEqual(entries([user, process, inject, answer, nextUser])[4]);
  });

  it("keeps tool errors and warnings plus incomplete subagent results outside the collapsed process", () => {
    const warning: UiItem = { kind: "assistant", streaming: false,
      timeline: [{ kind: "tool", callKey: "w" }, { kind: "sub", subId: "partial" }],
      toolsMap: { w: { callKey: "w", tool: "command", status: "ok", progressTail: "", outcome: { warnings: ["检查失败"] } } } };
    const result = taskTranscript(entries([user, warning, answer]), [run], [{ subId: "partial", role: "tester", description: "verify", step: 1, maxSteps: 1, tokens: 0, lastTools: [], status: "done", ended: "partial" }]);
    const task = result[1];
    if (!("kind" in task)) throw new Error("missing task");
    expect(task.visible[0].item.kind === "assistant" && task.visible[0].item.timeline).toHaveLength(2);
    expect(JSON.stringify(task.process)).not.toContain('"subId":"partial"');
  });

  it("uses wall duration directly with bilingual hour/minute/second formatting", () => {
    expect(taskDuration(138200, "zh-CN")).toBe("2 分 18 秒");
    expect(taskDuration(3600000, "en-US")).toBe("1h 0s");
    expect(taskDuration(200, "zh-CN")).toBe("0 秒");
  });
});

describe("task process UI and lifecycle", () => {
  function seed(items = [user, process, answer], metadata = run) {
    useRun.setState(state => {
      state.tabs[sid].items = items;
      state.tabs[sid].taskRuns = [metadata];
    });
  }
  it("collapses completed process, keeps summary/recommendations visible, and retains expansion across mode and Tab changes", () => {
    seed();
    const { container } = render(<App><ChatMessages /></App>);
    const toggle = () => container.querySelector(".task-process .ant-collapse-header")!;
    expect(toggle().getAttribute("aria-expanded")).toBe("false");
    expect(toggle().textContent).toContain("总耗时 2 分 18 秒");
    expect(container.querySelector(".assistant h2")?.textContent).toBe("总结");
    expect(container.querySelectorAll(".assistant h2")).toHaveLength(2);
    fireEvent.click(toggle());
    expect(toggle().getAttribute("aria-expanded")).toBe("true");
    expect(container.textContent).toContain("正在检查源码");
    expect(container.querySelectorAll(".assistant h2")).toHaveLength(2);
    act(() => useUi.getState().setOutputDisplay("compact"));
    expect(toggle().getAttribute("aria-expanded")).toBe("true");
    act(() => { useRun.getState().initTab("other"); useSessions.setState({ activeKey: "other" }); });
    expect(container.querySelector(".task-process")).toBeNull();
    act(() => useSessions.setState({ activeKey: sid }));
    expect(toggle().getAttribute("aria-expanded")).toBe("true");
  });

  it("defaults failed and cancelled processes to expanded and shows their true status", () => {
    seed([user, process, { kind: "error", text: "网络断开" }], { ...run, status: "error", final_answer: undefined });
    const { container } = render(<App><ChatMessages /></App>);
    expect(container.querySelector(".ant-collapse-header")?.getAttribute("aria-expanded")).toBe("true");
    expect(container.textContent).toContain("失败");
    expect(container.textContent).toContain("网络断开");
  });

  it("preserves a pure text answer and shows duration without an empty collapse control", () => {
    seed([user, { ...answer, timeline: [{ kind: "text", text: run.final_answer! }] }]);
    const { container } = render(<App><ChatMessages /></App>);
    expect(container.querySelector(".task-process")).toBeNull();
    expect(container.textContent).toContain("总耗时 2 分 18 秒");
    expect(container.querySelectorAll("h2")).toHaveLength(2);
  });

  it("maps collapsed members to the header and excludes mounted hidden children after reclosing", () => {
    seed();
    const { container } = render(<App><ChatMessages /></App>);
    const scroller = container.querySelector<HTMLElement>(".chat-messages")!;
    const header = container.querySelector<HTMLElement>(".ant-collapse-header")!;
    const aliases = container.querySelector<HTMLElement>("[data-anchor-aliases]")!;
    aliases.getBoundingClientRect = () => ({ top: 100, height: 32, bottom: 132 } as DOMRect);
    const before = collectNodes(scroller).find(node => node.key === "live:1");
    expect(before?.top).toBe(100);
    fireEvent.click(header);
    expect(container.querySelector('[data-key="live:1"]')?.closest("[hidden]")).toBeNull();
    fireEvent.click(header);
    const hidden = container.querySelector('[data-key="live:1"]');
    expect(hidden?.closest("[hidden]")).not.toBeNull();
    const after = collectNodes(scroller).filter(node => node.key === "live:1");
    expect(after).toHaveLength(1);
    expect(after[0].top).toBe(100);
  });

  it.each(["text", "warning", "subWarning"])("keeps a reader at the process header when completion collapses a reading position (%s)", variant => {
    const sourceProcess: UiItem = variant !== "text" ? { ...process, timeline: [variant === "subWarning" ? { kind: "sub", subId: "done" } : { kind: "text", text: "正在检查源码" }, { kind: "tool", callKey: "warn" }],
      toolsMap: { warn: { callKey: "warn", tool: "command", status: "ok", progressTail: "", outcome: { warnings: ["请核查"] } } } } : process;
    useRun.setState(state => {
      state.tabs[sid].items = [user, sourceProcess, { ...answer, streaming: true }];
      state.tabs[sid].running = true;
      state.tabs[sid].subs = [{ subId: "done", role: "tester", description: "已完成的检查", step: 1, maxSteps: 1, tokens: 0, lastTools: [], status: "done", ended: "report" }];
    });
    const { container } = render(<App><ChatMessages /></App>);
    const scroller = container.querySelector<HTMLElement>(".chat-messages")!;
    Object.defineProperties(scroller, { scrollHeight: { configurable: true, value: 1200 }, clientHeight: { configurable: true, value: 200 } });
    const geometry = vi.spyOn(HTMLElement.prototype, "getBoundingClientRect").mockImplementation(function(this: HTMLElement) {
      const owner = this.closest<HTMLElement>("[data-key]")?.dataset.key;
      const logical = this.matches(".task-process [data-anchor-aliases]") || owner === "live:1" ? 100 : owner === "live:2" ? 800 : 0;
      const top = this === scroller ? 0 : logical - scroller.scrollTop;
      return { top, bottom: top + 32, height: 32, left: 0, right: 300, width: 300, x: 0, y: top, toJSON: () => ({}) };
    });
    try {
      scroller.scrollTop = 110;
      fireEvent.wheel(scroller, { deltaY: -50 });
      fireEvent.scroll(scroller);
      act(() => useRun.setState(state => {
        state.tabs[sid].items = [user, sourceProcess, answer];
        state.tabs[sid].taskRuns = [run];
        state.tabs[sid].running = false;
      }));
      expect(scroller.scrollTop).toBe(100);
      expect(container.querySelector(".ant-collapse-header")?.getAttribute("aria-expanded")).toBe("false");
    } finally { geometry.mockRestore(); }
  });

  it("recovers duration from the authoritative snapshot when the terminal event was lost", async () => {
    useRun.setState(state => {
      state.tabs[sid].items = [user, { ...answer, streaming: true }];
      state.tabs[sid].runId = "r1";
      state.tabs[sid].running = true;
    });
    const snapshot = vi.spyOn(ipc, "sessionRunSnapshot").mockResolvedValue({ running: false, run_id: "r1", terminal: { run_id: "r1", kind: "done", task_run: run } });
    await useRun.getState().reconcileRun(sid);
    expect(useRun.getState().tabs[sid].taskRuns).toEqual([run]);
    expect(useRun.getState().tabs[sid].running).toBe(false);
    snapshot.mockRestore();
  });

  it("restores a reader in a split text prefix to the process header instead of the visible final suffix", () => {
    const finalAnswer = "最终答复";
    const single: UiItem = { kind: "assistant", createdAt: finished, streaming: true, toolsMap: {},
      timeline: [{ kind: "text", text: `过程前缀\n${finalAnswer}` }] };
    useRun.setState(state => {
      state.tabs[sid].items = [user, single];
      state.tabs[sid].running = true;
    });
    const { container } = render(<App><ChatMessages /></App>);
    const scroller = container.querySelector<HTMLElement>(".chat-messages")!;
    Object.defineProperties(scroller, { scrollHeight: { configurable: true, value: 1500 }, clientHeight: { configurable: true, value: 200 } });
    const geometry = vi.spyOn(HTMLElement.prototype, "getBoundingClientRect").mockImplementation(function(this: HTMLElement) {
      const isFinal = this.dataset.outputKey === "text:0:final";
      const owner = this.closest<HTMLElement>("[data-key]")?.dataset.key;
      const logical = isFinal ? 800 : this.matches(".task-process [data-anchor-aliases]") || owner === "live:1" ? 100 : 0;
      const top = this === scroller ? 0 : logical - scroller.scrollTop;
      return { top, bottom: top + 32, height: 32, left: 0, right: 300, width: 300, x: 0, y: top, toJSON: () => ({}) };
    });
    try {
      scroller.scrollTop = 110;
      fireEvent.wheel(scroller, { deltaY: -50 });
      fireEvent.scroll(scroller);
      act(() => useRun.setState(state => {
        state.tabs[sid].items = [user, { ...single, streaming: false }];
        state.tabs[sid].taskRuns = [{ ...run, final_answer: finalAnswer }];
        state.tabs[sid].running = false;
      }));
      expect(container.querySelector('.assistant [data-output-key="text:0:final"] p')?.textContent).toBe(finalAnswer);
      expect(JSON.parse(container.querySelector<HTMLElement>(".task-process [data-output-group]")!.dataset.outputGroup!)).toContain("text:0");
      expect(scroller.scrollTop).toBe(100);
    } finally { geometry.mockRestore(); }
  });

  it("restores mixed final thinking by its original segment index after text is split out", () => {
    const mixed: UiItem = { ...answer, timeline: [
      { kind: "text", text: "完成" }, { kind: "thinking", text: "最后核查" }, { kind: "text", text: "建议测试" },
    ] };
    const mixedRun = { ...run, final_answer: "完成建议测试" };
    useRun.setState(state => {
      state.tabs[sid].items = [user, process, { ...mixed, streaming: true }];
      state.tabs[sid].running = true;
    });
    const { container } = render(<App><ChatMessages /></App>);
    const scroller = container.querySelector<HTMLElement>(".chat-messages")!;
    Object.defineProperties(scroller, { scrollHeight: { configurable: true, value: 1500 }, clientHeight: { configurable: true, value: 200 } });
    const geometry = vi.spyOn(HTMLElement.prototype, "getBoundingClientRect").mockImplementation(function(this: HTMLElement) {
      const owner = this.closest<HTMLElement>("[data-key]")?.dataset.key;
      const logical = this.matches(".task-process [data-anchor-aliases]") ? 100
        : owner === "live:2" ? this.dataset.outputKey === "thinking:1" ? 800 : this.dataset.outputKey === "text:2" ? 900 : 700
          : this.dataset.key === "live:1" ? 100 : 0;
      const top = this === scroller ? 0 : logical - scroller.scrollTop;
      return { top, bottom: top + 32, height: 32, left: 0, right: 300, width: 300, x: 0, y: top, toJSON: () => ({}) };
    });
    try {
      scroller.scrollTop = 810;
      fireEvent.wheel(scroller, { deltaY: -50 });
      fireEvent.scroll(scroller);
      act(() => useRun.setState(state => {
        state.tabs[sid].items = [user, process, mixed];
        state.tabs[sid].taskRuns = [mixedRun];
        state.tabs[sid].running = false;
      }));
      const summary = container.querySelector<HTMLElement>(".task-process [data-output-group]")!;
      expect(JSON.parse(summary.dataset.outputGroup!)).toContain("thinking:1");
      expect(scroller.scrollTop).toBe(100);
    } finally { geometry.mockRestore(); }
  });

  it("calibrates live optimistic timestamps from terminal metadata and ignores a stale terminal", () => {
    const optimistic = "2026-10-15T09:59:59.123Z";
    useRun.setState(state => {
      state.tabs[sid].items = [{ ...user, createdAt: optimistic }, { ...answer, streaming: true, createdAt: optimistic }];
      state.tabs[sid].runUserCreatedAt = optimistic;
      state.tabs[sid].runId = "r1";
      state.tabs[sid].running = true;
    });
    const handlers = useRun.getState().bindGlobalHandlers();
    handlers["run:error"]({ session: sid, run_id: "old", task_run: { ...run, run_id: "old" } });
    expect(useRun.getState().tabs[sid].taskRuns).toBeUndefined();
    handlers["run:done"]({ session: sid, run_id: "r1", task_run: run });
    const tab = useRun.getState().tabs[sid];
    expect(tab.taskRuns).toEqual([run]);
    expect(tab.items[0].kind === "user" && tab.items[0].createdAt).toBe(started);
    expect(tab.items[1].kind === "assistant" && tab.items[1].createdAt).toBe(finished);
  });
});
