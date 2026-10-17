import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { cleanup, fireEvent, render, screen, waitFor, act } from "@testing-library/react";
import "../i18n";
import WorkflowSettings from "../features/panels/WorkflowSettings";
import WorkflowPanel from "../features/chat/WorkflowPanel";
const mock = vi.hoisted(() => ({ read: vi.fn(), validate: vi.fn(), save: vi.fn(), projection: vi.fn(), resume: vi.fn(), override: vi.fn() }));
vi.mock("../ipc/client", () => ({ ipc: { readProjectWorkflow: mock.read, validateProjectWorkflow: mock.validate, saveProjectWorkflow: mock.save, getWorkflowProjection: mock.projection, continueWorkflow: mock.resume, overrideWorkflowRoute: mock.override } }));
vi.mock("../stores/run", () => ({ useRun: (selector: (state: {
        tabs: Record<string, never>;
    }) => unknown) => selector({ tabs: {} }) }));
const view = { text: '{"schema":1,"id":"project"}', revision: 2, digest: "approved-bytes", approved: true, issues: [] };
const projection = { task_id: "task1", contract_revision: 3, root_run_id: "run1", route: { kind: "serial_surfaces", reasons: ["shared file"] }, units: [{ id: "u1", role_id: "backend-dev", model_id: "m1", depends_on: [], write_scopes: [{ root_id: "workspace", relative_path: "a.rs" }] }], nodes: [{ unit_id: "u1", state: "interrupted", attempt: 1, reason: "stopped" }], checks: [{ id: "tests", required: true, platforms: ["windows"], prerequisites: ["cargo"] }], leases: [], frozen_revision: 1, frozen_digest: "frozen", candidate: view, resume_allowed: true, recovery_error: null, design: { required: false, reasons: [] } };
beforeEach(() => { Object.values(mock).forEach(fn => fn.mockReset()); mock.read.mockResolvedValue(view); mock.validate.mockResolvedValue([]); mock.projection.mockResolvedValue(projection); mock.save.mockResolvedValue({ ...view, revision: 3 }); mock.resume.mockResolvedValue("new-run"); });
afterEach(() => { cleanup(); vi.useRealTimers(); });
function deferred<T>() {
    let resolve!: (value: T) => void;
    let reject!: (reason: Error) => void;
    const promise = new Promise<T>((yes, no) => { resolve = yes; reject = no; });
    return { promise, resolve, reject };
}
function confirm() { const buttons = Array.from(document.querySelectorAll<HTMLButtonElement>(".ant-modal-footer button")); const button = buttons.find(b => b.classList.contains("ant-btn-primary")); expect(button).toBeTruthy(); fireEvent.click(button!); }
describe("Desktop workflow", () => {
    it("reviews exact draft before saving with byte/version CAS", async () => { render(<WorkflowSettings sessionId="s1"/>); const input = await screen.findByLabelText("工作流 JSON"); fireEvent.change(input, { target: { value: '{"schema":1,"id":"new-draft"}' } }); fireEvent.click(screen.getByText("审核并保存")); expect(mock.save).not.toHaveBeenCalled(); expect(screen.getByText('采用以下工作流定义？')).toBeTruthy(); confirm(); await waitFor(() => expect(mock.save).toHaveBeenCalled()); expect(mock.save.mock.calls[0].slice(0, 4)).toEqual(["s1", '{"schema":1,"id":"new-draft"}', 2, "approved-bytes"]); });
    it("keeps draft and field issues after conflict/validation failure", async () => { mock.save.mockRejectedValue(new Error("revision conflict")); render(<WorkflowSettings sessionId="s1"/>); const input = await screen.findByLabelText("工作流 JSON"); fireEvent.change(input, { target: { value: "my draft" } }); fireEvent.click(screen.getByText("审核并保存")); confirm(); await screen.findByText(/revision conflict/); expect((input as HTMLTextAreaElement).value).toBe("my draft"); expect(mock.read).toHaveBeenCalledTimes(1); });
    it("late old-session reads cannot replace new session draft", async () => { let old!: (value: typeof view) => void; mock.read.mockImplementationOnce(() => new Promise(resolve => { old = resolve; })).mockResolvedValueOnce({ ...view, text: "new session" }); const { rerender } = render(<WorkflowSettings sessionId="s1"/>); rerender(<WorkflowSettings sessionId="s2"/>); await waitFor(() => expect((screen.getByLabelText("工作流 JSON") as HTMLTextAreaElement).value).toBe("new session")); await act(async () => { old(view); }); expect((screen.getByLabelText("工作流 JSON") as HTMLTextAreaElement).value).toBe("new session"); });
    it("renders frozen/current versions, ownership and platform gaps without pass buttons", async () => { const { container } = render(<WorkflowPanel sessionId="s1" visible/>); await screen.findByText("shared file"); expect(container.textContent).toContain("采用版本 1"); expect(container.textContent).toContain("当前项目版本 2"); expect(container.textContent).toContain("backend-dev"); expect(container.textContent).toContain("windows"); expect(container.textContent).not.toContain("标记完成"); fireEvent.click(screen.getByText("以新尝试继续")); expect(mock.resume).not.toHaveBeenCalled(); confirm(); await waitFor(() => expect(mock.resume).toHaveBeenCalledWith("s1", "task1", 3)); });
    it.each(["success", "failure"])("late old-session save %s cannot change the new session draft or review", async outcome => {
        const save = deferred<typeof view>();
        mock.save.mockReturnValueOnce(save.promise);
        mock.read.mockResolvedValueOnce(view).mockResolvedValueOnce({ ...view, text: "session two draft" });
        const { rerender } = render(<WorkflowSettings sessionId="s1"/>);
        await waitFor(() => expect((screen.getByLabelText("工作流 JSON") as HTMLTextAreaElement).value).toBe(view.text));
        fireEvent.click(screen.getByText("审核并保存"));
        confirm();
        await waitFor(() => expect(mock.save).toHaveBeenCalledTimes(1));
        rerender(<WorkflowSettings sessionId="s2"/>);
        await waitFor(() => expect((screen.getByLabelText("工作流 JSON") as HTMLTextAreaElement).value).toBe("session two draft"));
        fireEvent.click(screen.getByText("审核并保存"));
        await act(async () => {
            if (outcome === "success") save.resolve({ ...view, text: "stale saved text" });
            else save.reject(new Error("old save failed"));
        });
        expect((screen.getByLabelText("工作流 JSON") as HTMLTextAreaElement).value).toBe("session two draft");
        expect(screen.queryByText(/old save failed/)).toBeNull();
        expect(screen.getByText("采用以下工作流定义？")).toBeTruthy();
        expect(mock.read).toHaveBeenCalledTimes(2);
    });
    it("switching session while save validation is pending never writes the old draft", async () => {
        const validation = deferred<[]>();
        mock.validate.mockReturnValueOnce(validation.promise);
        const { rerender } = render(<WorkflowSettings sessionId="s1"/>);
        await waitFor(() => expect((screen.getByLabelText("工作流 JSON") as HTMLTextAreaElement).value).toBe(view.text));
        fireEvent.click(screen.getByText("审核并保存"));
        confirm();
        expect(mock.validate).toHaveBeenCalledTimes(1);
        rerender(<WorkflowSettings sessionId="s2"/>);
        await waitFor(() => expect(mock.read).toHaveBeenCalledTimes(2));
        await act(async () => { validation.resolve([]); });
        expect(mock.save).not.toHaveBeenCalled();
        expect((screen.getByText("审核并保存") as HTMLElement).closest("button")?.disabled).toBe(false);
    });
    it("repeated save confirmations stay single-flight through validation and persistence", async () => {
        const validation = deferred<[]>();
        const save = deferred<typeof view>();
        mock.validate.mockReturnValueOnce(validation.promise);
        mock.save.mockReturnValueOnce(save.promise);
        render(<WorkflowSettings sessionId="s1"/>);
        await waitFor(() => expect((screen.getByLabelText("工作流 JSON") as HTMLTextAreaElement).value).toBe(view.text));
        fireEvent.click(screen.getByText("审核并保存"));
        act(() => { confirm(); confirm(); });
        expect(mock.validate).toHaveBeenCalledTimes(1);
        expect(mock.save).not.toHaveBeenCalled();
        await act(async () => { validation.resolve([]); });
        expect(mock.save).toHaveBeenCalledTimes(1);
        confirm();
        expect(mock.validate).toHaveBeenCalledTimes(1);
        expect(mock.save).toHaveBeenCalledTimes(1);
        await act(async () => { save.resolve(view); });
        expect((screen.getByText("审核并保存") as HTMLElement).closest("button")?.disabled).toBe(false);
    });
    it.each(["success", "failure"])("late old-session resume %s cannot close the new session confirmation or report an error", async outcome => {
        const resume = deferred<string>();
        mock.resume.mockReturnValueOnce(resume.promise);
        const { rerender } = render(<WorkflowPanel sessionId="s1" visible/>);
        await screen.findByText("shared file");
        fireEvent.click(screen.getByText("以新尝试继续"));
        confirm();
        expect(mock.resume).toHaveBeenCalledWith("s1", "task1", 3);
        rerender(<WorkflowPanel sessionId="s2" visible/>);
        await screen.findByText("shared file");
        fireEvent.click(screen.getByText("以新尝试继续"));
        await act(async () => {
            if (outcome === "success") resume.resolve("old-new-attempt");
            else resume.reject(new Error("old resume failed"));
        });
        expect(screen.queryByText(/old resume failed/)).toBeNull();
        expect(screen.getByRole("dialog")).toBeTruthy();
        expect(mock.projection.mock.calls.filter(([session]) => session === "s2")).toHaveLength(1);
        confirm();
        await waitFor(() => expect(mock.resume).toHaveBeenCalledWith("s2", "task1", 3));
    });
    it("repeated resume confirmations create one new attempt", async () => {
        const resume = deferred<string>();
        mock.resume.mockReturnValueOnce(resume.promise);
        render(<WorkflowPanel sessionId="s1" visible/>);
        await screen.findByText("shared file");
        fireEvent.click(screen.getByText("以新尝试继续"));
        act(() => { confirm(); confirm(); });
        expect(mock.resume).toHaveBeenCalledTimes(1);
        await act(async () => { resume.resolve("one-attempt"); });
        await waitFor(() => expect(mock.projection).toHaveBeenCalledTimes(2));
        expect(mock.resume).toHaveBeenCalledTimes(1);
    });
    it("hidden panel never queries or starts work", () => { render(<WorkflowPanel sessionId="s1" visible={false}/>); fireEvent.focus(window); expect(mock.projection).not.toHaveBeenCalled(); expect(mock.resume).not.toHaveBeenCalled(); });
});
