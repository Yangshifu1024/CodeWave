// run store 的纯帧 reducer —— 无 zustand/store 依赖，可独立单测。
// 自 run.ts 拆出（[docs/fence-hardening-and-powershell-ast](../../../docs/fence-hardening-and-powershell-ast.md) 重构）：帧到达顺序 = timeline 顺序；所有函数就地变异
// 传入的 immer 草稿且无返回值（工厂 helper 除外）。
import type { Frame, Message } from "../ipc/types";
import { cacheDenominator, type CacheSemantics } from "../utils/models";
import type {
  AssistantItem,
  TabRunState,
  TimelineSeg,
  ToolView,
  UiItem,
  UsageTotals,
} from "./run.types";

/** Tab 运行态初值工厂 */
export function blank(): TabRunState {
  return {
    items: [],
    running: false,
    streamGen: 0,
    ask: null,
    breakdown: null,
    todos: [],
    suggestions: [],
    subs: [],
    subStreams: {},
    subDrawer: { open: false, subId: null },
    gitEntries: null,
    writeTick: 0,
    queue: [],
    pendingItemId: null,
    draftFromQueue: null,
    lastDoneRunId: null,
    goalRev: 0,
    compacting: false,
    usage: { input: 0, output: 0, cacheRead: 0, cacheWrite: 0 },
  };
}
export const BLANK: TabRunState = blank();

/** 收尾所有仍处于 streaming 的 assistant 项（keep = 需要保留的当前流式项）：置 streaming=false 并冻结其思考时长。
 *  兜底用途：任何漏网分支留下的 streaming 项都会渲染成永久等待指示，故运行收尾/新建流式项前统一扫一遍。 */
export function closeStreamingAssistantItems(t: TabRunState, keep?: AssistantItem) {
  for (const it of t.items) {
    if (it.kind !== "assistant" || !it.streaming || it === keep) continue;
    it.streaming = false;
    closeOpenThinking(it.timeline);
  }
}

/** 取/建草稿上当前流式中的 assistant 项（创建时盖章：本地时间 ≈ 消息生成时间；重开后以持久化值为准）
 *
 *  不变量：同一 Tab 内**至多一个** assistant 项处于 streaming（＝一条等待指示），且它恒为 items 末项。
 *  破坏方式：run:inject / run:retry / sub:error 都会往 items 末尾 push 一条 notice，于是流式项不再是末项；
 *  下一帧 delta 走到本函数时就「新建」出第二个流式项，旧项的 streaming 再无人清 → 聊天里同时出现两个等待指示，
 *  且运行结束后收尾三兄弟只翻末项，旧指示永久残留。故「新建前先显式收尾遗留流式项」是本不变量的唯一守卫，
 *  一处改动即覆盖上述三个触发点；新项仍 push 在末尾，保证新文本落在 notice 下方（时间顺序）且等待指示只在最底部。 */
export function currentAssistantIm(t: TabRunState): AssistantItem {
  const last = t.items[t.items.length - 1];
  if (!last || last.kind !== "assistant" || !last.streaming) {
    // 只收尾 assistant 项，notice / user / tool 一概不碰；历史脏数据里存在多个遗留流式项时一并收尾
    closeStreamingAssistantItems(t);
    const item: AssistantItem = {
      kind: "assistant", timeline: [], toolsMap: {}, streaming: true,
      createdAt: new Date().toISOString(),
    };
    t.items.push(item);
    return item;
  }
  // 末项已是流式项：其前方仍可能残留更早的流式项（历史脏数据），同样收尾以保证等待指示唯一
  closeStreamingAssistantItems(t, last);
  return last;
}

/** 收尾 timeline 尾部未定格的 thinking 分段：冻结思考时长（无 startedAt 的历史分段跳过） */
export function closeOpenThinking(timeline: TimelineSeg[]) {
  const last = timeline[timeline.length - 1];
  if (last && last.kind === "thinking" && last.durationMs === undefined && last.startedAt !== undefined) {
    last.durationMs = Math.max(0, Date.now() - last.startedAt);
  }
}

/** 追加 text/thinking delta：同类合并进尾段，异类新开分段（保持穿插顺序）；
 *  异类新段前先收尾进行中的 thinking 分段；新 thinking 分段记录开始时间（思考计时器/跑马灯） */
export function appendDelta(timeline: TimelineSeg[], kind: "text" | "thinking", text: string) {
  if (!text) return;
  const last = timeline[timeline.length - 1];
  if (last && last.kind === kind) {
    last.text += text;
    return;
  }
  closeOpenThinking(timeline);
  timeline.push(
    kind === "text"
      ? { kind: "text", text }
      : { kind: "thinking", text, startedAt: Date.now() },
  );
}

