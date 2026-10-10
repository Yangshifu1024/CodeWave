// 后端事件 handler 工厂 —— 29 键事件面（契约测试锚点；键名不可增删）。
// 自 run.ts 拆出（[docs/fence-hardening-and-powershell-ast](../../../docs/fence-hardening-and-powershell-ast.md) 重构）：每族是一个 (set, get) => handler-record 工厂；
// run.ts 的 bindGlobalHandlers 保持唯一注册点并展开它们，
// Object.keys(bindGlobalHandlers()) 必须与拆分前事件面逐字节一致。
import type {
  AskOpenedEvent,
  HistorySaveReport,
  HistoryStatus,
  McpStatusPayload,
  SubagentEvent,
} from "../ipc/types";
import type { WritableDraft } from "immer";
import { ipc } from "../ipc/client";
import { titleOf, useSessions } from "./sessions";
import { useUi } from "./ui";
import { useTasks } from "./tasks";
import { i18n } from "../i18n";
import { blank, closeRunningTools, closeStreamingAssistantItems, currentAssistantIm, hasRunningTools, stripRecoveredInTab } from "./runFrames";
import type { RunStore } from "./run";
import type { TabRunState, UiItem } from "./run.types";

/** immer set：对 store 草稿原地变异 */
type SetFn = (fn: (s: WritableDraft<RunStore>) => void) => void;
/** 读取当前 store 快照 */
type GetFn = () => RunStore;

let notifyReady = false;
// 系统通知：项目会话后端优先（Windows/macOS 原生直驱，点击 → notify:activate → 回跳会话），失败回退插件；临时会话（无 session）直接走插件
async function systemNotify(sessionId: string | null, title: string, body: string) {
  if (sessionId) {
    try {
      await ipc.notifySystem(sessionId, title, body);
      return;
    } catch {
      /* 后端不支持/失败 → 插件路径 */
    }
  }
  try {
    const { isPermissionGranted, requestPermission, sendNotification } =
      await import("@tauri-apps/plugin-notification");
    if (!notifyReady) {
      if (!(await isPermissionGranted())) {
        notifyReady = (await requestPermission()) === "granted";
      } else {
        notifyReady = true;
      }
    }
    if (notifyReady) sendNotification({ title, body });
  } catch {
    /* 通知失败保持静默 */
  }
}

/** [docs/ask-ink-accent-and-composer-cover](../../../docs/ask-ink-accent-and-composer-cover.md)：会话结束未读点。必须在 run:done/run:error 幂等守卫之前调用——
 *  已关闭 Tab（桶已删）收到的迟到结束事件也要标上，否则左栏行永远收不到「有新结果」信号 */
function markUnreadIfAway(session: string | undefined) {
  if (!session) return;
  const s = useSessions.getState();
  if (s.activeKey !== session) s.markUnread(session);
}

/** 历史保存不干净的会话内提示（[docs/session-history-limits](../../../docs/session-history-limits.md)）：
 *  ① `run:done.history_save`（当次保存结果，仅当保存不干净时后端才带上）——干净返回 null，零打扰；
 *  ② `SessionMeta.history_status`（挂在索引上，重启后仍在）——恢复历史时用同一套文案，保证两处口径一致。
 *
 *  P4（历史体积约束）新增两个**体积**变体：`warned`（超软线，**照常写**）与 `fused`
 *  （超硬线，**停止写入**但绝不删既有历史），两者都带 `bytes` / `threshold`。
 *  四个变体同属 `ui/src/ipc/types.ts` 的 `HistoryStatus` 判别联合，按 `kind` 收窄即可。 */

/** 字节 → 人类可读（KB / MB / GB，一位小数）。格式化在前端做：后端只给数字，不塞格式化字符串。 */
function formatBytes(bytes?: number | null): string {
  const n = typeof bytes === "number" && Number.isFinite(bytes) && bytes > 0 ? bytes : 0;
  if (n >= 1024 ** 3) return `${(n / 1024 ** 3).toFixed(1)} GB`;
  if (n >= 1024 ** 2) return `${(n / 1024 ** 2).toFixed(1)} MB`;
  return `${Math.max(1, Math.round(n / 1024))} KB`;
}

export function historySaveNotice(h: HistorySaveReport): string | null {
  if (h.saved === false) return i18n.t("notice.historyRejected");
  // P4：体积裁决随载荷一起来（`history_status`，与索引侧同源）→ 与恢复路径共用文案
  if (h.history_status) return historyStatusNotice(h.history_status);
  // 旧载荷 / 无状态：仍按计数文案兜底（剥图 / 丢轮）
  const images = h.stripped_images ?? 0;
  const rounds = h.dropped_rounds ?? 0;
  if (images > 0 || rounds > 0) return i18n.t("notice.historyDegraded", { images, rounds });
  return null;
}

/** 索引里的历史状态 → 会话内提示文案（无状态 = 干净 = null）。语义同 historySaveNotice：
 *  rejected = 磁盘上仍是上一次成功保存的历史；degraded = 图片 / 轮次被省略；
 *  warned / fused = P4 的体积提醒（超软线照常写 / 超硬线已停写）。 */
