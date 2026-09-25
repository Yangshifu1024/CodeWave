// 计划任务独立页（[docs/tasks-module-polish]）：与设置页同范式（绝对定位全屏覆盖 + 页内 Esc + 返回工作区），
// 取代原 TaskCenterPanel 弹框；「计划」从手写表达式改成周期选择器（表达式由 schedulePreset 生成，仍留自定义入口）。
//
// 分工（本页只做渲染与交互）：
//   · 列表数据的**单一来源**是 stores/tasks（scheduled:fired/done 也由它消费）——本页不自己 invoke 列表、
//     不订阅事件、不弹运行结果提示（用户已拍板静默）；
//   · 暂停开关/编辑/立即运行/删除改成命令后把返回的 ScheduledTask 交给 upsertLocal（免整表刷新）；
//   · 三态分明：loading / error（保留旧列表）/ 空列表。
import { useEffect, useRef, useState } from "react";
import type { KeyboardEvent as ReactKeyboardEvent } from "react";
import { App, Button, Card, Empty, Form, Input, InputNumber, Modal, Popconfirm, Select, Spin, Switch, Tag } from "antd";
import { ArrowLeftOutlined, DeleteOutlined, DownOutlined, EditOutlined, PlayCircleOutlined, PlusOutlined } from "@ant-design/icons";
import { useTranslation } from "react-i18next";
import { ipc } from "../../ipc/client";
import type { ScheduledTask } from "../../ipc/types";
import { fullscreenNavWidth } from "../../utils/layout";
import { useDisplayWidths } from "../shell/useDisplayWidths";
import { useActiveId, useSessions } from "../../stores/sessions";
import { statusKind, statusLabel, useTasks } from "../../stores/tasks";
import { useUi } from "../../stores/ui";
import {
  EVERY_LIMITS, describeExpr, exprToPreset, formatDateTimeLocal, parseDateTimeLocal, presetToExpr,
  type EveryUnit, type SchedulePreset,
} from "./schedulePreset";

const { TextArea } = Input;

/** 执行历史最多渲染条数（后端也只留最近 20 条；这里再兜一次，防旧数据异常膨胀） */
const HISTORY_LIMIT = 20;

/** 新建任务时的默认钟点（09:00）：最常见的意图，省一次输入 */
const DEFAULT_TIME = "09:00";

/** 一次性任务的默认时刻：一小时后（秒归零，免得表达式带上无意义的秒） */
function defaultOnceAt(): Date {
  const d = new Date(Date.now() + 60 * 60 * 1000);
  d.setSeconds(0, 0);
  return d;
}

/** ISO / 可解析时间串 → 本地 `YYYY-MM-DD HH:mm`；解析不出原样回显（坏数据不该让整行消失） */
function fmtLocal(value: string): string {
  const d = new Date(value);
  if (Number.isNaN(d.getTime())) return value;
  const pad = (n: number) => String(n).padStart(2, "0");
  return `${d.getFullYear()}-${pad(d.getMonth() + 1)}-${pad(d.getDate())} ${pad(d.getHours())}:${pad(d.getMinutes())}`;
}

/**
 * 是否有 antd 浮层打开：Esc 优先让位给组件库（下拉/气泡/抽屉），不平级返回工作区。
 * 弹窗（Modal）不在这里判——关闭后的 Modal 仍留在 DOM 里，靠 DOM 探测会永远挡住 Esc；
 * 它由 editor 状态单独把关（editorOpenRef）。
 */
function overlayOpen(): boolean {
  if (typeof document === "undefined") return false;
  return !!document.querySelector(
    ".ant-select-dropdown:not(.ant-select-dropdown-hidden), .ant-dropdown:not(.ant-dropdown-hidden)," +
      " .ant-popover:not(.ant-popover-hidden), .ant-drawer:not(.ant-drawer-hidden)",
  );
}

/** 弹窗内的编辑态（新建与编辑共用一份；id = null 表示新建） */
interface EditorState {
  id: string | null;
  name: string;
  instruction: string;
  preset: SchedulePreset;
  /** 一次性任务的**原始输入串**：`<input type="datetime-local">` 可以处在空/半截状态，Date 装不下 */
  onceText: string;
}

