// 更新流程（utils/updateCheck）的失败原因识别与状态机写入：
//   · GitHub 匿名 API 配额耗尽（403 / 429 / rate limit，大小写不敏感）→ 专用可行动指引文案；
//   · 其余错误 → 通用 notice.updateFailed（带原始错误串）；
//   · 无更新 → markUpToDate（不弹窗）；有更新 → markAvailable + 弹窗打开；静默检查全程不可见。
// （latest.json 若把下载地址指向 GitHub REST 资产 API 端点，匿名配额 60 次/小时/出口 IP 耗尽即 403；
// 403 是穿透代理拿回的 HTTP 状态码，换代理出口 IP 无效 —— 故文案明确写「不要改代理设置」。）
import { describe, it, expect, vi, beforeEach, beforeAll } from "vitest";
import { i18n } from "../i18n";
import { useUi } from "../stores/ui";
import { useUpdater } from "../stores/updater";
import { checkForUpdate, checkForUpdates, startUpdate, retryUpdate } from "../utils/updateCheck";

// vi.mock 工厂被提升到所有 import 之上：mock 句柄必须先用 vi.hoisted 构造，否则报「Cannot access before initialization」
const mocks = vi.hoisted(() => ({
  check: vi.fn<(opts?: { timeout?: number; proxy?: string }) => Promise<unknown>>(),
  resolveProxy: vi.fn(async (): Promise<string | null> => null),
  restartApp: vi.fn(async (): Promise<void> => {}),
  appVersion: vi.fn(async (): Promise<string> => "1.0.0"),
  isAppimage: vi.fn(async (): Promise<boolean> => true),
}));

// 更新模块走动态 import（await import("@tauri-apps/plugin-updater")），vi.mock 对动态 import 同样生效
vi.mock("@tauri-apps/plugin-updater", () => ({ check: mocks.check }));

// 只替换 resolveProxy / restartApp / appVersion / isAppimage，其余 IPC 成员保留原样（stores/ui → utils/uiState 也会用到 ipc）
// 注意：specifier 相对本测试文件解析，必须与 updateCheck 中的 "../ipc/client" 指向同一文件
vi.mock("../ipc/client", async (importOriginal) => {
  const actual = await importOriginal<typeof import("../ipc/client")>();
  return {
    ...actual,
    ipc: {
      ...actual.ipc,
      resolveProxy: mocks.resolveProxy,
      restartApp: mocks.restartApp,
      appVersion: mocks.appVersion,
      isAppimage: mocks.isAppimage,
    },
  };
});

/** 假 Update：downloadAndInstall 在传入 err 时 reject；close 供流程释放实例。
 *  回调参数显式标注（否则 vi.fn 推断为零参签名，测试里的 mockImplementation(async (cb) => …) 过不了 tsc）。 */
function fakeUpdate(err?: Error) {
  return {
    currentVersion: "1.0.0",
    version: "9.9.9",
    body: "## 新特性\n- 更快的启动",
    downloadAndInstall: vi.fn<(cb: (e: unknown) => void) => Promise<void>>(async () => {
      if (err) throw err;
    }),
    close: vi.fn(async () => {}),
  };
}

/** 最近一条通知正文（显式检查的「已是最新」仍走 toast） */
function lastToast(): string {
  const list = useUi.getState().notifications;
  return list[list.length - 1]?.body ?? "";
}

beforeAll(async () => {
  // 文案断言依赖语言（happy-dom 下默认 zh-CN），显式固定以免受宿主 localStorage 影响
  await i18n.changeLanguage("zh-CN");
});

beforeEach(() => {
  mocks.check.mockReset();
  mocks.resolveProxy.mockClear();
  mocks.restartApp.mockClear();
  mocks.appVersion.mockClear();
  mocks.isAppimage.mockClear();
  mocks.resolveProxy.mockImplementation(async () => null);
  mocks.appVersion.mockImplementation(async () => "1.0.0");
  mocks.isAppimage.mockImplementation(async () => true);
  useUi.setState({ notifications: [] });
  // 每个用例从干净的状态机起跑（phase/modalOpen/epoch 都是跨用例的模块级状态）
  useUpdater.setState({
    phase: "idle",
    modalOpen: false,
    currentVersion: null,
    newVersion: null,
    notes: null,
    downloadedBytes: 0,
    totalBytes: null,
    error: null,
    checkEpoch: 0,
  });
});

