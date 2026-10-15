/**
 * 剪贴板写入（Tauri 插件封装，调用方只依赖本文件，不碰 Tauri 细节）。
 *
 * 组件不得直接 import @tauri-apps/api（见 ui/src/ipc/client.ts 头部约定），
 * 与 dragdrop.ts 同模式把 Tauri 细节收在 ipc/ 层。
 *
 * 为什么要走原生插件而不用浏览器接口：打包版前端跑在 tauri://localhost 自定义协议下，
 * 该来源下 navigator.clipboard 不可靠（用户消息「复制」按钮点击无响应即由此而来），
 * 故以插件为主路径（[docs/chat-and-interface](../../../docs/chat-and-interface.md)）。
 *
 * 运行环境不是 Tauri（浏览器里跑 vite dev、单元测试环境）时返回 false，
 * 由调用方决定是否继续走浏览器接口兜底——不在此处静默假装成功。
 */
import { writeText } from "@tauri-apps/plugin-clipboard-manager";

/** 原生插件写入；不在 Tauri 环境或写入失败一律返回 false，绝不抛给调用方。 */
export async function writeClipboardText(text: string): Promise<boolean> {
  if (!("__TAURI_INTERNALS__" in window)) return false;
  try {
    await writeText(text);
    return true;
  } catch (err) {
    // ACL 未授予（capabilities 缺 allow-write-text）等情况在这里收敛，不冒泡到 UI 事件回调。
    console.warn("原生剪贴板写入失败：", err);
    return false;
  }
}
