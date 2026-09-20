// [docs/session-artifacts-and-files-tab](../../../../docs/session-artifacts-and-files-tab.md)：会话产物查看弹窗——markdown 渲染（mermaid/katex 升级）/ 图片 base64 预览 / 代码高亮
// [docs/office-and-pdf-support](../../../../docs/office-and-pdf-support.md)：扩展名分派扩到表格（.xlsx/.xlsm/.csv/.tsv）、Word（.docx）与 PDF（.pdf）
import { useCallback, useEffect, useRef, useState } from "react";
import { Alert, Image, Modal, Spin } from "antd";
import { useTranslation } from "react-i18next";
import { ipc } from "../../ipc/client";
import { renderMarkdown } from "../../utils/markdown";
import { upgradeDiagrams } from "../../utils/diagrams";
import CodeBlock from "../../components/CodeBlock";
import SheetView from "./SheetView";
import PdfView from "./PdfView";
import { maxCols, parseCsv, parseTsv, parseTsvFile } from "./parseTable";

/** 扩展名判断统一在 utils/fileKind（附件入口也用它）；这里再导出一次，保持既有引用点不变。 */
import { extOf, isImagePath, isMarkdownPath } from "../../utils/fileKind";
export { extOf, isImagePath, isMarkdownPath };

/** 表格：走结构化预览通道（后端解析 xlsx）。 */
const SHEET_EXTS = new Set(["xlsx", "xlsm"]);
/** 纯文本表格：没有后端解析器，读文本后前端自己切分。 */
const TEXT_TABLE_EXTS = new Set(["csv", "tsv"]);
const DOC_EXTS = new Set(["docx"]);

function mimeOf(path: string): string {
  const e = extOf(path);
  if (e === "svg") return "image/svg+xml";
  if (e === "jpg") return "image/jpeg";
  return `image/${e}`;
}

/** 预览方式：其余扩展名（含无扩展名）统一走代码/文本高亮。 */
type PreviewKind = "image" | "markdown" | "sheet" | "tableText" | "doc" | "pdf" | "code";