describe("检查更新的失败原因识别", () => {
  it("403 Forbidden（匿名 API 配额耗尽）：给专用指引而非裸报错，不重启", async () => {
    mocks.check.mockResolvedValue(
      fakeUpdate(new Error("Download request failed with status: 403 Forbidden")),
    );

    // 检查本身成功（发现更新），403 发生在下载安装阶段
    await checkForUpdates();
    expect(useUpdater.getState().phase).toBe("available");
    await startUpdate();

    const { error } = useUpdater.getState();
    expect(error).toBe(i18n.t("notice.updateFailedRateLimited"));
    expect(error).not.toBe(
      i18n.t("notice.updateFailed", {
        error: "Error: Download request failed with status: 403 Forbidden",
      }),
    );
    // 三个信息齐备：原因（403 / 配额）+ 替代（稍后重试）+ 替代（Release 页手动下载）
    expect(error).toMatch(/403/);
    expect(error).toMatch(/配额|quota/);
    expect(error).toMatch(/稍后|later/);
    expect(error).toMatch(/Release/);
    expect(mocks.restartApp).not.toHaveBeenCalled();
  });

  it("其他错误（非配额）：保持通用 notice.updateFailed 并带上原始错误串", async () => {
    mocks.check.mockResolvedValue(fakeUpdate(new Error("boom")));

    await checkForUpdates();
    await startUpdate();

    expect(useUpdater.getState().error).toBe(
      i18n.t("notice.updateFailed", { error: "Error: boom" }),
    );
    expect(useUpdater.getState().error).not.toBe(i18n.t("notice.updateFailedRateLimited"));
    expect(mocks.restartApp).not.toHaveBeenCalled();
  });

  it.each([
    "Download request failed with status: 429 Too Many Requests",
    "API rate limit exceeded for 203.0.113.7",
    "RATE LIMIT EXCEEDED",
  ])("429 / rate limit 变体同样识别：%s", async (message) => {
    mocks.check.mockResolvedValue(fakeUpdate(new Error(message)));

    await checkForUpdates();
    await startUpdate();

    expect(useUpdater.getState().error).toBe(i18n.t("notice.updateFailedRateLimited"));
  });

  it("检查阶段本身的异常：错误弹窗展示同一套文案（不再 toast）", async () => {
    mocks.check.mockRejectedValue(new Error("Download request failed with status: 403 Forbidden"));

    await checkForUpdates();

    const st = useUpdater.getState();
    expect(st.phase).toBe("error");
    expect(st.modalOpen).toBe(true);
    expect(st.error).toBe(i18n.t("notice.updateFailedRateLimited"));
  });
});

describe("无更新与静默检查", () => {
  it("无更新（check 返回 null）：toast upToDate，不弹窗不下载不重启", async () => {
    mocks.check.mockResolvedValue(null);

    await checkForUpdates();

    // 无更新走 toast（弹窗只服务「有更新 / 失败 / 待重启」三态）
    expect(lastToast()).toBe(i18n.t("notice.upToDate"));
    expect(useUi.getState().notifications).toHaveLength(1);
    const st = useUpdater.getState();
    expect(st.phase).toBe("up-to-date");
    expect(st.modalOpen).toBe(false);
    expect(mocks.restartApp).not.toHaveBeenCalled();
  });

  it("静默检查失败：不弹窗、不报错、无 toast", async () => {
    mocks.check.mockRejectedValue(new Error("offline"));

    await checkForUpdate({ silent: true });

    const st = useUpdater.getState();
    expect(st.phase).toBe("idle");
    expect(st.modalOpen).toBe(false);
    expect(useUi.getState().notifications).toHaveLength(0);
  });

  it("静默检查发现新版本：仅弹窗，不产生 toast", async () => {
    mocks.check.mockResolvedValue(fakeUpdate());

    await checkForUpdate({ silent: true });

    expect(useUpdater.getState().phase).toBe("available");
    expect(useUpdater.getState().modalOpen).toBe(true);
    expect(useUi.getState().notifications).toHaveLength(0);
  });
});

