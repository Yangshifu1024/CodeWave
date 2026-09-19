// 设置页「关于」页（[docs/settings-ia](../../../../docs/settings-ia.md)）：
// 原「关于」弹框（AboutModal，本批退役）的内容整体迁入全屏设置页第 8 页，
// 并收编旧「通用」页的自动更新开关 + 手动检查更新 —— 从此「关于」只有一处入口。
// 版本号仍是懒加载（失败降级 ?.?.?）：本页被渲染时才拉一次（离开本页即卸载、再进入重新拉取），
// 不在应用启动时拉取。
import { useEffect, useState } from "react";
import { Button, Form, Switch } from "antd";
import { useTranslation } from "react-i18next";
import { CloudDownloadOutlined, FolderOpenOutlined, GithubOutlined } from "@ant-design/icons";
import { ipc } from "../../ipc/client";
import { checkForUpdates, useAutoUpdateSetting } from "../../utils/updateCheck";
import storeLogo from "../../assets/store-logo.png";

// 仓库地址（收拢为单一常量，迁移只需改一行）
const REPO_URL = "https://github.com/Yangshifu1024/CodeWave";

/** 关于页：应用身份（logo / 版本 / slogan）+ 数据目录与仓库入口 + 自动更新开关与手动检查更新。
 *  「检查更新」是 Windows/Linux 的唯一更新入口（macOS 另有应用菜单项，
 *  [docs/version-bump-and-release](../../../../docs/version-bump-and-release.md)）。 */
export function AboutSettings() {
  const { t } = useTranslation();
  const [version, setVersion] = useState<string | null>(null);
  const [actionError, setActionError] = useState<string | null>(null);
  // 手动检查更新进行中（按钮 loading）；结果经 UpdateModal / toast 反馈：有更新与失败会弹窗，
  // 本页只需 loading
  const [checking, setChecking] = useState(false);
  // 自动更新偏好（localStorage 落盘、默认开启；与 GitWave 同形）
  const [autoUpdate, setAutoUpdate] = useAutoUpdateSetting();

  // 版本号懒加载：失败降级为占位符，不阻塞本页
  useEffect(() => {
    let stale = false;
    void ipc
      .appVersion()
      .then((v) => {
        if (!stale) setVersion(v);
      })
      .catch(() => {
        if (!stale) setVersion("?.?.?");
      });
    return () => {
      stale = true;
    };
  }, []);

  async function runUpdateCheck() {
    setActionError(null);
    setChecking(true);
    try {
      await checkForUpdates();
    } finally {
      setChecking(false);
    }
  }

  async function openDataDir() {
    setActionError(null);
    try {
      await ipc.openDataDir();
    } catch (e) {
      setActionError(String(e));
    }
  }

  async function openRepo() {
    setActionError(null);
    try {
      await ipc.openUrl(REPO_URL);
    } catch (e) {
      setActionError(String(e));
    }
  }

  return (
    <div className="about-pane">
      <div className="about-body">
        <img className="about-logo" src={storeLogo} alt="CodeWave" draggable={false} />
        <div className="about-name">CodeWave</div>
        <div className="about-version">{version ?? "…"}</div>
        <div className="about-slogan">{t("about.slogan")}</div>
        <div className="about-actions">
          <Button size="small" icon={<FolderOpenOutlined />} onClick={() => void openDataDir()}>
            {t("about.appData")}
          </Button>
          <Button size="small" icon={<GithubOutlined />} onClick={() => void openRepo()}>
            {t("about.repo")}
          </Button>
        </div>
        {actionError && <div className="about-error">{actionError}</div>}
      </div>

      <Form layout="vertical">
        <Form.Item label={t("settings.updates")} tooltip={t("settings.updatesHint")}>
          <div className="settings-update-row">
            {/* 即时生效：开关直接写 localStorage（useAutoUpdateSetting），不进 draft 脏标记 */}
            <Switch
              size="small"
              checked={autoUpdate}
              onChange={setAutoUpdate}
              aria-label={t("settings.autoUpdateCheckbox")}
            />
            <span className="settings-update-label">{t("settings.autoUpdateCheckbox")}</span>
            <span className="settings-instant">{t("settings.instantApply")}</span>
            <Button size="small" icon={<CloudDownloadOutlined />} loading={checking} onClick={() => void runUpdateCheck()}>
              {t("settings.checkForUpdates")}
            </Button>
          </div>
        </Form.Item>
      </Form>
    </div>
  );
}
