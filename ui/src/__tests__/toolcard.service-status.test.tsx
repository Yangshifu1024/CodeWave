// 后台服务（service 工具）卡片状态的三态回归（服务状态修复的展示层配套）。
//
// 主缺陷：旧实现用 `data.tail ? "运行中" : "已停止"` 代理存活。
// `start` 出参根本没有 tail（只有 {id, pid, purpose, owner_root_id, note, running}），
// 无输出的服务（如 `sleep 300`）tail 恒为空 → 进程明明在跑，界面却永久显示「已停止」，
// 且「停止服务」按钮一并消失 → 形成占端口的僵尸服务。
// 修复后改为读权威字段 `running`：true=运行中 / false=已停止 / 缺失=状态未知，
// 且**未知态同样保留停止按钮**（否则旧历史 / 推送全丢时用户仍无收手入口）。
import { describe, it, expect, vi, afterEach, beforeEach } from "vitest";
import { render, cleanup, fireEvent } from "@testing-library/react";
import { i18n } from "../i18n";
import ToolCallCard from "../features/tools/ToolCallCard";
import { useSessions } from "../stores/sessions";
import type { ToolView } from "../stores/run";

const { stopService } = vi.hoisted(() => ({ stopService: vi.fn(async () => {}) }));

vi.mock("@tauri-apps/api/core", () => ({
  invoke: vi.fn(async () => null),
  Channel: class {
    onmessage: ((f: any) => void) | null = null;
  },
}));

// 惯例：唯一 invoke 入口 ui/src/ipc/client 必须 mock（不直接 mock @tauri-apps/api/core）
vi.mock("../ipc/client", () => ({ ipc: { stopService } }));

/** service 卡：出参默认就是后端 `start` 的真实形状（无 tail），可按用例补字段 */
function serviceCard(data: Record<string, unknown>, args?: Record<string, unknown>): ToolView {
  return {
    callKey: "b1:0",
    tool: "service",
    status: "ok",
    progressTail: "",
    argsPreview: JSON.stringify(args ?? { action: "start", command: "sleep 300" }),
    outcome: { ok: true, data },
  };
}

function expand(): void {
  fireEvent.click(document.querySelector(".tool-card .tool-head")!);
}

/** service 分支的状态 Tag 文本（第一块 .kv 里的 Tag） */
function statusTagText(): string {
  const tag = document.querySelector(".tool-card .tool-body .kv .ant-tag");
  if (!tag) throw new Error("service status tag not found");
  return tag.textContent ?? "";
}

/** antd 两字按钮会插空格（"停 止"…），按文本匹配前先去掉所有空白 */
function stopButton(): HTMLElement | null {
  const want = i18n.t("tools.stopService").replace(/\s/g, "");
  const buttons = Array.from(document.querySelectorAll<HTMLElement>(".tool-card .tool-body button"));
  return buttons.find((b) => (b.textContent ?? "").replace(/\s/g, "") === want) ?? null;
}

function bodyText(): string {
  return document.querySelector(".tool-card .tool-body")?.textContent ?? "";
}

beforeEach(() => {
  stopService.mockClear();
  useSessions.setState({ activeKey: "s1" });
});

afterEach(() => {
  cleanup();
  useSessions.setState({ activeKey: null });
});

