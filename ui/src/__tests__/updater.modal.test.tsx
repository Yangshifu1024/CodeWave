// 自动更新弹窗（features/panels/UpdateModal）的相位渲染与按钮行为：
//   available（版本行 + 发布说明 + 下载并安装）/ manual-download（手动下载说明 + 打开发布页）/
//   downloading（进度条 + 已下载字节 + 隐藏不取消）/ ready（立即重启）/ error（文案 + 重试两条分支）。
// 弹窗只读 stores/updater 的状态、只调 utils/updateCheck 的流程函数——本文件因此用 setState 直接摆相位。
import { describe, it, expect, vi, beforeEach, afterEach, beforeAll } from "vitest";
import { render, screen, fireEvent, cleanup } from "@testing-library/react";
import { App } from "antd";
import { i18n } from "../i18n";
import UpdateModal from "../features/panels/UpdateModal";
import { useUpdater } from "../stores/updater";

const mocks = vi.hoisted(() => ({
  openUrl: vi.fn(async (): Promise<void> => {}),
  restartApp: vi.fn(async (): Promise<void> => {}),
  check: vi.fn(async (): Promise<unknown> => null),
  downloadAndInstall: vi.fn(async (): Promise<void> => {}),
  // Linux 降级分支依赖这两个：显式 mock 才能脱离环境（真 ipc 在测试环境会抛，被 .catch 吞成 null/false）
  resolveProxy: vi.fn(async (): Promise<string | null> => null),
  isAppimage: vi.fn(async (): Promise<boolean> => true),
  appVersion: vi.fn(async (): Promise<string> => "1.0.0"),
}));

vi.mock("@tauri-apps/plugin-updater", () => ({ check: mocks.check }));
vi.mock("../ipc/client", async (importOriginal) => {
  const actual = await importOriginal<typeof import("../ipc/client")>();
  return {
    ...actual,
    ipc: {
      ...actual.ipc,
      openUrl: mocks.openUrl,
      restartApp: mocks.restartApp,
      resolveProxy: mocks.resolveProxy,
      isAppimage: mocks.isAppimage,
      appVersion: mocks.appVersion,
    },
  };
});

// happy-dom 的 navigator.userAgent 随宿主平台变（macOS 本地不报、Linux runner 上报），
// 而 isLinux() 依赖它。测试必须自己固定 UA，否则「available 还是 manual-download」会在不同机器上翻车。
const MAC_UA = "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/605.1.15 (KHTML, like Gecko)";
const LINUX_UA = "Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/605.1.15 (KHTML, like Gecko)";
function setUserAgent(ua: string) {
  Object.defineProperty(window.navigator, "userAgent", { value: ua, configurable: true });
}

/** 把 store 摆到某个相位（弹窗只消费这些字段） */
function seed(over: Partial<ReturnType<typeof useUpdater.getState>> = {}) {
  useUpdater.setState({
    phase: "idle",
    modalOpen: true,
    currentVersion: "1.0.0",
    newVersion: "9.9.9",
    notes: null,
    downloadedBytes: 0,
    totalBytes: null,
    error: null,
    checkEpoch: 0,
    ...over,
  });
}

function renderModal() {
  return render(
    <App>
      <UpdateModal />
    </App>,
  );
}

/** 按钮文案匹配：antd 会给两字按钮插空格，先去空白再比较 */
function button(label: string): HTMLElement {
  const hit = screen
    .getAllByRole("button")
    .find((b) => (b.textContent ?? "").replace(/\s/g, "") === label.replace(/\s/g, ""));
  if (!hit) throw new Error(`未找到按钮：${label}`);
  return hit;
}

beforeAll(async () => {
  await i18n.changeLanguage("zh-CN");
});

beforeEach(() => {
  mocks.openUrl.mockClear();
  mocks.restartApp.mockClear();
  mocks.check.mockClear();
  mocks.downloadAndInstall.mockClear();
  mocks.isAppimage.mockImplementation(async () => true);
  setUserAgent(MAC_UA);
  seed();
});

afterEach(() => {
  cleanup();
  seed({ phase: "idle", modalOpen: false });
});

