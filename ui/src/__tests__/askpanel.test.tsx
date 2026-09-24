// AskPanel refactor ([docs/run-queue-and-ask-revamp](../../../docs/run-queue-and-ask-revamp.md)): approval three options (incl. "always allow this project"), resolve payload / keyboard navigation / plan card
import { describe, it, expect, vi, afterEach } from "vitest";
import { render, screen, fireEvent, cleanup, waitFor, act } from "@testing-library/react";
import "../i18n"; // i18n init (nothing triggers it when rendering the component directly; otherwise t() returns the raw key)
import AskPanel from "../features/tools/AskPanel";
import type { ApprovalMode } from "../ipc/types";
import { useSessions } from "../stores/sessions";
import { useRun } from "../stores/run";

const calls: { cmd: string; args: any }[] = [];

vi.mock("@tauri-apps/api/core", () => ({
  invoke: vi.fn(async (cmd: string, args?: any) => {
    calls.push({ cmd, args });
    if (cmd === "read_workspace_file") return { path: args?.path ?? "", size: 10, content: "# 计划全文" };
    return null;
  }),
  Channel: class {
    onmessage: ((f: any) => void) | null = null;
  },
}));

const prefs = { approval_mode: "plan" as const, model_id: null, reasoning_effort: null };

function seed(mode: ApprovalMode = "plan") {
  useSessions.setState({
    tabs: [{ key: "s1", sessionId: "s1", workspace: "/tmp/ws", title: "s1", projectId: null, createdAt: "2026-09-01T00:00:00Z", prefs: { ...prefs, approval_mode: mode } }],
    activeKey: "s1",
    projects: [],
  });
}

function seedAsk(ask: any, mode: ApprovalMode = "plan") {
  seed(mode);
  useRun.setState((s) => {
    s.tabs["s1"] = {
      items: [], running: false, streamGen: 0, ask, breakdown: null,
      todos: [], suggestions: [], subs: [], subStreams: {}, subDrawer: { open: false, subId: null }, gitEntries: null, writeTick: 0,
      queue: [], pendingItemId: null, draftFromQueue: null, lastDoneRunId: null, compacting: false,
    };
  });
}

afterEach(() => {
  cleanup();
  calls.length = 0;
  useSessions.setState({ tabs: [], activeKey: null, projects: [] });
  useRun.setState((s) => {
    s.tabs = {}; s.drafts = {};
  });
});

// antd inserts a space inside two-character buttons: find buttons by whitespace-stripped text (pitfalls list)
function btnByText(text: string): HTMLElement {
  const b = Array.from(document.querySelectorAll("button")).find((x) =>
    (x.textContent ?? "").replace(/\s/g, "").includes(text),
  );
  if (!b) throw new Error(`button not found: ${text}`);
  return b as HTMLElement;
}

