// 设置弹窗「外观」页签：主题三档（跟随系统/亮色/暗色，即选即生效）+ 自定义字体双槽
// （[docs/custom-font-and-titlebar](../../../../docs/custom-font-and-titlebar.md)，Enter/失焦提交）。
// 两者均为 localStorage 持久化的纯 UI 偏好，绕过设置保存按钮。
import { useState } from "react";
import { Button, Form, Input, Select } from "antd";
import { UndoOutlined } from "@ant-design/icons";
import { useTranslation } from "react-i18next";
import { useUi, type ThemePref } from "../../stores/ui";
import {
  DEFAULT_FONT_LEADS, previewFontFamily, readStoredFonts, sanitizeFontList, storeFonts,
  type FontPreferences, type FontSlot,
} from "../../utils/fonts";

const PREVIEW_SAMPLE = "Aa Bb 0123 — The quick brown fox · 中文字体预览 0O1lI";

/** 外观页签容器：主题三档选择 + 字体双槽（本组件持有唯一 Form，FontSettings 只渲染表单项）。 */
export function AppearanceSettings() {
  const { t } = useTranslation();
  const theme = useUi((s) => s.theme);

  return (
    <Form layout="vertical">
      <Form.Item label={t("settings.theme")} extra={t("settings.themeHint")}>
        <Select
          size="small"
          style={{ width: 160 }}
          value={theme}
          onChange={(v) => useUi.getState().setTheme(v as ThemePref)}
          options={[
            { label: t("settings.themeSystem"), value: "system" },
            { label: t("settings.themeLight"), value: "light" },
            { label: t("settings.themeDark"), value: "dark" },
          ]}
        />
      </Form.Item>
      <FontSettings />
    </Form>
  );
}

/** 单个字体槽输入项：Enter/失焦提交、值有变化才落盘，附一键恢复默认按钮。 */
function FontField({ slot, label, hint, onApplied }: {
  slot: FontSlot;
  label: string;
  hint: string;
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
      <FontField slot="sans" label={t("settings.uiFont")} hint={t("settings.fontHint")} onApplied={() => setApplied(sanitizeStored())} />
      <FontField slot="mono" label={t("settings.monoFont")} hint={t("settings.fontHint")} onApplied={() => setApplied(sanitizeStored())} />
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
