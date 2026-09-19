// 设置页（全屏覆盖式，[docs/settings-fullscreen-shell](../../../../docs/settings-fullscreen-shell.md)）：
// 从 SettingsModal 迁入——去掉 Modal 外壳与 footer，改为绝对定位贴在内层 Layout 上的全屏页；
// 7 页签、draft/save 全量提交语义不变。
// 本文件承担三件事：
//   1. 容器：左导航列（返回工作区 + 运行中指示 + 7 页导航）+ 右内容列（操作条 + 页体）；
//   2. 逐页脏标记（draft 与已保存配置的差集，即时生效项不打点）；
//   3. 离开拦截（切页 / 返回 / 页内 Esc / 关窗退出四条路径共用同一份三选弹框）。
import { useEffect, useMemo, useRef, useState } from "react";
import type { ReactNode } from "react";
import {
  App, Button, Divider, Empty, Form, Input, InputNumber, Modal, Popconfirm, Radio, Select, Slider, Switch, Tabs, Tooltip, Typography,
} from "antd";
import { ArrowLeftOutlined, DeleteOutlined } from "@ant-design/icons";
import { useTranslation } from "react-i18next";
import { ipc } from "../../ipc/client";
import { DEFAULT_LSP_SETTINGS, LSP_LANGUAGES, lspCommandOf, withLspCommand } from "../../ipc/types";
import type { ConfigState, LspLanguage, LspServerStatus, ShellInfo, SkillMeta, ValidationSettings } from "../../ipc/types";
import { originLabel } from "../../utils/skills";
import { clampNavWidth } from "../../utils/layout";
import { respondExitRequest } from "../../utils/uiState";
import { useActiveId } from "../../stores/sessions";
import { useRun } from "../../stores/run";
import { useSettings } from "../../stores/settings";
import { useUi } from "../../stores/ui";
import { checkForUpdates, useAutoUpdateSetting } from "../../utils/updateCheck";
import { useDisplayWidths } from "../shell/useDisplayWidths";
import { AppearanceSettings } from "./FontSettings";
import ProvidersPanel, { validateProvider } from "./ProvidersPanel";

const { TextArea } = Input;

/** 自定义代理地址前缀白名单（与后端 reqwest 支持一致；保存校验用） */
const PROXY_URL_RE = /^(https?|socks5h?):\/\//;

/** 设置页 7 个分区（本批沿用既有归属；8 页重划 / 注册表 / 自建导航都是后续批次） */
type PageKey = "general" | "appearance" | "providers" | "security" | "network" | "mcp" | "skills";

const PAGE_ORDER: PageKey[] = ["general", "appearance", "providers", "security", "network", "mcp", "skills"];

/** 左导航列基准宽（= 工作区左栏默认宽 280，复用同一套栏宽度量） */
const SETTINGS_NAV_W = 280;
/** 窄窗收缩比例：导航列随窗口宽度收缩，下限/上限由 clampNavWidth（180/480）兜底 */
const SETTINGS_NAV_RATIO = 0.32;

/**
 * 逐页脏判定的比较片段：只取该页真正写进 draft 的配置键。
 * 即时生效项（界面语言 ui.language、自动更新开关）刻意**不**纳入——它们改完立即生效，不该亮脏点（
 * 界面语言同时写 useUi 与 draft.ui.language，纳入就会永远显示未保存）；
 * 外观页的字体/主题走 localStorage（FontSettings），配置侧只有 font_size/accent，本批没有编辑入口。
 * MCP 不在 config 里（独立 mcp.json），脏判定在组件内单独算。
 */
function pageSlice(c: ConfigState, key: PageKey): unknown {
  switch (key) {
    case "general":
      return {
        ai_language: c.ui.ai_language ?? null,
        compact_threshold: c.compact_threshold,
        compact_timeout_seconds: c.compact_timeout_seconds,
        custom_prompt: c.custom_prompt,
        log: c.log,
        shell: c.shell,
      };
    case "appearance":
      return { font_size: c.ui.font_size, accent: c.ui.accent };
    case "providers":
      return { providers: c.providers, active_model_id: c.active_model_id };
    case "security":
      // validation 段的缺省语义与后端 serde default 对齐：java 缺省 false、dart 缺省 true、
      // lsp 缺省 = DEFAULT_LSP_SETTINGS（后端 `LspSettings::default()` 的同形镜像）。
      // 不折缺省时：把 java 开关打开又关掉、或点开任意 LSP 字段后改回原样，都会永远显示未保存。
      return {
        approval: c.approval,
        validation: {
          ...c.validation,
          java: c.validation.java ?? false,
          dart: c.validation.dart ?? true,
          lsp: c.validation.lsp ?? DEFAULT_LSP_SETTINGS,
        },
      };
    case "network":
      // proxy = null 等价于 ProxyConfig::default()（mode = system，[docs/network-proxy-settings]）：
      // 折成默认对象再比，否则打开设置后点一下本来就处于选中态的「系统代理」卡片就凭空染脏。
      return { proxy: c.proxy ?? { mode: "system", url: "" }, network: c.network };
    case "skills":
      return { disabled_skills: c.disabled_skills };
    case "mcp":
      return null;
  }
}

/**
 * 脏比较归一：把「空值三态」（`null` / `undefined` / `""` / 纯空白串）折叠成同一形态，字符串顺带 trim
 * ——保存路径本身就会 trim 大部分自由文本字段（见 `save()`：代理地址 / 自定义请求头 / JDK 路径 /
 * 命令覆盖 / 额外 SDK 根）。递归处理对象与数组，对象键排序保证键序不影响比较。
 *
 * 不归一时：把「自定义提示词」的文字删空后 draft 是 `""`、存量配置是 `null`，两侧永远不等 →
 * 脏点常亮、「返回工作区」误弹三选、点「保存并离开」还会把 `custom_prompt: ""` 落盘。
 * 数组里归一后为空的条目直接丢弃、全空对象视作空值（保存路径同样会丢：空 root / 空 key / 空请求头都不落盘）
 * ——所以「点一下添加按钮又什么都没填」不会被判成未保存改动。
 */
function normalizeForCompare(value: unknown): unknown {
  if (value === null || value === undefined) return null;
  if (typeof value === "string") {
    const trimmed = value.trim();
    return trimmed === "" ? null : trimmed;
  }
  if (Array.isArray(value)) {
    return value.map(normalizeForCompare).filter((v) => v !== null);
  }
  if (typeof value === "object") {
    const src = value as Record<string, unknown>;
    const out: Record<string, unknown> = {};
    for (const key of Object.keys(src).sort()) out[key] = normalizeForCompare(src[key]);
    // 全空对象视作空值（再被数组丢弃）：典型是「加一行自定义请求头」但没填任何东西
    // ——save() 也会把无名行丢掉，此时不该算改过。
    return Object.values(out).every((v) => v === null) ? null : out;
  }
  return value;
}

/** 两段配置片段是否等价（先按空值三态归一，再比 JSON；键序无关） */
function sameSlice(a: unknown, b: unknown): boolean {
  return JSON.stringify(normalizeForCompare(a)) === JSON.stringify(normalizeForCompare(b));
}

/** 离开拦截意图：切页（带目标页）或「返回工作区」/页内 Esc；关窗退出由 ui.exitRequest 驱动 */
type LeaveIntent = { kind: "tab"; tab: string } | { kind: "leave" };

/**
 * 页内 Esc 前的浮层探测：Select/Dropdown/Popover/Drawer 展开时，Esc 先归组件库（先关浮层，不平级返回）。
 * Drawer 也算浮层：子代理过程抽屉是 Portal 到 body 的 drawer，漏掉它会与设置页的 Esc 双响应
 * （同一按 Esc 既关抽屉又返回工作区）。
 */