/** timeline 无该工具锚点则追加，返回可更新的 ToolView（工具卡按插入位置穿插）。
 *  结构上泛型：主流 assistant 项与子代理流（SubStream）共用同一形状。 */
export function ensureToolAnchorIm(
  a: { timeline: TimelineSeg[]; toolsMap: Record<string, ToolView> },
  callKey: string,
): ToolView {
  if (!a.timeline.some((s) => s.kind === "tool" && s.callKey === callKey)) {
    closeOpenThinking(a.timeline);
    a.timeline.push({ kind: "tool", callKey });
  }
  const existing = a.toolsMap[callKey];
  if (existing) return existing;
  const created: ToolView = { callKey, tool: "?", status: "running", progressTail: "" };
  a.toolsMap[callKey] = created;
  return created;
}

/** 在 items 里按 callKey 找工具卡（跨**所有** assistant 项，不只末项）：跑批期间 run:inject / notice 会给 items
 *  末尾 push 一条通知，下一帧 delta 让 currentAssistantIm 另建 assistant 项，而工具卡锚点留在更早的项里；
 *  工具结果必须命中原卡而非另建一张（否则旧卡永久 running）。 */
export function findToolViewInItems(items: UiItem[], callKey: string): ToolView | undefined {
  for (const it of items) {
    if (it.kind !== "assistant") continue;
    const hit = it.toolsMap[callKey];
    if (hit) return hit;
  }
  return undefined;
}

/** tool:start 事件的有效载荷（前端消费子集；形状见 ipc/types.ts 的 ToolStartEvent） */
type ToolStartPayload = { call_key: string; tool: string; args_preview?: string; phase: string };

/** 工具开始事件的状态迁移（tool 由调用方定位后传入）。迟到守卫：卡已落定（ok/error）只做 argsPreview
 *  空值回填、不得翻回运行态——否则已完成的卡会永久转圈；否则按 phase 翻相（waiting = 审批/范围确认等待中）。
 *
 *  独立成函数的原因：定位 tool 的容器有两种来源——子代理流是稳定容器（直接按 toolsMap 查），主会话则必须
 *  跨 assistant 项查找（见 findToolViewInItems）。若把定位写死在这里，主会话在 waiting→running 之间插入
 *  notice（run:inject / sub:error）时会落到新项、为同一次调用再建一张卡。 */
export function updateToolFromStart(tool: ToolView, p: ToolStartPayload) {
  if (tool.status === "ok" || tool.status === "error") {
    if (!tool.argsPreview && p.args_preview) tool.argsPreview = p.args_preview;
    return;
  }
  tool.status = p.phase === "waiting" ? "waiting" : "running";
  if (p.tool && tool.tool === "?") tool.tool = p.tool;
  if (!tool.argsPreview && p.args_preview) tool.argsPreview = p.args_preview;
}

/** 工具开始事件（tool:start）在**指定容器内**落卡（缺卡则建锚点）：同一 call_key 可能先 waiting 后 running
 *  （写工具在审批/范围确认门期间 waiting）；只读工具只收一次 running。
 *  容器已稳定时用它（子代理流）；主会话请先 findToolViewInItems 跨项命中、未命中再调本函数。 */
export function applyToolStart(
  a: { timeline: TimelineSeg[]; toolsMap: Record<string, ToolView> },
  p: ToolStartPayload,
) {
  updateToolFromStart(a.toolsMap[p.call_key] ?? ensureToolAnchorIm(a, p.call_key), p);
}

/** 「已中断」落定形态：空 message 的 E_INTERRUPTED（渲染层据此走中性样式、显示「已中断」且省略错误行）。 */
function markInterrupted(tool: ToolView) {
  tool.status = "error";
  tool.outcome = { ok: false, data: null, error: { code: "E_INTERRUPTED", message: "" } };
  tool.progressTail = "";
}

/** 收尾一个 toolsMap 里仍在途（running / waiting）的工具卡；已落定（ok/error）的一律不动。 */
export function settleRunningTools(map: Record<string, ToolView>) {
  for (const tool of Object.values(map)) {
    if (tool.status === "running" || tool.status === "waiting") markInterrupted(tool);
  }
}

