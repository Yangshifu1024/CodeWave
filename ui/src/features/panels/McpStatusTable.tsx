// MCP 服务器状态表（纯展示 + 回调，交互状态收在组件内）。
//
// 列：服务器名称 / 来源 / 状态 / 工具数 / PID / 操作。
// 数据口径由调用方拼好（名单 = 配置条目 ∪ 状态记录），见 SettingsPage 的 mcpStatusRows。
//
// 状态映射由后端载荷驱动（[docs/mcp-module-rebuild](../../../docs/mcp-module-rebuild.md) §2.3/§2.4）：
//   · `starting` / `ready` / `stopped` / `evicted` 是字符串；
//   · 失败态是外部标签枚举形态 `{ error: "..." }`（serde 默认），`error.kind === "config"` 归为「配置错」；
//   · **未知或缺失状态一律落到「未连接」**，不当作故障——后端将来加新态时不该被误报成错误。
import { useState } from "react";
import type { HTMLAttributes, KeyboardEvent as ReactKeyboardEvent } from "react";
import { Button, Empty, Spin, Tag, Tooltip } from "antd";
import { PlusOutlined, ReloadOutlined } from "@ant-design/icons";
import { useTranslation } from "react-i18next";
import type { McpScope, McpStatusPayload } from "../../ipc/types";

/** 状态行的展示形态（六态；未连接是正常态，不是故障） */
export type McpStatusKind =
  | "ready"
  | "starting"
  | "error"
  | "config_error"
  | "evicted"
  | "disconnected";

export interface McpStatusRow {
  name: string;
  kind: McpStatusKind;
  tools: number;
  /** 因工具过滤而未注入的数量（>0 时在工具数列标注） */
  toolsFiltered: number;
  pid: number | null;
  /** 失败原因（可展开） */
  error?: string;
  /** 可操作建议（可展开） */
  hint?: string | null;
  /** 可见性提示（「已被淘汰（资源上限）」等） */
  note?: string | null;
  /** 生效来源（来自配置视图；缺失时不显示来源列内容） */
  source?: McpScope;
  /** 被它覆盖的下层 */
  overridden?: McpScope | null;
}

/** 状态记录的最小形态（`mcp_status` 命令与 `mcp:status` 事件共用）。 */
type StatusLike = Pick<
  McpStatusPayload,
  "state" | "tools" | "tools_filtered" | "pid" | "error" | "note"
>;

/**
 * 由一条状态记录（或缺失）得到展示行。
 *
 * `meta` 携带配置侧信息（来源 / 被覆盖层）——状态记录本身只有连接信息，
 * 两者在调用方按 server 名拼在一起。
 */
export function mcpStatusRow(
  name: string,
  st?: StatusLike,
  meta?: { source?: McpScope; overridden?: McpScope | null },
): McpStatusRow {
  const base = {
    name,
    tools: 0,
    toolsFiltered: st?.tools_filtered ?? 0,
    pid: st?.state === "ready" || st?.state === "starting" ? (st?.pid ?? null) : null,
    note: st?.note ?? null,
    source: meta?.source,
    overridden: meta?.overridden ?? null,
  };
  const s = st?.state;
  if (s === "ready") return { ...base, kind: "ready", tools: st?.tools ?? 0 };
  if (s === "starting") return { ...base, kind: "starting" };
  if (s === "evicted") return { ...base, kind: "evicted" };
  if (s && typeof s === "object" && "error" in s) {
    const isConfig = st?.error?.kind === "config";
    return {
      ...base,
      kind: isConfig ? "config_error" : "error",
      error: st?.error?.message ?? String((s as { error: unknown }).error),
      hint: st?.error?.hint ?? null,
    };
  }
  return { ...base, kind: "disconnected" };
}

interface Props {
  rows: McpStatusRow[];
  /** 刷新中：按钮转圈 + 防重复点击（只重读状态，不会重连） */
  refreshing: boolean;
  onRefresh: () => void;
  onCreate?: () => void;
  rawConfig?: boolean;
  onEdit?: (name: string) => void;
  editableNames?: ReadonlySet<string>;
  /** 有活跃会话时断开 / 重连才可用（两者都作用于会话可见集；「测试」在配置卡片上，无需会话） */
  hasSession: boolean;
  onDisconnect?: (name: string) => void;
  onReconnect?: (name: string) => void;
}

/**
 * 服务器状态段：小标题（含刷新）+ 六列表格 + 全局语义说明。
 * 两侧皆空（没配服务器、也没状态记录）时整段返回 null——空表会把「没配」与「没连」显示成同一个样子。
 */
