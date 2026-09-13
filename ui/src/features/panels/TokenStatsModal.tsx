import { useEffect, useState } from "react";
import { Empty, Modal } from "antd";
import { useTranslation } from "react-i18next";
import { ipc } from "../../ipc/client";
import type { DailyStats } from "../../ipc/types";
import { useUi } from "../../stores/ui";

/** token 数缩写：M/k 分级缩写，便于柱状图标签与摘要展示。 */
function fmt(n: number): string {
  if (n >= 1_000_000) return `${(n / 1_000_000).toFixed(1)}M`;
  if (n >= 1_000) return `${(n / 1_000).toFixed(1)}k`;
  return String(n);
}

/** 任务/统计弹窗：近 30 天 token 消耗柱状图 + 汇总（总量/run 数/最常用模型）+ 按来源拆分。 */
export default function TokenStatsModal() {
  const { t } = useTranslation();
  const [days, setDays] = useState<DailyStats[]>([]);

  useEffect(() => {
    void ipc.getTokenStats(30).then(setDays).catch(() => setDays([]));
  }, []);

  const topModel = (() => {
    if (!days.length) return null;
    const agg: Record<string, number> = {};
    for (const d of days) {
      for (const [m, v] of Object.entries(d.by_model ?? {})) {
        agg[m] = (agg[m] ?? 0) + v.input + v.output + v.cache_read;
      }
    }
    const top = Object.entries(agg).sort((a, b) => b[1] - a[1])[0];
    return top ? `${top[0].slice(0, 8)}… (${fmt(top[1])})` : null;
  })();

  // L10：按来源拆分（仅当存在子代理/定时任务用量时显示，避免全是主会话的噪音）
  const byKind = (() => {
    const agg: Record<string, number> = {};
    for (const d of days) {
      for (const [k, v] of Object.entries(d.by_kind ?? {})) {
        agg[k] = (agg[k] ?? 0) + v.input + v.output + v.cache_read;
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
    const max = Math.max(...days.map((x) => x.total.input + x.total.output + x.total.cache_read), 1);
    const v = d.total.input + d.total.output + d.total.cache_read;
    return `${Math.max(2, (v / max) * 120)}px`;
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
              title={`${d.date}: ${fmt(d.total.input + d.total.output + d.total.cache_read)} tokens, ${d.total.runs} runs`}
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
          {fmt(days.reduce((a, d) => a + d.total.input + d.total.output + d.total.cache_read, 0))} tokens ·
          {" "}{days.reduce((a, d) => a + d.total.runs, 0)} runs
          {topModel && ` · ${t("stats.topModel")}：${topModel}`}
        </div>
      )}
      {days.length > 0 && byKind && (
        <div className="stats-summary dim">{t("stats.sources", { list: byKind })}</div>
      )}
    </Modal>
  );
}
