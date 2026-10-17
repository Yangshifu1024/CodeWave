// 交付验收的**只读**投影（#16 完成门 / [docs/tools-and-safety](../../../../docs/tools-and-safety.md)）。
//
// 这块面板存在的唯一理由是**不让人把「运行结束」误读成「任务完成」**：
// `run:done` 只说明模型停了，能不能算交付通过由验收器在**已采纳的不可变证据**上判定。
//
// 渲染纪律（[docs/plan-and-approval](../../../../docs/plan-and-approval.md)）：
// - 组件**没有任何写入路径**——没有「标记完成」按钮，没有 set_passed。
// - 快照缺失（老会话 / 无契约）显示「未验证」，**不**显示「未通过」，更不显示「已完成」。
// - 颜色只映射风险等级（无彩色 = 默认 / 橙 = 需注意 / 红 = 危险），强调色仍是中性墨色。
import { useEffect, useState } from "react";
import { Empty, Tag, Tooltip } from "antd";
import { useTranslation } from "react-i18next";
import { ipc } from "../../ipc/client";
import { useRun } from "../../stores/run";
import type {
  DeliveryProjection,
  DeliveryDecision,
  AcceptanceItemStatus,
} from "../../ipc/types";

/** 验收状态 → 风险等级。只有三档：无彩色 / 橙 / 红（[docs/chat-and-interface]）。 */
function riskOf(status: AcceptanceItemStatus | undefined): "default" | "warn" | "danger" {
  if (status === "failed" || status === "stale") return "danger";
  if (status === "blocked" || status === "pending") return "warn";
  return "default";
}

/**
 * 枚举 → i18n 键的**静态映射**，不用模板字符串拼键。
 *
 * 拼出来的键有两处坏处：i18n 键集合守护看不见它（少一侧翻译不会红），
 * 拼错也只在运行时才炸。登记成字面量集合后，改键名必然双侧一起红。
 * （settings.registry.test.ts 会连注释里的示例字面量一起扫，注释也别写拼键示例。）
 */
const DECISION_KEY: Record<DeliveryDecision, string> = {
  allow: "acceptance.decision.allow",
  repair: "acceptance.decision.repair",
  blocked: "acceptance.decision.blocked",
  cancelled: "acceptance.decision.cancelled",
};

const STATUS_KEY: Record<AcceptanceItemStatus, string> = {
  pending: "acceptance.status.pending",
  passed: "acceptance.status.passed",
  failed: "acceptance.status.failed",
  blocked: "acceptance.status.blocked",
  stale: "acceptance.status.stale",
  waived: "acceptance.status.waived",
  cancelled: "acceptance.status.cancelled",
};

export default function AcceptancePanel({
  sessionId,
  visible,
}: {
  sessionId: string | null;
  visible: boolean;
}) {
  const { t } = useTranslation();
  const running = useRun((s) => sessionId ? s.tabs[sessionId]?.running : undefined);
  const epoch = useRun((s) => sessionId ? s.tabs[sessionId]?.runEpoch : undefined);
  const done = useRun((s) => sessionId ? s.tabs[sessionId]?.lastDoneRunId : undefined);
  const [state, setState] = useState<{
    sessionId: string | null;
    data: DeliveryProjection | null;
    loading: boolean;
    error: string | null;
  }>({ sessionId: null, data: null, loading: false, error: null });

  useEffect(() => {
    if (!visible || !sessionId) return;
    let alive = true;
    let inFlight = false;
    const refresh = async () => {
      if (!alive || inFlight) return;
      inFlight = true;
      // 重新验证期间不继续展示旧的通过结论；错误也不能保留旧通过态。
      setState({ sessionId, data: null, loading: true, error: null });
      try {
        const data = await ipc.getDeliveryProjection(sessionId);
        if (alive) setState({ sessionId, data, loading: false, error: null });
      } catch (error: unknown) {
        if (alive) setState({ sessionId, data: null, loading: false, error: String(error) });
      } finally {
        inFlight = false;
      }
    };
    void refresh();
    // 运行边界即时刷新；低频轮询与回到窗口覆盖外部文件变化和同轮契约更新。
    const timer = window.setInterval(() => { void refresh(); }, 10_000);
    const onFocus = () => { void refresh(); };
    window.addEventListener("focus", onFocus);
    return () => {
      alive = false;
      window.clearInterval(timer);
      window.removeEventListener("focus", onFocus);
    };
  }, [sessionId, visible, running, epoch, done]);

  if (!visible || !sessionId) return null;
  const current = state.sessionId === sessionId;
  const data = current ? state.data : null;
  const loading = !current || state.loading;
  const error = current ? state.error : null;

  // 没有交付契约：这是普通问答，**不该**凭空多出一块"未验证"面板。
  if (!loading && !error && data && !data.task_id) return null;

  if (loading) return <div className="rb-empty">{t("acceptance.loading")}</div>;
  if (error) return <div className="rb-empty">{t("acceptance.failed", { error })}</div>;
  if (!data) return <Empty image={Empty.PRESENTED_IMAGE_SIMPLE} description={t("acceptance.unverified")} />;

  const snapshot = data.snapshot;
  // 快照缺失但有契约 = 老会话缺少本能力：**显示未验证**，不补造结论。
  if (!snapshot) {
    return <Empty image={Empty.PRESENTED_IMAGE_SIMPLE} description={t("acceptance.unverified")} />;
  }

  const passed = data.allows_completion;

  return (
    <div className="acceptance-panel">
      <div className="acceptance-head">
        <span className="acceptance-title">{t("acceptance.title")}</span>
        <Tag color={passed ? "default" : data.decision === "cancelled" ? "default" : "warning"}>
          {passed ? t("acceptance.passed") : t(DECISION_KEY[data.decision])}
        </Tag>
      </div>

      {snapshot.waived_count > 0 && (
        <div className="acceptance-note">{t("acceptance.waived", { n: snapshot.waived_count })}</div>
      )}

      {data.repair_rounds_used > 0 && (
        <div className="acceptance-note">
          {t("acceptance.repairBudget", {
            used: data.repair_rounds_used,
            max: data.repair_rounds_max,
          })}
        </div>
      )}

      <ul className="acceptance-items">
        {(snapshot.items ?? []).map((item) => (
          <li key={item.requirement_id} className="acceptance-item">
            <Tag color={riskOf(item.status)}>{t(STATUS_KEY[item.status])}</Tag>
            <span className="acceptance-req">{item.requirement_id}</span>
            {(item.reasons ?? []).map((r, i) => (
              <Tooltip key={`${r.code}-${i}`} title={r.code}>
                <span className="acceptance-reason">{r.message}</span>
              </Tooltip>
            ))}
          </li>
        ))}
      </ul>

      {data.gaps.length > 0 && (
        <>
          <div className="acceptance-subtitle">{t("acceptance.gaps")}</div>
          <ul className="acceptance-gaps">
            {data.gaps.map((g, i) => (
              <li key={`${g.reason_code}-${i}`}>
                {g.check_id ? `${g.check_id}：` : ""}
                {g.message}
              </li>
            ))}
          </ul>
        </>
      )}

      <div className="acceptance-hint">{t("acceptance.hint")}</div>
    </div>
  );
}