export default function McpStatusTable({
  rows,
  refreshing,
  onRefresh,
  onCreate,
  rawConfig,
  onEdit,
  editableNames,
  hasSession,
  onDisconnect,
  onReconnect,
}: Props) {
  const { t } = useTranslation();
  /** 当前展开详情的服务器名（同时只展开一行；只有失败行可点） */
  const [expandedName, setExpandedName] = useState<string | null>(null);

  /** 六态文案（字面键，不拼键名：契约测试的纯动态键扫描看不见拼出来的键名） */
  function stateText(kind: McpStatusKind): string {
    if (kind === "ready") return t("settings.mcpStateReady");
    if (kind === "starting") return t("settings.mcpStateStarting");
    if (kind === "error") return t("settings.mcpStateError");
    if (kind === "config_error") return t("settings.mcpStateConfigError");
    if (kind === "evicted") return t("settings.mcpStateEvicted");
    return t("settings.mcpStateDisconnected");
  }

  /** 来源文案（无配置侧信息时留空，不编造） */
  function sourceText(row: McpStatusRow): string | null {
    if (!row.source) return null;
    if (row.source === "project") {
      return row.overridden
        ? t("settings.mcpSourceProjectOverrides")
        : t("settings.mcpSourceProject");
    }
    return t("settings.mcpSourceGlobal");
  }

  return (
    <div className="setting-anchor" data-setting-id="app.mcp_status">
      <div className="settings-subhead">
        <span>{t("settings.mcpStatus")}</span>
        <div className="mcp-list-actions">
          <Button size="small" icon={<PlusOutlined />} onClick={onCreate}>{rawConfig ? t("settings.mcpEditRaw") : t("settings.mcpNew")}</Button>
          <Tooltip title={t("settings.mcpStatusRefresh")}>
            <Button type="text" size="small" icon={<ReloadOutlined />} loading={refreshing} aria-label={t("settings.mcpStatusRefresh")} onClick={onRefresh} />
          </Tooltip>
        </div>
      </div>
      {rows.length === 0 ? <Empty image={Empty.PRESENTED_IMAGE_SIMPLE} description={t("settings.mcpNoServers")} /> : <div className="mcp-status">
        <div className="mcp-status-row mcp-status-row-head">
          <span className="mcp-status-name">{t("settings.mcpName")}</span>
          <span className="mcp-status-source">{t("settings.mcpColSource")}</span>
          <span className="mcp-status-state">{t("settings.mcpColState")}</span>
          <span className="mcp-status-tools">{t("settings.mcpColTools")}</span>
          <span className="mcp-status-pid">{t("settings.mcpColPid")}</span>
          <span className="mcp-status-actions">{t("settings.mcpColActions")}</span>
        </div>
        {rows.map((row) => {
          const failed = row.kind === "error" || row.kind === "config_error";
          const expanded = failed && expandedName === row.name;
          const toggle = () => setExpandedName(expanded ? null : row.name);
          // 失败行整行可点（展开完整错误与建议）；其余态是纯展示
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
          const source = sourceText(row);
          return (
            <div key={row.name}>
              <div
                className={`mcp-status-row${failed ? " mcp-status-row-clickable" : ""}`}
                {...interactive}
              >
                <span className="mcp-status-name" title={row.name}>
                  {row.name}
                </span>
                <span className="mcp-status-source">
                  {source && (
                    <span
                      className={`mcp-source-tag${row.overridden ? " mcp-source-override" : ""}`}
                    >
                      {source}
                    </span>
                  )}
                </span>
                <span className="mcp-status-state">
                  {row.kind === "starting" && <Spin size="small" />}
                  <Tag color={row.kind === "ready" ? "success" : failed ? "error" : row.kind === "evicted" || row.kind === "starting" ? "warning" : "default"}>{stateText(row.kind)}</Tag>
                </span>
                <span className="mcp-status-tools">
                  {row.kind !== "ready"
                    ? "—"
                    : row.toolsFiltered > 0
                      ? t("settings.mcpToolsFiltered", { n: row.tools, m: row.toolsFiltered })
                      : row.tools}
                </span>
                <span className="mcp-status-pid">{row.pid ?? "—"}</span>
                <span className="mcp-status-actions">
                  {onEdit && editableNames?.has(row.name) && <Button size="small" type="text" onClick={(event) => { event.stopPropagation(); onEdit(row.name); }}>{t("settings.mcpEdit")}</Button>}
                  <Button
                    size="small"
                    type="text"
                    disabled={!hasSession || row.kind === "disconnected"}
                    onClick={(event) => { event.stopPropagation(); onDisconnect?.(row.name); }}
                  >
                    {t("settings.mcpActionDisconnect")}
                  </Button>
                  <Button
                    size="small"
                    type="text"
                    disabled={!hasSession || row.kind === "ready" || row.kind === "starting"}
                    onClick={(event) => { event.stopPropagation(); onReconnect?.(row.name); }}
                  >
                    {t("settings.mcpActionReconnect")}
                  </Button>
                </span>
              </div>
              {expanded && (
                <div className="mcp-status-error">
                  {row.error}
                  {row.hint ? ` — ${row.hint}` : ""}
                </div>
              )}
              {row.kind === "evicted" && row.note && (
                <div className="mcp-status-error">{row.note}</div>
              )}
            </div>
          );
        })}
      </div>}
    </div>
  );
}