export function historyStatusNotice(st?: HistoryStatus | null): string | null {
  if (!st) return null;
  // 体积约束两条：与「未完整保存」**刻意区分**——这里说的是「历史还在，只是快长到头 / 已长到头」，
  // 不是「内容被省略了」。字节格式化在前端做（后端只给数字）。
  if (st.kind === "warned" || st.kind === "fused") {
    const size = formatBytes(st.bytes);
    const threshold = formatBytes(st.threshold);
    return st.kind === "fused"
      ? i18n.t("notice.historyFused", { size, threshold })
      : i18n.t("notice.historySizeWarned", { size, threshold });
  }
  return st.kind === "rejected"
    ? i18n.t("notice.historyRejected")
    : i18n.t("notice.historyDegraded", { images: st.stripped_images ?? 0, rounds: st.dropped_rounds ?? 0 });
}

/** 越限提示的同文案去重（[docs/session-history-limits](../../../docs/session-history-limits.md)）：
 *  会话长期越限时，降级只裁落盘用的局部副本、内存历史不变 → 之后每次 run 收尾仍然降级，`run:done` 于是逐 run
 *  带着 `history_save` 来追加同文案 notice（`run_id` 幂等守卫只管同一 run 的迟到 done，跨 run 拦不住）。
 *  口径：**从转录末尾往前找最近一条 notice**，其文案与本次要插的完全相同时才跳过。
 *  为什么不用「转录里已存在同文案即跳过」：那样用户中途做了别的事（取消 / 重试 / 压缩等别的 notice 插在后面）
 *  之后再越限就再也提示不到；只认「最近的 notice」则把连续多次降级（中间只隔着用户消息与助手回复这类非 notice 项）
 *  视为重复打扰，被别的 notice 打断后仍会重新提示一次。其它 notice 的文案与越限文案不同，故本判据不会误伤它们。 */
function hasTrailingNoticeText(items: readonly UiItem[], text: string): boolean {
  for (let i = items.length - 1; i >= 0; i--) {
    const it = items[i];
    if (it.kind === "notice") return it.text === text;
  }
  return false;
}

/** 子代理卡兜底收尾（[docs/subagent-terminal-event-loss](../../../docs/subagent-terminal-event-loss.md)）：
 *  主 run 收尾时，仍停在 `running` 的子代理卡一律落定——`sub:done` / `sub:error` 是子代理卡**唯一**的
 *  收尾途径（主会话工具有 `closeRunningTools` 兜底，子代理侧此前没有），一旦那一帧丢了，卡片就永久转圈、
 *  composer 的运行中计数也永久不归零（[docs/subagent-interaction-drawer](../../../docs/subagent-interaction-drawer.md)）。
 *
 *  为什么可以在这里无条件收敛：主 run 收尾时，子代理要么已完成（终态事件先到，无影响）、要么已随主 run 级联
 *  取消（后端 `parent_cancel` 传递），不存在「仍在跑」的真子代理——故不会误杀。
 *  `ended: "no_report"` 而非静默改 done：报告可能已到（`sub:report` 先于 `sub:done` 发射），
 *  卡片仍应按「未按约定汇报」口径展示橙色警示，不伪装成干净完成。
 *  幂等：已终态的卡不动，重复调 run:done 无副作用。 */
function settleRunningSubs(t: TabRunState): void {
  for (const sub of t.subs) {
    if (sub.status !== "running") continue;
    sub.status = "done";
    if (!sub.ended) sub.ended = "no_report";
  }
  for (const st of Object.values(t.subStreams)) {
    if (st.status === "running") st.status = "done";
  }
}

/** run 终态的**共享收敛体**（[docs/run-terminal-event-fallback](../../../docs/run-terminal-event-fallback.md)）：
 *  `run:done` / `run:error` / `run:cancelled` 三个 handler 与 run 终态看门狗共用同一套收敛动作。
 *
 *  为什么必须共用：这三件事原本就是同一套清理动作散在三处，而**它们本身全都由一次性终态事件驱动**
 *  ——一旦 `run:done` 丢失，原先挂在它身上的 `settleRunningSubs` / `closeRunningTools` /
 *  `closeStreamingAssistantItems` 会一起失效（子代理卡永久转圈）。提成共享函数后，
 *  看门狗可以在「后端已空闲但本地仍有未收敛状态」时复现同一套动作，成为整条链的兜底网。
 *
 *  `t.ask = null` 是本函数新增的一格：`t.ask` 此前**只有** `ask:closed` 一个清理入口，而
 *  `Composer.tsx` 在 `askActive` 为真时不渲染输入区（提问卡覆盖整个输入区）——
 *  `ask:closed` 丢失即整个会话无法输入。run 都收尾了还挂着的 ask 必是残帧，清掉是正确语义。
 *
 *  幂等：各项判据都是「只在仍是未收敛态时才动」，重复调用无副作用。 */
