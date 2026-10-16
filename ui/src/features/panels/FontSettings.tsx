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
import OutputDisplaySelect from "../tools/OutputDisplaySelect";
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
      <SettingsSection title={t("settings.outputDisplay")}>
        <SettingsFormItem label={t("settings.outputDisplay")} settingDescription={t("settings.outputDisplayHint")}>
          <div className="setting-anchor w-narrow" data-setting-id="ui.output_display">
            <OutputDisplaySelect />
          </div>
        </SettingsFormItem>
      </SettingsSection>
      {/* 三槽各占一行（界面 / 内容 / 代码），行内是「字体族输入 + 字号下拉」加该槽预览。
          共性说明只在分段标题右侧说一次，见 fontHint。 */}
      <SettingsSection title={t("settings.fontGroup")} extra={t("settings.fontHint")}>
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

/** Three independent groups: interface, content, and code. */
function FontSettings() {
  const { t } = useTranslation();
  const applied = useFontPreferences();

  return (
    <>
      <FontSlotRow
        slot="sans"
        label={t("settings.fontSlotSans")}
        familyLabel={t("settings.uiFont")}
        sizeLabel={t("settings.uiFontSize")}
        appliedFont={applied.sans}
        appliedLevel={applied.fontSizeSans}
      />
      <FontSlotRow
        slot="content"
        label={t("settings.fontSlotContent")}
        familyLabel={t("settings.contentFont")}
        sizeLabel={t("settings.contentFontSize")}
        appliedFont={applied.content}
        appliedLevel={applied.fontSizeContent}
      />
      <FontSlotRow
        slot="mono"
        label={t("settings.fontSlotMono")}
        familyLabel={t("settings.monoFont")}
        sizeLabel={t("settings.monoFontSize")}
        appliedFont={applied.mono}
        appliedLevel={applied.fontSizeMono}
      />
    </>
  );
}

/** 单槽合并行：左侧短标签，右上行内「字体族 + 字号 + 行末重置」，下方一行预览。
 *
 *  为什么一槽只占一行（而不是字体族、字号各一行）：同槽两者原本各带一个预览行，而两个预览样本
 *  完全相同（同字体族 + 同字号），纯属重复；控件同属一槽，合并后预览与控件也归到同一行，行数 6 → 3。
 *  两个控件各自保留独立的锚点（ui.font_<slot> / ui.font_size_<slot>），设置搜索仍能分别定位。
 *
 *  重置只有一个按钮（行末）：字体族与字号同属一槽，分开两个按钮反而让人猜哪个管哪个。
 *  点击后本槽的字体族清空（回系统默认）、字号回默认档，一次提交写全六字段。 */
