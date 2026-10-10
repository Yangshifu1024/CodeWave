import { memo, useCallback, useEffect, useMemo, useRef, useState } from "react";
import { Collapse } from "antd";
import { useTranslation } from "react-i18next";
import { renderMarkdown } from "../../utils/markdown";
import type { TimelineSeg, ToolView } from "../../stores/run";
import ToolCallCard from "../tools/ToolCallCard";
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
  const html = renderMarkdown(text, diagramPending);
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

// ---------- 打字机逐字揭示（[docs/typewriter-stream](../../../../docs/typewriter-stream.md)） ----------
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
}: {
  text: string;
  active: boolean;
  diagramPending?: string;
}) {
  // active 由 true 翻 false 即收尾（定稿 / 用户中断）：此时按 FINISH_BUDGET_MS 快速追平后直出。
  const revealed = useTypewriter(text, active);
  const visible = sliceRevealed(text, revealed);
  const html = renderMarkdown(visible, diagramPending);
  return <div className="md" data-streaming={active || undefined} dangerouslySetInnerHTML={{ __html: html }} />;
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


// 思考标题右侧占满余下头部空间的通栏走马灯（[docs/thinking-marquee-rewrite](../../../../docs/thinking-marquee-rewrite.md)）：显示思考文本的最新一行。
// 一行先锚定在左缘；溢出通栏后自右向左爬行、最新内容钉在右缘（tail-follow）；只有流真正
// 换到新的一行（来了 "\n"）才翻转上滚——原行内增长绝不重触发滚动。
const MARQUEE_DWELL_MS = 700; // 一行在左缘静置片刻后才开始爬行
const MARQUEE_TICK_MS = 150; // 爬行采样间隔；CSS 过渡把阶梯平滑成连续爬行
const MARQUEE_ROLL_MS = 300; // 与 app.css 的 thinking-roll-up 动画时长保持同步
/** 思考走马灯：单行展示最新思考内容，溢出时 tail-follow 爬行、换行时上滚翻转；
 *  prefers-reduced-motion 下退化为静态左锚定。 */
function ThinkingMarquee({ text }: { text: string }) {
  const boxRef = useRef<HTMLSpanElement>(null);
  const lineRef = useRef<HTMLSpanElement>(null);
  const lines = useMemo(() => text.split("\n"), [text]);
  const latestLine = lines[lines.length - 1] ?? "";
  const [curr, setCurr] = useState(""); // 当前展示的行
  const [prev, setPrev] = useState<string | null>(null); // 上滚翻转中的旧行
  const [x, setX] = useState(0); // 展示行的水平偏移（tail-follow 爬行）
  const appearAt = useRef(0);
  const lineCountRef = useRef(lines.length); // 已见行数；只有计数增加才算真正换行

  useEffect(() => {
    const t = latestLine.trim();
    if (!t || t === curr) return;
    // 行数超过已处理行数即视为出现真正的新行（"\n" 之后短暂的空白瞬间不推进 ref 跳过，
    // 迟到的字符仍会计入「新行」）
    if (lines.length > lineCountRef.current && curr) {
      setX(0);
      appearAt.current = Date.now();
      setPrev(curr); // 翻转上滚；翻转中新到的行只是给进行中的动画换目标
    }
    setCurr(t);
    lineCountRef.current = lines.length;
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [latestLine, curr, prev]);

  // 翻转结束后释放旧行（用超时而非 animationend，保证 prefers-reduced-motion 禁用
  // CSS 动画时滚动同样能完成）
  useEffect(() => {
    if (prev === null) return;
    const id = setTimeout(() => setPrev(null), MARQUEE_ROLL_MS + 20);
    return () => clearTimeout(id);
  }, [prev]);

  // tail-follow 爬行：行溢出（且静置期已过）后保持行尾可见于右缘；对仍在增长的行，
  // 溢出量持续增加，逐 tick 重定位读起来就是连续的自右向左爬行。
  // reduced-motion 用户得到静态左锚定的行。
  useEffect(() => {
    const reduced = window.matchMedia("(prefers-reduced-motion: reduce)").matches;
    const id = setInterval(() => {
      const box = boxRef.current;
      const line = lineRef.current;
      if (!box || !line) return;
      const overflow = Math.ceil(line.scrollWidth - box.clientWidth);
      if (overflow <= 2) {
        setX(0);
        return;
      }
      if (reduced || Date.now() - appearAt.current < MARQUEE_DWELL_MS) return;
      setX(-overflow);
    }, MARQUEE_TICK_MS);
    return () => clearInterval(id);
  }, []);

  return (
    <span className="thinking-marquee" ref={boxRef} aria-hidden>
      <span className={prev !== null ? "thinking-roll rolling" : "thinking-roll"}>
        {prev !== null && <span className="thinking-roll-line out">{prev || "\u00A0"}</span>}
        <span className="thinking-roll-line in" ref={lineRef} style={{ transform: `translateX(${x}px)` }}>
          {curr || "\u00A0"}
        </span>
      </span>
    </span>
  );
}

/** 思考折叠块：头部为耗时标签 + 走马灯，展开后思考体 live tail-follow；
 *  展开/收起即阅读意图信号，经 onToggle 通知外层暂停自动跟随。 */
export const ThinkingBlock = memo(function ThinkingBlock({
  text,
  startedAt,
  durationMs,
  onToggle,
}: {
  text: string;
  startedAt?: number;
  durationMs?: number;
  onToggle?: () => void;
}) {
  const { t } = useTranslation();
  const active = durationMs === undefined && startedAt !== undefined; // 活跃中：尚未收尾且有开始时间
  // 展开态 + 内部 tail-follow 开关（用户在思考体内上滚即暂停，回到底部恢复）
  const [expanded, setExpanded] = useState(false);
  const bodyRef = useRef<HTMLDivElement>(null);
  const tailRef = useRef(true);
  // 活跃期间以 500ms 心跳刷新耗时计时（完成后无 interval，时间定格）
  const [, setTick] = useState(0);
  useEffect(() => {
    if (!active) return;
    const id = setInterval(() => setTick((n) => n + 1), 500);
    return () => clearInterval(id);
  }, [active]);
  // live tail-follow：展开且用户未上滚时，思考体始终滚到底部（tail -f）
  useEffect(() => {
    if (!expanded || !tailRef.current) return;
    const el = bodyRef.current;
    if (el) el.scrollTop = el.scrollHeight;
  }, [expanded, text]);
  function onBodyScroll() {
    const el = bodyRef.current;
    if (!el) return;
    tailRef.current = el.scrollHeight - el.scrollTop - el.clientHeight < 20;
  }
  const elapsed = active && startedAt ? Date.now() - startedAt : durationMs ?? 0;
  const secs = Math.max(1, Math.floor(elapsed / 1000));
  const secondsLabel = `${secs}s`;

  const label = active
    ? t("chat.thinkingActive", { seconds: secondsLabel })
    : t("chat.thinkingDone", { seconds: secondsLabel });
  return (
    <Collapse
      size="small"
      ghost
      className="thinking-block"
      onChange={(keys) => {
        const exp = Array.isArray(keys) ? keys.includes("1") : keys === "1";
        setExpanded(exp);
        if (exp) tailRef.current = true; // 重新展开即恢复 tail-follow
        onToggle?.(); // 展开/收起 = 阅读意图：外层暂停自动跟随，思考体切换为 live tail-follow
      }}
      items={[
        {
          key: "1",
          label: (
            <span className="thinking-label">
              <span className="thinking-title">{label}</span>
              {active && <ThinkingMarquee text={text} />}
            </span>
          ),
          children: (
            <div className="thinking-body" ref={bodyRef} onScroll={onBodyScroll}>
              {text}
            </div>
          ),
        },
      ]}
    />
  );
});

/** timeline 段渲染（[docs/subagent-interaction-drawer](../../../../docs/subagent-interaction-drawer.md) 自 ChatMessages 抽取共享）：思考折叠面板 / 工具卡 /
 *  子代理卡 / markdown 正文；主聊天与子代理过程抽屉共用同一视觉。 */
export function TimelineSegsView({
  timeline,
  toolsMap,
  streaming,
  onUserToggle,
  stripReport = false,
}: {
  timeline: TimelineSeg[];
  toolsMap: Record<string, ToolView>;
  streaming?: boolean;
  onUserToggle?: () => void;
  /** 是否剥离 `<report>` 协议标记（仅子代理过程流抽屉开启，见 stripReportMarkers）。
   *  默认 false：主聊天正文引用该标记是合法内容，不可全局剥离。 */
  stripReport?: boolean;
}) {
  const { t } = useTranslation();
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
    <>
      {timeline.map((seg, i) => {
        if (seg.kind === "thinking") {
          return <ThinkingBlock key={i} text={seg.text} startedAt={seg.startedAt} durationMs={seg.durationMs} onToggle={onUserToggle} />;
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
          return tool ? <ToolCallCard key={seg.callKey} tool={tool} onToggle={onUserToggle} /> : null;
        }
        if (seg.kind === "sub") {
          return <SubagentItemCard key={seg.subId} subId={seg.subId} />;
        }
        // 剥离发生在渲染时（text 段已合并，标记完整）；见 stripReportMarkers 里关于增量分片的理由。
        // 注意：剥离必须在打字机切片**之前** —— 切片会把 <report> 切成两片，破坏剥离的完整性前提。
        const text = stripReport ? stripReportMarkers(seg.text) : seg.text;
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
            />
          );
        }
        const html = renderCached(text, diagramPending);
        // data-streaming：流式消息内的 mermaid 占位符推迟到定稿（diagrams.ts 据此跳过），防止滚动风暴
        return <div key={i} className="md" data-streaming={streaming || undefined} dangerouslySetInnerHTML={{ __html: html }} />;
      })}
    </>
  );
}
