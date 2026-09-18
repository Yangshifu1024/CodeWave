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
import { checkForUpdates } from "../utils/updateCheck";

const mocks = vi.hoisted(() => ({
  openUrl: vi.fn(async (): Promise<void> => {}),
  restartApp: vi.fn(async (): Promise<void> => {}),
  check: vi.fn(async (): Promise<unknown> => null),
  downloadAndInstall: vi.fn(async (): Promise<void> => {}),
}));

vi.mock("@tauri-apps/plugin-updater", () => ({ check: mocks.check }));
vi.mock("../ipc/client", async (importOriginal) => {
  const actual = await importOriginal<typeof import("../ipc/client")>();
  return {
    ...actual,
    ipc: { ...actual.ipc, openUrl: mocks.openUrl, restartApp: mocks.restartApp },
  };
});

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
    // 先让流程拿到一个 Update 实例（pendingUpdate），再点按钮——否则 startUpdate 会因无实例直接返回
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
    await checkForUpdates();
    expect(useUpdater.getState().phase).toBe("available");

    renderModal();
    fireEvent.click(button(i18n.t("updater.actions.downloadInstall")));

    // 下载中：相位转到 downloading（按钮已接到 startUpdate）
    await vi.waitFor(() => {
      expect(mocks.downloadAndInstall).toHaveBeenCalledTimes(1);
    });
    expect(useUpdater.getState().phase).toBe("downloading");

    // 下载完成：转 ready 且弹窗强制保持打开（重启入口不被吞）
    releaseDownload();
    await vi.waitFor(() => {
      expect(useUpdater.getState().phase).toBe("ready");
    });
    expect(useUpdater.getState().modalOpen).toBe(true);
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
    seed({ phase: "error", newVersion: null, error: "检查失败" });
    mocks.check.mockResolvedValueOnce(null);

    renderModal();
    fireEvent.click(button(i18n.t("updater.actions.tryAgain")));

    await vi.waitFor(() => {
      expect(mocks.check).toHaveBeenCalledTimes(1);
    });
    // 重新检查且无更新 → 相位回到 up-to-date（弹窗不再有动作按钮）
    expect(useUpdater.getState().phase).toBe("up-to-date");
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
