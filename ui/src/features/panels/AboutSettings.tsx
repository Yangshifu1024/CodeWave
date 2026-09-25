// 设置页「关于」页（[docs/settings-ia](../../../../docs/settings-ia.md)）：
// 原「关于」弹框（AboutModal，批② 退役）的内容整体迁入全屏设置页第 8 页。
// 批④（[docs/settings-terminology](../../../../docs/settings-terminology.md)）做两件事：
//   1. 版式对齐（仅本页页内）：说明位一律走 Form.Item 的 extra（不再依赖悬停 tooltip），
//      只读身份与入口（版本 / 数据目录 / 日志目录 / 代码仓库 / 开源许可证）各自成「标签行 + 说明 + 行内控件」；
//   2. 补两个入口：打开日志目录（复用 ipc.openLogsDir，右栏「日志」Tab 的入口保留）
//      与开源许可证（复用 ipc.openUrl 打开仓库 LICENSE）——两个都零后端改动。
// 版本号仍是懒加载（失败降级 ?.?.?）：本页被渲染时才拉一次（离开本页即卸载、再进入重新拉取），
// 不在应用启动时拉取。
// 版式（2026-09-20 调整）：「检查更新」按钮从「更新」行移至**版本号右侧**——版本与更新检查是同一件事
// （看版本 / 查新版），同行更顺；「更新」行只留自动更新开关 + 「即时生效」提示。
import { useEffect, useState } from "react";
import type { ReactNode } from "react";
import { Button, Switch } from "antd";
import { useTranslation } from "react-i18next";
import { SettingsForm, SettingsFormItem } from "./settings/SettingsTheme";
import {
  CloudDownloadOutlined,
  FileProtectOutlined,
  FileTextOutlined,
  FolderOpenOutlined,
  GithubOutlined,
} from "@ant-design/icons";
import { ipc } from "../../ipc/client";
import { checkForUpdates, useAutoUpdateSetting } from "../../utils/updateCheck";
import storeLogo from "../../assets/store-logo.png";

// 仓库地址（收拢为单一常量，迁移只需改一行）
const REPO_URL = "https://github.com/Yangshifu1024/CodeWave";
// 许可证指向同一仓库的默认分支（main）根目录 LICENSE，不复制许可证全文（本批明确非目标）
const LICENSE_URL = `${REPO_URL}/blob/main/LICENSE`;

/** 关于页：应用身份（logo / 名称 / 简介）+ 只读身份与入口（版本 / 数据目录 / 日志目录 / 代码仓库 / 许可证）
 *  + 自动更新开关与手动检查更新。「检查更新」是 Windows/Linux 的唯一更新入口
 * （macOS 另有应用菜单项，[docs/version-bump-and-release](../../../../docs/version-bump-and-release.md)）。 */