/** 收尾 Tab 内在途工具卡：有 subId 只扫该子代理流（子代理结束时主会话可能仍在跑，不得误伤主会话在途工具）；
 *  无 subId 扫全 Tab（所有 assistant 项 toolsMap + 所有 subStreams[*].toolsMap）。
 *  关键：不可并入 closeStreamingAssistantItems——那个函数在运行中也会被 currentAssistantIm 调用（新建流式项前
 *  收尾遗留流式项），合并会把在途工具误标「已中断」。 */
export function closeRunningTools(t: TabRunState, subId?: string) {
  if (subId) {
    const st = t.subStreams[subId];
    if (st) settleRunningTools(st.toolsMap);
    return;
  }
  for (const it of t.items) {
    if (it.kind === "assistant") settleRunningTools(it.toolsMap);
  }
  for (const st of Object.values(t.subStreams)) settleRunningTools(st.toolsMap);
}

/** 将一个 Channel 帧归一到 Tab 状态（就地变异草稿；帧到达顺序 = timeline 顺序）。
 *  导出以便测试驱动真实帧序列（[docs/thinking-interleave-report](../../../docs/thinking-interleave-report.md) 契约：delta_text / delta_thinking / tool_progress）。 */
export function applyFrameToTab(t: TabRunState, frame: Frame) {
  if (frame.type === "sub") {
    // [docs/subagent-interaction-drawer](../../../docs/subagent-interaction-drawer.md)：子代理信封帧路由进独立消息流（不走主流水线；流自身按 status 丢弃收尾后的迟到帧）
    applyFrameToSub(t, frame.sub_id, frame.frame);
    return;
  }
  // usage 帧：只进会话级累加器，不进转录。必须放在 !t.running 早退之前——
  // 本轮最后一条 usage 帧常在 run 收尾之后才到达，否则命中率会漏掉最新一轮。
  if (frame.type === "usage") {
    applyUsageFrame(t, frame);
    return;
  }
  if (!t.running) return; // review C1：运行结束后的迟到帧不得重建流式项（幽灵等待指示）
  if (frame.type === "delta_text" || frame.type === "delta_thinking") {
    if (frame.gen < t.streamGen) return; // review C2：run:retry 清场后，丢弃旧尝试的半帧
    t.streamGen = frame.gen;
    // 帧到达顺序 = 展示顺序：text/thinking 按到达顺序追加进 timeline
    appendDelta(currentAssistantIm(t).timeline, frame.type === "delta_text" ? "text" : "thinking", frame.text || "");
  } else if (frame.type === "tool_progress") {
    const callKey = `${frame.batch}:${frame.index}`;
    // 先跨 assistant 项找已有卡：跑批期间 notice 插队会另建末项，若此处只查末项就会为同一次调用再建一张卡
    // （旧卡停在 running，直到 run 收尾才被标「已中断」）
    const tool = findToolViewInItems(t.items, callKey) ?? ensureToolAnchorIm(currentAssistantIm(t), callKey);
    // 帧携带工具名时回填运行中占位卡（幂等守卫：迟到帧不得覆盖 tool:result 已回填的真名）
    if (frame.name && tool.tool === "?") tool.tool = frame.name;
    tool.progressTail = frame.chunk;
  }
}

/** usage 帧载荷（ipc/types.ts 的 Frame usage 形状：snake_case，与后端 dto 一致）。
 *  `duration_ms` = 该 LLM step **成功尝试**的生成耗时（请求发出 → 流失结束，不含失败尝试与退避）；
 *  `ttft_ms` = 首个任意类型增量（text 或 thinking）延迟。两字段缺省/为 null = 该步无数据
 *  （旧后端），按「无数据」处理：不累加、不计步、不显示（[docs/composer-token-rate](../../../docs/composer-token-rate.md)）。 */
export interface UsageFramePayload {
  input?: number;
  output?: number;
  cache_read?: number;
  cache_write?: number;
  duration_ms?: number | null;
  ttft_ms?: number | null;
}

/** usage 帧累加（纯函数，就地变异草稿）：字段缺失按 0 计，帧重放/乱序无害。
 *  只接受帧的 snake_case 字段（cache_read / cache_write），与 Frame 契约一致。 */
export function applyUsageFrame(t: TabRunState, u: UsageFramePayload | undefined) {
  if (!t.usage) t.usage = { input: 0, output: 0, cacheRead: 0, cacheWrite: 0 };
  t.usage.input += u?.input ?? 0;
  t.usage.output += u?.output ?? 0;
  t.usage.cacheRead += u?.cache_read ?? 0;
  t.usage.cacheWrite += u?.cache_write ?? 0;
  applyRunMetricsFrame(t, u);
}

