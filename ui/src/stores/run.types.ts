// 每个 Tab 运行态 store 的共享类型（run.ts、runFrames.ts 与 UI 组件共同消费）。
import type { Breakdown, Todo } from "../ipc/types";

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
  questions?: any[];
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
  /** 压缩进行中（run:compacting 与 run:compacted/failed 之间）：CompactButton loading + notice 去重 */
  compacting: boolean;
  /** 会话级用量累加（usage 帧此前被丢弃，工具条「命中」显示的唯一数据源）。
   *  缓存命中率 = cacheRead / (cacheRead + input)（两种协议下缓存读 token 都已从 input 中扣除）。
   *  可选：仅 runtime 新建的桶（blank()）必定有值，测试夹具的历史字面量可缺省——
   *  消费方（applyUsageFrame / cacheHitRate / Composer）均已做缺省处理。 */
  usage?: UsageTotals;
}

/** 会话级 usage 累加值（命中率派生源；跨帧累加，Tab 关闭即丢弃、不持久化）。 */
export interface UsageTotals {
  input: number;
  output: number;
  cacheRead: number;
  cacheWrite: number;
}
