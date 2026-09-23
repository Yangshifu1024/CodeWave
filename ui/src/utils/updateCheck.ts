// 应用内自动更新的流程控制（tauri-plugin-updater，[docs/version-bump-and-release](../../../docs/version-bump-and-release.md)）：
// 检查 → 发现新版本（弹窗展示发布说明）→ 下载安装（弹窗进度条）→ 重启进新版。
//
// 入口：
//   · checkForUpdates()：打开「关于」弹框点「检查更新」（三平台）、macOS 应用菜单「检查更新」；
//   · useStartupUpdateCheck()：应用启动 3s 后静默检查一次（默认开启，可用 ws_auto_update=false 关掉）；
//   · 弹窗按钮：开始下载 / 打开 Release 页 / 隐藏 / 立即重启 / 重试（见 features/panels/UpdateModal）。
//
// 状态一律落 stores/updater（phase 机 + 进度 + 弹窗开关），本文件只管流程与时序：
//   · epoch 守卫：整条流程带检查代次，陈旧结果丢弃（启动静默检查与手动检查可能重叠，
//     旧结果不得把 available 回滚成 up-to-date）；
//   · pendingUpdate：模块级槽位，下载失败重试时复用它（不必重新检查）；
//   · installInFlight：下载安装的防重入闸（弹窗按钮 + 菜单可能同时触发）；
//   · 进度降频：进度事件按 chunk 到达，逐条写 store 会让弹窗重渲染风暴（见 startUpdate 的 writeProgress）。
// 无更新/失败不再 toast（弹窗接管反馈），但 notice.updateFailed* 文案仍是错误文案来源。
// restart_app / open_url 走自定义 IPC（公钥在 tauri.conf.json；更新源 = GitHub Releases latest.json）。
import { useEffect, useRef, useState } from "react";
import type { Update } from "@tauri-apps/plugin-updater";
import { i18n } from "../i18n";
import { ipc } from "../ipc/client";
import { useUi } from "../stores/ui";
import { useUpdater } from "../stores/updater";

/** Linux deb/rpm 无法自替换二进制，只能手动下载安装（AppImage 可以，见 ipc.isAppimage） */
const RELEASES_LATEST = "https://github.com/Yangshifu1024/CodeWave/releases/latest";
/** 单个版本的 Release 页（「查看发布说明」按钮；页面上有完整的 markdown 渲染与资产列表） */
const releaseTagUrl = (version: string) =>
  `https://github.com/Yangshifu1024/CodeWave/releases/tag/v${version}`;

/** 启动静默检查的偏好 key：只有显式写入 "false" 才关闭（默认开启） */
export const AUTO_UPDATE_KEY = "ws_auto_update";
/** 启动检查延迟：等配置/会话/工作区恢复完再打网络，避免和启动链路抢资源 */
const STARTUP_DELAY_MS = 3_000;

/** 下载安装防重入：true 时 startUpdate 直接返回（弹窗按钮与菜单可能同时触发） */
let installInFlight = false;
/** 已检查到、尚未安装完成的 Update 实例（失败重试时复用，避免重新走 check） */
let pendingUpdate: Update | null = null;

// 配额耗尽识别：latest.json 若把下载地址指向 GitHub REST 资产 API 端点，匿名配额（60 次/小时/出口 IP）
// 耗尽即回 403（偶发 429 / rate limit）。tauri 抛出的 Error 经 String() 后可能带前缀，故用宽松正则匹配。
const RATE_LIMIT_RE = /\b(403|429)\b|rate limit/i;

/** 服务端未给 content-length 时的进度写入步长：没有百分比可用，只能用字节粗粒度限流 */
const PROGRESS_BYTE_STEP = 64 * 1024;

/** 失败文案：403/429 走可行动指引（配额耗尽该等或手动下载，换代理无效），其余带原始错误串 */
function formatError(e: unknown): string {
  const text = String(e);
  return RATE_LIMIT_RE.test(text)
    ? i18n.t("notice.updateFailedRateLimited")
    : i18n.t("notice.updateFailed", { error: text });
}

/** Linux 判定：只影响「能否自替换」这一分支，误判的最坏结果是给用户一条多余的手动下载说明 */
function isLinux(): boolean {
  return typeof navigator !== "undefined" && /Linux/i.test(navigator.userAgent);
}

/**
 * 检查更新。silent = 启动自动检查：全程不可见（无更新/失败/离线都不打扰用户）。
 * 显式检查：有更新开弹窗，无更新 toast「已是最新」，失败开错误弹窗。
 */
