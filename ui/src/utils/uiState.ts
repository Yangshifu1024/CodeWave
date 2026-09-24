// ui-state：会话现场态的落盘与恢复（会话保存与恢复优化 · 批1）。
// 磁盘文件 `~/.codewave/ui-state.json`（后端只校验顶层 schema === 1 与体积上限 ≤ 8MB，JSON 结构归前端所有）。
//
// 持久化边界（[docs/technical-design] 之外的批1 约定，见 plan.md §三）：
//   进 ui-state：Tab 集合/顺序/活跃 Tab/活跃项目、窗口几何、滚动锚点、草稿、前端队列、
//                会话树展开态与未读集合、面板级 UI 态（子代理抽屉/todos/suggestions）
//   留在 localStorage：主题/语言/左右栏开合 —— 首屏同步可得，改走 IPC 会闪一帧；两类不双写
//   例外（2026-09-19）：字体偏好 localStorage 降级为「首帧缓存」，真源在后端 config.ui.font_sans/font_mono
//   （修「输了界面字体却没写进去」的缺陷），启动时按后端对账——见 utils/fonts.ts::reconcileFontsFromConfig
//
// 模块级单例：内存里有「锚点表 / 关 Tab 后保留的内容 / 最近一次窗口几何」三类易变态，
// 由 store 订阅与各写入口驱动 1.2s 防抖落盘（scheduleFlush，另有一枚不重置的 2s 最长等待计时器兜住流式持续输出），
// 退出前强制 flushNow。
// 左栏树展开/折叠态的事实源在 useUi store（不是本模块内存）：ProjectNav 首渲染早于异步 hydrate，
// 组件只在挂载时读一次内存值就永远拿不到恢复值；订阅 store 才能被 hydrate 的 setState 唤起重渲染。
// 窗口几何一律写**逻辑像素**：后端用 set_size(LogicalSize)/set_position(LogicalPosition) 还原。
import { ipc } from "../ipc/client";
import { DEFAULT_PREFS } from "../ipc/types";
import type { SessionPrefs, Todo } from "../ipc/types";
import { useRun } from "../stores/run";
import type { PendingImage, QueueItem } from "../stores/run.types";
import { useSessions } from "../stores/sessions";
import { useUi } from "../stores/ui";
import type { ScrollAnchor } from "./scrollAnchor";

/** 落盘 schema 版本（后端校验字段）；结构变更必须同步递增并自行处理旧版本降级 */
export const UI_STATE_SCHEMA = 1;
/** 变更 → 落盘防抖（1.2 秒：崩溃/强杀最多丢 1–2 秒内的现场态变化） */
export const FLUSH_DEBOUNCE_MS = 1200;
/** 连续变更下的最长等待（2 秒）：纯防抖在流式持续输出时会被无限推迟（每帧都重置计时器 → 一次不落盘），
 *  这枚不重置的计时器保证现场态最多滞后 2 秒落盘（耐久线「最多丢 1–2 秒」） */
export const FLUSH_MAX_WAIT_MS = 2000;
/** 滚动 → 记录锚点的防抖（比落盘快一档：切 Tab / 恢复要立刻拿到最新锚点） */
export const ANCHOR_DEBOUNCE_MS = 200;
/** 草稿/队列图片的落盘预算（base64 体积大；超出则只保留文本，避免顶到后端 8MB 上限） */
export const MAX_IMAGE_PAYLOAD_CHARS = 3_000_000;
/** 写盘失败提示节流（连续失败不刷屏） */
const FAIL_REPORT_INTERVAL_MS = 30_000;

/** 持久化的 Tab 骨架（消息本身由 load_session 按需拉取，不进 ui-state） */
export interface UiTabSnapshot {
  workspace: string;
  title: string;
  projectId: string | null;
  createdAt: string;
  prefs: SessionPrefs;
}

/** 面板级 UI 态（会随会话恢复，不含流式内容） */
export interface UiPanels {
  subDrawer?: { open: boolean; subId: string | null };
  todos?: Todo[];
  suggestions?: string[];
}

