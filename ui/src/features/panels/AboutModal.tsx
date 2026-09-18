// 关于弹框：应用身份、懒加载的版本号与快捷入口（数据目录 / 仓库 / 检查更新）。
// ui.aboutOpen 为 true 时由 AppShell 渲染；入口按钮在左下角状态区。
// 「检查更新」是 Windows/Linux 的唯一更新入口（macOS 另有应用菜单项，[docs/version-bump-and-release](../../../docs/version-bump-and-release.md)）。
import { useEffect, useState } from "react";
import { App, Button, Modal } from "antd";
import { useTranslation } from "react-i18next";
import { CloudDownloadOutlined, FolderOpenOutlined, GithubOutlined } from "@ant-design/icons";
import { ipc } from "../../ipc/client";
import { useUi } from "../../stores/ui";
import { checkForUpdates } from "../../utils/updateCheck";
import storeLogo from "../../assets/store-logo.png";

// 仓库地址（收拢为单一常量，迁移只需改一行）
const REPO_URL = "https://github.com/Yangshifu1024/CodeWave";

/** 关于弹框：展示应用 Logo/版本/口号，提供打开数据目录与访问仓库两个动作。 */
export default function AboutModal() {
  const { t } = useTranslation();
  const { message } = App.useApp();
  const [version, setVersion] = useState<string | null>(null);
  const [actionError, setActionError] = useState<string | null>(null);
  // 检查更新进行中（按钮 loading）；结果经 UpdateModal / toast 反馈（有更新与失败会弹窗）
  const [checking, setChecking] = useState(false);

  async function runUpdateCheck() {
    setActionError(null);
    setChecking(true);
    try {
      await checkForUpdates();
    } finally {
      setChecking(false);
    }
  }

  // 版本号在首次挂载时懒加载，而非应用启动时；
  // 加载失败降级为占位符，不阻塞弹框。弹框关闭即卸载，
  // 因此每次重开都会自然地重新拉取最新版本。
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
    <Modal
      open
      width={320}
      footer={null}
      title={t("about.title")}
      onCancel={() => useUi.setState({ aboutOpen: false })}
    >
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
          <Button
            size="small"
            icon={<CloudDownloadOutlined />}
            loading={checking}
            onClick={() => void runUpdateCheck()}
          >
            {t("about.checkUpdates")}
          </Button>
        </div>
        {actionError && <div className="about-error">{actionError}</div>}
      </div>
    </Modal>
  );
}
