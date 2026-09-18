// 启动静默检查 + 自动更新偏好（utils/updateCheck 的 useStartupUpdateCheck / useAutoUpdateSetting）：
//   · 启动 3s 后静默检查一次（且仅一次）；
//   · ws_auto_update === "false" 时不检查；
//   · StrictMode 的 mount→unmount→remount 后仍会检查（标志位在 timer 回调内翻转的意义所在）。
// 用假定时器推进，避免真等 3 秒。
import { describe, it, expect, vi, beforeEach, afterEach } from "vitest";
import { render, cleanup } from "@testing-library/react";
import { StrictMode } from "react";
import { useAutoUpdateSetting, useStartupUpdateCheck, AUTO_UPDATE_KEY } from "../utils/updateCheck";
import { useUpdater } from "../stores/updater";

const mocks = vi.hoisted(() => ({
  check: vi.fn(async (): Promise<unknown> => null),
  resolveProxy: vi.fn(async (): Promise<string | null> => null),
  appVersion: vi.fn(async (): Promise<string> => "1.0.0"),
  isAppimage: vi.fn(async (): Promise<boolean> => true),
}));

vi.mock("@tauri-apps/plugin-updater", () => ({ check: mocks.check }));
vi.mock("../ipc/client", async (importOriginal) => {
  const actual = await importOriginal<typeof import("../ipc/client")>();
  return {
    ...actual,
    ipc: {
      ...actual.ipc,
      resolveProxy: mocks.resolveProxy,
      appVersion: mocks.appVersion,
      isAppimage: mocks.isAppimage,
    },
  };
});

/** 只挂 hook 的宿主组件（hook 是「调用一次」语义，不需要渲染任何东西） */
function Host() {
  useStartupUpdateCheck();
  return null;
}

/** 推进假定时器并让 microtask 队列跑完（静默检查内部有多层 await） */
async function advance(ms: number) {
  await vi.advanceTimersByTimeAsync(ms);
}

beforeEach(() => {
  vi.useFakeTimers();
  mocks.check.mockClear();
  useUpdater.setState({ phase: "idle", modalOpen: false, checkEpoch: 0 });
  localStorage.removeItem(AUTO_UPDATE_KEY);
});

afterEach(() => {
  cleanup();
  vi.useRealTimers();
  localStorage.removeItem(AUTO_UPDATE_KEY);
});

describe("useStartupUpdateCheck", () => {
  it("启动 3s 后静默检查一次（提前不检查，之后不重复）", async () => {
    render(<Host />);

    await advance(2_000);
    expect(mocks.check).not.toHaveBeenCalled();

    await advance(1_500);
    expect(mocks.check).toHaveBeenCalledTimes(1);

    // 静默路径：无更新时不改相位、不弹窗、不产生通知
    const st = useUpdater.getState();
    expect(st.phase).toBe("idle");
    expect(st.modalOpen).toBe(false);

    await advance(10_000);
    expect(mocks.check).toHaveBeenCalledTimes(1);
  });

  it("ws_auto_update === \"false\" 时不检查", async () => {
    localStorage.setItem(AUTO_UPDATE_KEY, "false");
    render(<Host />);

    await advance(10_000);

    expect(mocks.check).not.toHaveBeenCalled();
  });

  it("StrictMode 重挂载后仍会检查（标志位在 timer 回调内翻转）", async () => {
    // StrictMode 会 mount→unmount→remount：首个 timer 被清理，若标志位在 effect 体内翻转，
    // 真正的第二次挂载就会跳过检查——本用例正是守护这一点。
    render(
      <StrictMode>
        <Host />
      </StrictMode>,
    );

    await advance(3_500);

    expect(mocks.check).toHaveBeenCalledTimes(1);
  });

  it("静默检查发现新版本：弹窗打开（但无 toast 打扰）", async () => {
    mocks.check.mockResolvedValueOnce({
      currentVersion: "1.0.0",
      version: "9.9.9",
      body: "## 新特性",
      downloadAndInstall: vi.fn(async () => {}),
      close: vi.fn(async () => {}),
    });

    render(<Host />);
    await advance(3_500);

    const st = useUpdater.getState();
    expect(st.phase).toBe("available");
    expect(st.modalOpen).toBe(true);
    expect(st.notes).toBe("## 新特性");
  });
});

describe("useAutoUpdateSetting", () => {
  /** 把 [值, 写入函数] 暴露到外部，供断言 */
  let captured: [boolean, (v: boolean) => void] | null = null;

  function SettingHost() {
    captured = useAutoUpdateSetting();
    return null;
  }

  it("默认开启；写入 false 后落盘并可读回", () => {
    render(<SettingHost />);
    expect(captured![0]).toBe(true);

    captured![1](false);
    expect(localStorage.getItem(AUTO_UPDATE_KEY)).toBe("false");

    cleanup();
    render(<SettingHost />);
    expect(captured![0]).toBe(false);
  });

  it("非 \"false\" 的脏值一律视为开启", () => {
    localStorage.setItem(AUTO_UPDATE_KEY, "yes");
    render(<SettingHost />);
    expect(captured![0]).toBe(true);
  });
});