function FontSlotRow({ slot, label, familyLabel, sizeLabel, appliedFont, appliedLevel }: {
  slot: FontSlot;
  /** 行标签：只说清是哪一槽（界面 / 内容 / 代码） */
  label: string;
  /** 字体项的无障碍名（「界面字体」）：与所在行的短标签区分 */
  familyLabel: string;
  /** 字号项的无障碍名（「界面字号」）：与所在行的短标签区分 */
  sizeLabel: string;
  appliedFont: string;
  appliedLevel: FontSizeLevel;
}) {
  const { t } = useTranslation();
  const [saveFailed, setSaveFailed] = useState(false);
  const [retrying, setRetrying] = useState(false);
  const saveGeneration = useRef(0);
  /** 行末重置按钮的无障碍名：说清它一次清两样（字体族 + 字号），避免与旧的单一重置混淆 */
  const resetLabel = `${t("settings.fontReset")}・${familyLabel} + ${sizeLabel}`;

  function commit(next: FontPreferences) {
    const generation = ++saveGeneration.current;
    setRetrying(true);
    void persistFontPrefsToBackend(next).then((saved) => {
      if (generation !== saveGeneration.current) return;
      setSaveFailed(saved === "failed");
      setRetrying(false);
    });
  }

  /** 整槽重置：字体族清空 + 字号回默认档，只提交一次**。
   *  commitFontSlot / commitFontSizeSlot 都基于 readStoredFonts() 叠加（各自返回全量六字段），
   *  两者串行执行后只提交最终结果：既保留各自的净化与去重逻辑，也不会打两次请求。
   *  注意不能写成提交两次——那会在同一个串行队列里连发两轮，界面无必要地闪两次保存态。 */
  function resetSlot() {
    commitFontSlot(slot, "");
    commit(commitFontSizeSlot(slot, DEFAULT_FONT_SIZE_LEVEL[slot]));
  }

  return (
    <SettingsFormItem className="settings-font-row" label={label}>
      <div className="settings-font-pair">
        <FontFamilyControl
          slot={slot}
          label={familyLabel}
          settingId={`ui.font_${slot}`}
          appliedFont={appliedFont}
        />
        <FontSizeControl
          slot={slot}
          label={sizeLabel}
          settingId={`ui.font_size_${slot}`}
          appliedLevel={appliedLevel}
        />
        <Button
          className="settings-font-reset"
          icon={<UndoOutlined />}
          title={t("settings.fontReset")}
          aria-label={resetLabel}
          onClick={() => resetSlot()}
        />
        {saveFailed && (
          <Button
            size="small"
            loading={retrying}
            onClick={() => commit(readStoredFonts())}
            aria-label={`${t("common.retry")}・${resetLabel}`}
          >
            {t("common.retry")}
          </Button>
        )}
        {/* 该槽预览样本：字体族与字号都取本槽已生效值 */}
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

/** 单槽字体族输入控件：回车 / 失焦 / 停手 / 卸载四处都提交，值有变化才落盘，附一键恢复默认按钮。
 *
 *  为什么要四处提交（2026-09-19 修缺陷）：原来只有「回车或失焦」两个时机，而提交前有个
 *  「没变化就 return」的短路——输入后没碰回车、也没点别处（直接关设置页/关窗口），就等于什么都没发生，
 *  界面看上去就是「字体没保存」。 */
function FontFamilyControl({ slot, label, settingId, appliedFont }: {
  slot: FontSlot;
  label: string;
  /** 设置项锚点 id（= 注册表 SETTINGS_ITEMS.id）：搜索命中定位与临时高亮靠它查 */
  settingId: string;
  appliedFont: string;
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
    /* 字体族与字号同占一行，锚点各挂各的容器；重置入口在行末（FontSlotRow），不在框内 */
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
      />
      {saveFailed && <Button size="small" loading={retrying}
        onClick={() => savePreferences(readStoredFonts())}
        aria-label={`${t("common.retry")}・${label}`}>
        {t("common.retry")}
      </Button>}
    </div>
  );
}

/** Independent size slot: integer 12–20px choices, serialized with all families.
 *  改即提交：不需要“停手 / 回车 / 失焦”三处提交（控件本身是枚举选择、不存在部分输入态）。
 *  重置入口在行末（FontSlotRow），因此这里不挂 size="small" 的折叠箭头按钮：
 *  它会让 Select 比同行 Input 矮一截、两条下边线对不齐。 */
function FontSizeControl({ slot, label, settingId, appliedLevel }: {
  slot: FontSlot;
  label: string;
  /** 设置项锚点 id（= 注册表 SETTINGS_ITEMS.id）：搜索命中定位与临时高亮靠它查 */
  settingId: string;
  appliedLevel: FontSizeLevel;
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
    <div className="setting-anchor settings-font-size-control" data-setting-id={settingId}>
      <Select<FontSizeLevel>
        className="settings-font-size-select"
        value={appliedLevel}
        onChange={(v) => {
          const next = commitFontSizeSlot(slot, v);
          commit(next);
        }}
        options={FONT_SIZE_LEVELS.map((lvl) => ({ value: lvl, label: `${lvl}px` }))}
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
    </div>
  );
}