describe("AskPanel（docs/run-queue-and-ask-revamp）", () => {
  it("命令审批渲染三选项（允许/始终允许本项目/拒绝），点击回车送达 {approved, always} 载荷", async () => {
    seedAsk({ askId: "a1", kind: "approval", title: "高危命令确认", detail: "$ rm -rf x", allowAlways: true });
    render(<AskPanel />);
    expect(screen.getByText("允许")).toBeTruthy();
    expect(screen.getByText("始终允许本项目")).toBeTruthy();    expect(screen.getByText("拒绝")).toBeTruthy();
    // Click "always allow this project" → the card footer primary "Confirm" button → payload includes always
    fireEvent.click(screen.getByText("始终允许本项目"));
    const confirmBtn = Array.from(document.querySelectorAll(".ask-foot button")).find((b) =>
      (b.textContent ?? "").replace(/\s/g, "").includes("确认"),
    ) as HTMLElement;
    fireEvent.click(confirmBtn);
    await waitFor(() => expect(calls.some((c) => c.cmd === "resolve_ask")).toBe(true));
    const payload = calls.find((c) => c.cmd === "resolve_ask")?.args?.value;
    expect(payload.approved).toBe(true);
    expect(payload.always).toBe(true);
  });

  it("allowAlways=false 时不渲染第三选项；Enter 确认高亮项", async () => {
    seedAsk({ askId: "a2", kind: "approval", title: "高危命令确认", detail: "$ cmd", allowAlways: false });
    render(<AskPanel />);
    expect(screen.getByText("允许")).toBeTruthy();
    expect(screen.queryByText("始终允许本项目")).toBeNull();
    // First item (Allow) is highlighted by default → Enter
    fireEvent.keyDown(document.querySelector(".ask-card")!, { key: "Enter" });
    await waitFor(() => expect(calls.some((c) => c.cmd === "resolve_ask")).toBe(true));
    const payload = calls.find((c) => c.cmd === "resolve_ask")?.args?.value;
    expect(payload.approved).toBe(true);
    expect(payload.always).toBe(false);
  });

  it("计划卡片：渲染计划全文 + 「查看完整计划」打开文件查看器", async () => {
    seedAsk({
      askId: "a3", kind: "ask",
      planFile: "/ws/.codewave/tasks/plan-20260901-120000.md",
      questions: [{
        id: "q1", question: "## 步骤\n1. 改 A\n2. 验证 B",
        options: [{ id: "approve", label: "执行方案" }, { id: "revise", label: "补充意见" }],
      }],
    });
    render(<AskPanel />);
    expect(document.querySelector(".plan-card")).toBeTruthy();
    expect(screen.getByText("计划")).toBeTruthy();
    fireEvent.click(screen.getByText(/查看完整计划/));
    await waitFor(() => expect(calls.some((c) => c.cmd === "read_workspace_file" && c.args.path.includes("plan-20260901"))).toBe(true));
  });

  it("ask 提交载荷 {answers}：批准形点击「执行方案」即直接提交（免二次提交钮）", async () => {
    seedAsk({
      askId: "a4", kind: "ask",
      questions: [{
        id: "q1", question: "选一个",
        options: [{ id: "approve", label: "执行方案" }, { id: "revise", label: "补充意见" }],
      }],
    });
    render(<AskPanel />);
    // Clicking the approve option → immediate resolve_ask (no second submit button)
    fireEvent.click(screen.getByText("执行方案"));
    await waitFor(() => expect(calls.some((c) => c.cmd === "resolve_ask")).toBe(true));
    const payload = calls.find((c) => c.cmd === "resolve_ask")?.args?.value;
    expect(payload.answers.q1.selections).toContain("approve");
  });

  it("批准门第三选项「先看预览」：点击即直提，且不把 ConfirmEach 会话切到自动编辑档", async () => {
    seedAsk(
      {
        askId: "a4b", kind: "ask",
        questions: [{
          id: "q1", question: "是否按上述计划执行？",
          options: [
            { id: "approve", label: "执行方案", recommended: true },
            { id: "revise", label: "补充意见" },
            { id: "preview", label: "先看预览" },
          ],
        }],
      },
      "confirm_each",
    );
    render(<AskPanel />);
    fireEvent.click(screen.getByText("先看预览"));
    await waitFor(() => expect(calls.some((c) => c.cmd === "resolve_ask")).toBe(true));
    const payload = calls.find((c) => c.cmd === "resolve_ask")?.args?.value;
    expect(payload.answers.q1.selections).toEqual(["preview"]);
    // 等一拍让 submitWith 里 await resolveAsk 之后的同步/微任务跑完再断言「没有提权」
    //（否则 bug 版的 updatePrefs 可能晚于断言发生 → 假绿）
    await act(async () => {
      await new Promise((r) => setTimeout(r, 0));
    });
    // 「先看预览」既不是批准也不是有效应答：不得静默提权（后端 preview_only 同样排除）
    expect(calls.some((c) => c.cmd.includes("prefs"))).toBe(false);
    expect(useSessions.getState().tabs[0].prefs.approval_mode).toBe("confirm_each");
  });

  it("预览 + 补充说明：仍是「只看预览」，不切档（后端同口径：补充说明不算表态）", async () => {
    seedAsk(
      {
        askId: "a4d", kind: "ask",
        questions: [{
          id: "q1", question: "是否按上述计划执行？",
          options: [
            { id: "approve", label: "执行方案", recommended: true },
            { id: "revise", label: "补充意见" },
            { id: "preview", label: "先看预览" },
          ],
        }],
      },
      "confirm_each",
    );
    render(<AskPanel />);
    fireEvent.change(screen.getByPlaceholderText("补充说明（可选）"), { target: { value: "顺便看下界面" } });
    fireEvent.click(screen.getByText("先看预览"));
    await waitFor(() => expect(calls.some((c) => c.cmd === "resolve_ask")).toBe(true));
    const payload = calls.find((c) => c.cmd === "resolve_ask")?.args?.value;
    expect(payload.answers.q1.note).toBe("顺便看下界面");
    await act(async () => {
      await new Promise((r) => setTimeout(r, 0));
    });
    expect(calls.some((c) => c.cmd.includes("prefs"))).toBe(false);
    expect(useSessions.getState().tabs[0].prefs.approval_mode).toBe("confirm_each");
  });

  it("三选项批准门点「执行方案」：照旧直提并切到自动编辑档（守门不得带坏正常批准路径）", async () => {
    seedAsk(
      {
        askId: "a4c", kind: "ask",
        questions: [{
          id: "q1", question: "是否按上述计划执行？",
          options: [
            { id: "approve", label: "执行方案", recommended: true },
            { id: "revise", label: "补充意见" },
            { id: "preview", label: "先看预览" },
          ],
        }],
      },
      "confirm_each",
    );
    render(<AskPanel />);
    fireEvent.click(screen.getByText("执行方案"));
    await waitFor(() => expect(calls.some((c) => c.cmd === "resolve_ask")).toBe(true));
    const payload = calls.find((c) => c.cmd === "resolve_ask")?.args?.value;
    expect(payload.answers.q1.selections).toEqual(["approve"]);
    // 批准项照旧同步胶囊到自动编辑档（updatePrefs → set_session_prefs）
    await waitFor(() => expect(calls.some((c) => c.cmd.includes("prefs"))).toBe(true));
  });

  it("批准闸选「补充意见」：不切档（与后端同口径：闸形状上只有批准项才动档位）", async () => {
    seedAsk(
      {
        askId: "a4e", kind: "ask",
        questions: [{
          id: "q1", question: "是否按上述计划执行？",
          options: [
            { id: "approve", label: "执行方案", recommended: true },
            { id: "revise", label: "补充意见" },
            { id: "preview", label: "先看预览" },
          ],
        }],
      },
      "confirm_each",
    );
    render(<AskPanel />);
    fireEvent.click(screen.getByText("补充意见"));
    // 非批准项不直提：选中后点「提交回答」
    fireEvent.click(screen.getByText("提交回答"));
    await waitFor(() => expect(calls.some((c) => c.cmd === "resolve_ask")).toBe(true));
    await act(async () => {
      await new Promise((r) => setTimeout(r, 0));
    });
    expect(calls.some((c) => c.cmd.includes("prefs"))).toBe(false);
    expect(useSessions.getState().tabs[0].prefs.approval_mode).toBe("confirm_each");
  });

  it("非闸形状询问：有效应答照旧轻量切档（既有语义不得被闸形状收紧带坏）", async () => {
    seedAsk(
      {
        askId: "a4f", kind: "ask",
        questions: [{
          id: "q1", question: "选一个",
          options: [{ id: "a", label: "甲" }, { id: "b", label: "乙" }],
        }],
      },
      "confirm_each",
    );
    render(<AskPanel />);
    fireEvent.click(screen.getByText(/^乙/));
    fireEvent.click(screen.getByText("提交回答"));
    await waitFor(() => expect(calls.some((c) => c.cmd === "resolve_ask")).toBe(true));
    await waitFor(() => expect(calls.some((c) => c.cmd.includes("prefs"))).toBe(true));
  });

  it("ask 非批准形仍需提交钮：选中「补充意见」不直提，点提交后载荷携带", async () => {
    seedAsk({
      askId: "a5", kind: "ask",
      questions: [{
        id: "q1", question: "选一个",
        options: [{ id: "approve", label: "执行方案" }, { id: "revise", label: "补充意见" }],
      }],
    });
    render(<AskPanel />);
    fireEvent.click(screen.getByText("补充意见"));
    expect(calls.some((c) => c.cmd === "resolve_ask")).toBe(false); // not submitted directly
    fireEvent.click(screen.getByText("提交回答"));
    await waitFor(() => expect(calls.some((c) => c.cmd === "resolve_ask")).toBe(true));
    const payload = calls.find((c) => c.cmd === "resolve_ask")?.args?.value;
    expect(payload.answers.q1.selections).toContain("revise");
  });
  it("忽略缺陷回归（单题/末页）：点击「忽略」清空后直提，载荷 selections 为空", async () => {
    seedAsk({
      askId: "a6", kind: "ask",
      questions: [{
        id: "q1", question: "选一个",
        options: [{ id: "approve", label: "执行方案" }, { id: "revise", label: "补充意见" }],
      }],
    });
    render(<AskPanel />);
    fireEvent.click(btnByText("忽略"));
    await waitFor(() => expect(calls.some((c) => c.cmd === "resolve_ask")).toBe(true));
    const payload = calls.find((c) => c.cmd === "resolve_ask")?.args?.value;
    expect(payload.answers.q1.selections).toEqual([]);
  });

  it("忽略多题·非末页：清空当前题并翻页不提交，提交载荷验证已清空", async () => {
    seedAsk({
      askId: "a7", kind: "ask",
      questions: [
        { id: "q1", question: "第一题", options: [{ id: "a", label: "甲" }, { id: "b", label: "乙" }] },
        { id: "q2", question: "第二题", options: [{ id: "c", label: "丙" }] },
      ],
    });
    render(<AskPanel />);
    fireEvent.click(screen.getByText(/^甲/)); // select option A on question 1 (text carries a ✓ suffix, hence prefix match)
    fireEvent.click(btnByText("忽略")); // not the last page: clears q1 and moves to page 2, no resolve
    expect(calls.some((c) => c.cmd === "resolve_ask")).toBe(false);
    expect(screen.getByText("第二题")).toBeTruthy(); // page turned
    fireEvent.click(btnByText("提交回答"));
    await waitFor(() => expect(calls.some((c) => c.cmd === "resolve_ask")).toBe(true));
    const payload = calls.find((c) => c.cmd === "resolve_ask")?.args?.value;
    expect(payload.answers.q1.selections).toEqual([]); // ignore cleared question 1's answer
  });

  it("忽略多题·末页：直提且其余题作答保留、当前题清空", async () => {
    seedAsk({
      askId: "a8", kind: "ask",
      questions: [
        { id: "q1", question: "第一题", options: [{ id: "a", label: "甲" }] },
        { id: "q2", question: "第二题", options: [{ id: "c", label: "丙" }] },
      ],
    });
    render(<AskPanel />);
    fireEvent.click(screen.getByText(/^甲/)); // select option A on question 1 (answer must be kept)
    // Pager arrow enters the last page (bypassing ignore, keeping q1's answer); [docs/ask-ink-accent-and-composer-cover](../../../docs/ask-ink-accent-and-composer-cover.md) turned the pager into bare .pager-btn icons
    const pagerBtns = document.querySelectorAll(".ask-pager .pager-btn");
    fireEvent.click(pagerBtns[pagerBtns.length - 1]);
    expect(screen.getByText("第二题")).toBeTruthy();
    fireEvent.click(btnByText("忽略")); // last page: clears q2 then submits directly
    await waitFor(() => expect(calls.some((c) => c.cmd === "resolve_ask")).toBe(true));
    const payload = calls.find((c) => c.cmd === "resolve_ask")?.args?.value;
    expect(payload.answers.q1.selections).toContain("a"); // other question's answer kept
  });

  it("docs/notification-click-reveal：多题 ask（planFile 落盘）渲染计划卡片，双题文本拼接且每页题干仍显示", async () => {
    seedAsk({      askId: "a9", kind: "ask",
      planFile: "/ws/.codewave/tasks/plan-x.md",
      questions: [
        { id: "q1", question: "第一段方案", options: [{ id: "a", label: "甲" }] },
        { id: "q2", question: "第二段方案", options: [{ id: "c", label: "丙" }] },
      ],
    });
    render(<AskPanel />);
    expect(document.querySelector(".plan-card")).toBeTruthy(); // plan card applies to plain asks too
    expect(screen.getAllByText(/第一段方案/).length).toBeGreaterThan(0);
    expect(screen.getAllByText(/第二段方案/).length).toBeGreaterThan(0);
    expect(document.querySelector(".q-text")).toBeTruthy(); // per-page question text no longer hidden by isPlan
    fireEvent.click(screen.getByText(/查看完整计划/));
    await waitFor(() => expect(calls.some((c) => c.cmd === "read_workspace_file")).toBe(true));
  });

  it("docs/notification-click-reveal：confirm_each 档有效应答提交 → 胶囊同步切 auto_edit", async () => {
    seedAsk(
      {
        askId: "a10", kind: "ask",
        questions: [{ id: "q1", question: "选一个", options: [{ id: "a", label: "甲" }] }],
      },
      "confirm_each",
    );
    render(<AskPanel />);
    expect(screen.getByText(/提交后将切换到自动编辑档/)).toBeTruthy(); // mode upgrade made explicit
    fireEvent.click(screen.getByText(/^甲/));
    fireEvent.click(btnByText("提交回答"));
    await waitFor(() => expect(calls.some((c) => c.cmd === "set_session_prefs")).toBe(true));
    const prefsCall = calls.find((c) => c.cmd === "set_session_prefs");
    expect(prefsCall?.args?.prefs?.approval_mode).toBe("auto_edit");
  });

  it("docs/notification-click-reveal：confirm_each 档空应答提交 → 不切档不调 set_session_prefs", async () => {
    seedAsk(
      {
        askId: "a11", kind: "ask",
        questions: [{ id: "q1", question: "选一个", options: [{ id: "a", label: "甲" }] }],
      },
      "confirm_each",
    );
    render(<AskPanel />);
    fireEvent.click(btnByText("提交回答")); // submit without selecting anything
    await waitFor(() => expect(calls.some((c) => c.cmd === "resolve_ask")).toBe(true));
    expect(calls.some((c) => c.cmd === "set_session_prefs")).toBe(false);
  });

  it("docs/notification-click-reveal：plan 档空应答不触发胶囊同步（协议不变）", async () => {
    seedAsk({
      askId: "a12", kind: "ask",
      questions: [{ id: "q1", question: "选一个", options: [{ id: "approve", label: "执行方案" }, { id: "revise", label: "补充意见" }] }],
    });
    render(<AskPanel />);
    fireEvent.click(btnByText("提交回答"));
    await waitFor(() => expect(calls.some((c) => c.cmd === "resolve_ask")).toBe(true));
    expect(calls.some((c) => c.cmd === "set_session_prefs")).toBe(false);
  });

  it("docs/notification-click-reveal：plan 档批准直提（执行方案）→ 胶囊同步切 auto_edit（planPath 分支）", async () => {
    seedAsk({
      askId: "a13", kind: "ask",
      questions: [{ id: "q1", question: "选一个", options: [{ id: "approve", label: "执行方案" }, { id: "revise", label: "补充意见" }] }],
    });
    render(<AskPanel />);
    fireEvent.click(screen.getByText("执行方案")); // approve-type submits directly
    await waitFor(() => expect(calls.some((c) => c.cmd === "set_session_prefs")).toBe(true));
    const prefsCall = calls.find((c) => c.cmd === "set_session_prefs");
    expect(prefsCall?.args?.prefs?.approval_mode).toBe("auto_edit");
  });

  it("docs/notification-click-reveal：confirm_each + switchToAutoEdit 批准形 → 胶囊同步（flag 通道）", async () => {
    seedAsk(
      {
        askId: "a14", kind: "ask", switchToAutoEdit: true,
        questions: [{ id: "q1", question: "选一个", options: [{ id: "approve", label: "执行方案" }, { id: "revise", label: "补充意见" }] }],
      },
      "confirm_each",
    );
    render(<AskPanel />);
    fireEvent.click(screen.getByText("执行方案"));
    await waitFor(() => expect(calls.some((c) => c.cmd === "set_session_prefs")).toBe(true));
    const prefsCall = calls.find((c) => c.cmd === "set_session_prefs");
    expect(prefsCall?.args?.prefs?.approval_mode).toBe("auto_edit");
  });

  it("空载荷缺陷回归：recommended 项默认选中，直接提交载荷携带 recommended", async () => {
    seedAsk({
      askId: "a15", kind: "ask",
      questions: [{
        id: "q1", question: "选一个",
        options: [
          { id: "approve", label: "执行方案", recommended: true },
          { id: "revise", label: "补充意见" },
        ],
      }],
    });
    render(<AskPanel />);
    // Submit without clicking any option → recommended fallback applies
    fireEvent.click(btnByText("提交回答"));
    await waitFor(() => expect(calls.some((c) => c.cmd === "resolve_ask")).toBe(true));
    const payload = calls.find((c) => c.cmd === "resolve_ask")?.args?.value;
    expect(payload.answers.q1.selections).toContain("approve");
    // Pill sync: plan mode + approve → switch to auto_edit
    await waitFor(() => expect(calls.some((c) => c.cmd === "set_session_prefs")).toBe(true));
  });

  it("空载荷缺陷回归：忽略路径不受 recommended 兑底影响（保持未回答语义）", async () => {
    seedAsk({
      askId: "a16", kind: "ask",
      questions: [{
        id: "q1", question: "选一个",
        options: [
          { id: "approve", label: "执行方案", recommended: true },
          { id: "revise", label: "补充意见" },
        ],
      }],
    });
    render(<AskPanel />);
    fireEvent.click(btnByText("忽略")); // last-page direct submit: does not clear recommended, sends an empty payload
    await waitFor(() => expect(calls.some((c) => c.cmd === "resolve_ask")).toBe(true));
    const payload = calls.find((c) => c.cmd === "resolve_ask")?.args?.value;
    expect(payload.answers.q1.selections).toEqual([]);
    expect(calls.some((c) => c.cmd === "set_session_prefs")).toBe(false); // empty payload does not switch mode
  });
});

