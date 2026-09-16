// 左侧导航：项目 -> 会话（分组头 [项目∨ | 管理⠿ | 新建+]、会话行、显示更多、行内操作）+ 任务区
// 具名项目（注册表）为主分组；临时会话（project_id=None）其后按目录归组
import { useEffect, useMemo, useState } from "react";
import { App, Button, Dropdown, Empty, Input, Modal, Popconfirm, Tag } from "antd";
import {
  CaretDownOutlined,
  DeleteOutlined,
  EditOutlined,
  FolderOutlined,
  CopyOutlined,
  HolderOutlined,
  LoadingOutlined,
  PlusOutlined,
  WarningOutlined,
} from "@ant-design/icons";
import { useTranslation } from "react-i18next";
import { i18n } from "../../i18n";
import { ipc } from "../../ipc/client";
import type { ProjectEntry, SessionMeta } from "../../ipc/types";
import { MANAGED_DIR_NAME } from "../../utils/path";
import { useSessions } from "../../stores/sessions";
import { useRun } from "../../stores/run";
import { useUi } from "../../stores/ui";

const PREVIEW_COUNT = 5;

function relTime(iso: string): string {
  const t = Date.parse(iso);
  if (Number.isNaN(t)) return "";
  const m = Math.floor((Date.now() - t) / 60_000);
  if (m < 1) return i18n.t("nav.justNow");
  if (m < 60) return i18n.t("nav.minutes", { n: m });
  const h = Math.floor(m / 60);
  if (h < 24) return i18n.t("nav.hours", { n: h });
  return i18n.t("nav.days", { n: Math.floor(h / 24) });
}

// 左栏会话排序：按最后活跃时间倒序；未活跃会话（新建、尚未落检查点、updated_at 为空）以创建时间兜底 -> 新会话置顶
function navOrder(a: SessionMeta, b: SessionMeta): number {
  return (b.updated_at || b.created_at || "").localeCompare(a.updated_at || a.created_at || "");
}

function dirName(p: string): string {
  return p.split("/").filter(Boolean).pop() || p;
}

interface NavGroup {
  key: string;
  name: string;
  title: string;
  /** 新会话落点：具名项目 -> projectId；临时会话分组 -> null（行内 + 禁用） */
  projectId: string | null;
  sessions: SessionMeta[];
}

/** 左侧导航：临时会话区 + 项目→会话树（分组头/会话行/显示更多/行内操作）+ 任务区；
 *  含项目新建/编辑弹窗、会话重命名弹窗、项目管理弹窗与删除级联确认。 */
