// 周期选择器 ↔ 计划表达式（[docs/tasks-module-polish]）：文法镜像的纯函数守门。
// 后端 `parse_schedule` 只认三种形态：`cron:<5 字段>`（内部前置补秒位）/ `every:<n> <unit>`（**空白是文法的一部分**）
// / `once:<RFC3339>`。本文件把「六种 preset ↔ 表达式」的往返与「认不出 → custom」钉死。
import { describe, it, expect } from "vitest";
import {
  EVERY_LIMITS, describeExpr, exprToPreset, formatDateTimeLocal, isValidTime, parseDateTimeLocal, presetToExpr,
  toLocalRfc3339, type SchedulePreset,
} from "../features/panels/schedulePreset";
import { i18n } from "../i18n"; // 描述文案已改为注入 t：测试直接用 i18next 实例的取词函数

/** 一次性任务的构造：秒归零（toLocalRfc3339 保留秒，测试才能逐字比对往返） */
function at(y: number, mo: number, d: number, h: number, mi: number): Date {
  return new Date(y, mo - 1, d, h, mi, 0, 0);
}

describe("presetToExpr：六种 preset → 表达式", () => {
  it("每天 / 每周 / 每月 → cron 的 5 字段形态（分 时 日 月 周）", () => {
    expect(presetToExpr({ kind: "daily", time: "09:00" })).toBe("cron:0 9 * * *");
    expect(presetToExpr({ kind: "weekly", dows: [1, 3, 5], time: "09:00" })).toBe("cron:0 9 * * 1,3,5");
    expect(presetToExpr({ kind: "monthly", day: 1, time: "09:00" })).toBe("cron:0 9 1 * *");
    // 非零分与时：分在前、时在后（与后端文法一致，别写成 cron:<时> <分>）
    expect(presetToExpr({ kind: "daily", time: "23:45" })).toBe("cron:45 23 * * *");
  });

  it("每周：星期列表去重、升序、7 归一为 0（cron 里周日既可是 0 也可是 7）", () => {
    expect(presetToExpr({ kind: "weekly", dows: [5, 1, 1], time: "08:05" })).toBe("cron:5 8 * * 1,5");
    expect(presetToExpr({ kind: "weekly", dows: [7, 0], time: "08:05" })).toBe("cron:5 8 * * 0");
    // 全选 7 天与「每天」等价（表达式仍是显式列表，编辑时不再丢用户的选择）
    expect(presetToExpr({ kind: "weekly", dows: [0, 1, 2, 3, 4, 5, 6], time: "09:00" }))
      .toBe("cron:0 9 * * 0,1,2,3,4,5,6");
  });

  it("每隔 → `every:<n> <unit>`（空白必须存在：`every:30m` 是后端明文拒收的形态）", () => {
    expect(presetToExpr({ kind: "every", n: 30, unit: "m" })).toBe("every:30 m");
    expect(presetToExpr({ kind: "every", n: 6, unit: "h" })).toBe("every:6 h");
    expect(presetToExpr({ kind: "every", n: 1, unit: "d" })).toBe("every:1 d");
  });

  it("一次 → `once:<本地时区 RFC3339>`（保留钟点意图，不偏移成 UTC）", () => {
    const t = at(2026, 10, 1, 9, 0);
    expect(presetToExpr({ kind: "once", at: t })).toBe(`once:${toLocalRfc3339(t)}`);
    // 秒带零、偏移形式是 ±HH:MM（不是 Z：本地时区的写法才是用户看到的时间）
    expect(toLocalRfc3339(t)).toMatch(/^2026-10-01T09:00:00[+-]\d{2}:\d{2}$/);
  });

  it("自定义：原样透传（手写表达式的能力不回退）", () => {
    expect(presetToExpr({ kind: "custom", expr: "cron:*/5 * * * *" })).toBe("cron:*/5 * * * *");
    expect(presetToExpr({ kind: "custom", expr: "every:2 h" })).toBe("every:2 h");
  });

  it("时间/星期/日期非法时返回空串（不做静默兜底——保存按钮据此禁用）", () => {
    expect(presetToExpr({ kind: "daily", time: "" })).toBe("");
    expect(presetToExpr({ kind: "daily", time: "24:00" })).toBe("");
    expect(presetToExpr({ kind: "weekly", dows: [], time: "09:00" })).toBe("");
    expect(presetToExpr({ kind: "monthly", day: 0, time: "09:00" })).toBe("");
    expect(presetToExpr({ kind: "monthly", day: 32, time: "09:00" })).toBe("");
  });
});

