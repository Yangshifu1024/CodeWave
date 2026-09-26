// 每个 Tab 隔离的运行态：转录、流式、工具卡、ask/审批、计划、子代理、上下文分布
// immer 中间件：高频流式更新走结构共享（路径级拷贝）；组件只对订阅到的 TabRunState 引用反应
// [docs/fence-hardening-and-powershell-ast](../../../docs/fence-hardening-and-powershell-ast.md) 重构：纯帧 reducer 收敛于 runFrames.ts，后端事件 handler 工厂在 runHandlers.ts，
// 共享类型在 run.types.ts（下方 re-export，消费方 import 路径保持 "./run" 不变）。
import { create } from "zustand";
import { immer } from "zustand/middleware/immer";
import { Channel } from "@tauri-apps/api/core";
import { ipc } from "../ipc/client";
import type { Breakdown, GoalState, HistoryBoundary, HistoryStatus, Message, SessionPaging, ToolResultEvent, ToolStartEvent } from "../ipc/types";
import { useSessions } from "./sessions";
import { useUi } from "./ui";
import { i18n } from "../i18n";
import {
  BLANK,
  appendDelta,
  applyFrameToTab,
  applyToolStart,
  blank,
  currentAssistantIm,
  ensureToolAnchorIm,
  findToolViewInItems,
  messagesToSubStream,
  scanToolResults,
  restoredToolData,
  safeArgsPreview,
  updateToolFromStart,
} from "./runFrames";
import {
  askHandlers,
  compactHandlers,
  historyStatusNotice,
  miscHandlers,
  runLifecycleHandlers,
  subHandlers,
  toolHandlers,
} from "./runHandlers";

export type {
  ComposerDraft,
  HistoryPaging,
  PendingImage,
  QueueItem,
  SubStream,
  SubView,
  TabRunState,
  TimelineSeg,
  ToolView,
  UiItem,
} from "./run.types";
import type { ComposerDraft, HistoryPaging, PendingImage, SubStream, SubView, TabRunState, TimelineSeg, ToolView, UiItem } from "./run.types";

/** 续跑目标时后端写进会话历史的用户可见标记。**刻意不做 i18n**：它是要落进持久化历史的标记，
 *  必须与后端常量逐字一致（`src-tauri/src/host/commands/session.rs` 的 `RESUME_GOAL_TEXT`）——
 *  国际化会让「实时视图」与「重开会话后从历史恢复」的两处文案漂移。 */
const RESUME_GOAL_TEXT = "继续推进";

  /** 运行态 store 契约：tabs 按会话 id 分桶 + 全部动作；bindGlobalHandlers 的键集合即 30 键事件面（唯一注册点）。 */export interface RunStore {
  tabs: Record<string, TabRunState>;
  /** Composer 草稿平行分桶（key 同 tabs；独立于 tabs 的原因见 ComposerDraft 注释） */
  drafts: Record<string, ComposerDraft>;
  st(sessionId: string | null): TabRunState;
  initTab(sessionId: string): void;
  dispose(sessionId: string): void;
  markRunning(sessionId: string): void;
  setBreakdown(sessionId: string, b: Breakdown | null): void;
  pushItem(sessionId: string | null, item: UiItem): void;
  send(text: string, images?: { mime: string; data: string }[], targetKey?: string): Promise<boolean>;
  cancel(sessionId?: string): Promise<void>;
  /** 子代理卡单独停止该子代理（主代理会收到 E_SUBAGENT_STOPPED 并询问用户是否重派） */
  stopSubagent(sessionId: string | null, subId: string): Promise<void>;
  resolveAsk(askId: string, value: any): Promise<void>;
  /** 目标模式（`ApprovalMode::Goal`）：暂停后继续推进（调 `resume_goal`，与 `start_chat` 同构；状态走 `goal:update`） */
  resumeGoal(sessionId?: string | null): Promise<void>;
  /** 暂停后退回只读澄清期，允许修订合同并重新批准。 */
  reopenGoal(sessionId?: string | null): Promise<void>;
  /** 目标模式：回读目标状态初值（会话打开 / 惰性激活时；`goal:update` 推送仍是唯一的持续更新源） */
  syncGoal(sessionId: string): Promise<void>;
  /** [docs/run-queue-and-ask-revamp](../../../docs/run-queue-and-ask-revamp.md)：出队并运行下一条（run:done 后自动调用；error/cancelled 的暂停态由「继续」恢复） */
  runQueueNext(sessionId: string): Promise<void>;
  /** [docs/run-queue-and-ask-revamp](../../../docs/run-queue-and-ask-revamp.md)：队列项立即运行（运行中 = 先打断当前运行） */
  runNow(sessionId: string, id: string): Promise<void>;
  removeQueueItem(sessionId: string, id: string): void;
  /** 需求批次：运行队列拖拽排序——把 fromId 项移动到 toId 项当前位置 */
  reorderQueue(sessionId: string, fromId: string, toId: string): void;
  /** [docs/run-queue-and-ask-revamp](../../../docs/run-queue-and-ask-revamp.md)：取出队列项文本回填输入框（编辑） */
  editQueueItem(sessionId: string, id: string): void;
  consumeDraftFromQueue(sessionId?: string): void;
  /** Composer 草稿读写（平行分桶；缺省写当前活跃 Tab，异步回填路径传显式 sessionId 防切 tab 竞态；与 useState 同形支持 updater） */
  setDraftText(v: string | ((prev: string) => string), sessionId?: string): void;
  setDraftImages(v: PendingImage[] | ((prev: PendingImage[]) => PendingImage[]), sessionId?: string): void;
  /** 文件引用草稿（[docs/composer-file-ref-chips](../../../docs/composer-file-ref-chips.md)）：与文本/图片同桶、同隔离 */
  setDraftRefs(v: string[] | ((prev: string[]) => string[]), sessionId?: string): void;
  /** 发送接受后清空对应 Tab 草稿（文本 + 附件一并）；缺省为当前活跃 Tab */
  clearDraft(sessionId?: string): void;
  onToolResult(sessionId: string, p: ToolResultEvent, ok: boolean): void;
  /** 工具开始（tool:start）：建/翻工具卡（waiting → running）；运行已结束的迟到开始事件一律丢弃 */
  onToolStart(sessionId: string, p: ToolStartEvent): void;
  /** 恢复会话转录（首屏）。meta 可选：
   *  - `history_status`（[docs/session-history-limits](../../../docs/session-history-limits.md)）：带值时在重建结果末尾补一条
   *    历史状态提示（重启后仍可见的落点）；P4 的体积状态（`warned` / `fused`）走同一条链路，
   *    文案由 historyStatusNotice 统一分派；
   *  - `paging`（批2 P3）：首屏分页信息——带上即建立分页游标（可显示「加载更早的」），
   *    不带 = 未分页（旧后端 / 直接调用的测试），界面不显示入口。 */
  restoreFromMessages(sessionId: string, msgs: Message[], meta?: RestoreMeta): void;
  /** 加载更早一段历史（批2 P3 分页前翻）：把返回的一段**前置**到转录头部；
   *  失败 / 空返回都不破坏已有转录（后端对越界不报错）。缺省 sessionId 取当前活跃会话 */
  loadEarlier(sessionId?: string | null): Promise<void>;
  /** 收起更早的（批2 P3 AC-15 内存有界）：丢掉前翻加载的旧段，回到首屏那一段 */
  collapseEarlier(sessionId?: string | null): void;
  /** [docs/subagent-interaction-drawer](../../../docs/subagent-interaction-drawer.md)：打开子代理过程抽屉（归档子代理按需拉取过程历史重建消息流） */
  openSubDrawer(sessionId: string | null, subId: string): Promise<void>;
  closeSubDrawer(sessionId?: string | null): void;
  refreshGit(sessionId?: string | null): Promise<void>;
  bindGlobalHandlers(): Record<string, (p: any) => void>;
}

