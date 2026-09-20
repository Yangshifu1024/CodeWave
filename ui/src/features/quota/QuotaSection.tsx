// 额度与余额段（[docs/quota-from-provider-config](../../../../docs/quota-from-provider-config.md)）。
//
// 行集合 = CodeWave 供应商配置：后端 `quota_snapshots` 已按「可查询类在前（配置顺序）、unsupported 殿后」排好序，
// 前端不再重排序。五类行：
//   ok        —— 名称 + 最紧张窗口 + 剩余% + 进度条，点整行展开全部窗口明细（含倒计时）；
//   error / invalid —— 折叠态带 `!` 标记，展开给原因与重试（曾成功过时另附上次成功时间）；
//   rejected  —— 降级灰行，仍可展开看原因 + 时间行（按定义从未成功过）+ 重试；
//   no_key    —— 独立行，只给标签（不参与 unsupported 折叠）；
//   unsupported —— 灰行（「不支持额度查询」+ 原因标签），多于 3 家时默认折叠为一行汇总。
// 灰行（no_key / rejected / unsupported）右侧另有行内「去设置」小按钮，跳到设置页对应供应商。
// 刷新时机：可见时进入拉一次 + 每 5 分钟 + 手动刷新 + 配置/模型变化 debounce 500ms；
// 面板收起/切页签即停（`visible` 门控）。
// 「时间行」数据源 = 后端持久化的 `snapshot.last_ok_at`（`~/.codewave/quota.json` 里记的上次成功时刻），
// 前端不再自己记忆——否则重启/换机后回显就与磁盘事实不一致。该行是否出现只由**数据**（有无 `last_ok_at`）
// 决定，不按行态白名单，见 QuotaRow 里的 `showTimeRow`。
import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { Button, Progress, Tooltip } from "antd";
import { ReloadOutlined, SettingOutlined } from "@ant-design/icons";
import { useTranslation } from "react-i18next";
import { ipc } from "../../ipc/client";
import type { QuotaEntry, QuotaSnapshot } from "../../ipc/types";
import { useSettings } from "../../stores/settings";
import { useActiveTab } from "../../stores/sessions";
import { useUi } from "../../stores/ui";
import { activeProviderIdOf } from "../../utils/models";
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
import {
  agoKind,
  disambiguateTitles,
  entryStatusKind,
  hostOf,
  shouldCollapseUnsupported,
  splitBySupport,
  unsupportedReason,
} from "./quotaRow";

/** 自动刷新间隔（额度是小时/天级数据，5 分钟足够） */
const AUTO_REFRESH_MS = 5 * 60 * 1000;
/** 倒计时/相对时间的文本刷新间隔（不触发请求） */
const TICK_MS = 60 * 1000;
/** 配置/模型变化后的重拉延迟（连续改配置只发最后一次请求） */
const CONFIG_DEBOUNCE_MS = 500;

interface Props {
  /** 右栏展开且停在信息页签（隐藏即停轮询） */
  visible: boolean;
}

/** 风险等级 → 进度条包裹类（颜色走 app.css 的 --ws-warn/--ws-err token，不在这里硬编码） */
function barClass(risk: string): string {
  return risk === "normal" ? "rb-quota-bar" : `rb-quota-bar risk-${risk}`;
}

/**
 * 供应商名（窄栏可截断，`title` 补全）；有密钥来源时悬浮显示「密钥来源」。
 * 注意：悬浮文案只有「钥匙串 / 配置文件」两种口径，不再暴露具体的凭证链细节。
 */
function ProviderName({ name, keySource }: { name: string; keySource: string }) {
  const span = (
    <span className="rb-quota-name" title={name}>
      {name}
    </span>
  );
  return keySource ? <Tooltip title={keySource}>{span}</Tooltip> : span;
}

/**
 * 行内次级跳转按钮（去设置 → `anchor` 指向的供应商）。
 * 行本体是展开/收起用的 button，故这里必须与它**同级**（HTML 不允许 button 嵌套）。
 */
function GoSettingsButton({ anchor, withText = false }: { anchor: string; withText?: boolean }) {
  const { t } = useTranslation();
  return (
    <Button
      className={withText ? "rb-quota-gosettings rb-quota-gosettings-text" : "rb-quota-gosettings"}
      type="text"
      size="small"
      icon={<SettingOutlined />}
      title={t("rightbar.quotaGoSettings")}
      aria-label={t("rightbar.quotaGoSettings")}
      onClick={() => useUi.getState().showSettingsAt("providers", anchor)}
    >
      {withText ? t("rightbar.quotaGoSettings") : null}
    </Button>
  );
}