/** ui-state.json 结构（顶层键固定，后端按 schema 校验） */
export interface UiState {
  schema: number;
  /** Tab 集合：顺序 + 活跃 Tab + 各 Tab 骨架元数据 */
  tabs: { order: string[]; activeKey: string | null; items: Record<string, UiTabSnapshot> };
  /** 活跃 Tab 所属项目（activeKey 失效时用于回落到同项目邻位 Tab） */
  activeProject: string | null;
  /** 窗口几何（**逻辑像素**，与后端 LogicalSize/LogicalPosition 同单位）；
   *  读取失败且从未读到过 → null；位置缺省时省略 x/y（后端据此只还原尺寸、位置回落主屏居中） */
  window: { width: number; height: number; x?: number; y?: number } | null;
  /** 滚动锚点（key = sessionId） */
  scrollAnchors: Record<string, ScrollAnchor>;
  /** 草稿（key = sessionId；dataUrl 不落盘，恢复时按 mime+data 重建；refs 为后加字段，旧快照缺省按空处理） */
  drafts: Record<string, { text: string; images: { id: string; name: string; mime: string; data: string }[]; refs?: string[] }>;
  /** 前端排队消息（key = sessionId） */
  queue: Record<string, { id: string; text: string; images?: { mime: string; data: string }[] }[]>;
  /** 左栏状态：会话树展开/项目区折叠/未读集合（未读语义 = 恢复上次的集合，不是启动全标未读） */
  tree: { expanded: Record<string, boolean>; collapsed: boolean; unread: Record<string, boolean> };
  /** 面板级 UI 态（key = sessionId） */
  panels: Record<string, UiPanels>;
}

/** 关 Tab 时选择「保留草稿」后的驻留内容（Tab 已关、内存桶已删，内容先寄存在这里等重开时回填） */
interface RetainedContent {
  draft?: ComposerDraftSnapshot;
  queue?: QueueItem[];
  panels?: UiPanels;
}
interface ComposerDraftSnapshot {
  text: string;
  images: { id: string; name: string; mime: string; data: string }[];
  /** 文件引用 chip（[docs/composer-file-ref-chips](../../../docs/composer-file-ref-chips.md)）；旧快照可能缺字段 */
  refs?: string[];
}

interface Memory {
  loaded: UiState | null;
  anchors: Map<string, ScrollAnchor>;
  retained: Record<string, RetainedContent>;
  /** 最近一次成功读到的窗口几何（读取失败时回退用它，避免一次失败就把已记下的几何抹成 null） */
  lastWindow: UiState["window"];
}

function freshMemory(): Memory {
  return {
    loaded: null,
    anchors: new Map(),
    retained: {},
    lastWindow: null,
  };
}

let memory: Memory = freshMemory();
let flushTimer: ReturnType<typeof setTimeout> | null = null;
/** 最长等待计时器（首个变更挂上、不随后续变更重置） */
let maxWaitTimer: ReturnType<typeof setTimeout> | null = null;
/** 在途落盘（并发 flush 串行化的锁） */
let flushing: Promise<void> | null = null;
/** 在途落盘期间又有变更 → 落盘循环结束后补写一次 */
let flushPending = false;
let anchoring = false;
/** 滚动锚点防抖窗口的计时器（必须留句柄）：窗口的语义是「先挂起、过 ANCHOR_DEBOUNCE_MS 再读 DOM」，
 *  挂窗口的那个实例被卸载后它照样会触发——此时 reader 读到的是一个空容器，会把「贴底」写进那个会话
 *  （生产里切 Tab/关 Tab 后仍写底是兼容行为，但 reset 必须能把它清掉：否则上一轮挂着的窗口会漏进
 *  下一个场景，把刚记下的锚点踩成贴底 —— 见 reset 的注释） */
let anchorTimer: ReturnType<typeof setTimeout> | null = null;
/** 最近一次成功写入的序列化结果（内容未变则跳过写盘，避免空转 I/O） */
let lastWritten = "";
let lastFailAt = 0;
let subscribed = false;

