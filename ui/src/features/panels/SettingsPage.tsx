// 设置页（全屏覆盖式，[docs/settings-fullscreen-shell](../../../../docs/settings-fullscreen-shell.md) /
// 10 页 [docs/settings-ia](../../../../docs/settings-ia.md)）：绝对定位贴在内层 Layout 上的全屏页；
// 10 个分区按左导航三组铺开，draft/save 全量提交语义不变。
// 本文件承担四件事：
//   1. 容器：左导航列（返回工作区 + 运行中指示 + 三组 10 页自建导航）+ 右内容列（操作条 + 页体）；
//   2. 逐页脏标记（draft 与已保存配置的差集，字段归属由注册表 PAGE_FIELDS 驱动，即时生效项不打点）；
//   3. 离开拦截（切页 / 返回 / 页内 Esc / 关窗退出四条路径共用同一份三选弹框）；
//   4. 搜索与进阶折叠（批③，[docs/settings-search-and-advanced](../../../../docs/settings-search-and-advanced.md)）：
//      搜索命中结果**替掉**左导航（两套列表不同时存在，方向键不串味），进阶项按页级开关行内过滤
//      （只加类、零 DOM 搬迁），控件宽度统一走 .w-narrow / .w-mid / .w-wide 三档。
// 页面 JSX 手写（不做数据驱动渲染）：注册表只提供页序、页名、分组与字段归属。
import { Fragment, useEffect, useMemo, useRef, useState } from "react";
import type { ComponentType, KeyboardEvent as ReactKeyboardEvent, ReactNode } from "react";
import {
  App, Button, Empty, Form, Input, InputNumber, Modal, Popconfirm, Radio, Select, Slider, Switch, Tooltip, Typography,
} from "antd";
import {
  ApiOutlined, ArrowLeftOutlined, BgColorsOutlined, CheckSquareOutlined, DeleteOutlined, DeploymentUnitOutlined,
  GlobalOutlined, InfoCircleOutlined, ProfileOutlined, RobotOutlined, SafetyCertificateOutlined,
  ThunderboltOutlined,
} from "@ant-design/icons";
import { useTranslation } from "react-i18next";
import { ipc } from "../../ipc/client";
import { DEFAULT_POST_WRITE_CHECK } from "../../ipc/types";
import type { CleanupOutcome, CleanupPreview, CleanupStatus, ConfigState, PostWriteCheckSettings, ShellInfo, SkillMeta } from "../../ipc/types";
// 清理提示的去重口径与启动轻提示共用一份（详见 utils/cleanupNotice.ts）：设置页展示过结果就写记录
import { markCleanupNoticeSeen } from "../../utils/cleanupNotice";
import { originLabel } from "../../utils/skills";
import { clampNavWidth } from "../../utils/layout";
import { respondExitRequest } from "../../utils/uiState";
import { useActiveId, useSessions } from "../../stores/sessions";
import { useRun } from "../../stores/run";
import { useSettings } from "../../stores/settings";
import { useUi } from "../../stores/ui";
import { useDisplayWidths } from "../shell/useDisplayWidths";
import { AboutSettings } from "./AboutSettings";
import McpStatusTable, { mcpStatusRow, type McpStatusRow } from "./McpStatusTable";
import { AppearanceSettings } from "./FontSettings";
import ProvidersPanel, { validateProvider } from "./ProvidersPanel";
import {
  ADVANCED_ITEM_IDS,
  INSTANT_APPLY_FIELD_IDS,
  MCP_FIELD_ID,
  PAGE_FIELDS,
  PAGE_GROUPS,
  PAGE_LABEL_KEY,
  PAGE_ORDER,
  SETTINGS_ADVANCED_PREF_KEY,
  advancedCountByPage,
  matchSettings,
  normalizePageKey,
  type PageKey,
  type SettingFieldPath,
  type SettingItem,
} from "./settingsRegistry";

const { TextArea } = Input;

/** 自定义代理地址前缀白名单（与后端 reqwest 支持一致；保存校验用） */
const PROXY_URL_RE = /^(https?|socks5h?):\/\//;

/** 左导航列基准宽（= 工作区左栏默认宽 280，复用同一套栏宽度量） */
const SETTINGS_NAV_W = 280;
/** 窄窗收缩比例：导航列随窗口宽度收缩，下限/上限由 clampNavWidth（180/480）兜底 */
const SETTINGS_NAV_RATIO = 0.32;

/**
 * 页图标（Outlined 线性，跟随文本色 —— 不设 color，符合「无彩色 = 默认」的配色约定）。
 * 住在这里而不是注册表里：注册表必须保持纯数据（无 React 依赖，stores/ui.ts 直接引用它做页 key 归一）。
 * 键集由 PageKey 联合类型保证不重不漏。
 */
const PAGE_ICON: Record<PageKey, ComponentType<{ className?: string }>> = {
  appearance: BgColorsOutlined,
  providers: ApiOutlined,
  network: GlobalOutlined,
  security: SafetyCertificateOutlined,
  mcp: DeploymentUnitOutlined,
  skills: ThunderboltOutlined,
  tools: CheckSquareOutlined,
  agent: RobotOutlined,
  logs: ProfileOutlined,
  about: InfoCircleOutlined,
};

/**
 * 会话保留期档位（[docs/session-cleanup](../../../../docs/session-cleanup.md) §3 第 1 条）：`null` = 不清理，
 * 其余为天数。固定档位、不做自定义天数输入；本数组就是下拉的选项源。
 */
const RETENTION_OPTIONS: (number | null)[] = [null, 1, 3, 7, 14, 30];

/** 保留期下拉里「不清理」这一档的 Select 取值（数值档用数字字符串，两者不同形便于回读） */
const RETENTION_NEVER = "never";

/**
 * 「上次清理」时间的展示：本地化日期 + 时间。戳子解析不出来（后端记录被手改 / 旧格式）时**原样回显**
 * ——只读行必须始终有内容可看，且不得因一条坏数据把整个设置页抛掉。
 */
function formatCleanupTime(raw: string): string {
  const d = new Date(raw);
  if (Number.isNaN(d.getTime())) return raw;
  return `${d.toLocaleDateString()} ${d.toLocaleTimeString()}`;
}

/**
 * 预览里「这次会动到的东西」总数 = 会话数 + 索引外残留文件数。
 * 两者都为 0 才算没事可做（不弹确认框、不执行）：只看会话数会让「只有残留文件」的清理被静默跳过——
 * 预览口径与执行口径必须一致（[docs/session-cleanup](../../../../docs/session-cleanup.md) §4）。
 */
function cleanupPendingCount(preview: CleanupPreview): number {
  // 字段缺省兜底：老后端没有 orphan_count 时按 0 算，不因一个缺失字段让确认框永远弹不出来
  return (preview.count ?? 0) + (preview.orphan_count ?? 0);
}

/** 搜索命中后的临时高亮时长（毫秒）：到点自动摘掉 .settings-item-hit */
const HIT_HIGHLIGHT_MS = 1500;

/** 搜索结果列表的 DOM id（combobox 的 aria-controls 与 option 的 aria-activedescendant 都指向它） */
const SEARCH_LISTBOX_ID = "settings-search-listbox";

/** 进阶项 id 集合（注册表派生）：行内过滤只认这个集合，不手写第二份名单 */
const ADVANCED_ID_SET = new Set(ADVANCED_ITEM_IDS);

/** 读进阶折叠偏好（localStorage，默认收起；不可写 / 隐私模式下按收起处理，不报错） */
function readShowAdvanced(): boolean {
  try {
    return localStorage.getItem(SETTINGS_ADVANCED_PREF_KEY) === "1";
  } catch {
    return false;
  }
}

/** 逐段下钻取值（字段路径受 SettingFieldPath 联合类型约束，拼写错误在编译期暴露） */
function drill(node: unknown, path: string): unknown {
  let cur: unknown = node;
  for (const seg of path.split(".")) {
    if (cur === null || cur === undefined) return undefined;
    cur = (cur as Record<string, unknown>)[seg];
  }
  return cur;
}

/**
 * 单个字段路径的脏比较片段。缺省折叠与后端 serde default 对齐（不折缺省时，「点开又改回原样」会永远显示未保存）：
 *  - network.proxy：null 等价于 ProxyConfig::default()（mode = system，[docs/network-proxy-settings]）——
 *    否则打开设置后点一下本来就处于选中态的「系统代理」卡片就凭空染脏（字段路径按页面语义命名，指向 config.proxy）；
 *  - shell.selection / ui.ai_language：undefined 折 null（后端 serde default 语义）。
 */
function fieldSlice(c: ConfigState, path: SettingFieldPath): unknown {
  if (path === "network.proxy") return c.proxy ?? { mode: "system", url: "" };
  if (path === "ui.ai_language") return c.ui.ai_language ?? null;
  if (path === "shell.selection") return c.shell?.selection ?? null;
  return drill(c, path);
}

/**
 * 该页的字段比较片段（PAGE_FIELDS 驱动）。两类字段刻意排除：
 *  - 即时生效项（INSTANT_APPLY_FIELD_IDS）：改完立即生效，纳入就会永远显示未保存；
 *  - MCP（MCP_FIELD_ID）：不在 config 里（独立 mcp.json），脏判定在组件内按文本基线单独算。
 */
