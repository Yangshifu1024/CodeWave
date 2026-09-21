// [docs/office-and-pdf-support](../../../../docs/office-and-pdf-support.md)：表格预览视图。
// 行窗口化（虚拟滚动）是手写的最小实现——仓库没有虚拟滚动依赖，也不为一个弹窗引入；
// 文案由调用方 FileViewerModal 按当前语言组装后传入，本组件只管渲染与交互。
import { useEffect, useLayoutEffect, useRef, useState } from "react";
import { Select, Spin } from "antd";
import { columnLabel, maxCols } from "./parseTable";

/** 行高与列宽固定：窗口化渲染必须先知道行的真实高度，否则滚动位置会跳、行会重叠。 */
const ROW_H = 26;
const CELL_W = 160;
/** 视口上下各多渲染几行，减少快速滚动时的露白。 */
const OVERSCAN = 8;
/** 表格滚动区高度（弹窗主体上限 70vh 以内），同时是窗口化数学里的视口高度。 */
const VIEW_H = 400;

/** 表格视图的文案包（已按当前语言组装）。 */
export interface SheetLabels {
  /** 工作表下拉前的说明文字 */
  pick: string;
  /** 当前窗口与总行数：第 from–to 行 · 共 total 行 */
  page: string;
  /** 空工作表的提示 */
  empty: string;
  /** 宽表被截断的提示 */
  truncated: string;
}

export default function SheetView({
  sheets,
  active,
   rows,
   truncated,
  loading,
  onSelectSheet,
  labels,
}: {
  /** 工作表名（多张表才显示下拉） */
  sheets: string[];
  /** 当前工作表名 */
  active: string;
  /** 已取到的那一段行数据 */
   rows: string[][];
   /** 是否被截断（宽表只取到了前若干列） */
  truncated: boolean;
  /** 正在取另一张表的数据 */
  loading: boolean;
  onSelectSheet: (name: string) => void;
  labels: SheetLabels;
}) {
  const boxRef = useRef<HTMLDivElement>(null);
  const [scrollTop, setScrollTop] = useState(0);
  const [viewH, setViewH] = useState(VIEW_H);

  const cols = maxCols(rows);
  // 有行列结构但全为空白（空工作表 / 只剩格式的表格）按「没有内容」处理
  const hasContent = rows.some((r) => r.some((c) => c.trim() !== ""));

  // 换工作表或换文件后行集合整体替换：滚动条拉回顶部，否则会停在新数据里不存在的位置
  useEffect(() => {
    setScrollTop(0);
    if (boxRef.current) boxRef.current.scrollTop = 0;
  }, [active, rows]);

  // 视口真实高度随窗口尺寸变化：量到多少用多少，量不到（测试环境）用常量兜底
  useLayoutEffect(() => {
    const h = boxRef.current?.clientHeight ?? 0;
    if (h > 0) setViewH(h);
  }, [rows.length, hasContent]);

  const first = Math.floor(scrollTop / ROW_H);
  const start = Math.max(0, first - OVERSCAN);
  const end = Math.min(rows.length, Math.ceil((scrollTop + viewH) / ROW_H) + OVERSCAN);

  return (
    <div>
      {sheets.length > 1 && (
        <div style={{ display: "flex", alignItems: "center", gap: 8, marginBottom: 8 }}>
          <span style={{ color: "var(--ws-dim)" }}>{labels.pick}</span>
          <Select
            size="small"
            value={active}
            style={{ minWidth: 160 }}
            aria-label={labels.pick}
            options={sheets.map((s) => ({ value: s, label: s }))}
            onChange={(v) => onSelectSheet(v)}
          />
        </div>
      )}
      {loading ? (
        <div style={{ textAlign: "center", padding: 32 }}>
          <Spin />
        </div>
      ) : !hasContent ? (
        <div style={{ color: "var(--ws-dim)", padding: "16px 0" }}>{labels.empty}</div>
      ) : (
        <div
          ref={boxRef}
          onScroll={(e) => setScrollTop(e.currentTarget.scrollTop)}
          style={{
            height: VIEW_H,
            overflow: "auto",
            border: "1px solid var(--ws-border)",
            background: "var(--ws-bg-main)",
          }}
        >
          {/* 外层按内容撑出完整宽高（滚动条长度 = 全部行数 × 行高），行本体绝对定位在窗口起点 */}
          <div style={{ width: cols * CELL_W, minWidth: "100%" }}>
            <div
              style={{
                position: "sticky",
                top: 0,
                zIndex: 1,
                display: "flex",
                background: "var(--ws-panel)",
                borderBottom: "1px solid var(--ws-border)",
              }}
            >
              {Array.from({ length: cols }, (_, c) => (
                <div
                  key={c}
                  style={{
                    width: CELL_W,
                    flex: "0 0 auto",
                    padding: "0 6px",
                    lineHeight: `${ROW_H}px`,
                    color: "var(--ws-dim)",
                    fontSize: 12,
                    overflow: "hidden",
                    whiteSpace: "nowrap",
                  }}
                >
                  {columnLabel(c)}
                </div>
              ))}
            </div>
            <div style={{ position: "relative", height: rows.length * ROW_H }}>
              <div style={{ position: "absolute", top: start * ROW_H, left: 0, width: "100%" }}>
                {rows.slice(start, end).map((r, i) => (
                  <div key={start + i} style={{ display: "flex", height: ROW_H }}>
                    {Array.from({ length: cols }, (_, c) => {
                      const cell = r[c] ?? "";
                      return (
                        // 单元格一律带 title：列宽固定 + 省略号，悬停才看得到长内容的全文
                        <div
                          key={c}
                          title={cell}
                          style={{
                            width: CELL_W,
                            flex: "0 0 auto",
                            padding: "0 6px",
                            lineHeight: `${ROW_H}px`,
                            overflow: "hidden",
                            textOverflow: "ellipsis",
                            whiteSpace: "nowrap",
                          }}
                        >
                          {cell}
                        </div>
                      );
                    })}
                  </div>
                ))}
              </div>
            </div>
          </div>
        </div>
      )}
      {!loading && hasContent && (
        <div style={{ display: "flex", gap: 12, marginTop: 8, color: "var(--ws-dim)", fontSize: 12 }}>
          <span>{labels.page}</span>
          {truncated && <span>{labels.truncated}</span>}
        </div>
      )}
    </div>
  );
}
