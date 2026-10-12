import { act, cleanup, render, waitFor } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import PdfView from "../features/files/PdfView";

const mocks = vi.hoisted(() => ({ readFileChunk: vi.fn(), getDocument: vi.fn() }));
vi.mock("../ipc/client", () => ({ ipc: { readFileChunk: mocks.readFileChunk } }));
vi.mock("pdfjs-dist/legacy/build/pdf.mjs", () => ({ getDocument: mocks.getDocument, GlobalWorkerOptions: {} }));
vi.mock("pdfjs-dist/legacy/build/pdf.worker.min.mjs?url", () => ({ default: "/pdf.worker.mjs" }));

const labels = { failed: (error: string) => `Failed: ${error}`, tooLarge: "Too large" };
function deferred<T>() {
  let resolve!: (value: T) => void;
  let reject!: (reason: Error) => void;
  const promise = new Promise<T>((res, rej) => { resolve = res; reject = rej; });
  return { promise, resolve, reject };
}
function document() {
  const renderPage = vi.fn(() => ({ promise: Promise.resolve(), cancel: vi.fn() }));
  return { numPages: 1, getPage: vi.fn(async () => ({
    getViewport: () => ({ width: 100, height: 100 }), render: renderPage,
  })), renderPage };
}
function view(path = "one.pdf") {
  return <PdfView sessionId="s1" path={path} labels={labels} />;
}
beforeEach(() => {
  vi.resetAllMocks();
  mocks.readFileChunk.mockResolvedValue({ content: "JVBERg==", length: 4, eof: true });
});
afterEach(cleanup);

describe("PDF loading task ownership", () => {
  it("destroys the loading task after a loaded document is closed", async () => {
    const doc = document();
    const destroy = vi.fn(async () => {});
    mocks.getDocument.mockReturnValue({ promise: Promise.resolve(doc), destroy });
    const mounted = render(view());
    await waitFor(() => expect(doc.renderPage).toHaveBeenCalledOnce());
    mounted.unmount();
    expect(destroy).toHaveBeenCalledOnce();
  });

  it("aborts a pending load on close and ignores a late resolution", async () => {
    const pending = deferred<ReturnType<typeof document>>();
    const destroy = vi.fn(async () => {});
    mocks.getDocument.mockReturnValue({ promise: pending.promise, destroy });
    const mounted = render(view());
    await waitFor(() => expect(mocks.getDocument).toHaveBeenCalledOnce());
    mounted.unmount();
    expect(destroy).toHaveBeenCalledOnce();
    const doc = document();
    await act(async () => pending.resolve(doc));
    expect(doc.getPage).not.toHaveBeenCalled();
    expect(destroy).toHaveBeenCalledOnce();
  });

  it("stops reading chunks after close without creating a worker", async () => {
    const pending = deferred<{ content: string; length: number; eof: boolean }>();
    mocks.readFileChunk.mockReturnValue(pending.promise);
    const mounted = render(view());
    await waitFor(() => expect(mocks.readFileChunk).toHaveBeenCalledOnce());
    mounted.unmount();
    await act(async () => pending.resolve({ content: "JVBERg==", length: 4, eof: false }));
    expect(mocks.readFileChunk).toHaveBeenCalledOnce();
    expect(mocks.getDocument).not.toHaveBeenCalled();
  });

  it("destroys an old task when switching files, without replacing the new document", async () => {
    const old = deferred<ReturnType<typeof document>>();
    const oldDestroy = vi.fn(async () => {});
    const nextDestroy = vi.fn(async () => {});
    const next = document();
    mocks.getDocument.mockReturnValueOnce({ promise: old.promise, destroy: oldDestroy })
      .mockReturnValueOnce({ promise: Promise.resolve(next), destroy: nextDestroy });
    const mounted = render(view());
    await waitFor(() => expect(mocks.getDocument).toHaveBeenCalledOnce());
    mounted.rerender(view("two.pdf"));
    await waitFor(() => expect(next.renderPage).toHaveBeenCalledOnce());
    expect(oldDestroy).toHaveBeenCalledOnce();
    const stale = document();
    await act(async () => old.resolve(stale));
    expect(stale.getPage).not.toHaveBeenCalled();
    mounted.unmount();
    expect(nextDestroy).toHaveBeenCalledOnce();
  });

  it("cleans up a rejected load and reports its error", async () => {
    const destroy = vi.fn(async () => {});
    mocks.getDocument.mockImplementation(() => ({ promise: Promise.reject(new Error("Invalid PDF")), destroy }));
    const mounted = render(view());
    await waitFor(() => expect(mounted.getByText("Failed: Error: Invalid PDF")).toBeTruthy());
    expect(destroy).toHaveBeenCalledOnce();
    mounted.unmount();
    expect(destroy).toHaveBeenCalledOnce();
  });

  it("creates and destroys a new task when reopening the same file", async () => {
    const doc = document();
    const destroy = vi.fn(async () => {});
    mocks.getDocument.mockReturnValue({ promise: Promise.resolve(doc), destroy });
    const first = render(view());
    await waitFor(() => expect(doc.renderPage).toHaveBeenCalledOnce());
    first.unmount();
    const mounted = render(view());
    await waitFor(() => expect(doc.renderPage).toHaveBeenCalledTimes(2));
    expect(mocks.getDocument).toHaveBeenCalledTimes(2);
    mounted.unmount();
    expect(destroy).toHaveBeenCalledTimes(2);
  });
});
