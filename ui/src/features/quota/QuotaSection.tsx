// 订阅额度段（顶替原「会话」段；[docs/rightbar-info-refactor-and-subscription-quota](../../../../docs/rightbar-info-refactor-and-subscription-quota.md)）。
//
// 数据来自后端 `quota_snapshots`（只返回检测到凭证的提供商）。展示口径：
// 每家一行摘要 = 提供商名 + 最紧张窗口标签 + 剩余% + 细进度条，点整行展开全部窗口；
// 百分比行按剩余方向渲染（<20% 橙、<5% 红），数值行（余额）直接给文本。
// 刷新时机：可见时进入拉一次 + 每 5 分钟一次 + 手动刷新；面板收起/切页签即停（`visible` 门控）。
import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { Button, Progress, Tooltip } from "antd";
import { ReloadOutlined } from "@ant-design/icons";
import { useTranslation } from "react-i18next";
import { ipc } from "../../ipc/client";
import type { QuotaSnapshot } from "../../ipc/types";
import { useSettings } from "../../stores/settings";
import { useActiveTab } from "../../stores/sessions";
import { activeBaseUrlOf } from "../../utils/models";
import { readExpandedQuotaProviders, writeExpandedQuotaProviders } from "../../utils/rightbarPrefs";
import {
  countdown,
  entryLabel,
  formatPercent,
  relativeUpdatedAt,
  riskLevel,
  summaryEntry,
  summaryRemaining,
} from "./quotaFormat";

/** 自动刷新间隔（额度是小时/天级数据，5 分钟足够） */
const AUTO_REFRESH_MS = 5 * 60 * 1000;
/** 倒计时/相对时间的文本刷新间隔（不触发请求） */
const TICK_MS = 60 * 1000;

/** 无凭证提示里列出的来源（与后端凭证链一致） */
const CREDENTIAL_HINT = [
  "OPENCODE_API_KEY / DEEPSEEK_API_KEY / MINIMAX_*_API_KEY / KIMI_*_API_KEY / ZHIPU_*_API_KEY / ZAI_*_API_KEY",
  "~/.config/opencode/opencode.jsonc → provider.<name>.options.apiKey",
  "~/.local/share/opencode/auth.json",
];

interface Props {
  /** 右栏展开且停在信息页签（隐藏即停轮询） */
  visible: boolean;
}

/** 风险等级 → 进度条包裹类（颜色走 app.css 的 --ws-warn/--ws-err token，不在这里硬编码） */
function barClass(risk: string): string {
  return risk === "normal" ? "rb-quota-bar" : `rb-quota-bar risk-${risk}`;
}

/** 单家提供商的摘要行 + 展开行 */
function ProviderBlock({
  snapshot,
  expanded,
  now,
  onToggle,
}: {
  snapshot: QuotaSnapshot;
  expanded: boolean;
  /** 文本 ticker 的当前时刻（倒计时要跟着走，不能只靠刷新重建） */
  now: number;
  onToggle: () => void;
}) {
  const { t } = useTranslation();
  const summary = summaryEntry(snapshot.entries);
  const remaining = summaryRemaining(snapshot.entries);
  const risk = riskLevel(remaining);
  const summaryLabel = summary ? entryLabel(summary, (k) => t(`rightbar.window.${k}`)) : "";
  const source = snapshot.credential_source ? `\n${t("rightbar.quotaSource", { source: snapshot.credential_source })}` : "";

  return (
    <div className="rb-quota-provider">
      <button
        type="button"
        className="rb-quota-summary"
        aria-expanded={expanded}
        onClick={onToggle}
        title={`${snapshot.display_name}${source}`}
      >
        <span className="rb-quota-name">{snapshot.display_name}</span>
        {/* 未展开时也要能看出这家不对劲（错误详情在展开内容里） */}
        {snapshot.status !== "ok" && (
          <span className="rb-quota-flag" title={snapshot.error ?? undefined}>
            !
          </span>
        )}
        {/* 额度摘要（余额文本 / 最紧张窗口 + 剩余% + 进度条）：**只在折叠时显示**——
            展开后明细行里有更完整的一份，标题行再重复一遍会与明细互相打架 */}
        {!expanded && summary?.value_text && <span className="rb-quota-value">{summary.value_text}</span>}
        {!expanded && remaining !== null && remaining !== undefined && (
          <>
            <span className="rb-quota-window">{summaryLabel}</span>
            <span className={`rb-quota-remaining risk-${risk}`}>
              {t("rightbar.quotaRemaining", { pct: formatPercent(remaining) })}
            </span>
            <span className={barClass(risk)}>
              <Progress percent={remaining} showInfo={false} size="small" />
            </span>
          </>
        )}
      </button>

      {expanded && (
        <div className="rb-quota-entries">
          {snapshot.status !== "ok" && (
            <div className="rb-quota-error">
              {snapshot.status === "invalid"
                ? t("rightbar.quotaInvalid", { reason: snapshot.error ?? "" })
                : t("rightbar.quotaFailed", { reason: snapshot.error ?? "" })}
            </div>
          )}
          {snapshot.entries.map((entry) => {
            const entryRisk = riskLevel(entry.remaining_percent);
            const reset = countdown(entry.resets_at, now);
            return (
              <div className="rb-quota-entry" key={entry.key}>
                <span className="rb-quota-window">
                  {entryLabel(entry, (k) => t(`rightbar.window.${k}`))}
                </span>
                {entry.value_text !== null && entry.value_text !== undefined ? (
                  <span className="rb-quota-value rb-mono">{entry.value_text}</span>
                ) : (
                  <>
                    <span className={`rb-quota-remaining risk-${entryRisk}`}>
                      {t("rightbar.quotaRemaining", { pct: formatPercent(entry.remaining_percent) })}
                    </span>
                    <span className={barClass(entryRisk)}>
                      <Progress percent={entry.remaining_percent ?? 0} showInfo={false} size="small" />
                    </span>
                    {reset && (
                      <span className="rb-quota-reset rb-dim">{t(reset.key, reset.params)}</span>
                    )}
                  </>
                )}
              </div>
            );
          })}
        </div>
      )}
    </div>
  );
}