// ---------- [docs/ask-approval-shape-note-nav](../../../docs/ask-approval-shape-note-nav.md): approval-shape lenient detection + option visuals + note input in the keyboard ring ----------

describe("AskPanel docs/ask-approval-shape-note-nav", () => {
  afterEach(() => {
    cleanup();
    calls.length = 0;
    useSessions.setState({ tabs: [], activeKey: null, projects: [] });
    useRun.setState((s) => {
      s.tabs = {}; s.drafts = {};
    });
  });

  const shapeAsk = {
    askId: "d1", kind: "ask",
    // Backend lenient detection (mapped to camelCase by run.ts ask:opened): id self-invented (execute), label compliant
    approval: true, approveId: "execute",
    questions: [{
      id: "q1", question: "是否按此方案执行？",
      options: [{ id: "execute", label: "执行方案" }, { id: "feedback", label: "补充意见" }],
    }],
  };

  it("后端形状标记：id 自拟也按批准形渲染（radio + 单选 hint），批准项直提", async () => {
    seedAsk(shapeAsk);
    render(<AskPanel />);
    // radio indicators + role
    expect(document.querySelectorAll(".opt-box.radio")).toHaveLength(2);
    expect(document.querySelectorAll('.ask-opt[role="radio"]')).toHaveLength(2);
    // hint shows the single-select wording ("Enter confirms"), not the multi-select one
    expect(screen.getByText(/回车确认/)).toBeTruthy();
    // clicking the approval option (approve_id=execute) submits directly, no submit button needed
    fireEvent.click(screen.getByText("执行方案"));
    await waitFor(() => expect(calls.some((c) => c.cmd === "resolve_ask")).toBe(true));
    const payload = calls.find((c) => c.cmd === "resolve_ask")?.args?.value;
    expect(payload.answers.q1.selections).toEqual(["execute"]);
  });

  it("批准形单选互斥：选补充意见后再选不叠加，提交载荷单项", async () => {
    seedAsk(shapeAsk);
    render(<AskPanel />);
    fireEvent.click(screen.getByText("补充意见"));
    const checked = document.querySelectorAll(".opt-box.radio.checked");
    expect(checked).toHaveLength(1);
    expect((checked[0].parentElement as HTMLElement).textContent).toContain("补充意见");
    fireEvent.click(btnByText("提交回答"));
    await waitFor(() => expect(calls.some((c) => c.cmd === "resolve_ask")).toBe(true));
    expect(calls.find((c) => c.cmd === "resolve_ask")?.args?.value.answers.q1.selections).toEqual(["feedback"]);
  });

  it("多选题：复选框视觉 + 两项可同时勾选（不互斥）+ 多选 hint", () => {
    seedAsk({
      askId: "d2", kind: "ask",
      questions: [{
        id: "q1", question: "选择要执行的模块",
        options: [{ id: "ma", label: "模块甲" }, { id: "mb", label: "模块乙" }],
      }],
    });
    render(<AskPanel />);
    expect(document.querySelectorAll(".opt-box.check")).toHaveLength(2);
    expect(document.querySelectorAll('.ask-opt[role="checkbox"]')).toHaveLength(2);
    expect(screen.getByText(/空格选中/)).toBeTruthy();
    fireEvent.click(screen.getByText("模块甲"));
    fireEvent.click(screen.getByText("模块乙"));
    expect(document.querySelectorAll(".opt-box.check.checked")).toHaveLength(2); // multi-select is not exclusive
  });

  it("键盘导航环：Tab/箭头循环高亮（末槽 = 补充输入）；空格切换高亮项（回车不再用于选中）", () => {
    seedAsk({
      askId: "d3", kind: "ask",
      questions: [{
        id: "q1", question: "选一个",
        options: [{ id: "a", label: "甲" }, { id: "b", label: "乙" }],
      }],
    });
    render(<AskPanel />);
    const card = document.querySelector(".ask-card")!;
    const note = document.querySelector(".ask-note") as HTMLElement;
    const key = (k: string) => fireEvent.keyDown(card, { key: k, bubbles: true });
    key("Tab"); // cursor=1 (option B)
    key(" "); // 空格 = 切换高亮项（回车已让位给 下一题/提交）
    const checked = document.querySelectorAll(".opt-box.check.checked");
    expect(checked).toHaveLength(1);
    expect((checked[0].parentElement as HTMLElement).textContent).toContain("乙");
    expect(calls.some((c) => c.cmd === "resolve_ask")).toBe(false); // 空格只选中不提交
    key(" "); // 再按取消
    expect(document.querySelectorAll(".opt-box.check.checked")).toHaveLength(0);
    // 箭头循环含补充输入末槽（Tab 末槽放行自然焦点，环循环由箭头承担）
    key("ArrowDown"); // cursor 1 → 2 = 补充输入末槽
    expect(note.className).toContain("kb");
    key("ArrowDown"); // 环回选项头
    expect(note.className).not.toContain("kb");
    expect(document.querySelectorAll(".ask-opt")[0].className).toContain("kb");
  });

  it("回车 = 提交（单题/末页）：高亮在补充输入槽时回车直接提交整个 ask", async () => {
    seedAsk({
      askId: "d6", kind: "ask",
      questions: [{
        id: "q1", question: "选一个",
        options: [{ id: "a", label: "甲" }, { id: "b", label: "乙" }],
      }],
    });
    render(<AskPanel />);
    const card = document.querySelector(".ask-card")!;
    const key = (k: string) => fireEvent.keyDown(card, { key: k, bubbles: true });
    key("Tab");
    key("Tab"); // cursor 到补充输入末槽
    key("Enter"); // 回车 = 提交（不再聚焦输入框；输入框经 Tab/鼠标可达）
    await waitFor(() => expect(calls.some((c) => c.cmd === "resolve_ask")).toBe(true));
  });

  it("回车 = 下一题（多题非末页，不 resolve）；末页回车 = 提交", async () => {
    seedAsk({
      askId: "d7", kind: "ask",
      questions: [
        { id: "q1", question: "第一题", options: [{ id: "a", label: "甲" }] },
        { id: "q2", question: "第二题", options: [{ id: "c", label: "丙" }] },
      ],
    });
    render(<AskPanel />);
    const card = document.querySelector(".ask-card")!;
    fireEvent.keyDown(card, { key: "Enter", bubbles: true }); // 非末页：翻页
    expect(screen.getByText("第二题")).toBeTruthy();
    expect(calls.some((c) => c.cmd === "resolve_ask")).toBe(false);
    fireEvent.keyDown(card, { key: "Enter", bubbles: true }); // 末页：提交
    await waitFor(() => expect(calls.some((c) => c.cmd === "resolve_ask")).toBe(true));
    const payload = calls.find((c) => c.cmd === "resolve_ask")?.args?.value;
    expect(payload.answers.q1.selections).toEqual([]); // 未作答题保持空（回车提交与按钮提交同路径）
  });

  it("审批形态：radio checked 跟随键盘高亮位（待确认项可视）", () => {
    seedAsk({ askId: "d4", kind: "approval", title: "高危命令确认", detail: "$ cmd", allowAlways: false });
    render(<AskPanel />);
    // allowAlways=false → allow/deny, two options only
    expect(document.querySelectorAll(".opt-box.radio")).toHaveLength(2);
    expect((document.querySelectorAll(".opt-box.radio.checked")[0].parentElement as HTMLElement).textContent).toContain("允许");
    fireEvent.keyDown(document.querySelector(".ask-card")!, { key: "ArrowDown", bubbles: true });
    const checked = document.querySelectorAll(".opt-box.radio.checked");
    expect(checked).toHaveLength(1);
    expect((checked[0].parentElement as HTMLElement).textContent).toContain("拒绝");
  });

  it("recommended 标记：✓ 后缀退役改描边推荐 pill", () => {
    seedAsk({
      askId: "d5", kind: "ask", approval: true, approve_id: "approve",
      questions: [{
        id: "q1", question: "选一个",
        options: [{ id: "approve", label: "执行方案", recommended: true }, { id: "revise", label: "补充意见" }],
      }],
    });
    render(<AskPanel />);
    expect(document.querySelector(".rec-pill")?.textContent).toBe("推荐");
    const label = Array.from(document.querySelectorAll(".ask-opt .label")).find((el) => el.textContent === "执行方案");
    expect(label).toBeTruthy(); // the ✓ suffix is retired; the label is clean
  });
});