export function settleRun(t: TabRunState): void {
  // 本地运行态一并落定。三个 handler 此前各自在调本函数**之前**就设了 running=false，
  // 但看门狗 / 对账路径不经过 handler —— 后端已空闲而本地仍显示「运行中」同样是卡死的一种形态，
  // 故收敛体自身必须负责置位（幂等，重复设无副作用）。
  t.running = false;
  // 兜底收尾：不能只翻末项的等待指示——notice 插队（run:inject / run:retry / sub:error）后旧流式项可能不在末位，
  // 漏网的 streaming 项就是聊天里那个永久残留的等待指示（见 runFrames.currentAssistantIm 的不变量注释）。
  closeStreamingAssistantItems(t);
  // 工具卡兜底：仍在途（running / waiting）的卡落定「已中断」——运行结束不会有结果事件了
  closeRunningTools(t);
  // 子代理卡兜底（docs/subagent-terminal-event-loss）：sub:done 丢了就永久转圈
  settleRunningSubs(t);
  // ask / 审批面板兜底：此前只有 ask:closed 一个清理入口，丢帧即输入锁死
  if (t.ask) t.ask = null;
}

/** 本 Tab 是否仍有「未收敛」状态——run 终态看门狗的本地侧判据（[docs/run-terminal-event-fallback](../../../docs/run-terminal-event-fallback.md)）。
 *
 *  刻意**不只看 `t.running`**：`run:start` 丢失时 `t.running` 恒为 false，而 `sub:spawn` 建卡不检查它
 *  （`runHandlers.ts` 的 sub:spawn 只判 `if (!t) return`），此时子代理卡仍会是 running。
 *  只按 `t.running` 判会让看门狗在这种场景下**根本不启动**，漏洞原样保留。 */
export function hasUnsettledRunState(t: TabRunState): boolean {
  if (t.running || t.ask) return true;
  if (t.subs.some((s) => s.status === "running")) return true;
  return hasRunningTools(t);
}

