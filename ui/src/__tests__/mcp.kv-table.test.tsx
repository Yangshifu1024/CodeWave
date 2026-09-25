import { useState } from "react";
import { describe, expect, it } from "vitest";
import { fireEvent, render } from "@testing-library/react";
import { McpKvTable, isSensitiveMcpKey } from "../features/panels/McpKvTable";
import type { McpKeyValueRow } from "../utils/mcpConfig";

function Editor() {
  const [rows, setRows] = useState<McpKeyValueRow[]>([
    { key: "API_KEY", value: "first-secret" },
    { key: "API_KEY", value: "second-secret" },
  ]);
  return <McpKvTable
    rows={rows}
    keyPlaceholder="Key"
    valuePlaceholder="Value"
    deleteLabel="Delete row"
    addLabel="Add row"
    showSecretLabel="Show secret"
    hideSecretLabel="Hide secret"
    maskSensitive
    onPatch={(index, patch) => setRows((previous) => previous.map((row, i) => i === index ? { ...row, ...patch } : row))}
    onAdd={() => setRows((previous) => [...previous, { key: "", value: "" }])}
    onRemove={(index) => setRows((previous) => previous.filter((_, i) => i !== index))}
  />;
}

describe("MCP 键值编辑器", () => {
  it("删除已显示的密钥行后，同名下一行仍保持遮罩", () => {
    const { container } = render(<Editor />);
    const firstRow = container.querySelectorAll(".mcp-table-row")[0];
    fireEvent.click(firstRow.querySelector<HTMLButtonElement>('[aria-label="Show secret"]')!);
    expect(firstRow.querySelector<HTMLTextAreaElement>('textarea[placeholder="Value"]')!.value).toBe("first-secret");

    fireEvent.click(firstRow.querySelector<HTMLButtonElement>('[aria-label="Delete row"]')!);
    const remainingRow = container.querySelector(".mcp-table-row")!;
    expect(remainingRow.querySelector<HTMLTextAreaElement>('textarea[placeholder="Value"]')!.value).toBe("••••••••");
  });

  it.each(["GITHUB_PAT", "accessToken", "clientSecret", "X-API-Key", "Authorization"])("识别密钥名称 %s", (key) => {
    expect(isSensitiveMcpKey(key)).toBe(true);
  });
});