// ---------- single-question shape: model-declared exclusive options (radio, replace-style toggle) ----------

describe("AskPanel 单选题（single）", () => {
  afterEach(() => {
    cleanup();
    calls.length = 0;
    useSessions.setState({ tabs: [], activeKey: null, projects: [] });
    useRun.setState((s) => {
      s.tabs = {}; s.drafts = {};
    });
  });

  const singleAsk = {
    askId: "s1", kind: "ask",
    questions: [{
      id: "q1", question: "归档文件名是否保留序号前缀？", single: true,
      options: [{ id: "keep", label: "保留序号前缀", recommended: true }, { id: "strip", label: "彻底去掉序号" }],
    }],
  };

  it("single 题渲染 radio（非 checkbox）+ 单选 hint，与多选题视觉区分", () => {
    seedAsk(singleAsk);
    render(<AskPanel />);
    expect(document.querySelectorAll(".opt-box.radio")).toHaveLength(2);
    expect(document.querySelectorAll('.ask-opt[role="radio"]')).toHaveLength(2);
    expect(document.querySelectorAll(".opt-box.check")).toHaveLength(0);
    expect(screen.getByText(/单选：点选或数字键选中/)).toBeTruthy();
    expect(screen.queryByText(/空格选中/)).toBeNull();
  });

  it("互斥替换：选新项替换旧选中；点已选项保持选中（radio 语义不出现取消态）", () => {
    seedAsk(singleAsk);
    render(<AskPanel />);
    fireEvent.click(screen.getByText("彻底去掉序号"));
    let checked = document.querySelectorAll(".opt-box.radio.checked");
    expect(checked).toHaveLength(1);
    expect((checked[0].parentElement as HTMLElement).textContent).toContain("彻底去掉序号");
    // pick the other → replaces (still exactly one checked)
    fireEvent.click(screen.getByText("保留序号前缀"));
    checked = document.querySelectorAll(".opt-box.radio.checked");
    expect(checked).toHaveLength(1);
    expect((checked[0].parentElement as HTMLElement).textContent).toContain("保留序号前缀");
    // re-pick the picked one → stays selected（radio 语义：单击不取消）
    fireEvent.click(screen.getByText("保留序号前缀"));
    checked = document.querySelectorAll(".opt-box.radio.checked");
    expect(checked).toHaveLength(1);
    expect((checked[0].parentElement as HTMLElement).textContent).toContain("保留序号前缀");
  });

  it("提交载荷 selections 单元素", async () => {
    seedAsk(singleAsk);
    render(<AskPanel />);
    fireEvent.click(screen.getByText("彻底去掉序号"));
    fireEvent.click(btnByText("提交回答"));
    await waitFor(() => expect(calls.some((c) => c.cmd === "resolve_ask")).toBe(true));
    expect(calls.find((c) => c.cmd === "resolve_ask")?.args?.value.answers.q1.selections).toEqual(["strip"]);
  });

  it("recommended 预选；点已选项保持选中（radio 语义），直接提交载荷携带 recommended", async () => {
    seedAsk({ ...singleAsk, askId: "s2" });
    render(<AskPanel />);
    expect(document.querySelectorAll(".opt-box.radio.checked")).toHaveLength(1); // recommended keep 预选
    fireEvent.click(screen.getByText("保留序号前缀")); // 点已选项：保持选中（radio 语义不取消）
    expect(document.querySelectorAll(".opt-box.radio.checked")).toHaveLength(1);
    fireEvent.click(btnByText("提交回答"));
    await waitFor(() => expect(calls.some((c) => c.cmd === "resolve_ask")).toBe(true));
    expect(calls.find((c) => c.cmd === "resolve_ask")?.args?.value.answers.q1.selections).toEqual(["keep"]);
  });

  it("批准形优先：approval 题即使带 single:true 仍走批准形直提，不受影响", async () => {
    seedAsk({
      askId: "s3", kind: "ask", approval: true, approveId: "execute",
      questions: [{
        id: "q1", question: "是否按此方案执行？", single: true,
        options: [{ id: "execute", label: "执行方案" }, { id: "feedback", label: "补充意见" }],
      }],
    });
    render(<AskPanel />);
    fireEvent.click(screen.getByText("执行方案"));
    await waitFor(() => expect(calls.some((c) => c.cmd === "resolve_ask")).toBe(true));
    const payload = calls.find((c) => c.cmd === "resolve_ask")?.args?.value;
    expect(payload.answers.q1.selections).toEqual(["execute"]);
    // direct submit path: exactly one resolve call, submitted at the click
    expect(calls.filter((c) => c.cmd === "resolve_ask")).toHaveLength(1);
  });

  it("键盘路径：数字键选中新项替换预选，重复按键保持选中（radio 语义不取消）", () => {
    seedAsk(singleAsk);
    render(<AskPanel />);
    const card = document.querySelector(".ask-card")!;
    const key = (k: string) => fireEvent.keyDown(card, { key: k, bubbles: true });
    expect(document.querySelectorAll(".opt-box.radio.checked")).toHaveLength(1); // recommended keep 预选
    key("2"); // number key → replace-style: keep cleared, strip checked
    let checked = document.querySelectorAll(".opt-box.radio.checked");
    expect(checked).toHaveLength(1);
    expect((checked[0].parentElement as HTMLElement).textContent).toContain("彻底去掉序号");
    key("2"); // repeat → 保持选中（radio 语义：单击不取消）
    checked = document.querySelectorAll(".opt-box.radio.checked");
    expect(checked).toHaveLength(1);
    expect((checked[0].parentElement as HTMLElement).textContent).toContain("彻底去掉序号");
  });

  it("多题分页中 single 题互斥：两题各自单选，互不串扰", () => {
    seedAsk({
      askId: "s4", kind: "ask",
      questions: [
        { id: "q1", question: "第一问", single: true, options: [{ id: "a", label: "甲" }, { id: "b", label: "乙" }] },
        { id: "q2", question: "第二问", options: [{ id: "c", label: "丙" }, { id: "d", label: "丁" }] },
      ],
    });
    render(<AskPanel />);
    // page 1: single question renders radio; multi-question ask renders the pager
    expect(screen.getByText("1/2")).toBeTruthy();
    expect(document.querySelectorAll(".opt-box.radio")).toHaveLength(2);
    fireEvent.click(screen.getByText("甲"));
    // page 2: multi question renders checkbox (pager buttons are aria-label only, no text node)
    fireEvent.click(document.querySelector('[aria-label="下一题"]')!);
    expect(document.querySelectorAll(".opt-box.check")).toHaveLength(2);
    fireEvent.click(screen.getByText("丙"));
    fireEvent.click(screen.getByText("丁")); // multi stays additive
    expect(document.querySelectorAll(".opt-box.check.checked")).toHaveLength(2);
  });
});

