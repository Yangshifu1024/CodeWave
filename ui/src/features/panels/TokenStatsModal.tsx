import { useEffect, useMemo, useState } from "react";
import { Empty, Modal } from "antd";
import { useTranslation } from "react-i18next";
import { ipc } from "../../ipc/client";
import type { DailyStats, ModelAgg } from "../../ipc/types";
import { useSettings } from "../../stores/settings";
import { useUi } from "../../stores/ui";
import { cacheDenominator, cacheSemanticsOfFormat, type CacheSemantics } from "../../utils/models";

/** token 数缩写：M/k 分级缩写，便于柱状图标签与摘要展示。 */
function fmt(n: number): string {
  if (n >= 1_000_000) return `${(n / 1_000_000).toFixed(1)}M`;
  if (n >= 1_000) return `${(n / 1_000).toFixed(1)}k`;
  return String(n);
}

/**
 * 计费语义（[docs/prompt-caching-hardening]）：anthropic_messages 的 usage.input 不含缓存部分
 * （真输入 = input + cache_read + cache_write）；openai_chat / openai_responses 的 input 已包含
 * cached_tokens（cache_read 是 input 的子集）。两套口径下「总量 / 命中率」公式不同，必须按协议区分。
 *
 * 公式本身**不在本文件定义**：语义判定与命中率分母来自 `utils/models.ts`
 * （`cacheSemanticsOfFormat` / `cacheDenominator`），与 Composer 工具条的命中率同源——
 * 两处各写一份必然漂移。这里只保留 stats 侧特有的「总量」口径。
 */

/** 计费 token 总量：anthropic 四维相加；openai 的 input 已含缓存命中，只加 output。 */
function trueTotal(a: ModelAgg, sem: CacheSemantics): number {
  return sem === "anthropic"
    ? a.input + a.output + a.cache_read + a.cache_write
    : a.input + a.output;
}

