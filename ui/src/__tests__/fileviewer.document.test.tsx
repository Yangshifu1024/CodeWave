// Office 与 PDF 预览（[docs/office-and-pdf-support](../../../docs/office-and-pdf-support.md)）：
// 表格视图渲染与工作表切换 / PDF 扫描件提示 / 非文档类型仍走原有通道（回归）。
import { describe, it, expect, vi, afterEach } from "vitest";
import { render, screen, fireEvent, waitFor, cleanup } from "@testing-library/react";
import "../i18n"; // 独立挂载 FileViewerModal 必须先初始化 i18next（文案全走 t()）
import FileViewerModal from "../features/files/FileViewerModal";
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