// ---------- plan-approval question de-dup: the plan card above already renders the full plan ----------

describe("AskPanel 问句去重", () => {
  afterEach(() => {
    cleanup();
    calls.length = 0;
    useSessions.setState({ tabs: [], activeKey: null, projects: [] });
    useRun.setState((s) => {
      s.tabs = {}; s.drafts = {};
    });
  });

  const longPlan = "【方案】第一步\n第二步\n第三步";

  it("计划批准形单题：题干精简为固定问句，不再重复方案全文", () => {
    seedAsk({
      askId: "p1", kind: "ask", approval: true, approveId: "execute",
      planFile: "/ws/.codewave/tasks/plan-x.md",
      questions: [{ id: "q1", question: longPlan, options: [{ id: "execute", label: "执行方案" }, { id: "feedback", label: "补充意见" }] }],
    });
    render(<AskPanel />);
    const qt = document.querySelector(".q-text");
    expect(qt?.textContent).toBe("是否按上述计划执行？");
  });

  it("向后兼容：无 approval 字段时经 fallback（单题含 approve 选项）同样替换", () => {
    seedAsk({
      askId: "p2", kind: "ask",
      planFile: "/ws/.codewave/tasks/plan-x.md",
      questions: [{ id: "q1", question: longPlan, options: [{ id: "approve", label: "执行方案" }, { id: "revise", label: "补充意见" }] }],
    });
    render(<AskPanel />);
    expect(document.querySelector(".q-text")?.textContent).toBe("是否按上述计划执行？");
  });

  it("多题分页：每页题干保留原文不替换", () => {
    seedAsk({
      askId: "p3", kind: "ask",
      planFile: "/ws/.codewave/tasks/plan-x.md",
      questions: [
        { id: "q1", question: "第一段方案", options: [{ id: "a", label: "甲" }] },
        { id: "q2", question: "第二段方案", options: [{ id: "c", label: "丙" }] },
      ],
    });
    render(<AskPanel />);
    expect(document.querySelector(".q-text")?.textContent).toBe("第一段方案");
  });

  it("非批准形单题：题干保留原文", () => {
    seedAsk({
      askId: "p4", kind: "ask",
      planFile: "/ws/.codewave/tasks/plan-x.md",
      questions: [{ id: "q1", question: "选择要执行的模块", options: [{ id: "ma", label: "模块甲" }, { id: "mb", label: "模块乙" }] }],
    });
    render(<AskPanel />);
    expect(document.querySelector(".q-text")?.textContent).toBe("选择要执行的模块");
  });

  it("守卫分支：approval=true + 多题时 approvalShape 为真但仍保留每页原文", () => {
    seedAsk({
      askId: "p5", kind: "ask", approval: true, approveId: "execute",
      planFile: "/ws/.codewave/tasks/plan-x.md",
      questions: [
        { id: "q1", question: "第一段方案", options: [{ id: "execute", label: "执行方案" }] },
        { id: "q2", question: "第二段方案", options: [{ id: "feedback", label: "补充意见" }] },
      ],
    });
    render(<AskPanel />);
    expect(document.querySelector(".q-text")?.textContent).toBe("第一段方案");
  });
});