/** `restoreFromMessages` 的可选元信息（首屏恢复） */
export interface RestoreMeta {
  /** 历史保存状态（索引侧 `SessionMeta.history_status` 与 `run:done.history_save` 同一枚举） */
  history_status?: HistoryStatus;
  /** 首屏分页信息（`load_session` 的 `paging` 字段）；缺省 = 未分页（旧后端 / 直接调用的测试） */
  paging?: SessionPaging;
  /** 压缩边界（批2 P2）：契约口径为 `load_session` 载荷**顶层**字段；`paging.boundaries` 的内嵌形态
   *  也认（两种取其一，见 sessions.ts）。缺省 = 无边界 → 不渲染分隔线。 */
  boundaries?: HistoryBoundary[];
}

/** 恢复回填用的最小 set 契约（helper 定义在 store 之外，避免把 immer 的完整签名搬进来） */
type RestoreSet = (fn: (s: any) => void) => void;
/** `load_tool_outcomes` 的行形态（与 ipc/client.ts 的返回类型一致） */
type ToolOutcomeRow = { call_id: string; outcome: any; duration_ms?: number | null };

/** 「历史里那份模型侧文本取不回出参」的调用 id（[docs/session-restore-fidelity](../../../docs/session-restore-fidelity.md)）。
 *  判据与 restoredToolData 同源：文本在、却解析不出 JSON 对象（被头尾截断 / `[error E_XXX: …]` 前缀 / 其它非 JSON）——
 *  这类调用在后端可能留有原样 sidecar（`tools/batch.rs` 的 should_persist 用同一判据）。
 *  跳过 `[interrupted]`（后端 repair 补的中断文本，从未落盘）与整条结果缺失的调用；去重保序。 */
export function lossyToolKeys(msgs: Message[]): string[] {
  const results = scanToolResults(msgs);
  const keys: string[] = [];
  const seen = new Set<string>();
  for (const m of msgs) {
    if (m.role !== "assistant") continue;
    for (const c of m.content) {
      if (c.type !== "tool_use") continue;
      const callKey = (c as any).id as string;
      if (!callKey || seen.has(callKey)) continue;
      const stored = results[callKey];
      if (!stored || stored.content.trimStart().startsWith("[interrupted]")) continue;
      const data = restoredToolData(stored.content) as any;
      if (!data || data.restored !== true) continue;
      seen.add(callKey);
      keys.push(callKey);
    }
  }
  return keys;
}

/** sidecar 行 → 工具卡：整套 ToolOutcome 直接换回（失败卡不再被当成成功卡）；
 *  极老数据只存了 data 时按成功包裹。 */
function applyToolOutcomes(map: Record<string, any>, rows: ToolOutcomeRow[]): void {
  const byKey = new Map(rows.map((r) => [r.call_id, r]));
  for (const tv of Object.values(map)) {
    const row = byKey.get(tv.callKey);
    if (!row) continue;
    const o = row.outcome;
    tv.outcome = o && typeof o === "object" && "ok" in o ? o : { ok: true, data: o };
    tv.status = tv.outcome.ok === false ? "error" : "ok";
    if (row.duration_ms != null) tv.durationMs = row.duration_ms;
  }
}

/** 子代理卡改名：合成 key（`restored:<callKey>`，历史文本被截断时无从得知真实 sub_id）→ sidecar 里的真实 sub_id。
 *  不改名抽屉就拉不到真实过程流（load_subagent_history 认真实 id）；顺带补回被父历史截断的 report。 */
function renameRestoredSubs(t: TabRunState, rows: ToolOutcomeRow[], subByKey: Record<string, string>): void {
  for (const row of rows) {
    const from = subByKey[row.call_id];
    if (!from || !from.startsWith("restored:")) continue;
    const o = row.outcome;
    const data = o && typeof o === "object" ? (o as any).data : null;
    const to = data && typeof data === "object" ? data.sub_id : undefined;
    if (typeof to !== "string" || !to) continue;
    for (const item of t.items) {
      if (item.kind !== "assistant") continue;
      for (const seg of item.timeline) {
        if (seg.kind === "sub" && seg.subId === from) seg.subId = to;
      }
    }
    const sv = t.subs.find((x) => x.subId === from);
    if (sv) {
      sv.subId = to;
      if (typeof data.report === "string") sv.report = data.report;
    }
    const st = t.subStreams[from];
    if (st && !t.subStreams[to]) t.subStreams[to] = st;
    delete t.subStreams[from];
  }
}

/** 一次批量 IPC 拉回完整出参并回填：主会话卡片与子代理过程抽屉（subId）两处共用。
 *  没有备份（旧会话 / 已被清理）或没有 Tauri 运行时（纯 store 测试）就静默保持占位——
 *  卡片已给「历史未保留…」提示，不打扰用户。 */
function backfillToolOutcomes(
  set: RestoreSet,
  sessionId: string,
  keys: string[],
  subId?: string,
  subByKey?: Record<string, string>,
): void {
  if (keys.length === 0) return;
  try {
    void ipc
      .loadToolOutcomes(sessionId, keys)
      .then((rows) => {
        if (!rows.length) return;
        set((s: RunStore) => {
          const t = s.tabs[sessionId];
          if (!t) return;
          if (subId) {
            // 子代理过程抽屉：工具卡挂在子代理流上，不在主转录
            const st = t.subStreams[subId];
            if (st) applyToolOutcomes(st.toolsMap, rows);
            return;
          }
          for (const item of t.items) {
            if (item.kind === "assistant") applyToolOutcomes(item.toolsMap, rows);
          }
          if (subByKey) renameRestoredSubs(t, rows, subByKey);
        });
      })
      .catch(() => {
        /* 拉不到就保持占位 */
      });
  } catch {
    /* 无 Tauri 运行时（纯 store 测试等）：保持占位 */
  }
}

