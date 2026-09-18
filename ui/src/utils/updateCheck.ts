// 自动更新检查（tauri-plugin-updater，[docs/version-bump-and-release](../../../docs/version-bump-and-release.md)）：
// check → 有更新则 downloadAndInstall → 重启进新版。入口两处：macOS 应用菜单「检查更新」
// （AppShell 菜单监听）与「关于」弹框按钮（三平台）。检查由用户显式发起，无后台轮询；
// 无更新/失败均只 toast 降级，不打断当前会话。restartApp 经自定义 IPC（无密码密钥签名，
// 公钥在 tauri.conf.json；更新源 = GitHub Releases latest.json）。
import { i18n } from "../i18n";
import { ipc } from "../ipc/client";
import { useUi } from "../stores/ui";

// 配额耗尽识别：latest.json 若把下载地址指向 GitHub REST 资产 API 端点，匿名配额（60 次/小时/出口 IP）
// 耗尽即回 403（偶发 429 / rate limit）。tauri 抛出的 Error 经 String() 后可能带前缀，故用宽松正则匹配。
const RATE_LIMIT_RE = /\b(403|429)\b|rate limit/i;

export async function checkForUpdates(): Promise<void> {
  try {
    const { check } = await import("@tauri-apps/plugin-updater");
    // 更新请求跟随网络代理设置（[docs/network-proxy-settings](../../../docs/network-proxy-settings.md)）：
    // 插件命令层 check 原生接受 proxy 且 download/install 复用同一 Update 上的代理；null（直连/无代理）时不传保持默认
    const proxy = await ipc.resolveProxy().catch(() => null);
    const update = await check({ timeout: 15_000, ...(proxy ? { proxy } : {}) });
    if (!update) {
      useUi.getState().toast(i18n.t("notice.upToDate"));
      return;
    }
    useUi.getState().toast(i18n.t("notice.updateDownloading", { version: update.version ?? "" }));
    try {
      await update.downloadAndInstall();
    } catch (e) {
      const text = String(e);
      // 配额耗尽的正确出路是「稍后重试」或「到 Release 页面手动下载」（github.com 走 CDN 不消耗配额）；
      // 403 是穿透代理拿回的 HTTP 状态码，换代理出口 IP 无效，故单独给可行动指引而非裸报错
      if (RATE_LIMIT_RE.test(text)) {
        useUi.getState().toast(i18n.t("notice.updateFailedRateLimited"));
        return;
      }
      useUi.getState().toast(i18n.t("notice.updateFailed", { error: text }));
      return;
    }
    useUi.getState().toast(i18n.t("notice.updateRestarting"));
    // 给 toast 留一拍渲染时间再重启；restart_app 不返回（进程重启）
    setTimeout(() => void ipc.restartApp(), 1_500);
  } catch {
    useUi.getState().toast(i18n.t("notice.updateCheckFailed"));
  }
}