// ---------- 多题提交分页（缺陷修复：首页点提交不再把未浏览的题用推荐值静默提交全卷） ----------

describe("AskPanel 多题提交分页", () => {
  afterEach(() => {
    cleanup();
    calls.length = 0;
    useSessions.setState({ tabs: [], activeKey: null, projects: [] });
    useRun.setState((s) => {
      s.tabs = {}; s.drafts = {};
    });
  });

  const twoQAsk = {
    askId: "m1", kind: "ask",
    questions: [
      { id: "q1", question: "第一题", options: [{ id: "a", label: "甲" }, { id: "b", label: "乙" }] },
      { id: "q2", question: "第二题", options: [{ id: "c", label: "丙", recommended: true }, { id: "d", label: "丁" }] },
    ],
  };

  it("非末页主按钮显示「下一题」：点击仅翻页，不 resolve", () => {
    seedAsk(twoQAsk);
    render(<AskPanel />);
    expect(screen.queryByText("提交回答")).toBeNull();
    fireEvent.click(btnByText("下一题"));
    expect(screen.getByText("第二题")).toBeTruthy();
    expect(calls.some((c) => c.cmd === "resolve_ask")).toBe(false);
  });

  it("末页按钮回到「提交回答」：一次点击提交全部题（未作答的末题由 recommended 兜底，且只发生一次 resolve）", async () => {
    seedAsk(twoQAsk);
    render(<AskPanel />);
    fireEvent.click(screen.getByText(/^甲/)); // 答第一题
    fireEvent.click(btnByText("下一题")); // 翻页（不提交）
    expect(calls.some((c) => c.cmd === "resolve_ask")).toBe(false);
    fireEvent.click(btnByText("提交回答")); // 末页才真正提交
    await waitFor(() => expect(calls.some((c) => c.cmd === "resolve_ask")).toBe(true));
    const payload = calls.find((c) => c.cmd === "resolve_ask")?.args?.value;
    expect(payload.answers.q1.selections).toContain("a");
    expect(payload.answers.q2.selections).toContain("c");
    expect(calls.filter((c) => c.cmd === "resolve_ask")).toHaveLength(1);
  });

  it("单题 ask 主按钮仍是「提交回答」且直接提交", async () => {
    seedAsk({
      askId: "m2", kind: "ask",
      questions: [{ id: "q1", question: "选一个", options: [{ id: "a", label: "甲" }] }],
    });
    render(<AskPanel />);
    fireEvent.click(btnByText("提交回答"));
    await waitFor(() => expect(calls.some((c) => c.cmd === "resolve_ask")).toBe(true));
  });

  it("修复：在补充说明输入框内按回车 = 提交（此前 isFormTarget 早退，输入框内回车完全无反应）", async () => {
    seedAsk({
      askId: "kb1", kind: "ask",
      questions: [{ id: "q1", question: "选一个", options: [{ id: "a", label: "甲" }] }],
    });
    render(<AskPanel />);
    const note = (document.querySelector(".ask-note input") ?? document.querySelector(".ask-note")) as HTMLInputElement;
    expect(note).toBeTruthy();
    fireEvent.change(note, { target: { value: "补充说明文字" } });
    fireEvent.keyDown(note, { key: "Enter" });
    await waitFor(() => expect(calls.some((c) => c.cmd === "resolve_ask")).toBe(true));
    const payload = calls.find((c) => c.cmd === "resolve_ask")?.args?.value;
    expect(payload.answers.q1.note).toBe("补充说明文字");
  });
});

