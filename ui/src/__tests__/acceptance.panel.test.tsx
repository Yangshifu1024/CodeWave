// 交付验收只读投影（#16 完成门，[docs/plan-and-approval](../../../docs/plan-and-approval.md)）。
//
// 契约守护的是**语义**，不是像素：
// 1. 运行终态 ≠ 验收通过——面板不得出现任何「标记完成」类写入口；
// 2. 快照缺失必须显示「未验证」，绝不能显示「已通过」或「已完成」；
// 3. 无交付契约的普通问答**不渲染**这块面板（不为每条回答加门）。
import { describe, it, expect, vi, beforeEach, afterEach } from "vitest";
import { render, screen, waitFor, cleanup, fireEvent, act } from "@testing-library/react";
// 必须导入 i18n 实例：否则 useTranslation 返回 key 本身，断言中文文案永远失败。
import "../i18n";
import AcceptancePanel from "../features/chat/AcceptancePanel";
import type { DeliveryProjection } from "../ipc/types";

const { runState } = vi.hoisted(() => ({ runState: { tabs: {} as Record<string, { running?: boolean; runEpoch?: number; lastDoneRunId?: string | null }> } }));
vi.mock("../stores/run", () => ({ useRun: (selector: (state: typeof runState) => unknown) => selector(runState) }));
const getDeliveryProjection = vi.fn();
vi.mock("../ipc/client", () => ({
  ipc: {
    getDeliveryProjection: (sessionId: string) => getDeliveryProjection(sessionId),
  },
}));

function projection(over: Partial<DeliveryProjection> = {}): DeliveryProjection {
  return {
    task_id: "task-1",
    contract_revision: 1,
    snapshot: {
      task_id: "task-1",
      contract_revision: 1,
      revision: 3,
      overall: "passed",
      waived_count: 0,
      items: [{ requirement_id: "r1", status: "passed", reasons: [] }],
      reasons: [],
    },
    decision: "allow",
    allows_completion: true,
    gaps: [],
    repair_rounds_used: 0,
    repair_rounds_max: 2,
    ...over,
  };
}

beforeEach(() => {
  getDeliveryProjection.mockReset();
  runState.tabs = {};
});
afterEach(() => { cleanup(); vi.useRealTimers(); });

