/**
 * MCP 配置的解析 / 序列化纯函数（**无损往返**）。
 *
 * 替代原先内联在 `SettingsPage.tsx` 里的 `parseMcpEntries` / `serializeMcpEntries`，
 * 那版有三个真实缺陷（均无测试守护）：
 *  1. `args` 用 `join(" ")` / `split(/\s+/)` 往返 → 含空格的 Windows 路径被拆坏；
 *  2. `env` 用 `K=V` 文本域、值 `.trim()`、缺 `=` 的行静默丢弃；
 *  3. 表单未覆盖的键（`headers` / `cwd` / `$schema` / 自定义键）保存即丢；
 *     未知 `transport` 取值被静默改写成 `stdio`。
 *
 * 本模块的保证：
 *  · args / env / headers 一律表格化，**不拆分、不 trim 值**；
 *  · 未识别的键原样保留（条目级 `extra` + 顶层 `extraTop`），保存时写回；
 *  · 未识别的 transport 取值（如 `"sse"`）原样保留，交由后端给出定向报错，前端不改写语义；
 *  · 只在「键与值都为空」时丢弃表格行（点了「＋」没填的行）。
 *
 * 形状见 [docs/mcp-module-rebuild](../../../docs/mcp-module-rebuild.md) §2.1。
 */

/** 工具过滤模式（与后端 `ToolFilterMode` 对齐）。 */
export type McpToolFilterMode = "all" | "allow" | "deny";

/** 参数行（值原样保留，含空格）。 */
export interface McpArgRow {
  value: string;
}

/** 环境变量 / 请求头行。 */
export interface McpKeyValueRow {
  key: string;
  value: string;
}

/** 一个 server 的可编辑草稿。 */
export interface McpServerDraft {
  name: string;
  /**
   * 文件里的 transport 原文：已知取值为 `"stdio"` / `"streamable_http"`，
   * 未识别取值（如 `"sse"`）原样保留；`null` = 文件未声明（由 command/url 推导）。
   */
  transportRaw: string | null;
  command: string;
  args: McpArgRow[];
  env: McpKeyValueRow[];
  cwd: string;
  url: string;
  headers: McpKeyValueRow[];
  enabled: boolean;
  /** 毫秒；空串 = 未配置（后端用默认 120000）。 */
  timeoutMs: string;
  readOnly: boolean;
  alwaysAllow: boolean;
  toolsMode: McpToolFilterMode;
  /** 白/黑名单，空格分隔（支持 `*` 通配）。 */
  toolsList: string;
  /** 表单未覆盖的键：原样保留。 */
  extra: Record<string, unknown>;
}

/** 整份文档的草稿。 */
export interface McpDraftDoc {
  servers: McpServerDraft[];
  /** 顶层未覆盖的键（如 `$schema`）。 */
  extraTop: Record<string, unknown>;
}

/** 后端识别（因而表单接管）的条目级键。 */
const MANAGED_KEYS = new Set([
  "transport",
  "type",
  "command",
  "args",
  "env",
  "cwd",
  "url",
  "headers",
  "enabled",
  "timeout_ms",
  "timeoutMs",
  "read_only",
  "readOnly",
  "always_allow",
  "alwaysAllow",
  "tools",
]);

/** 读字符串字段（非字符串一律当空）。 */
function str(v: unknown): string {
  return typeof v === "string" ? v : "";
}

/** 读布尔字段，缺省用 `fallback`。 */
function bool(v: unknown, fallback: boolean): boolean {
  return typeof v === "boolean" ? v : fallback;
}

/** 对象 → 键值行（值保留原文，不 trim）。 */
function toRows(v: unknown): McpKeyValueRow[] {
  if (!v || typeof v !== "object" || Array.isArray(v)) return [];
  return Object.entries(v as Record<string, unknown>).map(([key, val]) => ({
    key,
    value: typeof val === "string" ? val : String(val ?? ""),
  }));
}

