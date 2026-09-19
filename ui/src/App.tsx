import { useEffect, useMemo, useState } from "react";
import { App as AntApp, ConfigProvider, theme } from "antd";
import zhCN from "antd/locale/zh_CN";
import enUS from "antd/locale/en_US";
import { I18nextProvider } from "react-i18next";
import { i18n } from "./i18n";
import { useUi } from "./stores/ui";
import ThemeBridge from "./theme/bridge";
import AppShell from "./features/shell/AppShell";
import { bindCodeCopyDelegate } from "./utils/codecopy";
import { bindExternalLinkDelegate } from "./utils/linkhandler";


/** 根组件：i18n provider + antd ConfigProvider（主题三档：跟随系统/亮色/暗色）+ 应用外壳。 */
export default function App() {
  const language = useUi((s) => s.language);
  // 主题偏好（设置 → 外观）：system = 跟随系统亮暗；light/dark = 用户固定档。
  // store 初始化时同步读 localStorage，首帧即为持久化主题（无亮↔暗闪屏）。
  const themePref = useUi((s) => s.theme);
  const [osDark, setOsDark] = useState(() => window.matchMedia("(prefers-color-scheme: dark)").matches);

  useEffect(() => {
    const mq = window.matchMedia("(prefers-color-scheme: dark)");
    const fn = (e: MediaQueryListEvent) => setOsDark(e.matches);
    mq.addEventListener("change", fn);
    return () => mq.removeEventListener("change", fn);
  }, []);

  useEffect(() => {
    if (i18n.language !== language) void i18n.changeLanguage(language);
  }, [language]);

  // 代码块复制按钮：document 级点击委托，一处覆盖所有 md 渲染面（聊天/抽屉/文件查看器/计划卡）
  useEffect(() => bindCodeCopyDelegate(), []);

  // 外部链接打开：target="_blank" 在 Tauri WebView 无新窗口处理器，点击无效；
  // 拦截后走 open_url 用系统浏览器打开（同样 document 级委托覆盖所有 md 渲染面）
  useEffect(() => bindExternalLinkDelegate(), []);


  // 派生单源：所有依赖亮暗的 token 与 class 必须统一消费 effectiveDark，不得各自回查 osDark/themePref
  const effectiveDark = themePref === "system" ? osDark : themePref === "dark";

  useEffect(() => {
    // mermaid 图表按此 class 选主题（diagrams.ts）；ThemeBridge 的 --ws-* 变量随 antd token 自动更新
    document.documentElement.classList.toggle("dark", effectiveDark);
  }, [effectiveDark]);

  const themeCfg = useMemo(
    () => ({
      algorithm: effectiveDark ? theme.darkAlgorithm : theme.defaultAlgorithm,
      // 字体走双槽 token（[docs/custom-font-and-titlebar](../../docs/custom-font-and-titlebar.md)）：antd 组件不继承 body 字体，必须经 token 下发取值；
      // 取值在挂载前由 utils/fonts.ts 内联到 <html>，随设置即时变化。
      // 强调色墨化（[docs/ask-ink-accent-and-composer-cover](../../docs/ask-ink-accent-and-composer-cover.md)）：亮色近黑 / 暗色中灰（暗色 primary 仍配白字，中灰保对比）；
      // --ws-accent 经 ThemeBridge 跟随此值，全应用强调色统一为中性墨色
      token: {
        fontFamily: "var(--ws-font-sans)",
        fontFamilyCode: "var(--ws-font-mono)",
        colorPrimary: effectiveDark ? "#424242" : "#1f1f1f",
        // 聚焦外晕（controlOutline 2px 外圈）会在墨色边框外再画一圈浅灰，视觉上像双重描边；
        // 全局去掉后聚焦态收敛为单圈墨色边框（[docs/focus-ring-fix](../../docs/focus-ring-fix.md)）
        controlOutline: "transparent",
        // [docs/sidebar-collapse-animation-and-titlebar-blend](../../docs/sidebar-collapse-animation-and-titlebar-blend.md)：菜单选中项背景——默认选中底色以 colorPrimary（近黑墨）为基，
        // 会与 controlItemBgHover 层叠加成深灰块，压过条目本身的彩色标题；
        // 底色归零后「选中」只比 hover 深一档（剩余填充层继续随主题自适应）
        colorPrimaryBgHover: "transparent",
        // [docs/dropdown-selected-fill](../../docs/dropdown-selected-fill.md)：静息选中背景仍源自 colorPrimaryBg（近黑 primary 下是
        // 中深灰块）——改用比 hover 深一档的中性填充（colorFillTertiary）；
        // 选中项上的 hover（上一行已归零）则再比选中深半档
        controlItemBgActive: effectiveDark ? "rgba(255,255,255,0.10)" : "rgba(0,0,0,0.08)",
        controlItemBgActiveHover: effectiveDark ? "rgba(255,255,255,0.14)" : "rgba(0,0,0,0.10)",
      },
    }),
    [effectiveDark],
  );

  return (
    <I18nextProvider i18n={i18n}>
      <ConfigProvider locale={language === "zh-CN" ? zhCN : enUS} theme={themeCfg}>
        <ThemeBridge />
        <AntApp>
          <AppShell />
        </AntApp>
      </ConfigProvider>
    </I18nextProvider>
  );
}
