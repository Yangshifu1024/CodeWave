import type { CSSProperties, ReactNode } from "react";
import { App as AntdApp, ConfigProvider, Form, Typography } from "antd";
import type { FormItemProps, FormProps } from "antd";
import "./settings-theme.css";

/** One source for typography, density, surfaces, and field geometry across every settings page. */
export const SETTINGS_VISUAL = {
  contentWidth: 960,
  pageGutter: 32,
  pageGutterNarrow: 16,
  pageTitleSize: 24,
  sectionTitleSize: 16,
  fieldTitleSize: 14,
  fieldDescriptionSize: 14,
  metadataSize: 12,
  sectionGap: 32,
  rowGap: 24,
  rowPaddingBlock: 10,
  rowPaddingInline: 20,
  controlWidth: { narrow: 180, mid: 240, wide: 360 },
} as const;

const FORM_LABEL_COL = { span: 15 };
const FORM_CONTROL_COL = { span: 9 };
const FORM_CONTROL_ONLY_COL = { span: 24 };
const SETTINGS_STYLE = {
  "--settings-content-width": `${SETTINGS_VISUAL.contentWidth}px`,
  "--settings-page-gutter": `${SETTINGS_VISUAL.pageGutter}px`,
  "--settings-page-gutter-narrow": `${SETTINGS_VISUAL.pageGutterNarrow}px`,
  "--settings-page-title-size": `${SETTINGS_VISUAL.pageTitleSize}px`,
  "--settings-section-title-size": `${SETTINGS_VISUAL.sectionTitleSize}px`,
  "--settings-field-title-size": `${SETTINGS_VISUAL.fieldTitleSize}px`,
  "--settings-field-description-size": `${SETTINGS_VISUAL.fieldDescriptionSize}px`,
  "--settings-metadata-size": `${SETTINGS_VISUAL.metadataSize}px`,
  "--settings-section-gap": `${SETTINGS_VISUAL.sectionGap}px`,
  "--settings-row-gap": `${SETTINGS_VISUAL.rowGap}px`,
  "--settings-row-padding-block": `${SETTINGS_VISUAL.rowPaddingBlock}px`,
  "--settings-row-padding-inline": `${SETTINGS_VISUAL.rowPaddingInline}px`,
  "--settings-control-width-narrow": `${SETTINGS_VISUAL.controlWidth.narrow}px`,
  "--settings-control-width-mid": `${SETTINGS_VISUAL.controlWidth.mid}px`,
  "--settings-control-width-wide": `${SETTINGS_VISUAL.controlWidth.wide}px`,
} as CSSProperties;

/** Locally themes settings and all antd overlays while preserving the application's light/dark algorithm. */
export function SettingsThemeProvider({ children }: { children: ReactNode }) {
  return (
    <ConfigProvider
      componentSize="medium"
      theme={{
        token: {
          fontFamily: "var(--ws-font-sans)",
          fontFamilyCode: "var(--ws-font-mono)",
          fontSize: SETTINGS_VISUAL.fieldTitleSize,
          fontSizeSM: SETTINGS_VISUAL.metadataSize,
          lineHeight: 1.5,
          controlHeight: 36,
          borderRadius: 8,
          borderRadiusLG: 12,
        },
        components: {
          Form: { itemMarginBottom: 0, verticalLabelPadding: "0 0 4px" },
          Button: { controlHeight: 34 },
          Card: { bodyPadding: 0, bodyPaddingSM: 0 },
        },
      }}
    >
      <SettingsLocalApp>{children}</SettingsLocalApp>
    </ConfigProvider>
  );
}

/** App belongs inside the local provider so App.useApp-created dialogs inherit the same theme. */
function SettingsLocalApp({ children }: { children: ReactNode }) {
  return (
    <AntdApp>
      <SettingsThemeScope>{children}</SettingsThemeScope>
    </AntdApp>
  );
}

/** Reapplies shared styles and sizing variables inside body-portaled settings dialogs. */
export function SettingsThemeScope({ children, className }: { children: ReactNode; className?: string }) {
  return (
    <div className={["settings-theme-root", className].filter(Boolean).join(" ")} style={SETTINGS_STYLE}>
      {children}
    </div>
  );
}

/** Shared horizontal layout used by settings forms; field values and event handlers remain page-owned. */
type SettingsFormProps = Omit<FormProps, "children"> & { children?: ReactNode };

export function SettingsForm(props: SettingsFormProps) {
  const { className, layout: _layout, labelCol: _labelCol, wrapperCol: _wrapperCol, ...rest } = props;
  return (
    <Form
      {...rest}
      className={["settings-form", className].filter(Boolean).join(" ")}
      layout="horizontal"
      colon={false}
      labelCol={FORM_LABEL_COL}
      wrapperCol={FORM_CONTROL_COL}
    />
  );
}

type SettingsFormItemProps = FormItemProps & { settingDescription?: ReactNode };

/** Renders helper text beside the field label and keeps validation messages beside their control. */
export function SettingsFormItem({
  className,
  label,
  extra,
  help,
  validateStatus,
  settingDescription,
  labelCol: _labelCol,
  wrapperCol: _wrapperCol,
  style: _style,
  ...rest
}: SettingsFormItemProps) {
  const description = settingDescription ?? extra;
  const hasLabel = label !== undefined && label !== null;
  const hasDescription = description !== undefined && description !== null && description !== "";
  const copy = hasLabel || hasDescription ? (
    <span className="settings-row-copy">
      {hasLabel && <span className="settings-row-title">{label}</span>}
      {hasDescription && (
        <Typography.Text className="settings-row-description" type="secondary">
          {description}
        </Typography.Text>
      )}
    </span>
  ) : undefined;

  return (
    <Form.Item
      {...rest}
      className={["settings-row", !copy && "settings-row-full", className].filter(Boolean).join(" ")}
      label={copy}
      colon={false}
      labelCol={copy ? FORM_LABEL_COL : FORM_CONTROL_ONLY_COL}
      wrapperCol={copy ? FORM_CONTROL_COL : FORM_CONTROL_ONLY_COL}
      style={_style}
      extra={undefined}
      help={help}
      validateStatus={validateStatus}
    />
  );
}

/** Section heading and bordered surface used by all settings groups. */
export function SettingsSection({
  title,
  extra,
  children,
  className,
}: {
  title?: ReactNode;
  extra?: ReactNode;
  children: ReactNode;
  className?: string;
}) {
  return (
    <section className={["settings-section", className].filter(Boolean).join(" ")}>
      {title != null && (extra != null ? (
        <div className="settings-section-heading">
          <Typography.Title level={4} className="settings-section-title">{title}</Typography.Title>
          <div className="settings-section-extra">{extra}</div>
        </div>
      ) : (
        <Typography.Title level={4} className="settings-section-title">{title}</Typography.Title>
      ))}
      <div className="settings-section-card">{children}</div>
    </section>
  );
}
