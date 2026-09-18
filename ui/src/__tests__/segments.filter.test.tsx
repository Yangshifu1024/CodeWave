// 共享渲染层的两个缺陷的回归测试：
//  ① 主代理调用 subagent 时不再多渲染一张通用工具卡（渲染层过滤）
//  ② 子代理过程流的 `<report>` 协议标记在渲染时剥离（且默认不剥）
// 相关实现见 ../features/chat/segments.tsx。
import { describe, it, expect, afterEach, beforeEach } from "vitest";
import { render, cleanup, fireEvent } from "@testing-library/react";
import { App as AntApp } from "antd";
import "../i18n"; // 直接挂载的组件必须自行初始化 i18next

import { stripReportMarkers, TimelineSegsView } from "../features/chat/segments";
import { useRun } from "../stores/run";
import { useSessions } from "../stores/sessions";
import type { SubView, TimelineSeg, ToolView } from "../stores/run";

function tool(over: Partial<ToolView> = {}): ToolView {
  return { callKey: "b1:0", tool: "grep", status: "ok", progressTail: "", ...over };
}

function renderSegs(timeline: TimelineSeg[], toolsMap: Record<string, ToolView>, stripReport?: boolean) {
  return render(
    <AntApp>
      <TimelineSegsView timeline={timeline} toolsMap={toolsMap} stripReport={stripReport} />
    </AntApp>,
  );
}

/** 种子：sub 段卡片依赖 store 里的 SubView（SubagentItemCard 找不到就返回 null） */
function seedSub() {
  const sub: SubView = {
    subId: "sub_1", role: "explore", name: "explore", description: "探索代码",
    task: "调研", step: 1, maxSteps: 25, tokens: 10, lastTools: [], status: "running",
  };
  useRun.setState((s) => {
    s.tabs["s1"] = {
      items: [], running: true, streamGen: 0, ask: null, breakdown: null, todos: [], suggestions: [],
      subs: [sub], subStreams: {}, subDrawer: { open: false, subId: null },
      gitEntries: null, writeTick: 0, queue: [], pendingItemId: null, draftFromQueue: null,
      lastDoneRunId: null, compacting: false,
    } as any;
  });
  useSessions.setState({ activeKey: "s1" });
}

beforeEach(() => {
  useRun.setState({ tabs: {}, drafts: {} });
});
afterEach(() => {
  cleanup();
  useRun.setState({ tabs: {}, drafts: {} });
});

describe("stripReportMarkers（缺陷 2：报告标记剥离）", () => {
  it("首尾成对的标记全部去掉，只剩正文", () => {
    expect(stripReportMarkers("<report>正文</report>")).toBe("正文");
  });

  it("标记夹在正文中间时只去标记、不丢任何正文字符", () => {
    expect(stripReportMarkers("前言<report>结论</report>")).toBe("前言结论");
  });

  it("无标记时原样返回", () => {
    expect(stripReportMarkers("普通正文")).toBe("普通正文");
  });

  it("只有闭合标记时也去掉", () => {
    expect(stripReportMarkers("结论</report>")).toBe("结论");
  });

  it("落盘数据的真实形态：标记独占一行、正文多行", () => {
    const raw = "<report>\n\n## a) 交付文件清单\n\n无 commit / push / branch 操作。\n\n</report>";
    const out = stripReportMarkers(raw);
    expect(out).not.toContain("<report>");
    expect(out).not.toContain("</report>");
    expect(out.startsWith("## a) 交付文件清单")).toBe(true);
    expect(out.endsWith("无 commit / push / branch 操作。")).toBe(true);
  });
});

describe("TimelineSegsView · subagent 调用不再渲染通用工具卡（缺陷 1）", () => {
  it('tool === "subagent" 的 tool 段不渲染 .tool-card', () => {
    renderSegs([{ kind: "tool", callKey: "b1:0" }], { "b1:0": tool({ tool: "subagent" }) });
    expect(document.querySelector(".tool-card")).toBeNull();
  });

  it("同一 assistant 项里 subagent 工具段与 sub 段并存时只出现子代理卡", () => {
    seedSub();
    renderSegs(
      [{ kind: "tool", callKey: "b1:0" }, { kind: "sub", subId: "sub_1" }],
      { "b1:0": tool({ tool: "subagent" }) },
    );
    expect(document.querySelector(".tool-card")).toBeNull();
    expect(document.querySelector(".sub-card")).not.toBeNull();
  });

  it('其他工具（grep）照常渲染 .tool-card（防误伤）', () => {
    renderSegs([{ kind: "tool", callKey: "b1:0" }], { "b1:0": tool({ tool: "grep" }) });
    expect(document.querySelector(".tool-card")).not.toBeNull();
  });

  it('占位名 "?"（真名未回填）照常渲染，不误伤运行中的工具', () => {
    renderSegs([{ kind: "tool", callKey: "b1:0" }], { "b1:0": tool({ tool: "?" }) });
    expect(document.querySelector(".tool-card")).not.toBeNull();
  });

  it("失败的 subagent 调用照常渲染错误卡（E_ARGS / E_SUBAGENT_BUSY 无 sub 段，过滤会让它零痕迹）", () => {
    // 这两类错误在后端 sub:spawn 之前就返回了（参数校验 / 并发抢槽），timeline 里没有 sub 段——
    // 若把它们一并过滤，聊天里就只剩空白，连错误码都看不到。
    renderSegs([{ kind: "tool", callKey: "b1:0" }], {
      "b1:0": tool({
        tool: "subagent",
        status: "error",
        outcome: { ok: false, error: { code: "E_SUBAGENT_BUSY", message: "并发已达上限" } },
      }),
    });
    const card = document.querySelector(".tool-card");
    expect(card).not.toBeNull();
    // 错误行在展开体内（ToolCallCard 的既有结构）：展开后应能看到错误码——这正是「不得零痕迹」的实质
    fireEvent.click(card!.querySelector(".tool-head") as HTMLElement);
    expect(document.querySelector(".tool-card")!.textContent).toContain("E_SUBAGENT_BUSY");
  });
});

describe("TimelineSegsView · stripReport 开关（缺陷 2）", () => {
  it("stripReport=true：text 段渲染结果不含 <report>，正文保留", () => {
    renderSegs([{ kind: "text", text: "<report>汇报正文</report>" }], {}, true);
    const md = document.querySelector(".md") as HTMLElement;
    expect(md).not.toBeNull();
    expect(md.innerHTML).not.toContain("<report>");
    expect(md.innerHTML).not.toContain("&lt;report");
    expect(md.textContent).toContain("汇报正文");
  });

  it("stripReport 缺省（false）：同一输入保留标记（默认不剥，守住主聊天内容）", () => {
    renderSegs([{ kind: "text", text: "<report>汇报正文</report>" }], {});
    const md = document.querySelector(".md") as HTMLElement;
    expect(md.innerHTML).toContain("&lt;report&gt;"); // markdown 把未知标签转义成字面文本
  });

  it("stripReport=true 不影响非标记文本段", () => {
    renderSegs([{ kind: "text", text: "普通过程输出" }], {}, true);
    expect((document.querySelector(".md") as HTMLElement).textContent).toContain("普通过程输出");
  });
});