// ---------- 批准门选档（[docs/mode-gate-and-subagent-sync]）：选项带 mode 即直提 + 胶囊按实际档位同步 ----------

describe("AskPanel 批准门选档（[docs/mode-gate-and-subagent-sync]）", () => {
  afterEach(() => {
    cleanup();
    calls.length = 0;
    useSessions.setState({ tabs: [], activeKey: null, projects: [] });
    useRun.setState((s) => {
      s.tabs = {}; s.drafts = {};
    });
  });

  // 后端批准门形态：两个批准类选项各带目标档位（auto_edit / full_access）+ 一个非批准类选项
  const gateAsk = (askId: string) => ({
    askId, kind: "ask", approval: true, approveId: null,
    planFile: "/ws/.codewave/tasks/plan-gate.md",
    questions: [{
      id: "q1", question: "【方案】第一步",
      options: [
        { id: "mode_auto", label: "以自动编辑档执行", mode: "auto_edit", recommended: true },
        { id: "mode_full", label: "以完全访问档执行", mode: "full_access" },
        { id: "revise", label: "补充意见" },
      ],
    }],
  });

  it("带 mode 的选项点一下就直提：自动编辑档选项直接提交且胶囊同步 auto_edit", async () => {
    seedAsk(gateAsk("g1"));
    render(<AskPanel />);
    fireEvent.click(screen.getByText("以自动编辑档执行"));
    await waitFor(() => expect(calls.some((c) => c.cmd === "resolve_ask")).toBe(true));
    expect(calls.filter((c) => c.cmd === "resolve_ask")).toHaveLength(1);
    expect(calls.find((c) => c.cmd === "resolve_ask")?.args?.value.answers.q1.selections).toEqual(["mode_auto"]);
    await waitFor(() => expect(calls.some((c) => c.cmd === "set_session_prefs")).toBe(true));
    expect(calls.find((c) => c.cmd === "set_session_prefs")?.args?.prefs?.approval_mode).toBe("auto_edit");
  });

  it("带 mode 的选项点一下就直提：完全访问档选项直接提交且胶囊同步 full_access（不再硬编码 auto_edit）", async () => {
    seedAsk(gateAsk("g2"));
    render(<AskPanel />);
    fireEvent.click(screen.getByText("以完全访问档执行"));
    await waitFor(() => expect(calls.some((c) => c.cmd === "resolve_ask")).toBe(true));
    expect(calls.filter((c) => c.cmd === "resolve_ask")).toHaveLength(1);
    expect(calls.find((c) => c.cmd === "resolve_ask")?.args?.value.answers.q1.selections).toEqual(["mode_full"]);
    await waitFor(() => expect(calls.some((c) => c.cmd === "set_session_prefs")).toBe(true));
    expect(calls.find((c) => c.cmd === "set_session_prefs")?.args?.prefs?.approval_mode).toBe("full_access");
  });

  it("非批准类选项（无 mode）不直提：点「补充意见」仍需提交钮，且不切档", async () => {
    seedAsk(gateAsk("g3"));
    render(<AskPanel />);
    fireEvent.click(screen.getByText("补充意见"));
    expect(calls.some((c) => c.cmd === "resolve_ask")).toBe(false);
    fireEvent.click(btnByText("提交回答"));
    await waitFor(() => expect(calls.some((c) => c.cmd === "resolve_ask")).toBe(true));
    expect(calls.find((c) => c.cmd === "resolve_ask")?.args?.value.answers.q1.selections).toEqual(["revise"]);
    expect(calls.some((c) => c.cmd === "set_session_prefs")).toBe(false);
  });

  it("完全访问档无 description：渲染兜底风险说明行（其余档位不补文案）", () => {
    seedAsk(gateAsk("g4"));
    render(<AskPanel />);
    const optOf = (label: string) =>
      Array.from(document.querySelectorAll(".ask-opt")).find((el) => el.textContent?.includes(label)) as HTMLElement;
    expect(optOf("以完全访问档执行").querySelector(".desc")?.textContent).toBe("跳过所有审批弹窗，灾难级命令仍拦截");
    expect(optOf("以自动编辑档执行").querySelector(".desc")).toBeNull();
    expect(optOf("补充意见").querySelector(".desc")).toBeNull();
  });

  it("完全访问档带 description：后端文案在前，兜底风险说明恒定叠在其后（不被模型 description 顶掉）", () => {
    seedAsk({
      askId: "g5", kind: "ask", approval: true,
      questions: [{
        id: "q1", question: "【方案】",
        options: [{ id: "mode_full", label: "以完全访问档执行", mode: "full_access", description: "模型自述的风险" }],
      }],
    });
    render(<AskPanel />);
    const desc = document.querySelector(".ask-opt .desc")?.textContent ?? "";
    // 后端文案仍在前（不被覆盖），兜底风险文案恒定在后（提示注入式淡化无效）
    expect(desc).toBe("模型自述的风险；跳过所有审批弹窗，灾难级命令仍拦截");
  });

  it("旧形态回归（无 mode、id=approve）：直提行为不变，胶囊回落 auto_edit", async () => {
    seedAsk({
      askId: "g6", kind: "ask", approval: true, approveId: "approve",
      questions: [{
        id: "q1", question: "选一个",
        options: [{ id: "approve", label: "执行方案" }, { id: "revise", label: "补充意见" }],
      }],
    });
    render(<AskPanel />);
    fireEvent.click(screen.getByText("执行方案"));
    await waitFor(() => expect(calls.some((c) => c.cmd === "resolve_ask")).toBe(true));
    expect(calls.find((c) => c.cmd === "resolve_ask")?.args?.value.answers.q1.selections).toEqual(["approve"]);
    await waitFor(() => expect(calls.some((c) => c.cmd === "set_session_prefs")).toBe(true));
    expect(calls.find((c) => c.cmd === "set_session_prefs")?.args?.prefs?.approval_mode).toBe("auto_edit");
  });
});