/** 任务/统计弹窗：近 30 天 token 消耗柱状图 + 汇总（总量/run 数/最常用模型/缓存命中率）+ 按来源拆分。 */
export default function TokenStatsModal() {
  const { t } = useTranslation();
  const [days, setDays] = useState<DailyStats[]>([]);
  const config = useSettings((s) => s.config);

  useEffect(() => {
    void ipc.getTokenStats(30).then(setDays).catch(() => setDays([]));
  }, []);

  // model_id → 计费语义（provider 级 api_format；历史已删模型查不到，其量不参与命中率计算）
  const sems = useMemo(() => {
    const map: Record<string, CacheSemantics> = {};
    for (const p of config?.providers ?? []) {
      const sem = cacheSemanticsOfFormat(p.api_format);
      for (const m of p.models) map[m.id] = sem;
    }
    return map;
  }, [config]);

  // model_id → 显示名（ProviderModel.model 即界面各处展示的模型名；
  // by_model 的 key 是内部 model_id，直接展示会露出无意义 id）
  const names = useMemo(() => {
    const map: Record<string, string> = {};
    for (const p of config?.providers ?? []) {
      for (const m of p.models) map[m.id] = m.model;
    }
    return map;
  }, [config]);

  // 30 天按模型聚合：命中率分母口径随协议不同，只能按模型算再汇总
  const perModel = useMemo(() => {
    const agg: Record<string, ModelAgg> = {};
    for (const d of days) {
      for (const [m, v] of Object.entries(d.by_model ?? {})) {
        const cur = (agg[m] ??= { input: 0, output: 0, cache_read: 0, cache_write: 0, runs: 0 });
        cur.input += v.input;
        cur.output += v.output;
        cur.cache_read += v.cache_read;
        cur.cache_write += v.cache_write;
        cur.runs += v.runs;
      }
    }
    return agg;
  }, [days]);

  const dayTotal = (d: DailyStats): number =>
    Object.entries(d.by_model ?? {}).reduce(
      (sum, [m, v]) => sum + trueTotal(v, sems[m] ?? "openai"),
      0,
    );

  const topModel = (() => {
    if (!days.length) return null;
    const agg: Record<string, number> = {};
    for (const [m, v] of Object.entries(perModel)) {
      agg[m] = trueTotal(v, sems[m] ?? "openai");
    }
    const top = Object.entries(agg).sort((a, b) => b[1] - a[1])[0];
    if (!top) return null;
    // 已删/未知模型无显示名，回退截断 id 保证可辨认
    const name = names[top[0]] ?? `${top[0].slice(0, 8)}…`;
    return `${name} (${fmt(top[1])})`;
  })();

  // 缓存命中（按可识别协议的模型汇总；未知协议模型只跳过比率，不影响其他模型的比率正确性）
  const hit = (() => {
    let num = 0;
    let den = 0;
    let write = 0;
    let known = false;
    for (const [m, v] of Object.entries(perModel)) {
      const sem = sems[m];
      if (!sem) continue;
      known = true;
      num += v.cache_read;
      write += v.cache_write;
      den += cacheDenominator({ input: v.input, cacheRead: v.cache_read, cacheWrite: v.cache_write }, sem);
    }
    if (!known || den === 0) return null;
    return { rate: num / den, read: num, write };
  })();

  // L10：按来源拆分（仅当存在子代理/定时任务用量时显示，避免全是主会话的噪音）。
  // 口径用 output：输出 token 语义跨协议一致；input 的缓存口径随协议不同，而 kind 聚合
  // 已丢失 model 维度，无法精确归一口径（total 口径同理，统一走 by_model 聚合）
  const byKind = (() => {
    const agg: Record<string, number> = {};
    for (const d of days) {
      for (const [k, v] of Object.entries(d.by_kind ?? {})) {
        agg[k] = (agg[k] ?? 0) + v.output;
      }
    }
    if ((agg.sub ?? 0) + (agg.task ?? 0) === 0) return null;
    const label: Record<string, string> = {
      main: t("stats.kindMain"),
      sub: t("stats.kindSub"),
      task: t("stats.kindTask"),
      compact: t("stats.kindCompact"),
      title: t("stats.kindTitle"),
    };
    return Object.entries(agg)
      .sort((a, b) => b[1] - a[1])
      .map(([k, v]) => `${label[k] ?? k} ${fmt(v)}`)
      .join(" · ");
  })();

  function barHeight(d: DailyStats): string {
    const max = Math.max(...days.map(dayTotal), 1);
    return `${Math.max(2, (dayTotal(d) / max) * 120)}px`;
  }

  return (
    <Modal
      open
      onCancel={() => useUi.setState({ statsOpen: false })}
      footer={null}
      width={760}
      title={t("stats.title")}
    >
      {days.length > 0 ? (
        <div className="chart">
          {days.map((d) => (
            <div
              className="bar-col"
              key={d.date}
              title={`${d.date}: ${fmt(dayTotal(d))} tokens, ${d.total.runs} runs`}
            >
              <div className="bar" style={{ height: barHeight(d) }} />
              <span className="lbl">{d.date.slice(5)}</span>
            </div>
          ))}
        </div>
      ) : (
        <Empty description={t("stats.empty")} />
      )}
      {days.length > 0 && (
        <div className="stats-summary dim">
          {t("stats.total")}：
          {fmt(days.reduce((a, d) => a + dayTotal(d), 0))} tokens ·
          {" "}{days.reduce((a, d) => a + d.total.runs, 0)} runs
          {topModel && ` · ${t("stats.topModel")}：${topModel}`}
        </div>
      )}
      {days.length > 0 && hit && (
        <div className="stats-summary dim">
          {t("stats.cacheHit", {
            rate: `${(hit.rate * 100).toFixed(1)}%`,
            read: fmt(hit.read),
            write: fmt(hit.write),
          })}
        </div>
      )}
      {days.length > 0 && byKind && (
        <div className="stats-summary dim">{t("stats.sources", { list: byKind })}</div>
      )}
    </Modal>
  );
}
