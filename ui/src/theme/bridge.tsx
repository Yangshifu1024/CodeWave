// 主题桥：组件只消费 --ws-* token，取值唯一来源是 antd 主题（亮暗由 effectiveDark 驱动，见 App.tsx）
import { useEffect } from "react";
import { theme } from "antd";

/** 按 colorBgBase 亮度判定亮暗（[docs/rightbar-visual-batch](../../../docs/rightbar-visual-batch.md)）：侧栏分色最终设计值只在亮色模式生效，暗色回退自动值 */
function isDarkBase(colorBgBase: string): boolean {
  const hex = colorBgBase.replace("#", "");
  if (!/^[0-9a-fA-F]{6}$/.test(hex)) return false;
  const n = parseInt(hex, 16);
  return (0.299 * ((n >> 16) & 255) + 0.587 * ((n >> 8) & 255) + 0.114 * (n & 255)) < 128;
}

/** 主题桥组件：把 antd token 摊平写入 `<html>` 的 `--ws-*` CSS 变量，随主题变化自动重写；自身不渲染任何 DOM。
 *  意图：手写样式一律消费 CSS 变量而非硬编码色值，亮暗切换由 antd 算法统一驱动（单一样式事实源）。 */
export default function ThemeBridge() {
  const { token } = theme.useToken();

  useEffect(() => {
    const s = document.documentElement.style;
    s.setProperty("--ws-bg", token.colorBgLayout);
    s.setProperty("--ws-panel", token.colorBgContainer);
    // 侧栏分色（[docs/rightbar-visual-batch](../../../docs/rightbar-visual-batch.md)）：左栏导航 #ececee、中右栏 #f8f8f8（仅亮色生效；暗色回退默认容器/布局色，与改动前观感一致）
    s.setProperty("--ws-bg-nav", isDarkBase(token.colorBgBase) ? token.colorBgContainer : "#eef4f9");
    s.setProperty("--ws-bg-main", isDarkBase(token.colorBgBase) ? token.colorBgLayout : "#FFFFFF");
    s.setProperty("--ws-code-bg", token.colorFillQuaternary);
    s.setProperty("--ws-border", token.colorBorderSecondary);
    s.setProperty("--ws-hover", token.colorFillTertiary);
    s.setProperty("--ws-text-1", token.colorText);
    s.setProperty("--ws-text-2", token.colorTextSecondary);
    s.setProperty("--ws-dim", token.colorTextTertiary);
    // 基础字号（[docs/proxy-mode-title-invisible](../../../docs/proxy-mode-title-invisible.md)）：antd 6 给
    // `.ant-radio-group` 等容器设了 `font-size: 0`（消除 inline-block 空白间隙），挂在其中且未显式声明
    // font-size 的自定义内容会继承成 0 号字（DOM 里有文本、肉眼看不见）。需要还原字号的容器统一取此变量。
    s.setProperty("--ws-font-size", `${token.fontSize}px`);
    // 强调色取 colorPrimaryText（[docs/ask-ink-accent-and-composer-cover](../../../docs/ask-ink-accent-and-composer-cover.md)）：算法自适应的前景变体——亮色 = 近黑 primary 本身，
    // 暗色 = 提亮后的灰（colorPrimary #424242 作为文字/边框/流光在暗底上不可读）。
    // --ws-accent 的全部消费点都是前景用法（文字/图标/边框/流光/条形填充），没有任何一处拿它当背景画白字
    s.setProperty("--ws-accent", token.colorPrimaryText);
    s.setProperty("--ws-ok", token.colorSuccess);
    s.setProperty("--ws-warn", token.colorWarning);
    s.setProperty("--ws-err", token.colorError);
    s.setProperty("--ws-user-bubble", token.colorFillSecondary);
    // diff 行语义色：antd 标准 success/error 变体（亮暗自适应）；取代 app.css 中硬编码的 GitHub 暗色调色板
    s.setProperty("--ws-diff-add-bg", token.colorSuccessBg);
    s.setProperty("--ws-diff-add-text", token.colorSuccessText);
    s.setProperty("--ws-diff-del-bg", token.colorErrorBg);
    s.setProperty("--ws-diff-del-text", token.colorErrorText);
    // [docs/markdown-style-refresh] 主题感 hljs token 与 code-block 容器色：亮色继承 GitHub 浅色调色板
    // （保留现观感），暗色按 GitHub Dark 标准色板。markdown.ts 不切换 github.css，css 用 var() 覆盖即可。
    const dark = isDarkBase(token.colorBgBase);
    s.setProperty("--ws-hl-keyword", dark ? "#ff7b72" : "#d73a49");
    s.setProperty("--ws-hl-string", dark ? "#a5d6ff" : "#032f62");
    s.setProperty("--ws-hl-number", dark ? "#79c0ff" : "#005cc5");
    s.setProperty("--ws-hl-name", dark ? "#d2a8ff" : "#6f42c1");
    s.setProperty("--ws-hl-built", dark ? "#ffa657" : "#e36209");
    s.setProperty("--ws-hl-comment", dark ? "#8b949e" : "#6a737d");
    s.setProperty("--ws-hl-attr", dark ? "#a5d6ff" : "#032f62");
    s.setProperty("--ws-code-block-bg", dark ? "#0d1117" : "#f6f8fa");
    s.setProperty("--ws-code-block-border", dark ? "#30363d" : "#d0d7de");
    s.setProperty("--ws-code-block-text", dark ? "#e6edf3" : "#1f2328");
  }, [token]);

  return null;
}
