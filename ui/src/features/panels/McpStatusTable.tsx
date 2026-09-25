import { useEffect, useRef, useState } from "react";
import { Button, Card, Empty, Spin, Tag, Tooltip, Typography } from "antd";
import { PlusOutlined, QuestionCircleOutlined, ReloadOutlined } from "@ant-design/icons";
import { useTranslation } from "react-i18next";
import type { McpScope, McpStatusPayload } from "../../ipc/types";
import { SettingsSection } from "./settings/SettingsTheme";

export type McpStatusKind = "ready" | "starting" | "error" | "config_error" | "evicted" | "disconnected";
type McpToolSummary = NonNullable<McpStatusPayload["tool_details"]>[number];

export interface McpStatusRow {
  name: string;
  kind: McpStatusKind;
  tools: number;
  toolDetails: McpToolSummary[];
  toolsFiltered: number;
  pid: number | null;
  error?: string;
  hint?: string | null;
  note?: string | null;
  source?: McpScope;
  overridden?: McpScope | null;
  /** 本会话生效的作用域是否与卡片作用域一致；非生效条目只可编辑配置。 */
  actionable?: boolean;
}

type StatusLike = Pick<McpStatusPayload, "state" | "tools" | "tool_details" | "tools_filtered" | "pid" | "error" | "note">;

/** Unknown states are displayed as disconnected, never as an invented failure. */
export function mcpStatusRow(name: string, st?: StatusLike, meta?: { source?: McpScope; overridden?: McpScope | null; actionable?: boolean }): McpStatusRow {
  const base = {
    name,
    tools: 0,
    toolDetails: [] as McpToolSummary[],
    toolsFiltered: st?.tools_filtered ?? 0,
    pid: st?.state === "ready" || st?.state === "starting" ? (st?.pid ?? null) : null,
    note: st?.note ?? null,
    source: meta?.source,
    overridden: meta?.overridden ?? null,
    actionable: meta?.actionable,
  };
  const s = st?.state;
  if (s === "ready") return { ...base, kind: "ready", tools: st?.tools ?? 0, toolDetails: st?.tool_details ?? [] };
  if (s === "starting") return { ...base, kind: "starting" };
  if (s === "evicted") return { ...base, kind: "evicted" };
  if (s && typeof s === "object" && "error" in s) {
    return { ...base, kind: st?.error?.kind === "config" ? "config_error" : "error", error: st?.error?.message ?? String(s.error), hint: st?.error?.hint ?? null };
  }
  return { ...base, kind: "disconnected" };
}

interface Props {
  rows: McpStatusRow[];
  refreshing: boolean;
  pendingAction?: (name: string) => "disconnect" | "reconnect" | undefined;
  onRefresh: () => void;
  onCreate?: () => void;
  rawConfig?: boolean;
  onEdit?: (name: string) => void;
  editableNames?: ReadonlySet<string>;
  hasSession: boolean;
  onDisconnect?: (name: string) => void;
  onReconnect?: (name: string) => void;
}

/** Show a question-mark tooltip only when the inline description is actually clipped. */
function ToolDescription({ name, description }: McpToolSummary) {
  const { t } = useTranslation();
  const ref = useRef<HTMLSpanElement>(null);
  const [clipped, setClipped] = useState(false);
  useEffect(() => {
    const element = ref.current;
    if (!element) return;
    const measure = () => setClipped(element.scrollWidth > element.clientWidth + 1);
    measure();
    if (typeof ResizeObserver === "undefined") return;
    const observer = new ResizeObserver(measure);
    observer.observe(element);
    return () => observer.disconnect();
  }, [description]);
  if (!description) return <span className="mcp-tool-description mcp-tool-description-empty">{t("settings.mcpToolNoDescription")}</span>;
  return <div className="mcp-tool-description-line">
    <span ref={ref} className="mcp-tool-description">{description}</span>
    {clipped && <Tooltip title={description} trigger={["hover", "focus"]}><Button type="text" size="small" className="mcp-tool-help" icon={<QuestionCircleOutlined />} aria-label={t("settings.mcpToolFullDescription", { name })} /></Tooltip>}
  </div>;
}

