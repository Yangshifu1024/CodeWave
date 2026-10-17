// 界面字体设置（[docs/chat-and-interface](../../../docs/chat-and-interface.md)）：
// 提交不丢（回车 / 失焦 / 停手 / 卸载四处）+ 后端真源落盘 + 输入法组合态保护 + 未编辑不写存储。
import { describe, it, expect, vi, beforeEach, afterEach } from "vitest";
import { render, screen, fireEvent, cleanup, waitFor } from "@testing-library/react";
import { App as AntApp } from "antd";
import "../i18n"; // 独立挂载必须显式初始化 i18n
import { invoke } from "@tauri-apps/api/core";
import { useSettings } from "../stores/settings";
import { useUi } from "../stores/ui";
import { configSnapshot, configSaved } from "./configFixtures";
import { AppearanceSettings } from "../features/panels/FontSettings";
import type { FontSlot } from "../utils/fonts";

const calls: Array<[string, any]> = [];

async function baseInvoke(cmd: string, args?: any) {
  calls.push([cmd, args]);
  switch (cmd) {
    case "set_font_prefs": {
      const current = useSettings.getState().config!;
      return configSaved({ ...current, ui: { ...current.ui,
        font_sans: args.sans, font_content: args.content, font_mono: args.mono,
        font_size_sans: args.fontSizeSans, font_size_content: args.fontSizeContent, font_size_mono: args.fontSizeMono,
      } });
    }
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
  vi.mocked(invoke).mockImplementation(baseInvoke);
  const snapshot = configSnapshot({ providers: [], ui: {
    font_sans: "", font_content: "", font_mono: "",
    font_size_sans: 14, font_size_content: 14, font_size_mono: 12, language: "zh-CN",
  } });
  useSettings.setState({ snapshot, config: snapshot.config, loaded: true });
  document.documentElement.style.removeProperty("--ws-font-sans");
  document.documentElement.style.removeProperty("--ws-font-content");
  document.documentElement.style.removeProperty("--ws-font-mono");
  document.documentElement.style.removeProperty("--ws-font-size-sans");
  document.documentElement.style.removeProperty("--ws-font-size-content");
  document.documentElement.style.removeProperty("--ws-font-size-mono");
});

afterEach(() => {
  cleanup();
  vi.restoreAllMocks();
});

function mount() {
  return render(
    <AntApp>
      <AppearanceSettings />
    </AntApp>,
  );
}

/** 按字体槽定位输入框：走设置项锚点（ui.font_<slot>），不依赖行标签文字。
 *  字体与字号已合并成三行、行标签简化为「界面 / 内容 / 代码」，按 Form.Item label 找已不成立。 */
const FAMILY_SLOT_BY_LABEL: Record<string, FontSlot> = {
  界面字体: "sans", 内容字体: "content", 代码字体: "mono",
};

