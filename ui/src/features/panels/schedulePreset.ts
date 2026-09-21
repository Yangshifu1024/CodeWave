// 周期选择器 ↔ 计划表达式（[docs/tasks-module-polish]）：界面不再让用户手写 cron。
// 文法由后端 `parse_schedule` 定死，本文件是它的**前端镜像**（只读语义，不改后端）：
//   cron:<分> <时> <日> <月> <周>   5 字段（后端内部前置补秒位）；周 0/7 = 周日，支持 `1,3,5` 列表
//   every:<n> <m|h|d>             **必须带空白**（`every:30m` 非法）；n ≥ 1；上限 30 天
//   once:<RFC3339>                本地时区
// 认不出的表达式一律落回 custom（原样透传，绝不臆造语义）——手写表达式的能力因此不回退。
//
// 描述文案（describeExpr）走 i18n：t 由调用方注入（组件传 react-i18next 的 t，测试传 i18next 实例的 t）——
// 早先它把中文写死在纯函数里，英文界面会露中文（本批修正）。

/** 「每隔」的单位（分钟 / 小时 / 天） */
export type EveryUnit = "m" | "h" | "d";

/** 周期选择的六种形态（custom = 原表达式透传） */
export type SchedulePreset =
  | { kind: "daily"; time: string }
  | { kind: "weekly"; dows: number[]; time: string }
  | { kind: "monthly"; day: number; time: string }
  | { kind: "every"; n: number; unit: EveryUnit }
  | { kind: "once"; at: Date }
  | { kind: "custom"; expr: string };

/** 「每隔」的上限（与后端一致）：分钟 ≤ 43200（30 天）、小时 ≤ 720、天 ≤ 30 */
export const EVERY_LIMITS: Record<EveryUnit, number> = { m: 43200, h: 720, d: 30 };

/** 星期显示顺序：一~六 在前、周日垫尾（值与 cron 的 0/7 = 周日一致） */
export const WEEKDAY_ORDER = [1, 2, 3, 4, 5, 6, 0] as const;

/** cron 的「秒」位之外的分、时字段（1~2 位数字，不做零填充要求——`09` 与 `9` 等价） */
const NUM_RE = /^\d{1,2}$/;
/** 周字段的纯列表形态（`1,3,5`）；`1-5` / `*` 等一律不认（落 custom） */
const DOW_LIST_RE = /^[0-7](,[0-7])*$/;
/** every 的从宽形态：**空白是文法的一部分**，`every:30m` 必须落 custom */
const EVERY_RE = /^every:(\d+)\s+([mhd])$/;
/** once 的载荷必须是 RFC3339（带 Z 或 ±HH:MM 偏移），否则落 custom */
const ONCE_RE = /^once:(\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}(?:\.\d+)?(?:Z|[+-]\d{2}:\d{2}))$/;

function pad2(n: number): string {
  return String(n).padStart(2, "0");
}

/** "HH:mm"（也容忍 "H:m"）→ { h, m }；非法返回 null */
export function parseTime(time: string): { h: number; m: number } | null {
  const m = /^(\d{1,2}):(\d{1,2})$/.exec((time ?? "").trim());
  if (!m) return null;
  const h = Number(m[1]);
  const min = Number(m[2]);
  if (h > 23 || min > 59) return null;
  return { h, m: min };
}

/** 时间串是否可用（界面用它决定「保存」是否可点） */
export function isValidTime(time: string): boolean {
  return parseTime(time) !== null;
}

/** 星期列表归一：去重、7→0、升序（cron 不看顺序，但表达式要稳定可比对） */
export function normalizeDows(dows: number[]): number[] {
  const set = new Set<number>();
  for (const d of dows) {
    if (!Number.isInteger(d)) continue;
    const v = d === 7 ? 0 : d;
    if (v >= 0 && v <= 6) set.add(v);
  }
  return [...set].sort((a, b) => a - b);
}