describe("epoch 守卫与参数", () => {
  it("陈旧结果被丢弃：先发起的那次不得覆盖后发起的那次", async () => {
    // 语义：每次检查递增代次，**后发起**的那次独占 UI。
    // 本用例让陈旧的那次返回「有更新」：若守卫缺失，它会把已定的 up-to-date 改写成弹窗——
    // 这正是启动静默检查与用户手动检查重叠时的真实风险。
    let releaseStale: (v: unknown) => void = () => {};
    const staleCheck = new Promise((resolve) => {
      releaseStale = resolve;
    });
    // 第 1 次 check 调用 = 先发起的检查（挂着不返回）；第 2 次 = 后发起的（无更新）
    mocks.check.mockReturnValueOnce(staleCheck).mockResolvedValueOnce(null);

    const stale = checkForUpdates();
    // 等首个检查真正进入 check（先占住第一个 mock 槽位），再发第二次
    await new Promise((r) => setTimeout(r, 0));
    const fresh = checkForUpdates();
    await fresh;
    expect(useUpdater.getState().phase).toBe("up-to-date");
    expect(mocks.check).toHaveBeenCalledTimes(2);

    // 陈旧结果晚到且「发现更新」：不得改写成弹窗
    releaseStale(fakeUpdate());
    await stale;

    expect(useUpdater.getState().phase).toBe("up-to-date");
    expect(useUpdater.getState().modalOpen).toBe(false);
    expect(useUpdater.getState().newVersion).toBeNull();
  });
  it("无代理（resolveProxy 返回 null）时，check 只带 timeout 参数", async () => {
    mocks.check.mockResolvedValue(fakeUpdate());

    await checkForUpdates();

    expect(mocks.resolveProxy).toHaveBeenCalled();
    expect(mocks.check).toHaveBeenCalledWith({ timeout: 15_000 });
  });
});

describe("下载进度事件与重试分支", () => {
  it("Started / Progress / Finished 三步后进度到 100%", async () => {
    mocks.check.mockResolvedValue(fakeUpdate());
    await checkForUpdates();

    // 让 downloadAndInstall 按插件语义逐个回调事件（实例就是 check 返回的那个）
    const inst = await (mocks.check.mock.results[0].value as ReturnType<typeof fakeUpdate>);
    inst.downloadAndInstall.mockImplementation(async (cb: (e: unknown) => void) => {
      cb({ event: "Started", data: { contentLength: 100 } });
      cb({ event: "Progress", data: { chunkLength: 60 } });
      expect(useUpdater.getState().downloadedBytes).toBe(60);
      cb({ event: "Progress", data: { chunkLength: 40 } });
      cb({ event: "Finished" });
    });
    await startUpdate();

    const st = useUpdater.getState();
    expect(st.phase).toBe("ready");
    expect(st.downloadedBytes).toBe(100);
    expect(st.totalBytes).toBe(100);
  });

  it("总量未知：进度累计不死循环、不产生 NaN", async () => {
    mocks.check.mockResolvedValue(fakeUpdate());
    await checkForUpdates();
    const inst = await (mocks.check.mock.results[0].value as ReturnType<typeof fakeUpdate>);
    inst.downloadAndInstall.mockImplementation(async (cb: (e: unknown) => void) => {
      cb({ event: "Started", data: {} });
      cb({ event: "Progress", data: { chunkLength: 2048 } });
      cb({ event: "Finished" });
    });
    await startUpdate();

    const st = useUpdater.getState();
    expect(st.phase).toBe("ready");
    expect(st.totalBytes).toBeNull();
    expect(st.downloadedBytes).toBe(2048);
    expect(Number.isNaN(st.downloadedBytes)).toBe(false);
  });

  it("pendingUpdate 存在时 retryUpdate 直接重下（不再 check）", async () => {
    mocks.check.mockResolvedValue(fakeUpdate(new Error("boom")));
    await checkForUpdates();
    await startUpdate();
    await retryUpdate();

    expect(mocks.check).toHaveBeenCalledTimes(1);
    const inst = await (mocks.check.mock.results[0].value as Promise<ReturnType<typeof fakeUpdate>>);
    expect(inst.downloadAndInstall).toHaveBeenCalledTimes(2);
  });

  it("pendingUpdate 不存在时 retryUpdate 重新检查", async () => {
    // 另起一个模块实例：pendingUpdate / installInFlight 都是模块级槽位，    // 前面的用例已把它们填过，直接用本模块实例测不了「未检查过」这条分支
    vi.resetModules();
    const freshUpdater = (await import("../stores/updater")).useUpdater;
    freshUpdater.setState({ phase: "error", modalOpen: true, error: "boom", checkEpoch: 0 });
    mocks.check.mockResolvedValue(null);
    const freshFlow = await import("../utils/updateCheck");

    await freshFlow.retryUpdate();

    expect(mocks.check).toHaveBeenCalledTimes(1);
    expect(freshUpdater.getState().phase).toBe("up-to-date");
  });
});
