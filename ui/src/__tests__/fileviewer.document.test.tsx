// Office 与 PDF 预览（[docs/office-and-pdf-support](../../../docs/office-and-pdf-support.md)）：
// 表格视图渲染与工作表切换 / PDF 扫描件提示 / 非文档类型仍走原有通道（回归）。
import { describe, it, expect, vi, afterEach } from "vitest";
import { render, screen, fireEvent, waitFor, cleanup } from "@testing-library/react";
import "../i18n"; // 独立挂载 FileViewerModal 必须先初始化 i18next（文案全走 t()）
import FileViewerModal, { VIEWER_WIDTH_TABLE, VIEWER_WIDTH_TEXT } from "../features/files/FileViewerModal";
import { useRun } from "../stores/run";

const mocks = vi.hoisted(() => ({
  calls: [] as { cmd: string; args: unknown[] }[],
  preview: vi.fn(),
  readFile: vi.fn(),
  readBase64: vi.fn(),
}));

vi.mock("../ipc/client", () => ({
  ipc: {
    previewDocument: (...a: unknown[]) => {
      mocks.calls.push({ cmd: "preview_document", args: a });
      return mocks.preview(...a);
    },
    readWorkspaceFile: (...a: unknown[]) => {
      mocks.calls.push({ cmd: "read_workspace_file", args: a });
      return mocks.readFile(...a);
    },
    readWorkspaceFileBase64: (...a: unknown[]) => {
      mocks.calls.push({ cmd: "read_workspace_file_base64", args: a });
      return mocks.readBase64(...a);
    },
  },
}));

afterEach(() => {
  cleanup();
  mocks.calls.length = 0;
  mocks.preview.mockReset();
  mocks.readFile.mockReset();
  mocks.readBase64.mockReset();
  // zustand store 是模块级单例：清掉残留的 tab 运行态，避免跨用例串味
  useRun.setState((s) => {
    s.tabs = {};
    s.drafts = {};
  });
});

describe("表格预览", () => {
  it("拿到表格数据后渲染单元格，并显示截断提示", async () => {
    mocks.preview.mockImplementation(async (_sid: string, _p: string, opts?: { sheet?: string }) => {
      if (!opts?.sheet) {
        return { sheets: [{ name: "Sheet1", rows: 2, cols: 2 }, { name: "数据", rows: 1, cols: 1 }] };
      }
      if (opts.sheet === "Sheet1") {
        return { sheet: "Sheet1", totalRows: 2, totalCols: 99, truncated: true, text: "名称\t数量\n苹果\t3\n" };
      }
      return { sheet: "数据", totalRows: 1, totalCols: 1, truncated: false, text: "值\n" };
    });
    render(<FileViewerModal sessionId="s1" path="/ws/book.xlsx" onClose={() => {}} />);
    expect(await screen.findByText("苹果")).toBeTruthy();
    expect(screen.getByText("数量")).toBeTruthy();
    expect(screen.getByText(/已截断/)).toBeTruthy();
  });

  it("切换工作表后带上新的 sheet 参数再取一次数据", async () => {
    mocks.preview.mockImplementation(async (_sid: string, _p: string, opts?: { sheet?: string }) => {
      if (!opts?.sheet) {
        return { sheets: [{ name: "Sheet1", rows: 2, cols: 2 }, { name: "数据", rows: 1, cols: 1 }] };
      }
      if (opts.sheet === "Sheet1") return { sheet: "Sheet1", totalRows: 2, totalCols: 2, text: "名称\t数量\n苹果\t3\n" };
      return { sheet: "数据", totalRows: 1, totalCols: 1, text: "换表后的值\n" };
    });
    render(<FileViewerModal sessionId="s1" path="/ws/book.xlsx" onClose={() => {}} />);
    // 断言用文字而不是「A」：列头行也会渲染出 A/B/C，按单元格文字找才唯一
    expect(await screen.findByText("苹果")).toBeTruthy();

    // 展开下拉（antd 6.6：对 .ant-select 根元素 mouseDown）
    const select = document.querySelector(".ant-select") as HTMLElement;
    expect(select).toBeTruthy();
    fireEvent.mouseDown(select);
    const option = await waitFor(() => {
      const el = Array.from(document.querySelectorAll(".ant-select-item-option")).find((o) =>
        (o.textContent ?? "").includes("数据"),
      );
      expect(el).toBeTruthy();
      return el as HTMLElement;
    });
    fireEvent.click(option);

    await waitFor(() => expect(screen.getByText("换表后的值")).toBeTruthy());
    // 只取带 sheet 的那几次取数（第一次是不带参数的「拿工作表清单」调用）
    const withSheet = mocks.calls
      .filter((c) => c.cmd === "preview_document")
      .map((c) => (c.args[2] as { sheet?: string } | undefined)?.sheet)
      .filter((s) => s !== undefined);
    expect(withSheet).toEqual(["Sheet1", "数据"]);
  });

  it("空工作表显示空态文案", async () => {
    mocks.preview.mockImplementation(async (_sid: string, _p: string, opts?: { sheet?: string }) => {
      if (!opts?.sheet) return { sheets: [{ name: "Sheet1", rows: 0, cols: 0 }] };
      return { sheet: "Sheet1", totalRows: 0, totalCols: 0, text: "" };
    });
    render(<FileViewerModal sessionId="s1" path="/ws/empty.xlsx" onClose={() => {}} />);
    expect(await screen.findByText("这张工作表没有内容")).toBeTruthy();
  });
});

