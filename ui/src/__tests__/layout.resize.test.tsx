// 可拖拽栏宽（[docs/rightbar-info-refactor-and-subscription-quota](../../../docs/rightbar-info-refactor-and-subscription-quota.md)）：
// 纯函数夹取（只夹显示、保留记忆值）+ 分隔条交互（pointer 拖动 / 方向键 / 双击复位）。
import { describe, it, expect, vi, afterEach } from "vitest";
import { render, fireEvent, cleanup } from "@testing-library/react";
import { App as AntApp } from "antd";
import "../i18n";
import ResizeHandle from "../features/shell/ResizeHandle";
import RightBar from "../features/shell/RightBar";
import { useUi } from "../stores/ui";
import {
  CHAT_W_MIN,
  NAV_W_DEFAULT,
  NAV_W_MAX,
  NAV_W_MIN,
  RB_W_DEFAULT,
  RB_W_MAX,
  RB_W_MIN,
  clampNavWidth,
  clampRightBarWidth,
  dragLimit,
  resolveDisplayWidths,
} from "../utils/layout";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn(async () => []) }));

describe("栏宽纯函数", () => {
  it("读盘/回写值收敛到合法区间，非法值回默认", () => {
    expect(clampNavWidth(999)).toBe(NAV_W_MAX);
    expect(clampNavWidth(10)).toBe(NAV_W_MIN);
    expect(clampNavWidth("abc")).toBe(NAV_W_DEFAULT);
    expect(clampRightBarWidth(Number.NaN)).toBe(RB_W_DEFAULT);
    expect(clampRightBarWidth(200)).toBe(RB_W_MIN);
    expect(clampRightBarWidth(9999)).toBe(RB_W_MAX);
  });

  it("窗口足够宽时原样显示（不改记忆值）", () => {
    expect(resolveDisplayWidths(NAV_W_DEFAULT, RB_W_DEFAULT, 1600)).toEqual({
      nav: NAV_W_DEFAULT,
      rightBar: RB_W_DEFAULT,
    });
  });

  it("窗口变窄先压右栏、再压左栏，各自触底；不产生负值", () => {
    // 窄窗：右栏先压到 328（下限），仍不够再压左栏
    const narrow = resolveDisplayWidths(NAV_W_DEFAULT, 600, 1024);
    expect(narrow.rightBar).toBeGreaterThanOrEqual(RB_W_MIN);
    expect(narrow.nav).toBeGreaterThanOrEqual(NAV_W_MIN);
    // 中栏保底：两侧合计不超过 窗口 - CHAT_W_MIN（允许中栏自身继续压缩的下限约束）
    expect(narrow.nav + narrow.rightBar).toBeLessThanOrEqual(1024 - CHAT_W_MIN);
    // 极窄窗口不塌成 0
    const tiny = resolveDisplayWidths(NAV_W_MAX, RB_W_MAX, 700);
    expect(tiny.nav).toBeGreaterThanOrEqual(NAV_W_MIN);
    expect(tiny.rightBar).toBeGreaterThanOrEqual(RB_W_MIN);
  });

  it("拖动上限考虑另一侧占用与中栏保底", () => {
    expect(dragLimit("nav", RB_W_DEFAULT, 1600)).toBe(NAV_W_MAX);
    expect(dragLimit("right", NAV_W_DEFAULT, 1600)).toBe(RB_W_MAX);
    // 窄窗下右栏最多只能拿到 窗口 - 中栏保底 - 左栏（且不低于自身下限）
    expect(dragLimit("right", NAV_W_DEFAULT, 1024)).toBe(RB_W_MIN);
  });
});