/** Date → 本地时区 RFC3339（`2026-10-01T09:00:00+08:00`）：保留用户的钟点意图，后端 parse_from_rfc3339 直接收 */
export function toLocalRfc3339(d: Date): string {
  const off = -d.getTimezoneOffset(); // 分钟，东区为正
  const sign = off < 0 ? "-" : "+";
  const abs = Math.abs(off);
  return (
    `${d.getFullYear()}-${pad2(d.getMonth() + 1)}-${pad2(d.getDate())}` +
    `T${pad2(d.getHours())}:${pad2(d.getMinutes())}:${pad2(d.getSeconds())}` +
    `${sign}${pad2(Math.floor(abs / 60))}:${pad2(abs % 60)}`
  );
}

/** Date → 本地 `YYYY-MM-DD HH:mm`（列表行的「下次触发」与一次性任务的描述共用） */
export function formatLocalDateTime(d: Date): string {
  return (
    `${d.getFullYear()}-${pad2(d.getMonth() + 1)}-${pad2(d.getDate())}` +
    ` ${pad2(d.getHours())}:${pad2(d.getMinutes())}`
  );
}

/** `<input type="datetime-local">` 的值（本地，无秒无偏移）→ Date；空/非法返回 null */
export function parseDateTimeLocal(value: string): Date | null {
  const m = /^(\d{4})-(\d{2})-(\d{2})T(\d{2}):(\d{2})(?::(\d{2}))?$/.exec((value ?? "").trim());
  if (!m) return null;
  // 逐段构造（不交给 new Date 解析字符串）：入参与出参都在本地时区，语义不偏移
  const d = new Date(
    Number(m[1]), Number(m[2]) - 1, Number(m[3]),
    Number(m[4]), Number(m[5]), Number(m[6] ?? 0), 0,
  );
  // 反向校验：`2026-13-45T99:99` 这类会被 Date 吞成邻近日期的输入必须判非法
  if (
    d.getFullYear() !== Number(m[1]) || d.getMonth() !== Number(m[2]) - 1 || d.getDate() !== Number(m[3]) ||
    d.getHours() !== Number(m[4]) || d.getMinutes() !== Number(m[5])
  ) return null;
  return d;
}

/** Date → `<input type="datetime-local">` 的值（本地，分钟精度） */
export function formatDateTimeLocal(d: Date): string {
  return (
    `${d.getFullYear()}-${pad2(d.getMonth() + 1)}-${pad2(d.getDate())}` +
    `T${pad2(d.getHours())}:${pad2(d.getMinutes())}`
  );
}

/** 时间串是否全周（一~日 7 天都在：等价于「每天」，描述与展示按每天走） */
export function isFullWeek(dows: number[]): boolean {
  return normalizeDows(dows).length === 7;
}

/** 周期 → 计划表达式。custom 原样返回；时间非法时返回 ""（调用方据此禁用保存，不做静默兜底） */
export function presetToExpr(p: SchedulePreset): string {
  switch (p.kind) {
    case "daily": {
      const t = parseTime(p.time);
      return t ? `cron:${t.m} ${t.h} * * *` : "";
    }
    case "weekly": {
      const t = parseTime(p.time);
      const dows = normalizeDows(p.dows);
      if (!t || dows.length === 0) return "";
      return `cron:${t.m} ${t.h} * * ${dows.join(",")}`;
    }
    case "monthly": {
      const t = parseTime(p.time);
      if (!t || !Number.isInteger(p.day) || p.day < 1 || p.day > 31) return "";
      return `cron:${t.m} ${t.h} ${p.day} * *`;
    }
    case "every":
      return `every:${p.n} ${p.unit}`;
    case "once":
      return `once:${toLocalRfc3339(p.at)}`;
    case "custom":
      return p.expr;
  }
}