export default function ProjectNav() {
  const { t } = useTranslation();
  const { message, modal } = App.useApp();
  const tabs = useSessions((s) => s.tabs);
  const activeKey = useSessions((s) => s.activeKey);
  const sessionList = useSessions((s) => s.sessions);
  const projects = useSessions((s) => s.projects);
  const tasksOpen = useUi((s) => s.tasksOpen);
  // 左栏展开/项目区折叠态住在 useUi store，不用组件 useState：hydrate（AppShell 里异步读盘后回填）晚于本组件首渲染，
  // 只在挂载时读一次模块内存值永远拿不到恢复值；订阅 store 才能被 hydrate 的那一刻 setState 唤起重渲染
  // （会话保存与恢复优化 · 批1）。组件内不留第二份副本，避免双事实源
  const expanded = useUi((s) => s.treeExpand);
  const collapsed = useUi((s) => s.treeCollapsed);
  const [tasks, setTasks] = useState<{ id: string; name: string; last_status: string | null }[]>([]);
  // 新建/编辑项目弹窗
  const [editing, setEditing] = useState<ProjectEntry | null>(null);
  const [modalOpen, setModalOpen] = useState(false);
  const [projName, setProjName] = useState("");
  const [projDir, setProjDir] = useState<string | null>(null);
  const [savingProject, setSavingProject] = useState(false);
  // 会话重命名弹窗（缺陷修复：window.prompt 在 Tauri WKWebView 返回 null，按钮看似失效）
  const [renameTarget, setRenameTarget] = useState<SessionMeta | null>(null);
  const [renameTitle, setRenameTitle] = useState("");
  const [renaming, setRenaming] = useState(false);
  // 管理弹窗
  const [manageOpen, setManageOpen] = useState(false);

  // 空态「新建项目」引导：ChatMessages 经 ui store 标志触发弹窗（用后即重置）
  const createProjectRequested = useUi((s) => s.createProjectRequested);
  useEffect(() => {
    if (createProjectRequested) {
      useUi.setState({ createProjectRequested: false });
      openCreate();
    }
  }, [createProjectRequested]);

  const allSessions = useMemo(() => {
    const byId = new Map(sessionList.map((s) => [s.id, s]));
    for (const t of tabs) {
      if (!byId.has(t.sessionId)) {
        byId.set(t.sessionId, {
          id: t.sessionId,
          title: t.title,
          workspace: t.workspace,
          model_id: null,
          // 新建会话尚未落检查点：创建时间取 Tab.createdAt；最后活跃留空——
          // 行内时间列留白（创建时间不冒充活跃）；置顶由 navOrder 的创建时间兜底完成
          created_at: t.createdAt,
          updated_at: "",
          message_count: 0,
          project_id: t.projectId,
          roots: [t.workspace],
          // 批1 新增字段：Tab 内运行态另有来源（useRun.tabs），会话行徽标只认列表 meta；
          // 未落检查点的新会话必然没有中断标记
          running: false,
          interrupted: null,
        });
      }
    }
    return [...byId.values()];
  }, [sessionList, tabs]);

  // 临时会话（project_id=null）单独分区；具名项目严格按 project_id 归组
  const tempSessions = useMemo(
    () =>
      allSessions
        .filter((s) => !s.project_id)
        .sort(navOrder),
    [allSessions],
  );

  const groups = useMemo<NavGroup[]>(() => {
    const out: NavGroup[] = [];
    for (const p of projects) {
      const members = allSessions.filter((s) => s.project_id === p.id);
      out.push({
        key: `proj:${p.id}`,
        name: p.name,
        title: p.directory,
        projectId: p.id,
        sessions: [...members].sort(navOrder),
      });
    }
    return out;
  }, [projects, allSessions]);

  // 任务列表：任务中心开合或活跃会话变化时刷新（M-3：副作用归 useEffect，不进 useMemo）
  useEffect(() => {
    const sessionId = useSessions.getState().activeKey;
    if (!sessionId) {
      setTasks([]);
      return;
    }
    void ipc
      .listScheduledTasks(sessionId)
      .then((list) => setTasks(list.map((t) => ({ id: t.id, name: t.name, last_status: t.last_status }))))
      .catch(() => setTasks([]));
  }, [tasksOpen, activeKey]);

  function openCreate() {
    setEditing(null);
    setProjName("");
    setProjDir(null);
    setModalOpen(true);
  }

  function openEdit(p: ProjectEntry) {
    setEditing(p);
    setProjName(p.name);
    setProjDir(p.directory ?? null);
    setModalOpen(true);
  }

  async function pickDirectory() {
    const dir = await ipc.selectWorkspaceDir();
    if (dir) setProjDir(dir);
  }

  async function saveProject() {
    if (!projName.trim() || !projDir) return;
    setSavingProject(true);
    try {
      const entry: ProjectEntry = {
        id: editing?.id ?? crypto.randomUUID(),
        name: projName.trim(),
        directory: projDir,
        // 新项目数据目录 = <主目录>/.codewave（需求 1.5）；既有项目保留原值
        data_dir: editing?.data_dir ?? `${projDir}/${MANAGED_DIR_NAME}`,
        created_at: editing?.created_at ?? new Date().toISOString(),
      };
      if (editing) {
        await useSessions.getState().updateProject(entry);
      } else {
        await useSessions.getState().addProject(entry);
      }
      message.success(t("nav.saved"));
      setModalOpen(false);
    } catch (e) {
      message.error(String(e));
    } finally {
      setSavingProject(false);
    }
  }

  function confirmDeleteProject(p: ProjectEntry) {
    const count = allSessions.filter((s) => s.project_id === p.id).length;
    modal.confirm({
      title: t("nav.deleteProjectTitle", { name: p.name }),
      okText: t("nav.delete"),
      okButtonProps: { danger: true },
      content: (
        <div style={{ fontSize: 12.5, display: "grid", gap: 6 }}>
          <div>{t("nav.deleteProjectData")}</div>
          <div>{t("nav.deleteProjectSessions", { n: count })}</div>
          <div>{t("nav.deleteProjectCodeSafe")}</div>
        </div>
      ),
      onOk: async () => {
        const n = await useSessions.getState().deleteProject(p.id);
        message.success(t("nav.projectDeleted", { n }));
      },
    });
  }

  function openRename(meta: SessionMeta) {
    setRenameTarget(meta);
    setRenameTitle(meta.title);
  }

  async function submitRename() {
    const title = renameTitle.trim();
    if (!renameTarget || !title || renaming) return;
    setRenaming(true);
    try {
      await useSessions.getState().rename(renameTarget, title);
      useUi.getState().toast(t("nav.saved"));
      setRenameTarget(null);
    } catch (e) {
      // 失败反馈：弹框保持打开便于修正重试，不再静默
      message.error(t("nav.renameFailed", { error: String(e) }));
    } finally {
      setRenaming(false);
    }
  }

  const renderGroup = (g: NavGroup) => {
    const showAll = !!expanded[g.key];
    const visible = showAll ? g.sessions : g.sessions.slice(0, PREVIEW_COUNT);
    return (
      <div className="project-group" key={g.key}>
        <div className="project-row" title={g.title}>
          <FolderOutlined style={{ color: "var(--ws-dim)" }} />
          <span className="project-name">{g.name}</span>
          {g.projectId && (
            <Button
              className="row-action"
              type="text"
              size="small"
              title={t("nav.newSessionInProject")}
              icon={<PlusOutlined />}
              onClick={() => void useSessions.getState().openProjectSession(g.projectId!)}
            />
          )}
        </div>
        <div className="project-sessions">
          {visible.map((s) => (
            <SessionRow
              key={s.id}
              meta={s}
              active={s.id === activeKey}
              onOpen={() => void useSessions.getState().openSession(s)}
              onRename={() => openRename(s)}
              onRemove={() => void useSessions.getState().removeSession(s)}
            />
          ))}
          {g.sessions.length > PREVIEW_COUNT && (
            <div
              className="show-more"
              onClick={() => useUi.getState().setTreeGroupExpanded(g.key, !showAll)}
            >
              {showAll ? t("nav.showLess") : t("nav.showMore")}
            </div>
          )}
        </div>
      </div>
    );
  };

  return (
    <div className="project-nav">
      <div className="nav-header">
        <span className="nav-header-title">{t("nav.tempSessions")}</span>
        <span className="nav-header-actions">
          <Button
            type="text"
            size="small"
            title={t("nav.newTempSessionTip")}
            icon={<PlusOutlined />}
            onClick={() => void useSessions.getState().openFreeSession()}
          />
        </span>
      </div>
      {tempSessions.map((s) => (
        <SessionRow
          key={s.id}
          meta={s}
          active={s.id === activeKey}
          onOpen={() => void useSessions.getState().openSession(s)}
          onRename={() => openRename(s)}
          onRemove={() => void useSessions.getState().removeSession(s)}
        />
      ))}

      <div className="nav-header" style={{ marginTop: 14 }}>
        <span className="nav-header-title" onClick={() => useUi.getState().setTreeCollapsed(!collapsed)}>
          {t("nav.projects")}
          <CaretDownOutlined className={`caret${collapsed ? " collapsed" : ""}`} />
        </span>
        <span className="nav-header-actions">
          <Button type="text" size="small" title={t("nav.manageProjects")} icon={<HolderOutlined />} onClick={() => setManageOpen(true)} />
          <Button type="text" size="small" title={t("nav.newProject")} icon={<PlusOutlined />} onClick={openCreate} />
        </span>
      </div>
      {!collapsed && groups.map(renderGroup)}
      {!collapsed && groups.length === 0 && <div className="nav-empty">{t("nav.noProjects")}</div>}

      <div className="nav-section-title" style={{ marginTop: 14 }}>{t("nav.tasks")}</div>
      {tasks.length === 0 && <div className="nav-empty">{t("nav.noTasks")}</div>}
      {tasks.map((t) => (
        <div className="task-nav-row" key={t.id} onClick={() => useUi.setState({ tasksOpen: true })}>
          <span className={`task-dot${t.last_status === "ok" ? " ok" : t.last_status ? " err" : ""}`} />
          <span className="task-name">{t.name}</span>
        </div>
      ))}

      <Modal
        open={modalOpen}
        title={editing ? t("nav.editProject") : t("nav.newProject")}
        onCancel={() => setModalOpen(false)}
        footer={
          <div style={{ display: "flex", justifyContent: "flex-end", gap: 10 }}>
            <Button onClick={() => setModalOpen(false)}>{t("nav.cancel")}</Button>
            <Button
              type="primary"
              loading={savingProject}
              disabled={!projName.trim() || !projDir}
              onClick={() => void saveProject()}
            >
              {t("nav.save")}
            </Button>
          </div>
        }
      >
        <div className="proj-form">
          <div className="proj-field">
            <span className="proj-label">{t("nav.projectName")}</span>
            <Input
              value={projName}
              placeholder={t("nav.projectNamePh")}
              onChange={(e) => setProjName(e.target.value)}
            />
          </div>
          <div className="proj-field">
            <span className="proj-label">{t("nav.projectDir")}</span>
            <div className="proj-dirs">
              {projDir ? (
                <Tag closable onClose={() => setProjDir(null)}>
                  {projDir}
                </Tag>
              ) : null}
              <Button size="small" icon={<PlusOutlined />} onClick={() => void pickDirectory()}>
                {t("nav.chooseDir")}
              </Button>
            </div>
          </div>        </div>
      </Modal>

      <Modal
        open={renameTarget != null}
        title={t("nav.renameSession")}
        onCancel={() => setRenameTarget(null)}
        confirmLoading={renaming}
        okText={t("nav.save")}
        okButtonProps={{ disabled: !renameTitle.trim() }}
        onOk={() => void submitRename()}
      >
        <Input
          value={renameTitle}
          autoFocus
          onFocus={(e) => e.target.select()}
          onPressEnter={() => void submitRename()}
          onChange={(e) => setRenameTitle(e.target.value)}
          placeholder={t("nav.sessionNamePh")}
        />
      </Modal>

      <Modal open={manageOpen} title={t("nav.manageProjects")} onCancel={() => setManageOpen(false)} footer={null}>
        {projects.length === 0 && <Empty description={t("nav.noProjectsEmpty")} />}
        {projects.map((p) => (
          <div className="manage-row" key={p.id}>
            <div className="manage-info">
              <b>{p.name}</b>
              <div className="dim" style={{ fontSize: 11.5 }}>{p.directory}</div>
            </div>
            <Button
              size="small"
              type="text"
              title={t("nav.editProjectTip")}
              icon={<EditOutlined />}
              onClick={() => openEdit(p)}
            />
            <Button
              size="small"
              type="text"
              danger
              title={t("nav.deleteProject")}
              icon={<DeleteOutlined />}
              onClick={() => confirmDeleteProject(p)}
            />
          </div>
        ))}
      </Modal>
    </div>
  );
}