// ---------- 校验与读取 ----------

function isRecord(v: unknown): v is Record<string, any> {
  return typeof v === "object" && v !== null && !Array.isArray(v);
}

/** 窗口几何归一：宽/高必须为正有限数（与后端 `window_geometry` 判据一致，缺失/类型错/非正值一律当没有）；
 *  x/y 必须成对且有限，否则视为「未记录位置」（后端只还原尺寸并回落主屏居中） */
function normalizeGeometry(node: Record<string, any>): UiState["window"] {
  const { width, height } = node;
  if (typeof width !== "number" || typeof height !== "number") return null;
  if (!Number.isFinite(width) || !Number.isFinite(height) || width <= 0 || height <= 0) return null;
  const hasPos =
    typeof node.x === "number" &&
    typeof node.y === "number" &&
    Number.isFinite(node.x) &&
    Number.isFinite(node.y);
  return hasPos ? { width, height, x: node.x, y: node.y } : { width, height };
}

/** 结构归一：schema 不匹配或顶层结构损坏 ⇒ null（降级为无快照启动，不抛错、不报错）；
 *  子结构缺失一律按空处理（后端只守 schema 与体积，向前兼容靠这里兜） */
export function normalizeUiState(raw: unknown): UiState | null {
  if (!isRecord(raw) || raw.schema !== UI_STATE_SCHEMA) return null;
  const tabs = isRecord(raw.tabs) ? raw.tabs : null;
  if (!tabs || !Array.isArray(tabs.order) || !isRecord(tabs.items)) return null;
  const items: Record<string, UiTabSnapshot> = {};
  for (const key of tabs.order) {
    const it = tabs.items[key];
    if (typeof key !== "string" || !isRecord(it) || typeof it.workspace !== "string") continue;
    items[key] = {
      workspace: it.workspace,
      title: typeof it.title === "string" ? it.title : "",
      projectId: typeof it.projectId === "string" ? it.projectId : null,
      createdAt: typeof it.createdAt === "string" ? it.createdAt : "",
      prefs: isRecord(it.prefs) ? (it.prefs as SessionPrefs) : { ...DEFAULT_PREFS },
    };
  }
  const tree = isRecord(raw.tree) ? raw.tree : {};
  const win = isRecord(raw.window) ? raw.window : null;
  return {
    schema: UI_STATE_SCHEMA,
    tabs: {
      order: tabs.order.filter((k: unknown): k is string => typeof k === "string" && !!items[k]),
      activeKey: typeof tabs.activeKey === "string" ? tabs.activeKey : null,
      items,
    },
    activeProject: typeof raw.activeProject === "string" ? raw.activeProject : null,
    window: win ? normalizeGeometry(win) : null,
    scrollAnchors: isRecord(raw.scrollAnchors) ? (raw.scrollAnchors as Record<string, ScrollAnchor>) : {},
    drafts: isRecord(raw.drafts) ? (raw.drafts as UiState["drafts"]) : {},
    queue: isRecord(raw.queue) ? (raw.queue as UiState["queue"]) : {},
    tree: {
      expanded: isRecord(tree.expanded) ? tree.expanded : {},
      collapsed: tree.collapsed === true,
      unread: isRecord(tree.unread) ? tree.unread : {},
    },
    panels: isRecord(raw.panels) ? (raw.panels as Record<string, UiPanels>) : {},
  };
}

/** 启动时读一次 ui-state：读失败或结构损坏 ⇒ null（无快照启动）。
 *  草稿/队列/面板态先落进驻留表（retained），按 Tab 打开时机回填，保证「未开的 Tab 也有草稿」 */
