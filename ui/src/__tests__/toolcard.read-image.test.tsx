// 读图卡片（图片通道修复的展示层配套，会话 6bca80f4 的根因批次）：
// 历史里不再保留那段 base64（data_url）——重开会话后从历史重建的卡片按**路径**重新读出来显示，
// 读失败（文件被移动/删除/越界）才给占位提示；模型未勾选「支持图片输入」时另给一行提示。
import { describe, it, expect, vi, afterEach, beforeEach } from "vitest";
import { render, cleanup, fireEvent, waitFor } from "@testing-library/react";
import { i18n } from "../i18n";
import ToolCallCard from "../features/tools/ToolCallCard";
import { useSessions } from "../stores/sessions";
import type { ToolView } from "../stores/run";

const { calls } = vi.hoisted(() => ({ calls: [] as { cmd: string; args: any }[] }));

vi.mock("@tauri-apps/api/core", () => ({
  invoke: vi.fn(async (cmd: string, args: any) => {
    calls.push({ cmd, args });
    if (cmd === "read_workspace_file_base64") {
      if (args.path === "missing.png") throw new Error("E_NOT_FOUND: 文件不存在");
      return { path: args.path, size: 3, content: "aGk=" };
    }
    return null;
  }),
  Channel: class {
    onmessage: ((f: any) => void) | null = null;
  },
}));

function readCard(file: Record<string, unknown>): ToolView {
  return {
    callKey: "b1:0",
    tool: "read",
    status: "ok",
    progressTail: "",
    argsPreview: JSON.stringify({ files: [{ path: file.path }] }),
    outcome: { ok: true, data: { files: [file] } },
  };
}

function expand(): void {
  fireEvent.click(document.querySelector(".tool-head")!);
}

function imgSrc(): string | null {
  return document.querySelector<HTMLImageElement>(".tool-card img.img")?.getAttribute("src") ?? null;
}

function body(): string {
  return document.body.textContent ?? "";
}

beforeEach(() => {
  calls.length = 0;
  useSessions.setState({ activeKey: "s1" });
});

afterEach(() => {
  cleanup();
  useSessions.setState({ activeKey: null });
});

describe("read 卡片里的图片", () => {
  it("事件带 data_url：直接渲染，不再读盘", () => {
    render(
      <ToolCallCard
        tool={readCard({
          path: "a.png",
          kind: "image",
          media_type: "image/png",
          data_url: "data:image/png;base64,ZZZ",
        })}
      />,
    );
    expand();
    expect(imgSrc()).toBe("data:image/png;base64,ZZZ");
    expect(calls.filter((c) => c.cmd === "read_workspace_file_base64")).toHaveLength(0);
  });

  it("无 data_url（重开会话后）：按路径读出来渲染", async () => {
    render(
      <ToolCallCard
        tool={readCard({ path: "a.png", kind: "image", media_type: "image/png" })}
      />,
    );
    expand();
    await waitFor(() => expect(imgSrc()).toBe("data:image/png;base64,aGk="));
    expect(
      calls.some((c) => c.cmd === "read_workspace_file_base64" && c.args.path === "a.png"),
    ).toBe(true);
  });

  it("读失败（文件已被移动/删除）：给占位提示，不渲染图", async () => {
    render(
      <ToolCallCard
        tool={readCard({ path: "missing.png", kind: "image", media_type: "image/png" })}
      />,
    );
    expand();
    await waitFor(() =>
      expect(body()).toContain(i18n.t("tools.imageUnavailable", { path: "missing.png" })),
    );
    expect(imgSrc()).toBeNull();
  });

  it("模型未开启图片输入：提示图片未发送给模型", () => {
    render(
      <ToolCallCard
        tool={readCard({
          path: "a.png",
          kind: "image",
          media_type: "image/png",
          data_url: "data:image/png;base64,ZZZ",
          sent_to_model: false,
        })}
      />,
    );
    expand();
    expect(body()).toContain(i18n.t("tools.imageNotSent"));
    // 图片本身照旧显示（界面预览不受影响）
    expect(imgSrc()).toBe("data:image/png;base64,ZZZ");
  });

  it("勾选支持图片输入：不出现未发送提示", () => {
    render(
      <ToolCallCard
        tool={readCard({
          path: "a.png",
          kind: "image",
          media_type: "image/png",
          data_url: "data:image/png;base64,ZZZ",
          sent_to_model: true,
        })}
      />,
    );
    expand();
    expect(body()).not.toContain(i18n.t("tools.imageNotSent"));
  });
});
