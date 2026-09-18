// checkForUpdates 的失败原因识别：GitHub 匿名 API 配额耗尽（403 / 429 / rate limit，大小写不敏感）
// 走专用可行动指引文案；其余错误保持通用 notice.updateFailed；无更新走 upToDate。
// （latest.json 若把下载地址指向 GitHub REST 资产 API 端点，匿名配额 60 次/小时/出口 IP 耗尽即 403；
// 403 是穿透代理拿回的 HTTP 状态码，换代理出口 IP 无效 —— 故文案明确写「不要改代理设置」。）
import { describe, it, expect, vi, beforeEach, beforeAll } from "vitest";
import { i18n } from "../i18n";
import { useUi } from "../stores/ui";
import { checkForUpdates } from "../utils/updateCheck";

// vi.mock 工厂被提升到所有 import 之上：mock 句柄必须先用 vi.hoisted 构造，否则报「Cannot access before initialization」
const mocks = vi.hoisted(() => ({
  check: vi.fn<(opts?: { timeout?: number; proxy?: string }) => Promise<unknown>>(),
  resolveProxy: vi.fn(async (): Promise<string | null> => null),
  restartApp: vi.fn(async (): Promise<void> => {}),
}));

// 更新模块走动态 import（await import("@tauri-apps/plugin-updater")），vi.mock 对动态 import 同样生效
vi.mock("@tauri-apps/plugin-updater", () => ({ check: mocks.check }));

// 只替换 resolveProxy / restartApp，其余 IPC 成员保留原样（stores/ui → utils/uiState 也会用到 ipc）
// 注意：specifier 相对本测试文件解析，必须与 updateCheck 中的 "../ipc/client" 指向同一文件
vi.mock("../ipc/client", async (importOriginal) => {
  const actual = await importOriginal<typeof import("../ipc/client")>();
  return {
    ...actual,
    ipc: { ...actual.ipc, resolveProxy: mocks.resolveProxy, restartApp: mocks.restartApp },
  };
});

/** 假 Update：downloadAndInstall 在传入 err 时 reject */
function fakeUpdate(err?: Error) {
  return {
    version: "9.9.9",
    downloadAndInstall: vi.fn(async () => {
      if (err) throw err;
    }),
  };
}

/** 最近一条通知正文（下载前会先 toast「正在下载」，故失败断言取最后一条） */
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
  mocks.resolveProxy.mockImplementation(async () => null);
  useUi.setState({ notifications: [] });
});

describe("checkForUpdates 失败原因识别", () => {
  it("403 Forbidden（匿名 API 配额耗尽）：给专用指引而非裸报错，不重启", async () => {
    mocks.check.mockResolvedValue(
      fakeUpdate(new Error("Download request failed with status: 403 Forbidden")),
    );

    await checkForUpdates();

    const body = lastToast();
    expect(body).toBe(i18n.t("notice.updateFailedRateLimited"));
    expect(body).not.toBe(
      i18n.t("notice.updateFailed", {
        error: "Error: Download request failed with status: 403 Forbidden",
      }),
    );
    // 三个信息齐备：原因（403 / 配额）+ 替代（稍后重试）+ 替代（Release 页手动下载）
    expect(body).toMatch(/403/);
    expect(body).toMatch(/配额|quota/);
    expect(body).toMatch(/稍后|later/);
    expect(body).toMatch(/Release/);
    expect(mocks.restartApp).not.toHaveBeenCalled();
  });

  it("其他错误（非配额）：保持通用 notice.updateFailed 并带上原始错误串", async () => {
    mocks.check.mockResolvedValue(fakeUpdate(new Error("boom")));

    await checkForUpdates();

    expect(lastToast()).toBe(i18n.t("notice.updateFailed", { error: "Error: boom" }));
    expect(lastToast()).not.toBe(i18n.t("notice.updateFailedRateLimited"));
    expect(mocks.restartApp).not.toHaveBeenCalled();
  });

  it.each([
    "Download request failed with status: 429 Too Many Requests",
    "API rate limit exceeded for 203.0.113.7",
    "RATE LIMIT EXCEEDED",
  ])("429 / rate limit 变体同样识别：%s", async (message) => {
    mocks.check.mockResolvedValue(fakeUpdate(new Error(message)));

    await checkForUpdates();

    expect(lastToast()).toBe(i18n.t("notice.updateFailedRateLimited"));
  });

  it("无更新（check 返回 null）：toast upToDate，不下载不重启", async () => {
    mocks.check.mockResolvedValue(null);

    await checkForUpdates();

    expect(lastToast()).toBe(i18n.t("notice.upToDate"));
    expect(useUi.getState().notifications).toHaveLength(1);
    expect(mocks.restartApp).not.toHaveBeenCalled();
  });

  it("无代理（resolveProxy 返回 null）时，check 只带 timeout 参数", async () => {
    mocks.check.mockResolvedValue(fakeUpdate());

    await checkForUpdates();

    expect(mocks.resolveProxy).toHaveBeenCalled();
    expect(mocks.check).toHaveBeenCalledWith({ timeout: 15_000 });
  });
});