export async function loadUiState(): Promise<UiState | null> {
  let raw: unknown;
  try {
    raw = await ipc.getUiState();
  } catch {
    raw = null; // 读失败不阻断启动
  }
  const state = normalizeUiState(raw);
  memory.loaded = state;
  memory.anchors = new Map();
  memory.retained = {};
  if (!state) return null;
  for (const [sid, a] of Object.entries(state.scrollAnchors)) {
    if (a && (a.kind === "bottom" || a.kind === "item")) memory.anchors.set(sid, a);
  }
  for (const [sid, d] of Object.entries(state.drafts)) {
    memory.retained[sid] = { ...memory.retained[sid], draft: d };
  }
  for (const [sid, q] of Object.entries(state.queue)) {
    if (Array.isArray(q) && q.length) memory.retained[sid] = { ...memory.retained[sid], queue: q };
  }
  for (const [sid, p] of Object.entries(state.panels)) {
    memory.retained[sid] = { ...memory.retained[sid], panels: p };
  }
  return state;
}

/** 把快照落到各 store：Tab 骨架（含失效引用剔除）→ 未读集合 → 草稿/队列/面板态。
 *  返回活跃 Tab key（供调用方惰性加载），无快照/全被剔除时为 null；
 *  有快照时取 restoreTabs 裁决后的 store 值（含「同项目邻位 → 全局邻位 → 首个」回落） */
export function applyUiStateToStores(): { activeKey: string | null; kept: string[] } {
  const st = memory.loaded;
  if (!st) return { activeKey: null, kept: [] };
  const sessions = useSessions.getState();
  const kept = sessions.restoreTabs({
    tabs: st.tabs.order.map((key) => ({ key, ...st.tabs.items[key] })),
    activeKey: st.tabs.activeKey,
    activeProject: st.activeProject,
    unread: st.tree.unread,
  });
  // 左栏树展开/折叠态推进 useUi store：ProjectNav 订阅的是 store，这一步 setState 会立刻唤起已挂载的左栏重渲染。
  // 这正是「不能把组件内 useState 换成模块内存读」的原因——hydrate 在本函数（异步读盘之后）才发生，
  // 首渲染时快照还没到货；只有 store 才能把「后到货的值」推给已挂载的组件
  useUi.setState({ treeExpand: { ...st.tree.expanded }, treeCollapsed: st.tree.collapsed });
  // 内容回填：只有活下来的 Tab 才回填（被剔除的 Tab 其草稿一并丢弃，不留孤儿）
  for (const key of kept) applyRetainedContent(key);
  // 活跃 Tab 以 restoreTabs 的裁决为准（它含「同项目邻位优先 → 全局邻位 → 首个」的回落逻辑）：
  // 这里自行回落 kept[0] 会与 store 实际值不一致，会把调用方的急加载引到错的 Tab 上
  return { activeKey: useSessions.getState().activeKey, kept };
}

// ---------- 快照构建 ----------

function toPendingImages(imgs: ComposerDraftSnapshot["images"]): PendingImage[] {
  return imgs.map((im) => ({
    id: im.id,
    name: im.name,
    mime: im.mime,
    data: im.data,
    // 缩略预览按 mime+data 重建（落盘不带 dataUrl，省一份 base64 体积）
    dataUrl: `data:${im.mime};base64,${im.data}`,
  }));
}

/** 某会话的「当前内容」：驻留表优先（Tab 已关但用户选择保留），否则读运行态分桶 */
function contentOf(sessionId: string): RetainedContent {
  const kept = memory.retained[sessionId];
  if (kept) return kept;
  const run = useRun.getState();
  const draft = run.drafts[sessionId];
  const t = run.tabs[sessionId];
  const out: RetainedContent = {};
  if (draft && (draft.text !== "" || draft.images.length || (draft.refs?.length ?? 0) > 0)) {
    out.draft = {
      text: draft.text,
      images: draft.images.map((im) => ({ id: im.id, name: im.name, mime: im.mime, data: im.data })),
      refs: draft.refs ?? [],
    };
  }
  if (t?.queue.length) out.queue = t.queue;
  if (t && (t.subDrawer.open || t.todos.length || t.suggestions.length)) {
    out.panels = {
      subDrawer: t.subDrawer.open ? { open: true, subId: t.subDrawer.subId } : undefined,
      todos: t.todos.length ? t.todos : undefined,
      suggestions: t.suggestions.length ? t.suggestions : undefined,
    };
  }
  return out;
}