function overlayOpen(): boolean {
  return !!document.querySelector(
    ".ant-select-dropdown:not(.ant-select-dropdown-hidden), .ant-dropdown:not(.ant-dropdown-hidden), .ant-popover:not(.ant-popover-hidden), .ant-drawer:not(.ant-drawer-hidden)",
  );
}

// MCP 条目结构化视图（文件形态 {"mcpServers":{name:cfg}} 的前端呈现）
interface McpEntry {
  name: string;
  transport: "stdio" | "streamable_http";
  command: string;
  argsText: string;
  envText: string;
  url: string;
}

/** 把 mcpServers JSON 文本解析为结构化条目；格式非法返回 null（调用方回退原文本模式）。 */
function parseMcpEntries(raw: string): McpEntry[] | null {
  try {
    const parsed = JSON.parse(raw);
    const servers = parsed?.mcpServers ?? {};
    if (typeof servers !== "object" || Array.isArray(servers)) return null;
    return Object.entries(servers as Record<string, any>).map(([name, cfg]) => ({
      name,
      transport: cfg?.transport === "streamable_http" ? "streamable_http" : "stdio",
      command: typeof cfg?.command === "string" ? cfg.command : "",
      argsText: Array.isArray(cfg?.args) ? cfg.args.map(String).join(" ") : "",
      envText: Object.entries((cfg?.env ?? {}) as Record<string, string>)
        .map(([k, v]) => `${k}=${v}`)
        .join("\n"),
      url: typeof cfg?.url === "string" ? cfg.url : "",
    }));
  } catch {
    return null;
  }
}

/** 把结构化条目序列化回 mcpServers JSON 文本（未命名条目跳过）。 */
function serializeMcpEntries(entries: McpEntry[]): string {
  const servers: Record<string, any> = {};
  for (const e of entries) {
    const name = e.name.trim();
    if (!name) continue; // 跳过未命名条目
    if (e.transport === "streamable_http") {
      servers[name] = { transport: "streamable_http", url: e.url.trim() };
    } else {
      const env: Record<string, string> = {};
      for (const line of e.envText.split("\n")) {
        const idx = line.indexOf("=");
        if (idx > 0) env[line.slice(0, idx).trim()] = line.slice(idx + 1).trim();
      }
      servers[name] = {
        transport: "stdio",
        command: e.command.trim(),
        args: e.argsText.split(/\s+/).filter(Boolean),
        env,
      };
    }
  }
  return JSON.stringify({ mcpServers: servers }, null, 2);
}

/** Shell 路径回显三态：path = 可执行文件绝对路径；placeholder = 所选 shell 无固定路径（如 WSL）；
 *  null = 不显示回显（探测失败 / 所选 shell 已卸载 / auto 探测项无 path，均有既有警示文案兜底）。 */
type ShellDisplay = { kind: "path"; text: string } | { kind: "placeholder" } | null;

/** 依 draft 的 shell.selection 与探测列表解析回显内容；auto（selection=null）取探测列表 auto 项的 path
 *  （与「自动（默认：X）」标注同源，即执行时实际所用 shell）。纯函数不发 IPC。 */
function resolveShellDisplay(selection: string | null | undefined, shells: ShellInfo[] | null): ShellDisplay {
  if (!shells) return null; // 探测失败：不显示路径（shellDetectFailed 警示已覆盖）
  const current = selection ? shells.find((s) => s.id === selection) : shells.find((s) => s.auto);
  if (!current) return null; // 所选 shell 已卸载 / auto 探测项缺失：shellNotDetected 警示已覆盖
  if (!current.path) return { kind: "placeholder" }; // WSL 等无固定可执行文件
  return { kind: "path", text: current.path };
}

/** 语言 → i18n 展示名（行序与后端 `Lang::all()` 一致：typescript、rust、python、go、java、dart） */
const LANG_LABEL_KEY: Record<LspLanguage, string> = {
  typescript: "settings.validationLangTypescript",
  rust: "settings.validationLangRust",
  python: "settings.validationLangPython",
  go: "settings.validationLangGo",
  java: "settings.validationLangJava",
  dart: "settings.validationLangDart",
};

