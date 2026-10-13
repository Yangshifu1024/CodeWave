// Appearance preferences apply immediately. Fonts and sizes share one atomic
// backend patch; localStorage is only the first-frame preview cache.
import { useEffect, useRef, useState } from "react";
import { Button, Input, Radio, Select } from "antd";
import { UndoOutlined } from "@ant-design/icons";
import { useTranslation } from "react-i18next";
import { SettingsForm, SettingsFormItem, SettingsSection } from "./settings/SettingsTheme";
import type { ConfigState } from "../../ipc/types";
import { ipc } from "../../ipc/client";
import { i18n } from "../../i18n";
import { configErrorMessage, reconcileConfigOperation, useSettings } from "../../stores/settings";
import { useUi, type ThemePref } from "../../stores/ui";
import {
  commitFontSizeSlot, commitFontSlot, DEFAULT_FONT_LEADS, DEFAULT_FONT_SIZE_LEVEL, FONT_SIZE_LEVELS,
  levelToPx, previewFontFamily, readStoredFonts, sanitizeFontList, useFontPreferences,
  type FontPreferences, FontSizeLevel, type FontSlot,
} from "../../utils/fonts";

const PREVIEW_SAMPLE = "Aa Bb 0123 — The quick brown fox · 中文字体预览 0O1lI";

/** 界面页容器：主题三档 + 三组字体（本组件持有唯一 Form，FontSettings 只渲染表单项）+ 界面语言。
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
      {/* Each family is followed by its independent size and preview. */}
      <SettingsSection title={t("settings.fontGroup")}>
        <FontSettings />
      </SettingsSection>
    </SettingsForm>
  );
}

/** 输入停止多久后自动提交（敲完就走也不丢；回车/失焦立即提交） */
const COMMIT_IDLE_MS = 600;

// Serialize all slots together: each request contains the complete preference set.
// This prevents an earlier slow request from overwriting a later edit.
let fontPersistQueue: Promise<void> = Promise.resolve();

/** Local font preview remains immediate; the receipt updates saved config only. */
function persistFontPrefsToBackend(prefs: FontPreferences): Promise<"saved" | "failed" | "blocked"> {
  const operationId = crypto.randomUUID();
  const attempt = fontPersistQueue.then(async () => {
    try {
      const sizeSans = levelToPx(prefs.fontSizeSans);
      const sizeContent = levelToPx(prefs.fontSizeContent);
      const sizeMono = levelToPx(prefs.fontSizeMono);
      const result = await ipc.setFontPrefs(prefs.sans, prefs.content, prefs.mono, sizeSans, sizeContent, sizeMono, operationId)
        .catch((error: unknown) => reconcileConfigOperation(operationId, error));
      useSettings.getState().adoptSnapshot(result);
      return "saved" as const;
    } catch (error) {
      useUi.getState().notify(i18n.t("settings.title"),
        `${i18n.t("settings.fontSaveFailed")} ${configErrorMessage(error)}`);
      const code = error && typeof error === "object" && "code" in error ? error.code : undefined;
      return code === "OperationHistoryFull" || code === "ConfigUnreadable" ? "blocked" as const : "failed" as const;
    }
  });
  fontPersistQueue = attempt.then(() => undefined);
  return attempt;
}

/** 单个字体槽输入项：回车 / 失焦 / 停手 / 卸载四处都提交，值有变化才落盘，附一键恢复默认按钮。
 *
 * 为什么要四处提交（2026-09-19 修缺陷）：原来只有「回车或失焦」两个时机，而提交前有个
 * 「没变化就 return」的短路——输入后没碰回车、也没点别处（直接关设置页/关窗口），就等于什么都没发生，
 * 界面看上去就是「字体没保存」。 */
function FontField({ slot, label, hint, settingId, appliedFont, appliedLevel }: {
  slot: FontSlot;
  label: string;
  hint: string;
  /** 设置项锚点 id（= 注册表 SETTINGS_ITEMS.id）：搜索命中定位与临时高亮靠它查 */
  settingId: string;
  appliedFont: string;
  appliedLevel: FontSizeLevel;
}) {
  const { t } = useTranslation();
  const [draft, setDraft] = useState(() => readStoredFonts()[slot]);
  const [saveFailed, setSaveFailed] = useState(false);
  const [retrying, setRetrying] = useState(false);
  const saveGeneration = useRef(0);

  function savePreferences(prefs: FontPreferences) {
    const generation = ++saveGeneration.current;
    setRetrying(true);
    void persistFontPrefsToBackend(prefs).then((saved) => {
      if (generation !== saveGeneration.current) return;
      setSaveFailed(saved === "failed");
      setRetrying(false);
    });
  }
  /** 用户是否真的改过输入框：**没改过的空提交一律不写**（否则一次误触发就把已有偏好抹掉） */
  const edited = useRef(false);
  /** 输入法组合态：组合期间不提交（半成品不该落盘） */
  const composing = useRef(false);
  const idleTimer = useRef<number | null>(null);
  /** 卸载兑底提交要拿到最新草稿（闭包里的 draft 会过期） */
  const draftRef = useRef(draft);
  draftRef.current = draft;

  // Config can finish loading while settings is already open. Refresh untouched
  // inputs, while preserving an in-progress edit or IME composition.
  useEffect(() => {
    if (edited.current || composing.current) return;
    draftRef.current = appliedFont;
    setDraft(appliedFont);
  }, [appliedFont]);

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
    savePreferences(next);
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
        {saveFailed && <Button size="small" loading={retrying}
          onClick={() => savePreferences(readStoredFonts())}
          aria-label={`${t("common.retry")}・${label}`}>
          {t("common.retry")}
        </Button>}
        <div className="font-preview-row" style={{
          fontFamily: previewFontFamily(appliedFont, slot),
          fontSize: `${levelToPx(appliedLevel)}px`,
        }}>
          {PREVIEW_SAMPLE}
        </div>
      </div>
    </SettingsFormItem>
  );
}