/** 读窗口几何（**逻辑像素**）：仅在 flush 时读取。
 *  单位是关键：后端用 `set_size(LogicalSize)` / `set_position(LogicalPosition)` 还原，而 Tauri 的
 *  `outerPosition()/innerSize()` 返回**物理像素**——HiDPI（macOS 2x、Windows 125%/150%）下不除以 scale
 *  就会写出两倍尺寸与错位坐标，后端只会把它钳到工作区、或判越界后回落主屏居中（几何恢复等于失效）。
 *  `set_size` 落到内尺寸、`set_position` 落到外框左上角，故取 innerSize + outerPosition 与后端成对。
 *  失败（无 Tauri 运行时/命令不可用/窗口最小化时尺寸为 0）⇒ 回退上一次成功读取的几何，从未读到过才 null：
 *  绝不写入非法几何（后端会当没有），也不因一次读取失败抹掉已记下的几何 */
async function readWindowGeometry(): Promise<UiState["window"]> {
  try {
    const { getCurrentWindow } = await import("@tauri-apps/api/window");
    const win = getCurrentWindow();
    const [scaleRaw, pos, size] = await Promise.all([
      win.scaleFactor(),
      win.outerPosition(),
      win.innerSize(),
    ]);
    const scale = Number.isFinite(scaleRaw) && scaleRaw > 0 ? scaleRaw : 1;
    const geometry = normalizeGeometry({
      width: Math.round(size.width / scale),
      height: Math.round(size.height / scale),
      x: Math.round(pos.x / scale),
      y: Math.round(pos.y / scale),
    });
    if (!geometry) return memory.lastWindow;
    memory.lastWindow = geometry;
    return geometry;
  } catch {
    return memory.lastWindow;
  }
}

/** 图片超预算则整批丢弃（只保留文本）——后端 8MB 上限内以文本为主，图片可由用户重新添加 */
function pruneImages(state: UiState): void {
  let chars = 0;
  for (const d of Object.values(state.drafts)) for (const im of d.images) chars += im.data.length;
  for (const q of Object.values(state.queue)) for (const it of q) for (const im of it.images ?? []) chars += im.data.length;
  if (chars <= MAX_IMAGE_PAYLOAD_CHARS) return;
  for (const d of Object.values(state.drafts)) d.images = [];
  for (const q of Object.values(state.queue)) for (const it of q) delete it.images;
}

/** 组装快照：来源 = sessions store（Tab 顺序/活跃/未读）+ ui store（左栏树展开/折叠）+ run store（草稿/队列/面板态）+ 驻留表 + 窗口几何 */
export async function buildSnapshot(): Promise<UiState> {
  const s = useSessions.getState();
  const order = s.tabs.map((t) => t.key);
  const items: Record<string, UiTabSnapshot> = {};
  for (const t of s.tabs) {
    items[t.key] = {
      workspace: t.workspace,
      title: t.title,
      projectId: t.projectId,
      createdAt: t.createdAt,
      prefs: t.prefs,
    };
  }
  const active = s.tabs.find((t) => t.key === s.activeKey) ?? null;
  // 树态从 store 读（不再从本模块内存读）：单一事实源，避免「store 变了、快照还是旧值」的漂移
  const ui = useUi.getState();
  const drafts: UiState["drafts"] = {};
  const queue: UiState["queue"] = {};
  const panels: Record<string, UiPanels> = {};
  // 只收「活着的 Tab」∪「仍在会话列表里的 retained（关 Tab 时用户选择保留内容）」：
  // 收敛必要性（审查 E14）：无条件把 retained 全量写盘 ⇒ 会话删除后其草稿/队列/面板仍留在 ui-state.json，
  // 下次启动 loadUiState 又把它搬回驻留表 → 永久复现。known 用会话列表（后端权威）而非 Tab 条：
  // 用户主动关掉但**没删**的 Tab，其内容按设计必须保住。
  const known = new Set<string>([...order, ...s.sessions.map((m) => m.id)]);
  // 列表为空时不收敛：refresh() 把 listSessions 失败容错成空列表，此时按「未知即孤儿」整表清
  // 会把用户选择保留的草稿一起抹掉（且随下一次落盘永久丢失）；删会话后的清理另有删除路径兜底。
  const trusted = known.size > 0;
  if (trusted) {
    for (const key of Object.keys(memory.retained)) if (!known.has(key)) delete memory.retained[key];
  }
  for (const key of [...new Set([...order, ...Object.keys(memory.retained)])]) {
    const c = contentOf(key);
    if (c.draft) drafts[key] = c.draft;
    if (c.queue?.length) queue[key] = c.queue;
    if (c.panels) panels[key] = c.panels;
  }
  const unread: Record<string, boolean> = {};
  for (const [id, flag] of Object.entries(s.unread)) if (flag) unread[id] = true;
  const anchors: Record<string, ScrollAnchor> = {};
  for (const [id, a] of memory.anchors) if (order.includes(id)) anchors[id] = a;
  const state: UiState = {
    schema: UI_STATE_SCHEMA,
    tabs: { order, activeKey: s.activeKey, items },
    activeProject: active?.projectId ?? null,
    window: await readWindowGeometry(),
    scrollAnchors: anchors,
    drafts,
    queue,
    tree: { expanded: { ...ui.treeExpand }, collapsed: ui.treeCollapsed, unread },
    panels,
  };
  pruneImages(state);
  return state;
}

