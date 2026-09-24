// MCP 服务器状态表（组件级）：六态映射、来源列、操作列与展开交互。
//
// 映射口径的取向：**未知或缺失状态一律落到「未连接」**，不当作故障——
// 后端将来加新态（如 "stopping"）时不该在界面上被误报成错误。
import { afterEach, describe, expect, it, vi } from "vitest";
import { cleanup, fireEvent, render } from "@testing-library/react";
import "../i18n";
import McpStatusTable, {
  mcpStatusRow,
  type McpStatusRow,
} from "../features/panels/McpStatusTable";
import type { McpStatusPayload } from "../ipc/types";

// 组件测试之间必须清 DOM：否则上一用例的行会留在 document 里，
// 让「逐行渲染」这类按序断言的用例读到累积结果。
afterEach(() => cleanup());

type StatusLike = Pick<
  McpStatusPayload,
  "state" | "tools" | "tools_filtered" | "pid" | "error" | "note"
>;

/** 造一条状态记录（缺省为已连接）。 */
function st(patch: Partial<StatusLike> = {}): StatusLike {
  return {
    state: "ready",
    tools: 0,
    tools_filtered: 0,
    pid: null,
    error: null,
    note: null,
    ...patch,
  } as StatusLike;
}

function renderTable(props: Partial<Parameters<typeof McpStatusTable>[0]> = {}) {
  return render(
    <McpStatusTable
      rows={[]}
      refreshing={false}
      onRefresh={() => {}}
      hasSession
      {...props}
    />,
  );
}

function rows(): HTMLElement[] {
  return Array.from(document.querySelectorAll<HTMLElement>(".mcp-status-row")).filter(
    (r) => !r.classList.contains("mcp-status-row-head"),
  );
}

describe("mcpStatusRow 状态映射", () => {
  it("ready / starting 直映，工具数只在 ready 时给出", () => {
    expect(mcpStatusRow("a", st({ state: "ready", tools: 3 }))).toMatchObject({
      kind: "ready",
      tools: 3,
    });
    expect(mcpStatusRow("a", st({ state: "starting", tools: 3 }))).toMatchObject({
      kind: "starting",
      tools: 0,
    });
  });

  it("stopped → 未连接（主动断开是正常态，不是故障）", () => {
    expect(mcpStatusRow("a", st({ state: "stopped" })).kind).toBe("disconnected");
  });

  it("evicted → 已淘汰态，并带上可见性提示", () => {
    const r = mcpStatusRow("a", st({ state: "evicted", note: "已被淘汰（资源上限）" }));
    expect(r.kind).toBe("evicted");
    expect(r.note).toBe("已被淘汰（资源上限）");
  });

  it("失败态：error.kind = config → 配置错；其余 → 失败；都带原因与建议", () => {
    const cfg = mcpStatusRow(
      "a",
      st({
        state: { error: "不支持旧式 SSE" },
        error: { kind: "config", message: "不支持旧式 SSE", hint: "改用 /mcp", server_message: null },
      }),
    );
    expect(cfg.kind).toBe("config_error");
    expect(cfg.error).toBe("不支持旧式 SSE");
    expect(cfg.hint).toBe("改用 /mcp");

    const fail = mcpStatusRow(
      "a",
      st({
        state: { error: "握手超时" },
        error: { kind: "handshake", message: "握手超时", hint: null, server_message: null },
      }),
    );
    expect(fail.kind).toBe("error");
    expect(fail.error).toBe("握手超时");
  });

  it("未知 / 缺失状态 → 未连接（不当故障）", () => {
    expect(mcpStatusRow("a").kind).toBe("disconnected");
    expect(mcpStatusRow("a", st({ state: "stopping" as never })).kind).toBe("disconnected");
  });

  it("携带工具过滤计数 / PID / 来源与被覆盖层", () => {
    const r = mcpStatusRow(
      "a",
      st({ state: "ready", tools: 6, tools_filtered: 2, pid: 24188 }),
      { source: "project", overridden: "global" },
    );
    expect(r.toolsFiltered).toBe(2);
    expect(r.pid).toBe(24188);
    expect(r.source).toBe("project");
    expect(r.overridden).toBe("global");
  });
});