/** 内存有界（批2 P3 AC-15）：单会话最多往前加载多少段（页）。
 *
 *  依据：段粒度固定「200 条或 512KB 先触者封口」（plan §3 OQ-1），30 段 ⇒ 至多约 6000 条消息同时驻留内存，
 *  与「Tab 关闭即整桶释放」（dispose）同一思路。只在用户**主动连续前翻**时触达，不影响首屏；
 *  达到上限后入口改显终态提示 + 「收起更早的」（回到首屏一段，计数归位）——
 *  「一步跳到最早段」属于虚拟滚动 / 跳转批次（本批明确不做）。 */
export const MAX_PAGED_PAGES = 30;

/** 合并压缩边界（首屏 + 各次前翻页）：按 `seq + source` 去重、按 seq 升序。
 *
 *  合并即「并集」：前翻只带回**更早段**的边界，已加载段的边界不会变；重复请求同一段（重试 / 收起后再翻）
 *  也不会叠出重复分隔线。脏数据（seq 非有限数字）一律丢弃——渲染链路上少一个可能抛错的点。 */
function mergeBoundaries(prev: HistoryBoundary[] | undefined, next: HistoryBoundary[]): HistoryBoundary[] {
  const seen = new Set<string>();
  const out: HistoryBoundary[] = [];
  for (const b of [...(prev ?? []), ...next]) {
    if (!b || typeof b.seq !== "number" || !Number.isFinite(b.seq)) continue;
    const id = `${b.seq}:${b.source ?? "compact"}`;
    if (seen.has(id)) continue;
    seen.add(id);
    out.push(b);
  }
  return out.sort((a, b) => a.seq - b.seq);
}

/** 后端 `SessionPaging` → 前端分页游标。`loadedPages` 由调用方给（首屏 = 1，前翻 +1）。 */
function toPaging(p: SessionPaging, loadedPages: number): HistoryPaging {
  return {
    format: p.format,
    loadedFromSeq: p.loaded_from_seq,
    // 首屏段号是「收起更早的」的裁剪基准，与 loadedFromSeq（随前翻变化）分开记
    firstLoadedSeq: p.loaded_from_seq,
    hasMore: p.has_more,
    totalMessages: p.total_messages,
    segmentCount: p.segment_count,
    loadedPages,
    loading: false,
    // 坏段数（本次覆盖段范围内跳过的段）：缺省 0（旧后端 / 测试夹具不给该字段）—— >0 时转录顶部出轻量提示
    badSegments: p.bad_segments ?? 0,
  };
}

/** `buildTranscript` 的产物：转录项 + 稳定渲染键 + 归档子代理的注册数据 */
interface BuildResult {
  items: UiItem[];
  /** 稳定渲染键（`s<段号>:<段内序>`，与 items 一一对应） */
  keys: string[];
  /** 归档子代理卡（合并注册时按 subId 去重） */
  subs: SubView[];
  /** 子代理过程流（空流占位，抽屉首次打开按需拉取） */
  streams: Record<string, SubStream>;
  /** tool_use_id → 子代理卡当前 key（合成 key 待 sidecar 回填后改名成真实 sub_id） */
  subByKey: Record<string, string>;
}

/** 消息 → 转录项（**首屏与前翻共用的唯一一套转换**，见 loadEarlier）。
 *
 *  `seq` = 这批消息所属的**段序号**（首屏取 `paging.loaded_from_seq`；legacy / 无段 = 0）：
 *  除生成转录项外，还为每一项生成稳定渲染键 `s<段号>:<段内序>`——同一份消息每次加载必然同一批键，
 *  且分页前插不会改动任何已有键（[AC-14]，数组下标做不到）。 */
