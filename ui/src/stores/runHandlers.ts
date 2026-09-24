// 后端事件 handler 工厂 —— 29 键事件面（契约测试锚点；键名不可增删）。
// 自 run.ts 拆出（[docs/fence-hardening-and-powershell-ast](../../../docs/fence-hardening-and-powershell-ast.md) 重构）：每族是一个 (set, get) => handler-record 工厂；
// run.ts 的 bindGlobalHandlers 保持唯一注册点并展开它们，
// Object.keys(bindGlobalHandlers()) 必须与拆分前事件面逐字节一致。
import type { HistorySaveReport, HistoryStatus, McpStatusPayload, SubagentEvent } from "../ipc/types";
import type { WritableDraft } from "immer";
import { ipc } from "../ipc/client";
import { titleOf, useSessions } from "./sessions";
import { useUi } from "./ui";
import { useTasks } from "./tasks";
import { i18n } from "../i18n";
import { blank, closeRunningTools, closeStreamingAssistantItems, currentAssistantIm } from "./runFrames";
import type { RunStore } from "./run";
import type { UiItem } from "./run.types";

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

/** 历史未完整保存的会话内提示（[docs/session-history-limits](../../../docs/session-history-limits.md)）：
 *  ① `run:done.history_save`（当次保存结果，仅当保存不干净时后端才带上）——干净返回 null，零打扰；
 *  ② `SessionMeta.history_status`（挂在索引上，重启后仍在）——恢复历史时用同一套文案，保证两处口径一致。 */
export function historySaveNotice(h: HistorySaveReport): string | null {
  if (h.saved === false) return i18n.t("notice.historyRejected");
  const images = h.stripped_images ?? 0;
  const rounds = h.dropped_rounds ?? 0;
  if (images > 0 || rounds > 0) return i18n.t("notice.historyDegraded", { images, rounds });
  return null;
}

/** 索引里的历史状态 → 会话内提示文案（无状态 = 干净 = null）。语义同 historySaveNotice：
 *  rejected = 磁盘上仍是上一次成功保存的历史；degraded = 图片 / 轮次被省略。 */
export function historyStatusNotice(st?: HistoryStatus | null): string | null {
  if (!st) return null;
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

/** 运行生命周期（10 键）：start/done/error/cancelled/inject/retry + 自动命名 + 计划任务 toast + 计划 todos */
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
      if (!before?.running) return; // 幂等：迟到 done 场景
      // [docs/run-queue-and-ask-revamp](../../../docs/run-queue-and-ask-revamp.md)：按 run_id 对 suggest 的双 run:done 去重——done1 已同步出队并乐观把
      // running 翻回 true（新运行），旧 `!running` 守卫对 done2 失效；放行两次会错误重置 running 并重复出队
      if (p?.run_id && before.lastDoneRunId === p.run_id) return;
      set((s) => {
        const t = s.tabs[p.session];
        if (!t) return;
        if (p?.run_id) t.lastDoneRunId = p.run_id;
        t.running = false;
        t.pendingItemId = null; // docs/run-queue-and-ask-revamp：自然完成清掉「立即运行」标记，防止后续手动停止时插队
        // 兜底收尾：不能只翻末项的等待指示——notice 插队（run:inject / run:retry / sub:error）后旧流式项可能不在末位，
        // 漏网的 streaming 项就是聊天里那个永久残留的等待指示（见 runFrames.currentAssistantIm 的不变量注释）。
        // 这里扫全部 assistant 项统一收尾（streaming=false + 冻结思考时长）；本 handler 其余语义一概不动。
        closeStreamingAssistantItems(t);
        // 工具卡兜底：仍在途（running / waiting）的卡落定「已中断」——运行结束不会有结果事件了
        closeRunningTools(t);
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
        t.pendingItemId = null; // docs/run-queue-and-ask-revamp：同 done，防止残留标记在后续手动停止时插队
        // 兜底收尾：failure 路径同样扫全部 assistant 项（不只是末项），同 run:done
        closeStreamingAssistantItems(t);
        closeRunningTools(t);
        // errorKind 携带后端 ProviderError 分类（[docs/auth-error-guidance](../../../docs/auth-error-guidance.md)）：auth/billing 有设置快捷入口
        t.items.push({ kind: "error", text: String(p?.error ?? i18n.t("notice.runFailed")), errorKind: p?.kind });
      });
    },
    "run:cancelled": (p) => {
      set((s) => {
        const t = s.tabs[p.session];
        if (!t) return;
        t.running = false;
        // 兜底收尾：取消路径同样扫全部 assistant 项（不只是末项），同 run:done（必须在 push notice 之前，保证语义清晰）
        closeStreamingAssistantItems(t);
        closeRunningTools(t);
        t.items.push({ kind: "notice", text: i18n.t("notice.cancelled") });
      });
      // [docs/run-queue-and-ask-revamp](../../../docs/run-queue-and-ask-revamp.md)：「立即运行」打断后立即执行该项；普通取消 = 队列保持暂停
      const t = get().tabs[p.session];
      const pid = t?.pendingItemId;
      if (t && pid) {
        const item = t.queue.find((q) => q.id === pid);
        if (item) {
          set((s) => {
            const t = s.tabs[p.session];
            if (t) {
              t.pendingItemId = null;
              t.queue = t.queue.filter((q) => q.id !== pid);
            }
          });
          void get().send(item.text, item.images, p.session);
        }
      }
    },
    "run:inject": (p) => {
      set((s) => {
        const t = s.tabs[p.session];
        if (!t) return;
        t.items.push({ kind: "notice", text: i18n.t("notice.injected", { n: p?.count ?? 1 }) });
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
    "ask:opened": (p) => {
      set((s) => {
        const t = s.tabs[p.session];
        if (!t) return; // M-1：不重建桶——避免无人能应答的幽灵审批
        t.ask = {
          askId: p.ask_id,
          kind: p.kind,
          title: p.title,
          detail: p.detail,
          questions: p.questions,
          switchToAutoEdit: p.switch_to_auto_edit,
          allowAlways: p.allow_always,
          planFile: p.plan_file ?? null,
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
        sub.step = p.step ?? sub.step;
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
      // M-9：tail 更新 / removed / exited 都要反映到 service 工具卡（状态翻「running → stopped」）
      set((s) => {
        const t = s.tabs[p.session];
        if (!t) return;
        for (const item of t.items) {
          if (item.kind !== "assistant") continue;
          for (const tool of Object.values(item.toolsMap)) {
            if (tool.tool !== "service") continue;
            const sid = tool.outcome?.data?.id;
            const hit = (p.removed && sid === p.removed) || (p.id && sid === p.id);
            if (!hit) continue;
            const data = { ...tool.outcome.data };
            if (typeof p.tail === "string") data.tail = p.tail;
            if (p.removed || p.exited) data.tail = "";
            tool.outcome = { ...tool.outcome, data };
          }
        }
      });
    },
  };
}
