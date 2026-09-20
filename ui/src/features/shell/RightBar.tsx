// 常驻右侧栏（[docs/session-artifacts-and-files-tab](../../../../docs/session-artifacts-and-files-tab.md)）：Tabs 四页签 [信息 | 日志 | 文件 | 变更]（[docs/rightbar-visual-batch](../../../../docs/rightbar-visual-batch.md) 调序，信息为默认页签）
// 变更 = 工作区 vs HEAD 的多根聚合 diff（原 GitDiffModal 面板化）；信息 = 项目目录 / 会话 / 技能 / 当前计划；
// 日志 = 会话黑盒 / 全局滚动日志（[docs/session-logging-report](../../../../docs/session-logging-report.md)）；文件 = 会话产物登记列表（create/edit 边车），点击弹窗查看
// 隐藏时保持挂载（[docs/sidebar-collapse-animation-and-titlebar-blend](../../../../docs/sidebar-collapse-animation-and-titlebar-blend.md)）：宽度经 0.2s 过渡收零（裁切式收缩）；visibility 待动画结束后才隐藏，
// 轮询 hook 均以 rightBarOpen 门控——隐藏即停轮询。顶栏切换按钮是唯一开合入口；
// rbTab 上收到 ui store 供外部切换（Composer 命令等）
import { useCallback, useEffect, useRef, useState } from "react";
import type { CSSProperties } from "react";
import { App as AntApp, Button, Collapse, Segmented, Select, Switch, Tabs } from "antd";
import { FolderOpenOutlined } from "@ant-design/icons";
import { useTranslation } from "react-i18next";
import { ipc } from "../../ipc/client";
import type { LogFileEntry, SkillMeta } from "../../ipc/types";
import { originLabel } from "../../utils/skills";
import {
  readCollapsedSections,
  writeCollapsedSections,
  type CollapsibleSection,
} from "../../utils/rightbarPrefs";
import { useActiveRun } from "../../stores/run";
import { useActiveTab, useSessions } from "../../stores/sessions";
import { useUi } from "../../stores/ui";
import FilesPanel from "../files/FilesPanel";
import FileViewerModal from "../files/FileViewerModal";
import { useSessionFiles } from "../files/useSessionFiles";
import QuotaSection from "../quota/QuotaSection";
import ChangesPanel from "../workspace/ChangesPanel";
import OpenInEditorSelect from "./OpenInEditorSelect";
import SkillDetailModal from "./SkillDetailModal";
import { useDisplayWidths } from "./useDisplayWidths";

/**
 * 信息页签：项目目录 / 额度与余额 / 技能 / 当前计划。
 * - 项目目录行：文件管理器按钮（Windows 自动识别 Files）+ 「在编辑器中打开」下拉；
 *   数据目录行已移除（`open_data_dir` 能力保留给「关于」弹框）；
 * - 额度与余额：顶替原「会话」段（开始时间信息已删）；行集合 = CodeWave 供应商配置，与凭证链无关；
 * - 技能 / 当前计划：antd Collapse，默认展开，折叠态记 localStorage（全局一份）。
 *
 * [docs/rightbar-info-refactor-and-subscription-quota](../../../../docs/rightbar-info-refactor-and-subscription-quota.md)
 */