function buildTranscript(msgs: Message[], seq: number): BuildResult {
  const out: UiItem[] = [];
  // [docs/subagent-interaction-drawer](../../../docs/subagent-interaction-drawer.md)：tool_use_id → 结果（从子代理 outcome 解析 sub_id/report）
  const toolResults = scanToolResults(msgs);
  // [docs/session-restore-fidelity](../../../docs/session-restore-fidelity.md)：历史里存的是**模型侧瘦身文本**，
  // 超阈值会被头尾截断 → 下面 restoredToolData 会退化成 {restored:true} 占位。
  // 这类调用在后端有「原样 sidecar」（按 provider 侧 tool_use id）：恢复末尾由 lossyToolKeys 统一收集、
  // 一次 IPC 批量补回完整出参（缺失就保持占位，卡片文案已覆盖）。子代理卡与过程抽屉走同一条通路。
  const subByKey: Record<string, string> = {};
  const restoredSubs: SubView[] = [];
  const restoredStreams: Record<string, SubStream> = {};
  for (const m of msgs) {
    if (m.role === "user") {
      const text = m.content.filter((c) => c.type === "text").map((c: any) => c.text).join("\n");
      // 附件图片同样恢复（data 不带 data: 前缀，与发送管线同构）
      const imgs = m.content
        .filter((c) => c.type === "image")
        .map((c: any) => ({ mediaType: (c as any).media_type as string, data: (c as any).data as string }));
      if (text === "<run-cancelled/>" || text.includes("handoff-summary")) {
        out.push({ kind: "notice", text: text === "<run-cancelled/>" ? i18n.t("notice.cancelled") : i18n.t("notice.compactSummary") });
      } else {
        out.push({ kind: "user", text, createdAt: m.created_at ?? undefined, images: imgs.length ? imgs : undefined });
      }
    } else if (m.role === "assistant") {
      // 历史内容块已按生成顺序落盘：text/thinking 进 timeline，tool_use 落到锚点（思考不再丢弃）
      const timeline: TimelineSeg[] = [];
      const toolsMap: Record<string, ToolView> = {};
      for (const c of m.content) {
        if (c.type === "text") {
          appendDelta(timeline, "text", (c as any).text ?? "");
        } else if (c.type === "thinking") {
          appendDelta(timeline, "thinking", (c as any).text ?? "");
        } else if (c.type === "tool_use") {
          const callKey = (c as any).id as string;
          const args: any = (c as any).args ?? {};
          if ((c as any).name === "subagent") {
            // [docs/subagent-interaction-drawer](../../../docs/subagent-interaction-drawer.md)：subagent 调用 → 子代理卡片锚点（替代通用工具卡）；归档卡可点击回看完整过程
            let subId: string | null = null;
            let report: string | undefined;
            try {
              const d = JSON.parse(toolResults[callKey]?.content ?? "");
              if (d && typeof d.sub_id === "string") subId = d.sub_id;
              if (d && typeof d.report === "string") report = d.report;
            } catch {
              /* 旧会话 outcome 被截断/缺失：回退合成 key；抽屉降级展示 task + report（无过程流） */
            }
            const key = subId ?? `restored:${callKey}`;
            // 记下「callKey → 当前 sub key」：合成 key 待 sidecar 回填后改名成真实 sub_id（见 renameRestoredSubs）
            subByKey[callKey] = key;
            timeline.push({ kind: "sub", subId: key });
            restoredSubs.push({
              subId: key,
              role: args.role ?? "agent",
              name: null,
              description: args.description ?? args.role ?? "",
              task: typeof args.task === "string" ? args.task : undefined,
              step: 0,
              maxSteps: typeof args.maxSteps === "number" ? args.maxSteps : 25,
              tokens: 0,
              lastTools: [],
              status: "done",
              report,
            });
            restoredStreams[key] = { timeline: [], toolsMap: {}, status: "done", gen: 0, loaded: false };
          } else {
            timeline.push({ kind: "tool", callKey });
            // 出参尽量还原（restoredToolData）：read 读图卡片的图片条目里只剩 path/kind/media_type，
            // 卡片据此按路径重新加载图片；解析不出来才退回占位（下面按需从 sidecar 回填）。
            // 入参同样填上（safeArgsPreview）：摘要行、ask 问答行的题干、edit 的 diff 都靠它。
            const stored = toolResults[callKey];
            const data = stored ? restoredToolData(stored.content) : { restored: true };
            // 「无结果 / 被中断」（后端 repair 补的 [interrupted] 文本，或整条结果缺失）与子代理流的
            // 「已中断」语义对齐——此前一律按「已使用」呈现，与事实不符
            //（[docs/tool-call-card-live-key](../../../docs/tool-call-card-live-key.md) 的既有遗留）。
            // 注意不能用 is_error 当判据：它同样覆盖真实失败（E_EXIT_CODE 等），那些要走下面的错误码还原。
            const interrupted =
              !stored || stored.content.trimStart().startsWith("[interrupted]");
            // 失败结果：模型侧文本形如 `[error E_XXX: 说明]`，把错误码与说明还原到卡片上，
            // 而不是留一个「已使用 + 空数据」的假象（原始 outcome 早已不在历史里）
            const err = stored
              ? /^\[error ([A-Z_]+): ([\s\S]*)\]$/.exec(stored.content.trimStart())
              : null;
            // 「取不回出参」（restored 占位）的调用由末尾的 lossyToolKeys 统一收集后批量补回
            toolsMap[callKey] = {
              callKey,
              tool: (c as any).name,
              status: interrupted || err ? "error" : "ok",
              outcome: interrupted
                ? { ok: false, error: { code: "E_INTERRUPTED", message: "" } }
                : err
                  ? { ok: false, error: { code: err[1], message: err[2] } }
                  : { ok: true, data },
              argsPreview: safeArgsPreview((c as any).args),
              progressTail: "",
            };
          }
        }
      }
      if (timeline.length) out.push({ kind: "assistant", timeline, toolsMap, streaming: false, createdAt: m.created_at ?? undefined });
    }
  }
  return {
    items: out,
    // 稳定键：段号 + 段内序号（同一份消息每次加载得到同一批键）
    keys: out.map((_, n) => `s${seq}:${n}`),
    subs: restoredSubs,
    streams: restoredStreams,
    subByKey,
  };
}

