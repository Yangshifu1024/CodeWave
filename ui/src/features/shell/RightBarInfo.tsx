import { useEffect, useState } from "react";
import { App, Button, Card, Collapse, Tooltip, Typography } from "antd";
import { CheckOutlined, FileOutlined, FolderOpenOutlined, ForkOutlined, LoadingOutlined, RightOutlined, RobotOutlined, WarningOutlined } from "@ant-design/icons";
import { useTranslation } from "react-i18next";
import { ipc } from "../../ipc/client";
import type { SessionFileEntry, SkillMeta } from "../../ipc/types";
import { useRun } from "../../stores/run";
import type { SubView } from "../../stores/run";
import { useActiveTab, useSessions } from "../../stores/sessions";
import { useUi } from "../../stores/ui";
import { originLabel } from "../../utils/skills";
import { readCollapsedSections, writeCollapsedSections, type CollapsibleSection } from "../../utils/rightbarPrefs";
import QuotaSection from "../quota/QuotaSection";
import AcceptancePanel from "../chat/AcceptancePanel";
import type { WorkspaceChanges } from "../workspace/useWorkspaceChanges";
import OpenInEditorSelect from "./OpenInEditorSelect";
import SkillDetailModal from "./SkillDetailModal";

function subStatus(sub: SubView) {
  if (sub.status === "running") return "running";
  if (sub.status === "error" || sub.ended === "budget" || sub.ended === "no_report") return "error";
  if (sub.ended && sub.ended !== "report") return "attention";
  return "done";
}