function pageSlice(c: ConfigState, page: PageKey): Record<string, unknown> {
  const out: Record<string, unknown> = {};
  for (const path of PAGE_FIELDS[page]) {
    if (path === MCP_FIELD_ID || INSTANT_APPLY_FIELD_IDS.includes(path)) continue;
    out[path] = fieldSlice(c, path);
  }
  return out;
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
type LeaveIntent = { kind: "tab"; tab: PageKey } | { kind: "leave" };

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

/**
 * 段二定位的落点：命中项锚点缺失、或存在但**没有布局盒**（0 高度锚点）时，退化为页体容器。
 *
 * 0 高度锚点的典型：`approval.command_allowlist` 的锚点容器常驻，但其内 `<Form.Item>` 只在白名单
 * 非空时才渲染（默认空）——只说「锚点存在」不够：`scrollIntoView` 与 1px 高亮双双落空，
 * 用户观感是「搜了没反应」。退化清单见 [docs/settings-search-and-advanced](../../../../docs/settings-search-and-advanced.md) §1.6。
 *
 * 判定用 `offsetHeight` 与 `getClientRects()` 的**与**：无布局引擎的测试环境里 offsetHeight 恒为 0，
 * 单看它会把全部节点都判成退化（真实浏览器里 0 高度节点才两个条件同时成立）。
 *
 * 锚点 id 有两类来源，本函数一视同仁（都是属性选择器的精确值）：注册表项 id（搜索命中）与
 * **动态行级锚点** `providers.<uuid>`（外部跳转，[docs/settings-search-and-advanced] §1.6 例外）。
 * 后者与三个供应商视图根节点共用的 `providers` 不会互撞：属性选择器是等值匹配，不是前缀匹配。
 */
function hitTargetOf(root: HTMLElement, id: string): HTMLElement | null {
  const node = root.querySelector<HTMLElement>(`[data-setting-id="${id}"]`);
  const zeroSized = !!node && node.offsetHeight === 0 && node.getClientRects().length === 0;
  if (node && !zeroSized) return node;
  return root.querySelector<HTMLElement>(".settings-pane-body");
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

/** 设置页：10 个分区（界面 / 模型与供应商 / 网络与连接 / 安全与审批 / MCP / 技能 / 写入后检查与校验 /
 *  日志 / 关于），按左导航三组铺开。draft 只改内存、「保存」一次性提交；供应商校验失败报错并跳页不落盘；
 *  MCP 支持结构化条目与原文本兜底双模式（服务器状态表住在 McpStatusTable）。
 *  容器是全屏覆盖层（绝对定位贴在内层 Layout），工作区只隐藏不卸载——运行中会话的 DOM 与滚动容器不受影响。 */

export default function SettingsPage() {
  const { t } = useTranslation();
  const { message, modal } = App.useApp();
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
  // 受控页：上收到 useUi（[docs/auth-error-guidance](../../../../docs/auth-error-guidance.md)），外部可指定页打开设置页；
  // 下方保存校验跳页也走同一 store 状态（[docs/provider-form-validation](../../../../docs/provider-form-validation.md)）
  const tab = normalizePageKey(useUi((s) => s.settingsTab));
  const setTab = (next: PageKey) => useUi.getState().setSettingsTab(next);
  /**
   * 外部命中请求（额度灰行「去设置」等入口经 `showSettingsAt` 置位）：store 只负责「打开设置 + 切页 +
   * 置锚点」，真正的滚动 + 临时高亮住在下方 effect。本组件是唯一消费方，**消费后写回 null 丢弃**——
   * 否则关掉设置再打开时，这条陈旧的请求会重新滚动/高亮一次（用户没点任何东西却跳了）。
   */
  const settingsHit = useUi((s) => s.settingsHit);
  // MCP：结构化条目；null = 原 JSON 解析失败，回退 textarea 模式避免丢配置
  const [mcpEntries, setMcpEntries] = useState<McpEntry[] | null>(null);
  const [mcpRaw, setMcpRaw] = useState("");
  /** 打开时的 MCP 文本基线（已归一化），用于逐页脏判定与「放弃改动」回退 */
  const [mcpOriginal, setMcpOriginal] = useState("");
  /** MCP 运行时状态（useUi.mcpStatus：mcp:status 事件 upsert；打开设置页与进入 MCP 页各重读一次） */
  const mcpStatus = useUi((s) => s.mcpStatus);
  /** 状态刷新中：按钮转圈 + 防重复点击（只重读状态，不触发连接 / 重连） */
  const [mcpRefreshing, setMcpRefreshing] = useState(false);
  // shell 探测：null = 探测失败（仅显示「自动」+ 失败提示），[] = 探测成功但无可用项
  const [shells, setShells] = useState<ShellInfo[] | null>(null);
  // 系统代理探测回显（resolve_proxy 命令）：undefined = 未拉取，null = 未检测到
  const [sysProxy, setSysProxy] = useState<string | null | undefined>(undefined);
  // ---------- 会话保留期与清理（[docs/session-cleanup](../../../../docs/session-cleanup.md)） ----------
  /** 上次清理记录（null = 未取到：只读行回退「还没有清理记录」）；进入设置页拉一次、每次清理后刷新 */
  const [cleanupStatus, setCleanupStatus] = useState<CleanupStatus | null>(null);
  /** 「立即清理」执行中：按钮转圈 + 防重复点击 */
  const [cleaning, setCleaning] = useState(false);
  /**
   * 「这次确认的清理还会删掉几个索引外的残留数据文件」（预览口径：确认框里写的就是它）。
   * 用 ref 而不是 state：这个数只在报完成提示时被读一次，不参与渲染，也不该让页面重渲染。
   * 确认框与完成提示必须是同一个数——用户在确认框里看到「另有 3 个残留数据文件」并点了「清理」，
   * 完成提示却少这一句，就会以为那几个文件没删（[docs/session-cleanup](../../../../docs/session-cleanup.md) §3 第 26 条）。
   * 正常路径都是一次确认紧接一次完成提示；读取时顺手清零，避免陈旧值粘到下一条无关的完成提示上。
   */
  const confirmedOrphansRef = useRef(0);

  // ---------- 批③ 搜索与进阶折叠（[docs/settings-search-and-advanced](../../../../docs/settings-search-and-advanced.md)） ----------
  /** 搜索查询串。trim 后非空即「搜索态」：结果列表替掉左导航 tablist（两套列表不同时存在） */
  const [query, setQuery] = useState("");
  /** 结果列表高亮项下标；-1 = 无默认选中（多项命中不自动跳转、不默认选中） */
  const [hitIdx, setHitIdx] = useState(-1);
  /** 进阶折叠偏好（localStorage，全局单一偏好、默认收起、跨页跨会话） */
  const [showAdvanced, setShowAdvanced] = useState<boolean>(readShowAdvanced);
  /** 被「搜索命中」临时展开的页：**不写** localStorage，离开该页即回手动偏好值 */
  const [forcedAdvanced, setForcedAdvanced] = useState<PageKey | null>(null);
  /** 待定位的命中项（**搜索命中**专用：要带 page 才能跨页走三选拦截，故只收注册表项） */
  const [pendingHit, setPendingHit] = useState<SettingItem | null>(null);
  /**
   * 待定位的**动态行级锚点**（**外部跳转**专用，如额度灰行给的 `providers.<uuid>`）。
   * 与 `pendingHit` 分开一条轻量通道：这类锚点来自动态条目（供应商行），**不在注册表里**，
   * 伪造成 `SettingItem` 就等于顺手伪造 labelKey/page —— 那正是本通道要绕开的
   * （[docs/settings-search-and-advanced](../../../../docs/settings-search-and-advanced.md) §1.6）。
   */
  const [pendingAnchorId, setPendingAnchorId] = useState<string | null>(null);
  /** 本轮要滚动 + 临时高亮的目标（带 nonce：同一目标连点两次也要重新定位）。
   *  搜索命中与外部跳转共用这一段（两者只差锚点来源），故这里只存锚点 id */
  const [hitTarget, setHitTarget] = useState<{ anchorId: string; nonce: number } | null>(null);
  const hitSeqRef = useRef(0);
  const hitTimerRef = useRef<number | null>(null);
  /** 当前挂着 .settings-item-hit 的元素：1.5s 内换项时先摘旧的（旧定时器已被 clearTimeout） */
  const hitElRef = useRef<HTMLElement | null>(null);

  /** 命中结果：显示名 + keywords + 页名 + 组名、多词 AND（纯函数 matchSettings 在注册表里，单独可测） */
  // language 是「切语言即重算」的触发器：matchSettings 的显示名来自 t，而 t 的标识不随语言变化
  // eslint-disable-next-line react-hooks/exhaustive-deps
  const results = useMemo(() => matchSettings(query, t), [query, t, language]);
  /** 搜索态：trim 后非空（空串 / 仅空格 → 不渲染结果列表，恢复常规导航） */
  const searching = query.trim() !== "";
  /** 结果列表当前高亮项的 id：越界保护（结果集收缩时 hitIdx 可能越界，不猜） */
  const activeHitId = results[hitIdx]?.id;
  /** 当前页进阶项数（页级开关文案的计数；0 → 该行不渲染） */
  const advancedCount = advancedCountByPage(tab);
  /** 进阶项是否可见：手动偏好 ∨ 当前页被搜索临时展开 */
  const advancedVisible = showAdvanced || forcedAdvanced === tab;

  /** 行内过滤判定：该进阶项现在是否被收起（只影响类名，不挪 DOM 位置） */
  function advHidden(id: string): boolean {
    return !advancedVisible && ADVANCED_ID_SET.has(id);
  }

  /** 设置项锚点包裹层的类名（锚点 + 折叠类；锚点值 = 注册表 id） */
  function anchorCls(id: string): string {
    return `setting-anchor${advHidden(id) ? " settings-advanced-hidden" : ""}`;
  }

  /**
   * 页级进阶开关：只写 localStorage，**不碰 draft** —— 折叠切换因此不会产生未保存改动；
   * 进阶项自身的改动照旧由 PAGE_FIELDS 判定（语义不变）。
   */
  function toggleAdvanced(next: boolean) {
    setShowAdvanced(next);
    try {
      localStorage.setItem(SETTINGS_ADVANCED_PREF_KEY, next ? "1" : "0");
    } catch {
      // 隐私模式 / 配额不可写：本次会话内仍然生效（不外抛、不阻断开关）
    }
  }

  /** 清空搜索（Esc 第一次按 / 清除按钮）：连高亮下标一并复位，避免下次搜索带着旧下标 */
  function clearSearch() {
    setQuery("");
    setHitIdx(-1);
  }

  /**
   * 命中跳转：切页走与点击导航**同一条** onTabChange（脏改动存在时同样先走三选拦截，
   * 选「留在原地」则不跳并放弃本次定位）；命中项已在当前页时只定位，不切页、不改导航选中态。
   */
  function jumpToItem(item: SettingItem) {
    setPendingHit(item);
    if (item.page !== tab) onTabChange(item.page);
  }

  /**
   * 搜索框键盘：↑/↓ 只动结果列表（无默认选中：-1 起 ↑/↓ 都落到第 0 项，两端停住不循环）；
   * Enter 跳转后**焦点留在搜索框**（不主动移焦，故无需回焦）；未选中时 Enter 不动作。
   * IME 组合中（isComposing）不拦 ↑/↓ 与 Enter；Esc 交给全局链统一处置（先清空查询）。
   */
  function onSearchKeyDown(e: ReactKeyboardEvent<HTMLInputElement>) {
    if (e.nativeEvent.isComposing) return;
    if (e.key === "ArrowDown" || e.key === "ArrowUp") {
      if (results.length === 0) return;
      e.preventDefault();
      setHitIdx((prev) => {
        const next = e.key === "ArrowDown" ? prev + 1 : prev - 1;
        return Math.min(Math.max(next, 0), results.length - 1);
      });
      return;
    }
    if (e.key === "Enter") {
      const hit = results[hitIdx];
      if (!hit) return; // 无默认选中：未显式选中时 Enter 不动作
      e.preventDefault();
      jumpToItem(hit);
    }
  }

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
      // null = 原 JSON 解析失败 → 进入「文本兜底模式」（只有 mcpEntries === null 才走那个分支）。
      // 注意不能写成 `parsed ?? []`：那样兜底模式在真实使用中不可达——解析不了的 mcp.json 既不显示、
      // 点「保存并重连」还会被空配置覆盖（数据丢失）。回归用例：settings.mcp.test.tsx。
      setMcpEntries(parsed);
      setMcpRaw(normalized);
      setMcpOriginal(normalized);
      setSkills(await ipc.listSkills(sessionId).catch(() => []));
      const st = await ipc.mcpStatus().catch(() => []);
      useUi.setState({ mcpStatus: st });
      // shell 探测失败不阻塞面板：仅回退「自动」选项 + 失败提示
      setShells(await ipc.listAvailableShells().catch(() => null));
      // 系统代理探测回显：失败不阻塞（null = 未检测到提示）
      setSysProxy(await ipc.resolveProxy().catch(() => null));
      // 上次清理记录（只读行数据源）：同样失败静默 —— 只读信息缺失不该阻断设置页
      setCleanupStatus(await ipc.getCleanupStatus().catch(() => null));
    })();
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  /**
   * 展示即算「用户已经看过这次清理的结果」：写下与启动轻提示共用的去重记录
   * （[docs/session-cleanup](../../../../docs/session-cleanup.md) §3 第 14 条——两处说的是后端同一条记录）。
   * 所以设置页一打开就读到状态、或本页执行完清理刷新出状态，下次启动都不再为同一件事提醒。
   * 设置页只在打开时才挂载（AppShell 里 `{settingsOpen && <SettingsPage />}`），因此不会误呑启动提示。
   */
  useEffect(() => {
    markCleanupNoticeSeen(cleanupStatus?.last_run_at);
  }, [cleanupStatus?.last_run_at]);

  function patchDraft(patch: Partial<ConfigState>) {
    setDraft((prev) => (prev ? { ...prev, ...patch } : prev));
  }

  /** post_write_check 段局部更新（写入后检查命令；缺字段时以 DEFAULT_POST_WRITE_CHECK 为基准） */
  function patchPostWrite(patch: Partial<PostWriteCheckSettings>) {
    if (!draft) return;
    patchDraft({
      post_write_check: { ...(draft.post_write_check ?? DEFAULT_POST_WRITE_CHECK), ...patch },
    });
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

  // ---------- 会话保留期与清理（[docs/session-cleanup](../../../../docs/session-cleanup.md)） ----------

  /** 保留期下拉的草稿写入（"never" = 不清理 → null）；走页级保存，不是即时生效项。
   *  展开原有 sessions 段：后端将来往这一段加字段时，不会因改一次下拉就把别的字段抹掉 */
  function patchRetention(value: string) {
    patchDraft({ sessions: { ...draft?.sessions, retention_days: value === RETENTION_NEVER ? null : Number(value) } });
  }

  /** 「上次清理」只读文案：时间 + 删除条数（+ 失败条数）；没有记录时说清「还没有」而不是留空 */
  function cleanupStatusText(): string {
    const at = cleanupStatus?.last_run_at ?? null;
    if (!at) return t("settings.cleanupNeverRun");
    const base = t("settings.cleanupLastRun", { time: formatCleanupTime(at), n: cleanupStatus?.last_deleted ?? 0 });
    const failed = cleanupStatus?.last_failed ?? 0;
    return failed > 0 ? `${base}${t("settings.cleanupLastFailed", { n: failed })}` : base;
  }

  /**
   * 清理确认框内容（删除条数 + 前几条会话标题）：手动「立即清理」与「保存前确认」两条路径
   * 共用同一份版式，避免两处各写一套说法（[docs/session-cleanup](../../../../docs/session-cleanup.md) §3 第 12/26 条）。
   * 两种情形分开写：有会话要删时说的是「删 N 个会话（不可恢复）」+ 前几条标题；
   * **只有残留文件**时（会话一条不删，要删的是索引外的历史 / 边车文件）另给一条说明——
   * 那些会话早就不在列表里，写「删 N 个会话」用户认不出是谁，也不符合实际；
   * **两者都有**时会话那句照旧，再补一句残留文件条数（否则那些文件会在用户不知情的情况下被删掉）。
   */
  function cleanupConfirmContent(preview: CleanupPreview) {
    const orphans = preview.orphan_count ?? 0;
    const onlyOrphans = (preview.count ?? 0) === 0 && orphans > 0;
    // 会话与残留文件都要删时：只说「将删除 N 个会话」漏掉了残留文件，用户不知道这次还会动别的数据
    const alsoOrphans = !onlyOrphans && orphans > 0;
    return (
      <div style={{ fontSize: 12.5, display: "grid", gap: 6 }}>
        <div>
          {onlyOrphans
            ? t("settings.cleanupOrphanDesc", { n: orphans })
            : t("settings.cleanupConfirmDesc", { n: preview.count })}
        </div>
        {alsoOrphans && <div>{t("settings.cleanupOrphanExtra", { n: orphans })}</div>}
        {preview.titles.length > 0 && (
          <div>
            <div className="dim">{t("settings.cleanupConfirmListTitle")}</div>
            {preview.titles.map((title, idx) => (
              <div key={`${idx}-${title}`} style={{ overflowWrap: "anywhere" }}>
                {title}
              </div>
            ))}
          </div>
        )}
      </div>
    );
  }

  /** 弹确认框并把「确认 / 取消」折成布尔值：true = 用户点了确认（清理），false = 取消。
   *  onOk 与 onCancel 只会结算一次，不双应答 */
  function confirmCleanup(preview: CleanupPreview, title: string, okText: string, cancelText: string): Promise<boolean> {
    return new Promise<boolean>((resolve) => {
      let settled = false;
      const settle = (value: boolean) => {
        if (settled) return;
        settled = true;
        resolve(value);
      };
      modal.confirm({
        title,
        content: cleanupConfirmContent(preview),
        okText,
        cancelText,
        okButtonProps: { danger: true },
        onOk: () => {
          // 记下确认框里承诺的残留文件数：完成提示要复述同一个数（取消则不留值）
          confirmedOrphansRef.current = preview.orphan_count ?? 0;
          settle(true);
        },
        onCancel: () => {
          confirmedOrphansRef.current = 0;
          settle(false);
        },
      });
    });
  }

  /**
   * 清理失败的警示（手动与保存两条路径共用）：后端 `failed > 0` = 有会话没删干净（索引写失败 / 文件删不掉）。
   * 不报出来时用户只看到「已清理 0 个会话，关闭 0 个标签页」，会以为什么都没发生
   * （[docs/session-cleanup](../../../../docs/session-cleanup.md) §3 第 14 条）。
   */
  function warnCleanupFailed(outcome: CleanupOutcome) {
    const failed = outcome.failed ?? 0;
    if (failed > 0) message.warning(t("settings.cleanupFailed", { n: failed }));
  }

  /** 清理收尾（手动与保存两条路径共用）：关掉被删会话的 Tab → 刷新「上次清理」→ 报条数与关闭的标签页数 */
  async function applyCleanupOutcome(outcome: CleanupOutcome) {
    const closed = useSessions.getState().applyCleanup(outcome.ids);
    setCleanupStatus(await ipc.getCleanupStatus().catch(() => null));
    const done = t("settings.cleanupDone", { n: outcome.deleted, m: closed });
    // 本次确认的清理若同时动了索引外的残留文件，完成提示要一并说出来（读完即清零：只报这一次）
    const orphans = confirmedOrphansRef.current;
    confirmedOrphansRef.current = 0;
    message.success(orphans > 0 ? `${done}${t("settings.cleanupDoneOrphans", { n: orphans })}` : done);
    warnCleanupFailed(outcome);
  }

  /**
   * 手动「立即清理」：用**已保存**的保留期预览（不是草稿值）→ 确实有事可做（会话数 + 索引外残留文件数
   * 之和 > 0）才弹确认框，两者都是 0 则给一条轻提示、不打断 → 执行 → 报结果
   * （[docs/session-cleanup](../../../../docs/session-cleanup.md) §3 第 13/25/26 条）。
   */
  async function runCleanupNow() {
    if (cleaning || cleanupBlock !== null || savedRetention === null) return;
    setCleaning(true);
    try {
      const preview = await ipc.previewSessionCleanup(savedRetention);
      if (cleanupPendingCount(preview) === 0) {
        message.info(t("settings.cleanupNonePending"));
        return;
      }
      const ok = await confirmCleanup(
        preview,
        t("settings.cleanupConfirmTitle"),
        t("settings.cleanupConfirmOk"),
        t("common.cancel"),
      );
      if (!ok) return;
      await applyCleanupOutcome(await ipc.runSessionCleanup());
    } catch (e) {
      message.error(String(e));
    } finally {
      setCleaning(false);
    }
  }

  /**
   * 保存前的清理确认（[docs/session-cleanup](../../../../docs/session-cleanup.md) §3 第 12/27 条）：
   * 只在「保留期相对已保存值发生了变化」且新值不是「不清理」时才问，且预览里确实有事可做
   * （会话数 + 索引外残留文件数之和 > 0；只有残留文件时同样要问）才弹框。
   * 返回 true = 本次跳过清理（取消按钮已写明：配置照常保存，下次启动仍按新保留期清理）。
   * 预览失败时同样按「本次不清理」处理并提示：宁可少删一次，也不在用户没确认的情况下删数据。
   * 注：离开拦截的「保存并离开」复用本函数所在的 save()，因此那条路径同样会走这次确认（有意为之）。
   */
  async function confirmSaveCleanup(): Promise<boolean> {
    const next = draftRetention;
    if (next === null || next === savedRetention) return false;
    const preview = await ipc.previewSessionCleanup(next).catch(() => undefined);
    if (!preview) {
      message.warning(t("settings.cleanupPreviewFailed"));
      return true;
    }
    if (cleanupPendingCount(preview) === 0) return false;
    const confirmed = await confirmCleanup(
      preview,
      t("settings.cleanupSaveConfirmTitle"),
      t("settings.cleanupConfirmOk"),
      t("settings.cleanupSaveSkip"),
    );
    return !confirmed; // 未点确认（取消 / 关闭确认框）= 本次跳过清理，配置照常保存
  }

  /** 保存（全量提交语义不变）。返回是否真的落盘：离开拦截靠它判断能否执行「保存并离开」 */
  async function save(): Promise<boolean> {
    if (!draft) return false;
    // 代理地址校验（网络页）：仅自定义模式且非空时校验前缀白名单，非法跳页不落盘
    if (draft.proxy?.mode === "manual") {
      draft.proxy.url = draft.proxy.url.trim();
      if (draft.proxy.url !== "" && !PROXY_URL_RE.test(draft.proxy.url)) {
        message.error(t("settings.proxyUrlInvalid"));
        setTab("network");
        // 校验失败 = 本次离开动作没执行 → 命中的定位一并作废（同「留在原地」：
        // 否则用户之后手动切到目标页会莫名滚动 + 高亮）
        setPendingHit(null);
        return false;
      }
    }
    // 供应商字段校验（[docs/provider-form-validation](../../../../docs/provider-form-validation.md)/29）：无效时逐项报错、跳转供应商页、不落盘
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
      setPendingHit(null); // 同上：报错跳页不是「离开」，定位作废
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
    // 写入后检查命令落盘前归一化：命令 trim（空命令与关闭等价，后端也会按空命令跳过）
    if (draft.post_write_check) {
      draft.post_write_check = {
        ...draft.post_write_check,
        command: draft.post_write_check.command.trim(),
      };
    }
    setSaving(true);
    try {
      // 保留期有变化且新值非「不清理」时先弹确认框（取消 = 本次跳过清理，配置照常落盘）
      const skipCleanup = await confirmSaveCleanup();
      const outcome = await useSettings.getState().save(draft, skipCleanup ? { skipCleanup: true } : undefined);
      // 保存成功不关闭页面（[docs/provider-form-validation](../../../../docs/provider-form-validation.md)）：仅提示；何时关闭由用户决定
      message.success(t("common.saved"));
      if (skipCleanup) message.info(t("settings.cleanupSavedSkipped"));
      if (outcome) {
        // 后端本次确实执行了清理：真删到会话才收尾 Tab 并报结果（applyCleanupOutcome 内含刷新与两条提示）；
        // 只记了失败条数（deleted = 0）时只刷新只读行，但失败条数同样要报出来
        // ——「一条都没删、还有 N 条失败」正是最容易被误解成「什么都没发生」的情形
        if (outcome.deleted > 0) {
          await applyCleanupOutcome(outcome);
        } else {
          setCleanupStatus(await ipc.getCleanupStatus().catch(() => null));
          warnCleanupFailed(outcome);
        }
      }
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

  async function saveMcp() {
    try {
      // 结构化模式：条目 -> JSON；兜底模式：原文本原样保存
      const json = mcpEntries !== null ? serializeMcpEntries(mcpEntries) : mcpRaw;
      await ipc.saveMcpConfig(json);
      message.success(t("common.saved"));
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

  /** 刷新 MCP 状态：只重读 mcp_status（不会重连）——手动动作越少越好，避免用户误以为刷新 = 重连 */
  async function refreshMcpStatus() {
    setMcpRefreshing(true);
    try {
      // 失败保留旧值：整份替成 [] 会把「一次 IPC 抖动」伪装成「所有服务器都没连接」，
      // 而「未连接」在本页是**正常态**文案（见状态表下方的说明），误导性最强
      const st = await ipc.mcpStatus().catch(() => null);
      if (st) useUi.setState({ mcpStatus: st });
      else message.error(t("settings.mcpStatusRefreshFailed"));
    } finally {
      setMcpRefreshing(false);
    }
  }

  /**
   * 状态表行 = 配置名单 ∪ 状态记录：
   *  · 只取配置名单会漏掉「管理端仍持有连接、但配置里已删掉」的服务器；
   *  · 只取状态记录则会在保存配置后（后端 stop_all 清空、且无会话不重连）得到空表，
   *    看起来像「没配置服务器」——故两侧取并集，并把没记录的一律显示为「未连接」。
   */
  const mcpStatusRows = useMemo<McpStatusRow[]>(() => {
    // 文本兜底模式（原 JSON 解析失败）拿不到配置名单：那一半按空处理，状态表只列管理端已知的服务器
    const names: string[] = [];
    for (const e of mcpEntries ?? []) {
      const n = e.name.trim();
      if (n && !names.includes(n)) names.push(n);
    }
    const byName = new Map(mcpStatus.map((s) => [s.name, s]));
    const rows = names.map((n) => mcpStatusRow(n, byName.get(n)));
    for (const s of mcpStatus) if (!names.includes(s.name)) rows.push(mcpStatusRow(s.name, s));
    return rows;
  }, [mcpEntries, mcpStatus]);

  /**
   * 切到 MCP 页时重读一次状态：`mcp:status` 事件只在 connect_mcp 之后触发（开会话 / 保存配置），
   * 久留设置页会看到陈旧状态；「切页」本身就是用户主动要看状态的信号。
   */
  useEffect(() => {
    if (tab !== "mcp") return;
    void refreshMcpStatus();
    // 只在切页时重读：refreshMcpStatus 每次渲染都是新引用，纳入依赖会退化成频繁 refetch
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [tab]);

  // 代理模式视图态：proxy=null（从未配置）显示为「系统代理」——与 HTTP 栈默认行为一致（诚实呈现）
  const proxyMode = draft?.proxy?.mode ?? "system";
  const proxyUrl = draft?.proxy?.url ?? "";
  const proxyUrlInvalid = proxyMode === "manual" && proxyUrl.trim() !== "" && !PROXY_URL_RE.test(proxyUrl.trim());
  // 写入后检查视图态：旧配置缺该段时以 DEFAULT_POST_WRITE_CHECK（后端默认）为基准回显
  const postWrite = draft?.post_write_check ?? DEFAULT_POST_WRITE_CHECK;

  function patchProxyMode(mode: "none" | "system" | "manual") {
    // 切模式保留已填地址：来回切换不丢草稿
    patchDraft({ proxy: { mode, url: draft?.proxy?.url ?? "" } });
  }

  // ---------- 逐页脏标记与离开拦截（[docs/settings-fullscreen-shell](../../../../docs/settings-fullscreen-shell.md)） ----------
  const config = useSettings((s) => s.config);
  // MCP 不在 config 内（独立 mcp.json）：脏判定 = 当前文本与打开时基线的差集，挂在拥有它的 mcp 页
  // （MCP 配置拆成独立页后脏点必须跟着页走：挂在 tools 页会出现「改了 MCP、脏点却亮在写入后检查页」）
  const mcpSerialized = mcpEntries !== null ? serializeMcpEntries(mcpEntries) : mcpRaw;
  const mcpDirty = mcpSerialized !== mcpOriginal;
  const dirtyMap = useMemo(() => {
    const out = {} as Record<PageKey, boolean>;
    for (const page of PAGE_ORDER) {
      const configDirty = !!draft && !!config && !sameSlice(pageSlice(draft, page), pageSlice(config, page));
      out[page] = configDirty || (page === "mcp" && mcpDirty);
    }
    return out;
  }, [draft, config, mcpDirty]);
  const anyDirty = PAGE_ORDER.some((k) => dirtyMap[k]);

  // 保留期两态：draft = 下拉草稿值；config = 已保存值 —— 立即清理按钮与保存前确认都只认**已保存**值
  const draftRetention = draft?.sessions?.retention_days ?? null;
  const savedRetention = config?.sessions?.retention_days ?? null;
  /** 保留期有未保存改动：立即清理按钮禁用（按钮用的是已保存保留期，先保存才轮得到新档位） */
  const retentionDirty = draftRetention !== savedRetention;
  /**
   * 「立即清理」的禁用原因（null = 可点）。先判「不清理」：保留期为空时脏不脏都无从清理；
   * 再判未保存改动：此时点它会按旧保留期删，与用户刚选的档位不是一回事。
   */
  const cleanupBlock = savedRetention === null ? "none" : retentionDirty ? "dirty" : null;

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
    // 同加载路径：解析失败要回到文本兜底模式（而不是空结构化列表）
    setMcpEntries(parsed);
    setMcpRaw(parsed ? serializeMcpEntries(parsed) : mcpOriginal);
  }

  /** 顶部「取消」= 放弃全部未保存改动并返回工作区（不再二次确认） */
  function cancelAll() {
    discardDraft();
    closeShell();
  }

  /** 切页：脏改动存在时先走三选拦截（保存并离开 → 落盘后跳页；放弃 → 回基线后跳页；留在原地 → 停在本页） */
  function onTabChange(next: PageKey) {
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
      // 搜索跳转被拦截：留在原地 → 丢弃本次定位（否则以后手动切到该页会莫名高亮）
      setPendingHit(null);
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
      // （本次定位已由 save() 的两条校验失败分支丢弃，这里只需清离开意图）
      if (!ok) {
        setLeaveIntent(null);
        return;
      }
    } else {
      // 「放弃改动」**不丢**定位：跳转照常执行（与「保存并离开」同形），故目标页仍要高亮
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

  // 页内 Esc：捕获阶段注册并 preventDefault，抢在 AppShell 的全局 Esc（停止运行中会话）之前收口。
  // 优先级（批③ 把搜索态插在最前）：三选弹框 / 浮层 → 清空搜索查询 → 既有「返回工作区」链。
  const escRef = useRef<{ blocked: boolean; close: () => void; clearSearch: () => void }>({
    blocked: false, close: () => {}, clearSearch: () => {},
  });
  /** 查询串的同步镜像：Esc 走的是 window 捕获监听（不随 query 重挂），故用 ref 取最新值 */
  const queryRef = useRef("");
  useEffect(() => {
    escRef.current = { blocked: confirmOpen, close: requestClose, clearSearch };
    queryRef.current = query;
  });
  useEffect(() => {
    const onKeydown = (e: KeyboardEvent) => {
      if (e.key !== "Escape" || e.isComposing) return;
      if (escRef.current.blocked || overlayOpen()) return; // 浮层优先：让位给组件库，不平级返回
      if (queryRef.current.trim() !== "") {
        // 搜索态：Esc 第一次只清空查询（焦点留在搜索框），清空后再按才回落既有 Esc 链
        e.preventDefault();
        escRef.current.clearSearch();
        return;
      }
      e.preventDefault();
      escRef.current.close();
    };
    window.addEventListener("keydown", onKeydown, true);
    return () => window.removeEventListener("keydown", onKeydown, true);
  }, []);

  // 可达性：设置页是全屏 dialog，打开时把焦点交给导航首项（「返回工作区」）。
  // 刻意**不**引入焦点陷阱库（本批范围是容器化）：只保证键盘用户不必先穿过整个工作区。
  // 批③ 起用专用类名 .settings-nav-back 定位，不再泛选 `.settings-nav-head button`：
  // 「搜索框是 antd Input + allowClear，值非空时渲染一个清除 button，泛选会把焦点抢到清除键上」
  // 是**防御性**表述——搜索框挂载时值恒为空、清除按钮不存在，该保护当前不可构造验证；
  // 保留专用类名定位以防未来实现变化（如打开设置时恢复上次查询）。
  const shellRef = useRef<HTMLDivElement | null>(null);
  useEffect(() => {
    shellRef.current?.querySelector<HTMLButtonElement>(".settings-nav-back")?.focus();
  }, []);

  // ---------- 搜索命中定位（两段式：跨页 / 需临时展开时，目标节点当帧还不存在） ----------
  // 段一：目标页成为当前页（切页完成、脏改动三选已放行）后，需要时先临时展开该页进阶行，
  //       再把命中项交给段二；命中项在当前页时 tab 不变也照样命中（不切页、不改导航选中态）。
  useEffect(() => {
    if (!pendingHit || pendingHit.page !== tab) return;
    if (!advancedVisible && ADVANCED_ID_SET.has(pendingHit.id)) setForcedAdvanced(tab);
    setPendingHit(null);
    setHitTarget({ anchorId: pendingHit.id, nonce: ++hitSeqRef.current });
  }, [pendingHit, tab, advancedVisible]);

  // 段一·外部跳转：`useUi.settingsHit` 里的锚点是**动态行级锚点**（没有注册表项，也没有进阶折叠要展开），
  // 但「页体已渲染」这一关比搜索路径更硬：供应商页体挂在 `body: draft && (...)` 上，而 draft 要等本组件
  // 挂载后回填。冷启动（设置页首次打开）时若不等 draft，锚点当帧根本不存在，定位会静默落空
  // （观感：点了「去设置」没反应）。故把 draft 就绪纳入就绪条件。
  useEffect(() => {
    if (!settingsHit) return;
    const anchorId = settingsHit.anchorId;
    // 消费即丢弃：store 里这条请求是一次性的（同一行连点两次靠新的对象 + nonce 区分）
    useUi.setState({ settingsHit: null });
    setPendingAnchorId(anchorId);
  }, [settingsHit]);

  // 段一·就绪判定：锚点已在 DOM（不依赖 draft 的页）即刻定位；否则等 draft 回填后再来一次。
  // tab 也在依赖里：切页本身会让锚点出现或消失
  useEffect(() => {
    if (!pendingAnchorId) return;
    const root = shellRef.current;
    if (!root) return;
    if (!draft && !root.querySelector(`[data-setting-id="${pendingAnchorId}"]`)) return;
    setPendingAnchorId(null);
    setHitTarget({ anchorId: pendingAnchorId, nonce: ++hitSeqRef.current });
  }, [pendingAnchorId, draft, tab]);

  // 段二：节点已渲染（页体 + 临时展开都就位）→ 滚动到该项并加临时高亮类，约 1.5s 后摘掉。
  // 锚点缺失 / 0 高度锚点时退化为高亮页体容器（退化清单见 [docs/settings-search-and-advanced](../../../../docs/settings-search-and-advanced.md) §1.6）。
  // 外部跳转给的动态行级锚点（`providers.<uuid>`）走的是同一个 hitTargetOf：它按属性选择器精确匹配，
  // 天然不会误命中三个视图根节点的 `providers`。
  useEffect(() => {
    if (!hitTarget) return;
    const root = shellRef.current;
    if (!root) return;
    const el = hitTargetOf(root, hitTarget.anchorId);
    if (!el) return;
    el.scrollIntoView?.({ block: "center" });
    // 命令式加类：高亮不属于渲染态（1.5s 后自动摘），故不往渲染态里塞第二个状态。
    // 先摘掉上一次的高亮：1.5s 内换项时旧定时器已被 clearTimeout，旧元素上的类再无人移除。
    hitElRef.current?.classList.remove("settings-item-hit");
    el.classList.add("settings-item-hit");
    hitElRef.current = el;
    if (hitTimerRef.current !== null) window.clearTimeout(hitTimerRef.current);
    hitTimerRef.current = window.setTimeout(() => {
      el.classList.remove("settings-item-hit");
      if (hitElRef.current === el) hitElRef.current = null;
      hitTimerRef.current = null;
    }, HIT_HIGHLIGHT_MS);
  }, [hitTarget]);

  // 离开被临时展开的页 → 回手动偏好值（临时展开只在本页有效，且从未写过 localStorage）
  useEffect(() => {
    setForcedAdvanced((prev) => (prev && prev !== tab ? null : prev));
  }, [tab]);

  // 卸载时清掉高亮定时器，并摘掉仍挂着的临时高亮类（元素随组件销毁，留类只会污染测试与复挂场景）
  useEffect(() => () => {
    if (hitTimerRef.current !== null) window.clearTimeout(hitTimerRef.current);
    hitElRef.current?.classList.remove("settings-item-hit");
    hitElRef.current = null;
  }, []);

  /** 10 页清单：页名键与页序来自注册表，页体按当前页渲染到右列（不做数据驱动渲染） */
  const pages: { key: PageKey; labelKey: string; body: ReactNode }[] = [
    {
      key: "appearance",
      labelKey: PAGE_LABEL_KEY.appearance,
      // 界面页：主题 + 双字体槽 + 界面语言（后两者与主题一样即时生效，不参与脏标记）
      body: <AppearanceSettings draft={draft} patchDraft={patchDraft} />,
    },
    {
      key: "providers",
      labelKey: PAGE_LABEL_KEY.providers,
      body: draft && (
        <>
          <Form layout="vertical">
            {/* AI 回复语言：自由输入；留空 = 跟随会话语言。
                经系统提示词 <reply-language> 指令下发（core/prompt.rs）。 */}
            <Form.Item label={t("settings.aiLanguage")} extra={t("settings.aiLanguageHint")}>
              {/* 锚点（= 注册表 id）+ 宽度档：批③ 搜索定位与三档宽度都从这里走 */}
              <div className="setting-anchor" data-setting-id="ui.ai_language">
                <Input
                  size="small"
                  className="w-mid"
                  maxLength={40}
                  placeholder={t("settings.aiLanguagePlaceholder")}
                  value={draft.ui.ai_language ?? ""}
                  onChange={(e) => {
                    const v = e.target.value;
                    patchDraft({ ui: { ...draft.ui, ai_language: v.trim() === "" ? null : v } });
                  }}
                />
              </div>
            </Form.Item>
          </Form>
          <ProvidersPanel draft={draft} patchDraft={patchDraft} />
        </>
      ),
    },
    {
      key: "network",
      labelKey: PAGE_LABEL_KEY.network,
      body: draft && (
        <Form layout="vertical">
          <Form.Item label={t("settings.proxyMode")}>
            {/* 锚点挂在模式卡片区（network.proxy 的主控件；代理地址是同一项的子字段，只在自定义模式出现） */}
            <div className="setting-anchor" data-setting-id="network.proxy">
              {/* heroui radio-group 风格：整卡可点的三选一卡片，选中墨色描边（样式 .proxy-mode-card） */}
              <Radio.Group value={proxyMode} onChange={(e) => patchProxyMode(e.target.value)}>
                <div className="proxy-mode-list">
                  {([
                    ["none", t("settings.proxyNone"), t("settings.proxyNoneDesc")],
                    ["system", t("settings.proxySystem"), t("settings.proxySystemDesc")],
                    ["manual", t("settings.proxyManual"), t("settings.proxyManualDesc")],
                  ] as const).map(([mode, title, desc]) => (
                    <div key={mode} className={`proxy-mode-card${proxyMode === mode ? " active" : ""}`}>
                      {/* 整卡可点：主区（标题 + 说明 + 回显）是 label，点哪都能选中该模式 */}
                      <label className="proxy-mode-main">
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
                      {/* 自定义模式独有的地址输入：放进卡片**内部**（不再单独占一行漂在卡片下方）。
                          label 与输入框是字段区自己的，不嵌在外层 label 里（label 不能嵌 label） */}
                      {mode === "manual" && proxyMode === "manual" && (
                        <div className="proxy-mode-field">
                          <Form.Item
                            label={t("settings.proxyUrl")}
                            extra={t("settings.proxyUrlHint")}
                            validateStatus={proxyUrlInvalid ? "error" : undefined}
                            help={proxyUrlInvalid ? t("settings.proxyUrlInvalid") : undefined}
                          >
                            <Input
                              size="small"
                              className="w-wide"
                              placeholder="http://127.0.0.1:7890 或 socks5://127.0.0.1:1080"
                              value={proxyUrl}
                              onChange={(e) => patchDraft({ proxy: { mode: "manual", url: e.target.value } })}
                            />
                          </Form.Item>
                        </div>
                      )}
                    </div>
                  ))}
                </div>
              </Radio.Group>
            </div>
          </Form.Item>
          {/* 内网访问（批② 从「安全」页迁入本页：网络可达性归网络） */}
          <Form.Item label={t("settings.allowPrivate")}>
            <div className="setting-anchor" data-setting-id="network.allow_private_network">
              <Switch checked={draft.network.allow_private_network} onChange={(v) => patchDraft({ network: { allow_private_network: v } })} />
            </div>
          </Form.Item>
        </Form>
      ),
    },
    {
      key: "security",
      labelKey: PAGE_LABEL_KEY.security,
      body: draft && (
        <Form layout="vertical">
          <Form.Item label={t("settings.approvalEnabled")}>
            <div className="setting-anchor" data-setting-id="approval.enabled">
              <Switch checked={draft.approval.enabled} onChange={(v) => patchDraft({ approval: { ...draft.approval, enabled: v } })} />
            </div>
          </Form.Item>
          <Form.Item label={t("settings.confirmOutside")}>
            <div className="setting-anchor" data-setting-id="approval.confirm_outside_create">
              <Switch checked={draft.approval.confirm_outside_create} onChange={(v) => patchDraft({ approval: { ...draft.approval, confirm_outside_create: v } })} />
            </div>
          </Form.Item>
          <Form.Item label={t("settings.confirmPush")}>
            <div className="setting-anchor" data-setting-id="approval.confirm_git_push">
              <Switch checked={draft.approval.confirm_git_push} onChange={(v) => patchDraft({ approval: { ...draft.approval, confirm_git_push: v } })} />
            </div>
          </Form.Item>
          {/* docs/ask-ink-accent-and-composer-cover：审批等待策略——勾选后 5 分钟无应答自动确认推荐选项（allowed），不勾 = 永不超时 */}
          <Form.Item label={t("settings.autoConfirm")} extra={t("settings.autoConfirmHint")}>
            <div className="setting-anchor" data-setting-id="approval.auto_confirm">
              <Switch checked={draft.approval.auto_confirm} onChange={(v) => patchDraft({ approval: { ...draft.approval, auto_confirm: v } })} />
            </div>
          </Form.Item>
          {/* docs/run-queue-and-ask-revamp：「始终允许本项目」命令白名单（审批时选择加入，此处管理/移除）。
              存储条目 = cwd \u{1} 完整命令文本（cwd 跟随项目 -> 白名单不跨项目生效），展示时拆开。
              advanced 项：包一层锚点容器，收起时整块（含 Form.Item 标签）加类隐藏 —— 零 DOM 搬迁 */}
          <div className={anchorCls("approval.command_allowlist")} data-setting-id="approval.command_allowlist">
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
                          {t("common.delete")}
                        </Button>
                      </div>
                    );
                  })}
                </div>
              </Form.Item>
            )}
          </div>
        </Form>
      ),
    },
    {
      key: "tools",
      labelKey: PAGE_LABEL_KEY.tools,
      // 写入后检查与校验：MCP / 技能拆走后本页只剩这四项。原 `<Divider>写入后检查</Divider>` 与页名
      // 重复（拆页后才出现的冗余），故去掉——页名已承担分段标题职责。
      body: draft && (
        <>
          <Form layout="vertical">
            <div className="hint" style={{ marginBottom: 10 }}>{t("settings.postWriteHint")}</div>
            {/* 开关：Switch 无宽度档，锚点挂整行 */}
            <div className={anchorCls("post_write_check.enabled")} data-setting-id="post_write_check.enabled">
              <Form.Item style={{ marginBottom: 8 }}>
                <Switch
                  checked={postWrite.enabled}
                  aria-label={t("settings.postWriteEnabled")}
                  onChange={(v) => patchPostWrite({ enabled: v })}
                />
                <span style={{ marginInlineStart: 8 }}>{t("settings.postWriteEnabled")}</span>
              </Form.Item>
            </div>
            {/* 命令：整行输入 + {file} 占位符与各技术栈示例说明 */}
            <div className={anchorCls("post_write_check.command")} data-setting-id="post_write_check.command">
              <Form.Item label={t("settings.postWriteCommand")} extra={t("settings.postWriteCommandHint")}>
                <Input
                  value={postWrite.command}
                  placeholder={t("settings.postWriteCommandPh")}
                  onChange={(e) => patchPostWrite({ command: e.target.value })}
                />
              </Form.Item>
            </div>
            {/* 超时 / 输出尾部字符：两项都是进阶项（窄档；行内网格类收回 app.css） */}
            <div className="postcheck-grid">
              <Form.Item label={t("settings.postWriteTimeout")}>
                <div className={anchorCls("post_write_check.timeout_seconds")} data-setting-id="post_write_check.timeout_seconds">
                  <InputNumber
                    size="small"
                    className="w-narrow"
                    min={1}
                    max={3600}
                    value={postWrite.timeout_seconds}
                    onChange={(v) => patchPostWrite({ timeout_seconds: v ?? DEFAULT_POST_WRITE_CHECK.timeout_seconds })}
                  />
                </div>
              </Form.Item>
              <Form.Item label={t("settings.postWriteTailChars")}>
                <div className={anchorCls("post_write_check.tail_chars")} data-setting-id="post_write_check.tail_chars">
                  <InputNumber
                    size="small"
                    className="w-narrow"
                    min={100}
                    max={100000}
                    step={100}
                    value={postWrite.tail_chars}
                    onChange={(v) => patchPostWrite({ tail_chars: v ?? DEFAULT_POST_WRITE_CHECK.tail_chars })}
                  />
                </div>
              </Form.Item>
            </div>
          </Form>
        </>
      ),
    },
    {
      key: "mcp",
      labelKey: PAGE_LABEL_KEY.mcp,
      // MCP 页（[docs/settings-ia](../../../../docs/settings-ia.md)）：上段「服务器状态」、下段「服务器配置」。
      // 状态段的数据源是既有的 mcp_status 命令 + mcp:status 事件（此前只写不读）；配置段不在 config 内
      // （独立 mcp.json），保存按钮走 mcp_save_config（页级「保存」不覆盖它）。
      body: draft && (
        <>
          {/* 服务器状态段：行由本页拼好（配置名单 ∪ 状态记录），展示与交互都在 McpStatusTable 里
              （两侧皆空时该组件自身返回 null）。 */}
          <McpStatusTable rows={mcpStatusRows} refreshing={mcpRefreshing} onRefresh={() => void refreshMcpStatus()} />
          <div className="settings-subhead">{t("settings.mcpConfigHead")}</div>
          {mcpEntries === null ? (
            // 兜底模式：原 JSON 无法解析时的保命通道；直接保存避免丢失
            <div className="mcp-pane setting-anchor" data-setting-id="mcp.servers">
              <div className="hint">{t("settings.mcpRawHint")}</div>
              <TextArea rows={14} value={mcpRaw} spellCheck={false} className="mcp-json" onChange={(e) => setMcpRaw(e.target.value)} />
              <div>
                <Button size="small" type="primary" onClick={() => void saveMcp()}>{t("settings.mcpSave")}</Button>
              </div>
            </div>
          ) : (
            <div className="mcp-pane setting-anchor" data-setting-id="mcp.servers">
              <div className="hint">{t("settings.mcpHint")}</div>
              {mcpEntries.map((e, idx) => (
                <div className="mcp-entry" key={idx}>
                  <div className="mcp-entry-head">
                    <Input
                      size="small"
                      className="w-narrow"
                      value={e.name}
                      placeholder={t("settings.mcpName")}
                      onChange={(ev) => patchMcpEntry(idx, { name: ev.target.value })}
                    />
                    <Select
                      size="small"
                      className="w-narrow"
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
          )}

        </>
      ),
    },
    {
      key: "skills",
      labelKey: PAGE_LABEL_KEY.skills,
      // 技能页：拆走后只剩禁用清单（反向清单：默认全启用，只列被你禁用的）。
      // 原 `<Divider>技能</Divider>` 与页名重复，故去掉；锚点落在既有容器上
      // （技能行：名称 / 来源 / 开关 / 删除）。
      body: draft && (
        <>
          <div className="setting-anchor" data-setting-id="disabled_skills">
            <div className="skills-toolbar">
              <div className="hint">{t("settings.skillsHint")}</div>
              <Button size="small" loading={skillsBusy} onClick={() => void reloadSkills()}>
                {t("settings.reloadSkills")}
              </Button>
            </div>
            {skills.length === 0 && <Empty description={t("settings.skillsEmpty")} style={{ marginTop: 24 }} />}
            {skills.map((s) => {
              const builtin = s.origin === "<builtin>";
              const label = originLabel(s.origin);
              return (
                <div className="skill-row" key={s.name}>
                  <div className="skill-info">
                    <b>{s.name}</b>
                    {builtin ? (
                      <span className="skill-origin">{t("common.builtin")}</span>
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
        </>
      ),
    },
    {
      key: "agent",
      labelKey: PAGE_LABEL_KEY.agent,
      body: draft && (
        <Form layout="vertical">
          <Form.Item label={t("settings.shell")} extra={t("settings.shellHint")}>
            {/* 锚点挂在既有行容器上（shell.selection：选择器 + 路径回显是同一行） */}
            <div className="setting-anchor" data-setting-id="shell.selection" style={{ display: "flex", alignItems: "center", gap: 8, width: "100%", minWidth: 0 }}>
              <Select
                size="small"
                className="w-mid"
                style={{ flexShrink: 0 }}
                value={draft.shell?.selection ?? "auto"}
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
                const display = resolveShellDisplay(draft.shell?.selection ?? null, shells);
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
            {draft.shell?.selection && shells !== null && !shells.some((s) => s.id === draft.shell!.selection) && (
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
            {/* TextArea 整行（不参与宽度三档），只补锚点 */}
            <div className="setting-anchor" data-setting-id="custom_prompt">
              <TextArea
                rows={4}
                value={draft.custom_prompt ?? ""}
                // 空值归一：清空写 null（而非 ""），与 ai_language 同口径；后端也是按 trim 后非空才注入
                onChange={(e) => {
                  const v = e.target.value;
                  patchDraft({ custom_prompt: v.trim() === "" ? null : v });
                }}
              />
            </div>
          </Form.Item>
          <Form.Item label={t("settings.compactThreshold")}>
            {/* Slider 是整行控件（不参与宽度三档）：批③ 去掉内联 320 像素宽，宽度随容器 */}
            <div className="setting-anchor" data-setting-id="compact_threshold">
              <Slider
                min={0.1}
                max={0.9}
                step={0.05}
                value={draft.compact_threshold ?? 0.6}
                onChange={(v) => patchDraft({ compact_threshold: v })}
              />
            </div>
          </Form.Item>
          <Form.Item label={t("settings.compactTimeout")}>
            <div className="setting-anchor" data-setting-id="compact_timeout_seconds">
              <InputNumber
                className="w-narrow"
                min={30}
                max={3600}
                step={30}
                value={draft.compact_timeout_seconds ?? 180}
                onChange={(v) => patchDraft({ compact_timeout_seconds: v ?? 180 })}
              />
            </div>
          </Form.Item>
          {/* 会话保留期与清理（[docs/session-cleanup](../../../../docs/session-cleanup.md) §3 第 18/25/26 条）：
              下拉走页级保存（不是即时生效项）；动作按钮与只读状态行都不落盘（app.* 无配置字段） */}
          <Form.Item label={t("settings.sessionRetention")} extra={t("settings.sessionRetentionHint")}>
            <div className="setting-anchor" data-setting-id="sessions.retention_days">
              <Select
                size="small"
                className="w-narrow"
                value={draftRetention === null ? RETENTION_NEVER : String(draftRetention)}
                onChange={patchRetention}
                options={RETENTION_OPTIONS.map((days) => ({
                  label: days === null ? t("settings.cleanupNever") : t("settings.cleanupDays", { n: days }),
                  value: days === null ? RETENTION_NEVER : String(days),
                }))}
              />
            </div>
          </Form.Item>
          <Form.Item label={t("settings.cleanupNow")}>
            {/* 禁用原因写在按钮右侧而不是 Tooltip：两条禁用条件（未选保留期 / 未保存）都能一眼看到 */}
            <div
              className="setting-anchor"
              data-setting-id="app.cleanup_now"
              style={{ display: "flex", alignItems: "center", gap: 8, minWidth: 0 }}
            >
              <Button size="small" loading={cleaning} disabled={cleanupBlock !== null} onClick={() => void runCleanupNow()}>
                {t("settings.cleanupNow")}
              </Button>
              {cleanupBlock !== null && (
                <Typography.Text type="secondary" style={{ fontSize: 12 }}>
                  {cleanupBlock === "none" ? t("settings.cleanupNeedRetention") : t("settings.cleanupUnsavedFirst")}
                </Typography.Text>
              )}
            </div>
          </Form.Item>
          <Form.Item label={t("settings.cleanupStatus")}>
            {/* 只读信息项（数据源 = get_cleanup_status）：**常驻渲染**，不因无记录而整行消失 */}
            <div className="setting-anchor" data-setting-id="app.cleanup_status">
              <span className="dim" style={{ fontSize: 12.5 }}>
                {cleanupStatusText()}
              </span>
            </div>
          </Form.Item>
        </Form>
      ),
    },
    {
      key: "logs",
      labelKey: PAGE_LABEL_KEY.logs,
      body: draft && (
        <Form layout="vertical">
          <Form.Item label={t("settings.logLevel")} extra={t("settings.logLevelHint")}>
            <div className="setting-anchor" data-setting-id="log.level">
              <Select
                size="small"
                className="w-narrow"
                value={draft.log?.level ?? "info"}
                onChange={(v) => patchDraft({ log: { ...draft.log, level: v } })}
                options={["trace", "debug", "info", "warn", "error"].map((v) => ({ label: v, value: v }))}
              />
            </div>
          </Form.Item>
          {/* session_verbose 是进阶项：**整个 Form.Item** 包进锚点容器再加类隐藏（行留在原分组内）。
              不能在 Form.Item 内部加类：antd 的 label 与 control 是兄弟节点，只藏 control 会留下
              孤立标签 + 空控制行（与 approval.command_allowlist 同形） */}
          <div className={anchorCls("log.session_verbose")} data-setting-id="log.session_verbose">
            <Form.Item label={t("settings.sessionVerbose")} extra={t("settings.sessionVerboseHint")}>
              <Switch
                size="small"
                checked={draft.log?.session_verbose ?? false}
                onChange={(v) => patchDraft({ log: { ...draft.log, session_verbose: v } })}
              />
            </Form.Item>
          </div>
        </Form>
      ),
    },
    {
      key: "about",
      labelKey: PAGE_LABEL_KEY.about,
      // 关于页不需要 draft：身份信息只读，自动更新开关走 localStorage（即时生效）
      body: <AboutSettings />,
    },
  ];

  const activePage = pages.find((p) => p.key === tab) ?? pages[0];
  // 窄窗导航列宽：基准 280，随窗口宽度收缩，由 clampNavWidth 夹在 180..480 内
  const navWidth = clampNavWidth(Math.min(SETTINGS_NAV_W, Math.round(windowWidth * SETTINGS_NAV_RATIO)));

  /**
   * 导航方向键：↑/↓（兼认 ←/→）在页行之间移动**焦点**，Enter/Space 才激活（手动激活模式）——
   * 不动 tabIndex、不引入 roving tabindex，因此 Tab 键可达性与批① 的「打开即聚焦返回按钮」都不变；
   * 焦点不在页行上（如「返回工作区」/运行中指示）时让位给浏览器默认行为。
   */
  function onNavArrow(e: ReactKeyboardEvent<HTMLDivElement>) {
    const delta = e.key === "ArrowDown" || e.key === "ArrowRight" ? 1 : e.key === "ArrowUp" || e.key === "ArrowLeft" ? -1 : 0;
    if (delta === 0) return;
    const items = Array.from(e.currentTarget.querySelectorAll<HTMLButtonElement>(".settings-nav-item"));
    const idx = items.indexOf(document.activeElement as HTMLButtonElement);
    if (idx < 0) return;
    const next = items[idx + delta];
    if (!next) return; // 首尾不循环：停在两端
    e.preventDefault();
    next.focus();
  }

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
      {/* 左导航列：返回工作区 + 运行中指示 + 三组 10 页导航。
          导航自建（批② 起替掉 antd Tabs）：Tabs 无法承载「组标题 + 页行」两列式布局，
          且其 pane 机制与本页「页体渲染在右列」的布局要求相冲。 */}
      <nav className="settings-nav" style={{ width: navWidth }}>
        <div className="settings-nav-head">
          {/* 专用类名 .settings-nav-back：打开设置时的初始焦点靠它定位，不泛选 button——
              搜索框（antd Input + allowClear）值非空时会渲染一个清除 button，泛选会把焦点抢过去。
              防御性写法：搜索框挂载时值恒为空、清除按钮不存在，故该保护当前不可构造验证 */}
          <Button
            type="text"
            size="small"
            className="settings-nav-back"
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
          {/* 搜索框：放在「返回工作区」**下方**、整行（.settings-search 靠 flex-basis:100%
              在 .settings-nav-head 里独占一行，不与运行中指示挤同一行） */}
          <div className="settings-search">
            {/* combobox 语义挂在**输入框**上（aria-activedescendant 只有焦点元素会播报，挂无焦点的
                listbox 上读屏不念）；结果列表只在搜索态存在，故 aria-expanded 直接跟 searching 走 */}
            <Input
              size="small"
              allowClear
              role="combobox"
              aria-label={t("settings.searchPlaceholder")}
              aria-expanded={searching}
              aria-controls={searching ? SEARCH_LISTBOX_ID : undefined}
              aria-activedescendant={activeHitId ? `settings-search-opt-${activeHitId}` : undefined}
              placeholder={t("settings.searchPlaceholder")}
              value={query}
              onChange={(e) => {
                setQuery(e.target.value);
                setHitIdx(-1); // 查询一变就清掉高亮：不默认选中、不自动跳转
              }}
              onKeyDown={onSearchKeyDown}
            />
          </div>
        </div>
        {/* 搜索态与导航态**互斥**：搜索时不渲染 tablist（结果行用独立类名），因此 ↑/↓ 不可能在
            两套列表之间串味；清空查询即恢复常规导航（结果里无默认选中项） */}
        {searching ? (
          <div
            id={SEARCH_LISTBOX_ID}
            className="settings-search-results"
            role="listbox"
            aria-label={t("settings.searchResults")}
            /* 列表不是焦点目标（tabIndex=-1）：方向键与 Enter 都由搜索框接管 */
            tabIndex={-1}
          >
            {results.length === 0 ? (
              <div className="settings-search-empty">
                <div className="settings-search-empty-title">{t("settings.searchEmpty")}</div>
                <div className="hint">{t("settings.searchEmptyHint")}</div>
              </div>
            ) : (
              results.map((item, i) => (
                /* 行属性用独立名 data-search-hit：data-setting-id 是页内锚点（定位用），两者不能混 */
                <div
                  key={item.id}
                  id={`settings-search-opt-${item.id}`}
                  role="option"
                  aria-selected={i === hitIdx}
                  data-search-hit={item.id}
                  className={`settings-search-item${i === hitIdx ? " settings-search-item-active" : ""}`}
                  onMouseEnter={() => setHitIdx(i)}
                  onClick={() => jumpToItem(item)}
                >
                  <span className="settings-search-item-label">{t(item.labelKey)}</span>
                  {/* 次标：所属页名（弱化） */}
                  <span className="settings-search-item-page">{t(PAGE_LABEL_KEY[item.page])}</span>
                </div>
              ))
            )}
          </div>
        ) : (
          <div className="settings-nav-list" role="tablist" aria-orientation="vertical" onKeyDown={onNavArrow}>
            {PAGE_GROUPS.map((group) => (
              <Fragment key={group.titleKey}>
                {/* 组标题不是 tab：标 presentation，避免 tablist 的直接子节点混入非 tab 语义 */}
                <div className="settings-nav-group" role="presentation">{t(group.titleKey)}</div>
                {group.pages.map((key) => {
                  const Icon = PAGE_ICON[key];
                  return (
                    <button
                      key={key}
                      type="button"
                      role="tab"
                      id={`settings-tab-${key}`}
                      aria-selected={key === tab}
                      aria-controls="settings-panel"
                      data-page={key}
                      className={`settings-nav-item${key === tab ? " settings-nav-item-active" : ""}`}
                      onClick={() => onTabChange(key)}
                    >
                      <span className="settings-nav-label">
                        {/* 图标不设 color：跟随 .settings-nav-label 的文本色 */}
                        <Icon className="settings-nav-icon" />
                        {t(PAGE_LABEL_KEY[key])}
                        {dirtyMap[key] && <span className="settings-dirty-dot" title={t("settings.dirtyHint")} />}
                      </span>
                    </button>
                  );
                })}
              </Fragment>
            ))}
          </div>
        )}
      </nav>

      <div className="settings-content">
        <div className="settings-actions">
          <span className="settings-actions-title">
            {t("settings.title")} · {activePage ? t(activePage.labelKey) : ""}
          </span>
          {anyDirty && <span className="settings-dirty-dot" title={t("settings.dirtyHint")} />}
          <div className="settings-actions-buttons">
            <Tooltip title={t("settings.cancelHint")}>
              <Button onClick={cancelAll}>{t("common.cancel")}</Button>
            </Tooltip>
            <Button type="primary" loading={saving} onClick={() => void save()}>
              {t("common.save")}
            </Button>
          </div>
        </div>
        <div className="settings-pane">
          {/* 页体容器与导航 tab 配对（aria-controls ← → aria-labelledby 闭环；同一时刻只渲染一页） */}
          <div
            className="settings-pane-body"
            id="settings-panel"
            role="tabpanel"
            aria-labelledby={`settings-tab-${tab}`}
          >
            {/* 页级进阶开关（该页进阶项数为 0 时不渲染）：开关反映**手动偏好**（搜索临时展开
                不改开关状态），切换只写 localStorage、不碰 draft → 不产生未保存改动 */}
            {advancedCount > 0 && (
              <div className="settings-advanced-toggle">
                <Switch
                  size="small"
                  checked={showAdvanced}
                  onChange={toggleAdvanced}
                  aria-label={t("settings.showAdvanced", { n: advancedCount })}
                />
                <span className="settings-advanced-label">{t("settings.showAdvanced", { n: advancedCount })}</span>
                <span className="hint">{t("settings.advancedHint")}</span>
              </div>
            )}
            {activePage ? activePage.body : null}
          </div>
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