/** 键值行 → 对象；跳过键为空的行（值是空串仍保留，语义不同）。 */
function fromRows(rows: McpKeyValueRow[]): Record<string, string> {
  const out: Record<string, string> = {};
  for (const r of rows) {
    const k = r.key.trim();
    if (!k) continue;
    out[k] = r.value;
  }
  return out;
}

/** 一个 server 条目 → 草稿。 */
function toDraft(name: string, cfg: Record<string, unknown>): McpServerDraft {
  const rawTransport =
    typeof cfg.transport === "string"
      ? cfg.transport
      : typeof cfg.type === "string"
        ? (cfg.type as string)
        : null;
  const extra: Record<string, unknown> = {};
  for (const [k, v] of Object.entries(cfg)) {
    if (!MANAGED_KEYS.has(k)) extra[k] = v;
  }
  const timeout = cfg.timeout_ms ?? cfg.timeoutMs;
  const tools = (cfg.tools ?? {}) as Record<string, unknown>;
  const toolsMode = tools.mode;
  return {
    name,
    transportRaw: rawTransport,
    command: str(cfg.command),
    args: Array.isArray(cfg.args)
      ? cfg.args.map((a) => ({ value: typeof a === "string" ? a : String(a ?? "") }))
      : [],
    env: toRows(cfg.env),
    cwd: str(cfg.cwd),
    url: str(cfg.url),
    headers: toRows(cfg.headers),
    enabled: bool(cfg.enabled, true),
    timeoutMs: typeof timeout === "number" ? String(timeout) : "",
    readOnly: bool(cfg.read_only ?? cfg.readOnly, false),
    alwaysAllow: bool(cfg.always_allow ?? cfg.alwaysAllow, false),
    toolsMode: toolsMode === "allow" || toolsMode === "deny" ? toolsMode : "all",
    toolsList: Array.isArray(tools.list) ? tools.list.map(String).join(" ") : "",
    extra,
  };
}

/**
 * 解析 mcp.json 文本为草稿。
 *
 * 返回 `null` 表示**无法结构化编辑**（JSON 语法非法 / 顶层不是对象 /
 * `mcpServers` 不是对象 / 某个条目不是对象）——调用方应回退「原文文本模式」直接保存原文，
 * 而不是拿空配置覆盖用户文件。
 */
export function parseMcpDoc(raw: string): McpDraftDoc | null {
  // 空文件（或只有空白）= 空配置：没有内容可丢，按空文档处理而不是回退原文模式。
  // 回退只留给「有内容但无法结构化编辑」的情形——那才可能丢用户数据。
  if (raw.trim() === "") return { servers: [], extraTop: {} };
  let parsed: unknown;
  try {
    parsed = JSON.parse(raw);
  } catch {
    return null;
  }
  if (!parsed || typeof parsed !== "object" || Array.isArray(parsed)) return null;
  const root = parsed as Record<string, unknown>;
  const extraTop: Record<string, unknown> = {};
  for (const [k, v] of Object.entries(root)) {
    if (k !== "mcpServers") extraTop[k] = v;
  }
  const serversRaw = root.mcpServers;
  if (serversRaw === undefined || serversRaw === null) {
    // 没有 mcpServers 段 = 空配置（顶层其它键进 extraTop，保存时原样写回）
    return { servers: [], extraTop };
  }
  if (typeof serversRaw !== "object" || Array.isArray(serversRaw)) return null;
  const servers: McpServerDraft[] = [];
  for (const [name, cfg] of Object.entries(serversRaw as Record<string, unknown>)) {
    // 条目不是对象（用户手写的怪形状）→ 不结构化编辑，避免保存时把它改成对象
    if (!cfg || typeof cfg !== "object" || Array.isArray(cfg)) return null;
    servers.push(toDraft(name, cfg as Record<string, unknown>));
  }
  return { servers, extraTop };
}