/** 设置页：7 个分区（通用 / 外观 / 供应商 / 安全 / 网络 / MCP / 技能）。draft 只改内存、「保存」一次性提交；
 *  供应商校验失败报错并跳转页签不落盘；MCP 支持结构化条目与原文本兜底双模式。
 *  容器是全屏覆盖层（绝对定位贴在内层 Layout），工作区只隐藏不卸载——运行中会话的 DOM 与滚动容器不受影响。 */export default function SettingsPage() {
  const { t } = useTranslation();
  const { message } = App.useApp();
  const language = useUi((s) => s.language);
  const sessionId = useActiveId();
  // 窗口宽度：导航列宽按它收缩（复用工作区同一份显示宽度决议）
  const { windowWidth } = useDisplayWidths();

  const [draft, setDraft] = useState<ConfigState | null>(null);
  const [saving, setSaving] = useState(false);
  const [skills, setSkills] = useState<SkillMeta[]>([]);
  // 技能区异步操作 loading：reloadSkills 全局、删除按行（Popconfirm 确认按钮 loading）
  const [skillsBusy, setSkillsBusy] = useState(false);
  const [deletingName, setDeletingName] = useState<string | null>(null);
  // 受控页签：上收到 useUi（[docs/auth-error-guidance](../../../../docs/auth-error-guidance.md)），外部可指定页签打开设置页；
  // 下方保存校验跳页也走同一 store 状态（[docs/provider-form-validation](../../../../docs/provider-form-validation.md)）
  const tab = useUi((s) => s.settingsTab);
  const setTab = (t: string) => useUi.setState({ settingsTab: t });
  // MCP：结构化条目；null = 原 JSON 解析失败，回退 textarea 模式避免丢配置
  const [mcpEntries, setMcpEntries] = useState<McpEntry[] | null>(null);
  const [mcpRaw, setMcpRaw] = useState("");
  /** 打开时的 MCP 文本基线（已归一化），用于逐页脏判定与「放弃改动」回退 */
  const [mcpOriginal, setMcpOriginal] = useState("");
  // shell 探测：null = 探测失败（仅显示「自动」+ 失败提示），[] = 探测成功但无可用项
  const [shells, setShells] = useState<ShellInfo[] | null>(null);
  // 系统代理探测回显（resolve_proxy 命令）：undefined = 未拉取，null = 未检测到
  const [sysProxy, setSysProxy] = useState<string | null | undefined>(undefined);
  // LSP server 状态（lsp_status）：null = 未取到（探测失败/旧后端）→ 不显示状态徽标，面板不报错
  const [lspStatus, setLspStatus] = useState<LspServerStatus[] | null>(null);
  const [redetecting, setRedetecting] = useState(false);

  useEffect(() => {
    void (async () => {
      const config = useSettings.getState().config;
      if (!config) return;
      const cloned: ConfigState = JSON.parse(JSON.stringify(config));
      cloned.ui.language = useUi.getState().language;
      setDraft(cloned);
      const raw = await ipc.getMcpConfig().catch(() => "");
      const parsed = parseMcpEntries(raw);
      // 基线用归一化后的文本：否则「结构化条目重序列化与原文格式差异」会被误判成脏改动
      const normalized = parsed ? serializeMcpEntries(parsed) : raw;
      setMcpEntries(parsed ?? []);
      setMcpRaw(normalized);
      setMcpOriginal(normalized);
      setSkills(await ipc.listSkills(sessionId).catch(() => []));
      const st = await ipc.mcpStatus().catch(() => []);
      useUi.setState({ mcpStatus: st });
      // shell 探测失败不阻塞面板：仅回退「自动」选项 + 失败提示
      setShells(await ipc.listAvailableShells().catch(() => null));
      // 系统代理探测回显：失败不阻塞（null = 未检测到提示）
      setSysProxy(await ipc.resolveProxy().catch(() => null));
      // LSP server 状态：失败静默降级为不显示徽标（设置面板不得因此报错）
      setLspStatus(await ipc.lspStatus().catch(() => null));
    })();
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  function patchDraft(patch: Partial<ConfigState>) {
    setDraft((prev) => (prev ? { ...prev, ...patch } : prev));
  }

  /** validation 段局部更新（开关 / LSP 配置） */
  function patchValidation(patch: Partial<ValidationSettings>) {
    if (!draft) return;
    patchDraft({ validation: { ...draft.validation, ...patch } });
  }

  /** LSP 全局配置局部更新（预算 / 发现 / 命令覆盖；缺字段时以 DEFAULT_LSP_SETTINGS 为基准） */
  function patchLsp(next: typeof DEFAULT_LSP_SETTINGS) {
    patchValidation({ lsp: next });
  }

  /** 语言开关三态读写（显式分支而非动态键：java 缺省 false、dart 缺省 true，与后端 serde default 同源） */
  function langSwitchOf(lang: LspLanguage): { checked: boolean; onChange: (v: boolean) => void } {
    if (!draft) return { checked: false, onChange: () => {} };
    const v = draft.validation;
    switch (lang) {
      case "typescript": return { checked: v.typescript, onChange: (b) => patchValidation({ typescript: b }) };
      case "rust": return { checked: v.rust, onChange: (b) => patchValidation({ rust: b }) };
      case "python": return { checked: v.python, onChange: (b) => patchValidation({ python: b }) };
      case "go": return { checked: v.go, onChange: (b) => patchValidation({ go: b }) };
      case "java": return { checked: v.java ?? false, onChange: (b) => patchValidation({ java: b }) };
      case "dart": return { checked: v.dart ?? true, onChange: (b) => patchValidation({ dart: b }) };
    }
  }

  /** 状态徽标三态：未启用 = 已关闭；启用且找到 = 已找到（带版本）；启用但未探测到 = 未找到（警示色）；
   *  状态未取到（null）→ 不渲染徽标。 */
  function lspBadge(lang: LspLanguage): { text: string; warn: boolean } | null {
    const st = lspStatus?.find((s) => s.language === lang);
    if (!st) return null;
    if (!st.enabled) return { text: t("settings.lspDisabled"), warn: false };
    if (st.found) {
      return { text: st.version ? t("settings.lspFoundVersion", { version: st.version }) : t("settings.lspFound"), warn: false };
    }
    return { text: t("settings.lspMissing"), warn: true };
  }

  /** 重新探测（lsp_redetect）：清 PATH 与探测缓存后重查，刷新本页徽标。 */
  async function redetect() {
    setRedetecting(true);
    try {
      setLspStatus(await ipc.lspRedetect());
      message.success(t("settings.lspRedetected"));
    } catch (e) {
      message.error(`${t("settings.lspRedetectFailed")}：${String(e).replace(/^Error[:\s]*/i, "")}`);
    } finally {
      setRedetecting(false);
    }
  }

  function toggleSkill(name: string, disabled: boolean) {
    // 只改 draft；「保存」时一并提交
    if (!draft) return;
    const disabledSkills = draft.disabled_skills.filter((s) => s !== name);
    if (disabled) disabledSkills.push(name);
    setDraft({ ...draft, disabled_skills: disabledSkills });
  }

  /** 重新加载技能：后端清空索引缓存重扫（绕过 10s TTL），新放入/修改的技能立即可见。 */
  async function reloadSkills() {
    setSkillsBusy(true);
    try {
      const list = await ipc.reloadSkills(sessionId);
      setSkills(list);
      message.success(t("settings.skillsReloaded", { n: list.length }));
    } catch (e) {
      message.error(`${t("settings.skillsReloadFailed")}\n${String(e).replace(/^Error[:\s]*/i, "")}`);
    } finally {
      setSkillsBusy(false);
    }
  }

  /** 删除托管技能（可删性由后端 deletable 标记 + canonicalize 前缀双重校验；前端只传 name）。 */
  async function removeSkill(name: string) {
    setDeletingName(name);
    try {
      await ipc.deleteSkill(sessionId, name);
      message.success(t("settings.deleteSkillSuccess", { name }));
      setSkills((prev) => prev.filter((s) => s.name !== name));
      // 顺带清理 draft.disabled_skills 残留名，避免脏名随下次保存持久化
      if (draft?.disabled_skills.includes(name)) {
        setDraft({ ...draft, disabled_skills: draft.disabled_skills.filter((s) => s !== name) });
      }
    } catch (e) {
      message.error(`${t("settings.deleteSkillFailed")}\n${String(e).replace(/^Error[:\s]*/i, "")}`);
    } finally {
      setDeletingName(null);
    }
  }

  /** 保存（全量提交语义不变）。返回是否真的落盘：离开拦截靠它判断能否执行「保存并离开」 */
  async function save(): Promise<boolean> {
    if (!draft) return false;
    // 代理地址校验（网络页签）：仅自定义模式且非空时校验前缀白名单，非法跳转页签不落盘
    if (draft.proxy?.mode === "manual") {
      draft.proxy.url = draft.proxy.url.trim();
      if (draft.proxy.url !== "" && !PROXY_URL_RE.test(draft.proxy.url)) {
        message.error(t("settings.proxyUrlInvalid"));
        setTab("network");
        return false;
      }
    }
    // 供应商字段校验（[docs/provider-form-validation](../../../../docs/provider-form-validation.md)/29）：无效时逐项报错、跳转供应商页签、不落盘
    const problems: string[] = [];
    for (const p of draft.providers) {
      for (const issue of validateProvider(p)) {
        const fieldLabel =
          issue.field === "name" ? t("settings.providerName")
          : issue.field === "base_url" ? t("settings.baseUrl")
          : issue.field === "keys" ? "API Key"
          : issue.field === "headers" ? t("settings.customHeaders")
          : t("settings.modelList");
        const errText = issue.kind === "required" ? t("settings.vRequired") : issue.kind === "header" ? t("settings.vHeaders") : t("settings.vBaseUrl");
        problems.push(t("settings.vProblem", { name: p.name.trim() || p.id, field: fieldLabel, err: errText }));
      }
    }
    if (problems.length > 0) {
      message.error(`${t("settings.vSaveBlocked")}${problems.join(t("settings.vProblemSep"))}`);
      setTab("providers");
      return false;
    }
    const first = draft.providers.flatMap((p) => p.models)[0];
    // 活跃模型兜底：未设置取第一个模型；悬空（模型已删除）回退第一个
    if (!draft.active_model_id || !draft.providers.some((p) => p.models.some((m) => m.id === draft.active_model_id))) {
      draft.active_model_id = first?.id ?? null;
    }
    // 编辑期间保留空行（否则回车补一个 key 会被打断）；仅在保存时过滤空行（掩码/占位行保留，后端负责解掩码）
    draft.providers.forEach((p) => {
      p.keys = p.keys.map((s) => s.trim()).filter((s) => s !== "");
      // 自定义请求头：trim 头名，丢弃整行全空的行（[docs/provider-custom-headers](../../../../docs/provider-custom-headers.md)）
      p.headers = (p.headers ?? [])
        .map((h) => ({ name: h.name.trim(), value: h.value.trim() }))
        .filter((h) => h.name !== "");
    });
    // LSP 配置落盘前归一化：命令覆盖 / JDK 路径 trim，额外 SDK 根丢空行（编辑期间保留空行以便连续录入）
    if (draft.validation.lsp) {
      const lsp = draft.validation.lsp;
      const c = lsp.commands;
      draft.validation.lsp = {
        ...lsp,
        java_home: lsp.java_home.trim(),
        extra_roots: lsp.extra_roots.map((r) => r.trim()).filter((r) => r !== ""),
        commands: {
          typescript: c.typescript.trim(),
          rust: c.rust.trim(),
          python: c.python.trim(),
          go: c.go.trim(),
          java: c.java.trim(),
          dart: c.dart.trim(),
        },
      };
    }
    setSaving(true);
    try {
      await useSettings.getState().save(draft);
      // 保存成功不关闭弹窗（[docs/provider-form-validation](../../../../docs/provider-form-validation.md)）：仅提示；何时关闭由用户决定
      message.success(t("settings.saved"));
      // 系统代理模式：保存即触发后端重探测（save_config 热重建 client），刷新回显
      if ((draft.proxy?.mode ?? "system") === "system") {
        void ipc.resolveProxy().then(setSysProxy).catch(() => null);
      }
      return true;
    } catch (e) {
      message.error(String(e));
      return false;
    } finally {
      setSaving(false);
    }
  }

  // 自动更新偏好（设置 → 通用 → 更新）：与 GitWave 同形，localStorage 落盘、默认开启
  const [autoUpdate, setAutoUpdate] = useAutoUpdateSetting();
  // 设置页的「检查更新」按钮 loading（结果经 UpdateModal / toast 反馈）
  const [updateChecking, setUpdateChecking] = useState(false);

  async function saveMcp() {
    try {
      // 结构化模式：条目 -> JSON；兜底模式：原文本原样保存
      const json = mcpEntries !== null ? serializeMcpEntries(mcpEntries) : mcpRaw;
      await ipc.saveMcpConfig(json);
      message.success(t("settings.saved"));
      // 保存后自动重连（单条维护闭环）
      if (sessionId) {
        await ipc.connectMcp(sessionId).catch(() => null);
        useUi.setState({ mcpStatus: await ipc.mcpStatus().catch(() => []) });
      }
    } catch (e) {
      message.error(String(e));
    }
  }

  function patchMcpEntry(idx: number, patch: Partial<McpEntry>) {
    setMcpEntries((prev) =>
      prev ? prev.map((e, i) => (i === idx ? { ...e, ...patch } : e)) : prev,
    );
  }

  function addMcpEntry() {
    setMcpEntries((prev) => [
      ...(prev ?? []),
      { name: "", transport: "stdio", command: "", argsText: "", envText: "", url: "" },
    ]);
  }

  function removeMcpEntry(idx: number) {
    setMcpEntries((prev) => (prev ? prev.filter((_, i) => i !== idx) : prev));
  }

  // 代理模式视图态：proxy=null（从未配置）显示为「系统代理」——与 HTTP 栈默认行为一致（诚实呈现）
  const proxyMode = draft?.proxy?.mode ?? "system";
  const proxyUrl = draft?.proxy?.url ?? "";
  const proxyUrlInvalid = proxyMode === "manual" && proxyUrl.trim() !== "" && !PROXY_URL_RE.test(proxyUrl.trim());
  // LSP 配置视图态：旧配置缺 lsp 段时以 DEFAULT_LSP_SETTINGS（后端默认）为基准回显
  const lspCfg = draft?.validation.lsp ?? DEFAULT_LSP_SETTINGS;

  function patchProxyMode(mode: "none" | "system" | "manual") {
    // 切模式保留已填地址：来回切换不丢草稿
    patchDraft({ proxy: { mode, url: draft?.proxy?.url ?? "" } });
  }

  // ---------- 逐页脏标记与离开拦截（[docs/settings-fullscreen-shell](../../../../docs/settings-fullscreen-shell.md)） ----------
  const config = useSettings((s) => s.config);
  // MCP 不在 config 内（独立 mcp.json）：脏判定 = 当前文本与打开时基线的差集
  const mcpSerialized = mcpEntries !== null ? serializeMcpEntries(mcpEntries) : mcpRaw;
  const dirtyMap = useMemo(() => {
    const out: Record<PageKey, boolean> = {
      general: false, appearance: false, providers: false, security: false, network: false, mcp: false, skills: false,
    };
    for (const key of PAGE_ORDER) {
      if (key === "mcp") {
        out.mcp = mcpSerialized !== mcpOriginal;
        continue;
      }
      out[key] = !!draft && !!config && !sameSlice(pageSlice(draft, key), pageSlice(config, key));
    }
    return out;
  }, [draft, config, mcpSerialized, mcpOriginal]);
  const anyDirty = PAGE_ORDER.some((k) => dirtyMap[k]);

  // 聚合脏标记回写 store：关窗/退出时由 AppShell 的 ExitConfirm 读它决定先弹哪一层确认
  useEffect(() => {
    useUi.getState().setSettingsDirty(anyDirty);
  }, [anyDirty]);
  useEffect(() => () => {
    useUi.getState().setSettingsDirty(false);
  }, []);

  // 运行中会话数（点指示即返回工作区）：打开设置不影响运行，指示只是让用户知道后台还在跑
  const runningCount = useRun((s) => Object.values(s.tabs).filter((x) => x.running).length);
  const exitRequest = useUi((s) => s.exitRequest);
  const [leaveIntent, setLeaveIntent] = useState<LeaveIntent | null>(null);
  // 关窗/退出路径：设置页打开 + 有未保存改动 + 后端已下发退出请求 → 设置侧先处置
  const exitPending = anyDirty && !!exitRequest;
  const confirmOpen = leaveIntent !== null || exitPending;

  /** 关闭设置页（调用方须先处置未保存改动） */
  function closeShell() {
    useUi.setState({ settingsOpen: false, settingsDirty: false });
  }

  /** 放弃全部未保存改动：draft 与 MCP 都回到打开时的基线 */
  function discardDraft() {
    if (config) setDraft(JSON.parse(JSON.stringify(config)));
    const parsed = parseMcpEntries(mcpOriginal);
    setMcpEntries(parsed ?? []);
    setMcpRaw(parsed ? serializeMcpEntries(parsed) : mcpOriginal);
  }

  /** 顶部「取消」= 放弃全部未保存改动并返回工作区（不再二次确认） */
  function cancelAll() {
    discardDraft();
    closeShell();
  }

  /** 切页：脏改动存在时先走三选拦截（保存并离开 → 落盘后跳页；放弃 → 回基线后跳页；留在原地 → 停在本页） */
  function onTabChange(next: string) {
    if (next === tab) return;
    if (!anyDirty) {
      setTab(next);
      return;
    }
    setLeaveIntent({ kind: "tab", tab: next });
  }

  /** 返回工作区（返回按钮 / 运行中指示 / 页内 Esc 共用）：脏改动存在时先走同一份三选拦截 */
  function requestClose() {
    if (!anyDirty) {
      closeShell();
      return;
    }
    setLeaveIntent({ kind: "leave" });
  }

  /** 三选处置：切页 / 返回工作区 / 页内 Esc / 关窗退出四条路径共用这一份行为与文案 */
  async function answerLeave(action: "save" | "discard" | "stay") {
    const intent = leaveIntent;
    if (action === "stay") {
      setLeaveIntent(null);
      // 「留在原地」= 取消本次离开意图（切页/返回），**并且**取消这次退出（关窗路径）。
      // 两条并存时必须都处置：用户先点了切页/返回（intent）再关窗（exitPending）时，只清 intent
      // 而不回后端应答，后端就一直等在 ExitRequested 上（只有 2s 看门狗兜底）→ 应用退不出去。
      if (exitPending) {
        void respondExitRequest("cancel").finally(() => useUi.setState({ exitRequest: null }));
      }
      return;
    }
    if (action === "save") {
      const ok = await save();
      // 校验不通过：留在设置页（错误提示由 save 内部给出），不执行离开动作
      if (!ok) {
        setLeaveIntent(null);
        return;
      }
    } else {
      discardDraft();
    }
    setLeaveIntent(null);
    if (intent?.kind === "tab") {
      setTab(intent.tab);
      return;
    }
    if (intent) closeShell();
    // 关窗路径：设置侧已处置（脏标记已清零）→ 舞台交回 AppShell 的运行中会话确认
  }

  // 页内 Esc：捕获阶段注册并 preventDefault，抢在 AppShell 的全局 Esc（停止运行中会话）之前收口；
  // 浮层（Select/Dropdown/Popconfirm）或三选弹框开着时让位给组件库，不平级返回。
  const escRef = useRef<{ blocked: boolean; close: () => void }>({ blocked: false, close: () => {} });
  useEffect(() => {
    escRef.current = { blocked: confirmOpen, close: requestClose };
  });
  useEffect(() => {
    const onKeydown = (e: KeyboardEvent) => {
      if (e.key !== "Escape" || e.isComposing) return;
      if (escRef.current.blocked || overlayOpen()) return;
      e.preventDefault();
      escRef.current.close();
    };
    window.addEventListener("keydown", onKeydown, true);
    return () => window.removeEventListener("keydown", onKeydown, true);
  }, []);

  // 可达性：设置页是全屏 dialog，打开时把焦点交给导航首项（「返回工作区」）。
  // 刻意**不**引入焦点陷阱库（本批范围是容器化）：只保证键盘用户不必先穿过整个工作区。
  const shellRef = useRef<HTMLDivElement | null>(null);
  useEffect(() => {
    shellRef.current?.querySelector<HTMLButtonElement>(".settings-nav-head button")?.focus();
  }, []);

  /** 7 页清单：导航项与页体同源（导航只消费 key/labelKey，页体按当前页渲染到右列） */
  const pages: { key: PageKey; labelKey: string; body: ReactNode }[] = [
    {
      key: "general",
      labelKey: "settings.general",
      body: (
        <Form layout="vertical">
          <Form.Item label={t("settings.language")}>
            {/* 即时生效：改完立即写 useUi.setLanguage，因此不进 draft 脏标记（也就不会亮脏点） */}
            <Select
              size="small"
              style={{ width: 160 }}
              value={draft?.ui.language}
              onChange={(v) => {
                patchDraft({ ui: { ...draft!.ui, language: v } });
                useUi.getState().setLanguage(v as "zh-CN" | "en-US");
              }}
              options={[
                { label: "中文", value: "zh-CN" },
                { label: "English", value: "en-US" },
              ]}
            />
            <span className="settings-instant">{t("settings.instantApply")}</span>
          </Form.Item>
          {/* AI 回复语言：自由输入；留空 = 跟随会话语言。
              经系统提示词 <reply-language> 指令下发（core/prompt.rs）。 */}
          <Form.Item label={t("settings.aiLanguage")} extra={t("settings.aiLanguageHint")}>
            <Input
              size="small"
              style={{ width: 240 }}
              maxLength={40}
              placeholder={t("composer.effortDefault")}
              value={draft?.ui.ai_language ?? ""}
              onChange={(e) => {
                const v = e.target.value;
                patchDraft({ ui: { ...draft!.ui, ai_language: v.trim() === "" ? null : v } });
              }}
            />
          </Form.Item>
          <Form.Item label={t("settings.compactThreshold")}>
            <Slider
              style={{ width: 320 }}
              min={0.1}
              max={0.9}
              step={0.05}
              value={draft?.compact_threshold ?? 0.6}
              onChange={(v) => patchDraft({ compact_threshold: v })}
            />
          </Form.Item>
          <Form.Item label={t("settings.compactTimeout")}>
            <InputNumber
              min={30}
              max={3600}
              step={30}
              value={draft?.compact_timeout_seconds ?? 180}
              onChange={(v) => patchDraft({ compact_timeout_seconds: v ?? 180 })}
            />
          </Form.Item>
          <Form.Item label={t("settings.shell")} tooltip={t("settings.shellHint")}>
            <div style={{ display: "flex", alignItems: "center", gap: 8, width: "100%", minWidth: 0 }}>
              <Select
                size="small"
                style={{ width: 260, flexShrink: 0 }}
                value={draft?.shell?.selection ?? "auto"}
                onChange={(v) => patchDraft({ shell: { selection: v === "auto" ? null : v } })}
                options={[
                  {
                    // 自动默认项以后端 auto 标注为准（与 detect_shell 同源判定，PATH 上存在
                    // 非 Git bash 时 shells[0] 不一定等于自动探测结果）
                    label:
                      shells?.find((s) => s.auto)?.name !== undefined
                        ? t("settings.shellAutoWithDefault", { name: shells!.find((s) => s.auto)!.name })
                        : t("settings.shellAuto"),
                    value: "auto",
                  },
                  ...(shells ?? []).map((s) => ({
                    label: s.limited ? `${s.name}${t("settings.shellLimited")}` : s.name,
                    value: s.id,
                    title: s.path ?? s.name,
                  })),
                ]}
              />
              {/* 路径回显：所选 shell（或 auto 探测项）的可执行文件绝对路径；探测失败/已卸载不显示 */}
              {(() => {
                const display = resolveShellDisplay(draft?.shell?.selection ?? null, shells);
                if (!display) return null;
                return display.kind === "path" ? (
                  <code
                    title={display.text}
                    style={{
                      fontFamily: "var(--ws-font-mono)",
                      fontSize: 12,
                      color: "var(--ws-dim)",
                      overflow: "hidden",
                      textOverflow: "ellipsis",
                      whiteSpace: "nowrap",
                      minWidth: 0,
                    }}
                  >
                    {display.text}
                  </code>
                ) : (
                  <Typography.Text type="secondary" style={{ fontSize: 12 }}>
                    {t("settings.shellNoPath")}
                  </Typography.Text>
                );
              })()}
            </div>
            {/* 探测列表不含当前所选 shell（已卸载）时警示但不删选项 */}
            {draft?.shell?.selection && shells !== null && !shells.some((s) => s.id === draft.shell!.selection) && (
              <Typography.Text type="warning" style={{ fontSize: 12 }}>
                {t("settings.shellNotDetected")}
              </Typography.Text>
            )}
            {shells === null && (
              <Typography.Text type="warning" style={{ fontSize: 12 }}>
                {t("settings.shellDetectFailed")}
              </Typography.Text>
            )}
          </Form.Item>
          <Form.Item label={t("settings.customPrompt")}>
            <TextArea
              rows={4}
              value={draft?.custom_prompt ?? ""}
              // 空值归一：清空写 null（而非 ""），与 ai_language 同口径；后端也是按 trim 后非空才注入
              onChange={(e) => {
                const v = e.target.value;
                patchDraft({ custom_prompt: v.trim() === "" ? null : v });
              }}
            />
          </Form.Item>
          <Form.Item label={t("settings.logLevel")} tooltip={t("settings.logLevelHint")}>
            <Select
              size="small"
              style={{ width: 160 }}
              value={draft?.log?.level ?? "info"}
              onChange={(v) => patchDraft({ log: { ...draft!.log, level: v } })}
              options={["trace", "debug", "info", "warn", "error"].map((v) => ({ label: v, value: v }))}
            />
          </Form.Item>
          <Form.Item label={t("settings.updates")} tooltip={t("settings.updatesHint")}>
            <div className="settings-update-row">
              <Switch
                size="small"
                checked={autoUpdate}
                onChange={setAutoUpdate}
                aria-label={t("settings.autoUpdateCheckbox")}
              />
              <span className="settings-update-label">{t("settings.autoUpdateCheckbox")}</span>
              {/* 即时生效：开关直接写 localStorage（useAutoUpdateSetting），不进 draft */}
              <span className="settings-instant">{t("settings.instantApply")}</span>
              <Button
                size="small"
                loading={updateChecking}
                onClick={() => {
                  // 结果经 UpdateModal / toast 反馈（有更新与失败会弹窗），设置页不需要自己展示
                  setUpdateChecking(true);
                  void checkForUpdates().finally(() => setUpdateChecking(false));
                }}
              >
                {t("settings.checkForUpdates")}
              </Button>
            </div>
          </Form.Item>
          <Form.Item label={t("settings.sessionVerbose")} tooltip={t("settings.sessionVerboseHint")}>
            <Switch
              size="small"
              checked={draft?.log?.session_verbose ?? false}
              onChange={(v) => patchDraft({ log: { ...draft!.log, session_verbose: v } })}
            />
          </Form.Item>
        </Form>
      ),
    },
    {
      key: "appearance",
      labelKey: "settings.appearance",
      body: <AppearanceSettings />,
    },
    {
      key: "providers",
      labelKey: "settings.providers",
      body: draft && <ProvidersPanel draft={draft} patchDraft={patchDraft} />,
    },
    {
      key: "security",
      labelKey: "settings.security",
      body: draft && (
        <Form layout="vertical">
          <Form.Item label={t("settings.approvalEnabled")}>
            <Switch checked={draft.approval.enabled} onChange={(v) => patchDraft({ approval: { ...draft.approval, enabled: v } })} />
          </Form.Item>
          <Form.Item label={t("settings.confirmOutside")}>
            <Switch checked={draft.approval.confirm_outside_create} onChange={(v) => patchDraft({ approval: { ...draft.approval, confirm_outside_create: v } })} />
          </Form.Item>
          <Form.Item label={t("settings.confirmPush")}>
            <Switch checked={draft.approval.confirm_git_push} onChange={(v) => patchDraft({ approval: { ...draft.approval, confirm_git_push: v } })} />
          </Form.Item>
          {/* docs/ask-ink-accent-and-composer-cover：审批等待策略——勾选后 5 分钟无应答自动确认推荐选项（allowed），不勾 = 永不超时 */}
          <Form.Item label={t("settings.autoConfirm")} extra={t("settings.autoConfirmHint")}>
            <Switch checked={draft.approval.auto_confirm} onChange={(v) => patchDraft({ approval: { ...draft.approval, auto_confirm: v } })} />
          </Form.Item>
          {/* docs/run-queue-and-ask-revamp：「始终允许本项目」命令白名单（审批时选择加入，此处管理/移除）。
              存储条目 = cwd \u{1} 完整命令文本（cwd 跟随项目 -> 白名单不跨项目生效），展示时拆开 */}
          {(draft.approval.command_allowlist?.length ?? 0) > 0 && (
            <Form.Item label={t("settings.cmdAllowlist")}>
              <div className="cmd-allowlist">
                {draft.approval.command_allowlist.map((entry, i) => {
                  const sep = entry.indexOf("\u0001");
                  const cwd = sep >= 0 ? entry.slice(0, sep) : "";
                  const cmd = sep >= 0 ? entry.slice(sep + 1) : entry;
                  return (
                    <div className="cmd-allowlist-row" key={`${i}-${cmd}`}>
                      <code className="cmd-allowlist-cmd" title={cwd ? `${cmd}\n${t("settings.cmdAllowlistCwd")}: ${cwd}` : cmd}>
                        {cmd}
                      </code>
                      <Button
                        size="small"
                        type="text"
                        danger
                        onClick={() =>
                          patchDraft({
                            approval: { ...draft.approval, command_allowlist: draft.approval.command_allowlist.filter((_, j) => j !== i) },
                          })
                        }
                      >
                        {t("sessions.delete")}
                      </Button>
                    </div>
                  );
                })}
              </div>
            </Form.Item>
          )}
          <Form.Item label={t("settings.allowPrivate")}>
            <Switch checked={draft.network.allow_private_network} onChange={(v) => patchDraft({ network: { allow_private_network: v } })} />
          </Form.Item>
          <Divider>{t("settings.validation")}</Divider>
          <div className="hint" style={{ marginBottom: 10 }}>{t("settings.validationHint")}</div>
          <Form.Item style={{ marginBottom: 0 }}>
            {/* 六语言各一行：语言名 | 开关 | 命令覆盖 | 状态徽标（行序与后端 Lang::all() 同源；JSON 走内置解析，只给开关） */}
            <div className="validation-rows">
              {LSP_LANGUAGES.map((lang) => {
                const sw = langSwitchOf(lang);
                const badge = lspBadge(lang);
                return (
                  <div className="validation-row" data-lang={lang} key={lang}>
                    <span className="validation-label">{t(LANG_LABEL_KEY[lang])}</span>
                    <Switch size="small" checked={sw.checked} aria-label={t(LANG_LABEL_KEY[lang])} onChange={sw.onChange} />
                    <Input
                      size="small"
                      placeholder={t("settings.lspCommandPh")}
                      value={lspCommandOf(lspCfg, lang)}
                      onChange={(e) => patchLsp(withLspCommand(lspCfg, lang, e.target.value))}
                    />
                    <span className={`validation-status${badge?.warn ? " warn" : ""}`}>{badge?.text ?? ""}</span>
                  </div>
                );
              })}
              <div className="validation-row" data-lang="json">
                <span className="validation-label">{t("settings.validationLangJson")}</span>
                <Switch size="small" checked={draft.validation.json} onChange={(v) => patchValidation({ json: v })} />
                <span />
                <span className="validation-status" />
              </div>
            </div>
            {/* Java 代价提示：jdtls 首次启动会解析依赖树（可能数分钟、GB 级内存） */}
            <div className="hint" style={{ marginTop: 8 }} data-testid="lsp-java-cost">
              {t("settings.lspJavaCost")}
            </div>
          </Form.Item>

          <Divider plain>{t("settings.lspBudget")}</Divider>
          <div style={{ display: "grid", gridTemplateColumns: "repeat(2, minmax(220px, 1fr))", gap: "0 20px" }}>
            <Form.Item label={t("settings.lspSyncWindow")}>
              <InputNumber
                size="small"
                style={{ width: 180 }}
                min={0}
                max={60000}
                step={100}
                value={lspCfg.sync_window_ms}
                onChange={(v) => patchLsp({ ...lspCfg, sync_window_ms: v ?? DEFAULT_LSP_SETTINGS.sync_window_ms })}
              />
            </Form.Item>
            <Form.Item label={t("settings.lspMaxDiagnostics")}>
              <InputNumber
                size="small"
                style={{ width: 180 }}
                min={1}
                max={200}
                value={lspCfg.max_diagnostics}
                onChange={(v) => patchLsp({ ...lspCfg, max_diagnostics: v ?? DEFAULT_LSP_SETTINGS.max_diagnostics })}
              />
            </Form.Item>
            <Form.Item label={t("settings.lspMaxChars")}>
              <InputNumber
                size="small"
                style={{ width: 180 }}
                min={200}
                max={100000}
                step={200}
                value={lspCfg.max_chars}
                onChange={(v) => patchLsp({ ...lspCfg, max_chars: v ?? DEFAULT_LSP_SETTINGS.max_chars })}
              />
            </Form.Item>
            <Form.Item label={t("settings.lspIdleTtl")}>
              <InputNumber
                size="small"
                style={{ width: 180 }}
                min={0}
                max={86400000}
                step={60000}
                value={lspCfg.idle_ttl_ms}
                onChange={(v) => patchLsp({ ...lspCfg, idle_ttl_ms: v ?? DEFAULT_LSP_SETTINGS.idle_ttl_ms })}
              />
            </Form.Item>
            <Form.Item label={t("settings.lspMaxServers")}>
              <InputNumber
                size="small"
                style={{ width: 180 }}
                min={1}
                max={32}
                value={lspCfg.max_servers}
                onChange={(v) => patchLsp({ ...lspCfg, max_servers: v ?? DEFAULT_LSP_SETTINGS.max_servers })}
              />
            </Form.Item>
            <Form.Item label={t("settings.lspMaxFileBytes")}>
              <InputNumber
                size="small"
                style={{ width: 180 }}
                min={1024}
                max={104857600}
                step={1024}
                value={lspCfg.max_file_bytes}
                onChange={(v) => patchLsp({ ...lspCfg, max_file_bytes: v ?? DEFAULT_LSP_SETTINGS.max_file_bytes })}
              />
            </Form.Item>
            <Form.Item label={t("settings.lspDedupeLimit")}>
              <InputNumber
                size="small"
                style={{ width: 180 }}
                min={0}
                max={10}
                value={lspCfg.dedupe_limit}
                onChange={(v) => patchLsp({ ...lspCfg, dedupe_limit: v ?? DEFAULT_LSP_SETTINGS.dedupe_limit })}
              />
            </Form.Item>
          </div>

          <Divider plain>{t("settings.lspDiscovery")}</Divider>
          <Form.Item label={t("settings.lspExtraRoots")} extra={t("settings.lspExtraRootsHint")}>
            <div className="lsp-roots">
              {lspCfg.extra_roots.map((root, i) => (
                <div className="lsp-root-row" key={i}>
                  <Input
                    size="small"
                    value={root}
                    aria-label={t("settings.lspExtraRoots")}
                    onChange={(e) =>
                      patchLsp({ ...lspCfg, extra_roots: lspCfg.extra_roots.map((r, j) => (j === i ? e.target.value : r)) })
                    }
                  />
                  <Button
                    size="small"
                    type="text"
                    danger
                    aria-label={t("sessions.delete")}
                    icon={<DeleteOutlined />}
                    onClick={() => patchLsp({ ...lspCfg, extra_roots: lspCfg.extra_roots.filter((_, j) => j !== i) })}
                  />
                </div>
              ))}
              <div>
                <Button size="small" onClick={() => patchLsp({ ...lspCfg, extra_roots: [...lspCfg.extra_roots, ""] })}>
                  {t("settings.lspAddRoot")}
                </Button>
              </div>
            </div>
          </Form.Item>
          <Form.Item label={t("settings.lspJavaHome")} extra={t("settings.lspJavaHomeHint")}>
            <Input
              size="small"
              style={{ width: 360 }}
              value={lspCfg.java_home}
              placeholder={t("settings.lspCommandPh")}
              onChange={(e) => patchLsp({ ...lspCfg, java_home: e.target.value })}
            />
          </Form.Item>
          <Form.Item style={{ marginBottom: 0 }}>
            <Button size="small" loading={redetecting} onClick={() => void redetect()}>
              {t("settings.lspRedetect")}
            </Button>
          </Form.Item>
        </Form>
      ),
    },
    {
      key: "network",
      labelKey: "settings.network",
      body: draft && (
        <Form layout="vertical">
          <Form.Item label={t("settings.proxyMode")}>
            {/* heroui radio-group 风格：整卡可点的三选一卡片，选中墨色描边（样式 .proxy-mode-card） */}
            <Radio.Group value={proxyMode} onChange={(e) => patchProxyMode(e.target.value)}>
              <div className="proxy-mode-list">
                {([
                  ["none", t("settings.proxyNone"), t("settings.proxyNoneDesc")],
                  ["system", t("settings.proxySystem"), t("settings.proxySystemDesc")],
                  ["manual", t("settings.proxyManual"), t("settings.proxyManualDesc")],
                ] as const).map(([mode, title, desc]) => (
                  <label key={mode} className={`proxy-mode-card${proxyMode === mode ? " active" : ""}`}>
                    <div className="proxy-mode-head">
                      <Radio value={mode} />
                      <span className="proxy-mode-title">{title}</span>
                    </div>
                    <div className="proxy-mode-desc">{desc}</div>
                    {/* 系统代理探测回显：undefined = 未拉取不渲染；保存后经 save() 重探测刷新 */}
                    {mode === "system" && sysProxy !== undefined && (
                      <div className="proxy-mode-echo">
                        {sysProxy
                          ? t("settings.proxyDetected", { url: sysProxy })
                          : t("settings.proxyNotDetected")}
                      </div>
                    )}
                  </label>
                ))}
              </div>
            </Radio.Group>
          </Form.Item>
          {proxyMode === "manual" && (
            <Form.Item
              label={t("settings.proxyUrl")}
              extra={t("settings.proxyUrlHint")}
              validateStatus={proxyUrlInvalid ? "error" : undefined}
              help={proxyUrlInvalid ? t("settings.proxyUrlInvalid") : undefined}
            >
              <Input
                size="small"
                style={{ width: 360 }}
                placeholder="http://127.0.0.1:7890 或 socks5://127.0.0.1:1080"
                value={proxyUrl}
                onChange={(e) => patchDraft({ proxy: { mode: "manual", url: e.target.value } })}
              />
            </Form.Item>
          )}
        </Form>
      ),
    },
    {
      key: "mcp",
      labelKey: "settings.mcp",
      body: mcpEntries === null ? (
        // 兜底模式：原 JSON 无法解析时的保命通道；直接保存避免丢失
        <div className="mcp-pane">
          <div className="hint">{t("settings.mcpRawHint")}</div>
          <TextArea rows={14} value={mcpRaw} spellCheck={false} className="mcp-json" onChange={(e) => setMcpRaw(e.target.value)} />
          <div>
            <Button size="small" type="primary" onClick={() => void saveMcp()}>{t("settings.mcpSave")}</Button>
          </div>
        </div>
      ) : (
        <div className="mcp-pane">
          <div className="hint">{t("settings.mcpHint")}</div>
          {mcpEntries.map((e, idx) => (
            <div className="mcp-entry" key={idx}>
              <div className="mcp-entry-head">
                <Input
                  size="small"
                  style={{ width: 160 }}
                  value={e.name}
                  placeholder={t("settings.mcpName")}
                  onChange={(ev) => patchMcpEntry(idx, { name: ev.target.value })}
                />
                <Select
                  size="small"
                  style={{ width: 170 }}
                  value={e.transport}
                  options={[
                    { label: t("settings.mcpTransportStdio"), value: "stdio" },
                    { label: t("settings.mcpTransportHttp"), value: "streamable_http" },
                  ]}
                  onChange={(v) => patchMcpEntry(idx, { transport: v })}
                />
                <div className="flex" />
                <Button size="small" type="text" danger icon={<DeleteOutlined />} onClick={() => removeMcpEntry(idx)} />
              </div>
              {e.transport === "stdio" ? (
                <>
                  <div className="mcp-entry-row">
                    <span className="mcp-label">{t("settings.mcpCommand")}</span>
                    <Input
                      size="small"
                      value={e.command}
                      placeholder="npx -y @modelcontextprotocol/server-fs"
                      onChange={(ev) => patchMcpEntry(idx, { command: ev.target.value })}
                    />
                  </div>
                  <div className="mcp-entry-row">
                    <span className="mcp-label">{t("settings.mcpArgs")}</span>
                    <Input
                      size="small"
                      value={e.argsText}
                      onChange={(ev) => patchMcpEntry(idx, { argsText: ev.target.value })}
                    />
                  </div>
                  <div className="mcp-entry-row">
                    <span className="mcp-label">{t("settings.mcpEnv")}</span>
                    <TextArea
                      rows={2}
                      size="small"
                      value={e.envText}
                      onChange={(ev) => patchMcpEntry(idx, { envText: ev.target.value })}
                    />
                  </div>
                </>
              ) : (
                <div className="mcp-entry-row">
                  <span className="mcp-label">{t("settings.mcpUrl")}</span>
                  <Input
                    size="small"
                    value={e.url}
                    placeholder="https://example.com/mcp"
                    onChange={(ev) => patchMcpEntry(idx, { url: ev.target.value })}
                  />
                </div>
              )}
            </div>
          ))}
          <div style={{ display: "flex", gap: 10 }}>
            <Button size="small" onClick={addMcpEntry}>{t("settings.mcpAdd")}</Button>
            <Button size="small" type="primary" onClick={() => void saveMcp()}>{t("settings.mcpSave")}</Button>
          </div>
        </div>
      ),
    },
    {
      key: "skills",
      labelKey: "settings.skills",
      body: (
        <div>
          <div className="skills-toolbar">
            <div className="hint">{t("settings.skillsHint")}</div>
            <Button size="small" loading={skillsBusy} onClick={() => void reloadSkills()}>
              {t("settings.reloadSkills")}
            </Button>
          </div>
          {skills.length === 0 && <Empty description={t("sessions.empty")} style={{ marginTop: 24 }} />}
          {skills.map((s) => {
            const builtin = s.origin === "<builtin>";
            const label = originLabel(s.origin);
            return (
              <div className="skill-row" key={s.name}>
                <div className="skill-info">
                  <b>{s.name}</b>
                  {builtin ? (
                    <span className="skill-origin">{t("rightbar.skillBuiltin")}</span>
                  ) : (
                    label && <span className="skill-origin" title={s.origin}>{label}</span>
                  )}
                  <span className="dim"> {s.description}</span>
                  {s.whenToUse && <div className="dim small">when: {s.whenToUse}</div>}
                </div>
                <div className="skill-actions">
                  <Switch
                    checked={!draft?.disabled_skills.includes(s.name)}
                    onChange={(v) => toggleSkill(s.name, !v)}
                  />
                  {s.deletable && (
                    <Popconfirm
                      title={t("settings.deleteSkillConfirm", { name: s.name })}
                      description={s.origin}
                      okButtonProps={{ loading: deletingName === s.name }}
                      onConfirm={() => void removeSkill(s.name)}
                    >
                      <Button
                        type="text"
                        size="small"
                        danger
                        aria-label={t("settings.deleteSkill")}
                        icon={<DeleteOutlined />}
                      />
                    </Popconfirm>
                  )}
                </div>
              </div>
            );
          })}
        </div>
      ),
    },
  ];

  const activePage = pages.find((p) => p.key === tab) ?? pages[0];
  // 窄窗导航列宽：基准 280，随窗口宽度收缩，由 clampNavWidth 夹在 180..480 内
  const navWidth = clampNavWidth(Math.min(SETTINGS_NAV_W, Math.round(windowWidth * SETTINGS_NAV_RATIO)));

  return (
    // 全屏 dialog 语义（焦点在打开时移到导航首项，见上方 focus effect）
    <div
      ref={shellRef}
      className="settings-shell"
      data-testid="settings-page"
      data-confirm-open={confirmOpen ? "1" : "0"}
      role="dialog"
      aria-modal="true"
      aria-label={t("settings.title")}
    >
      {/* 左导航列：返回工作区 + 运行中指示 + 7 页导航。
          导航仍由 antd Tabs 提供（自建导航是后续批次），但它只消费 key/label——页体渲染在右列，
          这是「二列式全屏页」的布局要求（Tabs 自身无法把 nav 与 pane 拆到两列）。 */}
      <nav className="settings-nav" style={{ width: navWidth }}>
        <div className="settings-nav-head">
          <Button
            type="text"
            size="small"
            icon={<ArrowLeftOutlined />}
            aria-label={t("settings.backToWorkspace")}
            onClick={requestClose}
          >
            {t("settings.backToWorkspace")}
          </Button>
          {/* 运行中指示：数量为 0 时不渲染；点即返回工作区（会话继续跑，不受设置页影响） */}
          {runningCount > 0 && (
            <button type="button" className="run-indicator" title={t("settings.runningHint")} onClick={requestClose}>
              <span className="run-dot" aria-hidden />
              <span>{t("settings.runningCount", { n: runningCount })}</span>
            </button>
          )}
        </div>
        <Tabs
          tabPlacement="start"
          activeKey={tab}
          onChange={onTabChange}
          items={pages.map((p) => ({
            key: p.key,
            label: (
              <span className="settings-nav-label">
                {t(p.labelKey)}
                {dirtyMap[p.key] && <span className="settings-dirty-dot" title={t("settings.dirtyHint")} />}
              </span>
            ),
          }))}
        />
      </nav>

      <div className="settings-content">
        <div className="settings-actions">
          <span className="settings-actions-title">
            {t("settings.title")} · {activePage ? t(activePage.labelKey) : ""}
          </span>
          {anyDirty && <span className="settings-dirty-dot" title={t("settings.dirtyHint")} />}
          <div className="settings-actions-buttons">
            <Tooltip title={t("settings.cancelHint")}>
              <Button onClick={cancelAll}>{t("settings.cancel")}</Button>
            </Tooltip>
            <Button type="primary" loading={saving} onClick={() => void save()}>
              {t("settings.save")}
            </Button>
          </div>
        </div>
        <div className="settings-pane">
          <div className="settings-pane-body">{activePage ? activePage.body : null}</div>
        </div>
      </div>

      {/* 三选拦截：切页 / 返回工作区 / 页内 Esc（leaveIntent）与关窗退出（exitPending）共用这份文案与行为。
          Esc 走 antd Modal 默认行为 → onCancel = 留在原地（浮层优先，不平级返回）。 */}
      <Modal
        open={confirmOpen}
        title={t("settings.leaveTitle")}
        closable={false}
        mask={{ closable: false }}
        onCancel={() => void answerLeave("stay")}
        footer={[
          <Button key="stay" onClick={() => void answerLeave("stay")}>
            {t("settings.leaveStay")}
          </Button>,
          <Button key="discard" danger onClick={() => void answerLeave("discard")}>
            {t("settings.leaveDiscard")}
          </Button>,
          <Button key="save" type="primary" onClick={() => void answerLeave("save")}>
            {t("settings.leaveSave")}
          </Button>,
        ]}
      >
        <div>{t("settings.leaveDesc")}</div>
      </Modal>
    </div>
  );
}