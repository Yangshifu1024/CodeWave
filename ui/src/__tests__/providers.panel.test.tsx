// ProvidersPanel interaction tests ([docs/provider-management-refactor](../../../docs/provider-management-refactor.md)): full add-provider flow + active_model_id fallback when deleting models/providers.
// Panel mounted standalone (no AppShell): the draft mirrors SettingsModal's patchDraft merge semantics with local useState.
import { describe, it, expect, beforeAll, afterEach } from "vitest";
import { render, screen, fireEvent, cleanup } from "@testing-library/react";
import { useState } from "react";
import AntApp from "antd/es/app";
import "../i18n"; // Component mounted standalone must init i18next explicitly (no global entry outside App.tsx)
import ProvidersPanel from "../features/panels/ProvidersPanel";
import type { ConfigState, ProviderConfig } from "../ipc/types";

function makeConfig(providers: ProviderConfig[], active_model_id: string | null): ConfigState {
  return {
    schema_version: 2,
    providers,
    active_model_id,
    proxy: null,
    network: { allow_private_network: false },
    compact_threshold: 0.6,
    compact_timeout_seconds: 180,
    approval: { enabled: true, confirm_outside_create: true, confirm_git_push: true, auto_confirm: false, command_allowlist: [] },
    validation: { python: true, rust: true, typescript: true, go: true, json: true },
    ui: { font_size: 15, accent: "cyan", language: "zh-CN" },
    custom_prompt: null,
    disabled_skills: [],
    log: { level: "info", session_verbose: false },
  };
}

let latest: ConfigState | null = null;

function Harness({ initial }: { initial: ConfigState }) {
  const [d, setD] = useState<ConfigState>(() => JSON.parse(JSON.stringify(initial)));
  latest = d;
  return (
    <AntApp>
      <ProvidersPanel draft={d} patchDraft={(p) => setD((prev) => ({ ...prev, ...p }))} />
    </AntApp>
  );
}

/** antd inserts a space inside two-character buttons: find buttons by whitespace-stripped text */
function buttonByText(text: string): HTMLButtonElement {
  const btn = Array.from(document.querySelectorAll("button")).find(
    (b) => (b.textContent ?? "").replace(/\s/g, "") === text,
  );
  if (!btn) throw new Error(`button not found: ${text}`);
  return btn as HTMLButtonElement;
}

function inputByPlaceholder(ph: string): HTMLInputElement {
  return screen.getByPlaceholderText(ph) as HTMLInputElement;
}

/** Click the Popconfirm trigger, then click Confirm */
async function confirmPopconfirm(trigger: HTMLElement) {
  fireEvent.click(trigger);
  await new Promise((r) => setTimeout(r, 60));
  const ok = document.querySelector(".ant-popover .ant-btn-primary") as HTMLButtonElement;
  expect(ok).toBeTruthy();
  fireEvent.click(ok);
  await new Promise((r) => setTimeout(r, 60));
}

beforeAll(() => {
  Element.prototype.scrollTo = (Element.prototype as any).scrollTo ?? (() => {});
});

afterEach(() => {
  cleanup();
  latest = null;
});

