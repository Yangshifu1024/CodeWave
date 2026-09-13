// 代码块复制按钮：markdown.ts 渲染时把代码原文 URI 编码进 data-code，这里提供
// document 级点击委托（bindCodeCopyDelegate）——一处覆盖聊天/抽屉/文件查看器/计划卡
// 全部 md 消费点；复制成功后按钮短暂反馈「已复制」（1.5s 复原，重复点击重置计时）。
/** 绑定 document 级点击委托处理代码块复制，返回解绑函数（各 md 消费点共用这一处绑定）。 */
export function bindCodeCopyDelegate(): () => void {
  const handler = (e: MouseEvent) => {
    const target = e.target as HTMLElement | null;
    const btn = target?.closest?.(".code-copy") as HTMLElement | null;
    if (!btn) return;
    const wrap = btn.closest(".code-wrap");
    if (!wrap) return;
    const encoded = btn.getAttribute("data-code");
    if (!encoded) return;
    let text = encoded;
    try {
      text = decodeURIComponent(encoded);
    } catch {
      /* data-code 不是合法编码串则原样使用 */
    }
    void navigator.clipboard
      ?.writeText(text)
      .then(() => flashCopied(btn))
      .catch(() => flashCopied(btn, true));
  };
  document.addEventListener("click", handler);
  return () => document.removeEventListener("click", handler);
}

/** 按钮反馈：hint 文案短暂切换为「已复制」（失败显「失败」）；重复点击重置计时。 */
function flashCopied(btn: HTMLElement, failed = false) {
  const hint = btn.querySelector(".code-copy-hint") as HTMLElement | null;
  if (!hint) return;
  if (btn.dataset.copyTimer) {
    window.clearTimeout(Number(btn.dataset.copyTimer));
  }
  btn.classList.add("code-copy-done");
  if (failed) btn.classList.add("code-copy-fail");
  hint.textContent = failed ? "失败" : "已复制";
  btn.dataset.copyTimer = String(
    window.setTimeout(() => {
      btn.classList.remove("code-copy-done", "code-copy-fail");
      hint.textContent = "Copy";
      delete btn.dataset.copyTimer;
    }, 1500),
  );
}
