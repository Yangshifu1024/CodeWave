// 聊天等 markdown 消费点的外部链接打开：markdown.ts 渲染的 <a> 带 target="_blank"，
// 但 Tauri WebView 没有「新窗口打开」处理器——点击既不开新窗口也不调系统浏览器（点击无效）。
// 这里提供 document 级点击委托（bindExternalLinkDelegate），与 codecopy.ts 同模式：
// 一处覆盖聊天/抽屉/文件查看器/计划卡全部 md 消费点；http/https 链接拦截默认行为，
// 改走后端 open_url（协议校验在 Rust 侧 validate_open_url）用系统默认浏览器打开；
// 其他协议（file: 等）不拦截，交回默认行为。
import { ipc } from "../ipc/client";

/** 判定 href 是否可交由系统浏览器打开：http/https 协议（与后端 validate_open_url 对齐）。 */
export function isHttpUrl(href: string): boolean {
  try {
    const u = new URL(href);
    return u.protocol === "http:" || u.protocol === "https:";
  } catch {
    return false;
  }
}

/** 绑定 document 级点击委托处理外部链接打开，返回解绑函数（各 md 消费点共用这一处绑定）。 */
export function bindExternalLinkDelegate(): () => void {
  const handler = (e: MouseEvent) => {
    // 只响应主键点击；带修饰键（Ctrl/Cmd 新窗口等）交给默认行为
    if (e.defaultPrevented || e.button !== 0 || e.ctrlKey || e.metaKey || e.shiftKey || e.altKey) {
      return;
    }
    const target = e.target as HTMLElement | null;
    const anchor = target?.closest?.("a[href]") as HTMLAnchorElement | null;
    if (!anchor) return;
    const href = anchor.getAttribute("href");
    if (!href || !isHttpUrl(href)) return;
    e.preventDefault();
    void ipc.openUrl(href).catch(() => {
      // 打开失败（后端校验拒绝/进程失败）不提示，保持点击无副作用即可
    });
  };
  document.addEventListener("click", handler);
  return () => document.removeEventListener("click", handler);
}
