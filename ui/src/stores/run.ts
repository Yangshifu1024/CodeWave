// 每个 Tab 隔离的运行态：转录、流式、工具卡、ask/审批、计划、子代理、上下文分布
// immer 中间件：高频流式更新走结构共享（路径级拷贝）；组件只对订阅到的 TabRunState 引用反应
// [docs/fence-hardening-and-powershell-ast](../../../docs/fence-hardening-and-powershell-ast.md) 重构：纯帧 reducer 收敛于 runFrames.ts，后端事件 handler 工厂在 runHandlers.ts，
// 共享类型在 run.types.ts（下方 re-export，消费方 import 路径保持 "./run" 不变）。
import { create } from "zustand";
import { immer } from "zustand/middleware/immer";
import { Channel } from "@tauri-apps/api/core";
import { ipc } from "../ipc/client";
import type { Breakdown, Message, ToolResultEvent, ToolStartEvent } from "../ipc/types";
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
  miscHandlers,
  runLifecycleHandlers,
  subHandlers,
  toolHandlers,
} from "./runHandlers";

export type {
  ComposerDraft,
  PendingImage,
  QueueItem,
  SubStream,
  SubView,
  TabRunState,
  TimelineSeg,
  ToolView,
  UiItem,
} from "./run.types";
import type { ComposerDraft, PendingImage, SubStream, SubView, TabRunState, TimelineSeg, ToolView, UiItem } from "./run.types";

  /** 运行态 store 契约：tabs 按会话 id 分桶 + 全部动作；bindGlobalHandlers 的键集合即 29 键事件面（唯一注册点）。 */export interface RunStore {
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
  restoreFromMessages(sessionId: string, msgs: Message[]): void;
  /** [docs/subagent-interaction-drawer](../../../docs/subagent-interaction-drawer.md)：打开子代理过程抽屉（归档子代理按需拉取过程历史重建消息流） */
  openSubDrawer(sessionId: string | null, subId: string): Promise<void>;
  closeSubDrawer(sessionId?: string | null): void;
  refreshGit(sessionId?: string | null): Promise<void>;
  bindGlobalHandlers(): Record<string, (p: any) => void>;
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
        await ipc.startChat(sessionId, text, images ?? [], channel);
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

    restoreFromMessages(sessionId, msgs) {
      const out: UiItem[] = [];
      // [docs/subagent-interaction-drawer](../../../docs/subagent-interaction-drawer.md)：tool_use_id → 结果（从子代理 outcome 解析 sub_id/report）
      const toolResults = scanToolResults(msgs);
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
                // 卡片据此按路径重新加载图片；解析不出来才退回占位。
                // 入参同样填上（safeArgsPreview）：摘要行、ask 问答行的题干、edit 的 diff 都靠它。
                // 本次未处理：主会话历史恢复把无结果调用呈现为已使用，与子代理流的「已中断」语义不一致，留待后续
                toolsMap[callKey] = {
                  callKey,
                  tool: (c as any).name,
                  status: "ok",
                  outcome: { ok: true, data: restoredToolData(toolResults[callKey]?.content) },
                  argsPreview: safeArgsPreview((c as any).args),
                  progressTail: "",
                };
              }
            }
          }
          if (timeline.length) out.push({ kind: "assistant", timeline, toolsMap, streaming: false, createdAt: m.created_at ?? undefined });
        }
      }
      set((s) => {
        if (!s.tabs[sessionId]) s.tabs[sessionId] = blank();
        s.tabs[sessionId].items = out as any;
        // 注册归档子代理卡（流先留空，抽屉首次打开按需拉取过程流，防止重复导入）
        for (const sv of restoredSubs) {
          if (!s.tabs[sessionId].subs.some((x) => x.subId === sv.subId)) s.tabs[sessionId].subs.push(sv);
        }
        for (const [k, v] of Object.entries(restoredStreams)) {
          if (!s.tabs[sessionId].subStreams[k]) s.tabs[sessionId].subStreams[k] = v as any;
        }
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