/** 取当前周期里的时间串（切周期时沿用，免得用户每换一种都要重填钟点） */
function presetTime(p: SchedulePreset): string {
  return p.kind === "daily" || p.kind === "weekly" || p.kind === "monthly" ? p.time : DEFAULT_TIME;
}

/** 编辑态 → 计划表达式（once 以输入串为准；非法时返回 ""，由 canSave 拦下） */
function editorExpr(ed: EditorState): string {
  if (ed.preset.kind === "once") {
    const at = parseDateTimeLocal(ed.onceText);
    return at ? presetToExpr({ kind: "once", at }) : "";
  }
  return presetToExpr(ed.preset);
}

/** 计划任务全屏页：任务列表 + 周期选择器 + 执行历史 */
export default function TasksPage() {
  const { t } = useTranslation();
  const { message } = App.useApp();

  // 单一数据源（拉取与事件消费都在 store）
  const items = useTasks((s) => s.items);
  const loading = useTasks((s) => s.loading);
  const error = useTasks((s) => s.error);
  const runningIds = useTasks((s) => s.runningIds);
  const load = useTasks((s) => s.load);
  const upsertLocal = useTasks((s) => s.upsertLocal);
  const removeLocal = useTasks((s) => s.removeLocal);

  // 归属判定：只有「有会话且该会话属于某个项目」时新建才可能落盘（后端按会话推导 project_id）
  const sessionId = useActiveId();
  const projectId = useSessions((s) => s.tabs.find((x) => x.key === s.activeKey)?.projectId ?? null);
  const projects = useSessions((s) => s.projects);
  /** 项目注册表是否读盘失败（true = projects 不可信，不能拿它判「项目已不存在」） */
  const projectsLoadFailed = useSessions((s) => s.projectsLoadFailed);
  const canPersist = !!sessionId && !!projectId;

  // 左栏宽：与设置页左栏同一函数（utils/layout 的 fullscreenNavWidth）—— 两页左侧同宽才能视觉连续
  const { windowWidth } = useDisplayWidths();
  const navWidth = fullscreenNavWidth(windowWidth);

  const [expandedId, setExpandedId] = useState<string | null>(null);
  const [editor, setEditor] = useState<EditorState | null>(null);
  const [saving, setSaving] = useState(false);
  const pendingTaskActions = useRef(new Set<string>());
  const [pendingTaskActionKeys, setPendingTaskActionKeys] = useState<string[]>([]);
  /** 保存失败的后端原文（就地展示在弹窗里，不许吞成静默失败） */
  const [saveError, setSaveError] = useState<string | null>(null);

  /** 弹窗开合的最新值：Esc 监听只注册一次，靠 ref 取（不随 editor 重挂） */
  const editorOpenRef = useRef(false);
  useEffect(() => {
    editorOpenRef.current = editor !== null;
  });

  // 打开页面即拉一次全局列表（store 是唯一入口；失败只置 error，旧列表保留）
  useEffect(() => {
    void load();
  }, [load]);

  // 页内 Esc：捕获阶段注册并 preventDefault，抢在 AppShell 的全局 Esc（停止运行中会话）之前收口。
  // 弹窗/浮层打开时让位给组件库，不平级返回。
  useEffect(() => {
    const onKeydown = (e: KeyboardEvent) => {
      if (e.key !== "Escape" || e.isComposing) return;
      if (editorOpenRef.current || overlayOpen()) return;
      e.preventDefault();
      useUi.setState({ tasksOpen: false });
    };
    window.addEventListener("keydown", onKeydown, true);
    return () => window.removeEventListener("keydown", onKeydown, true);
  }, []);

  function close() {
    useUi.setState({ tasksOpen: false });
  }

  /** 任务的项目归属：后端 ScheduledTask 未声明 project_id 时按「未归属」显示（不硬造字段） */
  function projectLabel(task: ScheduledTask): string {
    // 用 `in` 窄化而不是类型断言 / any：字段真的从后端来了就能读到，没来就当未归属
    const raw = "project_id" in task ? task.project_id : null;
    const pid = typeof raw === "string" && raw ? raw : null;
    if (!pid) return t("tasks.unattributed");
    const hit = projects.find((p) => p.id === pid);
    if (hit) return hit.name;
    // 注册表读盘失败时**不能**宣称「项目已不存在」——那是把「没读到」说成「被删了」，
    // 用户会据此以为任务悬空（projectMissing 只在注册表可信且确实查无此 id 时用）。
    return projectsLoadFailed ? t("tasks.projectUnknown") : t("tasks.projectMissing");
  }

  /**
   * 下次触发的展示：已暂停 > 具体时刻 > 一次性且已过（不再触发）> 从未运行 > 未知（—）。
   * 顺序即优先级：暂停态优先于一切（用户刚点过开关，这是最该看到的反馈）；
   * `once:` 任务执行完（或被跳过）后后端会清空 next_run，此时「不再触发」才是它的真实语义。
   */
  function nextRunText(task: ScheduledTask): string {
    if (!task.enabled) return t("tasks.paused");
    if (task.next_run) return fmtLocal(task.next_run);
    if (task.schedule.trim().startsWith("once:")) return t("tasks.noMoreRuns");
    if (task.last_status == null) return t("tasks.neverRun");
    return "—";
  }

  async function toggleEnabled(task: ScheduledTask, enabled: boolean) {
    const key = `toggle:${task.id}`;
    if (pendingTaskActions.current.has(key)) return;
    pendingTaskActions.current.add(key);
    setPendingTaskActionKeys([...pendingTaskActions.current]);
    try {
      upsertLocal(await ipc.setScheduledTaskEnabled(task.id, enabled));
    } catch (e) {
      message.error(t("tasks.saveFailed", { msg: String(e) }));
    } finally {
      pendingTaskActions.current.delete(key);
      setPendingTaskActionKeys([...pendingTaskActions.current]);
    }
  }

  async function runNow(id: string) {
    const key = `run:${id}`;
    if (pendingTaskActions.current.has(key)) return;
    pendingTaskActions.current.add(key);
    setPendingTaskActionKeys([...pendingTaskActions.current]);
    try {
      // 手工触发是异步的：命令只表示「已开始」，行的「运行中」标记由 scheduled:fired 事件驱动
      await ipc.runScheduledTaskNow(id);
    } catch (e) {
      message.error(t("tasks.runRejected", { msg: String(e) }));
    } finally {
      pendingTaskActions.current.delete(key);
      setPendingTaskActionKeys([...pendingTaskActions.current]);
    }
  }

  async function remove(task: ScheduledTask) {
    try {
      // 删除仍带会话参数（后端按会话归属校验）；无会话时传空串，由后端自己拒绝
      await ipc.deleteScheduledTask(sessionId ?? "", task.id);
      removeLocal(task.id);
    } catch (e) {
      // 失败**不摘行**（任务还在，用户可原样重试），但重新对齐一次后端真相：
      // 失败原因也可能是「已被别处删掉」，本地那份就成了幽灵行——拉一次列表收口。
      message.error(t("tasks.deleteFailed", { msg: String(e) }));
      void load();
    }
  }

  function openCreate() {
    setSaveError(null);
    setEditor({ id: null, name: "", instruction: "", preset: { kind: "daily", time: DEFAULT_TIME }, onceText: "" });
  }

  function openEdit(task: ScheduledTask) {
    setSaveError(null);
    const preset = exprToPreset(task.schedule);
    setEditor({
      id: task.id,
      name: task.name,
      instruction: task.instruction,
      preset,
      onceText: preset.kind === "once" ? formatDateTimeLocal(preset.at) : "",
    });
  }

  function updateEditor(fn: (ed: EditorState) => EditorState) {
    setEditor((cur) => (cur ? fn(cur) : cur));
  }

  /** 切周期：能沿用的参数（钟点 / 星期 / 日期）尽量带过去，不能沿用的给默认值 */
  function changeKind(kind: SchedulePreset["kind"]) {
    updateEditor((ed) => {
      const time = presetTime(ed.preset);
      switch (kind) {
        case "daily":
          return { ...ed, preset: { kind, time } };
        case "weekly":
          return { ...ed, preset: { kind, dows: ed.preset.kind === "weekly" ? ed.preset.dows : [1], time } };
        case "monthly":
          return { ...ed, preset: { kind, day: ed.preset.kind === "monthly" ? ed.preset.day : 1, time } };
        case "every":
          return { ...ed, preset: ed.preset.kind === "every" ? ed.preset : { kind, n: 30, unit: "m" } };
        case "once": {
          const at = defaultOnceAt();
          return { ...ed, preset: { kind, at }, onceText: formatDateTimeLocal(at) };
        }
        case "custom":
          return { ...ed, preset: { kind, expr: ed.preset.kind === "custom" ? ed.preset.expr : editorExpr(ed) } };
      }
    });
  }

  function setTime(time: string) {
    updateEditor((ed) => {
      if (ed.preset.kind === "daily") return { ...ed, preset: { kind: "daily", time } };
      if (ed.preset.kind === "weekly") return { ...ed, preset: { kind: "weekly", dows: ed.preset.dows, time } };
      if (ed.preset.kind === "monthly") return { ...ed, preset: { kind: "monthly", day: ed.preset.day, time } };
      return ed;
    });
  }

  function setDows(dows: number[]) {
    updateEditor((ed) =>
      ed.preset.kind === "weekly" ? { ...ed, preset: { kind: "weekly", dows, time: ed.preset.time } } : ed,
    );
  }

  function setMonthDay(day: number) {
    updateEditor((ed) =>
      ed.preset.kind === "monthly" ? { ...ed, preset: { kind: "monthly", day, time: ed.preset.time } } : ed,
    );
  }

  function setEveryN(n: number) {
    updateEditor((ed) => (ed.preset.kind === "every" ? { ...ed, preset: { kind: "every", n, unit: ed.preset.unit } } : ed));
  }

  function setEveryUnit(unit: EveryUnit) {
    updateEditor((ed) => (ed.preset.kind === "every" ? { ...ed, preset: { kind: "every", n: ed.preset.n, unit } } : ed));
  }

  /** 一次性任务：输入串是权威源；能解析出来就同步进 preset（表达式按它算） */
  function setOnceText(value: string) {
    updateEditor((ed) => {
      const at = parseDateTimeLocal(value);
      return at ? { ...ed, onceText: value, preset: { kind: "once", at } } : { ...ed, onceText: value };
    });
  }

  function setCustomExpr(expr: string) {
    updateEditor((ed) => ({ ...ed, preset: { kind: "custom", expr } }));
  }

  const preset = editor?.preset ?? null;

  /** 每隔周期越界（越界就地提示且不许保存——超限表达式后端会拒） */
  const everyOverLimit =
    preset?.kind === "every" && (preset.n < 1 || preset.n > EVERY_LIMITS[preset.unit]);

  const canSave =
    !!editor &&
    editor.name.trim() !== "" &&
    editor.instruction.trim() !== "" &&
    !everyOverLimit &&
    editorExpr(editor) !== "";

  async function save() {
    if (!editor || !canSave) return;
    const expr = editorExpr(editor);
    setSaving(true);
    setSaveError(null);
    try {
      const saved = editor.id
        ? await ipc.updateScheduledTask(editor.id, editor.name.trim(), editor.instruction.trim(), expr)
        : await ipc.createScheduledTask(sessionId ?? "", editor.name.trim(), editor.instruction.trim(), expr);
      upsertLocal(saved);
      setEditor(null);
    } catch (e) {
      // 后端错误原文（含「已有任务正在运行」这类拒绝）就地展示：用户要照它改输入
      setSaveError(String(e));
    } finally {
      setSaving(false);
    }
  }

  function expand(task: ScheduledTask) {
    setExpandedId((cur) => (cur === task.id ? null : task.id));
  }

  const periodOptions = [
    { value: "daily", label: t("tasks.periodDaily") },
    { value: "weekly", label: t("tasks.periodWeekly") },
    { value: "monthly", label: t("tasks.periodMonthly") },
    { value: "every", label: t("tasks.periodEvery") },
    { value: "once", label: t("tasks.periodOnce") },
    { value: "custom", label: t("tasks.periodCustom") },
  ];

  const weekdayOptions = [
    { value: 1, label: t("tasks.weekdayMon") },
    { value: 2, label: t("tasks.weekdayTue") },
    { value: 3, label: t("tasks.weekdayWed") },
    { value: 4, label: t("tasks.weekdayThu") },
    { value: 5, label: t("tasks.weekdayFri") },
    { value: 6, label: t("tasks.weekdaySat") },
    { value: 0, label: t("tasks.weekdaySun") },
  ];

  return (
    // 全屏 dialog 语义（与设置页同范式）：绝对定位贴在内层 Layout 上，盖住工作区但不卸载它
    <div className="tasks-shell" data-testid="tasks-page" role="dialog" aria-modal="true" aria-label={t("tasks.title")}>
      {/* 左栏：与设置页左栏同度量同背景（--ws-bg-nav）——返回工作区 / 标题 / 新建都在这里。
          宽与设置页左栏逐像素一致（同一个 fullscreenNavWidth）；工作区左栏另有一套夹取规则，不保证等宽 */}
      <nav className="tasks-nav" style={{ width: navWidth }}>
        <div className="tasks-nav-head">
          <Button type="text" block className="tasks-nav-back" icon={<ArrowLeftOutlined />} onClick={close}>
            {t("tasks.backToWorkspace")}
          </Button>
        </div>
        <div className="tasks-nav-title">{t("tasks.title")}</div>
        <div className="tasks-nav-actions">
          {/* 禁用原因**就地**写在按钮旁（与设置页同一范式，不藏在 Tooltip 里） */}
          {!canPersist && (
            <span className="tasks-nav-note hint">
              {sessionId ? t("tasks.freeSessionNoPersist") : t("tasks.needProjectSession")}
            </span>
          )}
          <Button type="primary" block icon={<PlusOutlined />} disabled={!canPersist} onClick={openCreate}>
            {t("tasks.newTask")}
          </Button>
        </div>
      </nav>

      <main className="tasks-body">
        <div className="tasks-body-inner">
        <header className="tasks-page-header">
          <h1>{t("tasks.title")}</h1>
          <p>{t("tasks.unattendedApprovalHint")}</p>
        </header>
        {/* 加载失败：错误行常驻 + 旧列表照旧显示（一次瞬时失败不该把列表擦成空态） */}
        {error && <div className="tasks-error">{t("tasks.loadFailed", { msg: error })}</div>}

        {loading && items.length === 0 && (
          <div className="tasks-loading">
            <Spin size="small" />
          </div>
        )}

        {!loading && !error && items.length === 0 && (
          <Empty className="tasks-empty" description={t("tasks.empty")}>
            <Button type="primary" icon={<PlusOutlined />} disabled={!canPersist} onClick={openCreate}>
              {t("tasks.newTask")}
            </Button>
          </Empty>
        )}

        {items.map((task) => {
          const kind = statusKind(task.last_status);
          const running = runningIds.includes(task.id);
          const expanded = expandedId === task.id;
          const toggling = pendingTaskActionKeys.includes(`toggle:${task.id}`);
          const starting = pendingTaskActionKeys.includes(`run:${task.id}`);
          return (
            <Card
              className="task-row"
              key={task.id}
              variant="outlined"
              title={
                <div className="task-row-heading">
                  <strong className="task-row-name" title={task.name}>{task.name}</strong>
                  {/* 色彩只映射风险：正常收尾为中性，失败为红。 */}
                  {running && <Tag>{t("tasks.running")}</Tag>}
                  {kind !== "none" && (
                    <Tag color={kind === "error" ? "error" : undefined}>
                      {statusLabel(task.last_status, t)}
                    </Tag>
                  )}
                </div>
              }
              extra={
                <span className="task-row-enabled">
                  <span>{task.enabled ? t("tasks.enabled") : t("tasks.paused")}</span>
                  <Switch
                    size="small"
                    checked={task.enabled}
                    loading={toggling}
                    disabled={toggling}
                    aria-label={task.enabled ? t("tasks.pause") : t("tasks.resume")}
                    onChange={(next) => void toggleEnabled(task, next)}
                  />
                </span>
              }
              actions={[
                <Button key="run" type="text" block loading={starting} disabled={starting} icon={<PlayCircleOutlined />} onClick={() => void runNow(task.id)}>
                  {t("tasks.runNow")}
                </Button>,
                <Button key="edit" type="text" block icon={<EditOutlined />} onClick={() => openEdit(task)}>
                  {t("tasks.edit")}
                </Button>,
                <Popconfirm key="delete" title={`${t("common.delete")}?`} okButtonProps={{ danger: true }} onConfirm={() => void remove(task)}>
                  <Button type="text" block danger icon={<DeleteOutlined />}>
                    {t("common.delete")}
                  </Button>
                </Popconfirm>,
              ]}
            >
              {/* 主信息区只负责展开历史；开关与操作都在其外，避免交互嵌套。 */}
              <div
                className="task-row-main"
                role="button"
                tabIndex={0}
                aria-expanded={expanded}
                aria-label={`${task.name} · ${t("tasks.history")}`}
                onClick={() => expand(task)}
                onKeyDown={(e: ReactKeyboardEvent<HTMLDivElement>) => {
                  if (e.key === "Enter" || e.key === " ") {
                    e.preventDefault();
                    expand(task);
                  }
                }}
              >
                <p className="task-row-instruction" title={task.instruction}>{task.instruction}</p>
                <div className="task-row-meta">
                  <div><span>{t("tasks.period")}</span><strong className="task-row-sched">{describeExpr(task.schedule, t)}</strong></div>
                  <div><span>{t("tasks.nextRun")}</span><strong className="task-row-next" title={nextRunText(task)}>{nextRunText(task)}</strong></div>
                  <div><span>{t("tasks.project")}</span><strong className="task-row-project" title={projectLabel(task)}>{projectLabel(task)}</strong></div>
                </div>
                <span className="task-row-history-toggle">
                  {t("tasks.history")} <DownOutlined rotate={expanded ? 180 : 0} />
                </span>
              </div>
              {expanded && (
                <div className="tasks-history">
                  {task.runs.length === 0 ? (
                    <div className="hint">{t("tasks.historyEmpty")}</div>
                  ) : (
                    task.runs.slice(0, HISTORY_LIMIT).map((run, i) => {
                      const runKind = statusKind(run.status);
                      return (
                        <div className="tasks-history-row" key={`${run.at}-${i}`}>
                          <span className="tasks-history-at">{fmtLocal(run.at)}</span>
                          <span
                            className={
                              runKind === "error"
                                ? "tasks-history-status tasks-history-status-error"
                                : "tasks-history-status"
                            }
                          >
                            {statusLabel(run.status, t)}
                          </span>
                          <span className="tasks-history-source dim">
                            {run.source === "manual" ? t("tasks.sourceManual") : t("tasks.sourceSchedule")}
                          </span>
                          <span className="tasks-history-tokens dim">{t("tasks.tokens", { n: run.out_tokens })}</span>
                          {run.summary && <div className="tasks-history-summary">{run.summary}</div>}
                        </div>
                      );
                    })
                  )}
                </div>
              )}
            </Card>
          );
        })}
        </div>
      </main>

      {/* 新建 / 编辑：周期选择器 + 指令；保存失败把后端原文展示在弹窗内 */}
      <Modal
        open={!!editor}
        title={editor?.id ? t("tasks.editTask") : t("tasks.newTask")}
        onCancel={() => {
          setEditor(null);
          setSaveError(null);
        }}
        onOk={() => void save()}
        confirmLoading={saving}
        okButtonProps={{ disabled: !canSave }}
        okText={t("common.save")}
        cancelText={t("common.cancel")}
        width={680}
        className="tasks-editor-modal"
      >
        {editor && preset && (
          <Form className="tasks-form" layout="vertical" colon={false}>
            <section className="tasks-form-section" aria-label={t("tasks.detailsSection")}>
              <h3>{t("tasks.detailsSection")}</h3>
            <Form.Item label={t("tasks.name")} htmlFor="task-name">
              <Input
                id="task-name"
                value={editor.name}
                placeholder={t("tasks.name")}
                onChange={(e) => updateEditor((ed) => ({ ...ed, name: e.target.value }))}
              />
            </Form.Item>
            <Form.Item label={t("tasks.instruction")} htmlFor="task-instruction">
              <TextArea
                id="task-instruction"
                rows={4}
                value={editor.instruction}
                placeholder={t("tasks.instruction")}
                onChange={(e) => updateEditor((ed) => ({ ...ed, instruction: e.target.value }))}
              />
            </Form.Item>
            </section>

            <section className="tasks-form-section" aria-label={t("tasks.scheduleSection")}>
              <h3>{t("tasks.scheduleSection")}</h3>
              <div className="tasks-schedule-grid">

            <Form.Item label={t("tasks.period")} htmlFor="task-period">
              <Select
                id="task-period"
                aria-label={t("tasks.period")}
                value={preset.kind}
                options={periodOptions}
                onChange={changeKind}
              />
            </Form.Item>

            {/* 周期参数按类型渲染：每天/每周/每月共用时间控件，自定义回落原表达式输入 */}
            {preset.kind === "daily" || preset.kind === "weekly" || preset.kind === "monthly" ? (
              <Form.Item label={t("tasks.time")} htmlFor="task-time">
                <Input
                  id="task-time"
                  type="time"
                  aria-label={t("tasks.time")}
                  value={preset.time}
                  onChange={(e) => setTime(e.target.value)}
                />
              </Form.Item>
            ) : null}

            {preset.kind === "weekly" && (
              <Form.Item label={t("tasks.weekday")} htmlFor="task-weekday">
                <Select
                  id="task-weekday"
                  mode="multiple"
                  aria-label={t("tasks.weekday")}
                  value={preset.dows}
                  options={weekdayOptions}
                  onChange={(values: number[]) => setDows(values)}
                />
              </Form.Item>
            )}

            {preset.kind === "monthly" && (
              <Form.Item label={t("tasks.monthDay")} htmlFor="task-monthday">
                <InputNumber
                  id="task-monthday"
                  aria-label={t("tasks.monthDay")}
                  min={1}
                  max={31}
                  value={preset.day}
                  onChange={(v) => setMonthDay(typeof v === "number" ? v : 1)}
                />
              </Form.Item>
            )}

            {preset.kind === "every" && (
              <Form.Item label={t("tasks.periodEvery")} htmlFor="task-interval">
                <div className="tasks-interval">
                  <InputNumber
                    id="task-interval"
                    aria-label={t("tasks.periodEvery")}
                    min={1}
                    max={EVERY_LIMITS[preset.unit]}
                    value={preset.n}
                    onChange={(v) => setEveryN(typeof v === "number" ? v : 1)}
                  />
                  <Select
                    aria-label={t("tasks.intervalUnitMin")}
                    value={preset.unit}
                    options={[
                      { value: "m", label: t("tasks.intervalUnitMin") },
                      { value: "h", label: t("tasks.intervalUnitHour") },
                      { value: "d", label: t("tasks.intervalUnitDay") },
                    ]}
                    onChange={(unit: EveryUnit) => setEveryUnit(unit)}
                  />
                </div>
              </Form.Item>
            )}

            {preset.kind === "once" && (
              <Form.Item label={t("tasks.periodOnce")} htmlFor="task-once">
                <Input
                  id="task-once"
                  type="datetime-local"
                  aria-label={t("tasks.periodOnce")}
                  value={editor.onceText}
                  onChange={(e) => setOnceText(e.target.value)}
                />
              </Form.Item>
            )}

            {preset.kind === "custom" && (
              <Form.Item label={t("tasks.periodCustom")} htmlFor="task-expr">
                <Input
                  id="task-expr"
                  aria-label={t("tasks.periodCustom")}
                  value={preset.expr}
                  placeholder="cron:0 9 * * *"
                  onChange={(e) => setCustomExpr(e.target.value)}
                />
              </Form.Item>
            )}
              </div>

            {/* 语法说明常驻（原面板的教训：只当 placeholder 的话敲第一个字符就看不见了） */}
            <div className="tasks-schedule-hint">
              <span className="hint">{t("tasks.scheduleHint")}</span>
              {everyOverLimit && <span className="tasks-field-error">{t("tasks.intervalTooLarge")}</span>}
            </div>
            </section>

            {!canSave && <div className="hint">{t("tasks.createDisabled")}</div>}
            {saveError && <div className="tasks-error">{t("tasks.saveFailed", { msg: saveError })}</div>}
          </Form>
        )}
      </Modal>
    </div>
  );
}