export async function checkForUpdate({ silent = false }: { silent?: boolean } = {}): Promise<void> {
  const store = useUpdater.getState();
  const epoch = silent ? store.startCheckEpoch() : store.beginCheck();
  /** 我是否仍是最新一次检查（陈旧结果的丢弃闸门） */
  const fresh = () => useUpdater.getState().checkEpoch === epoch;

  try {
    const { check } = await import("@tauri-apps/plugin-updater");
    // 更新请求跟随网络代理设置（[docs/network-proxy-settings](../../../docs/network-proxy-settings.md)）：
    // 插件命令层 check 原生接受 proxy 且 download/install 复用同一 Update 上的代理；null（直连/无代理）时不传保持默认
    const proxy = await ipc.resolveProxy().catch(() => null);
    if (!fresh()) return;

    const update = await check({ timeout: 15_000, ...(proxy ? { proxy } : {}) });
    if (!fresh()) {
      // 已被更新的一次检查取代：丢弃该实例，别泄漏后端资源
      if (update) void update.close().catch(() => {});
      return;
    }

    if (!update) {      if (silent) return;
      // 无更新时的版本号：优先用清单返回的 currentVersion，兜底 appVersion（失败显示「—」而非崩）
      const current = await ipc.appVersion().catch(() => "");
      if (!fresh()) return;
      useUpdater.getState().markUpToDate(current || "—");
      // 显式检查必须给反馈：「已是最新」走 toast（弹窗只服务「有更新 / 失败 / 待重启」三态，
      // 无更新弹窗是打扰）；静默检查则完全不可见
      useUi.getState().toast(i18n.t("notice.upToDate"));
      return;
    }

    // 覆盖前先释放上一个实例（用户可能已经点过「稍后」、或上一次检查被赶超）
    void pendingUpdate?.close().catch(() => {});
    pendingUpdate = update;
    // Linux 上只有 AppImage 能自替换；deb/rpm 降级为「打开发布页手动下载」（判定走后端 is_appimage）
    const manual = isLinux() && !(await ipc.isAppimage().catch(() => false));
    if (!fresh()) {
      // 本次也已被赶超：连同刚记下的实例一起丢弃，不留悬空引用
      void update.close().catch(() => {});
      if (pendingUpdate === update) pendingUpdate = null;
      return;
    }
    useUpdater.getState().markAvailable({
      currentVersion: update.currentVersion,
      newVersion: update.version,
      notes: update.body ?? null,
      manual,
    });
  } catch (e) {
    // 静默检查失败是常态（离线 / 清单缺失 / 网络不通），不打扰用户
    if (silent || !fresh()) return;
    useUpdater.getState().fail(formatError(e));
  }
}

/** 下载并安装（弹窗「下载并安装」按钮与失败重试共用的复入口） */
export async function startUpdate(): Promise<void> {
  if (installInFlight) return;
  const update = pendingUpdate;
  if (!update) return;

  installInFlight = true;
  const store = () => useUpdater.getState();
  store().beginDownload();

  // 进度事件（Started/Progress/Finished）由插件按 chunk 回调；contentLength 可能缺省，
  // 此时只显示已下载字节、进度条走不确定态（antd Progress 不加 percent）。
  //
  // 关键：**不能每个事件都写 store**。插件对每个 HTTP 分片发一条 Progress（19MB 的安装包是
  // 千级到数千级事件），而每条事件都要经 tauri Channel 的 webview.eval 投递一次、并让整个
  // 弹窗重渲染一次。处理速度跟不上到达速度时显示值会一路落后于真实下载，下载结束又被
  // markReady 立刻换成「待重启」——用户看到的就是「进度卡在某个百分比直到下载完成」。
  // 故只把**用户能看出来的变化**写进 store：有总量时按可见整数百分比、无总量时按字节跨步；
  // Finished 与收尾一律强制写最终值。
  let downloaded = 0;
  let total: number | null = null;
  /** 已写进 store 的可见百分比（null = 无总量或尚未写过）与已写进的字节数 */
  let writtenPercent: number | null = null;
  let writtenBytes = 0;
  const writeProgress = (force = false) => {
    // 与界面口径一致（弹窗用 Math.round 且封顶 100）：下载量超出总量时百分比不再变化，不再写
    const percent =
      total !== null && total > 0 ? Math.min(100, Math.round((downloaded / total) * 100)) : null;
    const changed =
      percent !== null ? percent !== writtenPercent : downloaded - writtenBytes >= PROGRESS_BYTE_STEP;
    if (!force && !changed) return;
    writtenPercent = percent;
    writtenBytes = downloaded;
    store().setProgress(downloaded, total);
  };

  try {
    await update.downloadAndInstall((event) => {
      // 本回调**绝不能抛**：tauri 的 JS Channel 用严格递增序号保序（只有 index === nextMessageIndex
      // 才回调并递增），而 window.__TAURI_INTERNALS__.runCallback 没有 try/catch——抛一次异常就会
      // 让序号停住，之后所有进度事件永久积压（表现为进度彻底不动且不再恢复）。畸形事件忽略即可。
      try {
        if (event.event === "Started") {
          total = event.data.contentLength ?? null;
        } else if (event.event === "Progress") {
          downloaded += event.data.chunkLength;
        } else {
          // 收尾对齐总量：让进度条能走到 100%（服务端有小幅出入时不至于卡在 99%）
          downloaded = total ?? downloaded;
        }
        writeProgress();
      } catch {
        /* 单条事件解析失败不影响整体流程 */
      }
    });
    // 收尾强制写最终值：无总量时中间事件按字节步长被合并掉了，最后一次必须落库
    writeProgress(true);
    // 安装完成：释放 Update 实例（后端资源），进入待重启态
    installInFlight = false;
    pendingUpdate = null;
    void update.close().catch(() => {});
    store().markReady();
  } catch (e) {
    installInFlight = false;
    store().fail(formatError(e));
  }
}