/** 运行生命周期（11 键）：start/done/error/cancelled/inject/retry + 自动命名 + 计划任务 toast + 计划 todos + 目标状态 */
export function runLifecycleHandlers(set: SetFn, get: GetFn): Record<string, (p: any) => void> {
  return {
    "run:start": (p) => {
      set((s) => {
        const t = s.tabs[p.session];
        if (!t) return; // M-1：已关 Tab 的迟到事件不重建状态桶
        t.running = true;
        currentAssistantIm(t);
      });
    },
    "run:done": (p) => {
      markUnreadIfAway(p?.session);
      const before = get().tabs[p.session];
      if (!before?.running) {
        // 幂等守卫照旧早退，但**早退不等于无事可做**（[docs/run-terminal-event-fallback](../../../docs/run-terminal-event-fallback.md) 证据 5）：
        // `run:start` 丢失时 `t.running` 恒为 false，而 `sub:spawn` 建卡不检查它——
        // 原本这里直接 return 会让 settleRunningSubs / closeRunningTools / ask 清理**全部跳过**，
        // 子代理卡永久转圈。故早退前先收敛一次（仅在确有未收敛状态时动手，幂等无副作用）。
        //
        // 下面那道 `lastDoneRunId` 守卫**刻意不加**：它命中时新 run 已乐观把 running 置回 true，
        // 此时收敛会误杀正在跑的新 run。
        if (before && hasUnsettledRunState(before)) {
          set((s) => {
            const t = s.tabs[p.session];
            if (t) settleRun(t);
          });
        }
        return;
      }
      // [docs/run-queue-and-ask-revamp](../../../docs/run-queue-and-ask-revamp.md)：按 run_id 对 suggest 的双 run:done 去重——done1 已同步出队并乐观把
      // running 翻回 true（新运行），旧 `!running` 守卫对 done2 失效；放行两次会错误重置 running 并重复出队
      if (p?.run_id && before.lastDoneRunId === p.run_id) return;
      set((s) => {
        const t = s.tabs[p.session];
        if (!t) return;
        if (p?.run_id) t.lastDoneRunId = p.run_id;
        t.running = false;
        t.queueResumeAfterInjection = t.queue.some((q) => q.injecting);
        // 文本形态 ask 的待剥状态只在本轮有效（[docs/text-form-ask-fallback]）：清空后下一轮的增量不再被剥
        t.textRecovered = null;
        // [docs/preview-skill](../../../docs/preview-skill.md)：「先看预览」信号同样只在本 run 内有效——
        // 本轮没等到合格的 widget 就作废，不跨轮补弹（否则下一轮的第一张 widget 会莫名自己弹出来）
        t.widgetAutoOpen = null;
        t.pendingItemId = null; // docs/run-queue-and-ask-revamp：自然完成清掉「立即运行」标记，防止后续手动停止时插队
        // 收敛体与另两个 run 收尾 handler、以及 run 终态看门狗共用（docs/run-terminal-event-fallback）
        settleRun(t);
        if (p.suggestions) t.suggestions = p.suggestions;
        // [docs/session-history-limits](../../../docs/session-history-limits.md)：历史保存不干净时向本会话转录补一条提示。
        // 载荷仅在「拒存 / 有损保存」时才带 history_save（干净路径零打扰）；保存结果由后端在 run 收尾检查点上报，
        // run_id 幂等守卫同上——两次 done 只提示一次。跨 run 的同文案去重见 hasTrailingNoticeText（最近一条 notice 同文案则不追加）。
        // 本 handler 其余语义一概不动。
        const historyNotice = p?.history_save ? historySaveNotice(p.history_save) : null;
        if (historyNotice && !hasTrailingNoticeText(t.items, historyNotice)) {
          t.items.push({ kind: "notice", text: historyNotice });
        }
      });
      void get().refreshGit(p.session);
      const sessions = useSessions.getState();
      // 失焦时通知（应用内 + 系统通知）
      if (!document.hasFocus()) {
        const title = titleOf(sessions, p.session);
        useUi.getState().notify(title, i18n.t("notice.taskDone"), p.session);
        void systemNotify(p.session, title, i18n.t("notice.taskDone"));
      }
      // [docs/run-queue-and-ask-revamp](../../../docs/run-queue-and-ask-revamp.md)：任务完成后依序运行下一条队列项
      void get().runQueueNext(p.session);
    },
    "run:error": (p) => {
      markUnreadIfAway(p?.session); // docs/ask-ink-accent-and-composer-cover：失败收尾同样算会话结束
      set((s) => {
        const t = s.tabs[p.session];
        if (!t) return;
        t.running = false;
        t.queueResumeAfterInjection = false;
        // [docs/preview-skill](../../../docs/preview-skill.md)：失败路径同样作废待弹预览（同 run:done，不跨轮补弹）
        t.widgetAutoOpen = null;
        t.pendingItemId = null; // docs/run-queue-and-ask-revamp：同 done，防止残留标记在后续手动停止时插队
        // 收敛体同 run:done（docs/run-terminal-event-fallback）
        settleRun(t);
        // errorKind 携带后端 ProviderError 分类（[docs/auth-error-guidance](../../../docs/auth-error-guidance.md)）：auth/billing 有设置快捷入口
        t.items.push({ kind: "error", text: String(p?.error ?? i18n.t("notice.runFailed")), errorKind: p?.kind });
      });
    },
    "run:cancelled": (p) => {
      set((s) => {
        const t = s.tabs[p.session];
        if (!t) return;
        t.running = false;
        t.queueResumeAfterInjection = false;
        // [docs/preview-skill](../../../docs/preview-skill.md)：取消路径同样作废待弹预览（同 run:done，不跨轮补弹）
        t.widgetAutoOpen = null;
        // 收敛体同 run:done（docs/run-terminal-event-fallback）；必须在 push notice 之前，保证语义清晰
        settleRun(t);
        t.items.push({ kind: "notice", text: i18n.t("notice.cancelled") });
      });
      // [docs/steer-run-inject](../../../docs/steer-run-inject.md)：「↑ 立即」不再走打断路径
      // （旧版在此检查 pendingItemId 并出队执行），故取消 = 纯停止，队列保持暂停待「继续执行」。
      // `TabRunState.pendingItemId` 字段保留但已无写入方（删除会波及约 20 个测试文件的状态桶）。
    },
    "run:inject": (p) => {
      set((s) => {
        const t = s.tabs[p?.session];
        if (!t) return;
        // [docs/steer-run-inject](../../../docs/steer-run-inject.md)：drain_inject 把注入正文随 texts 带回，
        // 有正文时用「注入 N 条消息：」的 notice + injectedTexts（由 ChatMessages 逐条渲染）；
        // texts 缺省/为空（ask 方案批准注入、旧后端）时完全回落原键 notice.injected，不带 injectedTexts。
        const texts = Array.isArray(p?.texts)
          ? p.texts.filter((x: unknown): x is string => typeof x === "string" && x.trim() !== "")
          : [];
        t.items.push(
          texts.length > 0
            ? { kind: "notice", text: i18n.t("notice.injectedText", { n: p?.count ?? texts.length }), injectedTexts: texts }
            : { kind: "notice", text: i18n.t("notice.injected", { n: p?.count ?? 1 }) },
        );
      });
    },
    "session:title": (p) => {
      // 自动命名落地（[docs/session-auto-title](../../../docs/session-auto-title.md)）：同步会话列表 meta 与 Tab 标题（Tab 未开则等下次刷新）
      useSessions.getState().applyTitle(p.session, String(p?.title ?? ""));
    },
    "run:retry": (p) => {
      set((s) => {
        const t = s.tabs[p.session];
        if (!t) return;
        t.items.push({ kind: "notice", text: i18n.t("notice.retryAttempt", { n: p?.attempt ?? 1 }) });
        // 后端在本事件前已重置：低于新代际的在途帧会被 applyFrameToTab 丢弃（review C2）
        if (typeof p?.gen === "number") t.streamGen = p.gen;
        // 从尾部找最近的流式 assistant 项并清空（不与 notice 序列位置耦合，review M1）
        for (let i = t.items.length - 1; i >= 0; i--) {
          const it = t.items[i];
          if (it.kind === "assistant" && it.streaming) {
            it.timeline = [];
            it.toolsMap = {};
            break;
          }
        }
      });
    },
    "plan:update": (p) => {
      set((s) => {
        const sessions = useSessions.getState();
        const key = p.session ?? sessions.activeKey ?? "";
        if (!key) return;
        if (!s.tabs[key]) {
          // 仅为活跃会话建桶：非活跃会话（如后台计划任务）的迟到事件不得建桶（M-1，防泄漏）
          if (key !== sessions.activeKey) return;
          s.tabs[key] = blank();
        }
        s.tabs[key].todos = p.todos ?? [];
      });
    },
    // 计划任务运行态：静默驱动 tasks store（用户已拍板去掉这两个 toast；面板/左栏自行渲染运行中与状态）
    "scheduled:fired": (p) => useTasks.getState().applyFired(p),
    "scheduled:done": (p) => useTasks.getState().applyDone(p),
  };
}