/** 单行会话条目：运行中 spinner / 未读点 / 等待确认徽标三态槽位 + 中断标记 + 行内时间 + 悬停行内操作（重命名/删除）。 */
function SessionRow({
  meta,
  active,
  onOpen,
  onRename,
  onRemove,
}: {
  meta: SessionMeta;
  active: boolean;
  onOpen(): void;
  onRename(): void;
  onRemove(): void;
}) {
  // 行级订阅：仅本会话的 running/ask-pending 状态变化才重渲染
  const { t } = useTranslation();
  const running = useRun((s) => !!s.tabs[meta.id]?.running);
  const askPending = useRun((s) => !!s.tabs[meta.id]?.ask);
  // [docs/ask-ink-accent-and-composer-cover](../../../../docs/ask-ink-accent-and-composer-cover.md)：会话结束未读点（run done/error 且用户不在时置位；进入时由 store 订阅清除）
  const unread = useSessions((s) => !!s.unread[meta.id]);
  const { message } = App.useApp();
  // 复制会话 ID：右键菜单入口，写剪贴板后轻反馈（与 ChatMessages doCopy 同模式）
  const copyId = () => {
    void navigator.clipboard
      ?.writeText(meta.id)
      .then(() => message.success(t("nav.copiedSessionId")))
      .catch(() => {});
  };

  // 清除中断标记（会话保存与恢复优化 · 批1）：后端落盘清掉后本地就地撤徽标——
  // 该字段不是任何聚合的输入，没必要为一次点击再拉一遍会话列表（省一次往返与闪烁）
  const clearInterrupt = async () => {
    try {
      await ipc.clearSessionInterrupt(meta.id);
      useSessions.setState((s) => ({
        sessions: s.sessions.map((m) => (m.id === meta.id ? { ...m, interrupted: null } : m)),
      }));
      message.success(t("nav.interruptCleared"));
    } catch (e) {
      message.error(t("nav.clearInterruptFailed", { error: String(e) }));
    }
  };

  return (
    <Dropdown
      trigger={["contextMenu"]}
      menu={{
        items: [{ key: "copy-session-id", icon: <CopyOutlined />, label: t("nav.copySessionId"), onClick: copyId }],
      }}
    >
      <div
        className={`session-nav-row${active ? " active" : ""}`}
        onClick={onOpen}
        onContextMenu={(e) => e.stopPropagation()}
      >
      <span className="session-run-slot">
        {running ? <LoadingOutlined spin /> : unread ? <span className="session-unread-dot" title={t("nav.unreadReply")} /> : null}
      </span>
      {/* 上次运行被中断（崩溃 / 退出前中止）：橙 = 需注意档（无彩色 = 默认、红 = 危险）。
          放行首而非时间槽旁：.row-actions 是覆盖在时间槽上的绝对定位悬停层（还带不透明底色），
          徽标放那里会被裁掉一半、点击也会落到重命名按钮上。徽标本身即清除入口，title 写明语义 + 动作 */}
      {meta.interrupted && (
        <Button
          className="session-interrupt"
          type="text"
          size="small"
          title={`${t(meta.interrupted.kind === "crash" ? "nav.interruptedCrash" : "nav.interruptedQuit")} · ${t("nav.clearInterrupt")}`}
          icon={<WarningOutlined />}
          style={{ color: "var(--ws-warn)", flex: "none" }}
          onClick={(e) => {
            e.stopPropagation(); // 点徽标只清标记，不切会话
            void clearInterrupt();
          }}
        />
      )}
      <span className="session-title" title={meta.title}>{meta.title || "(untitled)"}</span>
      {askPending && (
        <Tag color="success" style={{ marginLeft: 6, flex: "none", fontSize: 10, lineHeight: "16px" }}>
          {t("nav.waitingConfirm")}
        </Tag>
      )}
      <span className="session-time">{relTime(meta.updated_at)}</span>
      {/* 等待确认期间行操作不可用：按钮悬停层与徽标同槽位、会相互叠压（docs/ask-ink-accent-and-composer-cover） */}
      {!askPending && (
        <span className="row-actions" onClick={(e) => e.stopPropagation()}>
          <Button className="row-action" type="text" size="small" title={t("sessions.rename")} icon={<EditOutlined />} onClick={onRename} />
          <Popconfirm title={t("nav.deleteSessionConfirm")} onConfirm={onRemove}>
            <Button className="row-action" type="text" size="small" danger title={t("sessions.delete")} icon={<DeleteOutlined />} />
          </Popconfirm>
        </span>
      )}
      </div>
    </Dropdown>
  );
}
