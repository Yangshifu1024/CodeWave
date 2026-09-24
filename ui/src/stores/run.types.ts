// 每个 Tab 运行态 store 的共享类型（run.ts、runFrames.ts 与 UI 组件共同消费）。
import type { ApprovalMode, AskQuestionPayload, Breakdown, GoalState, HistoryBoundary, HistoryFormat, Todo } from "../ipc/types";

/** 工具卡视图模型：timeline 锚点（callKey）+ 卡体数据；progressTail 为流式进度尾迹 */
export interface ToolView {
  callKey: string;
  tool: string;
  /** waiting = 审批 / 范围确认门等待中（tool:start 的 waiting 相；门通过后转 running，被门拒绝则直接来 tool:error） */
  status: "running" | "waiting" | "ok" | "error";
  argsPreview?: string;
  outcome?: any;
  durationMs?: number;
  progressTail: string;
}

/** timeline 分段：text/thinking 按到达顺序穿插，tool 为工具卡锚点（卡体存于 toolsMap）。
 *  thinking 计时：startedAt = 分段开始；durationMs = 定格时长（运行中 undefined）。 */
export type TimelineSeg =
  | { kind: "text"; text: string }
  | { kind: "thinking"; text: string; startedAt?: number; durationMs?: number }
  | { kind: "tool"; callKey: string }
  | { kind: "sub"; subId: string };

/** 转录 UI 项联合：用户消息 / 助手 timeline / 子代理卡锚点 / 通知 / 错误 */
export type UiItem =
  | { kind: "user"; text: string; createdAt?: string; images?: { mediaType: string; data: string }[] }
  | { kind: "assistant"; timeline: TimelineSeg[]; toolsMap: Record<string, ToolView>; streaming: boolean; createdAt?: string }
  | { kind: "sub"; subId: string }
  | { kind: "notice"; text: string }
  /** errorKind = 后端 ProviderError 分类（[docs/auth-error-guidance](../../../docs/auth-error-guidance.md)）；auth/billing 渲染「打开模型设置」快捷入口 */
  | { kind: "error"; text: string; errorKind?: string };

/** assistant 项的收窄别名（Extract 自 UiItem） */
export type AssistantItem = Extract<UiItem, { kind: "assistant" }>;

/** 询问/审批卡状态（ask:opened 落地，ask:closed 清空） */
export interface AskState {
  askId: string;
  kind: "ask" | "approval";
  title?: string;
  detail?: string;
  /** 问题列表（带 mode 的选项 = 批准类选项：选中即直提并把胶囊切到该档位；缺 mode = 非批准类） */
  questions?: AskQuestionPayload[];
  /** arch 审批门（[docs/arch-orchestrator](../../../docs/arch-orchestrator.md)）：批准后把权限胶囊同步为自动编辑档 */
  switchToAutoEdit?: boolean;
  /** [docs/run-queue-and-ask-revamp](../../../docs/run-queue-and-ask-revamp.md)：命令审批显示「始终允许本项目」第三选项 */
  allowAlways?: boolean;
  /** [docs/run-queue-and-ask-revamp](../../../docs/run-queue-and-ask-revamp.md)：计划文件路径（计划卡「查看完整计划」打开） */
  planFile?: string | null;
  /** [docs/ask-approval-shape-note-nav](../../../docs/ask-approval-shape-note-nav.md)：批准形标记 + 批准项 id（后端宽松识别；驱动 AskPanel 单选互斥与直提判定） */
  approval?: boolean;
  approveId?: string | null;
}

/** 运行队列项（运行中提交的任务，按序执行） */
export interface QueueItem {
  id: string;
  text: string;
  /** 图片附件（base64，与发送入参同构；出队后走完整发送管线） */
  images?: { mime: string; data: string }[];
}

