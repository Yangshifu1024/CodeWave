// ProvidersPanel interaction tests ([docs/provider-management-refactor](../../../docs/provider-management-refactor.md)): full add-provider flow + active_model_id fallback when deleting models/providers.
// Panel mounted standalone (no AppShell): the draft mirrors SettingsPage's patchDraft merge semantics with local useState.
import { describe, it, expect, beforeAll, afterEach } from "vitest";
import { render, screen, fireEvent, cleanup } from "@testing-library/react";
import { useState } from "react";
import AntApp from "antd/es/app";
import "../i18n"; // Component mounted standalone must init i18next explicitly (no global entry outside App.tsx)
import ProvidersPanel, { validateProvider } from "../features/panels/ProvidersPanel";
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
    post_write_check: { enabled: false, command: "", timeout_seconds: 30, tail_chars: 3000 },
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
    // keys 的提示已改成常驻 extra（不再用 placeholder 承载「掩码」说明），故按字段容器定位 textarea
    const keysBox = Array.from(document.querySelectorAll(".ant-form-item")).find((x) =>
      (x.querySelector(".ant-form-item-label")?.textContent ?? "").includes("API Key"),
    );
    fireEvent.change(keysBox!.querySelector("textarea") as HTMLTextAreaElement, { target: { value: "sk-test-1" } });
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
      headers: [],
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
      headers: [],
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
      headers: [],
    };
    render(<Harness initial={makeConfig([provider], "m1")} />);
    // Delete button on the list row (danger button, no text)
    const del = document.querySelector(".ant-list-item button.ant-btn-dangerous") as HTMLButtonElement;
    await confirmPopconfirm(del);
    expect(latest!.providers).toHaveLength(0);
    expect(latest!.active_model_id).toBeNull();
  });

  it("自定义请求头（[docs/provider-custom-headers](../../../docs/provider-custom-headers.md)）：编辑既有头、添加行入 draft、保留名实时红字拦截", async () => {
    const provider: ProviderConfig = {
      id: "p1", name: "P1", api_format: "openai_chat",
      base_url: "https://a.example/v1", keys: ["sk-x"],
      models: [
        { id: "m1", model: "model-a", max_tokens: 32768, context_window: 128000, reasoning_effort: null, vision: false, video: false },
      ],
      headers: [{ name: "x-opencode-session", value: "${session_id}" }],
    };
    render(<Harness initial={makeConfig([provider], "m1")} />);
    fireEvent.click(buttonByText("编辑供应商"));
    await new Promise((r) => setTimeout(r, 60));
    // 既有头回显
    expect((screen.getByDisplayValue("x-opencode-session") as HTMLInputElement).value).toBe("x-opencode-session");
    expect((screen.getByDisplayValue("${session_id}") as HTMLInputElement).value).toBe("${session_id}");
    // 添加一行并填入保留名 → 实时红字
    fireEvent.click(buttonByText("添加请求头"));
    await new Promise((r) => setTimeout(r, 60));
    const blankName = screen.getAllByPlaceholderText("x-opencode-session").at(-1) as HTMLInputElement;
    fireEvent.change(blankName, { target: { value: "Authorization" } });
    await new Promise((r) => setTimeout(r, 60));
    expect(document.body.textContent ?? "").toContain("保留名");
    // 改为合法名 → 校验通过、随 draft 持久（antd 错误节点离场动画在 happy-dom 不回收，故以校验结果为准）
    fireEvent.change(blankName, { target: { value: "x-custom" } });
    await new Promise((r) => setTimeout(r, 60));
    expect(validateProvider(latest!.providers[0]).some((i) => i.field === "headers")).toBe(false);
    expect(latest!.providers[0].headers.map((h) => h.name)).toEqual(["x-opencode-session", "x-custom"]);
  });
});

describe("validateProvider 自定义请求头校验（docs/provider-custom-headers）", () => {
  function makeProvider(headers: { name: string; value: string }[]): ProviderConfig {
    return {
      id: "p1", name: "P1", api_format: "openai_chat",
      base_url: "https://a.example/v1", keys: ["sk-x"],
      models: [
        { id: "m1", model: "model-a", max_tokens: 32768, context_window: 128000, reasoning_effort: null, vision: false, video: false },
      ],
      headers,
    };
  }

  it("头值含非 ASCII（如中文）→ 标记 headers 问题（与后端 HeaderValue::from_str 对齐）", () => {
    const issues = validateProvider(makeProvider([{ name: "x-custom", value: "值" }]));
    expect(issues.some((i) => i.field === "headers")).toBe(true);
  });

  it("头值含 CRLF → 标记 headers 问题", () => {
    const issues = validateProvider(makeProvider([{ name: "x-custom", value: "a\r\nb" }]));
    expect(issues.some((i) => i.field === "headers")).toBe(true);
  });

  it("整行全空的占位行 → 不标记 headers 问题", () => {
    const issues = validateProvider(makeProvider([{ name: "", value: "" }]));
    expect(issues.some((i) => i.field === "headers")).toBe(false);
  });

  it("既有合法行（含 ${session_id} 模板）→ 不标记 headers 问题", () => {
    const issues = validateProvider(makeProvider([{ name: "x-opencode-session", value: "${session_id}" }]));
    expect(issues.some((i) => i.field === "headers")).toBe(false);
  });
});
