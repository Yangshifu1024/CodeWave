import { afterEach, describe, expect, it } from "vitest";
import { miscHandlers } from "../stores/runHandlers";
import { useSessions } from "../stores/sessions";
import { useUi } from "../stores/ui";

const status = miscHandlers(() => {})["mcp:status"];
afterEach(() => { useUi.setState({ mcpStatus: [] }); useSessions.setState({ activeKey: null }); });
describe("MCP status identity", () => {
  it("accepts the backend name and drops old generations and other sessions", () => {
    useSessions.setState({ activeKey: "a" });
    status({ session: "a", name: "server", scope: "project", state: "ready", tools: 2, generation: 5, config_revision: 10 });
    status({ session: "a", name: "server", scope: "project", state: "starting", generation: 4, config_revision: 10 });
    status({ session: "b", name: "server", scope: "project", state: "stopped", generation: 99, config_revision: 99 });
    expect(useUi.getState().mcpStatus).toHaveLength(1);
    expect(useUi.getState().mcpStatus[0]).toMatchObject({ name: "server", state: "ready", generation: 5 });
    status({ session: "a", name: "server", scope: "project", state: "starting", generation: 6, config_revision: 11 });
    expect(useUi.getState().mcpStatus[0].state).toBe("starting");
  });
});
