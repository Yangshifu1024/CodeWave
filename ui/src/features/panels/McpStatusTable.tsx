// MCP 服务器状态表（自 SettingsPage 抽出，code-reviewer 建议）：纯展示 + 回调，交互状态收在组件内。
// 抽出目的是可单测——四态、空表不渲染、失败行的展开（含键盘）、刷新按钮都不再需要挂整页设置页
// （组件级用例见 __tests__/mcp.status-table.test.tsx；整页集成覆盖见 __tests__/settings.mcp.test.tsx）。
//
// 数据口径由调用方拼好：名单 = 配置的服务器 ∪ 状态记录（理由见 SettingsPage 的 mcpStatusRows）。
// 下方的说明文案（settings.mcpStatusHint）承担「未连接是正常态」的解释职责，不是装饰——别删。
import { useState } from "react";
import type { HTMLAttributes, KeyboardEvent as ReactKeyboardEvent } from "react";
import { Button, Spin, Tooltip, Typography } from "antd";
import { ReloadOutlined } from "@ant-design/icons";
import { useTranslation } from "react-i18next";

/** MCP 状态行的展示形态（`ready` / `starting` / 失败 / 未连接四态） */
export type McpStatusKind = "ready" | "starting" | "error" | "disconnected";

export interface McpStatusRow {
  name: string;
  kind: McpStatusKind;
  tools: number;
  error?: string;
}

/**
 * 由 `mcp_status` 的一条记录（或缺失）得到展示行：
 *  · state 是字符串 `"ready"` / `"starting"`；
 *  · 失败态是外部标签枚举（serde 默认形态）`{ error: "..." }`，错误串原样保留给展开区；
 *  · 记录缺失 = 当前没有会话驱动连接：「未连接」是正常态，不是故障。
 */
export function mcpStatusRow(name: string, st?: { state: unknown; tools: number }): McpStatusRow {
  if (st?.state === "ready") return { name, kind: "ready", tools: st.tools ?? 0 };
  if (st?.state === "starting") return { name, kind: "starting", tools: 0 };
  const err = st?.state && typeof st.state === "object" && "error" in st.state
    ? String((st.state as { error: unknown }).error)
    : undefined;
  if (err !== undefined) return { name, kind: "error", tools: 0, error: err };
  return { name, kind: "disconnected", tools: 0 };
}

interface Props {
  rows: McpStatusRow[];
  /** 刷新中：按钮转圈 + 防重复点击（只重读状态，不会重连） */
  refreshing: boolean;
  onRefresh: () => void;
}

/**
 * 服务器状态段：小标题（含刷新）+ 三列表格 + 全局语义说明。
 * 两侧皆空（没配服务器、也没状态记录）时整段返回 null——空表会把「没配」与「没连」显示成同一个样子。
 */
export default function McpStatusTable({ rows, refreshing, onRefresh }: Props) {
  const { t } = useTranslation();
  /** 当前展开错误详情的服务器名（同时只展开一行；只有连接失败的行可点） */
  const [expandedName, setExpandedName] = useState<string | null>(null);

  /** 四态文案（字面键，不拼键名：契约测试的纯动态键扫描看不见拼出来的键名） */
  function stateText(kind: McpStatusKind): string {
    if (kind === "ready") return t("settings.mcpStateReady");
    if (kind === "starting") return t("settings.mcpStateStarting");
    if (kind === "error") return t("settings.mcpStateError");
    return t("settings.mcpStateDisconnected");
  }

  if (rows.length === 0) return null;

  return (
    <div className="setting-anchor" data-setting-id="app.mcp_status">
      <div className="settings-subhead">
        <span>{t("settings.mcpStatus")}</span>
        <Tooltip title={t("settings.mcpStatusRefresh")}>
          <Button
            type="text"
            size="small"
            icon={<ReloadOutlined />}
            loading={refreshing}
            aria-label={t("settings.mcpStatusRefresh")}
            onClick={onRefresh}
          />
        </Tooltip>
      </div>
      <div className="mcp-status">
        <div className="mcp-status-row mcp-status-row-head">
          <span className="mcp-status-name">{t("settings.mcpName")}</span>
          <span className="mcp-status-state">{t("settings.mcpColState")}</span>
          <span className="mcp-status-tools">{t("settings.mcpColTools")}</span>
        </div>
        {rows.map((row) => {
          const failed = row.kind === "error";
          const expanded = failed && expandedName === row.name;
          const toggle = () => setExpandedName(expanded ? null : row.name);
          // 失败行整行可点（展开完整错误，含 stderr）；其余三态是纯展示
          const interactive = failed
            ? ({
                role: "button",
                tabIndex: 0,
                "aria-expanded": expanded,
                "aria-label": t("settings.mcpStatusToggleError"),
                onClick: toggle,
                onKeyDown: (e: ReactKeyboardEvent<HTMLDivElement>) => {
                  if (e.key === "Enter" || e.key === " ") {
                    e.preventDefault();
                    toggle();
                  }
                },
              } as HTMLAttributes<HTMLDivElement>)
            : {};
          return (
            <div key={row.name}>
              <div className={`mcp-status-row${failed ? " mcp-status-row-clickable" : ""}`} {...interactive}>
                <span className="mcp-status-name" title={row.name}>{row.name}</span>
                <span className="mcp-status-state">
                  {row.kind === "starting" && <Spin size="small" />}
                  {failed ? (
                    <Typography.Text type="danger">{stateText(row.kind)}</Typography.Text>
                  ) : (
                    <span className={row.kind === "disconnected" ? "dim" : undefined}>
                      {stateText(row.kind)}
                    </span>
                  )}
                </span>
                <span className="mcp-status-tools">{row.kind === "ready" ? row.tools : "—"}</span>
              </div>
              {expanded && <div className="mcp-status-error">{row.error}</div>}
            </div>
          );
        })}
      </div>
      <div className="hint">{t("settings.mcpStatusHint")}</div>
    </div>
  );
}
