// [docs/office-and-pdf-support](../../../../docs/office-and-pdf-support.md)：PDF 视图。
// 用 pdfjs-dist 在网页里渲染；字节走 read_file_chunk 分片取（大文件不能一次性塞进一条 IPC 消息）。
// pdfjs 与它的 worker 都走动态 import：不打开 PDF 就不加载这份几百 KB 的解析器。
import { useEffect, useRef, useState } from "react";
import { Alert, Button, Spin } from "antd";
import { ipc } from "../../ipc/client";

type PdfDoc = import("pdfjs-dist").PDFDocumentProxy;

/** 已按当前语言组装好的文案（渲染失败要带具体原因）。 */
export interface PdfLabels {
  failed: (error: string) => string;
  tooLarge: string;
}

/** base64 → 字节（后端分片通道给的是 base64）。 */
function base64ToBytes(b64: string): Uint8Array {
  const bin = atob(b64);
  const out = new Uint8Array(bin.length);
  for (let i = 0; i < bin.length; i++) out[i] = bin.charCodeAt(i);
  return out;
}

/** 放大倍率：1.5 在清晰度与显存之间够用，再高在长文档里会明显变慢。 */
const SCALE = 1.5;

export default function PdfView({
  sessionId,
  path,
  labels,
}: {
  sessionId: string;
  path: string;
  labels: PdfLabels;
}) {
  const canvasRef = useRef<HTMLCanvasElement>(null);
  const [doc, setDoc] = useState<PdfDoc | null>(null);
  const [page, setPage] = useState(1);
  const [error, setError] = useState("");
  const [loading, setLoading] = useState(true);

  useEffect(() => {
    let cancelled = false;
    let loaded: PdfDoc | null = null;
    setLoading(true);
    setError("");
    setDoc(null);
    setPage(1);
    (async () => {
      const pdfjs = await import("pdfjs-dist");
      // worker 走 ?url：vite 会把 node_modules 里的 worker 脚本打进产物并给同源相对路径，
      // 运行时零下载（比用 jsDelivr 之类的 CDN 更符合本应用的本地优先约定）。
      const worker = await import("pdfjs-dist/build/pdf.worker.min.mjs?url");
      pdfjs.GlobalWorkerOptions.workerSrc = worker.default;
      const chunks: Uint8Array[] = [];
      let offset = 0;
      for (;;) {
        const c = await ipc.readFileChunk(sessionId, path, offset);
        chunks.push(base64ToBytes(c.content));
        offset += c.length;
        if (c.eof || c.length === 0) break;
      }
      const total = chunks.reduce((n, c) => n + c.length, 0);
      const data = new Uint8Array(total);
      let at = 0;
      for (const c of chunks) {
        data.set(c, at);
        at += c.length;
      }
      const d = await pdfjs.getDocument({ data }).promise;
      loaded = d;
      if (cancelled) {
        void d.destroy();
        return;
      }
      setDoc(d);
      setLoading(false);
    })().catch((e: unknown) => {
      if (cancelled) return;
      setError(String(e));
      setLoading(false);
    });
    return () => {
      cancelled = true;
      void loaded?.destroy();
    };
  }, [sessionId, path]);

  // 逐页渲染：整本文档一次画完在大文件上会卡住界面，所以只画当前页
  useEffect(() => {
    if (!doc) return;
    let cancelled = false;
    let task: { cancel: () => void } | null = null;
    (async () => {
      const p = await doc.getPage(page);
      const canvas = canvasRef.current;
      if (cancelled || !canvas) return;
      const viewport = p.getViewport({ scale: SCALE });
      canvas.width = Math.floor(viewport.width);
      canvas.height = Math.floor(viewport.height);
      const t = p.render({ canvas, viewport });
      task = t;
      await t.promise;
    })().catch((e: unknown) => {
      if (!cancelled) setError(String(e));
    });
    return () => {
      cancelled = true;
      task?.cancel();
    };
  }, [doc, page]);

  if (error) {
    const tooLarge = /E_TOO_LARGE|超过预览上限/.test(error);
    return <Alert type="warning" showIcon message={tooLarge ? labels.tooLarge : labels.failed(error)} />;
  }

  return (
    <div>
      <div
        style={{
          maxHeight: "60vh",
          overflow: "auto",
          textAlign: "center",
          padding: 8,
          border: "1px solid var(--ws-border)",
          background: "var(--ws-panel)",
        }}
      >
        {loading && <Spin />}
        <canvas ref={canvasRef} style={{ maxWidth: "100%" }} />
      </div>
      {doc && doc.numPages > 1 && (
        <div style={{ display: "flex", alignItems: "center", gap: 8, marginTop: 8, justifyContent: "center" }}>
          <Button size="small" disabled={page <= 1} onClick={() => setPage((p) => Math.max(1, p - 1))}>
            ‹
          </Button>
          <span style={{ color: "var(--ws-dim)" }}>{`${page} / ${doc.numPages}`}</span>
          <Button size="small" disabled={page >= doc.numPages} onClick={() => setPage((p) => p + 1)}>
            ›
          </Button>
        </div>
      )}
    </div>
  );
}
