import { useCallback, useEffect, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import { renderMarkdown } from "../../utils/markdown";
import type { TimelineSeg, ToolView } from "../../stores/run";
import ToolCallCard from "../tools/ToolCallCard";
import type { ToolExpansion } from "../tools/ToolCallCard";
import ToolActivityGroup, { type ProcessGroupEntry } from "../tools/ToolActivityGroup";
import { ThinkingBlock } from "./thinkingBlock";
import { canJoinProcessGroup, processGroupKey, type ProcessMember, type ToolOrigin } from "../tools/toolPresentation";
import { useUi } from "../../stores/ui";
import SubagentItemCard from "../subagent/SubagentItemCard";

// markdown 渲染缓存：定稿的历史消息在流式期间不重复解析（容量上限防止长会话无限增长）
const MD_CACHE_MAX = 160;
const mdCache = new Map<string, string>();
/** 带缓存的 markdown 渲染：LRU 语义（超容量淘汰最早条目）；流式未定稿文本勿用。
 *  缓存键含 `diagramPending`（提示文案随语言变），否则切语言后会从缓存里取回旧语言的提示。 */
export function renderCached(text: string, diagramPending?: string): string {
  const key = `${diagramPending ?? ""}\u0000${text}`;
  const hit = mdCache.get(key);
  if (hit !== undefined) return hit;
  const html = renderMarkdown(text, diagramPending, { repairAssistantBlocks: true });
  if (mdCache.size >= MD_CACHE_MAX) {
    const first = mdCache.keys().next().value;
    if (first !== undefined) mdCache.delete(first);
  }
  mdCache.set(key, html);
  return html;
}

/** 剥离子代理汇报协议标记（`<report>` / `</report>`）：子代理与主代理之间用该标记
 *  包裹最终汇报，主聊天需要它、子代理过程流抽屉里则应只展示正文。
 *  刻意放在「渲染时」而非「delta 到达时」：增量流会把标记切成 `<repo` + `rt>` 两片，
 *  任一时刻单独剥离都会漏网；而渲染时 text 段已完成合并，标记必然完整。
 *  另注：绝不在主聊天默认开启——主聊天正文里引用该标记是合法内容，全局剥离会丢内容。 */
export function stripReportMarkers(text: string): string {
  // trim：标记通常独自占一整行，剥掉后会留下前后空行
  return text.replace(/<\/?report>/g, "").trim();
}

// ---------- 打字机逐字揭示（[docs/chat-and-interface](../../../../docs/chat-and-interface.md)） ----------
//
// 助手回复在流式输出时逐字铺开，接近 ChatGPT / Codex 的观感。四条硬约束：
// 1) 揭示游标只放组件局部 state，**绝不写 store** —— ChatMessages.tsx 的 contentLen 依赖 text
//    字符总数，若逐帧写 store 会让自动跟随 effect 以 60fps 触发 jumpToBottom + upgradeDiagrams，
//    撞既有滚动豁免逻辑（onWheel 上滚清窗口），用户上滚会被反复拽回底部。
// 2) 唯一开关 = 既有的 `streaming && i === tailIdx` —— 会话恢复 / 翻页加载的项恒 streaming:false
//    （run.ts 的恢复路径），天然整段直出、绝不重播，无需任何额外判断。
// 3) report 标记先剥离再切片 —— 切片会把 <report> 切成两片，破坏 stripReportMarkers 的完整性前提。
// 4) 不加闪烁光标 —— .cursor 类名与 CSS cursor 属性同名冲突，是历史误伤点（app.css 明确警告）。

/** 基础揭示速度（字/秒）：跟随数据时的原速基准。快模型靠自动追赶追平，慢模型原速不拖尾。 */
const BASE_CPS = 90;
/** 积压追赶增益：积压越多，每帧推进越快，避免长回复末尾长时间慢放。 */
const CATCHUP_GAIN = 0.05;
/** 思考静默阈值（ms）：模型停顿超过此值后回来，直接放行不逐字（否则像"慢放堆积文本"）。 */
const SILENT_THRESHOLD_MS = 800;
/** 首帧基础配额（字）：首帧就不能是 0 —— 否则会闪一下空白，且同步断言 / 辅助技术读不到内容。
首帧给一小段，之后由 rAF 逐帧推进，观感仍是逐字。 */
const FIRST_FRAME_CHARS = 12;

/** 游标收敛到 [0, len]：目标变短（如 report 剥离后）不越界，也不倒退。 */
export function clampReveal(v: number, len: number): number {
  if (Number.isNaN(v) || v <= 0) return 0;
  if (!Number.isFinite(v) || v > len) return len;
  return Math.floor(v);
}

/**
 * 按帧推进揭示游标（纯函数，无计时器无 DOM —— 可直接单测）。
 *
 * 跟随数据 + 自动追赶：每帧基础步长按 BASE_CPS 折算，再乘以「积压越大推进越快」的追赶增益。
 * 三种特殊情形：
 * - `silentMs > SILENT_THRESHOLD_MS`：思考停顿过久后回来，直接放行到全文（不慢放）。
 * - `finishing`（streaming 翻 false / 用户中断）：一次性放行到全文（收尾即完整，不拖尾）。
 * - 游标已追上目标：原样返回（追平即停，由调用方停 rAF）。
 *
 * 返回值恒在 [0, len] 且单调不减。
 */
export function advanceReveal(
  prev: number,
  len: number,
  dtMs: number,
  opts?: { silentMs?: number; finishing?: boolean },
): number {
  const target = clampReveal(prev, len);
  if (target >= len) return len;
  const silent = opts?.silentMs ?? 0;
  const dt = Number.isFinite(dtMs) && dtMs > 0 ? dtMs : 0;
  // 静默过久 → 直接放行（不逐字慢放堆积文本）
  if (silent > SILENT_THRESHOLD_MS) return len;
  const backlog = len - target;
  if (backlog <= 0) return len;
  // dt 为 0（首帧 / 同帧重复调用）→ 按最小步长 1 字推进，保证单调不减且可见
  const basePerFrame = dt > 0 ? (BASE_CPS * dt) / 1000 : 1;
  // 追赶增益：积压越多推进越快；上限 8 字/帧，避免长回复瞬间跳完失去打字机观感
  const catchUp = Math.min(8, 1 + backlog * CATCHUP_GAIN);
  const step = basePerFrame * catchUp;
  // 收尾 / 中断（streaming 翻 false / 用户停止）：**一次性放行到全文**。
  // 为什么不用「预算内摊分」：摊分需要「进入收尾时的初始剩余量」才能定出固定步长，
  // 而 backlog 每帧递减会让步长同步缩水、帧数按对数级膨胀（实测 300 字要 47 帧 ≈ 750ms，
  // 反而比直接放行更拖沓，正是 O-3 要避免的「拖尾」）。收尾本就应立即呈现完整内容。
  if (opts?.finishing) return len;
  // 不足 1 字时仍保证至少 1 字（单调不减的可观测保证）
  const next = target + Math.max(1, Math.floor(step));
  return clampReveal(next, len);
}

/** 按游标切片（纯函数）：revealed 恒在 [0, text.length] 内。 */
export function sliceRevealed(text: string, revealed: number): string {
  const n = clampReveal(revealed, text.length);
  return n >= text.length ? text : text.slice(0, n);
}

/** 是否处于"减弱动态效果"偏好（读一次即缓存：系统级设置，运行期不会变）。 */
let reducedMotionCache: boolean | null = null;
export function prefersReducedMotion(): boolean {
  if (reducedMotionCache !== null) return reducedMotionCache;
  try {
    reducedMotionCache = window.matchMedia("(prefers-reduced-motion: reduce)").matches;
  } catch {
    reducedMotionCache = false;
  }
  return reducedMotionCache;
}

/** 仅供测试重置缓存（生产路径不调用）。 */
export function __resetReducedMotionCache(): void {
  reducedMotionCache = null;
}

function TypewriterText({
  text,
  active,
  diagramPending,
  anchorKey,
}: {
  text: string;
  active: boolean;
  diagramPending?: string;
  anchorKey: string;
}) {
  // active 由 true 翻 false 即收尾（定稿 / 用户中断）：此时按 FINISH_BUDGET_MS 快速追平后直出。
  const revealed = useTypewriter(text, active);
  const visible = sliceRevealed(text, revealed);
  const html = renderMarkdown(visible, diagramPending, { repairAssistantBlocks: true });
  return <div className="md" data-output-key={anchorKey} data-streaming={active || undefined} dangerouslySetInnerHTML={{ __html: html }} />;
}

/**
 * 打字机 hook：返回当前应揭示的字符数。
 *
 * 三态直出（均首帧同步生效，不依赖 rAF）：
 * - `active === false`（非流式 / 非尾段 / 定稿）→ text.length
 * - prefers-reduced-motion → text.length
 *
 * 其余情况走 rAF 循环按帧推进；游标追上目标即停并释放 rAF。
 * 游标存于局部 state，绝不写 store（见文件头约束 1）。
 */
function useTypewriter(
  text: string,
  active: boolean,
  opts?: { finishing?: boolean },
): number {
  const len = text.length;
  const [revealed, setRevealed] = useState(() =>
    active && !prefersReducedMotion() ? Math.min(len, FIRST_FRAME_CHARS) : len,
  );
  const rafRef = useRef(0);
  const lastTsRef = useRef(0);
  const silentRef = useRef(0);
  const finishingRef = useRef(false);

  useEffect(() => {
    finishingRef.current = opts?.finishing ?? false;
  }, [opts?.finishing]);

  // 非流式 / reduced-motion：直出，且不留 rAF
  useEffect(() => {
    if (!active || prefersReducedMotion()) {
      if (rafRef.current) {
        cancelAnimationFrame(rafRef.current);
        rafRef.current = 0;
      }
      setRevealed(len);
      return;
    }
    // 目标变短（report 剥离等）：先收敛，绝不越界
    setRevealed((r) => clampReveal(r, len));
  }, [active, len]);

  const tick = useCallback(() => {
    const dt = lastTsRef.current ? Math.max(0, performance.now() - lastTsRef.current) : 0;
    lastTsRef.current = performance.now();
    setRevealed((prev) => {
      const next = advanceReveal(prev, len, dt, {
        silentMs: silentRef.current,
        finishing: finishingRef.current,
      });
      // 追平即停：不再排下一帧
      if (next >= len) {
        rafRef.current = 0;
        return len;
      }
      rafRef.current = requestAnimationFrame(tick);
      return next;
    });
  }, [len]);

  // 启动 / 续跑 rAF 循环（仅在 active 且未追平时）
  useEffect(() => {
    if (!active || prefersReducedMotion()) return;
    if (revealed >= len) return;
    if (rafRef.current) return;
    lastTsRef.current = performance.now();
    rafRef.current = requestAnimationFrame(tick);
    // 卸载时清理
    return () => {
      if (rafRef.current) {
        cancelAnimationFrame(rafRef.current);
        rafRef.current = 0;
      }
    };
  }, [active, revealed, len, tick]);

  return active && !prefersReducedMotion() ? clampReveal(revealed, len) : len;
}


/** timeline 段渲染（[docs/subagents](../../../../docs/subagents.md) 自 ChatMessages 抽取共享）：思考折叠面板 / 工具卡 /
 *  子代理卡 / markdown 正文；主聊天与子代理过程抽屉共用同一视觉。 */
export function TimelineSegsView({
  timeline,
  toolsMap,
  streaming,
  onUserToggle,
  stripReport = false,
  autoOpenCallKey,
  onConsumeAutoOpen,
  toolOrigins,
  toolExpansions,
  onToolExpansionChange,
  groupExpansions,
  onGroupExpansionChange,
}: {
  timeline: TimelineSeg[];
  toolsMap: Record<string, ToolView>;
  streaming?: boolean;
  onUserToggle?: () => void;
  /** 是否剥离 `<report>` 协议标记（仅子代理过程流抽屉开启，见 stripReportMarkers）。
   *  默认 false：主聊天正文引用该标记是合法内容，不可全局剥离。 */
  stripReport?: boolean;
  /** 自动弹预览信号（[docs/plan-and-approval](../../../../docs/plan-and-approval.md)）：命中的 widget 工具卡落地即弹框。
   *  刻意不设默认值：子代理过程抽屉 `SubagentDrawer` 调本组件时不传这两项 → 抽屉内的 widget 不会自动弹框
   *  （抽屉本身已在展示子代理的完整过程，再叠一个自动弹框只会糊住抽屉内容），这是有意为之。 */
  autoOpenCallKey?: string | null;
  /** 卡片已消费该信号后回抛（清掉 store 里的信号，保证一次性、不重放） */
  onConsumeAutoOpen?: (callKey: string) => void;
  toolOrigins?: Record<string, ToolOrigin>;
  toolExpansions?: Record<string, ToolExpansion>;
  onToolExpansionChange?: (callKey: string, expansion: ToolExpansion) => void;
  groupExpansions?: Record<string, boolean>;
  onGroupExpansionChange?: (callKey: string, expanded: boolean) => void;
}) {
  const { t } = useTranslation();
  const display = useUi((s) => s.outputDisplay);
  // callKey 所属时间线持有展开态，父结构/模式改变也不丢失正在阅读的详情。
  const [localExpansions, setExpansions] = useState<Record<string, ToolExpansion>>({});
  const expansions = toolExpansions ?? localExpansions;
  const updateExpansion = useCallback((callKey: string, expansion: ToolExpansion) => {
    if (onToolExpansionChange) onToolExpansionChange(callKey, expansion);
    else setExpansions((previous) => ({ ...previous, [callKey]: expansion }));
  }, [onToolExpansionChange]);
  // 简洁模式的过程分组：连续的思考 + 可入组工具合并为一行摘要（docs/chat-and-interface 5.2.x）。
  // 仍分隔的：真实正文、子代理卡、已打开详情的工具、审批等待与失败。
  const groups = new Map<number, ProcessMember[]>();
  const grouped = new Set<number>();
  if (display === "compact") {
    for (let i = 0; i < timeline.length; i++) {
      const first = i;
      const members: ProcessMember[] = [];
      while (i < timeline.length) {
        const seg = timeline[i];
        // 空白正文不分隔可见过程（只在本组已有成员时吸收，否则留给正文渲染分支）
        if (seg.kind === "text" && !seg.text.trim() && members.length > 0) {
          grouped.add(i++);
          continue;
        }
        // 正在阅读详情的工具独立保留：不能被卷进组，否则点开它会连带展开同组其它工具
        if (seg.kind === "tool" && expansions[seg.callKey]?.expanded) break;
        if (!canJoinProcessGroup(seg, toolsMap, { includeRunning: true })) break;
        // 谓词已确认非 thinking 段必为 tool 且能在 toolsMap 里取到；显式分流让 TS 看得见
        if (seg.kind === "thinking") members.push({ kind: "thinking", index: i, seg });
        else if (seg.kind === "tool") members.push({ kind: "tool", index: i, callKey: seg.callKey, tool: toolsMap[seg.callKey] });
        grouped.add(i++);
      }
      if (members.length) { groups.set(first, members); i--; }
    }
  }
  // 流式等待指示跟随最后一个未定稿的 text 段（其后只有 thinking/tool/sub 时，指示落在空尾）
  let tailIdx = -1;
  for (let i = timeline.length - 1; i >= 0; i--) {
    const s = timeline[i];
    if (s.kind === "tool" || s.kind === "sub") break;
    if (s.kind === "text") {
      tailIdx = i;
      break;
    }
  }
  return (
    <div className={display === "compact" ? "timeline-segs timeline-compact" : "timeline-segs"}>
      {timeline.map((seg, i) => {
        const group = groups.get(i);
        if (group) {
          const rootKey = processGroupKey(group);
          if (!rootKey) return null;
          const tools = group.filter(member => member.kind === "tool").map(member => member.tool);
          const entry: ProcessGroupEntry = { key: rootKey, tools, members: group };
          // 展开态回退：键不存在时传 undefined，让组用本地 state。
          // 不能恒传 !!groupExpansions[rootKey] —— 纯思考组（组内无工具）在单消息里
          // 不生成 origins，键永远读不到，受控值会恒为 false 把本地 state 完全盖掉，点开无反应。
          const expanded = groupExpansions && rootKey in groupExpansions ? groupExpansions[rootKey] : undefined;
          return <ToolActivityGroup key={`group:${rootKey}`} entry={entry} timeline={timeline} streaming={streaming} display={display} origins={toolOrigins} onToggle={onUserToggle} onUserToggle={onUserToggle} onExpansionChange={updateExpansion} expanded={expanded} onGroupExpansionChange={onGroupExpansionChange} />;
        }
        if (grouped.has(i)) return null;
        if (seg.kind === "thinking") {
          return <ThinkingBlock key={i} text={seg.text} startedAt={seg.startedAt} durationMs={seg.durationMs} onToggle={onUserToggle} compact={display === "compact"} streaming={!!streaming && i === timeline.length - 1} />;
        }
        if (seg.kind === "tool") {
          const tool = toolsMap[seg.callKey];
          // subagent 调用已有对应的 sub 段卡片，它自己的通用工具卡不再渲染（否则同一调用出现两张卡）。
          // 为什么在渲染层过滤而不是 store 层：「tool:result 事件（带真名）与 tool_progress 帧
          // （可能仍是占位名 "?"）到达顺序无保证，store 层无法可靠判定；渲染层过滤对两种顺序
          // 都成立，且与恢复路径「不留工具卡段」的语义一致。
          // 仅在工具名明确等于 "subagent" 时跳过；"?"（占位未回填）照常渲染，避免误伤真正运行中的工具。
          // 段本身仍保留在 timeline 里（等待指示定位等既有逻辑依赖它），只是不渲染卡片。
          // 例外：**失败的调用必须照常渲染**——E_ARGS / E_SUBAGENT_BUSY 在 sub:spawn 之前就返回了
          // （tools/subagent.rs 的校验与并发抢槽早于 spawn），此时 timeline 里根本没有 sub 段，
          // 再过滤掉工具卡就会让这次调用在聊天里零痕迹（连错误码都看不到）。
          // 反过来说，spawn 之后的失败（用户停止 / 内部异常）已有 sub 段承载，会多出一张错误卡，
          // 但那是修复前就存在的情况，不是回归；此处不引入按 callKey 关联 sub_id 的精确匹配（事件侧
          // 无该关联字段，属于契约变更，另开）。
          if (tool?.tool === "subagent" && tool.status !== "error") return null;
          return tool ? (
            <ToolCallCard
              key={seg.callKey}
              tool={tool}
              origin={toolOrigins?.[tool.callKey]}
              display={display}
              expansion={expansions[tool.callKey]}
              onExpansionChange={updateExpansion}
              onToggle={onUserToggle}
              autoOpenCallKey={autoOpenCallKey}
              onConsumeAutoOpen={onConsumeAutoOpen}
            />
          ) : null;
        }
        if (seg.kind === "sub") {
          return <SubagentItemCard key={seg.subId} subId={seg.subId} />;
        }
        // 剥离发生在渲染时（text 段已合并，标记完整）；见 stripReportMarkers 里关于增量分片的理由。
        // 注意：剥离必须在打字机切片**之前** —— 切片会把 <report> 切成两片，破坏剥离的完整性前提。
        const text = stripReport ? stripReportMarkers(seg.text) : seg.text;
        if (display === "compact" && !text.trim()) return null;
        // mermaid 占位提示的文案走 i18n（写死在 CSS / HTML 里的中文在英文界面会露馅）
        const diagramPending = t("chat.diagramPending");
        // 流式尾段走打字机（逐字揭示）；其余段（定稿 / 历史 / 中间段）完全走原 renderCached，一字不改。
        // 唯一开关 = 既有的 `streaming && i === tailIdx`：会话恢复与翻页加载的项恒 streaming=false，
        // 因此天然整段直出、绝不重播（打字机的"老会话不重播"不需要额外判断）。
        if (streaming && i === tailIdx) {
          return (
            <TypewriterText
              key={i}
              text={text}
              active={streaming}
              diagramPending={diagramPending}
              anchorKey={`text:${i}`}
            />
          );
        }
        const html = renderCached(text, diagramPending);
        // data-streaming：流式消息内的 mermaid 占位符推迟到定稿（diagrams.ts 据此跳过），防止滚动风暴
        return <div key={i} className="md" data-output-key={`text:${i}`} data-streaming={streaming || undefined} dangerouslySetInnerHTML={{ __html: html }} />;
      })}
    </div>
  );
}
