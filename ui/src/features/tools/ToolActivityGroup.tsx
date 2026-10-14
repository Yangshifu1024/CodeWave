import { useCallback, useState } from "react";
import { Button } from "antd";
import { FileSearchOutlined, RightOutlined, DownOutlined } from "@ant-design/icons";
import { useTranslation } from "react-i18next";
import type { ToolView } from "../../stores/run.types";
import ToolCallCard from "./ToolCallCard";
import type { ToolExpansion } from "./ToolCallCard";
import { toolFilePaths } from "./toolPresentation";

/** 只折叠展示；结果仍由 toolsMap 提供，不写摘要到历史。 */
export default function ToolActivityGroup({ tools, onToggle, onExpansionChange }: {
  tools: ToolView[];
  onToggle?: () => void;
  onExpansionChange: (callKey: string, expansion: ToolExpansion) => void;
}) {
  const { t } = useTranslation();
  const [expanded, setExpanded] = useState(false);
  const single = tools.length === 1;
  const onToolToggle = useCallback(() => {
    setExpanded(true);
    onToggle?.();
  }, [onToggle]);
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
      {!single && <Button type="text" className="tool-group-toggle" data-output-key={`tool:${tools[0].callKey}`} data-output-group={JSON.stringify(tools.map((tool) => `tool:${tool.callKey}`))} aria-expanded={expanded} onClick={() => {
        setExpanded(!expanded);
        onToggle?.();
      }}>
        <FileSearchOutlined />
        <span className="tool-group-label" title={label}>{label}</span>
        {expanded ? <DownOutlined /> : <RightOutlined />}
      </Button>}
      {/* 保持卡片挂载，收起/追加工具不丢已打开的详情。 */}
      <div hidden={!single && !expanded} className={!single ? "tool-group-body" : undefined}>
        {tools.map((tool) => <ToolCallCard key={tool.callKey} tool={tool} display="compact" onToggle={onToolToggle} onExpansionChange={onExpansionChange} />)}
      </div>
    </div>
  );
}