describe("PDF 与既有路径", () => {
  it("扫描件显示扫描提示，且不进入网页渲染器", async () => {
    mocks.preview.mockResolvedValue({ pages: 3, scanned: true, text: "" });
    render(<FileViewerModal sessionId="s1" path="/ws/scan.pdf" onClose={() => {}} />);
    expect(await screen.findByText(/扫描件/)).toBeTruthy();
    expect(document.querySelector("canvas")).toBeNull();
  });

  it("普通文本仍走文本通道，不触发文档预览", async () => {
    mocks.readFile.mockResolvedValue({ path: "/ws/a.txt", size: 5, content: "纯文本内容" });
    render(<FileViewerModal sessionId="s1" path="/ws/a.txt" onClose={() => {}} />);
    await waitFor(() => expect(mocks.readFile).toHaveBeenCalled());
    expect(mocks.calls.some((c) => c.cmd === "preview_document")).toBe(false);
    // CodeBlock 在 effect 里填内容，断言要等它落地；直接按 textContent 取，
    // 不依赖高亮后是否把文字包进 span（包了就会同时命中 pre 与内层元素）
    await waitFor(() => expect(document.querySelector(".code-block")?.textContent).toBe("纯文本内容"));
  });

  it("图片仍走 base64 通道", async () => {
    mocks.readBase64.mockResolvedValue({ path: "/ws/a.png", size: 4, content: "aGk=" });
    render(<FileViewerModal sessionId="s1" path="/ws/a.png" onClose={() => {}} />);
    await waitFor(() => expect(mocks.readBase64).toHaveBeenCalled());
    expect(mocks.calls.some((c) => c.cmd === "preview_document")).toBe(false);
  });
});

describe("文本编码提示", () => {
  it("GBK 解码的 csv：正常显示内容并说明是按 GBK 解的", async () => {
    mocks.readFile.mockResolvedValue({
      path: "/ws/表.csv", size: 20, encoding: "gbk", content: "月份,金额\n1月,120\n",
    });
    render(<FileViewerModal sessionId="s1" path="/ws/表.csv" onClose={() => {}} />);
    expect(await screen.findByText(/GBK 编码解码/)).toBeTruthy();
    // 内容进的是表格视图（按逗号切开了）
    expect(screen.getByText("月份")).toBeTruthy();
  });

  it("认不出编码的文本：给出可能乱码的警告，而不是默默显示乱码", async () => {
    mocks.readFile.mockResolvedValue({
      path: "/ws/a.txt", size: 8, encoding: "unknown", content: "\uFFFD\uFFFD",
    });
    render(<FileViewerModal sessionId="s1" path="/ws/a.txt" onClose={() => {}} />);
    expect(await screen.findByText(/不是 UTF-8 \/ GBK 编码/)).toBeTruthy();
  });

  it("普通 UTF-8 文本不出现编码提示（不打扰）", async () => {
    mocks.readFile.mockResolvedValue({ path: "/ws/a.txt", size: 5, encoding: "utf-8", content: "内容" });
    render(<FileViewerModal sessionId="s1" path="/ws/a.txt" onClose={() => {}} />);
    await waitFor(() => expect(mocks.readFile).toHaveBeenCalled());
    expect(document.body.textContent ?? "").not.toContain("GBK");
    expect(document.body.textContent ?? "").not.toContain("乱码");
  });
});

