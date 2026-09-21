// Composer 全局窗口事件（[docs/fence-hardening-and-powershell-ast](../../../../docs/fence-hardening-and-powershell-ast.md) 重构）：三个监听契约属于应用行为面——
// ws:focus-composer（Cmd/Ctrl+L，由 AppShell 派发）、ws:composer-fill（用户消息「修改」，由 ChatMessages 派发）、
// ws:composer-insert（技能详情「使用」，由右栏 SkillDetailModal 派发，[docs/slash-skills-and-dollar-agents](../../../../docs/slash-skills-and-dollar-agents.md)）；事件名勿改。
// [docs/composer-file-ref-chips](../../../../docs/composer-file-ref-chips.md)：引用 chip 不在正文里，fill 的「覆盖」语义因此必须显式带上 refs
// （否则上一份草稿的引用会被悄悄带进改后的消息）；insert 是追加语义，保留现有引用不动。
import { useEffect } from "react";
import type { PendingImage } from "../../stores/run.types";
import { recoverRefs } from "./composerRefs";

/** Composer 全局事件 hook：订阅 ws:focus-composer（聚焦输入框并移光标到末尾）、
 *  ws:composer-fill（外部文本+附件覆盖草稿并聚焦）与 ws:composer-insert（外部文本追加草稿并聚焦，
 *  空格分隔防粘连），均为 window 级自定义事件，卸载时解绑。 */
export function useComposerEvents(opts: {
  taRef: React.MutableRefObject<any>;
  setText: React.Dispatch<React.SetStateAction<string>>;
  setImages: React.Dispatch<React.SetStateAction<PendingImage[]>>;
  setRefs: React.Dispatch<React.SetStateAction<string[]>>;
  recalledImages: (imgs: { mediaType: string; data: string }[]) => PendingImage[];
}) {
  const { taRef, setText, setImages, setRefs, recalledImages } = opts;

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
  }, [taRef]);

  // 用户消息「修改」回填（ChatMessages 经 ws:composer-fill 派发）：文本 + 引用 + 附件覆盖草稿并聚焦到末尾；不自动发送
  useEffect(() => {
    const onFill = (e: Event) => {
      const detail = (e as CustomEvent<{ text?: string; images?: { mediaType: string; data: string }[] }>).detail;
      if (!detail?.text) return;
      // 被「修改」的消息正文是发送时合成过的（末尾带 `@路径`）：抽回引用 chip（门禁式解析，手打形态不动），
      // 同时把草稿里残留的旧引用一并覆盖掉——fill 是覆盖语义，chip 不在正文里，光 setText 盖不住
      const parsed = recoverRefs(detail.text);
      setText(parsed.text);
      setRefs(parsed.refs);
      // 附件随文本一并覆盖（带图消息回显缩略图）——与队列「编辑」同一还原链；
      // 有意分歧：无图时清空现有附件（「修改」= 覆盖语义），队列编辑无图时保留旧附件
      setImages(recalledImages(detail.images ?? []));
      const el = taRef.current?.resizableTextArea?.textArea ?? taRef.current;
      el?.focus?.();
      el?.setSelectionRange?.(parsed.text.length, parsed.text.length);
    };
    window.addEventListener("ws:composer-fill", onFill);
    return () => window.removeEventListener("ws:composer-fill", onFill);
  }, [taRef, setText, setImages, setRefs, recalledImages]);

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
  }, [taRef, setText]);
}