/** Three independent groups: interface, content, and code. */
function FontSettings() {
  const { t } = useTranslation();
  const applied = useFontPreferences();

  return (
    <>
      {/* 界面字体（sans）—— 字体族输入框 */}
      <FontField
        slot="sans"
        label={t("settings.uiFont")}
        hint={t("settings.fontHint")}
        settingId="ui.font_sans"
        appliedFont={applied.sans}
        appliedLevel={applied.fontSizeSans}
      />
      {/* 界面字号（sans）—— 紧贴界面字体下方的字号 Select，v2 布局 */}
      <FontSizeField
        slot="sans"
        label={t("settings.uiFontSize")}
        hint={t("settings.fontSizeHint")}
        settingId="ui.font_size_sans"
        appliedLevel={applied.fontSizeSans}
        appliedFont={applied.sans}
      />
      <FontField
        slot="content"
        label={t("settings.contentFont")}
        hint={t("settings.fontHint")}
        settingId="ui.font_content"
        appliedFont={applied.content}
        appliedLevel={applied.fontSizeContent}
      />
      <FontSizeField
        slot="content"
        label={t("settings.contentFontSize")}
        hint={t("settings.fontSizeHint")}
        settingId="ui.font_size_content"
        appliedLevel={applied.fontSizeContent}
        appliedFont={applied.content}
      />
      {/* 代码字体（mono）—— 字体族输入框 */}
      <FontField
        slot="mono"
        label={t("settings.monoFont")}
        hint={t("settings.fontHint")}
        settingId="ui.font_mono"
        appliedFont={applied.mono}
        appliedLevel={applied.fontSizeMono}
      />
      {/* 代码字号（mono）—— 紧贴代码字体下方的字号 Select */}
      <FontSizeField
        slot="mono"
        label={t("settings.monoFontSize")}
        hint={t("settings.fontSizeHint")}
        settingId="ui.font_size_mono"
        appliedLevel={applied.fontSizeMono}
        appliedFont={applied.mono}
      />
    </>
  );
}

/** Independent size slot: integer 12–20px choices, serialized with all families.
 *  改即提交：不需要“停手 / 回车 / 失焦”三处提交（控件本身是枚举选择、不存在部分输入态）。 */
function FontSizeField({ slot, label, hint, settingId, appliedLevel, appliedFont }: {
  slot: FontSlot;
  label: string;
  hint: string;
  settingId: string;
  appliedLevel: FontSizeLevel;
  appliedFont: string;
}) {
  const { t } = useTranslation();
  const [saveFailed, setSaveFailed] = useState(false);
  const [retrying, setRetrying] = useState(false);
  const saveGeneration = useRef(0);

  function commit(next: FontPreferences) {
    const generation = ++saveGeneration.current;
    setRetrying(true);
    void persistFontPrefsToBackend(next).then((saved) => {
      if (generation !== saveGeneration.current) return;
      setSaveFailed(saved === "failed");
      setRetrying(false);
    });
  }

  return (
    <SettingsFormItem className="settings-font-row" label={label} settingDescription={hint}>
      <div className="setting-anchor settings-font-size-control" data-setting-id={settingId}>
        <Select<FontSizeLevel>
          className="settings-font-size-select"
          value={appliedLevel}
          onChange={(v) => {
            const next = commitFontSizeSlot(slot, v);
            commit(next);
          }}
          options={FONT_SIZE_LEVELS.map((lvl) => ({ value: lvl, label: `${lvl}px` }))}
          suffixIcon={
            <Button
              type="text"
              size="small"
              title={t("settings.fontReset")}
              aria-label={`${t("settings.fontReset")}・${label}`}
              icon={<UndoOutlined />}
              onClick={(e) => {
                e.stopPropagation();
                const next = commitFontSizeSlot(slot, DEFAULT_FONT_SIZE_LEVEL[slot]);
                commit(next);
              }}
            />
          }
        />
        {saveFailed && (
          <Button
            size="small"
            loading={retrying}
            onClick={() => {
              const stored = readStoredFonts();
              commit(stored);
            }}
            aria-label={`${t("common.retry")}・${label}`}
          >
            {t("common.retry")}
          </Button>
        )}
        {/* 该槽预览样本：字号本档、字体族取已生效值 */}
        <div
          className="font-preview-row"
          style={{
            fontFamily: previewFontFamily(appliedFont, slot),
            fontSize: `${levelToPx(appliedLevel)}px`,
          }}
        >
          {PREVIEW_SAMPLE}
        </div>
      </div>
    </SettingsFormItem>
  );
}
