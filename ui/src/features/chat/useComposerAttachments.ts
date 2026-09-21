// Composer 附件与文件引用（[docs/fence-hardening-and-powershell-ast](../../../../docs/fence-hardening-and-powershell-ast.md) 重构 + [docs/office-and-pdf-support](../../../../docs/office-and-pdf-support.md) 扩展）。
//
// 两类东西，走两条完全不同的路：
//
// - **图片**：必须把内容送上 wire（模型看的是图），所以读成 base64 进待发附件列表。
//   校验链：仅图片类型、单张 5MB、最多 4 张、base64 总量 20MB 预算。
// - **其他文件**：原地引用，不复制副本——不再往正文里写路径，而是记进草稿的 `refs`（输入框里以 chip 展示，
//   发送前一刻才由 mergeRefs 合成 `@路径` 追加到正文末尾，模型需要时自己去读
//   （文本用 read、Office/PDF 用 read_document）。这类文件不走体积校验：
//   文件本身从不进对话，多大都不占上下文。见 [docs/composer-file-ref-chips](../../../../docs/composer-file-ref-chips.md)。
//
// 附件列表本体存 run store 每 Tab 桶（TabRunState.draft.images，按 Tab 隔离）；
// 本 hook 只持校验链与入口，列表经 opts 注入。
import { useCallback, useRef } from "react";
import { ipc } from "../../ipc/client";
import { isImagePath } from "../../utils/fileKind";
import type { PendingImage } from "../../stores/run.types";

export type { PendingImage };

const IMAGE_MAX_BYTES = 5 * 1024 * 1024; // 单张上限 5MB
const IMAGE_MAX_COUNT = 4;
const IMAGE_MAX_TOTAL_BASE64 = 20 * 1024 * 1024; // base64 总量预算

/** 项目外目录放行的用户决定（与 ExternalDirPrompt 的三按钮一致）。 */
export type DirDecision = "once" | "always" | "cancel";

/** 待发图片的入列项（正文与展示形式）。 */
interface ImageItem {
  name: string;
  mime: string;
  data: string;
  dataUrl: string;
}

/** Composer 附件 hook：图片待发列表 + 任意文件的引用入口 + 粘贴；校验链在两条路径上分开。 */
export function useComposerAttachments(opts: {
  t: (key: string, opts?: any) => string;
  message: { warning: (content: string) => void };
  images: PendingImage[];
  setImages: React.Dispatch<React.SetStateAction<PendingImage[]>>;
  /** 当前会话 id：读文件、判定边界、放行目录都要它 */
  sessionId: string | null;
  /** 把引用路径记进草稿的 refs（引用不再写进正文文本，见文件头说明） */
  onRefs: (refs: string[]) => void;
  /** 项目外目录的放行询问：返回用户的选择（调用方弹 ExternalDirPrompt 并等待） */
  askExternalDir: (dir: string) => Promise<DirDecision>;
}) {
  const { t, message, images, setImages, sessionId, onRefs, askExternalDir } = opts;
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

  /** 图片入列（选择与粘贴共用）：按张数与总量两道上限收口后一次性落库。 */
  async function pushImages(base: PendingImage[], items: ImageItem[]) {
    const next = [...base];
    for (const it of items) {
      if (next.length >= IMAGE_MAX_COUNT) {
        message.warning(t("composer.tooManyImages"));
        break;
      }
      next.push({ id: crypto.randomUUID(), ...it });
    }
    const total = next.reduce((acc, b) => acc + b.data.length, 0);
    if (total > IMAGE_MAX_TOTAL_BASE64) {
      message.warning(t("composer.imageTotalTooLarge"));
      return;
    }
    setImages(next);
  }

  // 粘贴入口：clipboardData 只给 File 对象（没有路径），故只处理图片
  async function addImageFiles(files: File[]) {
    if (!files.length) return;
    const accepted: ImageItem[] = [];
    for (const f of files) {
      if (!f.type.startsWith("image/")) {
        message.warning(t("composer.onlyImages"));
        continue;
      }
      if (f.size > IMAGE_MAX_BYTES) {
        message.warning(t("composer.imageTooLarge", { name: f.name }));
        continue;
      }
      const dataUrl = await new Promise<string>((resolve, reject) => {
        const r = new FileReader();
        r.onload = () => resolve(r.result as string);
        r.onerror = () => reject(r.error);
        r.readAsDataURL(f);
      }).catch(() => null);
      if (!dataUrl) continue;
      accepted.push({
        name: f.name,
        mime: f.type,
        data: dataUrl.slice(dataUrl.indexOf(",") + 1),
        dataUrl,
      });
    }
    await pushImages(images, accepted);
  }

  /**
   * 按路径添加文件（附件按钮与拖拽共用）：图片读成 base64 进附件，其余进草稿 refs（chip 展示）。
   * 遇到项目外路径时暂停等待用户决定，决定后重问一次判定——
   * 后端放行时会做规范化，引用写法以放行之后那次为准（否则同一文件会出现两种写法）。
   */
  async function addPaths(paths: string[]) {
    const list = paths.filter((p) => p.trim() !== "");
    if (!list.length) return;
    if (!sessionId) {
      message.warning(t("composer.fileRefHint"));
      return;
    }
    const refs: string[] = [];
    const imgs: ImageItem[] = [];
    for (const p of list) {
      try {
        let info = await ipc.checkExternalPath(sessionId, p);
        if (!info.inside) {
          const choice = await askExternalDir(info.dir);
          if (choice === "cancel") continue;
          await ipc.allowExternalDir(sessionId, info.dir, choice === "always");
          info = await ipc.checkExternalPath(sessionId, p);
        }
        if (isImagePath(p)) {
          const r = await ipc.readWorkspaceFileBase64(sessionId, p);
          if (r.size > IMAGE_MAX_BYTES) {
            message.warning(t("composer.imageTooLarge", { name: nameOf(p) }));
            continue;
          }
          const mime = mimeOfImage(p);
          imgs.push({ name: nameOf(p), mime, data: r.content, dataUrl: `data:${mime};base64,${r.content}` });
        } else {
          refs.push(info.ref);
        }
      } catch (e) {
        message.warning(String(e).replace(/^Error[:\s]*/i, ""));
      }
    }
    if (imgs.length) await pushImages(images, imgs);
    if (refs.length) onRefs(refs);
  }

  // 粘贴：图片直接附件化；非图片文件提示改用附件按钮或 @ 引用；纯文本（无 File）不拦截、走默认粘贴
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

  return { images, setImages, fileRef, recalledImages, addImageFiles, addPaths, onPaste };
}

/** 取路径最后一段作为显示名。 */
function nameOf(path: string): string {
  return path.split(/[\\/]/).filter(Boolean).pop() ?? path;
}

/** 图片路径 → MIME（与预览弹窗同口径，独立一份避免组件间循环引用）。 */
function mimeOfImage(path: string): string {
  const ext = path.split(".").pop()?.toLowerCase() ?? "";
  if (ext === "svg") return "image/svg+xml";
  if (ext === "jpg" || ext === "jpeg") return "image/jpeg";
  return `image/${ext}`;
}
