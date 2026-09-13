// 全局 event 订阅：注册列表直接取自调用方传入的 handler 键名（C1 修复：
// 旧实现硬编码 13 个 event 名，与 handler 清单脱节，导致 11 类事件静默失联）
import { listen, type UnlistenFn } from "@tauri-apps/api/event";

/** 事件 handler 表：键 = 后端事件名（27 键事件面，键名受契约测试守护不可增删），值 = 对应 payload 处理函数 */
export interface Handlers {
  [event: string]: (payload: any) => void;
}

/** 按键逐一 listen 订阅全部事件，返回解绑函数数组（挂载时绑定、卸载时逐一调用解绑）。 */
export async function bindEvents(handlers: Handlers): Promise<UnlistenFn[]> {
  const unlistens: UnlistenFn[] = [];
  for (const name of Object.keys(handlers)) {
    unlistens.push(await listen(name, (e) => handlers[name](e.payload)));
  }
  return unlistens;
}
