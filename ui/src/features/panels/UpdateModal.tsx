// 自动更新弹窗：发现新版本（含发布说明）/ 下载进度 / 安装完成待重启 / 失败重试 / 手动下载降级。
// 由 AppShell 无条件渲染（可见性取自 stores/updater 的 modalOpen），按钮只调 utils/updateCheck 的流程函数。
//
// 视觉走 antd Modal + Button + Progress，文案全部来自 i18n.updater.*，排版细节在 theme/app.css 的 .updater-* 段。
// 发布说明按 markdown 渲染（latest.json 的 notes 是 markdown 源文，如「## What's Changed + 列表」）：
// 复用 utils/markdown.ts（html:false 不信任原始 HTML、外链带 target=_blank、代码块带 Copy 按钮），
// 容器加 .md 类并接入 app.css 的共享 markdown 样式组；mermaid/公式不渲染（升级链路绑在聊天气泡上）。
import { useMemo } from "react";
import { App, Button, Modal, Progress } from "antd";
import { useTranslation } from "react-i18next";
import { useUpdater } from "../../stores/updater";
import { renderMarkdown } from "../../utils/markdown";
import {
  openReleases,
  restartUpdate,
  retryUpdate,
  startUpdate,
} from "../../utils/updateCheck";

/** 字节数人性化（B / KB / MB，1 位小数）：进度文案用 */
export function formatBytes(n: number): string {
  if (!Number.isFinite(n) || n <= 0) return "0 B";
  if (n < 1024) return `${Math.round(n)} B`;
  if (n < 1024 * 1024) return `${(n / 1024).toFixed(1)} KB`;
  return `${(n / (1024 * 1024)).toFixed(1)} MB`;
}

/** 自动更新弹窗（phase 驱动标题/正文/页脚三分支） */
export default function UpdateModal() {
  const { t } = useTranslation();
  const { message } = App.useApp();
  const phase = useUpdater((s) => s.phase);
  const modalOpen = useUpdater((s) => s.modalOpen);
  const currentVersion = useUpdater((s) => s.currentVersion);
  const newVersion = useUpdater((s) => s.newVersion);
  const notes = useUpdater((s) => s.notes);
  const downloadedBytes = useUpdater((s) => s.downloadedBytes);
  const totalBytes = useUpdater((s) => s.totalBytes);
  const error = useUpdater((s) => s.error);

  // 发布说明的 HTML 只在 notes 变化时重算：下载相位每次进度写入都会重渲染本组件，
  // 不缓存就会白白重解析一遍 markdown。notes 为空/纯空白已在 stores/updater 的 markAvailable 归一为 null。
  const notesHtml = useMemo(() => (notes ? renderMarkdown(notes) : ""), [notes]);

  const close = () => useUpdater.getState().setModalOpen(false);

  const title =
    phase === "ready"
      ? t("updater.title.installed")
      : phase === "error"
        ? t("updater.title.check")
        : t("updater.title.available");

  // 进度百分比：总量未知（服务端未给 content-length）时 null = antd 不确定态
  const percent =
    totalBytes && totalBytes > 0
      ? Math.min(100, Math.round((downloadedBytes / totalBytes) * 100))
      : null;
  const progressText =
    percent === null
      ? t("updater.downloaded", { downloaded: formatBytes(downloadedBytes) })
      : t("updater.downloadedOf", {
          downloaded: formatBytes(downloadedBytes),
          total: formatBytes(totalBytes ?? 0),
        });

  /** 页脚按钮：次要在前、主操作在后；无操作的 phase（checking / up-to-date）不渲染页脚 */
  function footerFor() {
    switch (phase) {
      case "available":
        return [
          <Button key="later" onClick={close}>
            {t("updater.actions.later")}
          </Button>,
          <Button key="download" type="primary" onClick={() => void startUpdate()}>
            {t("updater.actions.downloadInstall")}
          </Button>,
        ];
      case "manual-download":
        return [
          <Button key="later" onClick={close}>
            {t("updater.actions.later")}
          </Button>,
          <Button
            key="releases"
            type="primary"
            onClick={() => openReleases(newVersion)}
          >
            {t("updater.actions.openReleases")}
          </Button>,
        ];
      case "downloading":
        // 隐藏而非取消：下载继续在后台跑，完成后 markReady 会重新弹出
        return [
          <Button key="hide" onClick={close}>
            {t("updater.actions.hide")}
          </Button>,
        ];
      case "ready":
        return [
          <Button key="later" onClick={close}>
            {t("updater.actions.later")}
          </Button>,
          <Button
            key="restart"
            type="primary"
            onClick={() => {
              // 重启即卸载应用（restart_app 不返回）：先给用户一句提示再调
              void message.loading(t("updater.restartToApply"), 1);
              restartUpdate();
            }}
          >
            {t("updater.actions.restartNow")}
          </Button>,
        ];
      case "error":
        return [
          <Button key="close" onClick={close}>
            {t("updater.actions.close")}
          </Button>,
          <Button key="retry" type="primary" onClick={() => void retryUpdate()}>
            {t("updater.actions.tryAgain")}
          </Button>,
        ];
      default:
        return null;
    }
  }

  return (
    <Modal
      open={modalOpen}
      width={440}
      title={title}
      onCancel={close}
      footer={footerFor()}
      // antd 6：maskClosable 已废弃，改 mask 对象；更新弹窗不得被遮罩误关（下载中尤为敏感）
      mask={{ closable: false }}
      // 显式抬高层级：antd 的 Modal 默认 zIndex 相同，而容器位置在首次打开时就冻结，
      // 于是「更新弹窗曾开过 → 用户再从「关于」触发」时，更新弹窗会被关于弹窗的遮罩盖住（看不见）。
      // 常见入口就是关于页的「检查更新」，故这里必须固定高于默认 1000。
      zIndex={1100}
    >
      <div className="updater-body">
        {newVersion && (
          <div className="updater-version">
            <span className="updater-version-new">
              CodeWave v{newVersion} {t("updater.availableSuffix")}
            </span>
            <span className="updater-version-cur">
              {t("updater.runningVersion", { version: currentVersion ?? "—" })}
            </span>
          </div>
        )}

        {newVersion && (
          <Button
            size="small"
            type="link"
            className="updater-notes-link"
            onClick={() => openReleases(newVersion)}
          >
            {t("updater.viewReleaseNotes")}
          </Button>
        )}

        {notes && (
          <div className="updater-notes">
            <div className="updater-notes-title">{t("updater.notesTitle")}</div>
            {/* markdown 渲染（容器 .md 接入共享样式组）；html:false 已挡掉原始 HTML，无 XSS 面 */}
            <div className="updater-notes-body md" dangerouslySetInnerHTML={{ __html: notesHtml }} />
          </div>
        )}

        {phase === "manual-download" && (
          <div className="updater-note">{t("updater.manualDownloadNote")}</div>
        )}

        {phase === "downloading" && (
          <div className="updater-progress" aria-label={t("updater.downloadingAria")}>
            <Progress
              percent={percent ?? undefined}
              status="active"
              size="small"
              showInfo={percent !== null}
            />
            <div className="updater-progress-text">{progressText}</div>
          </div>
        )}

        {phase === "ready" && <div className="updater-note">{t("updater.restartToApply")}</div>}

        {phase === "error" && error && <div className="updater-error">{error}</div>}
      </div>
    </Modal>
  );
}