export default function McpStatusTable({ rows, refreshing, pendingAction, onRefresh, onCreate, rawConfig, onEdit, editableNames, hasSession, onDisconnect, onReconnect }: Props) {
  const { t } = useTranslation();
  const [expandedName, setExpandedName] = useState<string | null>(null);
  const stateText = (kind: McpStatusKind) => {
    if (kind === "ready") return t("settings.mcpStateReady");
    if (kind === "starting") return t("settings.mcpStateStarting");
    if (kind === "error") return t("settings.mcpStateError");
    if (kind === "config_error") return t("settings.mcpStateConfigError");
    if (kind === "evicted") return t("settings.mcpStateEvicted");
    return t("settings.mcpStateDisconnected");
  };
  const sourceText = (row: McpStatusRow) => row.source === "project"
    ? row.overridden ? t("settings.mcpSourceProjectOverrides") : t("settings.mcpSourceProject")
    : row.source === "global" ? t("settings.mcpSourceGlobal") : null;

  return <div className="setting-anchor" data-setting-id="app.mcp_status">
    <SettingsSection title={t("settings.mcpStatus")} className="settings-mcp-section" extra={<div className="mcp-list-actions">
      <Button icon={<PlusOutlined />} onClick={onCreate}>{rawConfig ? t("settings.mcpEditRaw") : t("settings.mcpNew")}</Button>
      <Tooltip title={t("settings.mcpStatusRefresh")}><Button type="text" icon={<ReloadOutlined />} loading={refreshing} aria-label={t("settings.mcpStatusRefresh")} onClick={onRefresh} /></Tooltip>
    </div>}>
      {rows.length === 0 ? <Empty image={Empty.PRESENTED_IMAGE_SIMPLE} description={t("settings.mcpNoServers")} /> : <div className="mcp-server-cards">
        {rows.map((row) => {
          const failed = row.kind === "error" || row.kind === "config_error";
          const expanded = failed && expandedName === row.name;
          const source = sourceText(row);
          const actionPending = pendingAction?.(row.name);
          return <Card key={row.name} size="small" variant="outlined" className="mcp-server-card" styles={{ header: { paddingBlock: 12 }, body: { padding: 16 } }}
            title={<div className="mcp-server-title">
              <span className="mcp-server-name" title={row.name}>{row.name}</span>
              <div className="mcp-server-meta">{source && <span className={row.overridden ? "mcp-source-override" : ""}>{source}</span>}
                {row.kind === "ready" && <span>{row.toolsFiltered > 0 ? t("settings.mcpToolsFiltered", { n: row.tools, m: row.toolsFiltered }) : t("settings.mcpToolCount", { count: row.tools })}</span>}
                {row.pid != null && <span>PID {row.pid}</span>}{row.actionable === false && <span>{t("settings.mcpInactiveScope")}</span>}</div>
              <span className="mcp-server-state">{row.kind === "starting" && <Spin size="small" />}
                <Tag color={row.kind === "ready" ? "success" : failed ? "error" : row.kind === "evicted" || row.kind === "starting" ? "warning" : "default"}>{stateText(row.kind)}</Tag></span>
            </div>}
            actions={[
              ...(onEdit && editableNames?.has(row.name) ? [<Button key="edit" type="text" onClick={() => onEdit(row.name)}>{t("settings.mcpEdit")}</Button>] : []),
              <Button key="disconnect" type="text" loading={actionPending === "disconnect"} disabled={!!actionPending || !hasSession || row.actionable === false || row.kind === "disconnected"} onClick={() => onDisconnect?.(row.name)}>{t("settings.mcpActionDisconnect")}</Button>,
              <Button key="reconnect" type="text" loading={actionPending === "reconnect"} disabled={!!actionPending || !hasSession || row.actionable === false || row.kind === "ready" || row.kind === "starting"} onClick={() => onReconnect?.(row.name)}>{t("settings.mcpActionReconnect")}</Button>,
            ]}>
            <div className="mcp-server-tools"><Typography.Text strong>{t("settings.mcpColTools")}</Typography.Text>
              {row.kind === "ready" && row.toolDetails.length > 0 ? <ul className="mcp-tool-list">{row.toolDetails.map((tool) => <li key={tool.name} className="mcp-tool-item"><span className="mcp-tool-name" title={tool.name}>{tool.name}</span><ToolDescription {...tool} /></li>)}</ul>
                : <Typography.Text type="secondary" className="mcp-tool-placeholder">{row.kind !== "ready" ? t("settings.mcpToolsNotReady") : row.tools === 0 ? t("settings.mcpToolsEmpty") : t("settings.mcpToolsUnavailable")}</Typography.Text>}
            </div>
            {failed && <div className="mcp-server-diagnostic"><Button type="link" size="small" aria-expanded={expanded} onClick={() => setExpandedName(expanded ? null : row.name)}>{t("settings.mcpStatusToggleError")}</Button>
              {expanded && <div className="mcp-status-error">{row.error}{row.hint ? ` — ${row.hint}` : ""}</div>}</div>}
            {row.kind === "evicted" && row.note && <div className="mcp-status-error">{row.note}</div>}
          </Card>;
        })}
      </div>}
    </SettingsSection>
  </div>;
}