describe("UpdateModal · available", () => {
  it("显示新版本与当前版本、发布说明全文，页脚为稍后 + 下载并安装", () => {    const notes = "## 新特性\n\n- 更快的启动\n- 修复若干问题";
    seed({ phase: "available", notes });

    renderModal();

    expect(document.querySelector(".updater-version-new")!.textContent).toContain("9.9.9");
    expect(document.querySelector(".updater-version-cur")!.textContent).toContain("1.0.0");
    // 发布说明按纯文本展示：多行原文必须在 DOM 文本里完整可见（含换行）
    const body = document.querySelector(".updater-notes-body")!;
    expect(body.textContent).toContain("更快的启动");
    expect(body.textContent).toContain("修复若干问题");
    expect(body.textContent).toContain("\n");
    expect(button(i18n.t("updater.actions.downloadInstall"))).toBeTruthy();
    expect(button(i18n.t("updater.actions.later"))).toBeTruthy();
  });

  it("notes 为空时不渲染发布说明区块", () => {
    seed({ phase: "available", notes: null });
    renderModal();
    expect(document.querySelector(".updater-notes")).toBeNull();
  });

  it("「查看发布说明」打开该版本的 tag 页", () => {
    seed({ phase: "available", notes: null });
    renderModal();

    fireEvent.click(button(i18n.t("updater.viewReleaseNotes")));

    expect(mocks.openUrl).toHaveBeenCalledWith(
      "https://github.com/Yangshifu1024/CodeWave/releases/tag/v9.9.9",
    );
  });

  it("「下载并安装」真正进入下载相位，完成后转为待重启（按钮 → 流程函数已接通）", async () => {
    // 流程的 pendingUpdate / installInFlight 是模块级槽位，跨用例会残留——本用例全程用
    // resetModules 后的干净模块实例（组件也要重新 import，否则它闭包里的还是旧模块）。
    vi.resetModules();
    const [{ default: FreshModal }, { useUpdater: freshStore }, flow] = await Promise.all([
      import("../features/panels/UpdateModal"),
      import("../stores/updater"),
      import("../utils/updateCheck"),
    ]);

    let releaseDownload: () => void = () => {};
    mocks.downloadAndInstall.mockImplementationOnce(
      () =>
        new Promise<void>((resolve) => {
          releaseDownload = resolve;
        }),
    );
    mocks.check.mockResolvedValueOnce({
      currentVersion: "1.0.0",
      version: "9.9.9",
      body: null,
      downloadAndInstall: mocks.downloadAndInstall,
      close: vi.fn(async () => {}),
    });
    await flow.checkForUpdates();
    expect(freshStore.getState().phase).toBe("available");

    render(
      <App>
        <FreshModal />
      </App>,
    );
    fireEvent.click(button(i18n.t("updater.actions.downloadInstall")));

    // 下载中：相位转到 downloading（按钮已接到 startUpdate）
    await vi.waitFor(() => {
      expect(mocks.downloadAndInstall).toHaveBeenCalledTimes(1);
    });
    expect(freshStore.getState().phase).toBe("downloading");

    // 下载完成：转 ready 且弹窗强制保持打开（重启入口不被吞）
    releaseDownload();
    await vi.waitFor(() => {
      expect(freshStore.getState().phase).toBe("ready");
    });
    expect(freshStore.getState().modalOpen).toBe(true);
  });
});

describe("UpdateModal · Linux 降级分支（deb/rpm 不能自替换）", () => {
  it("UA 为 Linux 且非 AppImage 时，检查到更新直接进 manual-download", async () => {
    vi.resetModules();
    setUserAgent(LINUX_UA);
    mocks.isAppimage.mockImplementation(async () => false);
    mocks.check.mockResolvedValueOnce({
      currentVersion: "1.0.0",
      version: "9.9.9",
      body: "## 新特性",
      downloadAndInstall: mocks.downloadAndInstall,
      close: vi.fn(async () => {}),
    });
    const { useUpdater: freshStore, checkForUpdates: freshCheck } = {
      useUpdater: (await import("../stores/updater")).useUpdater,
      checkForUpdates: (await import("../utils/updateCheck")).checkForUpdates,
    };

    await freshCheck();

    expect(mocks.isAppimage).toHaveBeenCalled();
    expect(freshStore.getState().phase).toBe("manual-download");
  });

  it("AppImage 上仍走 available（可自替换）", async () => {
    vi.resetModules();
    setUserAgent(LINUX_UA);
    mocks.isAppimage.mockImplementation(async () => true);
    mocks.check.mockResolvedValueOnce({
      currentVersion: "1.0.0",
      version: "9.9.9",
      body: null,
      downloadAndInstall: mocks.downloadAndInstall,
      close: vi.fn(async () => {}),
    });
    const { useUpdater: freshStore, checkForUpdates: freshCheck } = {
      useUpdater: (await import("../stores/updater")).useUpdater,
      checkForUpdates: (await import("../utils/updateCheck")).checkForUpdates,
    };

    await freshCheck();

    expect(freshStore.getState().phase).toBe("available");
  });
});

