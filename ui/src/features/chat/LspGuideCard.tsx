// LSP 语义校验引导卡（`lsp:server_missing` 的聊天内嵌落点）。
// 三景（后端 InstallKind）：
//   installable    —— 「未找到 <server>」+ 安装命令 + 「安装」（调 lsp_install，成功后重探测并自收）
//   manual         —— 「需要手动安装 <server>」+ 前置条件 + 官方地址（经 ipc.openUrl 交系统浏览器，不用 <a href>）
//   confirm_enable —— 「Java 语义校验默认关闭」+ 代价说明 + 「启用」（调 lsp_enable）
// 打扰控制：同一 (language, project_id) 在同一会话内只留一张卡；「忽略」后不再弹（run store per-Tab 桶）。
import { useState } from "react";
import { Alert, App, Button, Space } from "antd";
import { useTranslation } from "react-i18next";
import { ipc } from "../../ipc/client";
import type { LspLanguage } from "../../ipc/types";
import { lspHintKey, useLspHints, useRun } from "../../stores/run";
import type { LspHint } from "../../stores/run";

/** 活跃 Tab 的待展示引导卡（无卡时不渲染任何东西）。 */
export default function LspGuideCards() {
  const hints = useLspHints();
  if (hints.length === 0) return null;
  return (
    <div className="lsp-guide-cards">
      {hints.map((h) => (
        <LspGuideCard key={lspHintKey(h.language, h.projectId)} hint={h} />
      ))}
    </div>
  );
}

/** 单张引导卡（三景共用外壳，按钮按 kind 分流）。 */
export function LspGuideCard({ hint }: { hint: LspHint }) {
  const { t } = useTranslation();
  const { message } = App.useApp();
  const [busy, setBusy] = useState(false);
  const lang = hint.language as LspLanguage;
  const key = lspHintKey(hint.language, hint.projectId);
  // 忽略 / 安装完成 / 启用完成都走同一收口：移除卡片并登记去重键
  const dismiss = () => useRun.getState().dismissLspHint(key);

  /** 一键安装（TS/JS、Python 走 npx；Go 走 go install）：结果文案由后端回喂。 */
  async function install() {
    setBusy(true);
    try {
      const text = await ipc.lspInstall(lang);
      message.success(text || t("lsp.installDone", { server: hint.server }));
      // 安装改变了 PATH / 探测面：清缓存重探测一次，设置页再打开时徽标即最新；本卡不再需要
      void ipc.lspRedetect().catch(() => null);
      dismiss();
    } catch (e) {
      message.error(`${t("lsp.installFailed")}：${String(e).replace(/^Error[:\s]*/i, "")}`);
    } finally {
      setBusy(false);
    }
  }

  /** 启用（confirm_enable 路径：java 默认关闭，后端打开开关并落盘）。 */
  async function enable() {
    setBusy(true);
    try {
      await ipc.lspEnable(lang);
      message.success(t("lsp.enableDone", { server: hint.server }));
      dismiss();
    } catch (e) {
      message.error(`${t("lsp.enableFailed")}：${String(e).replace(/^Error[:\s]*/i, "")}`);
    } finally {
      setBusy(false);
    }
  }

  /** 官方地址：交后端 open_url 用系统浏览器打开（与应用内其他外链同一路径）。 */
  async function openDocs() {
    if (!hint.docsUrl) return;
    try {
      await ipc.openUrl(hint.docsUrl);
    } catch (e) {
      message.error(String(e));
    }
  }

  const title =
    hint.kind === "installable" ? t("lsp.installTitle", { server: hint.server })
    : hint.kind === "manual" ? t("lsp.manualTitle", { server: hint.server })
    : t("lsp.confirmTitle");

  return (
    <Alert
      className="lsp-guide-card"
      type="warning"
      showIcon={false}
      style={{ margin: "8px 0" }}
      message={title}
      description={
        <div className="lsp-guide-body">
          {/* reason / prerequisite 为后端回喂文案（已本地化的未找到原因，如「jdtls 需要 JDK 21+」） */}
          {hint.reason && <div className="lsp-guide-reason">{hint.reason}</div>}
          {hint.kind === "installable" && hint.command && (
            <code className="lsp-guide-cmd">{t("lsp.installCommand", { command: hint.command })}</code>
          )}
          {hint.kind === "manual" && hint.prerequisite && (
            <div>{t("lsp.prerequisite", { prerequisite: hint.prerequisite })}</div>
          )}
          {hint.kind === "confirm_enable" && <div>{t("lsp.confirmCost")}</div>}
          <Space size={8}>
            {hint.kind === "installable" && (
              <Button size="small" loading={busy} onClick={() => void install()}>
                {t("lsp.install")}
              </Button>
            )}
            {hint.kind === "manual" && hint.docsUrl && (
              <Button size="small" onClick={() => void openDocs()}>
                {t("lsp.docs")}
              </Button>
            )}
            {hint.kind === "confirm_enable" && (
              <Button size="small" loading={busy} onClick={() => void enable()}>
                {t("lsp.enable")}
              </Button>
            )}
            <Button size="small" type="text" onClick={dismiss}>
              {t("lsp.ignore")}
            </Button>
          </Space>
        </div>
      }
    />
  );
}
