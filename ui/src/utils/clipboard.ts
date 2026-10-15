// 复制到剪贴板的**唯一入口**：全仓所有复制按钮都必须调 copyText，不得裸写 navigator.clipboard。
// （守护：`grep -rn "navigator\.clipboard" ui/src/ --exclude-dir=__tests__` 只应命中本文件与 ipc/clipboard.ts；
//   测试目录要提到这个字符串来构造用例，故排除在外。见 AGENTS.md 踩坑清单。）
//
// 三级链路，逐级降级，全部包在 try/catch 里：
//   1. 原生插件（Tauri 主路径）——打包版跑在 tauri://localhost 下，浏览器接口在该来源不可靠，
//      用户消息「复制」按钮点击无响应即由此而来（[docs/chat-and-interface](../../../docs/chat-and-interface.md)）。
//   2. 浏览器 navigator.clipboard.writeText——vite dev / 单元测试环境。
//   3. 隐藏 textarea + document.execCommand("copy")——末级兜底。
//
// 关键纪律：本文件修的正是旧代码的病根——旧写法 `navigator.clipboard?.writeText(t).then(...)`
// 在 clipboard 不存在时，`?.` 短路得到 undefined，紧接着的 .then 会**同步**抛 TypeError；
// 而空 `.catch(() => {})` 又把错误吃掉，于是界面上什么现象都没有。
// 所以：每级单独 try/catch、失败即降级、最终返回布尔由调用方决定如何提示用户，绝不静默。
import { writeClipboardText } from "../ipc/clipboard";

/** 末级兜底：隐藏 textarea + execCommand。返回是否写入成功。 */
function execCommandFallback(text: string): boolean {
  const active = document.activeElement as HTMLElement | null;
  const ta = document.createElement("textarea");
  ta.value = text;
  // 置于视口外而非 display:none——部分 WebView 对不可见元素不给 copy 权限。
  ta.setAttribute("readonly", "");
  ta.style.position = "fixed";
  ta.style.top = "-9999px";
  ta.style.opacity = "0";
  document.body.appendChild(ta);
  try {
    ta.select();
    ta.setSelectionRange(0, text.length);
    return document.execCommand("copy");
  } catch {
    return false;
  } finally {
    // 用完即删，且恢复原焦点元素，避免抢走聊天输入框的光标。
    ta.remove();
    if (active && typeof active.focus === "function") active.focus();
  }
}

/**
 * 复制文本。返回是否最终成功——调用方必须据此给用户可见反馈（成功态 / 失败提示），
 * 不得再吞掉返回值，否则失败会重新变成「点了没反应」。
 */
export async function copyText(text: string): Promise<boolean> {
  if (!text) return false;

  if (await writeClipboardText(text)) return true;

  // 浏览器接口：必须先判 clipboard 存在再写。`navigator.clipboard?.writeText(text)` 在
  // clipboard 缺失时 `?.` 短路得到 undefined，而 `await undefined` 会**当作成功**——
  // 于是既没写入又直接返回 true，把失败伪装成成功（与旧代码同类的病根，只是方向相反）。
  // 故此处显式取 clipboard 并判存在，且拒绝时一律落下去兜底。
  try {
    const clipboard = navigator.clipboard;
    if (clipboard) {
      await clipboard.writeText(text);
      return true;
    }
  } catch {
    // 存在但被拒（权限 / 用户手势要求），落下去兜底。
  }

  return execCommandFallback(text);
}