/** 子代理卡片视图模型（聊天卡与机器人运行指示器的数据源） */
export interface SubView {
  subId: string;
  role: string;
  /** 注册表规范角色名（sub:spawn 下发；展示优先于原始 role 字符串） */
  name?: string | null;
  description: string;
  /** 完整任务文本（spawn 时截断至 2000 字符；恢复时取自 tool_use args），过程抽屉首块展示 */
  task?: string;
  step: number;
  maxSteps: number;
  tokens: number;
  lastTools: string[];
  status: "running" | "done" | "error";
  /** 收尾时的真实已启动步数（sub:done 刷新，纠正轮询采样滞后） */
  stepsUsed?: number;
  /** 收尾原因（sub:done）：report = 正常汇报收尾；budget = 预算耗尽；no_report = 未汇报即结束（疑似提前退出）；缺省 = 旧数据，按正常收尾展示 */
  ended?: "report" | "budget" | "no_report";
  /** 运行摘录（sub:step 采样） */
  detail?: string;
  /** 当前权限档位（sub:step 每步上报；归档 / 旧数据缺省 → 抽屉不渲染档位行） */
  approvalMode?: ApprovalMode;
  /** 最终报告（sub:report，收尾前下发） */
  report?: string;
}

/** 每个子代理独立的消息流（[docs/subagent-interaction-drawer](../../../docs/subagent-interaction-drawer.md)）：帧归一的产物，由过程抽屉渲染；归档子代理按需从后端重建。
 *  生命周期跟随 Tab（dispose 即丢弃），跨运行保留——任务结束后仍可回看。 */
export interface SubStream {
  timeline: TimelineSeg[];
  toolsMap: Record<string, ToolView>;
  status: "running" | "done" | "error";
  /** 代际阈值（对齐主流的 streamGen，拦截重试旧代际的半帧） */
  gen: number;
  /** 过程历史是否已从后端加载（随会话恢复的归档子代理在抽屉首次打开时拉取） */
  loaded: boolean;
}

/** Composer 待发图片附件（缩略预览 + base64 本体；类型/大小/数量校验链在 useComposerAttachments） */
export interface PendingImage {
  id: string;
  name: string;
  mime: string;
  data: string; // base64（不含 data: 前缀）
  dataUrl: string; // 缩略预览
}

/** Composer 草稿（文本 + 待发图片附件），按 Tab 平行分桶（key 同 tabs）；纯内存态：切会话各自保留，
 *  关 Tab 随 dispose 丢弃，重启不恢复。不放进 TabRunState：击键高频换桶引用会把重渲染广播给
 *  全部 useActiveRun 订阅者（ChatMessages 等），独立分片后只有 Composer 订阅者抖动。 */
export interface ComposerDraft {
  text: string;
  images: PendingImage[];
  /** 文件引用（[docs/composer-file-ref-chips](../../../docs/composer-file-ref-chips.md)）：输入框里以 chip 展示。
   *  引用不再是正文文本——发送前一刻由 mergeRefs 合成 `@<ref>` 追加到正文末尾（模型所见与改造前一致）。 */
  refs: string[];
}

/** 历史分页游标（分段 append-only JSONL · 批2 P3）：首屏 1 段 + 逐段前翻的界面状态。
 *
 *  与后端 `load_session` 的 `paging` 字段对应，但键名走前端惯例（camelCase），并多两个纯前端字段：
 *  `loadedPages`（内存有界策略的计数依据）与 `loading`。
 *
 *  缺省（undefined / null）＝**未分页**：新建会话、尚未加载、旧后端只回 `Message[]`、legacy 会话
 *  （首屏即整份，没有更早内容）——上述情形界面一律不显示「加载更早的」入口。 */