// ---------- 落盘 ----------

/** 写失败上报：节流以免连续失败刷屏；内容不更新 lastWritten，下一次防抖会重试（绝不静默丢弃） */
function reportWriteFailure(e: unknown): void {
  const now = Date.now();
  if (now - lastFailAt < FAIL_REPORT_INTERVAL_MS) return;
  lastFailAt = now;
  useUi.getState().toast(`界面状态保存失败：${String(e)}`);
}

/** 清掉两枚落盘计时器（落盘已发生 / 立刻手动落盘时都调用） */
function clearFlushTimers(): void {
  if (flushTimer) clearTimeout(flushTimer);
  if (maxWaitTimer) clearTimeout(maxWaitTimer);
  flushTimer = null;
  maxWaitTimer = null;
}

/** 变更后防抖落盘（store 订阅驱动；连点/流式高频变更只写最后一次）。
 *  只靠防抖在「流式持续输出」下会被无限推迟（每帧都重置计时器 → 一次都不落盘），
 *  故另挂一枚**不重置**的最长等待计时器：现场态最多滞后 FLUSH_MAX_WAIT_MS 落盘 */
export function scheduleFlush(): void {
  if (flushTimer) clearTimeout(flushTimer);
  flushTimer = setTimeout(onFlushDue, FLUSH_DEBOUNCE_MS);
  if (!maxWaitTimer) maxWaitTimer = setTimeout(onFlushDue, FLUSH_MAX_WAIT_MS);
}

function onFlushDue(): void {
  clearFlushTimers();
  void flushNow();
}

/** 组装 → 比对 → 写盘一次。与上次成功写入内容相同则跳过（避免空转 I/O）；
 *  组装失败 / 写盘失败都上报且**不更新 lastWritten**，下一次防抖会重试（绝不静默丢弃） */
async function writeSnapshot(): Promise<void> {
  let state: UiState;
  let json: string;
  try {
    state = await buildSnapshot();
    json = JSON.stringify(state);
  } catch (e) {
    reportWriteFailure(e);
    return;
  }
  if (json === lastWritten) return;
  try {
    await ipc.setUiState(state);
    lastWritten = json;
  } catch (e) {
    reportWriteFailure(e);
  }
}

/** 立即写盘（退出拦截应答前 / 关 Tab 现场变化后 / 测试）。
 *  并发调用串行化：在途落盘期间来的这一次只置一次「补写」标记，并等**在途落盘 + 补写**都完成才 resolve——
 *  保证 await flushNow() 返回时磁盘上已是最新现场态（退出应答依赖这一点），也避免两个快照交错落盘 */
