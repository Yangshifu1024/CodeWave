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

function renderCard(data: unknown, props?: { autoOpenCallKey?: string | null; onConsumeAutoOpen?: (k: string) => void }) {
  return render(
    <AntApp>
      <ToolCallCard tool={widgetCard(data)} {...props} />
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

// ---------- [docs/preview-skill](../../../../docs/preview-skill.md)：批准门「先看预览」→ 本 run 第一张
// 合格 widget 卡片自动弹框（不必回聊天区点那个很小的头部「预览」按钮）。组件级、零 store：
// store 侧只负责把一次性信号转成 callKey，这里只验「命中即弹 + 回抛消费」，不重复验 store。

describe("render_html 自动弹预览（autoOpenCallKey）", () => {
  it("信号命中本卡 + 有 html → 无需点头部按钮即出现弹框，且回抛消费（参数为本卡 callKey）", async () => {
    const onConsume = vi.fn();
    renderCard({ title: "Demo", html: HTML, chars: 23 }, { autoOpenCallKey: "b1:0", onConsumeAutoOpen: onConsume });
    await waitFor(() => expect(modalEl()).toBeTruthy());
    expect(frame()!.getAttribute("srcdoc")).toBe(HTML);
    expect(document.body.textContent).toContain("Demo");
    // 消费只发生一次，且带上本卡 callKey（上层据此清掉 store 里的一次性信号）
    await waitFor(() => expect(onConsume).toHaveBeenCalledTimes(1));
    expect(onConsume).toHaveBeenCalledWith("b1:0");
  });

  it("autoOpenCallKey 不匹配本卡 → 不自动弹框（仍需点头部入口；消费也不发生）", async () => {
    const onConsume = vi.fn();
    renderCard({ title: "Demo", html: HTML }, { autoOpenCallKey: "b9:9", onConsumeAutoOpen: onConsume });
    expect(modalEl()).toBeNull();
    expect(onConsume).not.toHaveBeenCalled();
    // 手动入口仍在（未被自动通道带坏）
    await openModal();
  });

  it("信号命中但无 html（{restored:true} 历史占位）→ 不弹、不消费（消费留给有 html 的那张卡）", () => {
    const onConsume = vi.fn();
    renderCard({ restored: true }, { autoOpenCallKey: "b1:0", onConsumeAutoOpen: onConsume });
    expect(modalEl()).toBeNull();
    expect(onConsume).not.toHaveBeenCalled();
  });

  it("弹框开关是卡片本地 state：信号消费置 null 后重渲染不再次弹出，消费回调仍只有一次", async () => {
    const onConsume = vi.fn();
    const data = { title: "Demo", html: HTML };
    const { rerender } = render(
      <AntApp>
        <ToolCallCard tool={widgetCard(data)} autoOpenCallKey="b1:0" onConsumeAutoOpen={onConsume} />
      </AntApp>,
    );
    await waitFor(() => expect(modalEl()).toBeTruthy());
    expect(onConsume).toHaveBeenCalledTimes(1);
    // 上层消费后信号变 null（store 已清），重渲染：弹框状态留在本卡，不被信号再次拉起
    await closeModal();
    rerender(
      <AntApp>
        <ToolCallCard tool={widgetCard(data)} autoOpenCallKey={null} onConsumeAutoOpen={onConsume} />
      </AntApp>,
    );
    expect(modalEl()).toBeNull();
    expect(onConsume).toHaveBeenCalledTimes(1);
  });

  it("信号先命中但出参未到（无 html）→ 不消费；随后 html 落地（同一 callKey）重渲染才弹并消费", async () => {
    const onConsume = vi.fn();
    const { rerender } = render(
      <AntApp>
        <ToolCallCard tool={widgetCard({})} autoOpenCallKey="b1:0" onConsumeAutoOpen={onConsume} />
      </AntApp>,
    );
    // 运行中：出参还没到 → 不弹、不消费（否则信号被空卡吃掉，真 widget 就再也不会自动弹）
    expect(modalEl()).toBeNull();
    expect(onConsume).not.toHaveBeenCalled();
    rerender(
      <AntApp>
        <ToolCallCard tool={widgetCard({ title: "Demo", html: HTML })} autoOpenCallKey="b1:0" onConsumeAutoOpen={onConsume} />
      </AntApp>,
    );
    await waitFor(() => expect(modalEl()).toBeTruthy());
    expect(onConsume).toHaveBeenCalledWith("b1:0");
  });
});
