// AboutModal tests: identity rendering (logo/name/version/slogan), lazy version load
// (success + failure placeholder), entry buttons call the right IPC commands, and
// action errors surface inline without closing the dialog.
import { describe, it, expect, vi, beforeEach, afterEach } from "vitest";
import { render, screen, fireEvent, waitFor, cleanup } from "@testing-library/react";
import { App } from "antd";
import "../i18n"; // i18n init (nothing triggers it when rendering the component directly; otherwise t() returns the raw key)
import AboutModal from "../features/panels/AboutModal";
import { useUi } from "../stores/ui";

const defaultImpl = async (cmd: string, _args?: unknown): Promise<unknown> => {
  switch (cmd) {
    case "app_version":
      return "0.2.0";
    case "open_data_dir":
      return null;
    case "open_url":
      return null;
    default:
      return null;
  }
};

const invokeMock = vi.fn(defaultImpl);

vi.mock("@tauri-apps/api/core", () => ({
  invoke: (cmd: string, args?: unknown) => invokeMock(cmd, args),
}));

function renderModal() {
  return render(
    <App>
      <AboutModal />
    </App>,
  );
}

beforeEach(() => {
  invokeMock.mockClear();
  invokeMock.mockImplementation(defaultImpl); // restore: later tests replace the impl
  useUi.setState({ aboutOpen: false });
});

afterEach(() => {
  cleanup();
  useUi.setState({ aboutOpen: false });
});

describe("AboutModal", () => {
  it("renders identity and lazy-loads the version", async () => {
    renderModal();
    expect(screen.getByText("CodeWave")).toBeTruthy();
    expect(screen.getByText("…")).toBeTruthy(); // placeholder before the version arrives
    await waitFor(() => expect(screen.getByText("0.2.0")).toBeTruthy());
    expect(invokeMock).toHaveBeenCalledWith("app_version", undefined);
  });

  it("degrades to a placeholder when the version command fails", async () => {
    invokeMock.mockImplementation(async (cmd: string) => {
      if (cmd === "app_version") throw new Error("boom");
      return null;
    });
    renderModal();
    await waitFor(() => expect(screen.getByText("?.?.?")).toBeTruthy());
    // Identity still renders around the failed version
    expect(screen.getByText("CodeWave")).toBeTruthy();
  });

  it("renders the commit short-sha appended after the version", async () => {
    invokeMock.mockImplementation(async (cmd: string): Promise<string | null> =>
      cmd === "app_version" ? "0.2.0 (a1b2c3d)" : null,
    );
    renderModal();
    await waitFor(() => expect(screen.getByText("0.2.0 (a1b2c3d)")).toBeTruthy());
  });

  it("renders the bare version without an empty sha placeholder", async () => {
    invokeMock.mockImplementation(async (cmd: string): Promise<string | null> =>
      cmd === "app_version" ? "0.2.0" : null,
    );
    renderModal();
    await waitFor(() => expect(screen.getByText("0.2.0")).toBeTruthy());
    expect(screen.queryByText(/\(\s*\)/)).toBeNull(); // 空括号不得出现
  });

  it("open-data-folder button calls open_data_dir and inline error shows on failure", async () => {
    renderModal();
    await waitFor(() => expect(screen.getByText("0.2.0")).toBeTruthy());
    fireEvent.click(screen.getByRole("button", { name: /数据目录/ }));
    await waitFor(() => expect(invokeMock).toHaveBeenCalledWith("open_data_dir", undefined));
    expect(useUi.getState().aboutOpen).toBe(false); // stays open, no auto-close

    invokeMock.mockImplementation(async (cmd: string) => {
      if (cmd === "open_data_dir") throw new Error("no file manager");
      return null;
    });
    fireEvent.click(screen.getByRole("button", { name: /数据目录/ }));
    await waitFor(() => expect(screen.getByText(/no file manager/)).toBeTruthy());
    expect(useUi.getState().aboutOpen).toBe(false); // still open after failure
  });

  it("repository button calls open_url with the repo link", async () => {
    renderModal();
    await waitFor(() => expect(screen.getByText("0.2.0")).toBeTruthy());
    fireEvent.click(screen.getByRole("button", { name: /代码仓库/ }));
    await waitFor(() =>
      expect(invokeMock).toHaveBeenCalledWith("open_url", {
        url: "https://github.com/Yangshifu1024/CodeWave",
      }),
    );
  });

  it("closes via the store flag", async () => {
    renderModal();
    fireEvent.click(document.querySelector(".ant-modal-close")!);
    expect(useUi.getState().aboutOpen).toBe(false);
  });
});
