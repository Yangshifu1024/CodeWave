import { afterEach, describe, expect, it, vi } from "vitest";
import { cleanup, fireEvent, render, waitFor } from "@testing-library/react";
import "../i18n";
import McpStatusTable, { mcpStatusRow } from "../features/panels/McpStatusTable";
import type { McpStatusPayload } from "../ipc/types";

afterEach(() => cleanup());
type StatusLike = Pick<McpStatusPayload, "state" | "tools" | "tool_details" | "tools_filtered" | "pid" | "error" | "note">;
function st(patch: Partial<StatusLike> = {}): StatusLike {
  return { state: "ready", tools: 0, tools_filtered: 0, pid: null, error: null, note: null, ...patch };
}
function renderCards(rows: ReturnType<typeof mcpStatusRow>[], props: Partial<Parameters<typeof McpStatusTable>[0]> = {}) {
  return render(<McpStatusTable rows={rows} refreshing={false} onRefresh={() => {}} hasSession {...props} />);
}
function cards() { return Array.from(document.querySelectorAll<HTMLElement>(".mcp-server-card")); }

describe("MCP status cards", () => {
  it("retains status mapping, source, filtered count and non-ready fallback", () => {
    const ready = mcpStatusRow("fs", st({ tools: 1, tools_filtered: 2, pid: 123, tool_details: [{ name: "read", description: "Read files" }] }), { source: "project", overridden: "global" });
    const stopped = mcpStatusRow("web", st({ state: "stopped", tools: 4, tool_details: [{ name: "old", description: "stale" }] }));
    expect(stopped.toolDetails).toEqual([]);
    renderCards([ready, stopped]);
    expect(cards()).toHaveLength(2);
    expect(cards()[0].textContent).toContain("项目（覆盖全局）");
    expect(cards()[0].textContent).toContain("1（已过滤 2）");
    expect(cards()[0].textContent).toContain("PID 123");
    expect(cards()[0].textContent).toContain("Read files");
    expect(cards()[1].textContent).toContain("连接后显示工具");
    expect(cards()[1].textContent).not.toContain("stale");
  });

  it("shows empty-description and old-payload fallback distinctly", () => {
    renderCards([
      mcpStatusRow("a", st({ tools: 1, tool_details: [{ name: "search", description: "" }] })),
      mcpStatusRow("b", st({ tools: 2 })),
      mcpStatusRow("c", st()),
    ]);
    expect(cards()[0].textContent).toContain("暂无说明");
    expect(cards()[1].textContent).toContain("工具详情暂不可用");
    expect(cards()[2].textContent).toContain("暂无可用工具");
  });

  it("only clipped descriptions get a keyboard-focusable question tooltip", async () => {
    const width = Object.getOwnPropertyDescriptor(HTMLElement.prototype, "clientWidth");
    const scroll = Object.getOwnPropertyDescriptor(HTMLElement.prototype, "scrollWidth");
    Object.defineProperty(HTMLElement.prototype, "clientWidth", { configurable: true, get: () => 100 });
    Object.defineProperty(HTMLElement.prototype, "scrollWidth", { configurable: true, get() { return this.textContent?.includes("Long") ? 200 : 80; } });
    try {
      renderCards([mcpStatusRow("fs", st({ tools: 2, tool_details: [{ name: "short", description: "Short" }, { name: "long", description: "Long explanation" }] }))]);
      await waitFor(() => expect(document.querySelectorAll(".mcp-tool-help")).toHaveLength(1));
      const help = document.querySelector<HTMLButtonElement>(".mcp-tool-help")!;
      expect(help.getAttribute("aria-label")).toContain("long");
      expect(help.tabIndex).not.toBe(-1);
      fireEvent.focus(help);
      await waitFor(() => expect(document.querySelector(".ant-tooltip")?.textContent).toContain("Long explanation"));
    } finally {
      if (width) Object.defineProperty(HTMLElement.prototype, "clientWidth", width);
      if (scroll) Object.defineProperty(HTMLElement.prototype, "scrollWidth", scroll);
    }
  });

  it("keeps actions and expandable error details in each card", () => {
    const onEdit = vi.fn();
    const onReconnect = vi.fn();
    const row = mcpStatusRow("fs", st({ state: { error: "timeout" }, error: { kind: "handshake", message: "timeout", hint: "check server", server_message: null } }));
    renderCards([row], { editableNames: new Set(["fs"]), onEdit, onReconnect });
    fireEvent.click(cards()[0].querySelector<HTMLButtonElement>("button[aria-expanded]")!);
    expect(cards()[0].textContent).toContain("check server");
    fireEvent.click(Array.from(cards()[0].querySelectorAll("button")).find((b) => b.textContent?.includes("编辑配置"))!);
    fireEvent.click(Array.from(cards()[0].querySelectorAll("button")).find((b) => b.textContent?.includes("重连"))!);
    expect(onEdit).toHaveBeenCalledWith("fs");
    expect(onReconnect).toHaveBeenCalledWith("fs");
  });

  it("keeps creation and status refresh available in the empty state", () => {
    const onCreate = vi.fn();
    const onRefresh = vi.fn();
    const { container } = renderCards([], { onCreate, onRefresh });
    expect(container.textContent).toContain("暂无服务器");
    fireEvent.click(Array.from(container.querySelectorAll("button")).find((b) => b.textContent?.includes("新建"))!);
    fireEvent.click(container.querySelector<HTMLButtonElement>('button[aria-label="刷新状态（不会重新连接）"]')!);
    expect(onCreate).toHaveBeenCalledOnce();
    expect(onRefresh).toHaveBeenCalledOnce();
  });
});
