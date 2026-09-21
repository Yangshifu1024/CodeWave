// McpStatusTable 组件级单测（自 SettingsPage 抽出后新增，code-reviewer 建议）：
// 与整页集成用例（settings.mcp.test.tsx：名单并集 / 空态 / 刷新接线 / 切页 refetch）分层——
// 这里只喂 props，专注四态渲染、空表整段不渲染、失败行的展开（点击 + 键盘 Enter/Space）、刷新回调，
// 以及纯函数 mcpStatusRow 的四态映射。
import { describe, it, expect, vi, afterEach } from "vitest";
import { render, fireEvent, cleanup } from "@testing-library/react";
import { App as AntApp } from "antd"; // 必须与组件同源（主入口）：es/app 子路径会产生另一个 context
import "../i18n"; // 直接挂载组件需显式初始化 i18next
import McpStatusTable, { mcpStatusRow, type McpStatusRow } from "../features/panels/McpStatusTable";

afterEach(() => cleanup());

/** 三列分别取文本：行内是并列的 span（JSX 会吃掉元素间空白），拼 textContent 拼不出分隔符 */
function bodyRows(): { name: string; state: string; tools: string }[] {
  return Array.from(document.querySelectorAll<HTMLElement>(".mcp-status-row"))
    .filter((r) => !r.classList.contains("mcp-status-row-head"))
    .map((r) => ({
      name: r.querySelector(".mcp-status-name")?.textContent ?? "",
      state: (r.querySelector(".mcp-status-state")?.textContent ?? "").trim(),
      tools: r.querySelector(".mcp-status-tools")?.textContent ?? "",
    }));
}

function renderTable(rows: McpStatusRow[], opts: { refreshing?: boolean; onRefresh?: () => void } = {}) {
  return render(
    <AntApp>
      <McpStatusTable rows={rows} refreshing={opts.refreshing ?? false} onRefresh={opts.onRefresh ?? (() => {})} />
    </AntApp>,
  );
}

const REFRESH_LABEL = "刷新状态（不会重新连接）";

describe("McpStatusTable：四态与空表", () => {
  it("四态各按自己的形态渲染：已连接带工具数 / 连接中带 spinner / 失败红字 / 未连接灰字", () => {
    renderTable([
      mcpStatusRow("a", { state: "ready", tools: 7 }),
      mcpStatusRow("b", { state: "starting", tools: 0 }),
      mcpStatusRow("c", { state: { error: "boom" }, tools: 0 }),
      mcpStatusRow("d", undefined),
    ]);

    expect(bodyRows()).toEqual([
      { name: "a", state: "已连接", tools: "7" },
      { name: "b", state: "连接中", tools: "—" },
      { name: "c", state: "连接失败", tools: "—" },
      { name: "d", state: "未连接", tools: "—" },
    ]);
    // 连接中才有 spinner
    expect(document.querySelectorAll(".mcp-status-state .ant-spin")).toHaveLength(1);
    // 表头 + 锚点 + 全局语义说明（说明承担「未连接是正常态」的解释职责）
    expect(document.querySelector(".mcp-status-row-head")?.textContent).toContain("工具数");
    expect(document.querySelector('[data-setting-id="app.mcp_status"]')).toBeTruthy();
    expect(document.querySelector(".hint")?.textContent).toContain("连接在打开会话时建立");
  });

  it("rows 为空时整段不渲染（空表会把「没配」与「没连」显示成同一个样子）", () => {
    const { container } = renderTable([]);
    expect(container.querySelector('[data-setting-id="app.mcp_status"]')).toBeNull();
    expect(container.textContent).toBe("");
  });
});