describe("UpdateModal · manual-download（Linux deb/rpm）", () => {
  it("显示手动下载说明，主操作是打开发布页（latest 页）", () => {
    seed({ phase: "manual-download", notes: null });

    renderModal();

    expect(document.querySelector(".updater-note")!.textContent).toContain(
      i18n.t("updater.manualDownloadNote"),
    );
    fireEvent.click(button(i18n.t("updater.actions.openReleases")));
    expect(mocks.openUrl).toHaveBeenCalledWith(
      "https://github.com/Yangshifu1024/CodeWave/releases/tag/v9.9.9",
    );
    // 手动下载路径不得出现「下载并安装」（该安装形态无法自替换）
    expect(() => button(i18n.t("updater.actions.downloadInstall"))).toThrow();
  });
});

describe("UpdateModal · downloading", () => {
  it("显示进度条与字节文案；页脚只有隐藏，隐藏后下载相位不变", () => {
    seed({ phase: "downloading", downloadedBytes: 512 * 1024, totalBytes: 1024 * 1024 });

    renderModal();

    expect(document.querySelector(".updater-progress")).toBeTruthy();
    expect(document.querySelector(".updater-progress-text")!.textContent).toContain("512.0 KB");
    expect(document.querySelector(".updater-progress-text")!.textContent).toContain("1.0 MB");
    const bar = document.querySelector(".ant-progress") as HTMLElement | null;
    expect(bar).toBeTruthy();

    fireEvent.click(button(i18n.t("updater.actions.hide")));

    const st = useUpdater.getState();
    expect(st.modalOpen).toBe(false);
    expect(st.phase).toBe("downloading"); // 隐藏 ≠ 取消：下载继续，完成后再弹
  });

  it("总量未知时只显示已下载字节，不出现 NaN", () => {
    seed({ phase: "downloading", downloadedBytes: 2048, totalBytes: null });
    renderModal();

    const text = document.querySelector(".updater-progress-text")!.textContent ?? "";
    expect(text).toContain("2.0 KB");
    expect(text).not.toMatch(/NaN|undefined/);
  });
});

describe("UpdateModal · ready", () => {
  it("显示重启说明，「立即重启」调用 restart_app", () => {
    seed({ phase: "ready", newVersion: null });

    renderModal();

    expect(document.querySelector(".updater-note")!.textContent).toContain(
      i18n.t("updater.restartToApply"),
    );
    fireEvent.click(button(i18n.t("updater.actions.restartNow")));
    expect(mocks.restartApp).toHaveBeenCalledTimes(1);
  });
});

describe("UpdateModal · error", () => {
  it("显示错误文案，页脚为关闭 + 重试", () => {
    seed({ phase: "error", newVersion: null, error: "下载失败：boom" });

    renderModal();

    expect(document.querySelector(".updater-error")!.textContent).toContain("boom");
    expect(button(i18n.t("updater.actions.tryAgain"))).toBeTruthy();
    fireEvent.click(button(i18n.t("updater.actions.close")));
    expect(useUpdater.getState().modalOpen).toBe(false);
  });

  it("「重试」在无待装实例时重新检查（按钮 → 流程函数已接通）", async () => {
    // 同样用干净模块实例：确保 pendingUpdate 为空，走「重新检查」那条分支
    vi.resetModules();
    mocks.check.mockResolvedValueOnce(null);
    const [{ default: FreshModal }, { useUpdater: freshStore }] = await Promise.all([
      import("../features/panels/UpdateModal"),
      import("../stores/updater"),
    ]);
    freshStore.setState({ phase: "error", modalOpen: true, newVersion: null, error: "检查失败" });

    render(
      <App>
        <FreshModal />
      </App>,
    );
    fireEvent.click(button(i18n.t("updater.actions.tryAgain")));

    await vi.waitFor(() => {
      expect(mocks.check).toHaveBeenCalledTimes(1);
    });
    // 重新检查且无更新 → 相位回到 up-to-date（弹窗不再有动作按钮）
    expect(freshStore.getState().phase).toBe("up-to-date");
  });
});

describe("UpdateModal · 相位与页脚对应关系", () => {
  it("无可操作相位（checking / up-to-date）不渲染任何动作按钮", () => {
    // 注：antd Modal 自带右上角关闭 X（role=button），所以不能数总按钮数，改为确认无任何动作文案
    const actionKeys = [
      "later",
      "downloadInstall",
      "openReleases",
      "hide",
      "restartNow",
      "close",
      "tryAgain",
    ] as const;
    for (const phase of ["checking", "up-to-date"] as const) {
      seed({ phase, modalOpen: true, newVersion: null });
      renderModal();
      const texts = screen
        .getAllByRole("button")
        .map((b) => (b.textContent ?? "").replace(/\s/g, ""));
      for (const key of actionKeys) {
        expect(texts).not.toContain(i18n.t(`updater.actions.${key}`).replace(/\s/g, ""));
      }
      cleanup();
    }
  });
});
