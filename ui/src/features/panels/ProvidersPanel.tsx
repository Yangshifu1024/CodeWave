// 供应商管理面板（[docs/provider-management-refactor](../../../../docs/provider-management-refactor.md)）：管理对象从模型改为供应商——端点 + 协议 + key 池 + 自有模型列表。
// 模型定义完全以用户输入为准（无内置目录）；「添加模型」弹窗覆盖上下文窗口 / 最大输出 / 输入输出类型。
// 注意：子组件必须定义在顶层（定义在组件内部会每次渲染重建组件类型，输入框每敲一键就失焦）。
import { useState } from "react";
import { Button, Card, Divider, Empty, Flex, Input, InputNumber, Modal, Popconfirm, Select, Space, Tag } from "antd";
import {
  DeleteOutlined, EditOutlined, EyeOutlined,
  LockOutlined, PlusOutlined, VideoCameraOutlined,
} from "@ant-design/icons";
import { useTranslation } from "react-i18next";
import { SettingsForm, SettingsFormItem, SettingsSection, SettingsThemeScope } from "./settings/SettingsTheme";
import type { ApiFormat, ConfigState, HeaderPair, ProviderConfig, ProviderModel } from "../../ipc/types";

const API_FORMAT_OPTIONS: { label: string; value: ApiFormat }[] = [
  { label: "OpenAI Chat", value: "openai_chat" },
  { label: "Anthropic Messages", value: "anthropic_messages" },
  { label: "OpenAI Responses", value: "openai_responses" },
];

// 已知官方默认端点：切协议时仅在这些值之间同步；自定义 URL 不受影响
const DEFAULT_BASE_URLS: Record<ApiFormat, string> = {
  openai_chat: "https://api.openai.com/v1",
  anthropic_messages: "https://api.anthropic.com",
  openai_responses: "https://api.openai.com/v1",
};

function apiFormatLabel(v: ApiFormat): string {
  return API_FORMAT_OPTIONS.find((o) => o.value === v)?.label ?? v;
}

function blankProvider(): ProviderConfig {
  return {
    id: crypto.randomUUID(),
    name: "",
    api_format: "openai_chat",
    base_url: "",
    keys: [],
    models: [],
    headers: [],
  };
}

function providerModelDefaults(): ProviderModel {
  return {
    id: crypto.randomUUID(),
    model: "",
    // [docs/max-tokens-truncation-fix](../../../../docs/max-tokens-truncation-fix.md)：与后端 ModelConfig/ProviderModel 默认值对齐（32k；8k 会过早截断长回复）
    max_tokens: 32768,
    context_window: 128000,
    reasoning_effort: null,
    vision: false,
    video: false,
  };
}

function firstModelId(providers: ProviderConfig[]): string | null {
  return providers.flatMap((p) => p.models)[0]?.id ?? null;
}

/** 供应商字段校验（[docs/provider-form-validation](../../../../docs/provider-form-validation.md)/29）：返回结构化问题项；措辞由调用方 i18n 化。
 *  API 格式是带默认值的 Select，结构上恒满足必填（仅保留 required 标记与位置上移）。 */
export type ProviderFieldIssue = { field: "name" | "base_url" | "keys" | "models" | "headers"; kind: "required" | "url" | "header" };