/** 运行生命周期（11 键：start/done/error/cancelled/inject/retry + 自动命名 + 计划任务 toast + 计划 todos）。

/** 压缩流（5 键）：compacting / failed / compacted 通知 + suggestions + token 分布 */
export function compactHandlers(set: SetFn): Record<string, (p: any) => void> {
  return {
    "run:compacting": (p) => {
      set((s) => {
        const t = s.tabs[p.session];
        if (!t) return;
        // 去重（与手动压缩的后续事件配对）：CompactButton 已本地插入 notice，
        // 凭 compacting 标志避免重复入列；只为按钮 loading 置标志
        if (!t.compacting) {
          const before = typeof p?.tokens_before === "number" ? i18n.t("notice.tokensAbout", { n: p.tokens_before }) : "";
          t.items.push({ kind: "notice", text: i18n.t("notice.compacting", { before }) });
        }
        t.compacting = true;
      });
    },
    "run:compact_failed": (p) => {
      set((s) => {
        const t = s.tabs[p.session];
        if (!t) return;
        t.items.push({ kind: "notice", text: i18n.t("notice.compactFailed", { error: p?.error ?? "" }) });
        t.compacting = false;
      });
    },
    "run:compacted": (p) => {
      set((s) => {
        const t = s.tabs[p.session];
        if (!t) return;
        const before = typeof p?.tokens_before === "number" ? i18n.t("notice.tokensBefore", { n: p.tokens_before }) : "";
        t.items.push({ kind: "notice", text: i18n.t("notice.compacted", { before }) });
        t.compacting = false;
      });
    },
    "run:suggestions": (p) => {
      set((s) => {
        const t = s.tabs[p.session];
        if (!t) return;
        t.suggestions = p.items ?? [];
      });
    },
    "tokens:update": (p) => {
      set((s) => {
        const t = s.tabs[p.session];
        if (!t) return;
        t.breakdown = (p?.breakdown ?? null) as any;
      });
    },
  };
}

/** 工具结果落地（3 键）：开始信号建/翻卡（waiting → running），成功/失败统一汇入 onToolResult */
export function toolHandlers(get: GetFn): Record<string, (p: any) => void> {
  return {
    "tool:start": (p) => get().onToolStart(p.session, p),
    "tool:result": (p) => get().onToolResult(p.session, p, true),
    "tool:error": (p) => get().onToolResult(p.session, p, false),
  };
}

/** ask/审批（2 键）：打开询问卡 + 失焦通知；close 清空 */
export function askHandlers(set: SetFn, get: GetFn): Record<string, (p: any) => void> {
  return {
    "ask:opened": (p: AskOpenedEvent) => {
      set((s) => {
        const t = s.tabs[p.session];
        if (!t) return; // M-1：不重建桶——避免无人能应答的幽灵审批
        // 文本形态 ask 兜底（[docs/text-form-ask-fallback](../../../docs/text-form-ask-fallback.md)）：后端剥的是**落盘历史**，
        // 而这段协议原文早已随流式帧进了**当轮气泡**（帧已下发、无法回收）——这里补剥当轮气泡。
        // 剥离实现与后端 `text_ask::strip_block`（`replacen(block, "", 1)` + `trim_end()`）同口径，统一在
        // runFrames.stripRecoveredText / stripRecoveredInTab：找不到就什么也不做（跨段切分 / 已被裁剪 / 已剥过
        // 都不报错、保持原样）。
        // **为什么要留下待剥字符串**：正文经 64ms 节流下发，`</ask>` 尾巴常在 ask:opened **之后**才作为 delta_text
        // 到达——只在这里剥一次，后到的帧会把尾巴又追加回去。待剥状态 `t.textRecovered` 由 applyFrameToTab 的
        // 每帧文本落地消费，`run:done` 清空（标记只在本轮有效）。
        // 纪律：这是**流式文本**的清理，绝不改 `t.ask` 内容、不给卡片加来源标注（产品决定：不标注，
        // 免得用户以为提问不可信）。
        const recovered = p.text_recovered;
        if (recovered) {
          t.textRecovered = recovered;
          stripRecoveredInTab(t); // 帧先到齐的常见序：此刻就剥一次，不等下一帧
        }
        t.ask = {
          askId: p.ask_id,
          kind: p.kind,
          title: p.title,
          detail: p.detail,
          questions: p.questions,
          switchToAutoEdit: p.switch_to_auto_edit,
          allowAlways: p.allow_always,
          approvalKind: p.approval_kind,
          planFile: p.plan_file ?? null,
          planBody: p.plan_body ?? null,
          approval: p.approval,
          approveId: p.approve_id ?? null,
        };
      });
      // [docs/ask-ink-accent-and-composer-cover](../../../docs/ask-ink-accent-and-composer-cover.md)：窗口失焦时系统通知（聚焦时不发，应用内 ask 窗口可见）；点击回跳走 [docs/notification-click-reveal](../../../docs/notification-click-reveal.md) 链路。
      // 仅在桶存在（有人能应答）时发送，与 M-1 守卫一致
      if (get().tabs[p.session] && !document.hasFocus()) {
        const sessions = useSessions.getState();
        void systemNotify(
          p.session,
          titleOf(sessions, p.session),
          p.kind === "approval" ? i18n.t("notice.waitingConfirm") : i18n.t("notice.waitingAnswer"),
        );
      }
    },
    "ask:closed": (p) => {
      set((s) => {
        if (s.tabs[p.session]) s.tabs[p.session].ask = null;
      });
    },
  };
}