/**
 * 计划表达式 → 周期。只认**我们自己生成的形态**（见文件头文法），其余一律 custom：
 *  - 范围与步进（如 `1-5`、星号后跟 `/5` 那种写法）、6 字段（含秒）的 cron、`every:30m` 无空白形态都落 custom；
 *  - 每月日期 / 每隔数值越界时也落 custom——选不出来就不假装能编辑它。
 */
export function exprToPreset(expr: string): SchedulePreset {
  const raw = (expr ?? "").trim();
  const custom: SchedulePreset = { kind: "custom", expr: raw };

  if (raw.startsWith("cron:")) {
    const fields = raw.slice("cron:".length).trim().split(/\s+/);
    if (fields.length !== 5) return custom;
    const [minS, hourS, domS, monS, dowS] = fields;
    if (!NUM_RE.test(minS) || !NUM_RE.test(hourS)) return custom;
    const min = Number(minS);
    const hour = Number(hourS);
    if (min > 59 || hour > 23) return custom;
    if (monS !== "*") return custom; // 月份/步进不进选择器
    const time = `${pad2(hour)}:${pad2(min)}`;
    if (domS === "*" && dowS === "*") return { kind: "daily", time };
    if (domS === "*" && DOW_LIST_RE.test(dowS)) {
      const dows = normalizeDows(dowS.split(",").map(Number));
      return dows.length > 0 ? { kind: "weekly", dows, time } : custom;
    }
    if (NUM_RE.test(domS) && dowS === "*") {
      const day = Number(domS);
      return day >= 1 && day <= 31 ? { kind: "monthly", day, time } : custom;
    }
    return custom;
  }

  const every = EVERY_RE.exec(raw);
  if (every) {
    const n = Number(every[1]);
    const unit = every[2] as EveryUnit;
    if (n < 1 || n > EVERY_LIMITS[unit]) return custom;
    return { kind: "every", n, unit };
  }

  const once = ONCE_RE.exec(raw);
  if (once) {
    const at = new Date(once[1]);
    return Number.isNaN(at.getTime()) ? custom : { kind: "once", at };
  }

  return custom;
}

/** 星期值 → i18n 键（0/7 = 周日，与 WEEKDAY_ORDER 同口径） */
const WEEKDAY_KEY: Record<number, string> = {
  0: "tasks.weekdaySun", 1: "tasks.weekdayMon", 2: "tasks.weekdayTue", 3: "tasks.weekdayWed",
  4: "tasks.weekdayThu", 5: "tasks.weekdayFri", 6: "tasks.weekdaySat", 7: "tasks.weekdaySun",
};

/** 「每隔」单位 → i18n 键 */
const UNIT_KEY: Record<EveryUnit, string> = {
  m: "tasks.intervalUnitMin", h: "tasks.intervalUnitHour", d: "tasks.intervalUnitDay",
};

/** 表达式 → 人类可读描述（列表行用；文案走 i18n，认不出的表达式原样返回） */
export function describeExpr(expr: string, t: (key: string, opts?: Record<string, unknown>) => string): string {
  const p = exprToPreset(expr);
  switch (p.kind) {
    case "daily":
      return t("tasks.descDaily", { time: p.time });
    case "weekly":
      // 全选 7 天与「每天」等价：描述按每天走（表达式保持 weekly 形态，编辑时也不再丢用户的选择）
      return isFullWeek(p.dows)
        ? t("tasks.descDaily", { time: p.time })
        : t("tasks.descWeekly", {
            days: p.dows.map((d) => t(WEEKDAY_KEY[d] ?? "tasks.weekdaySun")).join(t("tasks.weekdaySep")),
            time: p.time,
          });
    case "monthly":
      return t("tasks.descMonthly", { day: p.day, time: p.time });
    case "every":
      return t("tasks.descEvery", { n: p.n, unit: t(UNIT_KEY[p.unit]) });
    case "once":
      return t("tasks.descOnce", { at: formatLocalDateTime(p.at) });
    case "custom":
      return p.expr;
  }
}
