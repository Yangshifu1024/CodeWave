// [docs/session-artifacts-and-files-tab](../../../../docs/session-artifacts-and-files-tab.md)：会话产物查看弹窗——markdown 渲染（mermaid/katex 升级）/ 图片 base64 预览 / 代码高亮
import { useEffect, useRef, useState } from "react";
import { Alert, Image, Modal, Spin } from "antd";
import { useTranslation } from "react-i18next";
import { ipc } from "../../ipc/client";
import { renderMarkdown } from "../../utils/markdown";
import { upgradeDiagrams } from "../../utils/diagrams";
import CodeBlock from "../../components/CodeBlock";

const IMAGE_EXTS = new Set(["png", "jpg", "jpeg", "gif", "webp", "svg", "bmp", "ico"]);
const MD_EXTS = new Set(["md", "markdown"]);

/** 取路径扩展名（小写；无扩展名返回空串）。 */
export function extOf(path: string): string {
  const name = path.split(/[\\/]/).pop() ?? path;
  const i = name.lastIndexOf(".");
  return i >= 0 ? name.slice(i + 1).toLowerCase() : "";
}
/** 是否图片扩展名（FilesPanel 图标选择也复用）。 */
export function isImagePath(path: string): boolean {
  return IMAGE_EXTS.has(extOf(path));
}
/** 是否 markdown 扩展名。 */
export function isMarkdownPath(path: string): boolean {
  return MD_EXTS.has(extOf(path));
}
function mimeOf(path: string): string {
  const e = extOf(path);
  if (e === "svg") return "image/svg+xml";
  if (e === "jpg") return "image/jpeg";
  return `image/${e}`;
}

// highlight.js 语言名映射（扩展名 -> hljs id）
const LANG_MAP: Record<string, string> = {
  ts: "typescript", tsx: "typescript", js: "javascript", jsx: "javascript",
  mjs: "javascript", cjs: "javascript", json: "json", rs: "rust", py: "python",
  go: "go", java: "java", c: "c", h: "c", cpp: "cpp", hpp: "cpp", cs: "csharp",
  rb: "ruby", sh: "bash", bash: "bash", zsh: "bash", fish: "bash", ps1: "powershell",
  yml: "yaml", yaml: "yaml", toml: "ini", ini: "ini", conf: "ini",
  html: "xml", xml: "xml", svg: "xml", css: "css", scss: "scss", less: "less",
  sql: "sql", md: "markdown", txt: "plaintext", log: "plaintext",
};

interface ViewerState {
  loading: boolean;
  error: string;
  html?: string;
  text?: string;
  dataUrl?: string;
}

/** 产物查看弹窗：按扩展名分派预览方式——图片走 base64、markdown 走渲染（含 mermaid/katex 升级）、
 *  其余走代码高亮；路径变化即重读，卸载后丢弃迟到响应。 */
export default function FileViewerModal({
  sessionId,
  path,
  onClose,
}: {
  sessionId: string | null;
  /** 要查看的文件路径（null = 关闭）；自 [docs/run-queue-and-ask-revamp](../../../../docs/run-queue-and-ask-revamp.md) 起接受任意路径，不再要求 SessionFileEntry */
  path: string | null;
  onClose: () => void;
}) {
  const { t } = useTranslation();
  const [st, setSt] = useState<ViewerState>({ loading: false, error: "" });
  const bodyRef = useRef<HTMLDivElement>(null);
  const target = path ?? "";

  useEffect(() => {
    if (!target || !sessionId) return;
    let cancelled = false;
    setSt({ loading: true, error: "" });
    (async () => {
      try {
        if (isImagePath(target)) {
          const r = await ipc.readWorkspaceFileBase64(sessionId, target);
          if (!cancelled) setSt({ loading: false, error: "", dataUrl: `data:${mimeOf(target)};base64,${r.content}` });
        } else {
          const r = await ipc.readWorkspaceFile(sessionId, target);
          if (cancelled) return;
          if (isMarkdownPath(target)) {
            setSt({ loading: false, error: "", html: renderMarkdown(r.content) });
          } else {
            setSt({ loading: false, error: "", text: r.content });
          }
        }
      } catch (e) {
        if (!cancelled) setSt({ loading: false, error: String(e).replace(/^Error[:\s]*/i, "") });
      }
    })();
    return () => {
      cancelled = true;
    };
  }, [sessionId, target]);

  // markdown 定稿后再升级 mermaid/katex 占位符（不涉及流式逻辑——弹窗内容一次性到达）
  useEffect(() => {
    if (st.html && bodyRef.current) void upgradeDiagrams(bodyRef.current);
  }, [st.html]);

  const name = target.split(/[\\/]/).filter(Boolean).pop() ?? target;
  const lang = LANG_MAP[extOf(target)] ?? "plaintext";

  return (
    <Modal
      open={!!target}
      onCancel={onClose}
      footer={null}
      width={760}
      title={<span title={target}>{name}</span>}
      styles={{ body: { maxHeight: "70vh", overflow: "auto", paddingTop: 12 } }}
    >
      {st.loading ? (
        <div style={{ textAlign: "center", padding: 32 }}>
          <Spin />
        </div>
      ) : st.error ? (
        <Alert type="warning" showIcon message={t("files.cannotPreview")} description={st.error} />
      ) : st.dataUrl ? (
        <div style={{ textAlign: "center" }}>
          <Image src={st.dataUrl} alt={name} style={{ maxWidth: "100%" }} />
        </div>
      ) : st.html !== undefined ? (
        <div className="assistant">
          <div ref={bodyRef} className="md" dangerouslySetInnerHTML={{ __html: st.html }} />
        </div>
      ) : (
        <CodeBlock code={st.text ?? ""} language={lang} />
      )}
    </Modal>
  );
}
