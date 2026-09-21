// 供应商管理面板（[docs/provider-management-refactor](../../../../docs/provider-management-refactor.md)）：管理对象从模型改为供应商——端点 + 协议 + key 池 + 自有模型列表。
// 模型定义完全以用户输入为准（无内置目录）；「添加模型」弹窗覆盖上下文窗口 / 最大输出 / 输入输出类型。
// 注意：子组件必须定义在顶层（定义在组件内部会每次渲染重建组件类型，输入框每敲一键就失焦）。
import { useEffect, useState } from "react";
import { Button, Divider, Empty, Form, Input, InputNumber, List, Modal, Popconfirm, Select, Space, Tag } from "antd";
import {
  ArrowLeftOutlined, DeleteOutlined, EditOutlined, EyeOutlined, InfoCircleOutlined,
  LockOutlined, PlusOutlined, VideoCameraOutlined,
} from "@ant-design/icons";
import { useTranslation } from "react-i18next";
import type { ApiFormat, ConfigState, HeaderPair, ProviderConfig, ProviderModel } from "../../ipc/types";

const API_FORMAT_OPTIONS: { label: string; value: ApiFormat }[] = [
  { label: "OpenAI Chat（兼容 Ollama/DeepSeek/one-api 等）", value: "openai_chat" },
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

/** 添加/编辑模型弹窗（[docs/provider-management-refactor](../../../../docs/provider-management-refactor.md) 交互规格）：wire id + 上下文窗口 / 输出上限 + 输入类型标签 + 输出类型（文本，锁定）。 */
function ModelModal(props: {
  open: boolean;
  initial: ProviderModel | null;
  onOk: (m: ProviderModel) => void;
  onCancel: () => void;
}) {
  const { open, initial, onOk, onCancel } = props;
  const { t } = useTranslation();
  const [form, setForm] = useState<ProviderModel>(initial ?? providerModelDefaults());
  // 点击确定后才显示必填红字（打开即红太吵）；输入后自动消除
  const [idError, setIdError] = useState(false);

  useEffect(() => {
    if (open) {
      setForm(initial ?? providerModelDefaults());
      setIdError(false);
    }
  }, [open, initial]);

  const idTrimmed = form.model.trim();
  return (
    <Modal
      open={open}
      title={initial ? t("settings.editModel") : t("settings.addModel")}
      width={520}
      okText={t("common.save")}
      cancelText={t("common.cancel")}
      onOk={() => {
        if (!idTrimmed) {
          setIdError(true);
          return;
        }
        onOk({ ...form, model: idTrimmed });
      }}
      onCancel={onCancel}
      destroyOnHidden
    >
      <Form layout="vertical" style={{ marginTop: 16 }}>
        <Form.Item
          label={t("settings.modelId")}
          required
          validateStatus={idError && !idTrimmed ? "error" : undefined}
          help={idError && !idTrimmed ? t("settings.vRequired") : undefined}
        >
          <Input
            value={form.model}
            placeholder="glm-4.7 / claude-sonnet-4-5 / gpt-4o"
            onChange={(e) => setForm({ ...form, model: e.target.value })}
          />
        </Form.Item>
        <Space size={16} wrap>
          <Form.Item label={t("settings.contextWindow")} help={t("settings.contextWindowHint")} style={{ marginBottom: 0 }}>
            <InputNumber
              min={1000}
              step={1000}
              value={form.context_window}
              onChange={(v) => setForm({ ...form, context_window: v ?? 128000 })}
            />
          </Form.Item>
          <Form.Item label={t("settings.maxTokens")} help={t("settings.maxTokensHint")} style={{ marginBottom: 0 }}>
            <InputNumber
              min={256}
              step={256}
              value={form.max_tokens}
              onChange={(v) => setForm({ ...form, max_tokens: v ?? 32768 })}
            />
          </Form.Item>
        </Space>
        <Form.Item label={t("settings.inputTypes")} style={{ marginTop: 20, marginBottom: 12 }}>
          <Space size={8} wrap>
            {/* 文本输入恒开启（锁定） */}
            <Tag.CheckableTag checked>
              <LockOutlined style={{ marginRight: 4 }} />{t("settings.typeText")}
            </Tag.CheckableTag>
            <Tag.CheckableTag checked={form.vision} onChange={(c) => setForm({ ...form, vision: c })}>
              <EyeOutlined style={{ marginRight: 4 }} />{t("settings.typeImage")}
            </Tag.CheckableTag>
            <Tag.CheckableTag checked={form.video} onChange={(c) => setForm({ ...form, video: c })}>
              <VideoCameraOutlined style={{ marginRight: 4 }} />{t("settings.typeVideo")}
            </Tag.CheckableTag>
          </Space>
        </Form.Item>
        <Form.Item label={t("settings.outputTypes")} style={{ marginBottom: 12 }}>
          <Tag.CheckableTag checked>
            <LockOutlined style={{ marginRight: 4 }} />{t("settings.typeText")}
          </Tag.CheckableTag>
        </Form.Item>
        <Form.Item label={t("settings.reasoning")} style={{ marginBottom: 0 }}>
          <Select
            allowClear
            className="w-narrow"
            value={form.reasoning_effort}
            options={["low", "medium", "high", "max"].map((v) => ({ label: v, value: v }))}
            onChange={(v) => setForm({ ...form, reasoning_effort: v ?? null })}
          />
        </Form.Item>
      </Form>
    </Modal>
  );
}

/** 供应商表单内的模型列表：行 = wire id + 类型标签 + 窗口/输出摘要 + 当前标记；空态为虚线框 + 添加按钮。 */
function ModelListSection(props: {
  models: ProviderModel[];
  activeModelId: string | null;
  onAdd: () => void;
  onEdit: (m: ProviderModel) => void;
  onRemove: (m: ProviderModel) => void;
}) {
  const { models, activeModelId, onAdd, onEdit, onRemove } = props;
  const { t } = useTranslation();
  if (models.length === 0) {
    return (
      <div style={{ border: "1px dashed", borderRadius: 8, padding: "18px 0", textAlign: "center" }}>
        <div className="dim" style={{ marginBottom: 12 }}>{t("settings.noModels")}</div>
        <Button variant="dashed" icon={<PlusOutlined />} onClick={onAdd}>{t("settings.addModel")}</Button>
      </div>
    );
  }
  return (
    <>
      <List
        size="small"
        dataSource={models}
        split={false}
        renderItem={(m) => (
          <List.Item
            actions={[
              <Button key="edit" size="small" type="text" icon={<EditOutlined />} onClick={() => onEdit(m)}>
                {t("settings.editModel")}
              </Button>,
              <Popconfirm key="del" title={`${t("common.delete")}「${m.model}」?`} onConfirm={() => onRemove(m)}>
                <Button size="small" type="text" danger icon={<DeleteOutlined />} />
              </Popconfirm>,
            ]}
          >
            <Space size={8} wrap>
              <code>{m.model}</code>
              {m.vision && <Tag style={{ marginInlineEnd: 0 }}>{t("settings.typeImage")}</Tag>}
              {m.video && <Tag style={{ marginInlineEnd: 0 }}>{t("settings.typeVideo")}</Tag>}
              <span className="dim" style={{ fontSize: 12 }}>
                {Math.round(m.context_window / 1000)}k · {Math.round(m.max_tokens / 1000)}k out
              </span>
              {activeModelId === m.id && (
                /* [docs/ask-ink-accent-and-composer-cover](../../../../docs/ask-ink-accent-and-composer-cover.md) 墨色化：去掉 preset 蓝（processing），改为与全应用强调色一致的描边墨色。
                  锚点 data-setting-id = 注册表 id（批③ 搜索定位）。
                  本标记**不是**进阶项（2026-09-19 用户反馈修正）：它只存在于「编辑/新建供应商」视图里，
                  页级「显示进阶项」开关在列表视图对它无能为力（打开也没东西可变），故不再折叠 */
                <span className="setting-anchor" data-setting-id="active_model_id">
                  <Tag style={{ marginInlineEnd: 0, background: "transparent", borderColor: "var(--ws-accent)", color: "var(--ws-accent)" }}>
                    {t("settings.active")}
                  </Tag>
                </span>
              )}
            </Space>
          </List.Item>
        )}
      />
      <Button size="small" variant="dashed" block icon={<PlusOutlined />} onClick={onAdd} style={{ marginTop: 4 }}>
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
      <Form layout="vertical">
        <Form.Item
          label={t("settings.providerName")}
          required
          validateStatus={errors?.name ? "error" : undefined}
          help={errors?.name}
          style={{ marginBottom: 12 }}
        >
          <Input
            value={value.name}
            placeholder={t("settings.providerNamePh")}
            onChange={(e) => onPatch({ name: e.target.value })}
          />
        </Form.Item>
        <Form.Item label={t("settings.apiFormat")} required style={{ marginBottom: 12 }}>
          <Select
            style={{ maxWidth: 420 }}
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
        </Form.Item>
        <Form.Item
          label={t("settings.baseUrl")}
          required
          validateStatus={errors?.base_url ? "error" : undefined}
          help={errors?.base_url}
          style={{ marginBottom: 12 }}
        >
          <Input
            value={value.base_url}
            placeholder="https://api.example.com/v1"
            onChange={(e) => onPatch({ base_url: e.target.value })}
          />
        </Form.Item>
        <Form.Item
          label={t("settings.apiKeys")}
          required
          extra={t("settings.keyMaskedHint")}
          validateStatus={errors?.keys ? "error" : undefined}
          help={errors?.keys}
          style={{ marginBottom: 0 }}
        >
          <Input.TextArea
            rows={2}
            value={value.keys.join("\n")}
            onChange={(e) => onPatch({ keys: e.target.value.split("\n").map((s) => s.trim()) })}
          />
        </Form.Item>
        <Form.Item
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
              size="small"
              variant="dashed"
              icon={<PlusOutlined />}
              onClick={() => onPatch({ headers: [...headers, { name: "", value: "" }] })}
            >
              {t("settings.addHeader")}
            </Button>
          </Space>
        </Form.Item>
      </Form>
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

/** 视图状态：列表 / 新增供应商（本地表单，提交时整体并入 draft）/ 编辑既有供应商（直接改 draft）。 */
type View = { kind: "list" } | { kind: "add" } | { kind: "edit"; providerId: string };

/** 供应商页签：列表 / 新增 / 编辑三视图。新增用本地表单（提交才并入 draft），编辑直接补丁 draft；
 *  模型增删改经 ModelModal，删除供应商/模型时回收 active_model_id。 */
export default function ProvidersPanel({ draft, patchDraft }: Props) {
  const { t } = useTranslation();
  const [view, setView] = useState<View>({ kind: "list" });
  // 新增供应商的本地表单（提交前不并入 draft）
  const [addForm, setAddForm] = useState<ProviderConfig>(blankProvider);
  // 字段校验展示时机（[docs/provider-form-validation](../../../../docs/provider-form-validation.md)：字段变更后或点击提交才出红字；打开即红太吵）
  const [touched, setTouched] = useState<{ name?: boolean; base_url?: boolean; keys?: boolean }>({});
  const [submitTried, setSubmitTried] = useState(false);
  // 添加/编辑模型弹窗：target 区分提交到新增表单还是 draft 中某供应商
  const [modelModal, setModelModal] = useState<
    { target: "add-form" | { providerId: string }; editing: ProviderModel | null } | null
  >(null);

  const editing = view.kind === "edit" ? draft.providers.find((p) => p.id === view.providerId) : null;

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

  function patchProvider(id: string, patch: Partial<ProviderConfig>) {
    patchDraft({
      providers: draft.providers.map((p) => (p.id === id ? { ...p, ...patch } : p)),
    });
  }

  function removeProvider(p: ProviderConfig) {
    const providers = draft.providers.filter((x) => x.id !== p.id);
    const removed = new Set(p.models.map((m) => m.id));
    const activeValid = draft.active_model_id != null && !removed.has(draft.active_model_id);
    patchDraft({ providers, active_model_id: activeValid ? draft.active_model_id : firstModelId(providers) });
    if (view.kind === "edit" && view.providerId === p.id) setView({ kind: "list" });
  }

  function removeModel(providerId: string, modelId: string) {
    const provider = draft.providers.find((p) => p.id === providerId);
    if (!provider) return;
    const models = provider.models.filter((m) => m.id !== modelId);
    patchProvider(providerId, { models });
    if (draft.active_model_id === modelId) {
      const others = draft.providers.filter((p) => p.id !== providerId);
      patchDraft({ active_model_id: models[0]?.id ?? firstModelId(others) });
    }
  }

  function commitModel(m: ProviderModel) {
    const mm = modelModal;
    if (!mm) return;
    const { target } = mm;
    if (target === "add-form") {
      setAddForm((prev) => ({
        ...prev,
        models: mm.editing
          ? prev.models.map((x) => (x.id === m.id ? m : x))
          : [...prev.models, m],
      }));
    } else {
      const provider = draft.providers.find((p) => p.id === target.providerId);
      if (!provider) return;
      const models = mm.editing
        ? provider.models.map((x) => (x.id === m.id ? m : x))
        : [...provider.models, m];
      patchProvider(target.providerId, { models });
    }
    setModelModal(null);
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
    setView({ kind: "edit", providerId: provider.id });
  }

  const modelModalNode = (
    <ModelModal
      open={!!modelModal}
      initial={modelModal?.editing ?? null}
      onOk={commitModel}
      onCancel={() => setModelModal(null)}
    />
  );

  if (view.kind === "add") {
    return (
      <div className="setting-anchor" data-setting-id="providers" style={{ maxWidth: 560 }}>
        <div style={{ display: "flex", alignItems: "center", gap: 8, marginBottom: 12 }}>
          <Button size="small" type="text" icon={<ArrowLeftOutlined />} onClick={() => setView({ kind: "list" })} />
          <b>{t("settings.addProvider")}</b>
        </div>
        <div className="dim" style={{ marginBottom: 12, fontSize: 12.5 }}>{t("settings.addProviderHint")}</div>
        <ProviderFields
          value={addForm}
          errors={{
            name: addFieldError("name"),
            base_url: addFieldError("base_url"),
            keys: addFieldError("keys"),
            models: addFieldError("models"),
            headers: addFieldError("headers"),
          }}
          onPatch={(patch) => {
            setAddForm((prev) => ({ ...prev, ...patch }));
            if ("name" in patch) setTouched((tp) => ({ ...tp, name: true }));
            if ("base_url" in patch) setTouched((tp) => ({ ...tp, base_url: true }));
            if ("keys" in patch) setTouched((tp) => ({ ...tp, keys: true }));
          }}
        >
          <ModelListSection
            models={addForm.models}
            activeModelId={draft.active_model_id}
            onAdd={() => setModelModal({ target: "add-form", editing: null })}
            onEdit={(m) => setModelModal({ target: "add-form", editing: m })}
            onRemove={(m) => setAddForm((prev) => ({ ...prev, models: prev.models.filter((x) => x.id !== m.id) }))}
          />
        </ProviderFields>
        <Divider style={{ margin: "16px 0 12px" }} />
        <div style={{ display: "flex", alignItems: "center", justifyContent: "space-between", gap: 12 }}>
          <span className="dim" style={{ fontSize: 12.5 }}>
            <InfoCircleOutlined style={{ marginRight: 4 }} />
            {t("settings.providerNeedsModel")}
          </span>
          <Button type="primary" onClick={submitAdd}>
            {t("settings.addProvider")}
          </Button>
        </div>
        {modelModalNode}
      </div>
    );
  }

  if (view.kind === "edit" && editing) {
    return (
      <div className="setting-anchor" data-setting-id="providers" style={{ maxWidth: 560 }}>
        <div style={{ display: "flex", alignItems: "center", gap: 8, marginBottom: 12 }}>
          <Button size="small" type="text" icon={<ArrowLeftOutlined />} onClick={() => setView({ kind: "list" })} />
          <b>{t("settings.editProvider")}</b>
          <div className="flex" />
          <Popconfirm
            title={`${t("common.delete")}「${editing.name || editing.id}」?`}
            onConfirm={() => removeProvider(editing)}
          >
            <Button size="small" type="text" danger icon={<DeleteOutlined />}>{t("common.delete")}</Button>
          </Popconfirm>
        </div>
        <ProviderFields
          value={editing}
          errors={{
            name: editFieldError("name"),
            base_url: editFieldError("base_url"),
            keys: editFieldError("keys"),
            models: editFieldError("models"),
            headers: editFieldError("headers"),
          }}
          onPatch={(patch) => patchProvider(editing.id, patch)}
        >
          <ModelListSection
            models={editing.models}
            activeModelId={draft.active_model_id}
            onAdd={() => setModelModal({ target: { providerId: editing.id }, editing: null })}
            onEdit={(m) => setModelModal({ target: { providerId: editing.id }, editing: m })}
            onRemove={(m) => removeModel(editing.id, m.id)}
          />
        </ProviderFields>
        {modelModalNode}
      </div>
    );
  }

  // 列表视图（锚点 data-setting-id="providers"：搜索跳转落点；容器宽度 maxWidth 640 不变——本批明确非目标）
  return (
    <div className="setting-anchor" data-setting-id="providers" style={{ maxWidth: 640 }}>
      {draft.providers.length === 0 && (
        <Empty description={t("settings.noProviders")} style={{ margin: "24px 0" }} />
      )}
      <List
        dataSource={draft.providers}
        split={false}
        renderItem={(p) => (
          <List.Item
            /* 行级动态锚点：命名空间 `providers.<uuid>`（与三个视图根节点的 `providers`、
               以及注册表项的静态锚点都不同名）。外部入口（额度灰行的「去设置」）靠它定位到具体一行；
               类名沿用 `.setting-anchor` 包裹层约定（只补 min-width）。列表行为不变：整行点击仍进编辑视图。 */
            className="setting-anchor"
            data-setting-id={`providers.${p.id}`}
            style={{ cursor: "pointer", padding: "10px 4px" }}
            onClick={() => setView({ kind: "edit", providerId: p.id })}
            actions={[
              <Button
                key="edit"
                size="small"
                type="text"
                icon={<EditOutlined />}
                onClick={(e) => {
                  e.stopPropagation();
                  setView({ kind: "edit", providerId: p.id });
                }}
              >
                {t("settings.editProvider")}
              </Button>,
              <Popconfirm
                key="del"
                title={`${t("common.delete")}「${p.name || p.id}」?`}
                onConfirm={(e) => {
                  e?.stopPropagation();
                  removeProvider(p);
                }}
                onCancel={(e) => e?.stopPropagation()}
              >
                <Button
                  size="small"
                  type="text"
                  danger
                  icon={<DeleteOutlined />}
                  onClick={(e) => e.stopPropagation()}
                />
              </Popconfirm>,
            ]}
          >
            <List.Item.Meta
              title={
                <Space size={8}>
                  <b>{p.name || p.id}</b>
                  <Tag style={{ marginInlineEnd: 0 }}>{apiFormatLabel(p.api_format)}</Tag>
                </Space>
              }
              description={
                <span>
                  {p.base_url || "-"} · {t("settings.modelsCount", { n: p.models.length })}
                </span>
              }
            />
          </List.Item>
        )}
      />
      <Button
        variant="dashed"
        block
        icon={<PlusOutlined />}
        onClick={() => {
          setAddForm(blankProvider());
          setTouched({});
          setSubmitTried(false);
          setView({ kind: "add" });
        }}
      >
        {t("settings.addProvider")}
      </Button>
      {modelModalNode}
    </div>
  );
}