export interface HistoryPaging {
  /** 落盘格式：new = 段式 JSONL（可逐段前翻）；legacy = 旧单文件（无更早） */
  format: HistoryFormat;
  /** 已加载到的最早段序号（1 起；legacy / 无段 = 0）——下一次前翻的 `before_seq` */
  loadedFromSeq: number;
  /** 首屏段序号（**不随前翻变化**）：「收起更早的」据此把转录裁回首屏那一段 */
  firstLoadedSeq: number;
  /** 更早是否还有**可读**的段 */
  hasMore: boolean;
  /** 磁盘上的消息总数（display 口径；仅供展示） */
  totalMessages: number;
  /** 段文件总数 */
  segmentCount: number;
  /** 已加载页数（首屏 = 1，每成功前翻一页 +1）——内存有界（MAX_PAGED_PAGES）的计数依据 */
  loadedPages: number;
  /** 前翻请求进行中（入口 loading + 防重入） */
  loading: boolean;
  /** 已加载段范围内**跳过的坏段数**（首屏取 `paging.bad_segments`，前翻按回传值累加）——
   *  转录顶部轻量提示的数据源（>0 才显，见 ChatMessages）。可选：旧后端 / 测试夹具缺省 = 0。 */
  badSegments?: number;
  /** 上一次前翻失败：入口保留可重试；已有转录一字不动 */
  failed?: boolean;
}

/** 单个 Tab 的全部运行态（run store 的分桶单元，blank() 给出初值）。
 *  契约：帧 reducer（runFrames.ts）与事件 handler（runHandlers.ts）在此结构上就地变异（immer 草稿）。 */