/** 子代理事件（6 键）：卡片状态机 + 独立消息流生命周期 */
export function subHandlers(set: SetFn): Record<string, (p: any) => void> {
  return {
    "sub:spawn": (p: SubagentEvent) => {
      set((s) => {
        const t = s.tabs[p.session];
        if (!t) return; // M-1
        t.subs.push({
          subId: p.sub_id,
          role: p.role ?? "agent",
          name: p.name ?? null,
          task: typeof p.task === "string" ? p.task : undefined,
          description: p.description ?? "",
          step: 0,
          maxSteps: p.max_steps ?? 25,
          tokens: 0,
          lastTools: [],
          status: "running",
        });
        // 初始化独立消息流（流式帧经信封路由进 applyFrameToSub）
        if (!t.subStreams[p.sub_id]) {
          t.subStreams[p.sub_id] = { timeline: [], toolsMap: {}, status: "running", gen: 0, loaded: false };
        }
        // 把子代理卡锚进当前流式项的 timeline（与工具卡同机制，按调用位置穿插）
        currentAssistantIm(t).timeline.push({ kind: "sub", subId: p.sub_id });
        // [docs/subagent-interaction-drawer](../../../docs/subagent-interaction-drawer.md)：首个子代理启动自动打开过程抽屉（常显语义）；后续子代理不打断当前视图
        const runningCount = t.subs.filter((x) => x.status === "running").length;
        if (runningCount === 1) t.subDrawer = { open: true, subId: p.sub_id };
      });
    },
    "sub:step": (p: SubagentEvent) => {
      set((s) => {
        const sub = s.tabs[p.session]?.subs.find((x) => x.subId === p.sub_id);
        if (!sub) return;
        // 已收尾的子代理不再接受进度采样（[docs/subagent-terminal-event-loss](../../../docs/subagent-terminal-event-loss.md)）：
        // 后端 `sub:done`（subagent.rs:569）先于 `progress.abort()`（:629）发射，中间隔着
        // save_sub_history + stats 记录；该窗口内到达的迟到 tick 会把 `sub.step` 覆写回轮询采样值，
        // 表现为「卡片已翻 ✓ 但步数还在跳」。守卫兼作子代理已收尾的语义边界。
        if (sub.status !== "running") return;
        sub.step = p.step ?? sub.step;
        // 批准门选档（[docs/mode-gate-and-subagent-sync]）：子代理当前档位每步上报，过程抽屉显示档位行；
        // 旧后端 / 归档回放不带该字段 → 保持原值（不写成 undefined，避免抽屉出现空档位）
        if (p.approval_mode) sub.approvalMode = p.approval_mode;
        if (p.tool) {
          sub.lastTools.push(p.tool);
          if (sub.lastTools.length > 8) sub.lastTools.shift();
        }
        if (typeof p.detail === "string") sub.detail = p.detail;
      });
    },
    "sub:report": (p: SubagentEvent) => {
      // 最终报告在收尾前进卡（后端先发 sub:report 再发 sub:done）
      set((s) => {
        const sub = s.tabs[p.session]?.subs.find((x) => x.subId === p.sub_id);
        if (sub && typeof p.report === "string") sub.report = p.report;
      });
    },
    "sub:usage": (p: SubagentEvent) => {
      set((s) => {
        const sub = s.tabs[p.session]?.subs.find((x) => x.subId === p.sub_id);
        if (sub && p.usage) sub.tokens = (p.usage.input ?? 0) + (p.usage.output ?? 0);
      });
    },
    "sub:done": (p: SubagentEvent) => {
      set((s) => {
        const t = s.tabs[p.session];
        if (!t) return;
        const sub = t.subs.find((x) => x.subId === p.sub_id);
        if (sub) {
          sub.status = "done";
          // 收尾刷新：轮询采样可能滞后于真实步数，以事件携带的最终值纠正
          if (typeof p.steps_used === "number") sub.step = p.steps_used;
          if (p.ended) sub.ended = p.ended;
        }
        const st = t.subStreams[p.sub_id];
        if (st) st.status = "done";
        // 只扫该子流：子代理结束时主会话可能仍在跑，不得误伤主会话在途工具
        closeRunningTools(t, p.sub_id);
      });
    },
    "sub:error": (p: SubagentEvent) => {
      set((s) => {
        const t = s.tabs[p.session];
        if (!t) return;
        const sub = t.subs.find((x) => x.subId === p.sub_id);
        if (sub) sub.status = "error";
        const st = t.subStreams[p.sub_id];
        if (st) st.status = "error";
        closeRunningTools(t, p.sub_id);
        t.items.push({ kind: "notice", text: i18n.t("notice.subagentFailed", { error: p.error ?? "" }) });
      });
    },
  };
}