export default function QuotaSection({ visible }: Props) {
  const { t } = useTranslation();
  const config = useSettings((s) => s.config);
  const tab = useActiveTab();
  const [snapshots, setSnapshots] = useState<QuotaSnapshot[]>([]);
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState<string>("");
  const [now, setNow] = useState(() => Date.now());
  const [expanded, setExpanded] = useState<Set<string>>(() => readExpandedQuotaProviders());
  const requestId = useRef(0);

  // 当前会话生效模型所属 provider 的 base_url：命中的额度提供商在结果里置顶
  const activeBaseUrl = useMemo(
    () => activeBaseUrlOf(config, tab?.prefs?.model_id ?? null),
    [config, tab?.prefs?.model_id],
  );

  const refresh = useCallback(async () => {
    const myId = ++requestId.current;
    setLoading(true);
    try {
      const next = await ipc.quotaSnapshots(activeBaseUrl);
      if (requestId.current !== myId) return;
      setSnapshots(Array.isArray(next) ? next : []);
      setError("");
    } catch (e) {
      if (requestId.current !== myId) return;
      setError(String(e).replace(/^Error[:\s]*/i, ""));
      setSnapshots([]);
    } finally {
      if (requestId.current === myId) setLoading(false);
    }
  }, [activeBaseUrl]);

  // 可见时：进入拉一次 + 每 5 分钟自动刷新；隐藏即停（依赖变化会重建定时器）
  useEffect(() => {
    if (!visible) return;
    void refresh();
    const timer = setInterval(() => void refresh(), AUTO_REFRESH_MS);
    return () => clearInterval(timer);
  }, [visible, refresh]);

  // 仅文本 ticker：刷新「X 分钟前更新」与倒计时，不发请求
  useEffect(() => {
    if (!visible) return;
    const timer = setInterval(() => setNow(Date.now()), TICK_MS);
    return () => clearInterval(timer);
  }, [visible]);

  const toggle = (providerId: string) => {
    setExpanded((prev) => {
      const next = new Set(prev);
      if (next.has(providerId)) next.delete(providerId);
      else next.add(providerId);
      writeExpandedQuotaProviders(next);
      return next;
    });
  };

  const updated = relativeUpdatedAt(snapshots, now);

  return (
    <div className="rb-section rb-quota">
      <div className="rb-label-row">
        <div className="rb-label">{t("rightbar.quota")}</div>
        {/* 更新时间贴在刷新按钮左侧（同一标签行，不再沉到列表底部）。
            `.rb-quota-updated` 的 font-size 仍取自样式表；其 margin-top 是按「底部独立行」设计的，
            行内改用后必须内联归零（内联优先级高于样式表），否则文字会比按钮低 4px。 */}
        <div className="rb-label-actions">
          {updated && (
            <span className="rb-dim rb-quota-updated" style={{ marginTop: 0 }}>
              {t(updated.key, updated.params)}
            </span>
          )}
          <Button
            className="rb-open-dir-btn"
            type="text"
            size="small"
            loading={loading}
            icon={<ReloadOutlined />}
            title={t("rightbar.quotaRefresh")}
            aria-label={t("rightbar.quotaRefresh")}
            onClick={() => void refresh()}
          />
        </div>
      </div>

      {loading && snapshots.length === 0 && <div className="rb-dim">{t("rightbar.quotaLoading")}</div>}

      {!loading && error && (
        <div className="rb-quota-error">
          {t("rightbar.quotaFailed", { reason: error })}
          <Button size="small" type="link" onClick={() => void refresh()}>
            {t("rightbar.quotaRetry")}
          </Button>
        </div>
      )}

      {!error && snapshots.length === 0 && !loading && (
        <Tooltip title={<span style={{ whiteSpace: "pre-line" }}>{CREDENTIAL_HINT.join("\n")}</span>}>
          <div className="rb-dim rb-quota-empty">{t("rightbar.quotaNoCredential")}</div>
        </Tooltip>
      )}

      {snapshots.map((snapshot) => (
        <ProviderBlock
          key={snapshot.provider_id}
          snapshot={snapshot}
          expanded={expanded.has(snapshot.provider_id)}
          now={now}
          onToggle={() => toggle(snapshot.provider_id)}
        />
      ))}
    </div>
  );
}
