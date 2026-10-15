// ipc/clipboard.ts 的宿主环境守卫测试。
//
// 单独成文件：clipboard.util.test.ts 顶层 vi.mock 了 ../ipc/clipboard，
// 本组要测的正是**真函数**，两者互斥，必须独立文件。
//
// 这是本次唯一新增的「宿主环境判断」依赖：守卫写错会让 vite dev / 单测环境每次点击
// 都冒一条报错，或在真 Tauri 里静默不走原生路径（缺陷复发且无痕）。
import { describe, it, expect, vi, afterEach } from "vitest";

const pluginWriteText = vi.fn(async () => {});
vi.mock("@tauri-apps/plugin-clipboard-manager", () => ({ writeText: pluginWriteText }));

// Tauri 内部注入标记（window 上的 index 签名）：用 unknown 中转避开 Window 联合类型直接断言的报错。
type TauriWindow = Window & typeof globalThis & { __TAURI_INTERNALS__?: unknown };

function setTauri(present: boolean) {
  const w = window as unknown as TauriWindow;
  if (present) w.__TAURI_INTERNALS__ = {};
  else delete w.__TAURI_INTERNALS__;
}

afterEach(() => {
  setTauri(false);
  pluginWriteText.mockReset();
  pluginWriteText.mockImplementation(async () => {});
  vi.restoreAllMocks();
});

describe("writeClipboardText 环境守卫", () => {
  it("非 Tauri 环境（无 __TAURI_INTERNALS__）返回 false，不调插件", async () => {
    setTauri(false);
    const { writeClipboardText } = await import("../ipc/clipboard");

    await expect(writeClipboardText("内容")).resolves.toBe(false);
    expect(pluginWriteText).not.toHaveBeenCalled();
  });

  it("Tauri 环境下写入成功返回 true，且透传原文", async () => {
    setTauri(true);
    const { writeClipboardText } = await import("../ipc/clipboard");

    await expect(writeClipboardText("原样透传")).resolves.toBe(true);
    expect(pluginWriteText).toHaveBeenCalledWith("原样透传");
  });

  it("Tauri 环境下写入失败（如 ACL 未授予）返回 false 且不抛给调用方", async () => {
    setTauri(true);
    pluginWriteText.mockRejectedValueOnce(new Error("not allowed by ACL"));
    const warn = vi.spyOn(console, "warn").mockImplementation(() => {});
    const { writeClipboardText } = await import("../ipc/clipboard");

    // 关键：失败必须收敛成 false，绝不能冒泡成 React 事件回调里的未处理异常
    await expect(writeClipboardText("内容")).resolves.toBe(false);
    expect(warn).toHaveBeenCalled();
  });
});