describe("ProvidersPanel 供应商管理", () => {
  it("添加供应商全流程：字段 + 添加模型弹窗 + 提交入 draft 并默认 active", async () => {
    render(<Harness initial={makeConfig([], null)} />);
    // Empty list state → enter the add form
    expect(document.body.textContent ?? "").toContain("还没有供应商");
    fireEvent.click(buttonByText("添加供应商"));
    // Fill provider fields ([docs/provider-form-rules-tightened](../../../docs/provider-form-rules-tightened.md): API Key required; API format moved up, right under the name)
    fireEvent.change(inputByPlaceholder("如：智谱 GLM"), { target: { value: "智谱 GLM" } });
    fireEvent.change(screen.getByPlaceholderText(/掩码/) as HTMLTextAreaElement, { target: { value: "sk-test-1" } });
    fireEvent.change(inputByPlaceholder("https://api.example.com/v1"), { target: { value: "https://open.bigmodel.cn/api/paas/v4" } });
    // Field order: name → API format → Base URL ([docs/provider-form-rules-tightened](../../../docs/provider-form-rules-tightened.md))
    const labels = Array.from(document.querySelectorAll(".ant-form-item-label")).map((x) => (x.textContent ?? "").trim());
    const idx = (s: string) => labels.findIndex((x) => x.includes(s));
    expect(idx("API 格式")).toBeGreaterThan(idx("名称"));
    expect(idx("Base URL")).toBeGreaterThan(idx("API 格式"));
    // Submit validation: submitting with no models → model list error and nothing enters the draft ([docs/provider-form-rules-tightened](../../../docs/provider-form-rules-tightened.md))
    fireEvent.click(buttonByText("添加供应商"));
    await new Promise((r) => setTimeout(r, 60));
    expect(document.querySelector(".provider-models-error")?.textContent).toContain("必填");
    expect(latest!.providers).toHaveLength(0);
    // Add model: fill the wire id in the modal + check the image input
    fireEvent.click(buttonByText("添加模型"));
    await new Promise((r) => setTimeout(r, 80));
    fireEvent.change(inputByPlaceholder("glm-4.7 / claude-sonnet-4-5 / gpt-4o"), { target: { value: "glm-4.7" } });
    const imageTag = Array.from(document.querySelectorAll(".ant-tag-checkable")).find((x) =>
      x.textContent?.includes("图片"),
    ) as HTMLElement;
    fireEvent.click(imageTag);
    const okBtn = document.querySelector(".ant-modal .ant-btn-primary") as HTMLButtonElement;
    expect(okBtn.disabled).toBe(false);
    fireEvent.click(okBtn);
    await new Promise((r) => setTimeout(r, 80));
    // Model row appears in the form
    expect(document.body.textContent ?? "").toContain("glm-4.7");
    fireEvent.click(buttonByText("添加供应商"));
    await new Promise((r) => setTimeout(r, 60));
    // Submit result: provider + model enter the draft, first model becomes the default active
    const p = latest!.providers[0];
    expect(p.name).toBe("智谱 GLM");
    expect(p.base_url).toBe("https://open.bigmodel.cn/api/paas/v4");
    expect(p.models).toHaveLength(1);
    expect(p.models[0].model).toBe("glm-4.7");
    expect(p.models[0].vision).toBe(true);
    // [docs/max-tokens-truncation-fix](../../../docs/max-tokens-truncation-fix.md): new model default max_tokens 32768
    expect(p.models[0].max_tokens).toBe(32768);
    expect(latest!.active_model_id).toBe(p.models[0].id);
  });

  it("新增校验（docs/provider-form-validation/29）：空字段提交亮必填红字，非法 URL 有格式提示，缺 key/模型均不入 draft", async () => {
    render(<Harness initial={makeConfig([], null)} />);
    fireEvent.click(buttonByText("添加供应商"));
    // Submit the empty form directly → required errors for name/Base URL/API Key
    fireEvent.click(buttonByText("添加供应商"));
    await new Promise((r) => setTimeout(r, 60));
    expect(document.body.textContent ?? "").toContain("必填");
    expect(latest!.providers).toHaveLength(0);
    // Enter an invalid URL (shown once touched) → URL format error
    fireEvent.change(inputByPlaceholder("https://api.example.com/v1"), { target: { value: "abc" } });
    await new Promise((r) => setTimeout(r, 60));
    expect(document.body.textContent ?? "").toContain("http(s)://");
    expect(latest!.providers).toHaveLength(0);
    // Valid name/URL but no key and no models → submit still blocked: errors on both API Key and the model list ([docs/provider-form-rules-tightened](../../../docs/provider-form-rules-tightened.md))
    fireEvent.change(inputByPlaceholder("如：智谱 GLM"), { target: { value: "X" } });
    fireEvent.change(inputByPlaceholder("https://api.example.com/v1"), { target: { value: "https://api.example.com/v1" } });
    fireEvent.click(buttonByText("添加供应商"));
    await new Promise((r) => setTimeout(r, 60));
    // Name/URL now valid: errors narrow to API Key (asserted via the field container — antd error leave
    // animations do not recycle nodes in happy-dom, so count assertions are unreliable) and the model list
    const keysItem = Array.from(document.querySelectorAll(".ant-form-item")).find((x) =>
      (x.querySelector(".ant-form-item-label")?.textContent ?? "").includes("API Key"),
    );
    expect(keysItem?.querySelector(".ant-form-item-explain-error")?.textContent ?? "").toContain("必填");
    expect(document.querySelector(".provider-models-error")?.textContent).toContain("必填");
    expect(latest!.providers).toHaveLength(0);
  });

  it("编辑校验（docs/provider-form-validation/29）：清空名称/API Key 实时红字；删空模型列表红字", async () => {
    const provider: ProviderConfig = {
      id: "p1", name: "P1", api_format: "openai_chat",
      base_url: "https://a.example/v1", keys: ["***abcd"],
      models: [
        { id: "m1", model: "model-a", max_tokens: 8192, context_window: 128000, reasoning_effort: null, vision: false, video: false },
      ],
    };
    render(<Harness initial={makeConfig([provider], "m1")} />);
    fireEvent.click(buttonByText("编辑供应商"));
    await new Promise((r) => setTimeout(r, 60));
    // Clear the name → required error
    fireEvent.change(screen.getByDisplayValue("P1") as HTMLInputElement, { target: { value: "" } });
    await new Promise((r) => setTimeout(r, 60));
    expect(document.body.textContent ?? "").toContain("必填");
    // Clear the API Key (masked row) → required error ([docs/provider-form-rules-tightened](../../../docs/provider-form-rules-tightened.md))
    fireEvent.change(screen.getByDisplayValue("***abcd") as HTMLTextAreaElement, { target: { value: "" } });
    await new Promise((r) => setTimeout(r, 60));
    expect(document.querySelectorAll(".ant-form-item-explain-error").length).toBeGreaterThanOrEqual(2);
    // Delete the only model → model list error ([docs/provider-form-rules-tightened](../../../docs/provider-form-rules-tightened.md))
    const del = document.querySelector(".ant-list-item button.ant-btn-dangerous") as HTMLButtonElement;
    await confirmPopconfirm(del);
    expect(document.querySelector(".provider-models-error")?.textContent).toContain("必填");
  });

  it("添加模型弹窗校验（docs/provider-form-validation）：空 wire id 点确定 → 必填红字且弹窗不关", async () => {
    render(<Harness initial={makeConfig([], null)} />);
    fireEvent.click(buttonByText("添加供应商"));
    fireEvent.click(buttonByText("添加模型"));
    await new Promise((r) => setTimeout(r, 80));
    const okBtn = document.querySelector(".ant-modal .ant-btn-primary") as HTMLButtonElement;
    expect(okBtn.disabled).toBe(false); // no longer blocked by disabling; validated on click instead
    fireEvent.click(okBtn);
    await new Promise((r) => setTimeout(r, 80));
    expect(document.querySelector(".ant-modal")).toBeTruthy();
    expect(document.body.textContent ?? "").toContain("必填");
  });

  it("添加模型表单默认值与字段说明（docs/max-tokens-truncation-fix）：max_tokens 默认 32k + 两字段 help 澄清文案", async () => {
    render(<Harness initial={makeConfig([], null)} />);
    fireEvent.click(buttonByText("添加供应商"));
    await new Promise((r) => setTimeout(r, 60));
    fireEvent.click(buttonByText("添加模型"));
    await new Promise((r) => setTimeout(r, 80));
    // [docs/max-tokens-truncation-fix](../../../docs/max-tokens-truncation-fix.md): new-model default max output tokens is 32768 (8k truncates long agent replies too early)
    const modal = document.querySelector(".ant-modal");
    const numInputs = Array.from(modal?.querySelectorAll("input.ant-input-number-input") ?? []) as HTMLInputElement[];
    expect(numInputs.some((i) => i.value === "32768")).toBe(true);
    // Clarifying copy: max output tokens truncates replies; context window never affects output length
    const text = modal?.textContent ?? "";
    expect(text).toContain("超出即被截断");
    expect(text).toContain("不影响单次回复的输出长度");
  });

  it("删除当前 active 模型：active 回落到同供应商下一个模型", async () => {
    const provider: ProviderConfig = {
      id: "p1", name: "P1", api_format: "openai_chat",
      base_url: "https://a.example/v1", keys: ["sk-x"],
      models: [
        { id: "m1", model: "model-a", max_tokens: 32768, context_window: 128000, reasoning_effort: null, vision: false, video: false },
        { id: "m2", model: "model-b", max_tokens: 32768, context_window: 128000, reasoning_effort: null, vision: false, video: false },
      ],
    };
    render(<Harness initial={makeConfig([provider], "m2")} />);
    fireEvent.click(buttonByText("编辑供应商"));
    await new Promise((r) => setTimeout(r, 60));
    // Delete button on the model-b (active) row
    const row = Array.from(document.querySelectorAll(".ant-list-item")).find((x) =>
      x.textContent?.includes("model-b"),
    )!;
    const del = row.querySelector("button.ant-btn-dangerous") as HTMLButtonElement;
    await confirmPopconfirm(del);
    expect(latest!.providers[0].models.map((m) => m.id)).toEqual(["m1"]);
    expect(latest!.active_model_id).toBe("m1");
  });

  it("删除供应商：providers 移除且 active 清空", async () => {
    const provider: ProviderConfig = {
      id: "p1", name: "P1", api_format: "openai_chat",
      base_url: "https://a.example/v1", keys: ["sk-x"],
      models: [
        { id: "m1", model: "model-a", max_tokens: 32768, context_window: 128000, reasoning_effort: null, vision: false, video: false },
      ],
    };
    render(<Harness initial={makeConfig([provider], "m1")} />);
    // Delete button on the list row (danger button, no text)
    const del = document.querySelector(".ant-list-item button.ant-btn-dangerous") as HTMLButtonElement;
    await confirmPopconfirm(del);
    expect(latest!.providers).toHaveLength(0);
    expect(latest!.active_model_id).toBeNull();
  });
});