/** 本轮计数累加（[docs/composer-token-rate](../../../docs/composer-token-rate.md)）：与上面会话级 `usage` 平行。
 *  三条纪律：
 *  - **只有「该步被计入」的帧才进分子**：`duration_ms` 缺失 / null / 0 / 负值 → 整帧不计（`output` 也不累加），
 *    与后端 `UsageTiming::add_step`（`src-tauri/src/core/stats.rs`：耗时缺失或 0 → 整步不计）逐字同口径——
 *    否则「带 usage 却不带计时」的帧（旧后端混跑 / 将来新增发帧路径）会让分子多、分母少，速率虚高（AC-17）；
 *  - `genMs` / `steps` 同域累加（分母为 0 的步既不进耗时也不计步，防除零与虚高）；
 *  - `ttftMs` 只取**首个非空值**（本轮首步 TTFT，不是求和；后续步的 TTFT 与整体体感无关），且该步必须已被计入。
 *  惰性建立：任一 usage 帧到达即建（即使字段全缺）——全零计数经 `tokPerSec` 归为「无数据」，显示端整段隐藏。
 *  `output` 的「整帧不计」只作用于本计数器：会话级 `usage`（命中率数据源）仍是「收到就累加」，两者口径分工不同。 */
export function applyRunMetricsFrame(t: TabRunState, u: UsageFramePayload | undefined) {
  const m = (t.runMetrics ??= { output: 0, genMs: 0, steps: 0, ttftMs: null, toolMs: 0 });
  const genMs = u?.duration_ms ?? 0;
  if (genMs > 0) {
    m.output += u?.output ?? 0;
    m.genMs += genMs;
    m.steps += 1;
    if (m.ttftMs == null && u?.ttft_ms != null) m.ttftMs = u.ttft_ms;
  }
}

/** 会话级缓存命中率（0–1），分母按协议语义取（`utils/models.ts::cacheDenominator`）。
 *  无任何用量数据时为 null（显示端据此隐藏「命中」段）——注意 (0,0) 返回 null 而非 0：
 *  没有数据与「有数据但一次都没命中」必须区分。
 *  usage 可选（缺省视为无数据）：兼容既有测试夹具的 TabRunState 字面量。 */
export function cacheHitRate(
  t: { usage?: UsageTotals } | null | undefined,
  sem: CacheSemantics,
): number | null {
  const u = t?.usage;
  if (!u) return null;
  const denom = cacheDenominator(u, sem);
  return denom > 0 ? u.cacheRead / denom : null;
}

/** 工具条上下文占用档（相对自动压缩阈值）：high = 已达阈值（红）/ medium = 阈值 70% 以上（橙）/ low = 默认。 */
export type ContextTier = "low" | "medium" | "high";

/** 上下文占用分档：判定用原始 ratio（不用展示值），避免「显示 60% 却不显红」。 */
export function contextTier(ratio: number, threshold: number): ContextTier {
  if (!Number.isFinite(threshold) || threshold <= 0 || threshold > 1) return "low";
  if (ratio >= threshold) return "high";
  if (ratio >= threshold * 0.7) return "medium";
  return "low";
}

/** 缓存命中率四档着色：ok ≥99% / yellow 95–99% / warn 90–95% / danger <90%。 */
export type HitTier = "ok" | "yellow" | "warn" | "danger";
export function hitRateTier(rate: number): HitTier {
  if (rate >= 0.99) return "ok";
  if (rate >= 0.95) return "yellow";
  if (rate >= 0.9) return "warn";
  return "danger";
}

/** 子代理帧归一（[docs/subagent-interaction-drawer](../../../docs/subagent-interaction-drawer.md)）：镜像主流水线——delta 追加 timeline，tool_progress 落工具锚点。
 *  流不存在时惰性创建（channel 帧与 sub:spawn 事件走不同路径，无到达顺序保证）。 */
export function applyFrameToSub(t: TabRunState, subId: string, frame: Frame) {
  let st = t.subStreams[subId];
  if (!st) {
    st = { timeline: [], toolsMap: {}, status: "running", gen: 0, loaded: false };
    t.subStreams[subId] = st;
  }
  if (st.status !== "running") return; // 收尾后的迟到帧不再进流
  if (frame.type === "delta_text" || frame.type === "delta_thinking") {
    if (frame.gen < st.gen) return;
    st.gen = frame.gen;
    appendDelta(st.timeline, frame.type === "delta_text" ? "text" : "thinking", frame.text || "");
  } else if (frame.type === "tool_progress") {
    const tool = ensureToolAnchorIm(st, `${frame.batch}:${frame.index}`);
    if (frame.name && tool.tool === "?") tool.tool = frame.name; // 同主流：幂等回填占位名
    tool.progressTail = frame.chunk;
  }
}

