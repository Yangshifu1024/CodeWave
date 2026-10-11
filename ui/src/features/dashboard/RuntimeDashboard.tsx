import { useEffect, useState } from "react";
import { Alert, App, Button, Card, Collapse, Descriptions, Drawer, Empty, Flex, Space, Spin, Statistic, Tag, Tree, Typography } from "antd";
import { ReloadOutlined, RobotOutlined } from "@ant-design/icons";
import { useTranslation } from "react-i18next";
import type { ActivitySession, ActivitySub, ActivityWaiting } from "../../ipc/types";
import { useActivity, watchActivity } from "../../stores/activity";
import { useSessions } from "../../stores/sessions";
import { useUi } from "../../stores/ui";
import { openActivityWaiting } from "./navigation";
import "./dashboard.css";

interface DetailTarget {
  sessionId: string;
  runId: string;
  title: string;
  sub: ActivitySub;
}

const STATUS_KEYS = {
  running: "dashboard.running", done: "dashboard.done", error: "dashboard.error", cancelled: "dashboard.cancelled",
} as const;
const ENDING_KEYS = {
  report: "dashboard.report", partial: "dashboard.partial", budget: "dashboard.budget", no_report: "dashboard.no_report",
} as const;
function statusKey(status: ActivitySub["status"], ended: ActivitySub["ended"]): string {
  return status === "done" && ended ? ENDING_KEYS[ended] : STATUS_KEYS[status];
}

function SubStatus({ sub }: { sub: ActivitySub }) {
  const { t } = useTranslation();
  const attention = sub.status !== "cancelled" && sub.ended && sub.ended !== "report";
  return <Tag color={sub.status === "error" ? "error" : attention ? "warning" : undefined}>
    {t(statusKey(sub.status, sub.ended))}
  </Tag>;
}

/** 运行摘要和只读报告；目标来自会话/run/sub 身份，绝不跟随 active Tab。 */
function ActivityDetail({ target, onClose, onOpen }: { target: DetailTarget | null; onClose(): void; onOpen(id: string): Promise<void> }) {
  const { t } = useTranslation();
  const snapshot = useActivity((s) => s.snapshot);
  const [opening, setOpening] = useState(false);
  const current = target && (
    snapshot?.sessions.find((s) => s.session_id === target.sessionId && s.run_id === target.runId)?.subs.find((s) => s.sub_id === target.sub.sub_id)
    ?? snapshot?.recent.find((r) => r.session_id === target.sessionId && r.run_id === target.runId && r.sub?.sub_id === target.sub.sub_id)?.sub
    ?? target.sub
  );
  return <Drawer open={!!target} title={t("dashboard.agentDetails")} size="50vw" mask={{ closable: true }} onClose={onClose}>
    {target && current && <Space orientation="vertical" size="large" style={{ width: "100%" }}>
      <Typography.Title level={4} style={{ margin: 0 }}>{current.name || current.role}</Typography.Title>
      <SubStatus sub={current} />
      <Descriptions column={1} items={[
        { key: "session", label: t("dashboard.session"), children: target.title },
        { key: "role", label: t("dashboard.role"), children: current.role },
        { key: "task", label: t("dashboard.task"), children: <span className="activity-copy">{current.task}</span> },
        { key: "budget", label: t("dashboard.stepsBudget"), children: `${current.step} / ${current.max_steps}` },
        { key: "activity", label: t("dashboard.latest"), children: current.detail || t("dashboard.noActivity") },
        { key: "tool", label: t("dashboard.lastTool"), children: current.last_tool || "—" },
      ]} />
      <Typography.Text type="secondary">{t("dashboard.summaryHint")}</Typography.Text>
      <div>
        <Typography.Title level={5}>{t("dashboard.reportTitle")}</Typography.Title>
        {current.report ? <Typography.Paragraph className="activity-copy" copyable>{current.report}</Typography.Paragraph>
          : <Typography.Text type="secondary">{t("dashboard.noReport")}</Typography.Text>}
      </div>
      <Button loading={opening} onClick={() => {
        setOpening(true);
        void onOpen(target.sessionId).then(onClose).catch(() => {}).finally(() => setOpening(false));
      }}>{t("dashboard.openParent")}</Button>
    </Space>}
  </Drawer>;
}

