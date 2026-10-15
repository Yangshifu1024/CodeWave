import { memo, useCallback, useEffect, useState } from "react";
import { Button } from "antd";
import { FileSearchOutlined, RightOutlined, DownOutlined, LoadingOutlined } from "@ant-design/icons";
import { useTranslation } from "react-i18next";
import type { TimelineSeg, ToolView } from "../../stores/run.types";
import ToolCallCard from "./ToolCallCard";
import type { ToolExpansion } from "./ToolCallCard";
import { ThinkingBlock } from "../chat/thinkingBlock";
import { isThinkingActive, processGroupOutputKey, processMemberOutputKey, toolFilePaths, type ProcessMember, type ToolOrigin } from "./toolPresentation";

/** 一个过程组的渲染输入：稳定根键 + 按原 timeline 顺序排列的成员。 */
export interface ProcessGroupEntry {
  /** 稳定锚点根键：优先首个工具的 callKey，纯思考组退回 thinking:<下标>（见 processGroupKey） */
  key: string;
  tools: ToolView[];
  members: ProcessMember[];
}

/** 只折叠展示；结果仍由 toolsMap 提供，不写摘要到历史。 */
function ToolActivityGroupImpl({ entry, timeline, streaming, display, origins, onToggle, onUserToggle, onExpansionChange, expanded: controlledExpanded, onGroupExpansionChange }: {
  entry: ProcessGroupEntry;
  /** 组所在消息的完整时间线：思考块需要原始段（含计时字段） */
  timeline: readonly TimelineSeg[];
  streaming?: boolean;
  display: "compact" | "detailed";
  origins?: Record<string, ToolOrigin>;
  onToggle?: () => void;
  /** 展开/收起即阅读意图信号（与工具卡同一口径），透传给组内思考块 */
  onUserToggle?: () => void;
  onExpansionChange: (callKey: string, expansion: ToolExpansion) => void;
  expanded?: boolean;
  onGroupExpansionChange?: (callKey: string, expanded: boolean) => void;
}) {
  const { t } = useTranslation();
  const [localExpanded, setExpanded] = useState(false);
  const expanded = controlledExpanded ?? localExpanded;
  const members = entry.members;
  const rootKey = entry.key;
  const thinkings = members.filter((member): member is Extract<ProcessMember, { kind: "thinking" }> => member.kind === "thinking");
  const hasRunningTool = entry.tools.some(tool => tool.status === "running");
  // 计时中的思考位置：从尾部回扫、跳过空白正文段（空白段会被吸收进组，不算真实尾部）。
  // 直接用 timeline.length-1 会在末尾拖了空白正文时错判，摘要会从「思考中……Ns」退成「思考 N 次」。
  let tailIndex = timeline.length - 1;
  while (tailIndex >= 0) {
    const seg = timeline[tailIndex];
    if (seg.kind === "text" && !seg.text.trim()) { tailIndex--; continue; }
    break;
  }
  // 运行中的思考：只在它位于时间线尾部且消息仍在流式时才算「正在思考」。
  // 恢复的历史段与被中断的段都没有真实耗时 —— 它们的 label 走「思考 N 次」，绝不补 0s。
  const liveThinking = thinkings.find(member => isThinkingActive(member.seg, !!streaming && member.index === tailIndex));
  const live = !!liveThinking || hasRunningTool;
  // 运行中摘要要跳秒：以 500ms 心跳驱动，与 ThinkingBlock 的计时口径一致
  const [, setTick] = useState(0);
  useEffect(() => {
    if (!live) return;
    const id = setInterval(() => setTick(n => n + 1), 500);
    return () => clearInterval(id);
  }, [live]);

  const reads = entry.tools.filter(tool => tool.tool === "read" || tool.tool === "batch_read");
  const paths = new Set(reads.flatMap(toolFilePaths));
  const commands = entry.tools.filter(tool => tool.tool === "command").length;
  const searches = entry.tools.filter(tool => tool.tool === "grep").length;
  const lists = entry.tools.filter(tool => tool.tool === "list_files").length;
  const thinkingLabel = thinkings.length === 0 ? "" : liveThinking
    ? t("tools.groupThinkingActive", { seconds: `${Math.max(1, Math.floor((Date.now() - (liveThinking.seg.startedAt ?? Date.now())) / 1000))}s` })
    : t("tools.groupThinking", { n: thinkings.length });
  const label = [
    thinkingLabel,
    reads.length ? (paths.size ? t("tools.groupRead", { n: paths.size }) : t("tools.groupReadUnknown")) : "",
    searches ? t("tools.groupSearch", { n: searches }) : "",
    lists ? t("tools.groupList", { n: lists }) : "",
    commands ? t("tools.groupCommand", { n: commands }) : "",
  ].filter(Boolean).join(" · ");
  // 组根的定位别名：跨消息组必须覆盖每个成员所属的源消息（含思考段），
  // 否则滚动定位会掉回上一条消息（docs/chat-and-interface 5.2.x）
  const aliases: ToolOrigin[] = [];
  const seen = new Set<string>();
  for (const member of members) {
    const origin = origins?.[member.kind === "tool" ? member.callKey : processMemberOutputKey(member)];
    if (origin && !seen.has(origin.key)) { seen.add(origin.key); aliases.push(origin); }
  }
  const groupKeys = members.map(processMemberOutputKey);
  const outputKey = processGroupOutputKey(members);
  const onToolToggle = useCallback(() => {
    setExpanded(true);
    if (entry.tools.length !== 1) onGroupExpansionChange?.(rootKey, true);
    onToggle?.();
  }, [entry.tools.length, rootKey, onGroupExpansionChange, onToggle]);
  // 单成员组不画摘要行：直接内联渲染那一个成员。
  // 折叠行的意义是「多行收成一行」，单成员本来就是一行，再包一层只会平白多一次点击
  // （docs/chat-and-interface 5.2.x）。思考与工具交替出现时必是 ≥2 个成员，不受此规则影响。
  const renderMember = (member: ProcessMember) => {
    if (member.kind === "thinking") {
      return <ThinkingBlock key={`thinking:${member.index}`} text={member.seg.text} startedAt={member.seg.startedAt} durationMs={member.seg.durationMs} onToggle={onUserToggle} compact streaming={!!streaming && member.index === tailIndex} />;
    }
    return <ToolCallCard key={member.callKey} tool={member.tool} display={display} origin={origins?.[member.callKey]} onToggle={onToolToggle} onExpansionChange={onExpansionChange} />;
  };
  if (members.length === 1) return <div className="tool-activity-group">{renderMember(members[0])}</div>;
  return (
    <div className="tool-activity-group">
      <Button type="text" className="tool-group-toggle" data-output-key={outputKey ?? undefined} data-output-group={JSON.stringify(groupKeys)} data-anchor-aliases={!expanded && aliases.length > 1 ? JSON.stringify(aliases) : undefined} data-output-message-keys={aliases.length > 1 ? JSON.stringify(aliases.map(alias => alias.key)) : undefined} aria-expanded={expanded} onClick={() => {
        setExpanded(!expanded);
        onGroupExpansionChange?.(rootKey, !expanded);
        onToggle?.();
      }}>
        {expanded ? <DownOutlined /> : <RightOutlined />}
        {live ? <LoadingOutlined spin /> : <FileSearchOutlined />}
        <span className="tool-group-label" title={label}>{label}</span>
      </Button>
      {/* 保持卡片挂载，收起/追加工具不丢已打开的详情。 */}
      <div hidden={!expanded} className="tool-group-body">
        {members.map(renderMember)}
      </div>
    </div>
  );
}

export default memo(ToolActivityGroupImpl);