export const useRun = create<RunStore>()(
  immer((set, get) => ({
    tabs: {},
    drafts: {},

    // 仅供动作内部使用（返回可变引用生效前的快照；组件读取请用 useActiveRun）
    st(sessionId) {
      const key = sessionId ?? useSessions.getState().activeKey ?? "";
      if (!get().tabs[key]) {
        set((s) => {
          s.tabs[key] = blank();
        });
      }
      return get().tabs[key]!;
    },

    initTab(sessionId) {
      if (get().tabs[sessionId]) return;
      set((s) => {
        s.tabs[sessionId] = blank();
      });
    },

    dispose(sessionId) {
      // Tab 关闭时删除状态桶（连同完整转录与 git 缓存），长时间运行也能约束内存；草稿平行桶一并丢弃
      set((s) => {
        delete s.tabs[sessionId];
        delete s.drafts[sessionId];
      });
    },

    // M4：重开仍在后端运行中的会话（关 Tab 不打断运行）时，恢复运行态
    markRunning(sessionId) {
      set((s) => {
        if (!s.tabs[sessionId]) s.tabs[sessionId] = blank();
        s.tabs[sessionId].running = true;
      });
    },

    setBreakdown(sessionId, b) {
      set((s) => {
        if (!s.tabs[sessionId]) s.tabs[sessionId] = blank();
        s.tabs[sessionId].breakdown = b as any;
      });
    },

    pushItem(sessionId, item) {
      const key = sessionId ?? useSessions.getState().activeKey ?? "";
      set((s) => {
        if (!s.tabs[key]) s.tabs[key] = blank();
        s.tabs[key].items.push(item as any);
      });
    },

    // 返回消息是否被接受（M-2：无会话/失败时返回 false，由调用方决定是否清空输入框；
    // [docs/run-queue-and-ask-revamp](../../../docs/run-queue-and-ask-revamp.md)：运行中提交 = 入队；targetKey 让队列出队可以定向回源会话）
    async send(text, images, targetKey) {
      const sessions = useSessions.getState();
      text = text.trim();
      const curKey = targetKey ?? sessions.activeKey ?? "";
      const cur = get().tabs[curKey];
      if (!text) return false;

      if (!sessions.activeKey && !targetKey) {
        // 一切入口都在导航：无会话绝不自动创建，引导用户去左栏
        useUi.getState().toast(i18n.t("notice.needSession"));
        return false;
      }
      const sessionId = targetKey ?? useSessions.getState().activeKey!;
      // [docs/run-queue-and-ask-revamp](../../../docs/run-queue-and-ask-revamp.md)：运行中提交 → 入队（文本 + 附件原样保存；出队后走完整 send 管线）
      if (cur?.running) {
        set((s) => {
          const t = s.tabs[curKey];
          if (!t) return;
          t.queue.push({
            id: crypto.randomUUID(),
            text,
            images: images?.length ? images : undefined,
          });
        });
        return true;
      }
      set((s) => {
        if (!s.tabs[sessionId]) s.tabs[sessionId] = blank();
        const t = s.tabs[sessionId];
        // 本地回显含图片缩略图（后端落盘由 startChat 负责；重开会话经 restoreFromMessages 恢复）
        t.items.push({
          kind: "user",
          text,
          createdAt: new Date().toISOString(),
          images: images?.length ? images.map((im) => ({ mediaType: im.mime, data: im.data })) : undefined,
        });
        t.running = true;
        t.suggestions = [];
        // 本轮计数归零（[docs/composer-token-rate](../../../docs/composer-token-rate.md)）：
        // 会话级 usage 跨 run 累加（命中率需要），而「本轮速率 / 本轮工具等待」必须从零重建，
        // 否则数字跨轮累积失真；归零后尚未收到 usage 帧（genMs = 0）→ 速率段隐藏，即「新 run 覆盖」。
        t.runMetrics = { output: 0, genMs: 0, steps: 0, ttftMs: null, toolMs: 0 };
        // [docs/subagent-interaction-drawer](../../../docs/subagent-interaction-drawer.md)：子代理卡片与消息流保持归档（跨运行可回看），不再随新运行重置
      });
      const channel = new Channel<any>();
      channel.onmessage = (frame) => {
        set((s) => {
          const t = s.tabs[sessionId];
          if (!t) return;
          // 帧归一统一走 applyFrameToTab（帧到达顺序 = timeline 展示顺序）
          applyFrameToTab(t, frame);
        });
      };
      try {
        const runId = await ipc.startChat(sessionId, text, images ?? [], channel);
        // run_id 落进运行态（与 resumeGoal 同口径；失败路径不写）
        set((s) => {
          const t = s.tabs[sessionId];
          if (t) t.runId = runId;
        });
        return true;
      } catch (e) {
        set((s) => {
          const t = s.tabs[sessionId];
          if (!t) return;
          t.running = false;
          t.items.push({ kind: "error", text: String(e) });
        });
        return false;
      }
    },

    async cancel(sessionId) {
      const sid = sessionId ?? useSessions.getState().activeKey;
      if (sid && get().tabs[sid]?.running) {
        await ipc.cancelRun(sid).catch(() => {});
      }
    },

    async stopSubagent(sessionId, subId) {
      const sid = sessionId ?? useSessions.getState().activeKey;
      if (!sid) return;
      await ipc.stopSubagent(sid, subId).catch(() => {});
    },

    async resolveAsk(askId, value) {
      const sessionId = useSessions.getState().activeKey;
      if (!sessionId) return;
      await ipc.resolveAsk(sessionId, askId, value);
    },

    /** 目标模式：暂停后继续推进。守卫取**语义**而非 running——暂停态下 running 本就为假，
     *  用 running 守卫会让「继续推进」按钮永远点不动；只允许「已暂停」的目标续跑，
     *  执行中重复 resume 无意义，已达成/已中止更不该被续跑。
     *  调用形态与 `send()` 同构（后端 `resume_goal` 与 `start_chat` 同签名）：注册事件 channel、
     *  本地置运行态、run_id 落进运行态；目标状态（含起跑失败的回滚）全由后端 `goal:update` 下发。 */
    async resumeGoal(sessionId) {
      const sid = sessionId ?? useSessions.getState().activeKey;
      if (!sid) return;
      if (get().tabs[sid]?.goal?.status !== "paused" || get().tabs[sid]?.running) return;
      // 本地回显的落点（起跑失败时按位置撤回这一条）：期间的帧只会往后追加，不会挪动它
      const echoAt = get().tabs[sid].items.length;
      set((s) => {
        const t = s.tabs[sid];
        if (!t) return;
        // 本地回显（同 send() 的用户项）：后端 `resume_goal` 会把 RESUME_GOAL_TEXT 写进会话历史再起 run，
        // 这里同步补一条用户项——否则实时视图里只有助手输出、找不到对应的用户消息，
        // 只有重开会话（从历史恢复）才会出现那条。文案见文件顶部 RESUME_GOAL_TEXT 的注释。
        t.items.push({
          kind: "user",
          text: RESUME_GOAL_TEXT,
          createdAt: new Date().toISOString(),
        });
        t.running = true;
        // 新一轮 run 的计数从零重建（同 send()）：否则工具条速率段会带着上一轮的数字
        t.suggestions = [];
        t.runMetrics = { output: 0, genMs: 0, steps: 0, ttftMs: null, toolMs: 0 };
      });
      const channel = new Channel<any>();
      channel.onmessage = (frame) => {
        set((s) => {
          const t = s.tabs[sid];
          if (!t) return;
          applyFrameToTab(t, frame);
        });
      };
      try {
        const runId = await ipc.resumeGoal(sid, channel);
        set((s) => {
          const t = s.tabs[sid];
          if (t) t.runId = runId;
        });
      } catch (e) {
        // 起跑失败（未配置模型等）：后端已把目标回滚为「已暂停」并下发 `goal:update`，此处只落错误
        set((s) => {
          const t = s.tabs[sid];
          if (!t) return;
          // 撤回本地回显：run 没起跑 = 后端没往历史里写这条标记，气泡留下就与「重开会话后的形态」不一致
          if (t.items[echoAt]?.kind === "user") t.items.splice(echoAt, 1);
          t.running = false;
          t.items.push({ kind: "error", text: String(e) });
        });
      }
    },

    async reopenGoal(sessionId) {
      const sid = sessionId ?? useSessions.getState().activeKey;
      if (!sid || get().tabs[sid]?.goal?.status !== "paused") return;
      try {
        await ipc.reopenGoal(sid);
        // 后端会推 goal:update；回读兜住切 Tab 时漏接事件的情况。
        await get().syncGoal(sid);
      } catch (e) {
        useUi.getState().toast(String(e));
      }
    },

    /** 目标状态初值回读（会话打开 / 惰性激活时；`goal:update` 推送仍是唯一的持续更新源）。
     *  推送优先：回读期间收到推送（goalRev 已前进）则丢弃本次结果——回读只补初值，
     *  不得用更旧的值盖掉推送（否则刚推进到「执行中」的目标会被读回的「澄清中」打回去）。 */
    async syncGoal(sessionId) {
      const rev = get().tabs[sessionId]?.goalRev ?? 0;
      let goal: GoalState | null = null;
      try {
        goal = await ipc.getSessionGoal(sessionId);
      } catch {
        return; // 拉取失败保持现状：推送与后续运行仍会刷新
      }
      if ((get().tabs[sessionId]?.goalRev ?? 0) !== rev) return; // 期间有推送 → 本次结果已过期
      set((s) => {
        const t = s.tabs[sessionId];
        if (!t) return; // 已关 Tab：不重建状态桶（M-1）
        // null = 该会话当前无目标：写成 null 而不是留旧值（字段契约 `GoalState | null`）
        t.goal = (goal ?? null) as any;
      });
    },
    // [docs/run-queue-and-ask-revamp](../../../docs/run-queue-and-ask-revamp.md)：出队并运行下一条（run:done 后自动调用；error/cancelled 的暂停态由「继续」恢复）
    async runQueueNext(sessionId) {
      const t = get().tabs[sessionId];
      if (!t || t.running || t.queue.length === 0) return;
      const next = t.queue[0];
      set((s) => {
        const t = s.tabs[sessionId];
        if (t) t.queue = t.queue.slice(1);
      });
      await get().send(next.text, next.images, sessionId);
    },

    // [docs/run-queue-and-ask-revamp](../../../docs/run-queue-and-ask-revamp.md)：立即运行——空闲时直接执行；运行中则打断当前运行（以 pendingItemId 标记，待 run:cancelled 后执行）
    async runNow(sessionId, id) {
      const t = get().tabs[sessionId];
      const item = t?.queue.find((q) => q.id === id);
      if (!item) return;
      if (t.running) {
        set((s) => {
          const t = s.tabs[sessionId];
          if (t) t.pendingItemId = id;
        });
        await get().cancel(sessionId);
      } else {
        set((s) => {
          const t = s.tabs[sessionId];
          if (t) t.queue = t.queue.filter((q) => q.id !== id);
        });
        await get().send(item.text, item.images, sessionId);
      }
    },

    removeQueueItem(sessionId, id) {
      set((s) => {
        const t = s.tabs[sessionId];
        if (t) t.queue = t.queue.filter((q) => q.id !== id);
      });
    },

    reorderQueue(sessionId, fromId, toId) {
      set((s) => {
        const t = s.tabs[sessionId];
        if (!t || fromId === toId) return;
        const from = t.queue.findIndex((q) => q.id === fromId);
        const to = t.queue.findIndex((q) => q.id === toId);
        if (from < 0 || to < 0) return;
        const [moved] = t.queue.splice(from, 1);
        t.queue.splice(to, 0, moved);
      });
    },
    editQueueItem(sessionId, id) {
      const t = get().tabs[sessionId];
      const item = t?.queue.find((q) => q.id === id);
      if (!item) return;
      set((s) => {
        const t = s.tabs[sessionId];
        if (t) {
          t.queue = t.queue.filter((q) => q.id !== id);
          // 附件随文本一并回填（此前只回填文本，编辑会丢附件）
          t.draftFromQueue = { text: item.text, images: item.images };
        }
      });
    },

    consumeDraftFromQueue(sessionId) {
      set((s) => {
        const key = sessionId ?? useSessions.getState().activeKey ?? "";
        const t = s.tabs[key];
        if (t) t.draftFromQueue = null;
      });
    },

    // Composer 草稿（平行分桶）：缺省落当前活跃 Tab（composer 仅在活跃会话渲染；ws:composer-fill/insert
    // 事件也落在事件到达时的活跃会话，语义一致）；异步回填路径（发送清空、队列编辑）传显式 key 防切 tab 竞态。
    // 桶缺失时防御创建（测试 standalone 挂载路径）
    setDraftText(v, sessionId) {
      set((s) => {
        const key = sessionId ?? useSessions.getState().activeKey ?? "";
        const d = s.drafts[key] ?? (s.drafts[key] = { text: "", images: [], refs: [] });
        d.text = typeof v === "function" ? v(d.text) : v;
      });
    },

    setDraftImages(v, sessionId) {
      set((s) => {
        const key = sessionId ?? useSessions.getState().activeKey ?? "";
        const d = s.drafts[key] ?? (s.drafts[key] = { text: "", images: [], refs: [] });
        d.images = (typeof v === "function" ? v(d.images as PendingImage[]) : v) as PendingImage[];
      });
    },

    setDraftRefs(v, sessionId) {
      set((s) => {
        const key = sessionId ?? useSessions.getState().activeKey ?? "";
        const d = s.drafts[key] ?? (s.drafts[key] = { text: "", images: [], refs: [] });
        // 旧快照回填的桶可能没有 refs（ui-state schema 向前兼容）
        const prev = d.refs ?? [];
        d.refs = typeof v === "function" ? v(prev) : v;
      });
    },

    clearDraft(sessionId) {
      set((s) => {
        const key = sessionId ?? useSessions.getState().activeKey ?? "";
        const d = s.drafts[key];
        if (!d) return;
        d.text = "";
        d.images = [];
        d.refs = [];
      });
    },

    onToolResult(sessionId, p, ok) {
      set((s) => {
        const t = s.tabs[sessionId];
        // [docs/subagent-interaction-drawer](../../../docs/subagent-interaction-drawer.md)：子代理内部的工具结果（session = sub_id，无对应 Tab）→ 路由进所属会话的子代理流
        if (!t) {
          const owner = Object.values(s.tabs).find((tb) => tb.subStreams[sessionId]);
          const st = owner?.subStreams[sessionId];
          if (!owner || !st) return;
          if (ok && (p.tool === "create" || p.tool === "edit")) {
            owner.writeTick += 1; // 子代理的文件写同样记在主会话名下（docs/session-artifacts-and-files-tab 语义）
          }
          const tool = ensureToolAnchorIm(st, p.call_key);
          tool.tool = p.tool;
          tool.status = ok ? "ok" : "error";
          tool.outcome = p.outcome as any;
          tool.argsPreview = p.args_preview;
          tool.durationMs = p.duration_ms;
          // 注意：**不**把子代理内部工具耗时累加进 owner.runMetrics.toolMs
          // （[docs/composer-token-rate](../../../docs/composer-token-rate.md)）。
          // 主管道的 `subagent` 工具卡已把整个子代理运行的时长计过一次，再叠加内部工具即双计；
          // 且工具条「工具等待」只反映主会话自己等过的工具。
          // 结果落定：清掉流式期间的进度尾部。结果卡自带完整输出且只在 running 时展示尾部，
          // 残留的 tail 已无意义（且是 ANSI/裂字符的载体）
          tool.progressTail = "";
          return;
        }
        // [docs/session-artifacts-and-files-tab](../../../docs/session-artifacts-and-files-tab.md)：每次成功文件写递增信号；文件页签据此去抖拉取最新登记
        if (ok && (p.tool === "create" || p.tool === "edit")) {
          t.writeTick += 1;
        }
        // 先查后建：工具卡锚点可能落在更早的 assistant 项（跑批期间 notice 插队会另建末项），
        // 只查末项会另建第二张卡、旧卡永久 running。命中即原地落定，不白建流式项
        const tool = findToolViewInItems(t.items, p.call_key) ?? ensureToolAnchorIm(currentAssistantIm(t), p.call_key);
        tool.tool = p.tool;
        tool.status = ok ? "ok" : "error";
        tool.outcome = p.outcome as any;
        tool.argsPreview = p.args_preview;
        tool.durationMs = p.duration_ms;
        // 本轮工具等待累加（[docs/composer-token-rate](../../../docs/composer-token-rate.md)）：只算**本轮**已落定的工具卡——
        // 历史（会话恢复）的工具卡由 restoreFromMessages 直接构造、不经本路径，故天然不计入（AC-11）。
        // 不惰性新建 runMetrics：无本轮计数时凭空造桶会让「无数据」变成「有数据但为 0」。
        if (t.runMetrics) t.runMetrics.toolMs += p.duration_ms ?? 0;
        // 同上：落定即清进度尾部（主会话与子代理流两处同步）
        tool.progressTail = "";
      });
    },

    onToolStart(sessionId, p) {
      set((s) => {
        const t = s.tabs[sessionId];
        // 子代理内部的工具开始（session = sub_id，无对应 Tab）→ 路由进所属会话的子代理流（与 onToolResult 同构）
        if (!t) {
          const owner = Object.values(s.tabs).find((tb) => tb.subStreams[sessionId]);
          const st = owner?.subStreams[sessionId];
          // 流已收尾（done/error）：迟到的开始事件不得再把卡翻回在途
          if (!owner || !st || st.status !== "running") return;
          applyToolStart(st, p);
          return;
        }
        // run 已结束的迟到开始事件不得再造出会永久转圈的卡（同 review C1 的幽灵指示守卫）
        if (!t.running) return;
        // 与 onToolResult / tool_progress 同一守卫：先跨 assistant 项找已有卡。
        // 否则 waiting→running 两相之间插入 notice（run:inject / sub:error）时，running 相会落到新项，
        // 为同一次调用建出第二张卡（旧卡永久 running，结果事件只翻中第一张）
        const hit = findToolViewInItems(t.items, p.call_key);
        if (hit) updateToolFromStart(hit, p);
        else applyToolStart(currentAssistantIm(t), p);
      });
    },

    restoreFromMessages(sessionId, msgs, meta) {
      const built = buildTranscript(msgs, meta?.paging?.loaded_from_seq ?? 0);
      set((s) => {
        if (!s.tabs[sessionId]) s.tabs[sessionId] = blank();
        const t = s.tabs[sessionId];
        t.items = built.items as any;
        // 稳定渲染键（AC-14）：与 items 头部对齐。用数组下标作 key 时，分页前插会让所有节点错位
        t.itemKeys = built.keys;
        // 压缩边界（批2 P2）：只存数据、不存下标——位置由渲染侧按稳定键的段号现算（见 boundaryAtIndexes）。
        // 没有该字段（旧后端 / legacy / 从未压缩）⇒ 空数组，界面上一个字都不多。
        t.boundaries = meta?.boundaries ?? [];
        // 首屏是**唯一**建立分页游标的地方（前翻只推进游标）；没有 paging（旧后端 / 未分页）⇒ null
        t.paging = meta?.paging ? toPaging(meta.paging, built.items.length > 0 ? 1 : 0) : null;
        // [docs/session-history-limits](../../../docs/session-history-limits.md)：索引上的历史保存状态（重启后仍在）→ 转录末尾补一条提示。
        // 与历史重建同批写入（不额外触发一次渲染）；后端下一次干净保存后 status 会清空，
        // 但**已经 push 进转录的这条 notice 不会撒回**——要它消失得重开该会话（本次设计如此：
        // 不做「已读」交互、不加本地持久化，只要状态还在就每次打开都显示）。
        // P4 的体积状态同理：体积回落后后端会自愈清掉状态，但已 push 的这条不撒回。
        const historyNotice = historyStatusNotice(meta?.history_status);
        if (historyNotice) t.items.push({ kind: "notice", text: historyNotice });
        // 注册归档子代理卡（流先留空，抽屉首次打开按需拉取过程流，防止重复导入）
        for (const sv of built.subs) {
          if (!t.subs.some((x) => x.subId === sv.subId)) t.subs.push(sv);
        }
        for (const [k, v] of Object.entries(built.streams)) {
          if (!t.subStreams[k]) t.subStreams[k] = v as any;
        }
      });

      // 完整出参回填（[docs/session-restore-fidelity](../../../docs/session-restore-fidelity.md)）：
      // 只对「历史文本解析失败」的调用发起一次批量 IPC（lossyToolKeys 与 restoredToolData 同判据）；
      // 子代理卡顺带把合成 key 改名成真实 sub_id 并补回被截断的 report。
      backfillToolOutcomes(set, sessionId, lossyToolKeys(msgs), undefined, built.subByKey);
    },

    /** 加载更早**一段**历史（批2 P3 分页前翻）：把返回的一段**前置**到转录头部。
     *
     *  与首屏走**同一套**消息→项转换（buildTranscript），不另写一份——工具卡还原口径、子代理合成 key、
     *  notice 折叠规则都在那里，两套写法必随时间漂移。
     *  失败与空返回（后端对越界 / 已到最早 / 段损坏一律不报错，回 `from_seq: 0`）都不动已有转录，只收敛游标。 */
    async loadEarlier(sessionId) {
      const sid = sessionId ?? useSessions.getState().activeKey;
      if (!sid) return;
      const p = get().tabs[sid]?.paging;
      // 防重入 + 只在「段式 + 还有更早 + 未到内存上限」时真的发请求
      if (!p || p.format !== "new" || !p.hasMore || p.loading || p.loadedPages >= MAX_PAGED_PAGES) return;
      set((s) => {
        const t = s.tabs[sid];
        if (t?.paging) {
          t.paging.loading = true;
          t.paging.failed = false;
        }
      });
      try {
        const page = await ipc.loadSessionEarlier(sid, p.loadedFromSeq);
        // 空返回 / `from_seq = 0`：后端刻意不报错 → 收敛为「没有更早内容了」（不是失败）
        if (page.from_seq === 0 || page.messages.length === 0) {
          set((s) => {
            const t = s.tabs[sid];
            if (t?.paging) {
              t.paging.loading = false;
              t.paging.hasMore = false;
            }
          });
          return;
        }
        const built = buildTranscript(page.messages, page.from_seq);
        // 本页带回的压缩边界（批2 P2）：契约只含**本次覆盖段范围内**的边界，故不会既指向前翻段又指首屏段
        const pageBoundaries = page.boundaries ?? [];
        set((s) => {
          const t = s.tabs[sid];
          if (!t?.paging) return;
          // 前置：更早一段摆在头部。后端保证严格早于 before_seq（不重复），且整段给出（不丢）
          t.items = [...built.items, ...t.items] as any;
          t.itemKeys = [...built.keys, ...(t.itemKeys ?? [])];
          // 归档子代理卡与过程流：与首屏同一套合并规则（同 subId 不重复注册）
          for (const sv of built.subs) if (!t.subs.some((x) => x.subId === sv.subId)) t.subs.push(sv);
          for (const [k, v] of Object.entries(built.streams)) if (!t.subStreams[k]) t.subStreams[k] = v as any;
          t.paging.loadedFromSeq = page.from_seq;
          t.paging.hasMore = page.has_more;
          // 坏段计数：首屏值 + 本次回传值（后端 `EarlierPage` 也已回传 `bad_segments`，缺省兼容）
          if (page.bad_segments) t.paging.badSegments = (t.paging.badSegments ?? 0) + page.bad_segments;
          // 无条件合并：本页无边界且原也无边界时也落成 []，让「边界数组」始终是数组而非 undefined
          t.boundaries = mergeBoundaries(t.boundaries, pageBoundaries);
          t.paging.loadedPages += 1;
          t.paging.loading = false;
          t.paging.failed = false;
        });
        // 与首屏同一条回填链路（前翻回来的工具卡同样只有被截断的模型侧文本）
        backfillToolOutcomes(set, sid, lossyToolKeys(page.messages), undefined, built.subByKey);
      } catch {
        // 失败：入口保留可重试（failed 标记），已有转录一字不动
        set((s) => {
          const t = s.tabs[sid];
          if (t?.paging) {
            t.paging.loading = false;
            t.paging.failed = true;
          }
        });
      }
    },

    /** 收起更早的（批2 P3 AC-15 内存有界）：丢掉前翻加载的旧段，回到首屏那一段。
     *  判据是**渲染键的段前缀**（`s<首屏段号>:`）——只有前插过的内容才有更早的键，故不必另存边界下标。 */
    collapseEarlier(sessionId) {
      const sid = sessionId ?? useSessions.getState().activeKey;
      if (!sid) return;
      set((s) => {
        const t = s.tabs[sid];
        const p = t?.paging;
        if (!t || !p) return;
        const prefix = `s${p.firstLoadedSeq}:`;
        const keys = t.itemKeys ?? [];
        const at = keys.findIndex((k) => k.startsWith(prefix));
        if (at <= 0) return; // 没加载更早内容（或键已丢失）→ 无可收起
        t.items = t.items.slice(at);
        t.itemKeys = keys.slice(at);
        // 边界随前翻的段一起收起：段号早于首屏的边界已无对应内容（渲染侧还另有一道 minSeg 守卫，
        // 防的正是「state 里残留旧边界却在顶端伪出一条分隔线」）
        if (t.boundaries) t.boundaries = t.boundaries.filter((b) => b.seq >= p.firstLoadedSeq);
        // 收起后确实还有更早内容（上面证明 at > 0），计数归位到首屏
        t.paging = { ...p, loadedFromSeq: p.firstLoadedSeq, hasMore: true, loadedPages: 1, loading: false, failed: false };
      });
    },
    /** [docs/subagent-interaction-drawer](../../../docs/subagent-interaction-drawer.md)：打开子代理过程抽屉。归档子代理（会话恢复 / 旧运行）首次打开时按需
     *  拉取过程历史重建消息流；运行中子代理直接显示实时流。 */
    async openSubDrawer(sessionId, subId) {
      const sid = sessionId ?? useSessions.getState().activeKey;
      if (!sid) return;
      set((s) => {
        if (!s.tabs[sid]) s.tabs[sid] = blank();
        s.tabs[sid].subDrawer = { open: true, subId };
      });
      const t = get().tabs[sid];
      const st = t?.subStreams[subId];
      if (!st || st.loaded || st.status === "running") return;
      try {
        const msgs = await ipc.loadSubagentHistory(sid, subId);
        set((s) => {
          const t = s.tabs[sid];
          const stream = t?.subStreams[subId];
          if (!t || !stream || stream.loaded) return;
          if (stream.timeline.length === 0 && msgs.length > 0) {
            // 流所属会话已不在运行 → 无结果调用落「已中断」（运行中拉回的历史留给实时事件回填）
            const settleRunning = !get().tabs[sid]?.running;
            const mapped = messagesToSubStream(msgs, settleRunning);
            stream.timeline = mapped.timeline as any;
            stream.toolsMap = mapped.toolsMap as any;
          }
          stream.loaded = true;
        });
        // 过程流里的工具卡同样按 sidecar 回填（子代理历史存的也是被截断的模型侧文本）
        backfillToolOutcomes(set, sid, lossyToolKeys(msgs), subId);
      } catch {
        /* 拉取失败保持降级展示（task + 最终报告） */
      }
    },

    closeSubDrawer(sessionId) {
      const sid = sessionId ?? useSessions.getState().activeKey;
      if (!sid) return;
      set((s) => {
        const t = s.tabs[sid];
        // 关闭不销毁：流数据留在 subStreams，可经卡片/指示器再次进入
        if (t) t.subDrawer = { open: false, subId: t.subDrawer.subId };
      });
    },

    async refreshGit(sessionId = null) {
      const id = sessionId ?? useSessions.getState().activeKey;
      if (!id) return;
      try {
        const gitEntries = await ipc.gitStatus(id);
        set((s) => {
          if (!s.tabs[id]) s.tabs[id] = blank();
          s.tabs[id].gitEntries = gitEntries as any;
        });
      } catch {
        set((s) => {
          if (s.tabs[id]) s.tabs[id].gitEntries = null;
        });
      }
    },

    // ---------- 事件路由（事件面 = 契约测试锚点；键名不可增删） ----------
    // handler 体以族为单位收敛在 runHandlers.ts 的工厂里；本方法保持唯一注册点
    // （AppShell 经 bindEvents 绑定一次），键集合不得变化。
    bindGlobalHandlers() {
      return {
        ...runLifecycleHandlers(set, get),
        ...compactHandlers(set),
        ...toolHandlers(get),
        ...askHandlers(set, get),
        ...subHandlers(set),
        ...miscHandlers(set),
      };
    },
  })),
);

