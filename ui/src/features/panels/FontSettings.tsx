// 设置页「界面」页（批② 8 页重划后的 appearance）：界面语言（即时生效）+ 主题三档
// （跟随系统/亮色/暗色，即选即生效）+ 自定义字体双槽
// （[docs/custom-font-and-titlebar](../../../../docs/custom-font-and-titlebar.md)，Enter/失焦提交）。
// 三组偏好均为 localStorage 持久化的纯 UI 偏好，绕过设置保存按钮；
// 界面语言随批② 从旧「通用」页迁入本页（[docs/settings-ia](../../../../docs/settings-ia.md)）。
import { useState } from "react";
import { Button, Form, Input, Select } from "antd";
import { UndoOutlined } from "@ant-design/icons";
import { useTranslation } from "react-i18next";
import type { ConfigState } from "../../ipc/types";
import { useUi, type ThemePref } from "../../stores/ui";
import {
  DEFAULT_FONT_LEADS, previewFontFamily, readStoredFonts, sanitizeFontList, storeFonts,
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
    <Form layout="vertical">
      <Form.Item label={t("settings.theme")} extra={t("settings.themeHint")}>
        {/* 锚点 + 宽度档：批③ 搜索命中定位靠 data-setting-id，控件宽度走 .w-narrow（见 app.css） */}
        <div className="setting-anchor" data-setting-id="ui.theme">
          <Select
            size="small"
            className="w-narrow"
            value={theme}
            onChange={(v) => useUi.getState().setTheme(v as ThemePref)}
            options={[
              { label: t("settings.themeSystem"), value: "system" },
              { label: t("settings.themeLight"), value: "light" },
              { label: t("settings.themeDark"), value: "dark" },
            ]}
          />
        </div>
      </Form.Item>
      <FontSettings />
      {draft && patchDraft && (
        <Form.Item label={t("settings.language")}>
          {/* 即时生效：改完立即写 useUi.setLanguage（并镜像进 draft.ui.language），
              因此不进脏标记（也就不会亮脏点） */}
          <div className="setting-anchor" data-setting-id="ui.language">
            <Select
              size="small"
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
          <span className="settings-instant">{t("settings.instantApply")}</span>
        </Form.Item>
      )}
    </Form>
  );
}

/** 单个字体槽输入项：Enter/失焦提交、值有变化才落盘，附一键恢复默认按钮。 */
function FontField({ slot, label, hint, settingId, onApplied }: {
  slot: FontSlot;
  label: string;
  hint: string;
  /** 设置项锚点 id（= 注册表 SETTINGS_ITEMS.id）：搜索命中定位与临时高亮靠它查 */
  settingId: string;
  /** 提交生效后的回调（父组件刷新预览） */
  onApplied: () => void;
}) {
  const { t } = useTranslation();
  const [draft, setDraft] = useState(() => readStoredFonts()[slot]);

  /** 仅当净化后的值与已生效值不同才落盘（失焦但内容无变化时保持安静） */
  function commit(candidate: string) {
    const applied = readStoredFonts()[slot];
    const sanitized = sanitizeFontList(candidate);
    if (sanitized === applied) return;
    setDraft(storeFonts({ ...readStoredFonts(), [slot]: candidate })[slot]);
    onApplied();
  }

  return (
    <Form.Item label={label} help={hint}>
      {/* 字体槽是整行输入（不参与宽度三档，见 WIDTH_EXEMPT_ITEM_IDS）：只补锚点 */}
      <div className="setting-anchor" data-setting-id={settingId}>
        <Input
          value={draft}
          placeholder={DEFAULT_FONT_LEADS[slot]}
          onChange={(e) => setDraft(e.target.value)}
          onPressEnter={() => commit(draft)}
          onBlur={() => commit(draft)}
          suffix={
            <Button
              type="text" size="small" title={t("settings.fontReset")}
              aria-label={`${t("settings.fontReset")}・${label}`}
              icon={<UndoOutlined />}
              onClick={() => { setDraft(""); commit(""); }}
            />
          }
        />
      </div>
    </Form.Item>
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
      <FontField slot="sans" label={t("settings.uiFont")} hint={t("settings.fontHint")} settingId="ui.font_sans" onApplied={() => setApplied(sanitizeStored())} />
      <FontField slot="mono" label={t("settings.monoFont")} hint={t("settings.fontHint")} settingId="ui.font_mono" onApplied={() => setApplied(sanitizeStored())} />
      <Form.Item>
        <div className="font-preview">
          <div className="font-preview-row" style={{ fontFamily: previewFontFamily(applied.sans, "sans") }}>
            {PREVIEW_SAMPLE}
          </div>
          <div className="font-preview-row" style={{ fontFamily: previewFontFamily(applied.mono, "mono") }}>
            {PREVIEW_SAMPLE}
          </div>
        </div>
      </Form.Item>
    </>
  );
}

function sanitizeStored(): FontPreferences {
  const stored = readStoredFonts();
  return { sans: sanitizeFontList(stored.sans), mono: sanitizeFontList(stored.mono) };
}