/** 其他（3 键）：MCP 连接状态 upsert；service 工具卡尾迹/退出反映；退出拦截请求 */
export function miscHandlers(set: SetFn): Record<string, (p: any) => void> {
  return {
    // 退出拦截（会话保存与恢复优化 · 批1）：后端在 ExitRequested 下不可退，
    // 下发在跑的会话列表问询处置方式；这里只落到 ui store，弹窗由 AppShell 唯一渲染
    "app:exit_requested": (p) => {
      useUi.setState({ exitRequest: { running: Array.isArray(p?.running) ? p.running : [] } });
    },
    "mcp:status": (p) => {
      // 按 (作用域, server 名) upsert，绝不累积重复项。
      // 只按 name 作键是旧实现的串场根因：全局层与项目层可以有同名 server，
      // 两者是不同条目（后端池键含作用域），按名合并会让它们互相覆盖。
      const next: McpStatusPayload = {
        name: p.server,
        scope: p.scope,
        state: p.state,
        tools: p.tools,
        tool_details: p.tool_details ?? [],
        tools_filtered: p.tools_filtered ?? 0,
        pid: p.pid ?? null,
        error: p.error ?? null,
        note: p.note ?? null,
      };
      useUi.setState((s) => {
        const i = s.mcpStatus.findIndex(
          (m) => m.name === next.name && m.scope === next.scope,
        );
        if (i >= 0) {
          const list = s.mcpStatus.slice();
          list[i] = next;
          return { mcpStatus: list };
        }
        return { mcpStatus: [...s.mcpStatus, next] };
      });
    },
    "service:update": (p) => {
      // M-9：tail 更新 / removed / exited 都要反映到 service 工具卡。
      // `running` 是进程存活事实（后端 service.rs 的 done 标志），与 tail 是否有内容无关：
      // 无输出的服务 tail 恒为空，早期用 `tail` 当存活代理会让卡片永久误报「已停止」。
      set((s) => {
        // 子代理启动的服务，其 session = sub_id、不在 tabs 顶层——必须走属主查找
        // （对齐 run.ts onToolResult 的 subStreams 路由），否则该服务的事件永久静默丢弃。
        const targets = serviceStreams(s, p.session);
        if (targets.length === 0) {
          droppedServiceUpdates += 1;
          return;
        }
        for (const tools of targets) {
          for (const tool of tools) {
            const sid = tool.outcome?.data?.id;
            const hit = (p.removed && sid === p.removed) || (p.id && sid === p.id);
            if (!hit) continue;
            const data = { ...tool.outcome.data };
            if (typeof p.tail === "string") data.tail = p.tail;
            if (p.removed || p.exited) {
              data.tail = "";
              data.running = false;
            } else if (typeof p.running === "boolean") {
              data.running = p.running;
            }
            tool.outcome = { ...tool.outcome, data };
          }
        }
        // 收到事件即重置静默计时（推送停了不代表服务死了，见 reconcileServices）
        lastServiceUpdateAt = Date.now();
      });
    },
  };
}

/**
 * 收集一批工具锚点里的 service 卡。
 *
 * 两种**形状不同**的容器都要认（这是本函数存在的唯一理由，写错任一处都会让
 * 子代理启动的服务收不到任何状态更新）：
 * - 主会话：assistant 项，每项自带 `toolsMap`
 * - 子代理流 `SubStream`：**平铺**的 `toolsMap`（工具卡不在 `timeline` 里——
 *   `timeline` 只有 tool/text/thinking 等 seg，见 `closeRunningTools` 的遍历口径）
 */
function serviceToolsOf(container: any): any[] {
  const out: any[] = [];
  if (!container) return out;
  if (Array.isArray(container)) {
    for (const item of container) {
      if (item?.kind !== "assistant") continue;
      for (const tool of Object.values<any>(item.toolsMap ?? {})) {
        if (tool?.tool === "service") out.push(tool);
      }
    }
    return out;
  }
  for (const tool of Object.values<any>(container.toolsMap ?? {})) {
    if (tool?.tool === "service") out.push(tool);
  }
  return out;
}