export async function flushNow(): Promise<void> {
  clearFlushTimers();
  if (flushing) {
    flushPending = true;
    return flushing;
  }
  const run = (async () => {
    try {
      do {
        flushPending = false;
        await writeSnapshot();
      } while (flushPending);
    } finally {
      flushing = null;
    }
  })();
  flushing = run;
  return run;
}

// ---------- 滚动锚点 ----------

/** 记录锚点（null = 清除该项）；立即进内存表（切 Tab 读取要最新值），落盘走防抖 */
export function setScrollAnchor(sessionId: string, anchor: ScrollAnchor | null): void {
  if (!sessionId) return;
  if (anchor) memory.anchors.set(sessionId, anchor);
  else memory.anchors.delete(sessionId);
  scheduleFlush();
}

export function getScrollAnchor(sessionId: string): ScrollAnchor | null {
  return memory.anchors.get(sessionId) ?? null;
}

/** 滚动记录防抖调度（ChatMessages 每次滚动调用）：窗口内只读一次布局，后续调用直接丢弃。
 *  记的是本窗口首次滚动时的锚点（不是最后一次）；再滚一次会在下一个窗口重新记录 */
export function scheduleAnchor(
  sessionId: string,
  reader: () => ScrollAnchor,
): void {
  if (anchoring) return;
  anchoring = true;
  anchorTimer = setTimeout(() => {
    anchorTimer = null;
    try {
      setScrollAnchor(sessionId, reader());
    } catch {
      // 读锚点失败（容器已卸载/布局读数异常）：本次不记，且绝不把异常抛进事件循环
    } finally {
      // 无论成败都必须复位，否则一次异常会让锚点记录被永久关停
      anchoring = false;
    }
  }, ANCHOR_DEBOUNCE_MS);
}

// ---------- 左栏树状态 ----------
// 事实源 = useUi store（见文件顶部说明）；这四个导出是读写 store 的薄封装，
// 留给非组件的调用方（hydrate 回填、既有单测）用；组件内一律直接订阅 store。

export function getTreeExpanded(): Record<string, boolean> {
  // 返回副本：调用方改返回值不能污染 store（既有单测守护这一点）
  return { ...useUi.getState().treeExpand };
}

export function setTreeExpanded(next: Record<string, boolean>): void {
  useUi.setState({ treeExpand: { ...next } });
  scheduleFlush();
}

export function getTreeCollapsed(): boolean {
  return useUi.getState().treeCollapsed;
}

export function setTreeCollapsed(collapsed: boolean): void {
  useUi.setState({ treeCollapsed: collapsed });
  scheduleFlush();
}

// ---------- 关 Tab：草稿/队列的保留与丢弃 ----------

/** 该 Tab 是否有未发送内容（草稿文本/附件、前端队列）——关 Tab 二次确认的判据 */
export function tabHasPendingContent(sessionId: string): boolean {
  const run = useRun.getState();
  const d = run.drafts[sessionId];
  if (d && (d.text.trim() !== "" || d.images.length > 0 || (d.refs?.length ?? 0) > 0)) return true;
  return (run.tabs[sessionId]?.queue.length ?? 0) > 0;
}

/** 关 Tab 时选择「关闭但保留草稿」：把内容搬进驻留表（关 Tab 会删运行态分桶），并落盘 */
export function retainTabContent(sessionId: string): void {
  const c = contentOf(sessionId);
  if (c.draft || c.queue?.length || c.panels) memory.retained[sessionId] = c;
  scheduleFlush();
}

/** 关 Tab 时选择「丢弃并关闭」：清掉驻留内容（快照随之不再包含该项） */
export function dropTabContent(sessionId: string): void {
  delete memory.retained[sessionId];
  scheduleFlush();
}

