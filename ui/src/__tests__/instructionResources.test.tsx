import { afterEach, describe, expect, it, vi } from "vitest";
import { act, cleanup, fireEvent, render, waitFor } from "@testing-library/react";
import { App, ConfigProvider } from "antd";
import "../i18n";
import InstructionResourcesPanel from "../features/panels/InstructionResourcesPanel";
import type { InstructionResource } from "../ipc/types";

const invoke = vi.hoisted(() => vi.fn());
vi.mock("@tauri-apps/api/core", () => ({ invoke }));
afterEach(() => { cleanup(); invoke.mockReset(); });

const resource = (): InstructionResource => ({
  available: true, id: "resource-1", name: "AGENTS.md", kind: "instruction",
  origin: { source: "workspace_file", identity: "file:/project/AGENTS.md", version: "sha256:reviewed" },
  status: { trusted: false, remembered: false, revision: null, error: null }, active_versions: [],
});
function mount() { return render(<ConfigProvider><App><InstructionResourcesPanel sessionId="s1" /></App></ConfigProvider>); }
function button(text: string) {
  const result = Array.from(document.querySelectorAll("button")).find((button) => button.textContent?.replace(/\s/g, "") === text);
  if (!result) throw new Error(`Missing button ${text}`);
  return result;
}
function mock() {
  let row = resource();
  invoke.mockImplementation(async (command: string, args?: Record<string, string>) => {
    if (command === "inspect_instruction_resources") return { resources: [row], issues: [] };
    if (command === "prepare_instruction_review") return { id: "ticket-1", resource_id: row.id, name: "HOST NAME", origin: row.origin, text: "HOST DESCRIPTION AND BODY", action: args?.action };
    if (command === "confirm_instruction_review") {
      row = { ...row, status: { trusted: true, remembered: true, revision: 1, error: null } };
      return { operation_id: "ticket-1", confirmed: true, resolved: true, error: null };
    }
    if (command === "discard_instruction_review") return;
    throw new Error(`unmocked ${command}`);
  });
}
async function open(label = "记住此版本") {
  await waitFor(() => expect(button(label)).toBeTruthy());
  fireEvent.click(button(label));
  await waitFor(() => expect(document.querySelector(".ant-modal-confirm")?.textContent).toContain("HOST DESCRIPTION AND BODY"));
}
function confirm() { fireEvent.click(document.querySelector<HTMLButtonElement>(".ant-modal-confirm .ant-btn-primary")!); }

describe("instruction resource review", () => {
  it("shows the host projection and commits only its opaque confirmation", async () => {
    mock(); mount(); await open();
    expect(document.querySelector(".ant-modal-confirm")?.textContent).toContain("HOST NAME");
    confirm();
    await waitFor(() => expect(document.body.textContent).toContain("已记住版本"));
    expect(invoke.mock.calls.find(([command]) => command === "confirm_instruction_review")).toEqual(["confirm_instruction_review", { sessionId: "s1", confirmation: "ticket-1" }]);
  });

  it("cancels or unmounts prepared reviews without granting permission", async () => {
    mock(); const view = mount(); await open();
    fireEvent.click(document.querySelector<HTMLButtonElement>(".ant-modal-confirm .ant-btn-default")!);
    await waitFor(() => expect(invoke.mock.calls.some(([command]) => command === "discard_instruction_review")).toBe(true));
    view.unmount();
    expect(invoke.mock.calls.some(([command]) => command === "confirm_instruction_review")).toBe(false);
  });

  it("keeps unknown operation inspection after authority refresh and never resubmits approval", async () => {
    mock(); const base = invoke.getMockImplementation()!;
    let refreshed = false;
    invoke.mockImplementation(async (command: string, args?: Record<string, string>) => {
      if (command === "confirm_instruction_review") { refreshed = true; return { operation_id: "ticket-1", confirmed: false, resolved: false, error: "unknown" }; }
      if (command === "inspect_instruction_resources" && refreshed) return { resources: [{ ...resource(), status: { ...resource().status, revision: 2, error: "locked" } }], issues: [] };
      if (command === "instruction_review_operation") return { operation_id: "ticket-1", confirmed: false, resolved: true, error: "not retried" };
      return base(command, args);
    });
    mount(); await open(); confirm();
    await waitFor(() => expect(document.body.textContent).toContain("locked"));
    expect(button("记住此版本").disabled).toBe(true);
    fireEvent.click(button("检查指令许可结果"));
    await waitFor(() => expect(button("记住此版本").disabled).toBe(false));
    expect(invoke.mock.calls.filter(([command]) => command === "confirm_instruction_review")).toHaveLength(1);
    expect(invoke.mock.calls.find(([command]) => command === "instruction_review_operation")).toEqual(["instruction_review_operation", { sessionId: "s1", operationId: "ticket-1" }]);
  });

  it("discards a preparation arriving after its owner closes", async () => {
    mock(); const base = invoke.getMockImplementation()!;
    let release!: () => void;
    invoke.mockImplementation(async (command: string, args?: Record<string, string>) => {
      const result = await base(command, args);
      if (command === "prepare_instruction_review") await new Promise<void>((resolve) => { release = resolve; });
      return result;
    });
    const view = mount();
    await waitFor(() => expect(button("记住此版本")).toBeTruthy());
    fireEvent.click(button("记住此版本"));
    await waitFor(() => expect(release).toBeTypeOf("function"));
    view.unmount(); await act(async () => { release(); });
    await waitFor(() => expect(invoke.mock.calls.filter(([command]) => command === "discard_instruction_review")).toHaveLength(1));
    expect(invoke.mock.calls.some(([command]) => command === "confirm_instruction_review")).toBe(false);
  });

  it("does not turn a confirmed receipt into an unknown operation when refresh fails", async () => {
    mock(); const base = invoke.getMockImplementation()!;
    let committed = false;
    invoke.mockImplementation(async (command: string, args?: Record<string, string>) => {
      if (command === "inspect_instruction_resources" && committed) throw new Error("refresh failed");
      const result = await base(command, args);
      if (command === "confirm_instruction_review") committed = true;
      return result;
    });
    mount(); await open(); confirm();
    await waitFor(() => expect(document.body.textContent).toContain("refresh failed"));
    expect(document.body.textContent).not.toContain("检查指令许可结果");
  });
});