describe("exprToPreset：反解与往返", () => {
  const round = (p: SchedulePreset): SchedulePreset => exprToPreset(presetToExpr(p));

  it("六种 preset 经「生成 → 反解」回到原状", () => {
    expect(round({ kind: "daily", time: "09:00" })).toEqual({ kind: "daily", time: "09:00" });
    expect(round({ kind: "weekly", dows: [1, 3, 5], time: "09:00" })).toEqual({ kind: "weekly", dows: [1, 3, 5], time: "09:00" });
    expect(round({ kind: "monthly", day: 15, time: "07:30" })).toEqual({ kind: "monthly", day: 15, time: "07:30" });
    expect(round({ kind: "every", n: 30, unit: "m" })).toEqual({ kind: "every", n: 30, unit: "m" });
    const once = round({ kind: "once", at: at(2026, 10, 1, 9, 0) });
    expect(once.kind).toBe("once");
    if (once.kind === "once") expect(once.at.getTime()).toBe(at(2026, 10, 1, 9, 0).getTime());
    expect(round({ kind: "custom", expr: "cron:*/5 * * * *" })).toEqual({ kind: "custom", expr: "cron:*/5 * * * *" });
  });

  it("已知的三种形态各自解到对应 preset", () => {
    expect(exprToPreset("cron:0 9 * * *")).toEqual({ kind: "daily", time: "09:00" });
    expect(exprToPreset("cron:0 9 * * 1,3,5")).toEqual({ kind: "weekly", dows: [1, 3, 5], time: "09:00" });
    expect(exprToPreset("cron:0 9 1 * *")).toEqual({ kind: "monthly", day: 1, time: "09:00" });
    expect(exprToPreset("every:30 m")).toEqual({ kind: "every", n: 30, unit: "m" });
    // 周日的 7 与 0 等价：统一收敛到 0
    expect(exprToPreset("cron:0 9 * * 7")).toEqual({ kind: "weekly", dows: [0], time: "09:00" });
    // 两侧空白容忍（用户从文档里复制粘贴常带尾空格）
    expect(exprToPreset("  cron:0 9 * * *  ")).toEqual({ kind: "daily", time: "09:00" });
  });

  it("认不出的表达式一律 custom（原样保留，绝不臆造语义）", () => {
    for (const expr of [
      "cron:0 9 * * 1-5", // 范围：不在选择器的表现力内
      "cron:0 9 * * */2", // 步进
      "cron:0 9 1 5 *", // 指定月份
      "every:30m", // 缺空白的非法形态（后端断言其非法）
      "every:0 m", // n > 0
      `every:${EVERY_LIMITS.d + 1} d`, // 超上限（30 天）
      `every:${EVERY_LIMITS.m + 1} m`,
      "0 0 9 * * *", // 6 字段（含秒）——带秒不是本选择器生成的形态
      "once:明天早上九点",
      "once:2026-10-01T09:00:00", // 缺偏移
      "junk",
      "",
    ]) {
      expect(exprToPreset(expr), `${expr} 应落 custom`).toEqual({ kind: "custom", expr: expr.trim() });
    }
  });

  it("每月日期越界（cron 允许 0/32 之类的方言值）也落 custom", () => {
    expect(exprToPreset("cron:0 9 0 * *")).toEqual({ kind: "custom", expr: "cron:0 9 0 * *" });
    expect(exprToPreset("cron:0 9 32 * *")).toEqual({ kind: "custom", expr: "cron:0 9 32 * *" });
  });
});

describe("describeExpr：列表行的人话描述", () => {
  /** 描述文案走 i18n（注入 t）：这里传 i18next 实例的取词函数，与组件里的 react-i18next t 同源 */
  const tt = (key: string, opts?: Record<string, unknown>) => i18n.t(key, opts) as string;

  it("五种已知形态 + 全周归并 + 认不出原样", () => {
    expect(describeExpr("cron:0 9 * * *", tt)).toBe("每天 09:00");
    expect(describeExpr("cron:0 9 * * 1,3,5", tt)).toBe("每周一、三、五 09:00");
    expect(describeExpr("cron:0 9 * * 0", tt)).toBe("每周日 09:00");
    expect(describeExpr("cron:0 9 1 * *", tt)).toBe("每月 1 日 09:00");
    expect(describeExpr("every:30 m", tt)).toBe("每 30 分钟");
    expect(describeExpr("every:2 h", tt)).toBe("每 2 小时");
    expect(describeExpr("every:1 d", tt)).toBe("每 1 天");
    // 全选 7 天 = 每天（读起来不必数七个数）
    expect(describeExpr("cron:0 9 * * 0,1,2,3,4,5,6", tt)).toBe("每天 09:00");
    // 认不出的表达式原样显示（用户至少知道自己写了什么）
    expect(describeExpr("cron:0 9 * * 1-5", tt)).toBe("cron:0 9 * * 1-5");
    expect(describeExpr("junk", tt)).toBe("junk");
  });

  it("一次性任务：本地时刻（与 toLocalRfc3339 同一时区，不偏移）", () => {
    expect(describeExpr("once:2026-10-01T09:00:00+08:00", tt)).toMatch(/^仅一次 2026-10-01 \d{2}:\d{2}$/);
    const at9 = at(2026, 10, 1, 9, 0);
    expect(describeExpr(`once:${toLocalRfc3339(at9)}`, tt)).toBe("仅一次 2026-10-01 09:00");
  });
});

describe("datetime-local 输入串（一次性的编辑态）", () => {
  it("往返：format → parse 回到同一时刻（都在本地时区，语义不偏移）", () => {
    const t = at(2026, 10, 1, 9, 0);
    expect(formatDateTimeLocal(t)).toMatch(/^2026-10-01T09:00$/);
    expect(parseDateTimeLocal(formatDateTimeLocal(t))?.getTime()).toBe(t.getTime());
  });

  it("非法输入返回 null（空 / 半截 / 不存在的日期）", () => {
    for (const bad of ["", "2026-10-01", "2026-10-01T09", "2026-13-01T09:00", "2026-10-45T09:00", "2026-10-01T25:00"]) {
      expect(parseDateTimeLocal(bad), `${bad} 应判非法`).toBeNull();
    }
    expect(parseDateTimeLocal(" 2026-10-01T09:00 ")?.getTime()).toBe(at(2026, 10, 1, 9, 0).getTime());
  });

  it("时间串校验：HH:mm 合法，越界与空非法", () => {
    expect(isValidTime("09:00")).toBe(true);
    expect(isValidTime("9:0")).toBe(true);
    expect(isValidTime("24:00")).toBe(false);
    expect(isValidTime("09:60")).toBe(false);
    expect(isValidTime("")).toBe(false);
  });
});