/** 重开会话时回填驻留内容（幂等：无驻留则直接返回；已有非空草稿/队列不覆盖，避免盖掉用户新输入） */
export function applyRetainedContent(sessionId: string): void {
  const kept = memory.retained[sessionId];
  if (!kept) return;
  delete memory.retained[sessionId];
  const run = useRun.getState();
  run.initTab(sessionId);
  if (kept.draft) {
    const cur = run.drafts[sessionId];
    if (!cur || (cur.text === "" && cur.images.length === 0 && (cur.refs?.length ?? 0) === 0)) {
      const images = toPendingImages(kept.draft.images);
      // 旧快照缺 refs 时按空处理（schema 未递增，向前兼容靠这里兜）；顺带滤非法项与去重
      // （手改过 ui-state.json 的重复 ref 会与 React 的 key={ref} 撞车）
      const refs = Array.isArray(kept.draft.refs)
        ? [...new Set(kept.draft.refs.filter((r) => typeof r === "string" && r !== ""))]
        : [];
      useRun.setState((s) => {
        s.drafts[sessionId] = { text: kept.draft!.text, images, refs };
      });
    }
  }
  if (kept.queue?.length || kept.panels) {
    useRun.setState((s) => {
      const t = s.tabs[sessionId];
      if (!t) return;
      if (kept.queue?.length && t.queue.length === 0) t.queue = kept.queue!.map((q) => ({ ...q }));
      if (kept.panels?.subDrawer) t.subDrawer = kept.panels.subDrawer;
      if (kept.panels?.todos?.length) t.todos = kept.panels.todos;
      if (kept.panels?.suggestions?.length) t.suggestions = kept.panels.suggestions;
    });
  }
}

// ---------- 退出拦截 ----------

/** 应答后端退出请求：任何应答前先 flushNow（保证退出/中断时现场态不丢），再回 resolve_exit_request */
export async function respondExitRequest(
  action: "exit" | "cancel" | "abort" | "wait",
): Promise<void> {
  await flushNow();
  try {
    await ipc.resolveExitRequest(action);
  } catch {
    /* 后端可能已随退出流程关闭通道：忽略，不再二次打扰 */
  }
}

// ---------- 订阅与测试复位 ----------

/** 订阅 store 变化驱动防抖落盘（模块级单例，重复调用无副作用）。
 *  覆盖面：useSessions = Tab 集合/顺序/活跃 Tab/未读；useRun = 草稿/队列/面板态（subDrawer、todos、suggestions）。
 *  树展开/折叠态虽住在 useUi store，但不在这里订阅：写入口 setTreeExpanded/setTreeCollapsed 自行 scheduleFlush，
 *  免得通知堆栈/设置弹窗这类无关的高频变更白跑一次 buildSnapshot。
 *  另订阅退出请求：后端在无 run 在跑时只给前端 2 秒应答窗口（EXIT_WATCHDOG_SECS），
 *  不能只等防抖——请求一到就立刻落一次盘（用户点选时 respondExitRequest 还会再 flush 一次） */
export function initUiStatePersistence(): void {
  if (subscribed) return;
  subscribed = true;
  useSessions.subscribe(() => scheduleFlush());
  useRun.subscribe(() => scheduleFlush());
  useUi.subscribe((state, prev) => {
    if (state.exitRequest && state.exitRequest !== prev.exitRequest) void flushNow();
  });
}

/** 测试用复位：清内存态与计时器（不清订阅，订阅无副作用）。
 *  「计时器」**包含锚点防抖窗口**：它是延迟读取 DOM 的窗口，不在复位时清掉就会在复位之后触发，
 *  把上一轮现场读出的陈旧锚点（常见为「贴底」）写进新场景 */
export function reset(): void {
  clearFlushTimers();
  if (anchorTimer) clearTimeout(anchorTimer);
  anchorTimer = null;
  flushing = null;
  flushPending = false;
  memory = freshMemory();
  // 树态住在 store 里，reset 必须一并清：否则测试间串味，且 buildSnapshot 会读到上一轮的展开态
  useUi.setState({ treeExpand: {}, treeCollapsed: false });
  lastWritten = "";
  lastFailAt = 0;
  anchoring = false;
}
