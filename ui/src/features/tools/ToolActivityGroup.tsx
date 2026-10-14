import { useCallback, useState } from "react";
import { Button } from "antd";
import { FileSearchOutlined, RightOutlined, DownOutlined } from "@ant-design/icons";
import { useTranslation } from "react-i18next";
import type { ToolView } from "../../stores/run.types";
import ToolCallCard from "./ToolCallCard";
import type { ToolExpansion } from "./ToolCallCard";
import { toolFilePaths, type ToolOrigin } from "./toolPresentation";

/** 只折叠展示；结果仍由 toolsMap 提供，不写摘要到历史。 */
export default function ToolActivityGroup({ tools, onToggle, onExpansionChange, origins, expanded: controlledExpanded, onGroupExpansionChange }: {
  tools: ToolView[];
  onToggle?: () => void;
  onExpansionChange: (callKey: string, expansion: ToolExpansion) => void;
  origins?: Record<string, ToolOrigin>;
  expanded?: boolean;
  onGroupExpansionChange?: (callKey: string, expanded: boolean) => void;
}) {
  const { t } = useTranslation();
  const [localExpanded, setExpanded] = useState(false);
  const expanded = controlledExpanded ?? localExpanded;
  const single = tools.length === 1;
  const members = [...new Map(tools.flatMap(tool => origins?.[tool.callKey] ? [[origins[tool.callKey].key, origins[tool.callKey]] as const] : [])).values()];
  const onToolToggle = useCallback(() => {
    setExpanded(true);
    if (!single) onGroupExpansionChange?.(tools[0].callKey, true);
    onToggle?.();
  }, [onToggle, onGroupExpansionChange, tools, single]);
  const reads = tools.filter((tool) => tool.tool === "read" || tool.tool === "batch_read");
  const paths = new Set(reads.flatMap(toolFilePaths));
  const commands = tools.filter((tool) => tool.tool === "command").length;
  const searches = tools.filter((tool) => tool.tool === "grep").length;
  const lists = tools.filter((tool) => tool.tool === "list_files").length;
  const label = [
    reads.length ? (paths.size ? t("tools.groupRead", { n: paths.size }) : t("tools.groupReadUnknown")) : "",
    searches ? t("tools.groupSearch", { n: searches }) : "",
    lists ? t("tools.groupList", { n: lists }) : "",
    commands ? t("tools.groupCommand", { n: commands }) : "",
  ].filter(Boolean).join(" · ");
  return (
    <div className="tool-activity-group">
      {!single && <Button type="text" className="tool-group-toggle" data-output-key={`tool:${tools[0].callKey}`} data-output-group={JSON.stringify(tools.map((tool) => `tool:${tool.callKey}`))} data-anchor-aliases={!expanded && members.length > 1 ? JSON.stringify(members) : undefined} data-output-message-keys={members.length > 1 ? JSON.stringify(members.map(member => member.key)) : undefined} aria-expanded={expanded} onClick={() => {
        setExpanded(!expanded);
        onGroupExpansionChange?.(tools[0].callKey, !expanded);
        onToggle?.();
      }}>
        {expanded ? <DownOutlined /> : <RightOutlined />}
        <FileSearchOutlined />
        <span className="tool-group-label" title={label}>{label}</span>
      </Button>}
      {/* 保持卡片挂载，收起/追加工具不丢已打开的详情。 */}
      <div hidden={!single && !expanded} className={!single ? "tool-group-body" : undefined}>
        {tools.map((tool) => <ToolCallCard key={tool.callKey} tool={tool} display="compact" origin={origins?.[tool.callKey]} onToggle={onToolToggle} onExpansionChange={onExpansionChange} />)}
      </div>
    </div>
  );
}