/** Current-session summary; detail actions reuse the existing tabs, drawer and file viewer. */
export default function RightBarInfo({ visible, files, changes, onView }: {
  visible: boolean; files: SessionFileEntry[]; changes: WorkspaceChanges; onView(path: string): void;
}) {
  const { t } = useTranslation();
  const { message } = App.useApp();
  const tab = useActiveTab();
  const sessionId = tab?.sessionId ?? null;
  const projects = useSessions(s => s.projects);
  const todos = useRun(s => sessionId ? s.tabs[sessionId]?.todos : undefined) ?? [];
  const subs = useRun(s => sessionId ? s.tabs[sessionId]?.subs : undefined) ?? [];
  const rightBarOpen = useUi(s => s.rightBarOpen);
  const setRbTab = useUi(s => s.setRbTab);
  const [skillResult, setSkillResult] = useState<{ sessionId: string | null; skills: SkillMeta[] }>({ sessionId: null, skills: [] });
  const skills = skillResult.sessionId === sessionId ? skillResult.skills : [];
  const [detail, setDetail] = useState<{ sessionId: string | null; skill: SkillMeta } | null>(null);
  const [collapsed, setCollapsed] = useState<Set<CollapsibleSection>>(() => readCollapsedSections());

  useEffect(() => {
    if (!rightBarOpen || !visible) setDetail(null);
  }, [rightBarOpen, visible]);
  useEffect(() => { setDetail(null); }, [sessionId]);
  useEffect(() => {
    let current = true;
    if (sessionId) void ipc.listSkills(sessionId).then(result => {
      if (current) setSkillResult({ sessionId, skills: Array.isArray(result) ? result : [] });
    }).catch(() => {
      if (current) setSkillResult({ sessionId, skills: [] });
    });
    return () => { current = false; };
  }, [sessionId]);

  const project = tab?.projectId ? projects.find(p => p.id === tab.projectId) : null;
  const hasProject = !!tab?.projectId;
  const directory = hasProject ? project?.directory ?? tab?.workspace ?? "" : "";
  const sections: CollapsibleSection[] = [...(todos.length ? ["plan" as const] : []), ...(tab ? ["acceptance" as const] : []), "quota", "skills"];
  const openSections = sections.filter(id => !collapsed.has(id));
  const statuses = { running: 0, done: 0, attention: 0, error: 0 };
  for (const sub of subs) statuses[subStatus(sub)]++;
  const labels = {
    running: t("subagent.statusRunning"), done: t("subagent.statusDone"),
    attention: t("rightbar.subagentAttention"), error: t("rightbar.subagentError"),
  };
  const totals = changes.files?.reduce((sum, file) => ({ additions: sum.additions + file.additions, deletions: sum.deletions + file.deletions }), { additions: 0, deletions: 0 });
  const recentFiles = [...files].sort((a, b) => b.last_at.localeCompare(a.last_at)).slice(0, 3);

  return (
    <div className="rb-info-scroll">
      <Card className="rb-overview-card" role="region" aria-label={t("rightbar.overview")}>
        <div className="rb-section rb-project">
          <div className="rb-project-heading">
            <Typography.Title level={5} ellipsis={{ tooltip: true }} className="rb-project-name">
              {project?.name ?? (tab ? t("titlebar.tempSession") : "CodeWave")}
            </Typography.Title>
            {directory && <div className="rb-label-actions">
              <Button type="text" size="small" className="rb-open-dir-btn" icon={<FolderOpenOutlined />}
                title={t("rightbar.openInFileManager")} aria-label={t("rightbar.openInFileManager")}
                onClick={() => void ipc.openDir(directory).catch(error => message.error(`${t("rightbar.openDirFailed")}\n${String(error).replace(/^Error[:\s]*/i, "")}`))} />
              <OpenInEditorSelect dir={directory} />
            </div>}
          </div>
          {hasProject && <>
            <div className="rb-label">{t("rightbar.projectDir")}</div>
            <div className="rb-path" title={directory || undefined}>{directory || "—"}</div>
          </>}
          {tab && !hasProject && <div className="rb-dim">{t("rightbar.freeSession")}</div>}
        </div>

        <Tooltip title={changes.error || t("rightbar.viewChanges")}>
          <Button type="text" block className="rb-overview-changes" disabled={!sessionId} aria-label={t("rightbar.viewChanges")} onClick={() => setRbTab("changes")}>
            <ForkOutlined /><span className="rb-summary-title">{t("rightbar.tabChanges")}</span>
            <span className="rb-change-counts">
              {!sessionId ? t("rightbar.noChangesSession") : changes.loading ? <LoadingOutlined spin aria-label={t("rightbar.changesLoading")} />
                : changes.error ? <span className="rb-summary-error">{t("rightbar.changesUnavailable")}</span>
                  : totals ? (changes.files?.length === 0 ? t("rightbar.noChanges") : <><span className="rb-change-added">+{totals.additions}</span><span className="rb-change-deleted">−{totals.deletions}</span></>) : "—"}
            </span><RightOutlined />
          </Button>
        </Tooltip>

        <section className="rb-section rb-overview-agents" aria-label={t("rightbar.subagents")}>
          <div className="rb-section-heading"><span>{t("rightbar.subagents")}</span><Typography.Text type="secondary">{t("rightbar.subagentCount", { n: subs.length })}</Typography.Text></div>
          {subs.length === 0 ? <div className="rb-empty"><RobotOutlined /><Typography.Text type="secondary">{t("rightbar.noSubagents")}</Typography.Text></div> : <>
            <div className="rb-agent-summary">{Object.entries(statuses).filter(([, n]) => n > 0).map(([status, n]) => <span key={status} className={`rb-agent-status ${status}`}>{n} {labels[status as keyof typeof labels]}</span>)}</div>
            {subs.map(sub => {
              const status = subStatus(sub);
              const name = sub.name || sub.role;
              const statusText = sub.ended === "partial" ? t("subagent.endedPartial") : sub.ended === "budget" ? t("subagent.endedBudget") : sub.ended === "no_report" ? t("subagent.endedEarly") : labels[status];
              return <Tooltip key={sub.subId} title={[sub.description, statusText].filter(Boolean).join("\n")}>
                <Button type="text" block className="rb-overview-row" aria-label={t("rightbar.viewSubagent", { name })}
                  onClick={() => void useRun.getState().openSubDrawer(sessionId, sub.subId)}>
                  {status === "running" ? <LoadingOutlined spin /> : status === "done" ? <CheckOutlined /> : status === "error" ? <WarningOutlined /> : <RobotOutlined />}
                  <span className="rb-overview-name">{name}</span><span className={`rb-agent-status ${status}`}>{statusText}</span><RightOutlined />
                </Button>
              </Tooltip>;
            })}
          </>}
        </section>

        <section className="rb-section" aria-label={t("rightbar.sessionFiles")}>
          <div className="rb-section-heading"><span>{t("rightbar.sessionFiles")}</span><Typography.Text type="secondary">{t("rightbar.fileCount", { n: files.length })}</Typography.Text></div>
          {!sessionId ? <Typography.Text type="secondary">{t("files.noActiveSession")}</Typography.Text> : recentFiles.length === 0 ? <div className="rb-empty"><FileOutlined /><Typography.Text type="secondary">{t("files.emptySession")}</Typography.Text></div>
            : recentFiles.map(file => {
              const name = file.path.split(/[\\/]/).pop() || file.path;
              return <Tooltip key={file.path} title={file.path}>
                <Button type="text" block className="rb-overview-row" disabled={!file.exists} aria-label={t("rightbar.viewFile", { name })} onClick={() => onView(file.path)}>
                  <FileOutlined /><span className="rb-overview-name">{name}</span>{!file.exists && <Typography.Text type="secondary">{t("files.deleted")}</Typography.Text>}
                </Button>
              </Tooltip>;
            })}
          {sessionId && files.length > 0 && <Button type="text" size="small" className="rb-view-all" icon={<RightOutlined />} aria-label={t("rightbar.viewAll")} onClick={() => setRbTab("files")}>{t("rightbar.viewAll")}</Button>}
        </section>

        <QuotaSection visible={visible && openSections.includes("quota")} headerVisible={visible}>
          {(quotaActions, quotaContent) => (
            <Collapse ghost className="rb-collapse" expandIconPlacement="start" activeKey={openSections}
              onChange={keys => {
                const open = new Set(Array.isArray(keys) ? keys.map(String) : [String(keys)]);
                const next = new Set(collapsed);
                for (const id of sections) { if (open.has(id)) next.delete(id); else next.add(id); }
                setCollapsed(next);
                writeCollapsedSections(next);
              }}
              items={[
                ...(todos.length ? [{
                  key: "plan", label: t("rightbar.currentPlan"), extra: <Typography.Text type="secondary">{t("rightbar.planProgress", { done: todos.filter(todo => todo.status === "completed").length, total: todos.length })}</Typography.Text>,
                  children: todos.map((todo, i) => <div className="rb-todo" key={i}><span className={`rb-todo-dot ${todo.status}`}>{todo.status === "completed" ? <CheckOutlined /> : todo.status === "in_progress" ? "●" : "○"}</span><span className={todo.status === "completed" ? "rb-todo-done" : ""}>{todo.title}</span></div>),
                }] : []),
                // 交付验收（#16）：只读投影，无任何写入入口。它回答的是
                // 「能不能算交付通过」，与「运行是否结束」是两件事。
                ...(tab ? [{
                  key: "acceptance", label: t("acceptance.title"), forceRender: true,
                  children: <AcceptancePanel sessionId={sessionId} visible={visible} />,
                }] : []),
                { key: "quota", label: t("rightbar.quota"), extra: quotaActions, forceRender: true, children: quotaContent },
                { key: "skills", label: t("rightbar.skills"), extra: <Typography.Text type="secondary">{t("rightbar.skillCount", { n: skills.length })}</Typography.Text>, children: <>
                  {skills.length === 0 && <div className="rb-dim">{t("rightbar.noSkills")}</div>}
                  {skills.map(skill => <Tooltip key={skill.name} title={[skill.name, skill.description, skill.origin === "<builtin>" ? "" : skill.origin].filter(Boolean).join("\n")}>
                    <Button type="text" block className="rb-skill-line rb-skill-clickable" onClick={() => setDetail({ sessionId, skill })}>
                      <span className="rb-skill-name">{skill.name}</span><span className="rb-skill-origin">{skill.origin === "<builtin>" ? t("common.builtin") : originLabel(skill.origin)}</span>
                    </Button>
                  </Tooltip>)}
                </> },
              ]}
            />
          )}
        </QuotaSection>
      </Card>
      <SkillDetailModal skill={rightBarOpen && visible && detail?.sessionId === sessionId ? detail.skill : null} sessionId={sessionId} onClose={() => setDetail(null)} />
    </div>
  );
}
