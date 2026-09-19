// Composer 图片附件（[docs/fence-hardening-and-powershell-ast](../../../../docs/fence-hardening-and-powershell-ast.md) 重构）：文件选择、粘贴与历史召回共用同一条
// 校验链——仅图片类型、单张 5MB、最多 4 张、base64 总量 20MB 预算。
// 附件列表本体存 run store 每 Tab 桶（TabRunState.draft.images，按 Tab 隔离）；本 hook 只持校验链与入口，列表经 opts 注入。
import { useCallback, useRef } from "react";
import type { PendingImage } from "../../stores/run.types";

export type { PendingImage };

const IMAGE_MAX_BYTES = 5 * 1024 * 1024; // 单张上限 5MB
const IMAGE_MAX_COUNT = 4;
const IMAGE_MAX_TOTAL_BASE64 = 20 * 1024 * 1024; // base64 总量预算

/** Composer 附件 hook：图片待发列表 + 校验链（类型/单张/张数/总量），文件选择与粘贴共用；
 *  另提供历史图片 → 待发附件的防御性还原（recalledImages）。 */
export function useComposerAttachments(opts: {
  t: (key: string, opts?: any) => string;
  message: { warning: (content: string) => void };
  images: PendingImage[];
  setImages: React.Dispatch<React.SetStateAction<PendingImage[]>>;
}) {
  const { t, message, images, setImages } = opts;
  const fileRef = useRef<HTMLInputElement>(null);

  // 用 useCallback 固定标识：本函数会被 Composer / useComposerEvents 放进 effect 依赖，
  // 普通函数声明每次渲染都是新引用，会把这些「绑定一次」的监听反复重绑。
  const recalledImages = useCallback((imgs: { mediaType: string; data: string }[]): PendingImage[] => {
    const out: PendingImage[] = [];
    let total = 0;
    for (const im of imgs) {
      if (out.length >= IMAGE_MAX_COUNT) break;
      if (total + im.data.length > IMAGE_MAX_TOTAL_BASE64) break;
      total += im.data.length;
      out.push({
        id: crypto.randomUUID(),
        name: t("composer.recalledImage", { n: out.length + 1 }),
        mime: im.mediaType,
        data: im.data,
        dataUrl: `data:${im.mediaType};base64,${im.data}`,
      });
    }
    return out;
  }, [t]);

  // 附件校验链核心：文件选择与粘贴两个入口共用（行为与提示语一致）
  async function addImageFiles(files: File[]) {
    if (!files.length) return;
    const next = [...images];
    for (const f of files) {
      if (!f.type.startsWith("image/")) {
        message.warning(t("composer.onlyImages"));
        continue;
      }
      if (f.size > IMAGE_MAX_BYTES) {
        message.warning(t("composer.imageTooLarge", { name: f.name }));
        continue;
      }
      if (next.length >= IMAGE_MAX_COUNT) {
        message.warning(t("composer.tooManyImages"));
        break;
      }
      const dataUrl = await new Promise<string>((resolve, reject) => {
        const r = new FileReader();
        r.onload = () => resolve(r.result as string);
        r.onerror = () => reject(r.error);
        r.readAsDataURL(f);
      }).catch(() => null);
      if (!dataUrl) continue;
      const base64 = dataUrl.slice(dataUrl.indexOf(",") + 1);
      next.push({ id: crypto.randomUUID(), name: f.name, mime: f.type, data: base64, dataUrl });
    }
    const total = next.reduce((acc, b) => acc + b.data.length, 0);
    if (total > IMAGE_MAX_TOTAL_BASE64) {
      message.warning(t("composer.imageTotalTooLarge"));
      return;
    }
    setImages(next);
  }

  function addFiles(files: FileList | null) {
    if (!files?.length) return;
    void addImageFiles(Array.from(files));
  }

  // 粘贴：图片直接附件化；非图片文件提示改用 @ 引用；纯文本（无 File）不拦截、走默认粘贴
  function onPaste(e: React.ClipboardEvent) {
    const dt = e.clipboardData;
    // 双通道（files + items）扫描后按 name|size|type 去重：WKWebView 某些情况下会在两个通道给出同一文件
    const seen = new Set<string>();
    const files: File[] = [];
    for (const f of [...Array.from(dt.files), ...Array.from(dt.items).map((it) => (it.kind === "file" ? it.getAsFile() : null))]) {
      if (!f) continue;
      const key = `${f.name}|${f.size}|${f.type}`;
      if (seen.has(key)) continue;
      seen.add(key);
      files.push(f);
    }
    if (files.length === 0) return; // 纯文本：不 preventDefault，走默认粘贴
    e.preventDefault(); // 有文件：阻止文件名/路径文本被插入输入框
    const imgs = files.filter((f) => f.type.startsWith("image/"));
    if (imgs.length > 0) void addImageFiles(imgs);
    if (imgs.length < files.length) message.warning(t("composer.pasteFilesHint")); // 非图片部分提示一次
  }

  return { images, setImages, fileRef, recalledImages, addFiles, onPaste };
}
