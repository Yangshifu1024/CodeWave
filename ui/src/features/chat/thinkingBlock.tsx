import { memo, useEffect, useMemo, useRef, useState } from "react";
import { Collapse } from "antd";
import { useTranslation } from "react-i18next";

// 思考标题右侧占满余下头部空间的通栏走马灯（[docs/chat-and-interface](../../../../docs/chat-and-interface.md)）：显示思考文本的最新一行。
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
 *  展开/收起即阅读意图信号，经 onToggle 通知外层暂停自动跟随。
 *
 *  独立成模块的原因：简洁模式的过程组（ToolActivityGroup）要按原位置渲染组内思考段，
 *  而它又被 segments.tsx 引用 —— 两者互相 import 会成环，故把本组件提到中性位置。 */
export const ThinkingBlock = memo(function ThinkingBlock({
  text,
  startedAt,
  durationMs,
  onToggle,
  compact = false,
  streaming = false,
  anchorKey,
  messageKey,
}: {
  text: string;
  startedAt?: number;
  durationMs?: number;
  onToggle?: () => void;
  compact?: boolean;
  streaming?: boolean;
  anchorKey?: string;
  messageKey?: string;
}) {
  const { t } = useTranslation();
  const active = streaming && durationMs === undefined && startedAt !== undefined;
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
  const elapsed = active && startedAt !== undefined ? Date.now() - startedAt : durationMs ?? 0;
  const secs = Math.max(1, Math.floor(elapsed / 1000));
  const secondsLabel = `${secs}s`;

  const label = active ? t("chat.thinkingActive", { seconds: secondsLabel })
    : durationMs !== undefined ? t("chat.thinkingDone", { seconds: secondsLabel })
      : t("chat.thinking");
  return (
    <Collapse
      size="small"
      ghost
      className={compact ? "thinking-block thinking-compact" : "thinking-block"}
      data-output-key={anchorKey}
      data-output-message-key={messageKey}
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
              {active && !compact && <ThinkingMarquee text={text} />}
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
