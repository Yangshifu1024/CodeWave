// 设置页「界面」页（批② 8 页重划后的 appearance）：界面语言（即时生效）+ 主题三档
// （跟随系统/亮色/暗色，即选即生效）+ 自定义字体双槽
// （[docs/custom-font-and-titlebar](../../../../docs/custom-font-and-titlebar.md)，Enter/失焦提交）。
// 三组偏好均为 localStorage 持久化的纯 UI 偏好，绕过设置保存按钮；
// 界面语言随批② 从旧「通用」页迁入本页（[docs/settings-ia](../../../../docs/settings-ia.md)）。
import { useEffect, useRef, useState } from "react";
import { Button, Input, Radio, Select } from "antd";
import { UndoOutlined } from "@ant-design/icons";
import { useTranslation } from "react-i18next";
import { SettingsForm, SettingsFormItem, SettingsSection } from "./settings/SettingsTheme";
import type { ConfigState } from "../../ipc/types";
import { ipc } from "../../ipc/client";
import { useUi, type ThemePref } from "../../stores/ui";
import {
  commitFontSlot, DEFAULT_FONT_LEADS, previewFontFamily, readStoredFonts, sanitizeFontList,
  type FontPreferences, type FontSlot,
} from "../../utils/fonts";

const PREVIEW_SAMPLE = "Aa Bb 0123 — The quick brown fox · 中文字体预览 0O1lI";

/** 界面页容器：主题三档 + 字体双槽（本组件持有唯一 Form，FontSettings 只渲染表单项）+ 界面语言。
 *  界面语言需要页级 draft 与 patchDraft（由 SettingsPage 注入）——未注入时不渲染该项，
 *  保持本组件可独立挂载（测试与后续复用）。 */
export function AppearanceSettings({ draft, patchDraft }: {
  draft?: ConfigState | null;
  patchDraft?: (patch: Partial<ConfigState>) => void;
}) {
  const { t } = useTranslation();
  const theme = useUi((s) => s.theme);

  return (
    <SettingsForm>
      {draft && patchDraft && (
        <SettingsSection title={t("settings.language")}>
          <SettingsFormItem label={t("settings.language")} settingDescription={t("settings.languageHint")}>
            {/* 即时生效：改完立即写 useUi.setLanguage（并镜像进 draft.ui.language），不参与脏标记。 */}
            <div className="setting-anchor" data-setting-id="ui.language">
              <Select
                className="w-narrow"
                value={draft.ui.language}
                onChange={(v) => {
                  patchDraft({ ui: { ...draft.ui, language: v } });
                  useUi.getState().setLanguage(v as "zh-CN" | "en-US");
                }}
                options={[
                  { label: "中文", value: "zh-CN" },
                  { label: "English", value: "en-US" },
                ]}
              />
            </div>
          </SettingsFormItem>
        </SettingsSection>
      )}
      <SettingsSection title={t("settings.theme")}>
        <SettingsFormItem className="settings-theme-picker-row">
          <div className="setting-anchor settings-theme-picker" data-setting-id="ui.theme">
            <Radio.Group
              className="settings-theme-options"
              value={theme}
              onChange={(e) => useUi.getState().setTheme(e.target.value as ThemePref)}
            >
              {([
                ["system", t("settings.themeSystem"), "system"],
                ["light", t("settings.themeLight"), "light"],
                ["dark", t("settings.themeDark"), "dark"],
              ] as const).map(([value, label, preview]) => (
                <Radio key={value} value={value} className="settings-theme-option">
                  <span className={`settings-theme-preview settings-theme-preview-${preview}`} aria-hidden="true">
                    <span />
                    <span />
                  </span>
                  <span className="settings-theme-option-label">{label}</span>
                </Radio>
              ))}
            </Radio.Group>
          </div>
        </SettingsFormItem>
      </SettingsSection>
      {/* 字体组：sans / mono 双槽 + 预览合成一张 section 卡片（[docs/settings-fullscreen-shell]，
          与参考图「浅色主题」组的圆角浅灰容器同构）。Form 上下文由外层 Form 提供。 */}
      <SettingsSection title={t("settings.fontGroup")}>
        <FontSettings />
      </SettingsSection>
    </SettingsForm>
  );
}

/** 输入停止多久后自动提交（敲完就走也不丢；回车/失焦立即提交） */
const COMMIT_IDLE_MS = 600;

/** 后端真源落盘（失败不打断使用：字体已在本地生效，下次提交会再试） */
async function persistFontPrefsToBackend(prefs: FontPreferences): Promise<void> {
  try {
    await ipc.setFontPrefs(prefs.sans, prefs.mono);
  } catch (e) {
    console.warn("字体偏好落盘失败（本地已生效）", e);
  }
}

/** 单个字体槽输入项：回车 / 失焦 / 停手 / 卸载四处都提交，值有变化才落盘，附一键恢复默认按钮。
 *
 * 为什么要四处提交（2026-09-19 修缺陷）：原来只有「回车或失焦」两个时机，而提交前有个
 * 「没变化就 return」的短路——输入后没碰回车、也没点别处（直接关设置页/关窗口），就等于什么都没发生，
 * 界面看上去就是「字体没保存」。 */