// ---------- 组件订阅 helper ----------
/** 活跃 Tab 的运行态（缺桶时回退共享 blank 快照）。 */
export function useActiveRun(): TabRunState {
  const activeKey = useSessions((s) => s.activeKey);
  return useRun((s) => s.tabs[activeKey ?? ""] ?? BLANK);
}

/** 活跃 Tab 的 Composer 草稿（无桶时回退共享空草稿）。平行分桶：击键不换 tabs[key] 身份，
 *  订阅者只有 Composer 自身，不把重渲染广播给 ChatMessages 等整桶订阅者。 */
const EMPTY_DRAFT: ComposerDraft = { text: "", images: [], refs: [] };
export function useActiveDraft(): ComposerDraft {
  const activeKey = useSessions((s) => s.activeKey);
  return useRun((s) => s.drafts[activeKey ?? ""] ?? EMPTY_DRAFT);
}

/** 活跃会话的上下文占用百分比（保留一位小数）。 */
export function useContextPct(): number {
  const active = useActiveRun();
  return active.breakdown ? Math.round(active.breakdown.ratio * 1000) / 10 : 0;
}

// 供契约测试等非 React 场景使用
export function activeRunOf(state: RunStore, activeKey: string | null): TabRunState {
  return state.tabs[activeKey ?? ""] ?? BLANK;
}