describe("markdown 预览（[docs/plan-modal-table-scroll](../../../docs/plan-modal-table-scroll.md)）", () => {
  const MD_WITH_TABLE = "# 计划\n\n| 项 | 处置 |\n| --- | --- |\n| a | 上提 domain |\n";

  it("宽表格被 .table-wrap 包裹（滚动容器真实存在，不再静默裁切）", async () => {
    mocks.readFile.mockResolvedValue({
      path: "/ws/plan.md", size: 40, encoding: "utf-8", content: MD_WITH_TABLE,
    });
    render(<FileViewerModal sessionId="s1" path="/ws/plan.md" onClose={() => {}} />);
    await waitFor(() => expect(document.querySelector(".viewer-md.md .table-wrap table")).toBeTruthy());
    // 内容不丢：被包裹的是结构，不是文字
    expect(document.querySelector(".viewer-md.md")?.textContent ?? "").toContain("上提 domain");
  });

  it("不再复用聊天区 .assistant 容器（那套有 860px 行长上限 + overflow-x:hidden）", async () => {
    mocks.readFile.mockResolvedValue({
      path: "/ws/plan.md", size: 40, encoding: "utf-8", content: MD_WITH_TABLE,
    });
    render(<FileViewerModal sessionId="s1" path="/ws/plan.md" onClose={() => {}} />);
    await waitFor(() => expect(document.querySelector(".viewer-md.md")).toBeTruthy());
    expect(document.querySelector(".assistant")).toBeNull();
  });

  it("弹窗宽度是响应式而非固定 760px（断言导出的宽度契约）", () => {
    // happy-dom 不做布局，且其 CSS 校验器不认 min() 这类函数值（DOM inline width 会被丢弃），
    // 所以这里断言宽度常量本身，而不是从 DOM 上读像素。
    expect(VIEWER_WIDTH_TEXT).toContain("vw");
    expect(VIEWER_WIDTH_TABLE).toContain("vw");
    expect(VIEWER_WIDTH_TEXT).not.toBe("760px");
    // 表格类必须比正文类更宽（取 min() 里的第一个 px 值）
    const px = (w: string) => Number(/min\((\d+)px/.exec(w)?.[1]);
    expect(px(VIEWER_WIDTH_TABLE)).toBeGreaterThan(px(VIEWER_WIDTH_TEXT));
  });

  it("表格类文件走 wide 分支（SheetView 出现，而正文类的 .viewer-md 不出现）", async () => {
    mocks.preview.mockImplementation(async (_sid: string, _p: string, opts?: { sheet?: string }) => {
      if (!opts?.sheet) return { sheets: [{ name: "Sheet1", rows: 1, cols: 1 }] };
      return { sheet: "Sheet1", totalRows: 1, totalCols: 1, text: "值\n" };
    });
    render(<FileViewerModal sessionId="s1" path="/ws/book.xlsx" onClose={() => {}} />);
    await waitFor(() => expect(screen.getByText("值")).toBeTruthy());
    // wide 由 phase === "sheet" 决定；这里钉住的是「表格类进 sheet 分支」这个真实行为
    expect(document.querySelector(".viewer-md.md")).toBeNull();
  });
});

// [docs/plan-modal-table-scroll]：预览弹框换容器后，mermaid/katex 的样式必须仍然可达——
// 那 8 条规则此前全挂 .assistant .md 前缀，换容器即失配（公式丢 pre-wrap、katex-display 丢滚动）。
// happy-dom 不加载 app.css，所以这里断言的是「占位符确实渲染进了预览容器」这个可达前提，
// 样式层叠由 markdown.style.test.ts 的 :is() 组成员断言兜底。
describe("预览弹框内的图表占位符可达性", () => {
  it("mermaid 围栏渲染成 .ws-diagram 占位并落在 .viewer-md.md 内", async () => {
    mocks.readFile.mockResolvedValue({
      path: "/ws/plan.md", size: 40, encoding: "utf-8",
      content: "# 计划\n\n```mermaid\ngraph TD\nA-->B\n```\n",
    });
    render(<FileViewerModal sessionId="s1" path="/ws/plan.md" onClose={() => {}} />);
    await waitFor(() => expect(document.querySelector(".viewer-md.md .ws-diagram")).toBeTruthy());
  });

  it("块级公式渲染成 .ws-math 占位并落在 .viewer-md.md 内", async () => {
    mocks.readFile.mockResolvedValue({
      path: "/ws/plan.md", size: 40, encoding: "utf-8",
      content: "# 计划\n\n$$\n\\\\int_0^1 x\\\\,dx\n$$\n",
    });
    render(<FileViewerModal sessionId="s1" path="/ws/plan.md" onClose={() => {}} />);
    await waitFor(() => expect(document.querySelector(".viewer-md.md .ws-math")).toBeTruthy());
  });
});
