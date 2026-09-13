// Tool card header summary ([docs/tool-card-multi-file-summary](../../../docs/tool-card-multi-file-summary.md)): read/batch_read/edit batch args list all file names (basename, no path)
// outcome takes priority (still lists when oversized argsPreview was replaced by the truncation marker); falls back to args while running; single-path tools keep the full path
import { describe, it, expect, vi, afterEach } from "vitest";
import { render, screen, cleanup } from "@testing-library/react";
import "../i18n"; // i18n init (nothing triggers it when rendering components directly; t() would otherwise return the key itself)
import ToolCallCard from "../features/tools/ToolCallCard";
import type { ToolView } from "../stores/run";

vi.mock("@tauri-apps/api/core", () => ({
  invoke: vi.fn(async () => null),
  Channel: class {
    onmessage: ((f: any) => void) | null = null;
  },
}));

function toolView(partial: Partial<ToolView>): ToolView {
  return {
    callKey: "b1:0",
    tool: "read",
    status: "ok",
    progressTail: "",
    ...partial,
  };
}

function summaryText(): string {
  const el = document.querySelector<HTMLElement>(".tool-card .summary");
  if (!el) throw new Error("summary span not found");
  return el.textContent ?? "";
}

function verbText(): string {
  const el = document.querySelector<HTMLElement>(".tool-card .verb");
  if (!el) throw new Error("verb span not found");
  return el.textContent ?? "";
}

afterEach(() => {
  cleanup();
});

describe("ToolCallCard summary", () => {
  it("read 多文件：列出全部 basename，不含目录路径", () => {
    render(<ToolCallCard
      tool={toolView({
        argsPreview: JSON.stringify({ files: [{ path: "src-tauri/src/infrastructure/git/credentials.rs" }, { path: "src-tauri/src/infrastructure/git/remote.rs" }] }),
        outcome: { ok: true, data: { files: [
          { path: "src-tauri\\src\\infrastructure\\git\\credentials.rs", content: "a" },
          { path: "src-tauri\\src\\infrastructure\\git\\remote.rs", content: "b" },
        ] } },
      })}
    />);
    expect(summaryText()).toBe("credentials.rs, remote.rs");
    expect(summaryText()).not.toContain("infrastructure");
  });

  it("read 单文件：仅显示 basename", () => {
    render(<ToolCallCard
      tool={toolView({
        argsPreview: JSON.stringify({ files: [{ path: "src/main.rs" }] }),
        outcome: { ok: true, data: { files: [{ path: "src\\main.rs", content: "x" }] } },
      })}
    />);
    expect(summaryText()).toBe("main.rs");
  });

  it("read 运行中（无 outcome）：回退 argsPreview 列文件名", () => {
    render(<ToolCallCard
      tool={toolView({
        status: "running",
        outcome: undefined,
        argsPreview: JSON.stringify({ files: [{ path: "lib/util.ts" }, { path: "lib/log.ts" }] }),
      })}
    />);
    expect(summaryText()).toBe("util.ts, log.ts");
  });

  it("edit 多文件：优先 outcome.edited 列出全部 basename", () => {
    render(<ToolCallCard
      tool={toolView({
        tool: "edit",
        argsPreview: JSON.stringify({ files: [{ path: "docs/a.md", changes: [] }, { path: "docs/b.md", changes: [] }] }),
        outcome: { ok: true, data: { edited: ["docs/a.md", "docs/b.md", "docs/c.md"], count: 3 } },
      })}
    />);
    expect(summaryText()).toBe("a.md, b.md, c.md");
  });

  it("edit 运行中（无 outcome）：回退 argsPreview 列文件名", () => {
    render(<ToolCallCard
      tool={toolView({
        tool: "edit",
        status: "running",
        outcome: undefined,
        argsPreview: JSON.stringify({ files: [{ path: "x/y.md", changes: [] }] }),
      })}
    />);
    expect(summaryText()).toBe("y.md");
  });

  it("read 入参被截断：完成后仍从 outcome 列出文件名", () => {
    render(<ToolCallCard
      tool={toolView({
        argsPreview: JSON.stringify({ _args_truncated: true, hint: "参数过大，前端不展示 diff" }),
        outcome: { ok: true, data: { files: [
          { path: "big/one.rs", content: "x" },
          { path: "big/two.rs", content: "y" },
        ] } },
      })}
    />);
    expect(summaryText()).toBe("one.rs, two.rs");
  });

  it("create 单路径：保持显示完整路径", () => {
    render(<ToolCallCard
      tool={toolView({
        tool: "create",
        argsPreview: JSON.stringify({ path: "src/new_file.rs", content: "fn main() {}" }),
        outcome: { ok: true, data: {} },
      })}
    />);
    expect(summaryText()).toBe("src/new_file.rs");
  });

  it("summary 悬停 title 与文本一致", () => {
    render(<ToolCallCard
      tool={toolView({
        argsPreview: JSON.stringify({ files: [{ path: "a/b.rs" }] }),
        outcome: { ok: true, data: { files: [{ path: "a/b.rs", content: "" }] } },
      })}
    />);
    const el = document.querySelector<HTMLElement>(".tool-card .summary");
    expect(el?.getAttribute("title")).toBe("b.rs");
    expect(screen.queryByText("b.rs")).not.toBeNull();
  });
});