function InfoPanel({ visible }: { visible: boolean }) {
  const { t } = useTranslation();
  const { message } = AntApp.useApp();
  const active = useActiveRun();
  const tab = useActiveTab();
  const projects = useSessions((s) => s.projects);
  // 当前会话工作区可用技能（复用设置面板同源 IPC，随会话切换刷新；显示序由后端 list 按来源分桶排好）
  const [skills, setSkills] = useState<SkillMeta[]>([]);
  // 技能详情弹层（点击技能行弹出；共享组件仅查看，无插入动作）
  const [detail, setDetail] = useState<SkillMeta | null>(null);
  const sessionId = tab?.sessionId ?? null;
  const rightBarOpen = useUi((s) => s.rightBarOpen);
  // 折叠段（技能/当前计划）：默认全展开，只记「已折叠」集合
  const [collapsedSections, setCollapsedSections] = useState<Set<CollapsibleSection>>(() =>
    readCollapsedSections(),
  );
  // 面板关闭时收起详情（其 Portal 渲染到 <body>，会逃出外壳的 visibility:hidden；与产物查看弹窗同一守卫）
  useEffect(() => {
    if (!rightBarOpen && detail) setDetail(null);
  }, [rightBarOpen, detail]);
  useEffect(() => {
    if (!sessionId) {
      setSkills([]);
      return;
    }
    void ipc
      .listSkills(sessionId)
      .then(setSkills)
      .catch(() => setSkills([]));
  }, [sessionId]);

  const project = tab?.projectId ? projects.find((p) => p.id === tab.projectId) : null;
  // 「项目目录」section 要打开的目标：项目会话 = 主目录；临时会话 = 工作区；无目标则不渲染图标按钮
  const dirToOpen = project?.directory ?? tab?.workspace ?? "";
  // 折叠段集合：计划段仅在当前会话有计划时参与（避免「折叠了不存在的段」）
  const allSections: CollapsibleSection[] = active.todos.length > 0 ? ["skills", "plan"] : ["skills"];
  const openSections = allSections.filter((id) => !collapsedSections.has(id));

  const openInFileManager = (dir: string) => {
    if (!dir) return;
    void ipc.openDir(dir).catch((e) => message.error(`${t("rightbar.openDirFailed")}\n${String(e).replace(/^Error[:\s]*/i, "")}`));
  };

  return (
    <div className="rb-info-scroll">
      {/* 项目主目录：标签行带文件管理器按钮 + 「在编辑器中打开」下拉（无检测到编辑器时不下拉） */}
      <div className="rb-section">
        <div className="rb-label-row">
          <div className="rb-label">{t("rightbar.projectDir")}</div>
          <div className="rb-label-actions">
            {dirToOpen && (
              <Button
                className="rb-open-dir-btn"
                type="text"
                size="small"
                icon={<FolderOpenOutlined />}
                title={t("rightbar.openInFileManager")}
                aria-label={t("rightbar.openInFileManager")}
                onClick={() => openInFileManager(dirToOpen)}
              />
            )}
            <OpenInEditorSelect dir={dirToOpen} />
          </div>
        </div>
        {project ? (
          <div className="rb-path" title={project.directory}>{project.directory}</div>
        ) : tab ? (
          <>
            <div className="rb-path">{tab.workspace}</div>
            <div className="rb-dim">{t("rightbar.freeSession")}</div>
          </>
        ) : (
          <div className="rb-dim">—</div>
        )}
      </div>

      {/* 额度与余额段（顶替原「会话」段） */}
      <QuotaSection visible={visible} />

      {/* 技能 / 当前计划：antd Collapse（ghost），默认全展开；折叠态写 localStorage（全局一份）。
          expandIconPosition="end"：标题文字与「项目目录 / 额度与余额」左对齐，箭头落在行末；
          标题字号/颜色由 app.css 的 .rb-collapse .ant-collapse-title 统一为 11px + dim。 */}
      <Collapse
        className="rb-collapse"
        ghost
        expandIconPosition="end"
        activeKey={openSections}
        onChange={(keys) => {
          const open = new Set(Array.isArray(keys) ? keys.map(String) : [String(keys)]);
          const next = new Set<CollapsibleSection>();
          for (const id of allSections) if (!open.has(id)) next.add(id);
          setCollapsedSections(next);
          writeCollapsedSections(next);
        }}
        items={[
          {
            key: "skills",
            label: t("rightbar.skills"),
            children: (
              <>
                {skills.length === 0 && <div className="rb-dim">{t("rightbar.noSkills")}</div>}
                {skills.map((s) => {
                  const builtin = s.origin === "<builtin>";
                  const label = originLabel(s.origin);
                  const tip = [s.description, builtin ? "" : s.origin].filter(Boolean).join("\n");
                  return (
                    <div
                      className="rb-skill-line rb-skill-clickable"
                      key={s.name}
                      title={tip || undefined}
                      onClick={() => setDetail(s)}
                    >
                      <span className="rb-skill-name">{s.name}</span>
                      {builtin ? (
                        <span className="rb-skill-origin">{t("common.builtin")}</span>
                      ) : (
                        label && <span className="rb-skill-origin" title={s.origin}>{label}</span>
                      )}
                    </div>
                  );
                })}
              </>
            ),
          },
          // 计划段仅在当前会话有计划 todos 时出现（与批次前行为一致）
          ...(active.todos.length > 0
            ? [
                {
                  key: "plan",
                  label: t("rightbar.currentPlan"),
                  children: (
                    <>
                      {active.todos.map((td, i) => (
                        <div className="rb-todo" key={i}>
                          <span className={`rb-todo-dot ${td.status}`}>
                            {td.status === "completed" ? "✓" : td.status === "in_progress" ? "●" : "○"}
                          </span>
                          <span className={td.status === "completed" ? "rb-todo-done" : ""}>{td.title}</span>
                        </div>
                      ))}
                    </>
                  ),
                },
              ]
            : []),
        ]}
      />
      <SkillDetailModal skill={detail} sessionId={sessionId} onClose={() => setDetail(null)} />
    </div>
  );
}
const TAIL_LINES = 300;
const POLL_MS = 2000;

