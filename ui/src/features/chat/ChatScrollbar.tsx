// 自定义 overlay 滚动条（[docs/chat-scrollbar-overlay]）：仿 MiniMax Code / Slack / Discord 范式。
// native 滚动条全部隐藏（系统设置「始终显示滚动条」会画出带箭头的 native widget，CSS 改不掉——见 app.css 对应章节）。
// 本组件监听容器的 scroll 事件，按 scrollTop / scrollHeight 比例渲染细 thumb：
//   - 滚动时显色
//   - 停止 1s 后淡出（透明度过渡 0.25s）
// 不画箭头（彻底没有 native widget 介入）；点击 thumb 区域可点击跳到对应滚动位置。
import { useEffect, useRef, useState, type RefObject } from "react";

const IDLE_MS = 1000;
const MIN_THUMB_PX = 32; // thumb 最小高度（内容过短时也保留可见拖拽区）
const TRACK_PADDING = 6; // thumb 距容器上下边缘的 padding

export default function ChatScrollbar({ target }: { target: RefObject<HTMLElement | null> }) {
  const [visible, setVisible] = useState(false);
  const [hover, setHover] = useState(false);
  const [thumb, setThumb] = useState({ topPx: 0, heightPx: 0, visible: false });
  const idleTimer = useRef<number | null>(null);

  useEffect(() => {
    const el = target.current;
    if (!el) return;

    /** 重新计算 thumb 的 topPx / heightPx（容器尺寸 / 内容尺寸都可能变化：窗口 resize、新消息追加） */
    const update = () => {
      const { scrollHeight, clientHeight } = el;
      const trackH = clientHeight - TRACK_PADDING * 2;
      if (scrollHeight <= clientHeight) {
        // 内容不够滚：thumb 占满整条轨道（视觉对齐：满条说明「没什么可滚」）
        setThumb({ topPx: TRACK_PADDING, heightPx: Math.max(trackH, MIN_THUMB_PX), visible: false });
        return;
      }
      const heightPx = Math.max(trackH * (clientHeight / scrollHeight), MIN_THUMB_PX);
      const topPx = TRACK_PADDING + (trackH - heightPx) * (el.scrollTop / (scrollHeight - clientHeight));
      setThumb({ topPx, heightPx, visible: true });
    };

    /** 显示 + 重新算位置 + 启动 idle 计时器 */
    const poke = () => {
      update();
      setVisible(true);
      if (idleTimer.current !== null) window.clearTimeout(idleTimer.current);
      idleTimer.current = window.setTimeout(() => setVisible(false), IDLE_MS);
    };

    el.addEventListener("scroll", poke, { passive: true });
    // 内容/容器尺寸变化时（消息追加、窗口 resize、侧栏开合）需重算位置
    const ro = new ResizeObserver(() => {
      update();
      poke();
    });
    ro.observe(el);

    // 初始算一次（首屏无滚动条但 thumb 仍需渲染「满条」占位，让用户知道可滚动）
    update();

    return () => {
      el.removeEventListener("scroll", poke);
      ro.disconnect();
      if (idleTimer.current !== null) {
        window.clearTimeout(idleTimer.current);
        idleTimer.current = null;
      }
    };
  }, [target]);

  /** 点击轨道：跳到 thumb 中心对应的 scrollTop */
  const onTrackClick = (e: React.MouseEvent<HTMLDivElement>) => {
    const el = target.current;
    if (!el) return;
    const track = e.currentTarget;
    const trackRect = track.getBoundingClientRect();
    const trackUsableH = track.clientHeight - TRACK_PADDING * 2;
    const clickY = e.clientY - trackRect.top - TRACK_PADDING;
    const ratio = Math.max(0, Math.min(1, (clickY - thumb.heightPx / 2) / (trackUsableH - thumb.heightPx)));
    const max = el.scrollHeight - el.clientHeight;
    el.scrollTop = ratio * max;
  };

  /** 拖拽 thumb 跳位置 */
  const onThumbMouseDown = (e: React.MouseEvent<HTMLDivElement>) => {
    e.preventDefault();
    e.stopPropagation();
    const el = target.current;
    if (!el) return;
    const startY = e.clientY;
    const startTop = el.scrollTop;
    const max = el.scrollHeight - el.clientHeight;
    const track = e.currentTarget.parentElement!;
    const trackUsableH = track.clientHeight - TRACK_PADDING * 2;
    const move = (ev: MouseEvent) => {
      const dy = ev.clientY - startY;
      const ratio = dy / (trackUsableH - thumb.heightPx);
      el.scrollTop = Math.max(0, Math.min(max, startTop + ratio * max));
    };
    const up = () => {
      window.removeEventListener("mousemove", move);
      window.removeEventListener("mouseup", up);
    };
    window.addEventListener("mousemove", move);
    window.addEventListener("mouseup", up);
  };

  // 内容不够滚时 thumb 仍渲染占位，但持续隐藏（用户没东西可滚，无需提示）
  return (
    <div
      className={`chat-scrollbar${visible || hover ? " visible" : ""}`}
      onMouseEnter={() => setHover(true)}
      onMouseLeave={() => setHover(false)}
      data-state={visible || hover ? "on" : "off"}
    >
      <div className="chat-scrollbar-track" onClick={onTrackClick}>
        {thumb.visible && (
          <div
            className="chat-scrollbar-thumb"
            style={{ top: thumb.topPx, height: thumb.heightPx }}
            onMouseDown={onThumbMouseDown}
          />
        )}
      </div>
    </div>
  );
}