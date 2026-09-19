// 界面字体设置（[docs/custom-font-and-titlebar](../../../docs/custom-font-and-titlebar.md)）：
// 提交不丢（回车 / 失焦 / 停手 / 卸载四处）+ 后端真源落盘 + 输入法组合态保护 + 未编辑不写存储。
import { describe, it, expect, vi, beforeEach, afterEach } from "vitest";
import { render, screen, fireEvent, cleanup, waitFor } from "@testing-library/react";
import { App as AntApp } from "antd";
import "../i18n"; // 独立挂载必须显式初始化 i18n
import { AppearanceSettings } from "../features/panels/FontSettings";

const calls: Array<[string, any]> = [];

async function baseInvoke(cmd: string, args?: any) {
  calls.push([cmd, args]);
  switch (cmd) {
    case "set_font_prefs":
      return null;
    default:
      throw new Error(`unmocked command: ${cmd}`);
  }
}

vi.mock("@tauri-apps/api/core", () => ({
  invoke: vi.fn(baseInvoke),
  Channel: class {
    onmessage: any = null;
  },
}));

beforeEach(() => {
  localStorage.clear();
  calls.length = 0;
  document.documentElement.style.removeProperty("--ws-font-sans");
  document.documentElement.style.removeProperty("--ws-font-mono");
});

afterEach(() => {
  cleanup();
});

function mount() {
  return render(
    <AntApp>
      <AppearanceSettings />
    </AntApp>,
  );
}

/** 按 Form.Item 的 label 文本找字体输入框 */
function fontInput(label: string): HTMLInputElement {
  const item = Array.from(document.querySelectorAll(".ant-form-item")).find((el) =>
    (el.querySelector(".ant-form-item-label")?.textContent ?? "").includes(label),
  );
  const el = item?.querySelector("input") as HTMLInputElement | null;
  if (!el) throw new Error(`找不到字体输入框：${label}`);
  return el;
}

function fontPrefCalls(): any[] {
  return calls.filter((c) => c[0] === "set_font_prefs").map((c) => c[1]);
}

describe("界面字体：提交不丢与后端落盘", () => {
  it("输入后不按回车、停手 600ms 也会落盘（并写后端真源）", async () => {
    mount();
    fireEvent.change(fontInput("界面字体"), { target: { value: "PingFang SC" } });

    await waitFor(() => expect(localStorage.getItem("ws_font_sans")).toBe("PingFang SC"), {
      timeout: 2000,
    });
    await waitFor(() => expect(fontPrefCalls().some((a) => a?.sans === "PingFang SC")).toBe(true));
    expect(document.documentElement.style.getPropertyValue("--ws-font-sans")).toBe(
      '"PingFang SC", var(--ws-font-sans-fallback)',
    );
  });

  it("输入后立刻切走（组件卸载）也会提交——这是原来丢设置的主因", async () => {
    const view = mount();
    fireEvent.change(fontInput("界面字体"), { target: { value: "Inter" } });
    view.unmount();

    expect(localStorage.getItem("ws_font_sans")).toBe("Inter");
    await waitFor(() => expect(fontPrefCalls().some((a) => a?.sans === "Inter")).toBe(true));
  });

  it("回车立即提交（原行为不回归）", async () => {
    mount();
    const el = fontInput("等宽字体");
    fireEvent.change(el, { target: { value: "JetBrains Mono" } });
    fireEvent.keyDown(el, { key: "Enter", code: "Enter" });

    expect(localStorage.getItem("ws_font_mono")).toBe("JetBrains Mono");
  });

  it("输入法组合期间不提交，组合结束才落盘（半成品不写库）", async () => {
    mount();
    const el = fontInput("等宽字体");
    fireEvent.compositionStart(el);
    fireEvent.change(el, { target: { value: "上海宋" } });
    fireEvent.keyDown(el, { key: "Enter", code: "Enter" });
    // 组合中：回车不得落盘
    expect(localStorage.getItem("ws_font_mono")).toBeNull();

    fireEvent.compositionEnd(el);
    await waitFor(() => expect(localStorage.getItem("ws_font_mono")).toBe("上海宋"), {
      timeout: 2000,
    });
  });

  it("输入后失焦也提交（最基本的路径）", async () => {
    mount();
    const el = fontInput("界面字体");
    fireEvent.change(el, { target: { value: " Source Han Sans " } });
    fireEvent.blur(el);

    // 净化后写入（首尾空白被去），并同步后端真源
    expect(localStorage.getItem("ws_font_sans")).toBe("Source Han Sans");
    expect(el.value).toBe("Source Han Sans"); // 失焦回填：显示值与存储对齐
    await waitFor(() => expect(fontPrefCalls().some((a) => a?.sans === "Source Han Sans")).toBe(true));
  });

  it("输入法组合中失焦：按框内当前值提交（不让内容卡在组合态里丢掉）", async () => {
    mount();
    const el = fontInput("界面字体");
    fireEvent.compositionStart(el);
    fireEvent.change(el, { target: { value: "Shang Hai" } });
    // compositionend 未到时失焦：先解除组合态再提交，内容不得丢
    fireEvent.blur(el);
    expect(localStorage.getItem("ws_font_sans")).toBe("Shang Hai");
  });

  it("没有编辑过就不动存储（未编辑的空提交不得抹掉已存偏好）", async () => {
    localStorage.setItem("ws_font_mono", "Consolas");
    mount();
    // 挂载后再塞一个值：此时组件内草稿仍是挂载时的空串，只有 edited 守卫能拦住「拿空串覆盖」
    localStorage.setItem("ws_font_sans", "Inter");

    fireEvent.blur(fontInput("界面字体"));
    expect(localStorage.getItem("ws_font_sans")).toBe("Inter");
    expect(localStorage.getItem("ws_font_mono")).toBe("Consolas");
    expect(fontPrefCalls()).toHaveLength(0);
  });

  it("「恢复默认」清空该槽并把空值同步到后端", async () => {
    localStorage.setItem("ws_font_sans", "Inter");
    mount();
    expect(fontInput("界面字体").value).toBe("Inter");

    fireEvent.click(screen.getByLabelText(/恢复默认・界面字体/));
    await waitFor(() => expect(localStorage.getItem("ws_font_sans")).toBeNull());
    await waitFor(() => expect(fontPrefCalls().some((a) => a?.sans === "")).toBe(true));
  });
});
