// [docs/html-preview-modal](../../../../docs/html-preview-modal.md)：render_html 产物的大弹框预览。
// 卡片内不再内嵌渲染（那里高度退化为浏览器 iframe 默认的 150px、宽度还被常驻右栏挤压），
// 改由工具卡头部入口打开本弹框：90vw × 85vh 铺满 + 复制源码 / 重新加载 / 浅色·深色画布底色。
// 底色档只作用于承载画布（容器底色 + iframe 的 color-scheme），绝不改写 srcDoc——
// 「复制源码」必须与模型给的 HTML 逐字节一致。
import { useEffect, useRef, useState } from "react";
import { Button, Modal, Segmented, theme } from "antd";
import { useTranslation } from "react-i18next";

/** 按 colorBgBase 亮度判定当前主题亮暗（与 theme/bridge.tsx 的 isDarkBase 同法：取值仍是 antd token，不新增依赖）。 */
function isDarkBase(colorBgBase: string): boolean {
  const hex = colorBgBase.replace("#", "");
  if (!/^[0-9a-fA-F]{6}$/.test(hex)) return false;
  const n = parseInt(hex, 16);
  return 0.299 * ((n >> 16) & 255) + 0.587 * ((n >> 8) & 255) + 0.114 * (n & 255) < 128;
}

/** 复制反馈复位延时（与 utils/codecopy.ts 的 1.5s 同口径）。 */
const COPY_FLASH_MS = 1500;

/** 大弹框预览：iframe 铺满弹框主体，滚动交给 iframe 自身（弹框主体不出纵向滚动条）。 */
export default function WidgetPreviewModal({
  open,
  title,
  html,
  chars,
  onClose,
}: {
  open: boolean;
  /** 弹框标题（出参 title；调用方兜底） */
  title: string;
  /** 原始 HTML（与「复制源码」写出的内容逐字节一致） */
  html: string;
  /** 出参里的字符数（缺失时按 html 长度现算，仅用于展示） */
  chars?: number;
  onClose: () => void;
}) {
  const { t } = useTranslation();
  const { token } = theme.useToken();
  const appDark = isDarkBase(token.colorBgBase);
  /** 画布底色档：默认跟随应用主题；每次打开重置（不记忆、不跨会话） */
  const [light, setLight] = useState(!appDark);
  /** 重新加载 = 换 key 强制重挂 iframe（iframe 无 reload API，重设相同 srcDoc 在部分 WebView 不触发） */
  const [reloadSeq, setReloadSeq] = useState(0);
  const [copyState, setCopyState] = useState<"idle" | "done" | "fail">("idle");
  const copyTimer = useRef<number | undefined>(undefined);

  useEffect(() => {
    if (!open) return;
    setLight(!appDark);
    setCopyState("idle");
  }, [open, appDark]);

  // 卸载时清掉复制反馈计时器（避免定时器打到已卸载组件）
  useEffect(() => () => window.clearTimeout(copyTimer.current), []);

  async function copyHtml() {
    window.clearTimeout(copyTimer.current);
    try {
      await navigator.clipboard.writeText(html);
      setCopyState("done");
    } catch {
      // 剪贴板不可用（权限被拒 / 非安全上下文）不静默：按钮显式给失败反馈
      setCopyState("fail");
    }
    copyTimer.current = window.setTimeout(() => setCopyState("idle"), COPY_FLASH_MS);
  }

  return (
    <Modal
      open={open}
      onCancel={onClose}
      footer={null}
      width="90vw"
      centered
      // antd 6 废弃 maskClosable，等价写法是 mask.closable（与 AppShell 的拦截框同口径）
      mask={{ closable: true }}
      // 关闭即销毁内容：iframe 卸载 → 预览里的脚本与动画随之停止（不带它的话隐藏内容会一直留在 DOM 里）
      destroyOnHidden
      title={<span title={title}>{title}</span>}
      // 头部高度（约 56px）从 85vh 里扣掉 → 弹框总高 ≈85vh；居中后小窗口也不会向上溢出
      styles={{
        body: {
          padding: 0,
          height: "calc(85vh - 56px)",
          display: "flex",
          flexDirection: "column",
          minHeight: 0,
        },
      }}
    >
      <div className="widget-preview-toolbar">
        <span className="widget-preview-meta">{t("tools.previewChars", { n: chars ?? html.length })}</span>
        <span className="widget-preview-actions">
          <Button size="small" onClick={() => void copyHtml()}>
            {copyState === "done"
              ? t("tools.copied")
              : copyState === "fail"
                ? t("tools.copyFailed")
                : t("tools.copyHtml")}
          </Button>
          <Button size="small" onClick={() => setReloadSeq((n) => n + 1)}>
            {t("tools.reload")}
          </Button>
          <Segmented
            size="small"
            value={light ? "light" : "dark"}
            onChange={(v) => setLight(v === "light")}
            options={[
              { label: t("tools.bgLight"), value: "light" },
              { label: t("tools.bgDark"), value: "dark" },
            ]}
          />
        </span>
      </div>
      <div className="widget-preview-stage" style={{ background: light ? "#fff" : token.colorBgLayout }}>
        <iframe
          key={reloadSeq}
          className="widget-preview-frame"
          title={title}
          srcDoc={html}
          sandbox="allow-scripts"
          // color-scheme 让「无自带背景」的文档画布跟随底色档；文档自带背景时保持原样（不改写模型给的 HTML）
          style={{ colorScheme: light ? "light" : "dark" }}
        />
      </div>
    </Modal>
  );
}