/** 一行供应商（ok / error / invalid / rejected / no_key 五类里的前四类 + 无密钥独立行） */
function QuotaRow({
  snapshot,
  title,
  expanded,
  now,
  onToggle,
  onRetry,
}: {
  snapshot: QuotaSnapshot;
  /** 消歧后的显示名（同名两家 → 带主机名后缀） */
  title: string;
  expanded: boolean;
  /** 文本 ticker 的当前时刻（倒计时要跟着走，不能只靠刷新重建） */
  now: number;
  onToggle: () => void;
  onRetry: () => void;
}) {
  const { t } = useTranslation();
  const keySource = snapshot.key_source
    ? t(snapshot.key_source === "keyring" ? "rightbar.quotaKeySourceKeyring" : "rightbar.quotaKeySourceConfig")
    : "";
  const anchor = `providers.${snapshot.provider_id}`;
  const labelOf = (entry: QuotaEntry) => entryLabel(entry, (k) => t(`rightbar.window.${k}`));

  // 未配置密钥：独立行，没有明细可展开（只给标签 + 跳转设置）
  if (snapshot.status === "no_key") {
    return (
      <div className="rb-quota-row">
        <div className="rb-quota-summary rb-quota-static">
          <ProviderName name={title} keySource={keySource} />
          <span className="rb-quota-tag">{t("rightbar.quotaNoKey")}</span>
        </div>
        <GoSettingsButton anchor={anchor} />
      </div>
    );
  }

  const degraded = snapshot.status === "rejected";
  const failed = snapshot.status === "error" || snapshot.status === "invalid";
  const summary = summaryEntry(snapshot.entries);
  const remaining = summaryRemaining(snapshot.entries);
  const risk = riskLevel(remaining);
  const summaryLabel = summary ? labelOf(summary) : "";
  const reason = snapshot.error ?? snapshot.reason ?? "";

  // 时间行：**按数据决定**，不按行态白名单——后端 `last_ok_at` 才是「曾成功过」的地面事实：
  //   ok              → 不显示（本就有「X 分钟前更新」，再来一条只是重复）；
  //   error / invalid → `last_ok_at` 有值（曾成功过）才显示「上次成功：…」；null 表示从未成功过，
  //                     失败原因本身已足够，不再补一行；
  //   rejected        → 恒显示：按定义（401/403/404 且从未成功过）无值 → 直接给「从未成功查询」；
  //                     后端若意外带了历史值，则优先显示「上次成功：…」（防御性，正常产不出来）。
  const ago = agoKind(snapshot.last_ok_at, now);
  const showTimeRow = snapshot.status === "ok" ? false : ago !== null || degraded;
  let when = "";
  if (ago?.kind === "just_now") when = t("rightbar.quotaAgoJustNow");
  else if (ago?.kind === "minutes") when = t("rightbar.quotaAgoMinutes", { m: ago.value });
  else if (ago?.kind === "hours") when = t("rightbar.quotaAgoHours", { h: ago.value });
  else if (ago?.kind === "days") when = t("rightbar.quotaAgoDays", { d: ago.value });
  // 有可用时刻 → 「上次成功：…」；无值（只有 rejected 走得到）→ 直接说「从未成功查询」，
  // 不再拼成「上次成功：从未成功查询」这种自相矛盾的句子。
  const timeRowText = ago ? t("rightbar.quotaLastOk", { when }) : t("rightbar.quotaNeverOk");

  return (
    <div className="rb-quota-provider">
      <div className="rb-quota-row">
        <button
          type="button"
          className={`rb-quota-summary${degraded ? " rb-quota-degraded" : ""}`}
          aria-expanded={expanded}
          onClick={onToggle}
          title={title}
        >
          <ProviderName name={title} keySource={keySource} />
          {/* 折叠态也要能看出这家不对劲（错误详情在展开内容里） */}
          {failed && (
            <span className="rb-quota-flag" title={reason || undefined}>
              !
            </span>
          )}
          {degraded && <span className="rb-quota-tag">{t("rightbar.quotaRejected")}</span>}
          {/* 额度摘要（余额文本 / 最紧张窗口 + 剩余% + 进度条）：**只在折叠时显示**——
              展开后明细行里有更完整的一份，标题行再重复一遍会与明细互相打架 */}
          {!degraded && !expanded && summary?.value_text && (
            <span className="rb-quota-value">{summary.value_text}</span>
          )}
          {!degraded && !expanded && remaining !== null && remaining !== undefined && (
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
        {degraded && <GoSettingsButton anchor={anchor} />}
      </div>

      {expanded && (
        <div className="rb-quota-entries">
          {failed && (
            <div className="rb-quota-error">
              {t(snapshot.status === "invalid" ? "rightbar.quotaInvalid" : "rightbar.quotaFailed", { reason })}
              {showTimeRow && <div className="rb-quota-lastok rb-dim">{timeRowText}</div>}
              <Button size="small" type="link" onClick={onRetry}>
                {t("rightbar.quotaRetry")}
              </Button>
            </div>
          )}
          {degraded && (
            <div className="rb-quota-error">
              {t("rightbar.quotaFailed", { reason })}
              {showTimeRow && <div className="rb-quota-lastok rb-dim">{timeRowText}</div>}
              <Button size="small" type="link" onClick={onRetry}>
                {t("rightbar.quotaRetry")}
              </Button>
            </div>
          )}
          {snapshot.entries.map((entry) => {
            const entryRisk = riskLevel(entry.remaining_percent);
            const reset = countdown(entry.resets_at, now);
            const statusKind = entryStatusKind(entry.status);
            // 受限类走橙色标签；其它未知值原样灰显（forward-compatible，不隐藏数据）
            const statusLabel =
              statusKind === "limited" ? t("rightbar.quotaStatusRateLimited") : (entry.status ?? "");
            return (
              <div className="rb-quota-entry" key={entry.key}>
                <span className="rb-quota-window">{labelOf(entry)}</span>
                {statusKind !== "none" && (
                  <span
                    className={statusKind === "limited" ? "rb-quota-status risk-warn" : "rb-quota-status"}
                    title={statusLabel}
                  >
                    {statusLabel}
                  </span>
                )}
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
                    {reset && <span className="rb-quota-reset rb-dim">{t(reset.key, reset.params)}</span>}
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

/** unsupported 灰行：不支持额度查询 + 原因标签（原因由后端 reason 决定），行内给跳转小按钮 */
function UnsupportedRow({ snapshot, title }: { snapshot: QuotaSnapshot; title: string }) {
  const { t } = useTranslation();
  const keySource = snapshot.key_source
    ? t(snapshot.key_source === "keyring" ? "rightbar.quotaKeySourceKeyring" : "rightbar.quotaKeySourceConfig")
    : "";
  const reason = unsupportedReason(snapshot);
  const reasonText = t(
    reason === "empty_base_url" ? "rightbar.quotaReasonEmptyUrl" : "rightbar.quotaReasonNoAdapter",
  );
  return (
    <div className="rb-quota-row">
      <div className="rb-quota-summary rb-quota-static">
        <ProviderName name={title} keySource={keySource} />
        <span className="rb-quota-tag">{t("rightbar.quotaUnsupported")}</span>
        <span className="rb-quota-reason" title={reasonText}>
          {reasonText}
        </span>
      </div>
      <GoSettingsButton anchor={`providers.${snapshot.provider_id}`} />
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
  const [unsupportedOpen, setUnsupportedOpen] = useState(false);
  const requestId = useRef(0);

  const modelId = tab?.prefs?.model_id ?? null;
  // 当前会话生效模型所属供应商：额度查询的 IPC 入参
  const activeProviderId = useMemo(() => activeProviderIdOf(config, modelId), [config, modelId]);
  // provider_id → base_url 主机名（标题消歧用）
  const hostById = useMemo(() => {
    const map = new Map<string, string>();
    for (const p of config?.providers ?? []) {
      const host = hostOf(p.base_url);
      if (host) map.set(p.id, host);
    }
    return map;
  }, [config]);
  // 配置签名：增删 / name / base_url / keys 任一变化都要重拉（keys 只当变更指纹，不展示、不落盘、不进日志）
  const configSignature = useMemo(
    () => JSON.stringify((config?.providers ?? []).map((p) => [p.id, p.name, p.base_url, p.keys])),
    [config],
  );
  const refreshKey = `${configSignature}|${modelId ?? ""}|${activeProviderId ?? ""}`;

  const refresh = useCallback(async () => {
    const myId = ++requestId.current;
    setLoading(true);
    try {
      const next = await ipc.quotaSnapshots(activeProviderId);
      if (requestId.current !== myId) return;
      const list = Array.isArray(next) ? next : [];
      setSnapshots(list);
      setError("");
      // 展开态：并上磁盘里对当前快照合法的项（旧版本的 kind 串在这层被过滤，**不回写**磁盘）
      const validIds = new Set(list.map((s) => s.provider_id));
      const stored = readExpandedQuotaProviders(validIds);
      setExpanded((prev) => {
        const merged = new Set<string>();
        for (const id of prev) if (validIds.has(id)) merged.add(id);
        for (const id of stored) merged.add(id);
        return merged;
      });
    } catch (e) {
      if (requestId.current !== myId) return;
      setError(String(e).replace(/^Error[:\s]*/i, ""));
      setSnapshots([]);
    } finally {
      if (requestId.current === myId) setLoading(false);
    }
  }, [activeProviderId]);

  // refresh 的最新引用：定时器与 debounce 都经它调用，避免每次配置变化都重建 5 分钟定时器
  const refreshRef = useRef(refresh);
  useEffect(() => {
    refreshRef.current = refresh;
  }, [refresh]);

  // 可见时：进入拉一次 + 每 5 分钟自动刷新；隐藏即停（只随 visible 重建定时器）
  useEffect(() => {
    if (!visible) return;
    void refreshRef.current();
    const timer = setInterval(() => void refreshRef.current(), AUTO_REFRESH_MS);
    return () => clearInterval(timer);
  }, [visible]);

  // 配置 / 当前模型变化：debounce 500ms 重拉（刚进入面板那一次由上面的「进入拉一次」负责）
  const lastKey = useRef<string | null>(null);
  useEffect(() => {
    if (!visible) {
      lastKey.current = null;
      return;
    }
    if (lastKey.current === null) {
      lastKey.current = refreshKey;
      return;
    }
    if (lastKey.current === refreshKey) return;
    lastKey.current = refreshKey;
    const timer = setTimeout(() => void refreshRef.current(), CONFIG_DEBOUNCE_MS);
    return () => clearTimeout(timer);
  }, [visible, refreshKey]);

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

  const { main, unsupported } = useMemo(() => splitBySupport(snapshots), [snapshots]);
  const titles = useMemo(
    () => disambiguateTitles(snapshots, (id) => hostById.get(id) ?? null),
    [snapshots, hostById],
  );
  const collapsible = shouldCollapseUnsupported(unsupported.length);
  const titleOf = (snapshot: QuotaSnapshot) => titles.get(snapshot.provider_id) ?? snapshot.display_name;
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

      {/* 一家供应商都没配置：不再列举凭证链，直接给一条去设置的入口 */}
      {!error && !loading && snapshots.length === 0 && (
        <div className="rb-quota-empty">
          <div className="rb-dim">{t("rightbar.quotaEmpty")}</div>
          <GoSettingsButton anchor="providers" withText />
        </div>
      )}

      {main.map((snapshot) => (
        <QuotaRow
          key={snapshot.provider_id}
          snapshot={snapshot}
          title={titleOf(snapshot)}
          expanded={expanded.has(snapshot.provider_id)}
          now={now}
          onToggle={() => toggle(snapshot.provider_id)}
          onRetry={() => void refresh()}
        />
      ))}

      {/* unsupported 段：> 3 家时默认折叠为一行汇总，点同一行按钮展开/收起 */}
      {(!collapsible || unsupportedOpen) &&
        unsupported.map((snapshot) => (
          <UnsupportedRow key={snapshot.provider_id} snapshot={snapshot} title={titleOf(snapshot)} />
        ))}
      {collapsible && (
        <button
          type="button"
          className="rb-quota-collapse"
          aria-expanded={unsupportedOpen}
          onClick={() => setUnsupportedOpen((v) => !v)}
        >
          {t("rightbar.quotaCollapsed", { n: unsupported.length })}
        </button>
      )}
    </div>
  );
}