describe("McpStatusTable 渲染", () => {
  it("两侧皆空时整段不渲染（空表会把「没配」与「没连」显示成同一个样子）", () => {
    const { container } = renderTable({ rows: [] });
    expect(container.querySelector('[data-setting-id="app.mcp_status"]')).toBeFalsy();
  });

  it("六列表头齐备", () => {
    renderTable({ rows: [mcpStatusRow("fs", st({ state: "ready" }))] });
    expect(rows().length).toBe(1);
    expect(document.body.textContent).toContain("来源");
    expect(document.body.textContent).toContain("PID");
    expect(document.body.textContent).toContain("操作");
  });

  it("已连接行：工具数 / PID / 来源标注", () => {
    renderTable({
      rows: [
        mcpStatusRow("fs", st({ state: "ready", tools: 6, tools_filtered: 2, pid: 24188 }), {
          source: "project",
          overridden: "global",
        }),
      ],
    });
    const row = rows()[0];
    expect(row.querySelector(".mcp-status-state")?.textContent).toContain("已连接");
    expect(row.querySelector(".mcp-status-tools")?.textContent).toBe("6（已过滤 2）");
    expect(row.querySelector(".mcp-status-pid")?.textContent).toBe("24188");
    expect(row.querySelector(".mcp-status-source")?.textContent).toBe("项目（覆盖全局）");
  });

  it("非 ready 行的工具数与 PID 显示为破折号", () => {
    renderTable({ rows: [mcpStatusRow("fs", st({ state: "starting", pid: null }))] });
    const row = rows()[0];
    expect(row.querySelector(".mcp-status-tools")?.textContent).toBe("—");
    expect(row.querySelector(".mcp-status-pid")?.textContent).toBe("—");
  });

  it("已淘汰行显示提示文案（橙色警告态）", () => {
    renderTable({
      rows: [mcpStatusRow("big", st({ state: "evicted", note: "已被淘汰（资源上限）；需要时会自动重拉" }))],
    });
    expect(document.body.textContent).toContain("已淘汰（资源上限）");
    expect(document.body.textContent).toContain("需要时会自动重拉");
  });

  it("失败行整行可点展开（含键盘），显示原因与建议", () => {
    renderTable({
      rows: [
        mcpStatusRow(
          "remote",
          st({
            state: { error: "握手超时（30s）" },
            error: { kind: "handshake", message: "握手超时（30s）", hint: "确认服务端可达", server_message: null },
          }),
        ),
      ],
    });
    const row = rows()[0];
    expect(row.classList.contains("mcp-status-row-clickable")).toBe(true);
    expect(document.querySelector(".mcp-status-error")).toBeFalsy();

    fireEvent.click(row);
    expect(document.querySelector(".mcp-status-error")?.textContent).toContain("握手超时（30s）");
    expect(document.querySelector(".mcp-status-error")?.textContent).toContain("确认服务端可达");

    fireEvent.keyDown(row, { key: "Enter" });
    expect(document.querySelector(".mcp-status-error")).toBeFalsy();
  });

  it("非失败行不可展开", () => {
    renderTable({ rows: [mcpStatusRow("fs", st({ state: "ready" }))] });
    expect(rows()[0].classList.contains("mcp-status-row-clickable")).toBe(false);
    expect(rows()[0].getAttribute("role")).toBeNull();
  });

  it("无活跃会话时断开 / 重连禁用", () => {
    renderTable({ rows: [mcpStatusRow("fs", st())], hasSession: false });
    const btns = Array.from(rows()[0].querySelectorAll("button"));
    expect(btns.length).toBe(2);
    expect(btns.every((b) => (b as HTMLButtonElement).disabled)).toBe(true);
  });

  it("有活跃会话时断开 / 重连回调带上 server 名", () => {
    const onDisconnect = vi.fn();
    const onReconnect = vi.fn();
    renderTable({ rows: [mcpStatusRow("fs", st())], onDisconnect, onReconnect });
    const btns = Array.from(rows()[0].querySelectorAll("button"));
    fireEvent.click(btns[0]);
    fireEvent.click(btns[1]);
    expect(onDisconnect).toHaveBeenCalledWith("fs");
    expect(onReconnect).toHaveBeenCalledWith("fs");
  });

  it("刷新按钮：点击触发回调，且 aria-label 说明不会重连", () => {
    const onRefresh = vi.fn();
    renderTable({ rows: [mcpStatusRow("fs", st())], onRefresh });
    const btn = document.querySelector<HTMLElement>(
      'button[aria-label="刷新状态（不会重新连接）"]',
    )!;
    expect(btn).toBeTruthy();
    fireEvent.click(btn);
    expect(onRefresh).toHaveBeenCalledTimes(1);
  });

  it("多行时逐行渲染且顺序保持", () => {
    const list: McpStatusRow[] = [
      mcpStatusRow("a", st({ state: "ready" })),
      mcpStatusRow("b", st({ state: "starting" })),
      mcpStatusRow("c", st({ state: "stopped" })),
    ];
    renderTable({ rows: list });
    expect(rows().map((r) => r.querySelector(".mcp-status-name")?.textContent)).toEqual([
      "a",
      "b",
      "c",
    ]);
  });
});
