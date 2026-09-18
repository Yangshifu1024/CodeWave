// 显示态栏宽 hook：把 localStorage 里的记忆宽度按当前窗口宽度夹取
//（[docs/rightbar-info-refactor-and-subscription-quota](../../../../docs/rightbar-info-refactor-and-subscription-quota.md)）。
// AppShell（Sider/分隔条）、TopBar（左段背景带）、RightBar（CSS 变量）三处消费同一份决议，
// 保证顶栏分隔线与栏边界不脱节。窗口变窄只影响显示，不改记忆值。
import { useEffect, useState } from "react";
import { resolveDisplayWidths } from "../../utils/layout";
import { useUi } from "../../stores/ui";

function currentWindowWidth(): number {
  return typeof window === "undefined" ? 0 : window.innerWidth;
}

export function useDisplayWidths(): { nav: number; rightBar: number; windowWidth: number } {
  const navWidth = useUi((s) => s.navWidth);
  const rightBarWidth = useUi((s) => s.rightBarWidth);
  const [windowWidth, setWindowWidth] = useState(currentWindowWidth);

  useEffect(() => {
    const onResize = () => setWindowWidth(currentWindowWidth());
    window.addEventListener("resize", onResize);
    return () => window.removeEventListener("resize", onResize);
  }, []);

  return { ...resolveDisplayWidths(navWidth, rightBarWidth, windowWidth), windowWidth };
}