export default function RuntimeDashboard() {
  const { t } = useTranslation();
  const { message } = App.useApp();
  const snapshot = useActivity((s) => s.snapshot);
  const refreshing = useActivity((s) => s.refreshing);
  const error = useActivity((s) => s.error);
  const lastSyncedAt = useActivity((s) => s.lastSyncedAt);
  const expanded = useActivity((s) => s.expanded);
  const recentOpen = useActivity((s) => s.recentOpen);
  const projects = useSessions((s) => s.projects);
  const [target, setTarget] = useState<DetailTarget | null>(null);
  const [pending, setPending] = useState<string | null>(null);
  useEffect(() => watchActivity(), []);
  const sessions = snapshot?.sessions ?? [];
  const waiting = sessions.filter((s) => s.waiting.length > 0);
  const advancing = sessions.filter((s) => s.waiting.length === 0);
  const waitingCount = sessions.reduce((n, s) => n + s.waiting.length, 0);
  const subCount = sessions.reduce((n, s) => n + s.subs.filter((sub) => sub.status === "running").length, 0);
  const projectName = (id: string | null) => id ? projects.find((p) => p.id === id)?.name || t("dashboard.project") : t("dashboard.temporary");
  const elapsed = (start: number) => {
    const seconds = Math.max(0, Math.floor(((snapshot?.sampled_at_ms ?? start) - start) / 1000));
    return seconds < 60 ? t("dashboard.seconds", { n: seconds }) : t("dashboard.minutes", { n: Math.floor(seconds / 60) });
  };
  const openSession = async (id: string) => {
    try { await useSessions.getState().openSessionReady(id); }
    catch (e) { message.error(t("dashboard.openFailed", { error: String(e) })); throw e; }
  };
  const openWaiting = async (session: ActivitySession, item: ActivityWaiting) => {
    if (pending) return;
    setPending(item.ask_id);
    try {
      if (!await openActivityWaiting(session, item)) {
        message.info(t("dashboard.askExpired"));
        useUi.setState({ dashboardOpen: true });
      }
    } catch (e) { message.error(t("dashboard.openFailed", { error: String(e) })); }
    finally { setPending(null); }
  };
  const showDetail = (sessionId: string, runId: string, title: string, sub: ActivitySub) => setTarget({ sessionId, runId, title, sub });
  const hierarchy = (session: ActivitySession) => <Tree
    selectable={false} showLine defaultExpandAll
    treeData={[{
      key: `${session.run_id}:main`,
      title: <Space wrap><RobotOutlined /><Typography.Text strong>{t("dashboard.mainAgent")}</Typography.Text><Typography.Text type="secondary">{session.activity || t("dashboard.noActivity")}</Typography.Text></Space>,
      children: session.subs.map((sub) => ({
        key: sub.sub_id,
        title: <div className="activity-agent-row">
          <Space wrap><Typography.Text strong>{sub.name || sub.role}</Typography.Text><Typography.Text type="secondary">{sub.role}</Typography.Text><SubStatus sub={sub} /></Space>
          <Typography.Paragraph ellipsis={{ rows: 2, tooltip: sub.task }} style={{ marginBottom: 4 }}>{sub.task}</Typography.Paragraph>
          <Space wrap><Typography.Text type="secondary">{t("dashboard.stepsBudget")}: {sub.step} / {sub.max_steps}</Typography.Text>
            <Typography.Text type="secondary">{sub.detail || t("dashboard.noActivity")}</Typography.Text>
            <Button size="small" onClick={() => showDetail(session.session_id, session.run_id, session.title, sub)}>{t("dashboard.details")}</Button>
          </Space>
        </div>,
      })),
    }]}
  />;
  const sessionList = (items: ActivitySession[]) => <Collapse activeKey={expanded.filter((key) => items.some((s) => key === `${s.session_id}:${s.run_id}`))} onChange={(keys) => {
    const owned = new Set(items.map((s) => `${s.session_id}:${s.run_id}`));
    const next = Array.isArray(keys) ? keys.map(String) : [String(keys)];
    useActivity.getState().setExpanded([...new Set([...expanded.filter((key) => !owned.has(key)), ...next.filter((key) => owned.has(key))])]);
  }} items={items.map((session) => ({
    key: `${session.session_id}:${session.run_id}`,
    label: <div className="activity-session-label">
      <Space wrap><Typography.Text strong title={session.title}>{session.title || t("dashboard.untitled")}</Typography.Text>
        <Tag color={session.waiting.length ? "warning" : undefined}>{t(session.waiting.length ? "dashboard.waiting" : "dashboard.running")}</Tag>
        <Typography.Text type="secondary">{projectName(session.project_id)} · {elapsed(session.started_at_ms)} · {t("dashboard.subsCount", { n: session.subs.length })}</Typography.Text>
      </Space>
      <Typography.Paragraph type="secondary" ellipsis={{ rows: 2, tooltip: session.activity }} style={{ margin: "4px 0" }}>{session.activity || t("dashboard.noActivity")}</Typography.Paragraph>
      {session.waiting.length > 0 && <Space orientation="vertical" style={{ width: "100%" }}>
        {session.waiting.map((item) => <Flex key={item.ask_id} align="center" gap="small" wrap>
          <Tag color="warning">{t(item.kind === "approval" ? "dashboard.approval" : "dashboard.answer")}</Tag>
          <Typography.Text title={item.detail}>{item.title || item.detail || t("dashboard.waiting")}</Typography.Text>
          <Button size="small" loading={pending === item.ask_id} disabled={pending !== null && pending !== item.ask_id}
            onClick={(e) => { e.stopPropagation(); void openWaiting(session, item); }}>
            {t(item.kind === "approval" ? "dashboard.goApprove" : "dashboard.goAnswer")}
          </Button>
        </Flex>)}
      </Space>}
    </div>,
    extra: <Button size="small" onClick={(e) => { e.stopPropagation(); void openSession(session.session_id).catch(() => {}); }}>{t("dashboard.openSession")}</Button>,
    children: hierarchy(session),
  }))} />;
  return <section className="runtime-dashboard" aria-label={t("dashboard.title")}>
    <div className="activity-page-header">
      <div><Typography.Title level={3} style={{ margin: 0 }}>{t("dashboard.title")}</Typography.Title><Typography.Text type="secondary">{t("dashboard.subtitle")}</Typography.Text></div>
      <Space wrap>
        <Button icon={<ReloadOutlined />} loading={refreshing} onClick={() => void useActivity.getState().refresh()}>{t("dashboard.refresh")}</Button>
        <Button onClick={() => useUi.setState({ dashboardOpen: false })}>{t("dashboard.back")}</Button>
      </Space>
    </div>
    <div className="activity-page-body">
      <Space orientation="vertical" size="large" style={{ width: "100%" }}>
        {error && <Alert type="warning" showIcon title={t("dashboard.syncFailed")} description={<>{error}{lastSyncedAt && <div>{t("dashboard.lastSync", { time: new Date(lastSyncedAt).toLocaleTimeString() })}</div>}</>} />}
        {!snapshot && !error ? <Spin tip={t("dashboard.loading")}><div style={{ minHeight: 160 }} /></Spin> : snapshot && <>
          <div className="activity-summary">
            <Card><Statistic title={t("dashboard.activeSessions")} value={sessions.length} /></Card>
            <Card><Statistic title={t("dashboard.needsYou")} value={waitingCount} /></Card>
            <Card><Statistic title={t("dashboard.runningSubs")} value={subCount} /></Card>
          </div>
          {waiting.length > 0 && <div><Typography.Title level={4}>{t("dashboard.needsYou")}</Typography.Title>{sessionList(waiting)}</div>}
          <div><Typography.Title level={4}>{t("dashboard.advancing")}</Typography.Title>
            {advancing.length > 0 ? sessionList(advancing) : <Empty image={Empty.PRESENTED_IMAGE_SIMPLE} description={t(sessions.length ? "dashboard.allWaiting" : "dashboard.empty")} />}
          </div>
          <Collapse activeKey={recentOpen ? ["recent"] : []} onChange={(keys) => useActivity.getState().setRecentOpen(keys.length > 0)} items={[{
            key: "recent", label: t("dashboard.recent", { n: snapshot.recent.length }),
            children: <Space orientation="vertical" style={{ width: "100%" }}>
              <Typography.Text type="secondary">{t("dashboard.recentHint")}</Typography.Text>
              {snapshot.recent.length === 0 ? <Empty image={Empty.PRESENTED_IMAGE_SIMPLE} description={t("dashboard.noRecent")} /> : snapshot.recent.map((item) => <Card size="small" key={item.id}>
                <Flex justify="space-between" gap="small" wrap>
                  <Space wrap><Typography.Text strong title={item.title}>{item.title}</Typography.Text>
                    <Tag>{t(item.kind === "session" ? "dashboard.session" : "dashboard.subagent")}</Tag>
                    <Tag color={item.status === "error" ? "error" : item.status !== "cancelled" && item.ended && item.ended !== "report" ? "warning" : undefined}>{t(statusKey(item.status, item.ended))}</Tag>
                    <Typography.Text type="secondary">{projectName(item.project_id)} · {new Date(item.finished_at_ms).toLocaleTimeString()}</Typography.Text>
                  </Space>
                  {item.sub ? <Button size="small" onClick={() => showDetail(item.session_id, item.run_id,
                    sessions.find((s) => s.session_id === item.session_id)?.title
                    || useSessions.getState().sessions.find((s) => s.id === item.session_id)?.title || item.session_id, item.sub!)}>{t("dashboard.details")}</Button>
                    : <Button size="small" onClick={() => void openSession(item.session_id).catch(() => {})}>{t("dashboard.openSession")}</Button>}
                </Flex>
              </Card>)}
            </Space>,
          }]} />
        </>}
      </Space>
    </div>
    <ActivityDetail target={target} onClose={() => setTarget(null)} onOpen={openSession} />
  </section>;
}
