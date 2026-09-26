import { useState } from "react";
import { Alert, Button, Input, InputNumber, Modal, Radio, Space, Typography } from "antd";
import { useTranslation } from "react-i18next";
import type { GoalBudget, GoalState } from "../../ipc/types";
import { ipc } from "../../ipc/client";
import { useRun } from "../../stores/run";
import { useSessions } from "../../stores/sessions";

/** Only user actions can save budgets or accept delivery. Never infer either from model output. */
export default function GoalControls({ goal, sessionId, budgetOnly = false, readOnly = false }: {
  goal: GoalState; sessionId: string; budgetOnly?: boolean; readOnly?: boolean;
}) {
  const { t } = useTranslation();
  const [open, setOpen] = useState(false);
  const [choice, setChoice] = useState<"limited" | "unlimited" | null>(null);
  const [tokens, setTokens] = useState<number | null>(null);
  const [minutes, setMinutes] = useState<number | null>(null);
  const [feedback, setFeedback] = useState("");
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState("");
  const budget = goal.delivery?.budget;
  const editable = !readOnly && ["clarify", "paused", "awaiting_acceptance"].includes(goal.status);
  const valid = choice === "unlimited" || (choice === "limited" &&
    (tokens !== null || minutes !== null) &&
    (tokens === null || (Number.isSafeInteger(tokens) && tokens > 0)) &&
    (minutes === null || (Number.isFinite(minutes) && minutes > 0 && Number.isSafeInteger(minutes * 60000))));

  async function update(action: () => Promise<GoalState>) {
    setBusy(true);
    setError("");
    try {
      const rev = useRun.getState().tabs[sessionId]?.goalRev ?? 0;
      const next = await action();
      useRun.setState((state) => {
        const tab = state.tabs[sessionId];
        if (tab && (tab.goalRev ?? 0) === rev) { tab.goal = next; tab.goalRev = rev + 1; }
      });
      if (next.status === "done") await useSessions.getState().syncPrefs(sessionId);
      setOpen(false);
      setFeedback("");
    } catch (e) { setError(String(e)); }
    finally { setBusy(false); }
  }

  function editBudget() {
    setChoice(budget ? (budget.unlimited ? "unlimited" : "limited") : null);
    setTokens(budget?.token_limit ?? null);
    setMinutes(budget?.time_limit_ms == null ? null : budget.time_limit_ms / 60000);
    setError("");
    setOpen(true);
  }

  function saveBudget() {
    if (!valid || busy) return;
    const next: GoalBudget = choice === "unlimited"
      ? { unlimited: true, token_limit: null, time_limit_ms: null }
      : { unlimited: false, token_limit: tokens, time_limit_ms: minutes === null ? null : minutes * 60000 };
    void update(() => ipc.setGoalBudget(sessionId, next));
  }

  return <Space orientation="vertical" size="small" style={{ width: "100%" }}>
    <Typography.Text type="secondary">{t("goal.permission")}</Typography.Text>
    <Typography.Text>{!budget ? t("goal.budgetRequired") : budget.unlimited ? t("goal.unlimited") :
      t("goal.budgetSummary", { tokens: budget.token_limit ?? "—", minutes: budget.time_limit_ms == null ? "—" : budget.time_limit_ms / 60000 })}</Typography.Text>
    {editable && <Button size="small" onClick={editBudget}>{t("goal.configureBudget")}</Button>}
    {!budgetOnly && <Typography.Text type="secondary">{t("goal.usage", {
      tokens: goal.delivery?.used_tokens ?? 0, minutes: Math.floor((goal.delivery?.elapsed_ms ?? 0) / 60000),
    })}</Typography.Text>}
    {!budgetOnly && goal.status === "paused" && <Typography.Text type="secondary">{t("goal.pausedHint")}</Typography.Text>}
    {!budgetOnly && goal.status === "stopping" && <Alert type="info" title={t("goal.stoppingHint")} />}
    {!budgetOnly && !readOnly && goal.status === "awaiting_acceptance" && <>
      <Alert type="info" title={t("goal.acceptanceHint")} />
      <Button disabled={busy} onClick={() => void update(() => ipc.acceptGoal(sessionId, true))}>{t("goal.accept")}</Button>
      <Input.TextArea value={feedback} onChange={(e) => setFeedback(e.target.value)} placeholder={t("goal.feedbackPlaceholder")} autoSize={{ minRows: 2, maxRows: 6 }} />
      <Button disabled={busy || !feedback.trim()} onClick={() => void update(() => ipc.acceptGoal(sessionId, false, feedback.trim()))}>{t("goal.feedback")}</Button>
    </>}
    {error && !open && <Alert type="error" title={error} />}
    <Modal title={t("goal.configureBudget")} open={open} onCancel={() => !busy && setOpen(false)} onOk={saveBudget}
      okText={t("goal.saveBudget")} confirmLoading={busy} okButtonProps={{ disabled: !valid }}>
      <Space orientation="vertical" style={{ width: "100%" }}>
        <Radio.Group value={choice} onChange={(e) => setChoice(e.target.value)} options={[
          { value: "limited", label: t("goal.limited") }, { value: "unlimited", label: t("goal.unlimited") },
        ]} />
        {choice === "limited" && <>
          <Typography.Text>{t("goal.tokenLimit")}</Typography.Text>
          <InputNumber aria-label={t("goal.tokenLimit")} min={1} precision={0} value={tokens} onChange={setTokens} />
          <Typography.Text>{t("goal.timeLimit")}</Typography.Text>
          <InputNumber aria-label={t("goal.timeLimit")} min={1} precision={0} value={minutes} onChange={setMinutes} />
        </>}
        <Alert type="info" title={t("goal.budgetHint")} />
        {error && <Alert type="error" title={error} />}
      </Space>
    </Modal>
  </Space>;
}