/** 草稿 → JSON 文本（`null` 与 `undefined` 的 extra 值原样保留）。 */
export function serializeMcpDoc(doc: McpDraftDoc): string {
  const servers: Record<string, unknown> = {};
  for (const d of doc.servers) {
    const name = d.name.trim();
    if (!name) continue; // 未命名条目跳过（与旧行为一致）
    const cfg: Record<string, unknown> = { ...d.extra };
    // transport：只在文件里本来就有时写回（未知取值原样保留，让后端报定向错误）
    if (d.transportRaw) cfg.transport = d.transportRaw;
    else delete cfg.transport;
    delete cfg.type;
    cfg.command = d.command;
    cfg.args = d.args.map((a) => a.value).filter((v) => v !== "");
    const env = fromRows(d.env);
    if (Object.keys(env).length) cfg.env = env;
    else delete cfg.env;
    if (d.cwd.trim()) cfg.cwd = d.cwd;
    else delete cfg.cwd;
    if (d.url.trim()) cfg.url = d.url;
    else delete cfg.url;
    const headers = fromRows(d.headers);
    if (Object.keys(headers).length) cfg.headers = headers;
    else delete cfg.headers;
    if (!d.enabled) cfg.enabled = false;
    else delete cfg.enabled;
    const t = d.timeoutMs.trim();
    if (t && Number.isFinite(Number(t))) cfg.timeout_ms = Number(t);
    else delete cfg.timeout_ms;
    delete cfg.timeoutMs;
    if (d.readOnly) cfg.read_only = true;
    else delete cfg.read_only;
    delete cfg.readOnly;
    if (d.alwaysAllow) cfg.always_allow = true;
    else delete cfg.always_allow;
    delete cfg.alwaysAllow;
    if (d.toolsMode !== "all" || d.toolsList.trim()) {
      cfg.tools = {
        mode: d.toolsMode,
        list: d.toolsList.split(/\s+/).filter(Boolean),
      };
    } else {
      delete cfg.tools;
    }
    servers[name] = cfg;
  }
  return JSON.stringify({ ...doc.extraTop, mcpServers: servers }, null, 2);
}

/**
 * 归一化（解析 → 序列化）。无法解析时**原样返回**原文，保证「打开设置页即算脏」
 * 这类假阳性不会出现，也保证文本模式下的原文不被吞掉。
 */
export function normalizeMcpDoc(raw: string): string {
  const doc = parseMcpDoc(raw);
  return doc ? serializeMcpDoc(doc) : raw;
}

/** 空草稿（「添加服务器」用）。 */
export function emptyDraft(): McpServerDraft {
  return {
    name: "",
    transportRaw: null,
    command: "",
    args: [],
    env: [],
    cwd: "",
    url: "",
    headers: [],
    enabled: true,
    timeoutMs: "",
    readOnly: false,
    alwaysAllow: false,
    toolsMode: "all",
    toolsList: "",
    extra: {},
  };
}

/** 表单未展示、保存时会原样保留的键名（供卡片提示）。 */
export function extraKeysOf(d: McpServerDraft): string[] {
  return Object.keys(d.extra).sort();
}

/** 表单里展示的传输形态：已知取值归一化，未知/缺失时按 command / url 推导。 */
export function draftTransport(d: McpServerDraft): "stdio" | "streamable_http" {
  if (d.transportRaw === "streamable_http") return "streamable_http";
  if (d.transportRaw === "stdio") return "stdio";
  if (d.transportRaw) return "stdio"; // 未知取值：表单按 stdio 呈现，错误由后端定向报出
  if (d.url.trim() && !d.command.trim()) return "streamable_http";
  return "stdio";
}

/** 传输是否为文件里显式声明（供「文件内显式声明」标注）。 */
export function transportIsExplicit(d: McpServerDraft): boolean {
  return d.transportRaw === "stdio" || d.transportRaw === "streamable_http";
}

// 注：args / env / headers 现在由设置页的**表格**直接编辑（行模型即草稿模型，零编解码），
// 故此处不再提供「一行一个」的文本编解码函数——那层投影无法表达值里含换行的环境变量。