// render_html 大弹框预览（[docs/html-preview-modal](../../../docs/html-preview-modal.md)）：
// 卡片内不再内嵌渲染 → 头部「预览」入口 → 90vw × 85vh 弹框（复制源码 / 重新加载 / 浅深底色）；
// 无 html（历史占位）时入口不出现、只给一行提示。i18n 文案全部走 t()，故先初始化 i18next。
import { describe, it, expect, vi, afterEach, beforeEach } from "vitest";
import { render, cleanup, fireEvent, waitFor } from "@testing-library/react";
import { App as AntApp } from "antd";
import "../i18n";
import ToolCallCard from "../features/tools/ToolCallCard";
import type { ToolView } from "../stores/run";

vi.mock("@tauri-apps/api/core", () => ({
  invoke: vi.fn(async () => null),
  Channel: class {
    onmessage: ((f: any) => void) | null = null;
  },
}));

const HTML = "<b>hello</b><script>1</script>";
const writeText = vi.fn(async () => undefined);

function widgetCard(data: unknown): ToolView {
  return {
    callKey: "b1:0",
    tool: "render_html",
    status: "ok",
    progressTail: "",
    outcome: { ok: true, data } as any,
  };
}

function renderCard(data: unknown) {
  return render(
    <AntApp>
      <ToolCallCard tool={widgetCard(data)} />
    </AntApp>,
  );
}

/** 卡片头部的预览入口（render_html 卡独有；其他工具卡头部没有按钮）。 */
function headButton(): HTMLButtonElement | null {
  return document.querySelector<HTMLButtonElement>(".tool-card .tool-head button");
}

function modalEl(): HTMLElement | null {
  return document.querySelector<HTMLElement>(".ant-modal");
}

function frame(): HTMLIFrameElement | null {
  return document.querySelector<HTMLIFrameElement>(".widget-preview-frame");
}

/** 弹框工具栏按钮按文本查找（antd 两字按钮会插空格，故先去掉所有空白再 includes）。 */
function modalButton(text: string): HTMLButtonElement {
  const btn = Array.from(document.querySelectorAll<HTMLButtonElement>(".ant-modal button")).find((b) =>
    (b.textContent ?? "").replace(/\s/g, "").includes(text),
  );
  if (!btn) throw new Error(`弹框按钮未找到：${text}`);
  return btn;
}

/** Segmented 选项按文本点击（与 settings.mcp.test.tsx 同法：点 .ant-segmented-item-label）。 */
function segmentedItem(text: string): HTMLElement {
  const el = Array.from(document.querySelectorAll<HTMLElement>(".ant-segmented-item-label")).find((x) =>
    (x.textContent ?? "").replace(/\s/g, "").includes(text),
  );
  if (!el) throw new Error(`Segmented 选项未找到：${text}`);
  return el;
}

async function openModal() {
  const btn = headButton();
  if (!btn) throw new Error("预览入口按钮未渲染");
  fireEvent.click(btn);
  await waitFor(() => expect(modalEl()).toBeTruthy());
}

/** 关闭弹框：happy-dom 不执行 CSS 动画，rc-motion 的 leave 要反复派发 motion 结束事件才会真正卸载
 *  （destroyOnHidden 生效的时机；做法同 composer.per-tab.test.tsx）。 */
async function closeModal() {
  fireEvent.click(document.querySelector(".ant-modal-close")!);
  await waitFor(() => {
    document.querySelectorAll(".ant-modal, .ant-modal-wrap, .ant-modal-mask").forEach((el) => {
      fireEvent.transitionEnd(el);
      fireEvent.animationEnd(el);
    });
    expect(frame()).toBeNull();
  });
}

beforeEach(() => {
  writeText.mockClear();
  Object.defineProperty(navigator, "clipboard", { value: { writeText }, configurable: true });
});

afterEach(() => {
  cleanup();
});

describe("render_html 大弹框预览", () => {
  it("卡片内不再内嵌渲染；头部入口打开弹框，iframe 用出参 html 与沙箱", async () => {
    renderCard({ title: "Demo", html: HTML, chars: 23 });
    expect(frame()).toBeNull(); // 卡片里没有 iframe（旧的 150px 内嵌预览已移除）
    await openModal();
    const f = frame()!;
    expect(f.getAttribute("srcdoc")).toBe(HTML);
    expect(f.getAttribute("sandbox")).toBe("allow-scripts");
    expect(document.body.textContent).toContain("Demo"); // 弹框标题取出参 title
    expect(document.body.textContent).toContain("23 字符");
  });

  it("点入口不会顺带展开卡片（stopPropagation）", async () => {
    renderCard({ title: "Demo", html: HTML });
    await openModal();
    expect(document.querySelector(".tool-card .tool-body")).toBeNull();
  });

  it("无 html（历史占位 {restored:true}）→ 入口不渲染，给一行提示", () => {
    renderCard({ restored: true });
    expect(headButton()).toBeNull();
    expect(document.querySelector(".tool-card")!.textContent).toContain("历史未保留预览内容");
  });

  it("复制源码：写入原文（含 script）并给「已复制」反馈", async () => {
    renderCard({ title: "Demo", html: HTML });
    await openModal();
    fireEvent.click(modalButton("复制源码"));
    await waitFor(() => expect(writeText).toHaveBeenCalledWith(HTML));
    await waitFor(() => expect(modalButton("已复制")).toBeTruthy());
  });

  it("重新加载：iframe 重挂（DOM 节点换新）且 srcDoc 不变", async () => {
    renderCard({ title: "Demo", html: HTML });
    await openModal();
    const first = frame()!;
    fireEvent.click(modalButton("重新加载"));
    await waitFor(() => expect(frame()).not.toBe(first));
    expect(frame()!.getAttribute("srcdoc")).toBe(HTML);
  });

  it("底色切换：默认浅色，切「深色底」→ color-scheme 变 dark，可切回", async () => {
    renderCard({ title: "Demo", html: HTML });
    await openModal();
    expect(frame()!.style.colorScheme).toBe("light");
    fireEvent.click(segmentedItem("深色底"));
    await waitFor(() => expect(frame()!.style.colorScheme).toBe("dark"));
    fireEvent.click(segmentedItem("浅色底"));
    await waitFor(() => expect(frame()!.style.colorScheme).toBe("light"));
  });

  it("关闭弹框 → iframe 卸载（后台脚本与动画随之停止）", async () => {
    renderCard({ title: "Demo", html: HTML });
    await openModal();
    await closeModal();
  });

  it("关闭后重新打开：底色回到默认、复制反馈复位（不记忆上一次的选择）", async () => {
    renderCard({ title: "Demo", html: HTML });
    await openModal();
    fireEvent.click(segmentedItem("深色底"));
    await waitFor(() => expect(frame()!.style.colorScheme).toBe("dark"));
    fireEvent.click(modalButton("复制源码"));
    await waitFor(() => expect(modalButton("已复制")).toBeTruthy());
    await closeModal();
    await openModal();
    expect(frame()!.style.colorScheme).toBe("light"); // 默认跟随亮色主题
    expect(modalButton("复制源码")).toBeTruthy(); // 复制态已复位
  });
});