describe("ToolCallCard ask 未作答中性渲染", () => {
  // ask 忽略/取消在后端是刻意 err（防模型误读为默许），但用户视角只是「没回答」——
  // 展示层按错误码降为中性灰（st-neutral），其余错误仍红色失败。
  it("忽略（E_ASK_NOT_ANSWERED）：显示「未回答」且卡片为中性灰，不出现「失败」", () => {
    render(<ToolCallCard
      tool={toolView({
        tool: "ask",
        status: "error",
        outcome: { ok: false, data: {}, error: { code: "E_ASK_NOT_ANSWERED", message: "用户忽略了本次询问（未作答）" } },
      })}
    />);
    expect(verbText()).toContain("未回答");
    expect(verbText()).not.toContain("失败");
    expect(document.querySelector(".tool-card.st-neutral")).not.toBeNull();
    expect(document.querySelector(".tool-card.st-error")).toBeNull();
  });

  it("取消（E_ASK_CANCELLED）：显示「已取消」且同为中性灰", () => {
    render(<ToolCallCard
      tool={toolView({
        tool: "ask",
        status: "error",
        outcome: { ok: false, data: {}, error: { code: "E_ASK_CANCELLED", message: "用户取消或未回答" } },
      })}
    />);
    expect(verbText()).toContain("已取消");
    expect(verbText()).not.toContain("失败");
    expect(document.querySelector(".tool-card.st-neutral")).not.toBeNull();
  });

  it("其他错误码（如 E_ARGS）：仍按失败红点渲染", () => {
    render(<ToolCallCard
      tool={toolView({
        tool: "ask",
        status: "error",
        outcome: { ok: false, data: {}, error: { code: "E_ARGS", message: "参数解析失败" } },
      })}
    />);
    expect(verbText()).toContain("失败");
    expect(document.querySelector(".tool-card.st-error")).not.toBeNull();
    expect(document.querySelector(".tool-card.st-neutral")).toBeNull();
  });
});

describe("ToolCallCard 运行中占位卡（running-name fix）", () => {
  // 占位哨兵 "?"（无名 tool_progress 帧的兕底）：头部绝不渲染问号——
  // verb 只留状态词，名字段不渲染（修复「正在运行 ？？」双问号）。
  it("运行中占位：verb 仅「正在运行」，不出现问号与名字段", () => {
    render(<ToolCallCard tool={toolView({ tool: "?", status: "running", progressTail: "cargo test 输出尾流" })} />);
    expect(verbText()).toContain("正在运行");
    expect(verbText()).not.toContain("?");
    expect(document.querySelector(".tool-card .tool-name")).toBeNull();
    expect(document.querySelector(".tool-card .tool-head")?.textContent).not.toContain("?");
  });

  it("占位 + 失败状态：显示「失败」而非问号", () => {
    render(<ToolCallCard tool={toolView({ tool: "?", status: "error" })} />);
    expect(verbText()).toContain("失败");
    expect(document.querySelector(".tool-card .tool-head")?.textContent).not.toContain("?");
  });
});