function fontInput(label: string): HTMLInputElement {
  const slot = FAMILY_SLOT_BY_LABEL[label];
  const el = (slot
    ? document.querySelector<HTMLInputElement>(`[data-setting-id="ui.font_${slot}"] input`)
    : null) as HTMLInputElement | null;
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
    const el = fontInput("代码字体");
    fireEvent.change(el, { target: { value: "JetBrains Mono" } });
    fireEvent.keyDown(el, { key: "Enter", code: "Enter" });

    expect(localStorage.getItem("ws_font_mono")).toBe("JetBrains Mono");
  });

  it("输入法组合期间不提交，组合结束才落盘（半成品不写库）", async () => {
    mount();
    const el = fontInput("代码字体");
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

  it("行末「恢复默认」一次清掉本槽字体族与字号，且只提交一次", async () => {
    localStorage.setItem("ws_font_sans", "Inter");
    localStorage.setItem("ws_font_size_sans", "17");
    mount();
    expect(fontInput("界面字体").value).toBe("Inter");

    fireEvent.click(screen.getByLabelText("恢复默认・界面字体 + 界面字号"));
    await waitFor(() => expect(localStorage.getItem("ws_font_sans")).toBeNull());
    // 一次点击 = 一次落盘（两个 commitFont* 串行后只提交最终结果）
    await waitFor(() => expect(fontPrefCalls()).toHaveLength(1));
    expect(fontPrefCalls()[0]).toMatchObject({ sans: "", fontSizeSans: 14 });
    await waitFor(() => expect(useSettings.getState().config?.ui.font_sans).toBe(""));
  });
  it("authoritative font receipt updates saved state", async () => {
    mount();
    const input = fontInput("界面字体");
    fireEvent.change(input, { target: { value: "Saved Serif" } });
    fireEvent.keyDown(input, { key: "Enter", code: "Enter" });
    await waitFor(() => expect(useSettings.getState().config?.ui.font_sans).toBe("Saved Serif"));
    expect(useSettings.getState().snapshot?.version.sequence).toBe(1);
  });

  it.each([
    ["sans", "界面字体", "font_sans"],
    ["content", "内容字体", "font_content"],
    ["mono", "代码字体", "font_mono"],
  ] as const)("%s persistence failure retains the preview and retries the unchanged value", async (slot, label, field) => {
    const notification = vi.spyOn(useUi.getState(), "notify");
    let fail = true;
    vi.mocked(invoke).mockImplementation(async (cmd, args) => {
      if (cmd === "set_font_prefs" && fail) {
        calls.push([cmd, args]);
        throw { code: "CredentialWriteFailed", message: "Unlock the system keyring to save" };
      }
      return baseInvoke(cmd, args);
    });
    mount();
    const input = fontInput(label);
    fireEvent.change(input, { target: { value: "Retry Serif" } });
    fireEvent.keyDown(input, { key: "Enter", code: "Enter" });
    await screen.findByRole("button", { name: `重试・${label}` });
    expect(useSettings.getState().config?.ui[field]).toBe("");
    expect(localStorage.getItem(`ws_font_${slot}`)).toBe("Retry Serif");
    expect(notification).toHaveBeenCalledWith(expect.any(String), expect.stringContaining("Unlock the system keyring"));
    const firstOperation = fontPrefCalls()[0].operationId;
    fail = false;
    fireEvent.click(screen.getByRole("button", { name: `重试・${label}` }));
    await waitFor(() => expect(useSettings.getState().config?.ui[field]).toBe("Retry Serif"));
    expect(fontPrefCalls()).toHaveLength(2);
    expect(fontPrefCalls()[1].operationId).not.toBe(firstOperation);
    await waitFor(() => expect(screen.queryByRole("button", { name: `重试・${label}` })).toBeNull());
  });

  it("content edits persist all six preferences and preserve the other font groups", async () => {
    localStorage.setItem("ws_font_sans", "Inter");
    localStorage.setItem("ws_font_mono", "Menlo");
    localStorage.setItem("ws_font_size_sans", "16");
    localStorage.setItem("ws_font_size_content", "18");
    localStorage.setItem("ws_font_size_mono", "12");
    mount();
    const input = fontInput("内容字体");
    fireEvent.change(input, { target: { value: "Source Han Serif" } });
    fireEvent.blur(input);
    await waitFor(() => expect(fontPrefCalls()).toHaveLength(1));
    expect(fontPrefCalls()[0]).toMatchObject({
      sans: "Inter", content: "Source Han Serif", mono: "Menlo",
      fontSizeSans: 16, fontSizeContent: 18, fontSizeMono: 12,
    });
    await waitFor(() => expect(useSettings.getState().config?.ui.font_content).toBe("Source Han Serif"));
    expect(fontInput("界面字体").value).toBe("Inter");
    expect(fontInput("代码字体").value).toBe("Menlo");
    const preview = input.closest(".settings-font-pair")!.querySelector<HTMLElement>(".font-preview-row")!;
    expect(preview.style.fontFamily).toBe('"Source Han Serif", var(--ws-font-content-fallback)');
    expect(preview.style.fontSize).toBe("18px");
  });

  it("restoring the content slot preserves interface/code families", async () => {
    localStorage.setItem("ws_font_sans", "Inter");
    localStorage.setItem("ws_font_content", "Georgia");
    localStorage.setItem("ws_font_mono", "Menlo");
    mount();
    // 行末重置一次清内容槽的字体族与字号；另外两槽不动
    fireEvent.click(screen.getByLabelText("恢复默认・内容字体 + 内容字号"));
    await waitFor(() => expect(fontPrefCalls()).toHaveLength(1));
    expect(fontPrefCalls()[0]).toMatchObject({
      sans: "Inter", content: "", mono: "Menlo",
      fontSizeSans: 14, fontSizeContent: 14, fontSizeMono: 12,
    });
    expect(localStorage.getItem("ws_font_content")).toBeNull();
  });
});
