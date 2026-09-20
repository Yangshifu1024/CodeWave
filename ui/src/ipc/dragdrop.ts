/**
 * 系统拖入窗口的文件（Tauri 事件通道的唯一入口）。
 *
 * 组件不得直接 import @tauri-apps/api（见 ui/src/ipc/client.ts 头部约定），
 * 所以把这段封装放这里：需要拖放的组件只依赖本文件，不碰 Tauri 细节。
 *
 * 为什么要用 Tauri 事件而不是网页的 ondrop：窗口开启系统拖放后，网页层的 drop 事件
 * 不再触发，只能听 Tauri 的事件；而且网页层拿到的是 File 对象（没有真实路径），
 * 本应用对文件是「原地引用」，必须拿到路径。
 */
import { getCurrentWebview } from "@tauri-apps/api/webview";

/** 拖放回调：进入/离开只用来画提示条，落下才真的处理文件。 */
export interface FileDropHandlers {
  onOver?: () => void;
  onLeave?: () => void;
  onDrop: (paths: string[]) => void;
}

/**
 * 订阅系统拖放事件；返回取消订阅函数。
 *
 * 运行环境不是 Tauri（浏览器里跑 vite dev、单元测试环境）时静默返回空函数：
 * 拖放是便利入口，不该因为它不可用就每次挂载都往控制台刷一条报错。
 * 真在 Tauri 里但订阅失败（权限缺失）才记一条告警。
 */
export async function listenFileDrop(h: FileDropHandlers): Promise<() => void> {
  if (!("__TAURI_INTERNALS__" in window)) return () => {};
  try {
    return await getCurrentWebview().onDragDropEvent((e) => {
      const p = e.payload;
      if (p.type === "over") h.onOver?.();
      else if (p.type === "drop") h.onDrop(p.paths);
      else h.onLeave?.();
    });
  } catch (err) {
    console.warn("订阅系统拖放事件失败，拖放入口本次不可用：", err);
    return () => {};
  }
}