describe("AcceptancePanel 只读投影", () => {
  it("没有任何写入入口：面板里不存在能改状态的按钮", async () => {
    getDeliveryProjection.mockResolvedValue(projection());
    const { container } = render(<AcceptancePanel sessionId="s1" visible />);
    await waitFor(() => expect(screen.getByText("交付验收")).toBeTruthy());
    // 只读投影：全文不得出现「标记完成 / 设为通过 / set_passed」这类写动作。
    const text = container.textContent ?? "";
    expect(text).not.toMatch(/标记完成|设为通过|set[_-]?passed|写入验收/i);
    // 面板里不渲染任何 antd Button（Tag 是状态展示，不是动作）。
    expect(container.querySelectorAll("button")).toHaveLength(0);
  });

  it("无契约的普通问答不渲染面板", async () => {
    // task_id 为空 = 本会话没有交付契约（旧问答沿原收尾），不该凭空多一块提示。
    getDeliveryProjection.mockResolvedValue(projection({ task_id: "", snapshot: null, decision: "allow" }));
    const { container } = render(<AcceptancePanel sessionId="s1" visible />);
    await waitFor(() => expect(getDeliveryProjection).toHaveBeenCalled());
    expect(container.innerHTML).toBe("");
  });

  it("有契约但快照缺失显示「未验证」，不得显示「已通过」", async () => {
    getDeliveryProjection.mockResolvedValue(projection({ snapshot: null, decision: "blocked", allows_completion: false }));
    render(<AcceptancePanel sessionId="s1" visible />);
    await waitFor(() => expect(screen.getByText(/未验证/)).toBeTruthy());
    expect(screen.queryByText("已通过验收")).toBeNull();
  });

  it("未通过时展示「还差这些」，不显示通过态", async () => {
    getDeliveryProjection.mockResolvedValue(
      projection({
        snapshot: {
          task_id: "task-1",
          contract_revision: 1,
          revision: 4,
          overall: "failed",
          waived_count: 0,
          items: [
            {
              requirement_id: "r1",
              status: "failed",
              reasons: [{ code: "check_not_satisfied", message: "断言失败", target_id: "ci" }],
            },
          ],
          reasons: [],
        },
        decision: "blocked",
        allows_completion: false,
        gaps: [{ requirement_id: "r1", check_id: "ci", reason_code: "check_not_satisfied", message: "断言失败" }],
      }),
    );
    render(<AcceptancePanel sessionId="s1" visible />);
    await waitFor(() => expect(screen.getByText("还差这些")).toBeTruthy());
    expect(screen.queryByText("已通过验收")).toBeNull();
    expect(screen.getByText(/ci：/)).toBeTruthy();
  });

  it("豁免数量与已用修复轮都要显示", async () => {
    // waived_count 住在快照里（组件读 snapshot.waived_count），不在投影顶层。
    const p0 = projection({ repair_rounds_used: 1, repair_rounds_max: 2 });
    p0.snapshot!.waived_count = 2;
    getDeliveryProjection.mockResolvedValue(p0);
    render(<AcceptancePanel sessionId="s1" visible />);
    await waitFor(() => expect(screen.getByText(/含 2 项用户豁免/)).toBeTruthy());
    expect(screen.getByText(/已用修复轮 1 \/ 2/)).toBeTruthy();
  });

  it("不可见时不发起查询", () => {
    getDeliveryProjection.mockResolvedValue(projection());
    render(<AcceptancePanel sessionId="s1" visible={false} />);
    fireEvent.focus(window);
    expect(getDeliveryProjection).not.toHaveBeenCalled();
  });
  it("同会话后来创建契约时通过定时刷新显示", async () => {
    vi.useFakeTimers();
    getDeliveryProjection.mockResolvedValueOnce(projection({ task_id: "", snapshot: null })).mockResolvedValue(projection());
    render(<AcceptancePanel sessionId="s1" visible />);
    await act(async () => {});
    expect(screen.queryByText("交付验收")).toBeNull();
    await act(async () => { await vi.advanceTimersByTimeAsync(10_000); });
    expect(screen.getByText("交付验收")).toBeTruthy();
  });

  it("运行结束后重新查询，错误不保留旧通过态", async () => {
    runState.tabs.s1 = { running: true, runEpoch: 1 };
    getDeliveryProjection.mockResolvedValueOnce(projection()).mockRejectedValueOnce(new Error("read failed"));
    const { rerender } = render(<AcceptancePanel sessionId="s1" visible />);
    await waitFor(() => expect(screen.getByText("已通过验收")).toBeTruthy());
    runState.tabs.s1 = { running: false, runEpoch: 1, lastDoneRunId: "run1" };
    rerender(<AcceptancePanel sessionId="s1" visible />);
    await waitFor(() => expect(screen.getByText(/read failed/)).toBeTruthy());
    expect(screen.queryByText("已通过验收")).toBeNull();
  });

  it("窗口回焦刷新外部变更，同一请求未完成时不重复查询", async () => {
    let finish!: (value: DeliveryProjection) => void;
    getDeliveryProjection.mockResolvedValueOnce(projection()).mockImplementationOnce(() => new Promise((resolve) => { finish = resolve; }));
    render(<AcceptancePanel sessionId="s1" visible />);
    await waitFor(() => expect(screen.getByText("已通过验收")).toBeTruthy());
    fireEvent.focus(window);
    fireEvent.focus(window);
    expect(getDeliveryProjection).toHaveBeenCalledTimes(2);
    expect(screen.queryByText("已通过验收")).toBeNull();
    await act(async () => { finish(projection({ allows_completion: false, decision: "blocked" })); });
    expect(screen.queryByText("已通过验收")).toBeNull();
  });

  it("切会话后旧请求不能覆盖新会话，隐藏后停止刷新", async () => {
    let finish!: (value: DeliveryProjection) => void;
    getDeliveryProjection.mockImplementationOnce(() => new Promise((resolve) => { finish = resolve; })).mockResolvedValue(projection({ allows_completion: false, decision: "blocked" }));
    const { rerender } = render(<AcceptancePanel sessionId="s1" visible />);
    rerender(<AcceptancePanel sessionId="s2" visible />);
    await waitFor(() => expect(screen.getByText("交付验收")).toBeTruthy());
    await act(async () => { finish(projection()); });
    expect(screen.queryByText("已通过验收")).toBeNull();
    rerender(<AcceptancePanel sessionId="s2" visible={false} />);
    fireEvent.focus(window);
    expect(getDeliveryProjection).toHaveBeenCalledTimes(2);
  });

});