describe("McpStatusTable：失败行的展开与刷新", () => {
  const failed = () => mcpStatusRow("c", { state: { error: "spawn npx ENOENT" }, tools: 0 });

  it("只有失败行可点：带 role=button / tabIndex / aria-expanded", () => {
    renderTable([mcpStatusRow("a", { state: "ready", tools: 1 }), failed()]);

    const clickable = document.querySelectorAll<HTMLElement>(".mcp-status-row-clickable");
    expect(clickable).toHaveLength(1);
    expect(clickable[0].getAttribute("role")).toBe("button");
    expect(clickable[0].tabIndex).toBe(0);
    expect(clickable[0].getAttribute("aria-expanded")).toBe("false");
  });

  it("点击整行展开完整错误，再点收起", () => {
    renderTable([failed()]);
    const row = document.querySelector(".mcp-status-row-clickable") as HTMLElement;

    fireEvent.click(row);
    expect(document.querySelector(".mcp-status-error")?.textContent).toBe("spawn npx ENOENT");
    expect(row.getAttribute("aria-expanded")).toBe("true");

    fireEvent.click(row);
    expect(document.querySelector(".mcp-status-error")).toBeNull();
    expect(row.getAttribute("aria-expanded")).toBe("false");
  });

  it("键盘可达：Enter 与 Space 都能展开 / 收起（组件级才方便覆盖）", () => {
    renderTable([failed()]);
    const row = document.querySelector(".mcp-status-row-clickable") as HTMLElement;

    fireEvent.keyDown(row, { key: "Enter" });
    expect(document.querySelector(".mcp-status-error")).toBeTruthy();

    fireEvent.keyDown(row, { key: " " });
    expect(document.querySelector(".mcp-status-error")).toBeNull();
  });

  it("同时只展开一行：点第二行时第一行收起", () => {
    renderTable([
      mcpStatusRow("c", { state: { error: "A" }, tools: 0 }),
      mcpStatusRow("e", { state: { error: "B" }, tools: 0 }),
    ]);
    const rows = document.querySelectorAll<HTMLElement>(".mcp-status-row-clickable");

    fireEvent.click(rows[0]);
    expect(document.querySelector(".mcp-status-error")?.textContent).toBe("A");
    fireEvent.click(rows[1]);

    const blocks = document.querySelectorAll(".mcp-status-error");
    expect(blocks).toHaveLength(1);
    expect(blocks[0].textContent).toBe("B");
  });

  it("刷新按钮：aria-label 说明「不会重新连接」，点击回调一次；refreshing 时呈 loading（防重复点）", () => {
    const onRefresh = vi.fn();
    const rows = [mcpStatusRow("a", undefined)];
    const { rerender } = renderTable(rows, { onRefresh });

    const btn = document.querySelector(`button[aria-label="${REFRESH_LABEL}"]`) as HTMLButtonElement;
    expect(btn).toBeTruthy();
    fireEvent.click(btn);
    expect(onRefresh).toHaveBeenCalledTimes(1);

    rerender(
      <AntApp>
        <McpStatusTable rows={rows} refreshing onRefresh={onRefresh} />
      </AntApp>,
    );
    const loadingBtn = document.querySelector(`button[aria-label="${REFRESH_LABEL}"]`) as HTMLButtonElement;
    expect(loadingBtn.className).toContain("ant-btn-loading");
  });
});

describe("mcpStatusRow：状态记录 → 展示行的映射", () => {
  it("ready / starting / 失败枚举 / 记录缺失 四态，未知 state 值不猜（按未连接）", () => {
    expect(mcpStatusRow("a", { state: "ready", tools: 9 })).toEqual({ name: "a", kind: "ready", tools: 9 });
    expect(mcpStatusRow("a", { state: "starting", tools: 0 })).toEqual({ name: "a", kind: "starting", tools: 0 });
    expect(mcpStatusRow("a", { state: { error: "boom" }, tools: 0 })).toEqual({
      name: "a", kind: "error", tools: 0, error: "boom",
    });
    expect(mcpStatusRow("a", undefined)).toEqual({ name: "a", kind: "disconnected", tools: 0 });
    // 后端将来加新态（如 "stopping"）：不当成故障显示，落到「未连接」
    expect(mcpStatusRow("a", { state: "stopping", tools: 1 })).toEqual({ name: "a", kind: "disconnected", tools: 0 });
  });
});
