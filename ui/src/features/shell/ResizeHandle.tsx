// 栏宽分隔条（手写，零新依赖；[docs/rightbar-info-refactor-and-subscription-quota](../../../../docs/rightbar-info-refactor-and-subscription-quota.md)）。
//
// 交互：pointer 拖动（setPointerCapture，拖出元素仍持续跟手）+ 方向键 ±16px 微调 +
// 双击复位该侧默认宽；拖动期间给 <html> 挂 `ws-resizing` 关掉宽度过渡（否则栏会拖影）。
// 无障碍：role=separator + aria-orientation=vertical + aria-valuenow/min/max（可 Tab 聚焦）。
import { useCallback, useEffect, useRef } from "react";

/** 方向键单步微调量（px） */
const KEY_STEP = 16;

interface Props {
  /** nav = 左栏（向右拖变宽）；right = 右栏（向左拖变宽） */
  side: "nav" | "right";
  /** 当前显示宽度（受窗口夹取后的值） */
  width: number;
  min: number;
  max: number;
  /** 分隔条距容器对应边的偏移（px），由调用方按显示宽度给出 */
  offset: number;
  label: string;
  /** 当前显示宽度已被窗口夹小：拖拽/键盘会写坏记忆值，此时只留双击复位 */
  disabled?: boolean;
  onWidth: (width: number) => void;
  onReset: () => void;
}

/** 拖动期间关宽度过渡：类挂在 documentElement 上，CSS 一条规则全局生效 */
function setResizing(on: boolean) {
  if (typeof document === "undefined") return;
  document.documentElement.classList.toggle("ws-resizing", on);
}

export default function ResizeHandle({
  side,
  width,
  min,
  max,
  offset,
  label,
  disabled = false,
  onWidth,
  onReset,
}: Props) {
  const drag = useRef<{ startX: number; startWidth: number } | null>(null);

  const clamp = useCallback(
    (value: number) => Math.min(max, Math.max(min, Math.round(value))),
    [max, min],
  );

  const endDrag = (target: HTMLElement | null, pointerId: number) => {
    drag.current = null;
    setResizing(false);
    try {
      target?.releasePointerCapture(pointerId);
    } catch {
      // 指针已被取消/释放：忽略
    }
  };

  // 卸载兜底：拖动中面板被收起/组件被卸载时，ws-resizing 不能留在 <html> 上（会全局关掉宽度过渡）
  useEffect(() => () => setResizing(false), []);

  return (
    <div
      className={`rb-resize-handle resize-${side}${disabled ? " is-disabled" : ""}`}
      style={side === "nav" ? { left: `${offset}px` } : { right: `${offset}px` }}
      role="separator"
      aria-orientation="vertical"
      aria-label={label}
      aria-disabled={disabled || undefined}
      aria-valuenow={width}
      aria-valuemin={min}
      aria-valuemax={max}
      tabIndex={0}
      onPointerDown={(e) => {
        if (disabled || e.button !== 0) return;
        drag.current = { startX: e.clientX, startWidth: width };
        setResizing(true);
        e.currentTarget.setPointerCapture(e.pointerId);
      }}
      onPointerMove={(e) => {
        const state = drag.current;
        if (!state) return;
        const delta = e.clientX - state.startX;
        onWidth(clamp(side === "nav" ? state.startWidth + delta : state.startWidth - delta));
      }}
      onPointerUp={(e) => endDrag(e.currentTarget, e.pointerId)}
      onPointerCancel={(e) => endDrag(e.currentTarget, e.pointerId)}
      // 指针被系统夺走（窗口失焦/触控取消）时同样要摘掉过渡开关
      onLostPointerCapture={(e) => endDrag(e.currentTarget, e.pointerId)}
      onDoubleClick={onReset}
      onKeyDown={(e) => {
        if (e.key !== "ArrowLeft" && e.key !== "ArrowRight") return;
        // disabled 时不吞按键（不防碍方向键自身的滚动语义）
        if (disabled) return;
        e.preventDefault();
        const grow = side === "nav" ? e.key === "ArrowRight" : e.key === "ArrowLeft";
        onWidth(clamp(width + (grow ? KEY_STEP : -KEY_STEP)));
      }}
    />
  );
}