function FontField({ slot, label, hint, settingId, appliedFont, onApplied }: {
  slot: FontSlot;
  label: string;
  hint: string;
  /** 设置项锚点 id（= 注册表 SETTINGS_ITEMS.id）：搜索命中定位与临时高亮靠它查 */
  settingId: string;
  appliedFont: string;
  /** 提交生效后的回调（父组件刷新预览） */
  onApplied: () => void;
}) {
  const { t } = useTranslation();
  const [draft, setDraft] = useState(() => readStoredFonts()[slot]);
  /** 用户是否真的改过输入框：**没改过的空提交一律不写**（否则一次误触发就把已有偏好抹掉） */
  const edited = useRef(false);
  /** 输入法组合态：组合期间不提交（半成品不该落盘） */
  const composing = useRef(false);
  const idleTimer = useRef<number | null>(null);
  /** 卸载兑底提交要拿到最新草稿（闭包里的 draft 会过期） */
  const draftRef = useRef(draft);
  draftRef.current = draft;

  function clearIdle() {
    if (idleTimer.current !== null) {
      window.clearTimeout(idleTimer.current);
      idleTimer.current = null;
    }
  }

  /** 提交草稿：净化 → 写缓存并应用 → 后端落盘。返回是否真的写了。 */
  function commit(candidate: string, syncInput = true): boolean {
    clearIdle();
    if (composing.current || !edited.current) return false;
    const applied = readStoredFonts()[slot];
    if (sanitizeFontList(candidate) === applied) {
      edited.current = false;
      return false;
    }
    edited.current = false;
    const next = commitFontSlot(slot, candidate);
    if (syncInput) setDraft(next[slot]);
    void persistFontPrefsToBackend(next);
    onApplied();
    return true;
  }

  /** 停手 COMMIT_IDLE_MS 后自动提交（每次击键重置计时） */
  function scheduleIdleCommit() {
    clearIdle();
    idleTimer.current = window.setTimeout(() => {
      idleTimer.current = null;
      commit(draftRef.current);
    }, COMMIT_IDLE_MS);
  }

  // 卸载前兑底提交：切页 / 关设置页时，刚敲进去的内容不能丢
  useEffect(() => {
    return () => {
      if (idleTimer.current !== null) window.clearTimeout(idleTimer.current);
      if (composing.current || !edited.current) return;
      if (sanitizeFontList(draftRef.current) === readStoredFonts()[slot]) return;
      void persistFontPrefsToBackend(commitFontSlot(slot, draftRef.current));
    };
  }, [slot]);

  return (
    <SettingsFormItem className="settings-font-row" label={label} settingDescription={hint}>
      {/* 字体槽占用半行宽度（不参与宽度三档，见 WIDTH_EXEMPT_ITEM_IDS）。 */}
      <div className="setting-anchor settings-font-control" data-setting-id={settingId}>
        <Input
          value={draft}
          placeholder={DEFAULT_FONT_LEADS[slot]}
          onChange={(e) => {
            edited.current = true;
            setDraft(e.target.value);
            scheduleIdleCommit();
          }}
          onCompositionStart={() => {
            composing.current = true;
          }}
          onCompositionEnd={() => {
            composing.current = false;
            scheduleIdleCommit();
          }}
          onPressEnter={() => commit(draft)}
          onBlur={() => {
            // 失焦时先把组合态强制结束：compositionend 漏发（某些输入法/粘贴场景）时
            // 不能让 composing 永久为真——否则回车、停手、卸载三条提交路径全被挡住
            composing.current = false;
            commit(draft, false);
            // 失焦回填：显示值与存储对齐（已存值 / 刚净化后的值），避免两者漂移
            setDraft(readStoredFonts()[slot]);
          }}
          suffix={
            <Button
              type="text"  title={t("settings.fontReset")}
              aria-label={`${t("settings.fontReset")}・${label}`}
              icon={<UndoOutlined />}
              onClick={() => { edited.current = true; setDraft(""); commit(""); }}
            />
          }
        />
        <div className="font-preview-row" style={{ fontFamily: previewFontFamily(appliedFont, slot) }}>
          {PREVIEW_SAMPLE}
        </div>
      </div>
    </SettingsFormItem>
  );
}

/** 外观页签字体设置：sans/mono 双槽输入 + 应用值实时预览（Form 上下文由 AppearanceSettings 提供）。 */
function FontSettings() {
  const { t } = useTranslation();
  // 预览跟随已生效值（提交后立即变化）
  const [applied, setApplied] = useState<FontPreferences>(() => {
    const stored = readStoredFonts();
    return { sans: sanitizeFontList(stored.sans), mono: sanitizeFontList(stored.mono) };
  });

  return (
    <>
      <FontField slot="sans" label={t("settings.uiFont")} hint={t("settings.fontHint")} settingId="ui.font_sans" appliedFont={applied.sans} onApplied={() => setApplied(sanitizeStored())} />
      <FontField slot="mono" label={t("settings.monoFont")} hint={t("settings.fontHint")} settingId="ui.font_mono" appliedFont={applied.mono} onApplied={() => setApplied(sanitizeStored())} />
    </>
  );
}

function sanitizeStored(): FontPreferences {
  const stored = readStoredFonts();
  return { sans: sanitizeFontList(stored.sans), mono: sanitizeFontList(stored.mono) };
}