// ---------- 档位已经是 auto_edit / full_access 时的胶囊同步（[docs/mode-gate-and-subagent-sync]） ----------
// 缺陷复现：后端 wants_mode_switch 对 AutoEdit / FullAccess 档也返回 mode_requested（会切档），
// 但前端同步条件曾只看当前档是不是 plan / confirm_each——两条路径全假 → 不发 set_session_prefs，
// 于是胶囊与后端档位分叉，之后任意一次 prefs 全量写入又把后端档位静默翻回去（无测试守护）。
describe("AskPanel 高档位下的胶囊同步", () => {
  afterEach(() => {
    cleanup();
    calls.length = 0;
    useSessions.setState({ tabs: [], activeKey: null, projects: [] });
    useRun.setState((s) => {
      s.tabs = {}; s.drafts = {};
    });
  });

  const twoModeAsk = (askId: string) => ({
    askId, kind: "ask", approval: true, approveId: null,
    questions: [{
      id: "q1", question: "【方案】下一步怎么做",
      options: [
        { id: "mode_auto", label: "以自动编辑档执行", mode: "auto_edit" },
        { id: "mode_full", label: "以完全访问档执行", mode: "full_access" },
      ],
    }],
  });

  it("档位 auto_edit（无 switchToAutoEdit）+ 选 full_access：仍发 set_session_prefs 且档位 = full_access（升级方向）", async () => {
    seedAsk(twoModeAsk("h1"), "auto_edit");
    render(<AskPanel />);
    fireEvent.click(screen.getByText("以完全访问档执行"));
    await waitFor(() => expect(calls.some((c) => c.cmd === "resolve_ask")).toBe(true));
    expect(calls.find((c) => c.cmd === "resolve_ask")?.args?.value.answers.q1.selections).toEqual(["mode_full"]);
    // 修理前：当前档位不是 plan / confirm_each 且 ask 未带 switchToAutoEdit → 不发 prefs（本断言失败）
    await waitFor(() => expect(calls.some((c) => c.cmd === "set_session_prefs")).toBe(true));
    expect(calls.find((c) => c.cmd === "set_session_prefs")?.args?.prefs?.approval_mode).toBe("full_access");
    // 顺序契约：先 resolve_ask 再 set_session_prefs（颠倒会让后端误判「无需切档」）
    expect(calls.findIndex((c) => c.cmd === "resolve_ask")).toBeLessThan(
      calls.findIndex((c) => c.cmd === "set_session_prefs"),
    );
  });

  it("档位 full_access + 选 auto_edit：同步为 auto_edit（降档方向也要覆盖）", async () => {
    seedAsk(twoModeAsk("h2"), "full_access");
    render(<AskPanel />);
    fireEvent.click(screen.getByText("以自动编辑档执行"));
    await waitFor(() => expect(calls.some((c) => c.cmd === "resolve_ask")).toBe(true));
    expect(calls.find((c) => c.cmd === "resolve_ask")?.args?.value.answers.q1.selections).toEqual(["mode_auto"]);
    await waitFor(() => expect(calls.some((c) => c.cmd === "set_session_prefs")).toBe(true));
    expect(calls.find((c) => c.cmd === "set_session_prefs")?.args?.prefs?.approval_mode).toBe("auto_edit");
  });

  it("档位 full_access + 非批准类选项（无 mode）：不直提也不切档（不误发 prefs）", async () => {
    seedAsk({
      askId: "h3", kind: "ask", approval: true, approveId: null,
      questions: [{
        id: "q1", question: "【方案】",
        options: [{ id: "mode_full", label: "以完全访问档执行", mode: "full_access" }, { id: "revise", label: "补充意见" }],
      }],
    }, "full_access");
    render(<AskPanel />);
    fireEvent.click(screen.getByText("补充意见"));
    expect(calls.some((c) => c.cmd === "resolve_ask")).toBe(false);
    fireEvent.click(btnByText("提交回答"));
    await waitFor(() => expect(calls.some((c) => c.cmd === "resolve_ask")).toBe(true));
    expect(calls.find((c) => c.cmd === "resolve_ask")?.args?.value.answers.q1.selections).toEqual(["revise"]);
    expect(calls.some((c) => c.cmd === "set_session_prefs")).toBe(false);
  });

  it("两题 ask：第一题已选甲、第二题点带 mode 选项直提 → 载荷同时包含两题答案（直提不丢其他题的已选）", async () => {
    seedAsk({
      askId: "n1", kind: "ask",
      questions: [
        { id: "q1", question: "第一步", options: [{ id: "a", label: "甲" }, { id: "b", label: "乙" }] },
        { id: "q2", question: "第二步", options: [{ id: "mode_auto", label: "以自动编辑档执行", mode: "auto_edit" }] },
      ],
    }, "auto_edit");
    render(<AskPanel />);
    fireEvent.click(screen.getByText(/^甲/)); // 答第一题（不直提）
    fireEvent.click(btnByText("下一题")); // 翻到第二题
    fireEvent.click(screen.getByText("以自动编辑档执行")); // 直提
    await waitFor(() => expect(calls.some((c) => c.cmd === "resolve_ask")).toBe(true));
    const payload = calls.find((c) => c.cmd === "resolve_ask")?.args?.value;
    // 修理前：只带 { q2: ["mode_auto"] }，q1 的「甲」被丢弃
    expect(payload.answers.q1.selections).toEqual(["a"]);
    expect(payload.answers.q2.selections).toEqual(["mode_auto"]);
    expect(calls.filter((c) => c.cmd === "resolve_ask")).toHaveLength(1);
  });
});