/** 历史预扫描：tool_use_id → 配对结果（子代理 outcome 解析 / 过程流工具卡结果回填） */
export function scanToolResults(msgs: Message[]): Record<string, { content: string; is_error: boolean }> {
  const out: Record<string, { content: string; is_error: boolean }> = {};
  for (const m of msgs) {
    if (m.role !== "tool") continue;
    for (const c of m.content) {
      if (c.type === "tool_result") {
        const tc = c as any;
        out[tc.tool_use_id] = { content: typeof tc.content === "string" ? tc.content : "", is_error: !!tc.is_error };
      }
    }
  }
  return out;
}

/** 历史恢复：把配对的 tool_result 文本尽量还原成工具卡出参（该文本就是当时的模型侧 JSON）。
 *  解析不出来（被截断、`[error E_XXX: ...]` 等非 JSON）时退回 `{ restored: true }` 占位。
 *  为什么要还原：read 读图卡片的图片条目里 `data_url` 已不再落盘（那段 base64 曾把上下文顶爆），
 *  只剩 `path` / `kind` / `media_type` —— 卡片据此按路径重新加载图片；不还原则 `files` 为空，
 *  图片与占位提示都不会出现。 */
export function restoredToolData(content: string | undefined): unknown {
  if (typeof content !== "string") return { restored: true };
  const s = content.trim();
  if (!s.startsWith("{")) return { restored: true };
  try {
    const v = JSON.parse(s);
    return v && typeof v === "object" ? v : { restored: true };
  } catch {
    return { restored: true };
  }
}

/** args 序列化预览：超长截断为占位对象，序列化失败返回 undefined。
 *  历史恢复（主会话与子代理过程流）也要填 argsPreview：工具卡的摘要行、ask 的问答行
 *  （题干来自入参）与 edit 的 diff 都靠它。 */
export function safeArgsPreview(args: any): string | undefined {
  if (args === undefined || args === null) return undefined;
  try {
    const s = JSON.stringify(args);
    return s.length > 200_000 ? JSON.stringify({ _args_truncated: true }) : s;
  } catch {
    return undefined;
  }
}

/** 历史 Message[] → 子代理过程流（[docs/subagent-interaction-drawer](../../../docs/subagent-interaction-drawer.md)）：assistant 的 text/thinking 按序进 timeline，
 *  tool_use 落工具卡锚点并回填配对的 tool_result；user 任务消息不进流（抽屉头部单独展示 sub.task）。
 *  settleRunning=true（流所属会话已结束）时，缺配对结果的调用落定「已中断」（与 settleRunningTools 同形）；
 *  默认 false 保持既有行为（留 running，等实时事件回填）。 */
export function messagesToSubStream(msgs: Message[], settleRunning = false): { timeline: TimelineSeg[]; toolsMap: Record<string, ToolView> } {
  const results = scanToolResults(msgs);
  const timeline: TimelineSeg[] = [];
  const toolsMap: Record<string, ToolView> = {};
  for (const m of msgs) {
    if (m.role !== "assistant") continue;
    for (const c of m.content) {
      if (c.type === "text") {
        appendDelta(timeline, "text", (c as any).text ?? "");
      } else if (c.type === "thinking") {
        appendDelta(timeline, "thinking", (c as any).text ?? "");
      } else if (c.type === "tool_use") {
        const callKey = (c as any).id as string;
        const tool: ToolView = { callKey, tool: (c as any).name ?? "?", status: "running", progressTail: "" };
        const r = results[callKey];
        if (r) {
          tool.status = r.is_error ? "error" : "ok";
          try {
            tool.outcome = { ok: !r.is_error, data: JSON.parse(r.content) } as any;
          } catch {
            tool.outcome = { ok: !r.is_error, data: { tail: r.content } } as any;
          }
          tool.argsPreview = safeArgsPreview((c as any).args);
        } else if (settleRunning) {
          // 无配对结果且流已收尾：落定「已中断」（运行中被掐断的调用不会有结果事件）
          markInterrupted(tool);
        }
        timeline.push({ kind: "tool", callKey });
        toolsMap[callKey] = tool;
      }
    }
  }
  return { timeline, toolsMap };
}