/// 行级着色：同时兼容会话日志 `[WARN]` 与 tracing 文本 ` WARN ` 两种前缀
function lineClass(line: string): string {
  if (line.includes("[ERROR]") || /\bERROR\b/.test(line)) return "err";
  if (line.includes("[WARN]") || /\bWARN\b/.test(line)) return "warn";
  return "";
}

/** 日志页签（[docs/session-logging-report](../../../../docs/session-logging-report.md)）：会话视图（细粒度黑盒）/ 全局视图（tracing 滚动日志）。仅激活时轮询。 */
function LogPanel({ visible }: { visible: boolean }) {
  const { t } = useTranslation();
  const tab = useActiveTab();
  const [scope, setScope] = useState<"session" | "global">("session");
  const [auto, setAuto] = useState(true);
  const [files, setFiles] = useState<LogFileEntry[]>([]);
  const [fileName, setFileName] = useState<string | null>(null);
  const [content, setContent] = useState<string>("");
  const [truncated, setTruncated] = useState(false);
  const [error, setError] = useState<string>("");
  const preRef = useRef<HTMLPreElement>(null);
  const stickRef = useRef(true);
  // 过期响应守卫：切换会话/范围后，迟到的旧响应不得覆盖新视图
  const reqIdRef = useRef(0);
  const sessionId = tab?.sessionId ?? null;
  const projectId = tab?.projectId ?? null;

  const refresh = useCallback(async () => {
    const myId = ++reqIdRef.current;
    try {
      if (scope === "session") {
        if (!sessionId) return;
        const r = await ipc.readSessionLog(sessionId, projectId, TAIL_LINES);
        if (reqIdRef.current !== myId) return;
        setError("");
        setTruncated(r.truncated);
        setContent((prev) => (prev === r.content ? prev : r.content));
      } else {
        let name = fileName;
        if (!name) {
          const list = await ipc.listLogFiles();
          if (reqIdRef.current !== myId) return;
          setFiles(list);
          name = list[0]?.name ?? null;
          setFileName(name);
          if (!name) return;
        }
        const r = await ipc.readLogFile(name, TAIL_LINES);
        if (reqIdRef.current !== myId) return;
        setError("");
        setTruncated(r.truncated);
        setContent((prev) => (prev === r.content ? prev : r.content));
      }
    } catch (e) {
      if (reqIdRef.current !== myId) return;
      setError(String(e).replace(/^Error[:\s]*/i, ""));
      setContent("");
    }
  }, [scope, sessionId, projectId, fileName]);

  // 全局日志文件列表（切到全局视图时拉取；会话视图不用）
  useEffect(() => {
    if (visible && scope === "global" && files.length === 0) {
      void ipc.listLogFiles().then(setFiles).catch(() => setFiles([]));
    }
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [visible, scope]);

  // 手动刷新 + 自动轮询（仅页签激活时；依赖变化即重建定时器）
  useEffect(() => {
    if (!visible) return;
    void refresh();
    if (!auto) return;
    const t = setInterval(() => void refresh(), POLL_MS);
    return () => clearInterval(t);
  }, [visible, auto, refresh]);

  // 内容更新时贴底（用户上滚阅读时不再强行下拽）
  useEffect(() => {
    const el = preRef.current;
    if (el && stickRef.current) {
      el.scrollTop = el.scrollHeight;
    }
  }, [content]);

  return (
    <div className="rb-log-panel">
      <div className="rb-log-tools">
        <Segmented
          size="small"
          value={scope}
          onChange={(v) => {
            setScope(v as "session" | "global");
            setContent("");
            setError("");
            setFileName(null);
          }}
          options={[
            { label: t("rightbar.scopeSession"), value: "session" },
            { label: t("rightbar.scopeGlobal"), value: "global" },
          ]}
        />
        <Switch size="small" checked={auto} onChange={setAuto} checkedChildren={t("rightbar.auto")} unCheckedChildren={t("rightbar.manual")} />
        <Button size="small" onClick={() => void refresh()}>{t("rightbar.refresh")}</Button>
        <Button size="small" onClick={() => void ipc.openLogsDir().catch(() => {})}>{t("rightbar.openDir")}</Button>
      </div>
      {scope === "global" && (
        <Select
          size="small"
          style={{ width: "100%", marginTop: 4 }}
          placeholder={t("rightbar.noLogFiles")}
          value={fileName}
          onChange={(v) => {
            setFileName(v);
            setContent("");
          }}
          options={files.map((f) => ({
            label: t("rightbar.logFileKb", { name: f.name, kb: Math.max(1, Math.round(f.size / 1024)) }),
            value: f.name,
          }))}
        />
      )}
      {error && <div className="rb-dim rb-log-error">{error}</div>}
      <pre
        ref={preRef}
        className="rb-log"
        onScroll={(e) => {
          const el = e.currentTarget;
          stickRef.current = el.scrollTop + el.clientHeight >= el.scrollHeight - 24;
        }}
      >
        {content
          ? content.split("\n").map((line, i) => (
              <div key={i} className={lineClass(line)}>{line}</div>
            ))
          : scope === "session" && !sessionId
            ? t("rightbar.noActiveSession")
            : error
              ? ""
              : t("rightbar.noLogs")}
      </pre>
      {truncated && <div className="rb-dim">{t("rightbar.tailOnly", { n: TAIL_LINES })}</div>}
    </div>
  );
}

/** 常驻右侧栏根组件：四页签布局（信息/日志/文件/变更）+ 产物查看弹窗；折叠为裁切式收零动画、隐藏即停轮询。 */
export default function RightBar() {
  const { t } = useTranslation();
  const rightBarOpen = useUi((s) => s.rightBarOpen);
  const rbTab = useUi((s) => s.rbTab);
  const setRbTab = useUi((s) => s.setRbTab);
  // 右栏宽度 = 拖拽记忆值按窗口夹取后的显示值（与 AppShell 的分隔条同源；
  // CSS 变量必须写在这里，否则栏宽会一直停在默认 328）
  const { rightBar: rightBarWidth } = useDisplayWidths();
  const tab = useActiveTab();
  const sessionId = tab?.sessionId ?? null;
  // [docs/sidebar-collapse-animation-and-titlebar-blend](../../../../docs/sidebar-collapse-animation-and-titlebar-blend.md)：隐藏时传 null——面板关闭期间产物列表停止刷新
  const { files, refresh } = useSessionFiles(rightBarOpen ? sessionId : null);
  const [viewing, setViewing] = useState<string | null>(null);
  // 查看弹窗随面板一起关闭（评审 🟢2）：其 Portal 渲染到 <body>，会逃出外壳的 visibility:hidden
  // 悬浮在隐藏面板之上；[docs/titlebar-content-batch](../../../../docs/titlebar-content-batch.md) 的隐藏即卸载曾恰好掩盖了这一点。
  useEffect(() => {
    if (!rightBarOpen && viewing) setViewing(null);
  }, [rightBarOpen, viewing]);

  // [docs/sidebar-collapse-animation-and-titlebar-blend](../../../../docs/sidebar-collapse-animation-and-titlebar-blend.md)：隐藏时保持挂载——类名翻转为 .right-bar-closed（width/padding 经 0.2s 缓动收零、
  // visibility 待动画结束后隐藏）；顶栏切换按钮是唯一开合入口
  return (
    <div
      className={rightBarOpen ? "right-bar" : "right-bar right-bar-closed"}
      aria-hidden={!rightBarOpen}
      style={{ "--rb-width": `${rightBarWidth}px` } as CSSProperties}
    >
      <Tabs
        size="small"
        className="rb-tabs"
        activeKey={rbTab}
        onChange={setRbTab}
        items={[
          { key: "info", label: t("rightbar.tabInfo"), children: <InfoPanel visible={rightBarOpen && rbTab === "info"} /> },
          { key: "logs", label: t("rightbar.tabLogs"), children: <LogPanel visible={rightBarOpen && rbTab === "logs"} /> },
          {
            key: "files",
            label: files.length ? t("rightbar.tabFilesWithCount", { n: files.length }) : t("rightbar.tabFiles"),
            children: (
              <FilesPanel sessionId={sessionId} files={files} onView={setViewing} onRefresh={refresh} />
            ),
          },
          { key: "changes", label: t("rightbar.tabChanges"), children: <ChangesPanel visible={rightBarOpen && rbTab === "changes"} /> },
        ]}
      />
      <FileViewerModal sessionId={sessionId} path={viewing} onClose={() => setViewing(null)} />
    </div>
  );
}