export interface TabRunState {
  /** 转录 UI 项序列（timeline 展示顺序） */
  items: UiItem[];
  /** 是否有运行中的任务 */
  running: boolean;
  /** 代际阈值：run:retry 后低于该代际的 delta 帧一律丢弃（review C2） */
  streamGen: number;
  /** 当前询问/审批卡（null = 无） */
  ask: AskState | null;
  /** 上下文 token 分布（null = 未知） */
  breakdown: Breakdown | null;
  todos: Todo[];
  /** 目标模式（`ApprovalMode::Goal`）状态快照（`goal:update` 事件落地；null/缺省 = 无目标）。
   *  可选：历史测试夹具的字面量可缺省，消费方一律用 `tab.goal ?? null` 归一。 */
  goal?: GoalState | null;
  /** 目标推送代际（`goal:update` 每次 +1）：`syncGoal` 的回读结果只在代际未变时落地——
   *  推送永远更新，回读只补初值，绝不用更旧的值盖掉推送。 */
  goalRev?: number;
  /** 运行结束后的建议追问 */
  suggestions: string[];
  /** 子代理卡片列表 */
  subs: SubView[];
  /** [docs/subagent-interaction-drawer](../../../docs/subagent-interaction-drawer.md)：每个子代理独立消息流（key = subId），过程抽屉数据源 */
  subStreams: Record<string, SubStream>;
  /** [docs/subagent-interaction-drawer](../../../docs/subagent-interaction-drawer.md)：过程抽屉状态（按 Tab 记忆；关闭置 open=false 不销毁流数据） */
  subDrawer: { open: boolean; subId: string | null };
  /** git 状态缓存（null = 非 repo / 未知） */
  gitEntries: { repo: boolean; entries: any[]; branch?: string | null } | null;
  /** [docs/session-artifacts-and-files-tab](../../../docs/session-artifacts-and-files-tab.md)：成功 create/edit 写信号（每次 +1）；右栏文件页签据此刷新 */
  writeTick: number;
  /** [docs/run-queue-and-ask-revamp](../../../docs/run-queue-and-ask-revamp.md)：运行队列（运行中提交的新任务，当前任务完成后依序执行） */
  queue: QueueItem[];
  /** [docs/run-queue-and-ask-revamp](../../../docs/run-queue-and-ask-revamp.md)：待「立即运行」项（cancel 完成后立即出队执行） */
  pendingItemId: string | null;
  /** [docs/run-queue-and-ask-revamp](../../../docs/run-queue-and-ask-revamp.md)：队列项编辑回填草稿（文本 + 附件；Composer 消费后清除） */
  draftFromQueue: { text: string; images?: { mime: string; data: string }[] } | null;
  /** [docs/run-queue-and-ask-revamp](../../../docs/run-queue-and-ask-revamp.md)：最近一次 run:done 的 run_id（对 suggest 双 done 去重；done1 同步出队并翻转 running 使旧守卫失效） */
  lastDoneRunId: string | null;
  /** 最近一次起 run 的 run_id（`start_chat` / `resume_goal` 的返回值；缺省 = 本 Tab 尚未起过 run） */
  runId?: string | null;
  /** 压缩进行中（run:compacting 与 run:compacted/failed 之间）：CompactButton loading + notice 去重 */
  compacting: boolean;
  /** 会话级用量累加（usage 帧此前被丢弃，工具条「命中」显示的唯一数据源）。
   *  缓存命中率 = cacheRead / (cacheRead + input)（两种协议下缓存读 token 都已从 input 中扣除）。
   *  可选：仅 runtime 新建的桶（blank()）必定有值，测试夹具的历史字面量可缺省——
   *  消费方（applyUsageFrame / cacheHitRate / Composer）均已做缺省处理。 */
  usage?: UsageTotals;
  /** **本轮**（单次 run）计数（[docs/composer-token-rate](../../../docs/composer-token-rate.md)）：工具条速率段与 tooltip 的唯一数据源。
   *  与 `usage` 的关键差别是「不跨 run 累加」——`send()` 处归零，否则速率会跨轮累积失真。
   *  可选：缺省 = 本轮无 usage 数据（旧后端 / 重挂载后未收到帧）→ 速率段整体隐藏。 */
  runMetrics?: RunMetrics;
  /** 历史分页游标（分段 append-only JSONL · 批2 P3）：缺省 = 未分页（不显示「加载更早的」入口）。
   *  可选：仅首屏恢复（`load_session`）会建立它，既有测试夹具的字面量可缺省——
   *  消费方（ChatMessages）已做缺省处理。 */
  paging?: HistoryPaging | null;
  /** 转录项的**稳定渲染键**（批2 P3 AC-14），与 `items` **头部对齐**：`itemKeys[i]` ↔ `items[i]`。
   *
   *  长度 ≤ `items.length`：超出部分是本次运行新产生的项，按到达顺序取 `live:<序数>`（见 utils/scrollAnchor 的
   *  `itemKeysOf`）——恢复自磁盘的项用「段号 + 段内序号」（`s<段号>:<序>`），因此：
   *  **前插更早内容**（分页）不动任何已有键，**尾部追加**（流式）也不动——只有数组下标会两头漂移。 */
  itemKeys?: string[];
  /** 上下文压缩边界（批2 P2）：按 `seq` 升序去重，只管**已加载段范围内**的边界。
   *
   *  缺省 = 无边界（旧后端 / legacy / 从未压缩过）——界面不渲染分隔线，也不报错。
   *  渲染位置不存下标：由 [utils/scrollAnchor.boundaryAtIndexes] 在渲染时按稳定键的段号现算，
   *  因为分页前插会整体挪动下标，而段号（`s<段号>:<段内序>` 的前半）不动。 */
  boundaries?: HistoryBoundary[];
}

/** 本轮运行计数（[docs/composer-token-rate](../../../docs/composer-token-rate.md)）。派生展示口径全在
 *  `features/chat/composerMetrics.ts`（纯函数，不依赖 store），此处只描述字段来源。 */
export interface RunMetrics {
  /** Σ 本轮**被计入步**的 output（仅 `duration_ms > 0` 的帧，与 `genMs`/`steps` 同域） */
  output: number;
  /** Σ 各步生成耗时（仅 `duration_ms > 0` 的帧；= 速率分母，不含工具与审批等待） */
  genMs: number;
  /** 计入的 LLM 步数（与 `genMs` 同域：`duration_ms > 0` 才 +1） */
  steps: number;
  /** 本轮**首个**非空 TTFT（首个输出增量延迟，含思考；不是求和） */
  ttftMs: number | null;
  /** Σ 本轮工具卡 `durationMs`（工具卡计时起点在审批门之后，天然不含审批等待；仅 tooltip 单列，不进速率分母） */
  toolMs: number;
}

/** 会话级 usage 累加值（命中率派生源；跨帧累加，Tab 关闭即丢弃、不持久化）。 */
export interface UsageTotals {
  input: number;
  output: number;
  cacheRead: number;
  cacheWrite: number;
}