describe("service 卡片状态（running 权威，不再拿 tail 当存活代理）", () => {
  it("running:true 且无 tail（start 出参的真实形状）：显示「运行中」、无「已停止」，停止按钮可见", () => {
    // ← 钉死主缺陷：修复前 tail 缺失 → 显示「已停止」且按钮消失
    render(<ToolCallCard tool={serviceCard({ id: "svc-1", pid: 4242, purpose: "dev", running: true })} />);
    expand();

    expect(statusTagText()).toBe(i18n.t("tools.serviceRunning"));
    expect(statusTagText()).not.toContain(i18n.t("tools.serviceStopped"));
    expect(stopButton()).not.toBeNull();
  });

  it("running:false：显示「已停止」，且不含「运行中」（停止按钮消失）", () => {
    render(<ToolCallCard tool={serviceCard({ id: "svc-2", pid: 4242, running: false, status: "exited", exit_code: 0 })} />);
    expand();

    expect(statusTagText()).toBe(i18n.t("tools.serviceStopped"));
    expect(statusTagText()).not.toContain(i18n.t("tools.serviceRunning"));
    expect(stopButton()).toBeNull();
  });

  it("running 字段缺失（旧历史 / 推送全丢）：显示「状态未知」，不得谎报「已停止」", () => {
    // ← 关键回归点：旧实现在这里会显示「已停止」并吃掉停止按钮
    render(<ToolCallCard tool={serviceCard({ id: "svc-3", pid: 4242 })} />);
    expand();

    expect(statusTagText()).toBe(i18n.t("tools.serviceUnknown"));
    expect(statusTagText()).not.toContain(i18n.t("tools.serviceStopped"));
    expect(stopButton()).not.toBeNull(); // 未知也要留收手入口
  });

  it("running:true 且 tail 为空串（无输出服务，如 sleep 300）：仍显示「运行中」且停止按钮可见", () => {
    render(<ToolCallCard tool={serviceCard({ id: "svc-4", pid: 4242, running: true, tail: "" })} />);
    expand();

    expect(statusTagText()).toBe(i18n.t("tools.serviceRunning"));
    expect(stopButton()).not.toBeNull();
    // 空 tail 时输出区退化为「（无输出）」占位，不该把状态误读成已停止
    expect(bodyText()).toContain(i18n.t("tools.noOutput"));
  });

  it("running:true 且 tail 非空（日志有输出）：同样显示「运行中」（tail 不再参与状态判定）", () => {
    render(<ToolCallCard tool={serviceCard({ id: "svc-5", pid: 4242, running: true, tail: "listening on :5173" })} />);
    expand();

    expect(statusTagText()).toBe(i18n.t("tools.serviceRunning"));
    expect(bodyText()).toContain("listening on :5173");
    expect(stopButton()).not.toBeNull();
  });

  it("点击「停止服务」：调 ipc.stopService(会话 id, 服务 id) 两个参数都对", async () => {
    render(<ToolCallCard tool={serviceCard({ id: "svc-6", pid: 4242, running: true })} />);
    expand();

    const btn = stopButton();
    expect(btn).not.toBeNull();
    fireEvent.click(btn!);
    await vi.waitFor(() => expect(stopService).toHaveBeenCalledTimes(1));
    expect(stopService).toHaveBeenCalledWith("s1", "svc-6");
  });

  it("点停止按钮不会顺带展开/收起卡片（按钮冒泡被截断）", () => {
    render(<ToolCallCard tool={serviceCard({ id: "svc-7", pid: 4242, running: true })} />);
    expand();
    const chevron = () => document.querySelector(".tool-card .chev")?.textContent;
    expect(chevron()).toBe("▾"); // 展开态

    fireEvent.click(stopButton()!);
    expect(chevron()).toBe("▾"); // 仍是展开态：点按钮不该把卡片折叠回去
  });

  it("running 缺失（未知态）：停止按钮仍可见——防僵尸服务无收手入口", () => {
    render(<ToolCallCard tool={serviceCard({ id: "svc-8", pid: 4242 })} />);
    expand();
    expect(statusTagText()).toBe(i18n.t("tools.serviceUnknown"));
    expect(stopButton()).not.toBeNull();
  });

  it("收起态不渲染状态 Tag 与停止按钮（头部点击展开后才出现）", () => {
    render(<ToolCallCard tool={serviceCard({ id: "svc-9", pid: 4242, running: true })} />);
    expect(document.querySelector(".tool-card .tool-body")).toBeNull();
    expect(stopButton()).toBeNull();
  });
});
