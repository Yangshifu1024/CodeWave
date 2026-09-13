// Composer ↑↓ 历史消息召回（[docs/fence-hardening-and-powershell-ast](../../../../docs/fence-hardening-and-powershell-ast.md) 重构）：对当前会话的用户条目做浏览态
// （时间升序；↑ 翻向更旧、↓ 翻向更新），进入浏览态拍草稿快照、退出时还原，
// 图片还原走共享附件兜底（上限 4 张 / 20MB）。
import { useEffect, useRef, useState } from "react";
import { useRun } from "../../stores/run";
import { useSessions } from "../../stores/sessions";
import type { PendingImage } from "./useComposerAttachments";

/** Composer 历史召回 hook：提供 ↑↓ 浏览态管理（histIdx + 草稿快照 draftRef）与
 *  召回序列/应用/退出三个动作；文本与图片一起还原，退出未浏览过则只复位指针。 */
export function useComposerHistory(opts: {
  tabKey?: string;
  setText: React.Dispatch<React.SetStateAction<string>>;
  setImages: React.Dispatch<React.SetStateAction<PendingImage[]>>;
  recalledImages: (imgs: { mediaType: string; data: string }[]) => PendingImage[];
}) {
  const { tabKey, setText, setImages, recalledImages } = opts;
  // ↑↓ 历史召回浏览态：histIdx=null 表示未在浏览；否则为升序召回历史的下标（0 = 最旧、length-1 = 最新；↑ 翻向更旧、↓ 翻向更新）
  const [histIdx, setHistIdx] = useState<number | null>(null);
  // 进入浏览态时的草稿快照（退出时还原）
  const draftRef = useRef<{ text: string; images: PendingImage[] } | null>(null);

  // 会话/Tab 切换：退出历史浏览态；草稿暂不按 Tab 隔离——维持现状
  useEffect(() => {
    setHistIdx(null);
    draftRef.current = null;
  }, [tabKey]);

  // ---------- 历史消息召回（↑↓ 浏览） ----------

  // 召回序列：当前会话的用户条目（按时间升序）；keydown 时经 getState 现取，避免闭包过期
  function recallHistory(): { text: string; images: { mediaType: string; data: string }[] }[] {
    const key = useSessions.getState().activeKey ?? "";
    const items = useRun.getState().tabs[key]?.items ?? [];
    const hist: { text: string; images: { mediaType: string; data: string }[] }[] = [];
    for (const it of items) {
      if (it.kind === "user") hist.push({ text: it.text, images: it.images ?? [] });
    }
    return hist;
  }

  // 把一条历史应用到输入框（文本 + 图片还原 + 指针更新）；进入浏览与翻页共用
  function applyRecall(entry: { text: string; images: { mediaType: string; data: string }[] }, idx: number) {
    setText(entry.text);
    setImages(recalledImages(entry.images));
    setHistIdx(idx);
  }

  // 退出浏览态：还原进入前的草稿快照（无快照则只复位指针）
  function exitRecall() {
    const draft = draftRef.current;
    setHistIdx(null);
    draftRef.current = null;
    if (draft) {
      setText(draft.text);
      setImages(draft.images);
    }
  }

  return { histIdx, setHistIdx, draftRef, recallHistory, applyRecall, exitRecall };
}