/**
 * 定位承载 session 的 service 工具锚点集合。
 *
 * 子代理启动的服务，其 session = sub_id，**不在** `tabs` 顶层而是挂在所属 Tab 的
 * `subStreams` 下（对齐 run.ts `onToolResult` 的路由），早期只查 `tabs[session]` 会让
 * 子代理启动的服务事件永久静默丢弃。返回集合而非单个 toolsMap：同一 session 在重名
 * Tab 下可有多份视图。
 */
function serviceStreams(s: WritableDraft<RunStore>, session: string): any[][] {
  const out: any[][] = [];
  const push = (tools: any[]) => {
    if (tools.length > 0) out.push(tools);
  };
  push(serviceToolsOf(s.tabs[session]?.items));
  for (const tb of Object.values(s.tabs)) {
    if (tb.subStreams[session]) push(serviceToolsOf(tb.subStreams[session]));
  }
  return out;
}

// ---------- service 状态对账（推送通道的拉取兜底）----------

/** 因 session 未就绪而丢弃的 service 事件数（诊断面板可读；推送通道天然不可靠） */
let droppedServiceUpdates = 0;
/** 最近一次收到 service:update 的时刻 */
let lastServiceUpdateAt = Date.now();
/** 已排定的对账定时器（同一时刻至多一个） */
let reconcileTimer: ReturnType<typeof setTimeout> | null = null;

/** 静默超过该时长就拉一次权威快照——静默期不等于进程已死 */
const SERVICE_SILENCE_MS = 3000;

/** 累计丢弃的 service 事件数（供诊断面板读取；推送通道天生不可靠，丢帧本身不是异常） */
export function droppedServiceUpdateCount(): number {
  return droppedServiceUpdates;
}

/** 测试钩子：重置模块级对账状态（避免用例间残留定时器与时间戳） */
export function __resetServiceReconcileForTest(): void {
  droppedServiceUpdates = 0;
  lastServiceUpdateAt = Date.now();
  if (reconcileTimer) {
    clearTimeout(reconcileTimer);
    reconcileTimer = null;
  }
}

/** 当前 store 里是否存在 service 工具卡（决定对账是否有意义） */
function hasServiceCard(get: GetFn): boolean {
  const s = get();
  for (const tb of Object.values(s.tabs ?? {})) {
    if (serviceToolsOf(tb.items).length > 0) return true;
    for (const st of Object.values<any>(tb.subStreams ?? {})) {
      if (serviceToolsOf(st).length > 0) return true;
    }
  }
  return false;
}

/**
 * 用 `list_services` 权威快照校准所有 service 工具卡的 `running`。
 *
 * 推送是增量通道，丢一帧就永久停在错误状态（关 Tab / 分页未加载 / 子代理流未挂载）。
 * 本函数是唯一的兜底：把「落盘快照已过期」也一并修正——历史恢复出来的卡 `running`
 * 是启动当时的值，可能早已失效，这里以进程实况覆盖。
 * `set` 由调用方（run.ts）注入：本文件不得运行时 import store（会与 run.ts 形成循环依赖），
 * 只保留 type-only 引用。
 */
export async function reconcileServices(set: SetFn, get: GetFn): Promise<void> {
  // 无卡可校准时早退：既是廉价短路，也断了链式重排的续航（无卡无对账需求）
  if (!hasServiceCard(get)) return;
  const infos = await ipc.listServices();
  const live = new Set(infos.map((x) => x.id));
  const running = new Set(infos.filter((x) => x.running).map((x) => x.id));
  set((s) => {
    for (const tb of Object.values(s.tabs)) {
      const apply = (tools: any[]) => {
        for (const tool of tools) {
          const id = tool.outcome?.data?.id;
          if (typeof id !== "string") continue;
          const alive = live.has(id);
          const next = alive ? running.has(id) : false;
          if (tool.outcome?.data?.running === next) continue;
          tool.outcome = { ...tool.outcome, data: { ...tool.outcome.data, running: next } };
        }
      };
      apply(serviceToolsOf(tb.items));
      for (const st of Object.values<any>(tb.subStreams)) apply(serviceToolsOf(st));
    }
  });
}

/**
 * 按需排定一次对账（防抖）。`set` 由调用方注入；拉取失败静默——对账是兵底路，
 * 拉不到就保持上一次的已知状态，绝不反向写回一个未经证实的值。
 */
export function scheduleServiceReconcile(set: SetFn, get: GetFn): void {
  if (reconcileTimer) return;
  reconcileTimer = setTimeout(() => {
    reconcileTimer = null;
    const silentFor = Date.now() - lastServiceUpdateAt;
    // 自维持重排：无论本轮是否触发拉取都排下一轮，否则「推送刚到过 → 本轮跳过 → 无人再排」
    // 会让拉取兜底永久失效（而它恰恰是事件全丢时唯一的纠错手段）。
    // 链在无 service 卡时靠 hasServiceCard 早退自行停住，不会变成常驻轮询。
    scheduleServiceReconcile(set, get);
    if (silentFor < SERVICE_SILENCE_MS) return;
    void reconcileServices(set, get).catch(() => {
      /* 对账是兵底路：拉取失败不打断界面 */
    });
  }, SERVICE_SILENCE_MS);
}