/** 重试：下载中途失败优先复用已检查到的更新（重下），否则重新检查 */
export async function retryUpdate(): Promise<void> {
  if (pendingUpdate) await startUpdate();
  else await checkForUpdate({});
}

/** 重启应用以运行新版本（Windows 上 downloadAndInstall 已自退出，本入口主要服务 macOS/Linux） */
export function restartUpdate(): void {
  void ipc.restartApp();
}

/** 打开发布页（manual-download 态的手动下载出口，也是配额耗尽时的替代路径） */
export function openReleases(newVersion?: string | null): void {
  const url = newVersion ? releaseTagUrl(newVersion) : RELEASES_LATEST;
  void ipc.openUrl(url).catch(() => {});
}

/** 兼容入口：既有的「检查更新」按钮 / macOS 菜单项仍调它（等价于显式检查） */
export async function checkForUpdates(): Promise<void> {
  await checkForUpdate({ silent: false });
}

/** 读偏好：只有显式写入 "false" 才关闭（键缺失/非法值一律视为开启） */
function readStoredAutoUpdate(): boolean {
  try {
    return localStorage.getItem(AUTO_UPDATE_KEY) !== "false";
  } catch {
    // localStorage 不可用（极少见）时按开启处理，不让偏好读失败影响启动链路
    return true;
  }
}

/**
 * 启动自动检查更新的偏好（设置 → 通用 → 更新；默认开启）。
 * 与 GitWave 的 useAutoUpdateSetting 同形：返回 [当前值, 写入函数]，落盘尽力而为。
 */
export function useAutoUpdateSetting(): [boolean, (enabled: boolean) => void] {
  const [autoUpdate, setAutoUpdateState] = useState(readStoredAutoUpdate);
  const setAutoUpdate = (enabled: boolean) => {
    try {
      localStorage.setItem(AUTO_UPDATE_KEY, String(enabled));
    } catch {
      // 落盘尽力而为：内存值仍生效
    }
    setAutoUpdateState(enabled);
  };
  return [autoUpdate, setAutoUpdate];
}

/**
 * 启动静默检查（AppShell 挂载时调用一次）。
 * 偏好中途切换不重跑，下次启动生效；检查本身不可见，只有「有更新」才弹窗。
 */
export function useStartupUpdateCheck(): void {
  const [autoUpdate] = useAutoUpdateSetting();
  // StrictMode 下 mount→unmount→remount 会把首个 timer 清掉，所以标志位必须由 timer 回调自己翻转：
  // 若在 effect 体内翻转，首挂载就把开关用掉，真正的第二次挂载（重放）反而跳过检查。
  const startedRef = useRef(false);
  useEffect(() => {
    if (!autoUpdate) return;
    const timer = window.setTimeout(() => {
      if (startedRef.current) return;
      startedRef.current = true;
      void checkForUpdate({ silent: true });
    }, STARTUP_DELAY_MS);
    return () => window.clearTimeout(timer);
  }, [autoUpdate]);
}
