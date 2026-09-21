import { memo, useEffect, useMemo, useRef, useState } from "react";
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
        // 剥离发生在渲染时（text 段已合并，标记完整）；见 stripReportMarkers 里关于增量分片的理由
        const text = stripReport ? stripReportMarkers(seg.text) : seg.text;
        // 流式条目绕过缓存（文本每帧增长，避免前缀污染缓存）
        // mermaid 占位提示的文案走 i18n（写死在 CSS / HTML 里的中文在英文界面会露馅）
        const diagramPending = t("chat.diagramPending");
        const html = streaming && i === tailIdx ? renderMarkdown(text, diagramPending) : renderCached(text, diagramPending);
        // data-streaming：流式消息内的 mermaid 占位符推迟到定稿（diagrams.ts 据此跳过），防止滚动风暴
        return <div key={i} className="md" data-streaming={streaming || undefined} dangerouslySetInnerHTML={{ __html: html }} />;
      })}
    </>
  );
}
