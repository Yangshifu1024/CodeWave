// Composer 全局窗口事件（[docs/fence-hardening-and-powershell-ast](../../../../docs/fence-hardening-and-powershell-ast.md) 重构）：三个监听契约属于应用行为面——
// ws:focus-composer（Cmd/Ctrl+L，由 AppShell 派发）、ws:composer-fill（用户消息「修改」，由 ChatMessages 派发）、
// ws:composer-insert（技能详情「使用」，由右栏 SkillDetailModal 派发，[docs/slash-skills-and-dollar-agents](../../../../docs/slash-skills-and-dollar-agents.md)）；事件名勿改。
import { useEffect } from "react";

/** Composer 全局事件 hook：订阅 ws:focus-composer（聚焦输入框并移光标到末尾）、
 *  ws:composer-fill（外部文本覆盖草稿并聚焦）与 ws:composer-insert（外部文本追加草稿并聚焦，
 *  空格分隔防粘连），均为 window 级自定义事件，卸载时解绑。 */
export function useComposerEvents(opts: {
  taRef: React.MutableRefObject<any>;
  setText: React.Dispatch<React.SetStateAction<string>>;
}) {
  const { taRef, setText } = opts;

  // Cmd/Ctrl+L（AppShell 快捷键）：聚焦输入框并把光标移到末尾（继续写草稿而非覆盖）
  useEffect(() => {
    const onFocus = () => {
      const el = taRef.current?.resizableTextArea?.textArea ?? taRef.current;
      if (!el?.focus) return;
      el.focus();
      const end = el.value?.length ?? 0;
      el.setSelectionRange?.(end, end);
    };
    window.addEventListener("ws:focus-composer", onFocus);
    return () => window.removeEventListener("ws:focus-composer", onFocus);
  }, []);

  // 用户消息「修改」回填（ChatMessages 经 ws:composer-fill 派发）：文本覆盖草稿并聚焦到末尾；不自动发送
  useEffect(() => {
    const onFill = (e: Event) => {
      const detail = (e as CustomEvent<{ text?: string }>).detail;
      if (!detail?.text) return;
      setText(detail.text);
      const el = taRef.current?.resizableTextArea?.textArea ?? taRef.current;
      el?.focus?.();
      el?.setSelectionRange?.(detail.text.length, detail.text.length);
    };
    window.addEventListener("ws:composer-fill", onFill);
    return () => window.removeEventListener("ws:composer-fill", onFill);
  }, []);

  // 技能详情「使用」（右栏 SkillDetailModal 经 ws:composer-insert 派发）：文本以空格分隔追加草稿
  // （与 + 菜单 insertTrigger 同语义，保留既有草稿）并聚焦到末尾；不自动发送
  useEffect(() => {
    const onInsert = (e: Event) => {
      const detail = (e as CustomEvent<{ text?: string }>).detail;
      if (!detail?.text) return;
      setText((v) =>
        v === "" || v.endsWith(" ") || v.endsWith("\n") ? v + detail.text : v + " " + detail.text,
      );
      const el = taRef.current?.resizableTextArea?.textArea ?? taRef.current;
      el?.focus?.();
    };
    window.addEventListener("ws:composer-insert", onInsert);
    return () => window.removeEventListener("ws:composer-insert", onInsert);
  }, []);
}