/** 自定义请求头保留名（小写，与后端 `RESERVED_REQUEST_HEADERS` 一致，[docs/provider-custom-headers](../../../../docs/provider-custom-headers.md)）：不允许被覆盖。 */
const RESERVED_HEADER_NAMES = new Set(["content-type", "authorization", "x-api-key", "anthropic-version", "host", "content-length"]);
/** RFC 7230 token 允许集（与后端 `is_http_token` 对齐）。 */
const HTTP_TOKEN_RE = /^[!#$%&'*+.^_`|~0-9A-Za-z-]+$/;

/** 校验自定义请求头：头名非空/合法 token/非保留名/无重名，头值无换行且仅可见 ASCII（与后端 `validate_request_headers` 一致，避免请求期被静默丢弃）。 */
function validateHeaders(headers: HeaderPair[]): boolean {
  const seen = new Set<string>();
  for (const h of headers) {
    const name = h.name.trim();
    // 整行全空（刚点「添加」未填）视为待填占位，跳过；半填行按非法处理
    if (!name && !h.value.trim()) continue;
    if (!name || !HTTP_TOKEN_RE.test(name)) return false;
    const lower = name.toLowerCase();
    if (RESERVED_HEADER_NAMES.has(lower) || seen.has(lower)) return false;
    if (/[\r\n]/.test(h.value)) return false;
    if (!/^[\x20-\x7E]*$/.test(h.value)) return false;
    seen.add(lower);
  }
  return true;
}

/** 校验单个供应商配置：名称/Base URL 必填 + URL 格式 + key 非空（掩码行算已配置）+ 模型列表非空 + 自定义请求头合法。 */
export function validateProvider(p: ProviderConfig): ProviderFieldIssue[] {
  const issues: ProviderFieldIssue[] = [];
  if (!p.name.trim()) issues.push({ field: "name", kind: "required" });
  const url = p.base_url.trim();
  if (!url) issues.push({ field: "base_url", kind: "required" });
  else if (!/^https?:\/\/[^\s/]+/i.test(url)) issues.push({ field: "base_url", kind: "url" });
  // 掩码行（*** 开头）视为已配置（[docs/provider-form-rules-tightened](../../../../docs/provider-form-rules-tightened.md)：API Key 必填）
  if (!p.keys.some((k) => k.trim() !== "")) issues.push({ field: "keys", kind: "required" });
  if (p.models.length === 0) issues.push({ field: "models", kind: "required" });
  if (!validateHeaders(p.headers ?? [])) issues.push({ field: "headers", kind: "header" });
  return issues;
}

/** 供应商弹框内的模型编辑区：模型 CRUD 与供应商字段共用同一层弹框。 */
function ModelEditor(props: {
  value: ProviderModel;
  idError: boolean;
  onChange: (patch: Partial<ProviderModel>) => void;
}) {
  const { value, idError, onChange } = props;
  const { t } = useTranslation();
  const idEmpty = value.model.trim() === "";

  return (
    <div className="settings-provider-model-editor">
      <SettingsForm className="settings-provider-model-form">
        <SettingsFormItem
          label={t("settings.modelId")}
          required
          className="settings-provider-model-field-wide"
          validateStatus={idError && idEmpty ? "error" : undefined}
          help={idError && idEmpty ? t("settings.vRequired") : undefined}
        >
          <Input
            value={value.model}
            placeholder="glm-4.7 / claude-sonnet-4-5 / gpt-4o"
            onChange={(e) => onChange({ model: e.target.value })}
          />
        </SettingsFormItem>
        <SettingsFormItem label={t("settings.contextWindow")} help={t("settings.contextWindowHint")}>
          <InputNumber
            min={1000}
            step={1000}
            value={value.context_window}
            onChange={(v) => onChange({ context_window: v ?? 128000 })}
          />
        </SettingsFormItem>
        <SettingsFormItem label={t("settings.maxTokens")} help={t("settings.maxTokensHint")}>
          <InputNumber
            min={256}
            step={256}
            value={value.max_tokens}
            onChange={(v) => onChange({ max_tokens: v ?? 32768 })}
          />
        </SettingsFormItem>
        <SettingsFormItem label={t("settings.inputTypes")}>
          <Space size={8} wrap>
            <Tag.CheckableTag checked>
              <LockOutlined style={{ marginRight: 4 }} />{t("settings.typeText")}
            </Tag.CheckableTag>
            <Tag.CheckableTag checked={value.vision} onChange={(c) => onChange({ vision: c })}>
              <EyeOutlined style={{ marginRight: 4 }} />{t("settings.typeImage")}
            </Tag.CheckableTag>
            <Tag.CheckableTag checked={value.video} onChange={(c) => onChange({ video: c })}>
              <VideoCameraOutlined style={{ marginRight: 4 }} />{t("settings.typeVideo")}
            </Tag.CheckableTag>
          </Space>
        </SettingsFormItem>
        <SettingsFormItem label={t("settings.outputTypes")}>
          <Tag.CheckableTag checked>
            <LockOutlined style={{ marginRight: 4 }} />{t("settings.typeText")}
          </Tag.CheckableTag>
        </SettingsFormItem>
        <SettingsFormItem label={t("settings.reasoning")} className="settings-provider-model-field-wide">
          <Select
            allowClear
            className="w-narrow"
            value={value.reasoning_effort}
            options={["low", "medium", "high", "max"].map((v) => ({ label: v, value: v }))}
            onChange={(v) => onChange({ reasoning_effort: v ?? null })}
          />
        </SettingsFormItem>
      </SettingsForm>
    </div>
  );
}

/** 供应商表单内的模型列表：行 = wire id + 类型标签 + 窗口/输出摘要 + 当前标记；空态为虚线框 + 添加按钮。 */
function ModelListSection(props: {
  models: ProviderModel[];
  activeModelId: string | null;
  disabled?: boolean;
  onAdd: () => void;
  onEdit: (m: ProviderModel) => void;
  onRemove: (m: ProviderModel) => void;
}) {
  const { models, activeModelId, disabled, onAdd, onEdit, onRemove } = props;
  const { t } = useTranslation();
  if (models.length === 0) {
    return (
      <div style={{ border: "1px dashed", borderRadius: 8, padding: "18px 0", textAlign: "center" }}>
        <div className="dim" style={{ marginBottom: 12 }}>{t("settings.noModels")}</div>
        <Button disabled={disabled} variant="dashed" icon={<PlusOutlined />} onClick={onAdd}>{t("settings.addModel")}</Button>
      </div>
    );
  }
  return (
    <>
      <Flex vertical className="settings-collection-rows" gap={8}>
        {models.map((m) => (
          <div className="settings-collection-row" key={m.id}>
            <Space size={8} wrap>
              <code>{m.model}</code>
              {m.vision && <Tag style={{ marginInlineEnd: 0 }}>{t("settings.typeImage")}</Tag>}
              {m.video && <Tag style={{ marginInlineEnd: 0 }}>{t("settings.typeVideo")}</Tag>}
              <span className="dim">{Math.round(m.context_window / 1000)}k · {Math.round(m.max_tokens / 1000)}k out</span>
              {activeModelId === m.id && (
                <span className="setting-anchor" data-setting-id="active_model_id">
                  <Tag style={{ marginInlineEnd: 0, background: "transparent", borderColor: "var(--ws-accent)", color: "var(--ws-accent)" }}>
                    {t("settings.active")}
                  </Tag>
                </span>
              )}
            </Space>
              <Space>
                <Button disabled={disabled} type="text" icon={<EditOutlined />} onClick={() => onEdit(m)}>{t("settings.editModel")}</Button>
                <Popconfirm title={`${t("common.delete")}「${m.model}」?`} onConfirm={() => onRemove(m)}>
                  <Button disabled={disabled} type="text" danger icon={<DeleteOutlined />}>{t("common.delete")}</Button>
                </Popconfirm>
              </Space>
          </div>
        ))}
      </Flex>
      <Button disabled={disabled} variant="dashed" block icon={<PlusOutlined />} onClick={onAdd} style={{ marginTop: 4 }}>
        {t("settings.addModel")}
      </Button>
    </>
  );
}

/** 供应商字段区（新增/编辑共用，[docs/provider-form-rules-tightened](../../../../docs/provider-form-rules-tightened.md) 顺序）：名称 / API 格式 / Base URL / API Key + 内嵌模型列表。
 *  API 格式位于名称之下——先定协议再同步默认端点；errors 携带已 i18n 化的字段错误文本
 *  （undefined = 不显示），渲染为 Form.Item 红字（[docs/provider-form-validation](../../../../docs/provider-form-validation.md)）。 */
function ProviderFields(props: {
  value: ProviderConfig;
  onPatch: (patch: Partial<ProviderConfig>) => void;
  errors?: { name?: string; base_url?: string; keys?: string; models?: string; headers?: string };
  children: React.ReactNode;
}) {
  const { value, onPatch, errors, children } = props;
  const { t } = useTranslation();
  const headers = value.headers ?? [];
  function patchHeader(index: number, patch: Partial<HeaderPair>) {
    onPatch({ headers: headers.map((h, i) => (i === index ? { ...h, ...patch } : h)) });
  }
  function removeHeader(index: number) {
    onPatch({ headers: headers.filter((_, i) => i !== index) });
  }
  return (
    <>
      <SettingsForm className="settings-provider-form">
        <SettingsFormItem
          label={t("settings.providerName")}
          required
          validateStatus={errors?.name ? "error" : undefined}
          help={errors?.name}
          style={{ marginBottom: 12 }}
        >
          <Input
            className="w-wide"
            value={value.name}
            placeholder={t("settings.providerNamePh")}
            onChange={(e) => onPatch({ name: e.target.value })}
          />
        </SettingsFormItem>
        <SettingsFormItem label={t("settings.apiFormat")} required style={{ marginBottom: 12 }}>
          <Select
            className="w-wide"
            value={value.api_format}
            options={API_FORMAT_OPTIONS}
            onChange={(v) => {
              // 切协议时同步 BASE_URL：仅当当前值为空或已知默认值才替换；自定义 URL 不受影响
              const cur = value.base_url.trim();
              const patch: Partial<ProviderConfig> = { api_format: v };
              if (!cur || Object.values(DEFAULT_BASE_URLS).includes(cur)) {
                patch.base_url = DEFAULT_BASE_URLS[v];
              }
              onPatch(patch);
            }}
          />
        </SettingsFormItem>
        <SettingsFormItem
          label={t("settings.baseUrl")}
          required
          validateStatus={errors?.base_url ? "error" : undefined}
          help={errors?.base_url}
          style={{ marginBottom: 12 }}
        >
          <Input
            className="w-wide"
            value={value.base_url}
            placeholder="https://api.example.com/v1"
            onChange={(e) => onPatch({ base_url: e.target.value })}
          />
        </SettingsFormItem>
        <SettingsFormItem
          label={t("settings.apiKeys")}
          required
          extra={t("settings.keyMaskedHint")}
          validateStatus={errors?.keys ? "error" : undefined}
          help={errors?.keys}
          style={{ marginBottom: 0 }}
        >
          <Input.TextArea
            className="w-wide"
            rows={2}
            value={value.keys.join("\n")}
            onChange={(e) => onPatch({ keys: e.target.value.split("\n").map((s) => s.trim()) })}
          />
        </SettingsFormItem>
        <SettingsFormItem
          label={t("settings.customHeaders")}
          extra={t("settings.customHeadersHint")}
          validateStatus={errors?.headers ? "error" : undefined}
          help={errors?.headers}
          style={{ marginBottom: 0, marginTop: 12 }}
        >
          <Space orientation="vertical" size={8} style={{ width: "100%" }}>
            {headers.map((h, i) => (
              <Space.Compact key={i} style={{ width: "100%" }}>
                <Input
                  style={{ width: "45%" }}
                  value={h.name}
                  placeholder="x-opencode-session"
                  onChange={(e) => patchHeader(i, { name: e.target.value })}
                />
                <Input
                  value={h.value}
                  placeholder="${session_id}"
                  onChange={(e) => patchHeader(i, { value: e.target.value })}
                />
                <Button icon={<DeleteOutlined />} onClick={() => removeHeader(i)} />
              </Space.Compact>
            ))}
            <Button

              variant="dashed"
              icon={<PlusOutlined />}
              onClick={() => onPatch({ headers: [...headers, { name: "", value: "" }] })}
            >
              {t("settings.addHeader")}
            </Button>
          </Space>
        </SettingsFormItem>
      </SettingsForm>
      <Divider style={{ margin: "16px 0 8px" }}>{t("settings.modelList")}</Divider>
      {errors?.models && (
        <div className="provider-models-error">
          {errors.models}
        </div>
      )}
      {children}
    </>
  );
}

interface Props {
  draft: ConfigState;
  patchDraft(patch: Partial<ConfigState>): void;
}

/** 视图状态：供应商列表，或在同一弹框中新增 / 编辑供应商。 */
type View = { kind: "list" } | { kind: "add" } | { kind: "edit"; providerId: string };

/** 供应商页签：新增供应商在确认后并入 draft；编辑直接补丁 draft；模型编辑内嵌在供应商弹框中。 */
export default function ProvidersPanel({ draft, patchDraft }: Props) {
  const { t } = useTranslation();
  const [view, setView] = useState<View>({ kind: "list" });
  // 新增供应商的本地表单（提交前不并入 draft）
  const [addForm, setAddForm] = useState<ProviderConfig>(blankProvider);
  // 编辑也先在弹框内暂存；完成时才写入页面草稿，取消时直接丢弃。
  const [editForm, setEditForm] = useState<ProviderConfig | null>(null);
  // 字段校验展示时机（[docs/provider-form-validation](../../../../docs/provider-form-validation.md)：字段变更后或点击提交才出红字；打开即红太吵）
  const [touched, setTouched] = useState<{ name?: boolean; base_url?: boolean; keys?: boolean }>({});
  const [submitTried, setSubmitTried] = useState(false);
  // 模型步骤与供应商表单复用同一弹框；target 区分新增表单和既有供应商。
  const [modelEditor, setModelEditor] = useState<
    { target: "add-form" | { providerId: string }; value: ProviderModel; isNew: boolean; idError: boolean } | null
  >(null);

  const editing = view.kind === "edit" && editForm?.id === view.providerId ? editForm : null;

  /** 新增表单字段错误文本（touched / 尝试提交后显示；模型列表与请求头无单列 touched，仅由提交尝试触发） */
  function addFieldError(field: "name" | "base_url" | "keys" | "models" | "headers"): string | undefined {
    const issue = validateProvider(addForm).find((e) => e.field === field);
    const fieldTouched = field === "models" || field === "headers" ? false : touched[field];
    if (!issue || (!fieldTouched && !submitTried)) return undefined;
    return headerOrFieldMessage(issue.kind);
  }

  /** 编辑表单字段错误文本（实时显示：进入时值本就有效，只有破坏后才见红字） */
  function editFieldError(field: "name" | "base_url" | "keys" | "models" | "headers"): string | undefined {
    const issue = editing ? validateProvider(editing).find((e) => e.field === field) : undefined;
    if (!issue) return undefined;
    return headerOrFieldMessage(issue.kind);
  }

  function headerOrFieldMessage(kind: ProviderFieldIssue["kind"]): string {
    if (kind === "required") return t("settings.vRequired");
    if (kind === "header") return t("settings.vHeaders");
    return t("settings.vBaseUrl");
  }

  function removeProvider(p: ProviderConfig) {
    const providers = draft.providers.filter((x) => x.id !== p.id);
    const removed = new Set(p.models.map((m) => m.id));
    const activeValid = draft.active_model_id != null && !removed.has(draft.active_model_id);
    patchDraft({ providers, active_model_id: activeValid ? draft.active_model_id : firstModelId(providers) });
    if (view.kind === "edit" && view.providerId === p.id) setView({ kind: "list" });
  }

  function startAddProvider() {
    setAddForm(blankProvider());
    setEditForm(null);
    setTouched({});
    setSubmitTried(false);
    setModelEditor(null);
    setView({ kind: "add" });
  }

  function startAddModel(target: "add-form" | { providerId: string }) {
    setModelEditor({ target, value: providerModelDefaults(), isNew: true, idError: false });
  }

  function startEditModel(target: "add-form" | { providerId: string }, model: ProviderModel) {
    setModelEditor({ target, value: { ...model }, isNew: false, idError: false });
  }

  function removeModel(providerId: string, modelId: string) {
    setEditForm((prev) => prev?.id === providerId
      ? { ...prev, models: prev.models.filter((m) => m.id !== modelId) }
      : prev);
  }

  function commitModel() {
    const editor = modelEditor;
    if (!editor) return;
    const { target, isNew } = editor;
    const model = { ...editor.value, model: editor.value.model.trim() };
    if (!model.model) {
      setModelEditor({ ...editor, idError: true });
      return;
    }
    if (target === "add-form") {
      setAddForm((prev) => ({
        ...prev,
        models: isNew
          ? [...prev.models, model]
          : prev.models.map((x) => (x.id === model.id ? model : x)),
      }));
    } else {
      if (!editing || editing.id !== target.providerId) return;
      const models = isNew
        ? [...editing.models, model]
        : editing.models.map((x) => (x.id === model.id ? model : x));
      setEditForm({ ...editing, models });
    }
    setModelEditor(null);
  }

  const addValid =
    validateProvider(addForm).length === 0 &&
    addForm.models.every((m) => m.model.trim() !== "");

  function submitAdd() {
    setSubmitTried(true); // 先亮出全部字段错误，再做有效性拦截
    if (!addValid) return;
    const provider: ProviderConfig = {
      ...addForm,
      name: addForm.name.trim(),
      base_url: addForm.base_url.trim(),
      keys: addForm.keys.map((s) => s.trim()).filter((s) => s !== ""),
      headers: addForm.headers
        .filter((h) => h.name.trim() !== "" || h.value.trim() !== "")
        .map((h) => ({ name: h.name.trim(), value: h.value.trim() })),
    };
    const providers = [...draft.providers, provider];
    patchDraft({ providers, active_model_id: draft.active_model_id ?? provider.models[0]?.id ?? null });
    setModelEditor(null);
    setView({ kind: "list" });
  }

  const providerModalOpen = view.kind === "add" || (view.kind === "edit" && editing != null);
  const modalProvider = view.kind === "add" ? addForm : editing;
  const modelTarget = view.kind === "add" ? "add-form" : editing ? { providerId: editing.id } : null;

  function closeProviderModal() {
    setModelEditor(null);
    setEditForm(null);
    setView({ kind: "list" });
  }

  function finishProviderModal() {
    if (view.kind === "add") {
      submitAdd();
      return;
    }
    if (view.kind === "edit" && editing) {
      const original = draft.providers.find((provider) => provider.id === editing.id);
      const normalizedOriginal = original && {
        ...original,
        keys: original.keys ?? [],
        headers: original.headers ?? [],
        models: original.models ?? [],
      };
      if (original && JSON.stringify(normalizedOriginal) !== JSON.stringify(editing)) {
        const providers = draft.providers.map((provider) => provider.id === editing.id ? editing : provider);
        const activeModelStillExists = draft.active_model_id == null || providers.some((provider) =>
          provider.models.some((model) => model.id === draft.active_model_id));
        patchDraft({
          providers,
          ...(activeModelStillExists ? {} : { active_model_id: firstModelId(providers) }),
        });
      }
    }
    closeProviderModal();
  }

  function startEditProvider(provider: ProviderConfig) {
    setEditForm({
      ...provider,
      keys: [...(provider.keys ?? [])],
      headers: (provider.headers ?? []).map((header) => ({ ...header })),
      models: (provider.models ?? []).map((model) => ({ ...model })),
    });
    setModelEditor(null);
    setView({ kind: "edit", providerId: provider.id });
  }

  // 列表视图（锚点 data-setting-id="providers"：搜索跳转落点；每个供应商独立成卡）。
  return (
    <div className="setting-anchor settings-collection" data-setting-id="providers">
      <SettingsSection
        title={t("settings.providers")}
        className="settings-provider-section"
        extra={(
          <Button type="primary" icon={<PlusOutlined />} onClick={startAddProvider}>
            {t("settings.addProvider")}
          </Button>
        )}
      >
        <div className="settings-provider-list">
          {draft.providers.length === 0 ? (
            <Empty description={t("settings.noProviders")} className="settings-provider-empty" />
          ) : (
            <div className="settings-provider-cards">
              {draft.providers.map((p) => (
                <Card
                  key={p.id}
                  size="small"
                  variant="outlined"
                  className="settings-provider-card setting-anchor"
                  data-setting-id={`providers.${p.id}`}
                  styles={{ header: { paddingBlock: 12 }, body: { padding: 16 } }}
                  title={(
                    <div className="settings-provider-title-row">
                      <strong className="settings-provider-name">{p.name || p.id}</strong>
                      <Tag className="settings-provider-api-tag">{apiFormatLabel(p.api_format)}</Tag>
                    </div>
                  )}
                  actions={[
                    <Button
                      key="edit"
                      type="text"
                      className="settings-provider-action-button"
                      icon={<EditOutlined />}
                      onClick={() => startEditProvider(p)}
                    >
                      {t("settings.editProviderAction")}
                    </Button>,
                    <Popconfirm
                      key="delete"
                      title={`${t("common.delete")}「${p.name || p.id}」?`}
                      okButtonProps={{ danger: true }}
                      onConfirm={(e) => { e?.stopPropagation(); removeProvider(p); }}
                      onCancel={(e) => e?.stopPropagation()}
                    >
                      <Button
                        type="text"
                        danger
                        className="settings-provider-action-button settings-provider-delete"
                        icon={<DeleteOutlined />}
                        onClick={(e) => e.stopPropagation()}
                      >
                        {t("common.delete")}
                      </Button>
                    </Popconfirm>,
                  ]}
                >
                  <div className="settings-provider-card-content">
                    <div className="settings-provider-endpoint-section">
                      <span className="settings-provider-block-label">{t("settings.apiEndpoint")}</span>
                      <span className="settings-provider-endpoint" title={p.base_url || undefined}>{p.base_url || "-"}</span>
                    </div>
                    <div className="settings-provider-model-section">
                      <span className="settings-provider-block-label">{t("settings.modelList")}</span>
                      <div className="settings-provider-model-list">
                        {p.models.length > 0 ? p.models.map((model) => (
                          <div className="settings-provider-model" key={model.id}>
                            <div className="settings-provider-model-details">
                              <span className="settings-provider-model-name">{model.model}</span>
                              <span className="settings-provider-model-limits">
                                {t("settings.modelSummary", {
                                  context: `${Math.round(model.context_window / 1000)}k`,
                                  output: `${Math.round(model.max_tokens / 1000)}k`,
                                })}
                              </span>
                            </div>
                            <Space size={6} wrap className="settings-provider-model-tags">
                              {model.vision && <Tag className="settings-provider-model-tag">{t("settings.typeImage")}</Tag>}
                              {model.video && <Tag className="settings-provider-model-tag">{t("settings.typeVideo")}</Tag>}
                              {draft.active_model_id === model.id && (
                                <Tag className="settings-provider-model-tag settings-provider-active-tag">{t("settings.active")}</Tag>
                              )}
                            </Space>
                          </div>
                        )) : (
                          <span className="settings-provider-model-empty">{t("settings.noModels")}</span>
                        )}
                      </div>
                    </div>
                  </div>
                </Card>
              ))}
            </div>
          )}
        </div>
      </SettingsSection>
      <Modal
        className="settings-dialog settings-provider-modal"
        open={providerModalOpen}
        title={modelEditor
          ? t(modelEditor.isNew ? "settings.addModel" : "settings.editModel")
          : view.kind === "add" ? t("settings.addProvider") : t("settings.editProvider")}
        width={860}
        footer={(
          <Space>
            {modelEditor ? (
              <>
                <Button onClick={() => setModelEditor(null)}>{t("settings.backToProvider")}</Button>
                <Button type="primary" onClick={commitModel}>{t("settings.saveModel")}</Button>
              </>
            ) : (
              <>
                <Button onClick={closeProviderModal}>{t("common.cancel")}</Button>
                <Button type="primary" onClick={finishProviderModal}>
                  {t(view.kind === "add" ? "settings.addProvider" : "settings.providerDone")}
                </Button>
              </>
            )}
          </Space>
        )}
        onCancel={closeProviderModal}
        destroyOnHidden
      >
        <SettingsThemeScope className="settings-dialog-body">
          {modelEditor ? (
            <ModelEditor
              value={modelEditor.value}
              idError={modelEditor.idError}
              onChange={(patch) => setModelEditor((prev) => prev ? {
                ...prev,
                value: { ...prev.value, ...patch },
              } : prev)}
            />
          ) : (
            <>
              {view.kind === "add" && (
                <div className="dim settings-provider-modal-hint">{t("settings.addProviderHint")}</div>
              )}
              {modalProvider && modelTarget && (
                <ProviderFields
                  value={modalProvider}
                  errors={{
                    name: view.kind === "add" ? addFieldError("name") : editFieldError("name"),
                    base_url: view.kind === "add" ? addFieldError("base_url") : editFieldError("base_url"),
                    keys: view.kind === "add" ? addFieldError("keys") : editFieldError("keys"),
                    models: view.kind === "add" ? addFieldError("models") : editFieldError("models"),
                    headers: view.kind === "add" ? addFieldError("headers") : editFieldError("headers"),
                  }}
                  onPatch={(patch) => {
                    if (view.kind === "add") {
                      setAddForm((prev) => ({ ...prev, ...patch }));
                      if ("name" in patch) setTouched((tp) => ({ ...tp, name: true }));
                      if ("base_url" in patch) setTouched((tp) => ({ ...tp, base_url: true }));
                      if ("keys" in patch) setTouched((tp) => ({ ...tp, keys: true }));
                    } else if (editing) {
                      setEditForm((prev) => prev?.id === editing.id ? { ...prev, ...patch } : prev);
                    }
                  }}
                >
                  <ModelListSection
                    models={modalProvider.models}
                    activeModelId={draft.active_model_id}
                    onAdd={() => startAddModel(modelTarget)}
                    onEdit={(model) => startEditModel(modelTarget, model)}
                    onRemove={(model) => {
                      if (view.kind === "add") {
                        setAddForm((prev) => ({ ...prev, models: prev.models.filter((x) => x.id !== model.id) }));
                      } else if (editing) {
                        removeModel(editing.id, model.id);
                      }
                    }}
                  />
                </ProviderFields>
              )}
            </>
          )}
        </SettingsThemeScope>
      </Modal>
    </div>
  );
}