describe("分隔条交互", () => {
  const props = {
    side: "nav" as const,
    width: 280,
    min: NAV_W_MIN,
    max: NAV_W_MAX,
    offset: 280,
    label: "调整左栏宽度",
  };

  afterEach(() => cleanup());

  it("左栏分隔条：aria 语义 + 位置内联", () => {
    const { container } = render(<ResizeHandle {...props} onWidth={() => {}} onReset={() => {}} />);
    const handle = container.querySelector(".rb-resize-handle") as HTMLElement;
    expect(handle.getAttribute("role")).toBe("separator");
    expect(handle.getAttribute("aria-orientation")).toBe("vertical");
    expect(handle.getAttribute("aria-label")).toBe("调整左栏宽度");
    expect(handle.getAttribute("aria-valuenow")).toBe("280");
    expect(handle.style.left).toBe("280px");
  });

  it("方向键 ±16px 微调并按 min/max 夹取", () => {
    const onWidth = vi.fn();
    const { container } = render(<ResizeHandle {...props} onWidth={onWidth} onReset={() => {}} />);
    const handle = container.querySelector(".rb-resize-handle") as HTMLElement;

    fireEvent.keyDown(handle, { key: "ArrowRight" });
    expect(onWidth).toHaveBeenLastCalledWith(296);
    fireEvent.keyDown(handle, { key: "ArrowLeft" });
    expect(onWidth).toHaveBeenLastCalledWith(264);
    // 越界夹取到 min
    const { container: atMin } = render(
      <ResizeHandle {...props} width={NAV_W_MIN} onWidth={onWidth} onReset={() => {}} />,
    );
    fireEvent.keyDown(atMin.querySelector(".rb-resize-handle")!, { key: "ArrowLeft" });
    expect(onWidth).toHaveBeenLastCalledWith(NAV_W_MIN);
    // 其他键不触发
    onWidth.mockClear();
    fireEvent.keyDown(handle, { key: "Enter" });
    expect(onWidth).not.toHaveBeenCalled();
  });

  it("右栏分隔条方向相反（向左拖变宽）", () => {
    const onWidth = vi.fn();
    const { container } = render(
      <ResizeHandle {...props} side="right" width={328} offset={328} onWidth={onWidth} onReset={() => {}} />,
    );
    const handle = container.querySelector(".rb-resize-handle") as HTMLElement;
    expect(handle.style.right).toBe("328px");
    fireEvent.keyDown(handle, { key: "ArrowLeft" });
    expect(onWidth).toHaveBeenLastCalledWith(344);
  });

  it("双击分隔条复位该侧默认宽", () => {
    const onReset = vi.fn();
    const { container } = render(
      <ResizeHandle {...props} width={420} onWidth={() => {}} onReset={onReset} />,
    );
    fireEvent.doubleClick(container.querySelector(".rb-resize-handle")!);
    expect(onReset).toHaveBeenCalledTimes(1);
  });

  it("pointer 拖动期间挂 ws-resizing（关过渡），松手摘掉", () => {
    const onWidth = vi.fn();
    const { container } = render(<ResizeHandle {...props} onWidth={onWidth} onReset={() => {}} />);
    const handle = container.querySelector(".rb-resize-handle") as HTMLElement;
    // happy-dom 无 setPointerCapture/releasePointerCapture 的真实实现：显式补齐，避免拖动路径被跳过
    (handle as any).setPointerCapture = () => {};
    (handle as any).releasePointerCapture = () => {};

    fireEvent.pointerDown(handle, { clientX: 300, button: 0, pointerId: 1 });
    expect(document.documentElement.classList.contains("ws-resizing")).toBe(true);
    fireEvent.pointerMove(handle, { clientX: 340, pointerId: 1 });
    expect(onWidth).toHaveBeenLastCalledWith(320);
    fireEvent.pointerUp(handle, { pointerId: 1 });
    expect(document.documentElement.classList.contains("ws-resizing")).toBe(false);
  });

  it("指针被系统夺走（onLostPointerCapture）也能摘掉 ws-resizing", () => {
    const { container } = render(<ResizeHandle {...props} onWidth={() => {}} onReset={() => {}} />);
    const handle = container.querySelector(".rb-resize-handle") as HTMLElement;
    (handle as any).setPointerCapture = () => {};
    (handle as any).releasePointerCapture = () => {};
    fireEvent.pointerDown(handle, { clientX: 300, button: 0, pointerId: 1 });
    expect(document.documentElement.classList.contains("ws-resizing")).toBe(true);
    fireEvent.lostPointerCapture(handle, { pointerId: 1 });
    expect(document.documentElement.classList.contains("ws-resizing")).toBe(false);
  });

  it("被窗口夹小时（disabled）忽略拖动与键盘，双击复位仍可用", () => {
    const onWidth = vi.fn();
    const onReset = vi.fn();
    const { container } = render(
      <ResizeHandle {...props} width={NAV_W_MIN} disabled onWidth={onWidth} onReset={onReset} />,
    );
    const handle = container.querySelector(".rb-resize-handle") as HTMLElement;
    expect(handle.getAttribute("aria-disabled")).toBe("true");
    expect(handle.classList.contains("is-disabled")).toBe(true);
    (handle as any).setPointerCapture = () => {};
    fireEvent.pointerDown(handle, { clientX: 100, button: 0, pointerId: 1 });
    fireEvent.pointerMove(handle, { clientX: 200, pointerId: 1 });
    fireEvent.keyDown(handle, { key: "ArrowRight" });
    expect(onWidth).not.toHaveBeenCalled();
    expect(document.documentElement.classList.contains("ws-resizing")).toBe(false);
    fireEvent.doubleClick(handle);
    expect(onReset).toHaveBeenCalledTimes(1);
  });
});

describe("栏宽真接线（store ↔ DOM ↔ localStorage）", () => {
  afterEach(() => {
    cleanup();
    localStorage.clear();
    useUi.setState({
      navWidth: NAV_W_DEFAULT,
      rightBarWidth: RB_W_DEFAULT,
      rightBarOpen: true,
      rbTab: "info",
    });
  });

  it("RightBar 把显示宽度写进 --rb-width（拖拽记忆值真的生效）", () => {
    useUi.setState({ rightBarWidth: 468 });
    const { container } = render(
      <AntApp>
        <RightBar />
      </AntApp>,
    );
    const bar = container.querySelector(".right-bar") as HTMLElement;
    expect(bar.style.getPropertyValue("--rb-width")).toBe("468px");
  });

  it("拖动分隔条 → 写 store 与 localStorage（不只测纯函数）", () => {
    useUi.setState({ navWidth: NAV_W_DEFAULT });
    const { container } = render(
      <ResizeHandle
        side="nav"
        width={NAV_W_DEFAULT}
        min={NAV_W_MIN}
        max={NAV_W_MAX}
        offset={NAV_W_DEFAULT}
        label="调整左栏宽度"
        onWidth={useUi.getState().setNavWidth}
        onReset={() => useUi.getState().setNavWidth(NAV_W_DEFAULT)}
      />,
    );
    const handle = container.querySelector(".rb-resize-handle") as HTMLElement;
    (handle as any).setPointerCapture = () => {};
    (handle as any).releasePointerCapture = () => {};
    fireEvent.pointerDown(handle, { clientX: 100, button: 0, pointerId: 1 });
    fireEvent.pointerMove(handle, { clientX: 140, pointerId: 1 });
    fireEvent.pointerUp(handle, { pointerId: 1 });
    expect(useUi.getState().navWidth).toBe(NAV_W_DEFAULT + 40);
    expect(localStorage.getItem("ws_nav_width")).toBe(String(NAV_W_DEFAULT + 40));
  });
});