export function AboutSettings() {
  const { t } = useTranslation();
  const [version, setVersion] = useState<string | null>(null);
  const [actionError, setActionError] = useState<{ at: string; message: string } | null>(null);
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

  /**
   * 目录 / 外链入口共用的「失败就地提示」路径：错误**记在触发的行上**（at = 该行锚点 id），
   * 由 entryRow 渲染在自己那行下方——原先统一堆在 Form 末尾，点第 2 行失败可能要到第 5 行之后才看到
   * （版式审计）。仍然不离开设置页。
   */
  async function runAction(at: string, action: () => Promise<void>) {
    setActionError(null);
    try {
      await action();
    } catch (e) {
      setActionError({ at, message: String(e) });
    }
  }

  /** 只读入口行的公共结构：锚点容器（= 注册表 id，批③ 搜索定位）+ 行内按钮 */
  function entryRow(label: string, hint: string, settingId: string, icon: ReactNode, buttonText: string, onClick: () => void) {
    return (
      <SettingsFormItem label={label} extra={hint}>
        <div className="setting-anchor" data-setting-id={settingId}>
          <Button  icon={icon} onClick={onClick}>
            {buttonText}
          </Button>
          {/* 失败提示贴在触发行上（而非统一堆在 Form 末尾） */}
          {actionError?.at === settingId && <div className="about-error">{actionError.message}</div>}
        </div>
      </SettingsFormItem>
    );
  }

  return (
    <div className="about-pane">
      {/* 身份区（只读、非设置项）：logo + 名称 + 一句简介。版本不进这里——它是设置项（app.version），单独成行 */}
      <div className="about-body">
        <img className="about-logo" src={storeLogo} alt="CodeWave" draggable={false} />
        <div className="about-name">CodeWave</div>
        <div className="about-slogan">{t("settings.aboutSlogan")}</div>
      </div>

      <SettingsForm>
        {/* 只读身份与入口：标签行 + extra 说明（不依赖悬停）+ 行内值 / 按钮，与其余 7 页同版式 */}
        {/* 版本号 + 手动检查更新同占一行：两个锚点是**兄弟**而非嵌套——批③ 的搜索定位按 data-setting-id
            打 .settings-item-hit，嵌套会让外层高亮框套住整行（高亮范围与命中项不一致）。
            复用 .settings-update-row 的通用 flex 行样式，不新造版式类。 */}
        <SettingsFormItem label={t("settings.aboutVersion")} extra={t("settings.aboutVersionHint")}>
          <div className="settings-update-row">
            <div className="setting-anchor" data-setting-id="app.version">
              <span className="about-version">{version ?? "…"}</span>
            </div>
            {/* 检查更新是 Windows/Linux 的唯一更新入口（macOS 另有应用菜单项）；loading 态跨到弹窗接管 */}
            <div className="setting-anchor" data-setting-id="app.check_updates">
              <Button  icon={<CloudDownloadOutlined />} loading={checking} onClick={() => void runUpdateCheck()}>
                {t("settings.checkForUpdates")}
              </Button>
            </div>
          </div>
        </SettingsFormItem>
        {entryRow(
          t("settings.aboutAppData"),
          t("settings.aboutAppDataHint"),
          "app.data_dir",
          <FolderOpenOutlined />,
          t("settings.aboutOpenAppData"),
          () => void runAction("app.data_dir", () => ipc.openDataDir()),
        )}
        {entryRow(
          t("settings.aboutLogsDir"),
          t("settings.aboutLogsDirHint"),
          "app.logs_dir",
          <FileTextOutlined />,
          t("settings.aboutOpenLogsDir"),
          () => void runAction("app.logs_dir", () => ipc.openLogsDir()),
        )}
        {entryRow(
          t("settings.aboutRepo"),
          t("settings.aboutRepoHint"),
          "app.repo",
          <GithubOutlined />,
          t("settings.aboutOpenRepo"),
          () => void runAction("app.repo", () => ipc.openUrl(REPO_URL)),
        )}
        {entryRow(
          t("settings.aboutLicense"),
          t("settings.aboutLicenseHint"),
          "app.license",
          <FileProtectOutlined />,
          t("settings.aboutViewLicense"),
          () => void runAction("app.license", () => ipc.openUrl(LICENSE_URL)),
        )}
        {/* 「即时生效」写进标题的括号里（instantApplySuffix）：本行是 flex 行、标题带 margin-right:auto，
            标注挂行尾会被推到最右侧，看不见。 */}
        <SettingsFormItem label={<>{t("settings.updates")}{t("settings.instantApplySuffix")}</>} extra={t("settings.updatesHint")}>
          <div className="settings-update-row">
            {/* 即时生效：开关直接写 localStorage（useAutoUpdateSetting），不进 draft 脏标记。
                锚点（data-setting-id）供批③ 搜索定位：整行控件，不参与宽度三档。
                手动「检查更新」按钮已上移到版本号一行（同一 flex 行样式）。 */}
            <div className="setting-anchor" data-setting-id="ui.auto_update">
              <Switch

                checked={autoUpdate}
                onChange={setAutoUpdate}
                aria-label={t("settings.autoUpdateCheckbox")}
              />
            </div>
            <span className="settings-update-label">{t("settings.autoUpdateCheckbox")}</span>
          </div>
        </SettingsFormItem>
      </SettingsForm>
    </div>
  );
}
