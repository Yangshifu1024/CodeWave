import { CheckOutlined, CloseOutlined, ExclamationCircleOutlined, LoadingOutlined, RobotOutlined, StopOutlined } from "@ant-design/icons";
import { useTranslation } from "react-i18next";
import { useActiveRun, useRun } from "../../stores/run";
import type { SubView } from "../../stores/run.types";

/** token 数缩写：千位以上显示为一位小数的 k 值（如 12.3k），其余原样。 */
function fmtTokens(n: number): string {
  return n >= 1000 ? `${(n / 1000).toFixed(1)}k` : String(n);
}

/** 提前结束判定：已收尾且收尾原因不是约定汇报（旧数据无 ended 字段 → 视为正常收尾，保持绿勾向后兼容） */
function isEarlyEnded(sub: SubView): boolean {
  return sub.status === "done" && (sub.ended === "budget" || sub.ended === "no_report");
}

// 单个子代理卡片：从 timeline 锚点渲染（与工具卡同机制，在调用点穿插）。
// [docs/subagent-interaction-drawer](../../../../docs/subagent-interaction-drawer.md)：单行化「子智能体 <名称> · <任务>」；点击打开过程抽屉
// （运行中 = 实时消息流；已结束 = 归档回放，跨 run 保留，不再被下一次 send 清空）。
/** 子代理聊天卡：展示名称/任务/步骤/token 进度，点击或回车打开子代理过程抽屉；
 *  运行中时右侧有停止按钮（单独停止该子代理，不打开抽屉）。 */
export default function SubagentItemCard({ subId }: { subId: string }) {
  const { t } = useTranslation();
  const sub = useActiveRun().subs.find((x) => x.subId === subId);
  if (!sub) return null;
  const stop = () => void useRun.getState().stopSubagent(null, sub.subId);
  return (
    <div
      className={`sub-card st-${sub.status}`}
      role="button"
      tabIndex={0}
      title={sub.task || sub.description}
      onClick={() => void useRun.getState().openSubDrawer(null, sub.subId)}
      onKeyDown={(e) => {
        if (e.key === "Enter" || e.key === " ") void useRun.getState().openSubDrawer(null, sub.subId);
      }}
    >
      <RobotOutlined className="sub-card-robot" />
      <span className="sub-card-label">{t("subagent.label")}</span>
      <span className="sub-card-role">{sub.name || sub.role}</span>
      {sub.description && <span className="sub-card-desc">· {sub.description}</span>}
      <span className="sub-card-meta">
        {sub.status === "running" && <LoadingOutlined spin />}
        {sub.status === "done" && !isEarlyEnded(sub) && <CheckOutlined />}
        {sub.status === "done" && isEarlyEnded(sub) && (
          // 提前退出警示：橙色（需注意），颜色取主题桥的 antd colorWarning，不硬编码色值
          <ExclamationCircleOutlined style={{ color: "var(--ws-warn)" }} />
        )}
        {sub.status === "error" && <CloseOutlined />}
        {sub.step}/{sub.maxSteps} · {fmtTokens(sub.tokens)} tok
        {sub.status === "done" && isEarlyEnded(sub) && (
          <span>· {sub.ended === "budget" ? t("subagent.endedBudget") : t("subagent.endedEarly")}</span>
        )}
      </span>
      {/* 停止按钮（仅运行中）：stopPropagation 防止误开抽屉；点击 = 单独停止该子代理 */}
      {sub.status === "running" && (
        <span
          className="sub-card-stop"
          role="button"
          aria-label={t("subagent.stop")}
          title={t("subagent.stop")}
          onClick={(e) => {
            e.stopPropagation();
            stop();
          }}
          onKeyDown={(e) => {
            if (e.key === "Enter" || e.key === " ") {
              e.stopPropagation();
              stop();
            }
          }}
        >
          <StopOutlined />
        </span>
      )}
    </div>
  );
}