function previewKindOf(path: string): PreviewKind {
  const ext = extOf(path);
  if (isImagePath(path)) return "image";
  if (isMarkdownPath(path)) return "markdown";
  if (SHEET_EXTS.has(ext)) return "sheet";
  if (TEXT_TABLE_EXTS.has(ext)) return "tableText";
  if (DOC_EXTS.has(ext)) return "doc";
  if (ext === "pdf") return "pdf";
  return "code";
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

/** 表格视图已取到的数据（工作表清单 + 当前表的那一段行）。 */
interface SheetState {
  /** 工作表名；纯文本表格（csv/tsv）没有工作表概念，故为空数组 */
  sheets: string[];
  active: string;
  rows: string[][];
  /** 后端报的总行数（可能大于已取到的行数） */
  total: number;
  truncated: boolean;
  /** 正在取另一张表的数据 */
  busy: boolean;
}

type ViewerState =
  | { phase: "loading" }
  | { phase: "error"; error: string; tooLarge: boolean }
  | { phase: "image"; dataUrl: string }
  | { phase: "markdown"; html: string }
  | { phase: "code"; text: string }
  | { phase: "sheet"; sheet: SheetState }
  | { phase: "doc"; text: string; truncated: boolean }
  | { phase: "pdf"; scanned: boolean };

/** 错误文本：去掉 `Error: ` 前缀，保留后端给的 `E_XXX: 说明` 形态。 */
function errorText(e: unknown): string {
  return String(e).replace(/^Error[:\s]*/i, "");
}

/** 产物查看弹窗：按扩展名分派预览方式——图片走 base64、markdown 走渲染（含 mermaid/katex 升级）、
 *  表格走结构化预览、Word 走文本渲染、PDF 走网页渲染器，其余走代码高亮；
 *  路径变化即重读，卸载后丢弃迟到响应。 */
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
  const [st, setSt] = useState<ViewerState>({ phase: "loading" });
  const bodyRef = useRef<HTMLDivElement>(null);
  /** 表格取数的代号：换文件或换工作表后，旧请求的结果直接丢弃 */
  const sheetGen = useRef(0);
  const target = path ?? "";

  /** 取某张工作表的数据（首屏取第一张，下拉切换时再取一次）。 */
  const loadSheet = useCallback(async (sid: string, p: string, name: string) => {
    const gen = ++sheetGen.current;
    setSt((s) => (s.phase === "sheet" ? { ...s, sheet: { ...s.sheet, busy: true } } : s));
    try {
      const r = await ipc.previewDocument(sid, p, { sheet: name });
      if (sheetGen.current !== gen) return;
      const rows = parseTsv(String(r?.text ?? ""));
      setSt((s) =>
        s.phase === "sheet"
          ? {
              ...s,
              sheet: {
                ...s.sheet,
                active: name,
                rows,
                total: Number(r?.totalRows ?? rows.length),
                truncated: !!r?.truncated,
                busy: false,
              },
            }
          : s,
      );
    } catch (e) {
      if (sheetGen.current !== gen) return;
      setSt({ phase: "error", error: errorText(e), tooLarge: false });
    }
  }, []);

  useEffect(() => {
    if (!target || !sessionId) return;
    let cancelled = false;
    sheetGen.current++; // 让上一份文件未完成的表格取数失效
    setSt({ phase: "loading" });
    const sid = sessionId;
    const kind = previewKindOf(target);
    (async () => {
      try {
        if (kind === "image") {
          const r = await ipc.readWorkspaceFileBase64(sid, target);
          if (!cancelled) setSt({ phase: "image", dataUrl: `data:${mimeOf(target)};base64,${r.content}` });
          return;
        }
        if (kind === "markdown") {
          const r = await ipc.readWorkspaceFile(sid, target);
          if (!cancelled) setSt({ phase: "markdown", html: renderMarkdown(r.content) });
          return;
        }
        if (kind === "tableText") {
          const r = await ipc.readWorkspaceFile(sid, target);
          if (cancelled) return;
          const rows = extOf(target) === "csv" ? parseCsv(r.content) : parseTsvFile(r.content);
          setSt({
            phase: "sheet",
            sheet: { sheets: [], active: "", rows, total: rows.length, truncated: false, busy: false },
          });
          return;
        }
        if (kind === "sheet") {
          // 先取结构摘要拿工作表清单，再取第一张表的数据——一次全表拉回来会超单次上限
          const sum = await ipc.previewDocument(sid, target);
          if (cancelled) return;
          const names: string[] = ((sum?.sheets ?? []) as { name?: unknown }[]).map((s) => String(s?.name ?? ""));
          if (names.length === 0) {
            setSt({
              phase: "sheet",
              sheet: { sheets: [], active: "", rows: [], total: 0, truncated: false, busy: false },
            });
            return;
          }
          setSt({
            phase: "sheet",
            sheet: { sheets: names, active: names[0]!, rows: [], total: 0, truncated: false, busy: true },
          });
          await loadSheet(sid, target, names[0]!);
          return;
        }
        if (kind === "doc") {
          const r = await ipc.previewDocument(sid, target);
          if (!cancelled) setSt({ phase: "doc", text: String(r?.text ?? ""), truncated: !!r?.truncated });
          return;
        }
        if (kind === "pdf") {
          const r = await ipc.previewDocument(sid, target);
          if (cancelled) return;
          setSt({ phase: "pdf", scanned: !!r?.scanned });
          return;
        }
        const r = await ipc.readWorkspaceFile(sid, target);
        if (!cancelled) setSt({ phase: "code", text: r.content });
      } catch (e) {
        if (cancelled) return;
        const msg = errorText(e);
        setSt({ phase: "error", error: msg, tooLarge: /E_TOO_LARGE/.test(msg) });
      }
    })();
    return () => {
      cancelled = true;
    };
  }, [sessionId, target, loadSheet]);

  // Word 预览是「# 标题 + | 表格」形态的文本：既按 markdown 渲染，也就同样需要升级 mermaid/katex 占位符
  const html = st.phase === "markdown" ? st.html : st.phase === "doc" ? renderMarkdown(st.text) : "";

  // 定稿后再升级图表占位符（不涉及流式逻辑——弹窗内容一次性到达）
  useEffect(() => {
    if (html && bodyRef.current) void upgradeDiagrams(bodyRef.current);
  }, [html]);

  const name = target.split(/[\\/]/).filter(Boolean).pop() ?? target;
  const lang = LANG_MAP[extOf(target)] ?? "plaintext";
  // 表格视图列多，窄弹窗横向滚动太憋屈
  const wide = st.phase === "sheet";

  return (
    <Modal
      open={!!target}
      onCancel={onClose}
      footer={null}
      width={wide ? 1100 : 760}
      title={<span title={target}>{name}</span>}
      styles={{ body: { maxHeight: "70vh", overflow: "auto", paddingTop: 12 } }}
    >
      {st.phase === "loading" ? (
        <div style={{ textAlign: "center", padding: 32 }}>
          <Spin />
        </div>
      ) : st.phase === "error" ? (
        <Alert
          type="warning"
          showIcon
          message={st.tooLarge ? t("files.pdfTooLarge") : t("files.cannotPreview")}
          description={st.error}
        />
      ) : st.phase === "image" ? (
        <div style={{ textAlign: "center" }}>
          <Image src={st.dataUrl} alt={name} style={{ maxWidth: "100%" }} />
        </div>
      ) : st.phase === "sheet" ? (
        <SheetView
          sheets={st.sheet.sheets}
          active={st.sheet.active}
          rows={st.sheet.rows}
          truncated={st.sheet.truncated}
          loading={st.sheet.busy}
          onSelectSheet={(n) => {
            if (sessionId) void loadSheet(sessionId, target, n);
          }}
          labels={{
            pick: t("files.sheetPick"),
            page: t("files.sheetPage", { from: 1, to: st.sheet.rows.length, total: st.sheet.total }),
            empty: t("files.sheetEmpty"),
            truncated: t("files.sheetTruncated", { cols: maxCols(st.sheet.rows) }),
          }}
        />
      ) : st.phase === "pdf" ? (
        st.scanned ? (
          <Alert type="info" showIcon message={t("files.pdfScanned")} />
        ) : sessionId ? (
          <PdfView
            sessionId={sessionId}
            path={target}
            labels={{
              failed: (error) => t("files.pdfRenderFailed", { error }),
              tooLarge: t("files.pdfTooLarge"),
            }}
          />
        ) : null
      ) : st.phase === "doc" ? (
        <div>
          <div style={{ color: "var(--ws-dim)", fontSize: 12, marginBottom: 8 }}>
            {t("files.docHint")}
            {st.truncated ? ` · ${t("files.docTruncated")}` : ""}
          </div>
          <div className="assistant">
            <div ref={bodyRef} className="md" dangerouslySetInnerHTML={{ __html: html }} />
          </div>
        </div>
      ) : st.phase === "markdown" ? (
        <div className="assistant">
          <div ref={bodyRef} className="md" dangerouslySetInnerHTML={{ __html: html }} />
        </div>
      ) : (
        <CodeBlock code={st.phase === "code" ? st.text : ""} language={lang} />
      )}
    </Modal>
  );
}
