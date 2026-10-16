import { useEffect, useRef, useState } from "react";
import { Alert, App, Button, Collapse, Empty, Popconfirm, Space, Switch, Tag, Typography } from "antd";
import { DeleteOutlined } from "@ant-design/icons";
import { useTranslation } from "react-i18next";
import { ipc } from "../../ipc/client";
import type { ResourceOrigin, SkillInspection, SkillMeta } from "../../ipc/types";
import { originLabel } from "../../utils/skills";
import { SettingsSection } from "./settings/SettingsTheme";

export function ResourceDetails({ resource }: { resource: ResourceOrigin }) {
  const { t } = useTranslation();
  const labels: Record<ResourceOrigin["source"], string> = {
    builtin: t("settings.resourceSource_builtin"),
    user_claude: t("settings.resourceSource_user_claude"),
    user_agents: t("settings.resourceSource_user_agents"),
    workspace_claude: t("settings.resourceSource_workspace_claude"),
    workspace_agents: t("settings.resourceSource_workspace_agents"),
    global_managed: t("settings.resourceSource_global_managed"),
    project_managed: t("settings.resourceSource_project_managed"),
  };
  return <Space orientation="vertical" size={0} className="resource-provenance">
    <Typography.Text type="secondary">{labels[resource.source]}</Typography.Text>
    <Typography.Text type="secondary">{resource.identity}</Typography.Text>
    <Typography.Text type="secondary" title={resource.version}>{t("settings.resourceVersion", { version: resource.version })}</Typography.Text>
  </Space>;
}

/** Inspection keeps disabled and shadowed candidates separate from the enabled composer index.
 * Toggling stays in the page draft; deleting re-inspects the backend's actual fallback. */
export default function SkillsPanel({ sessionId, disabled, savedDisabled, onToggle }: {
  sessionId: string | null;
  disabled: string[];
  savedDisabled: string[];
  onToggle: (name: string, disabled: boolean) => void;
}) {
  const { t } = useTranslation();
  const { message } = App.useApp();
  const [inspection, setInspection] = useState<SkillInspection>({ entries: [], issues: [] });
  const [inspectionScope, setInspectionScope] = useState(sessionId);
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(true);
  const [deleting, setDeleting] = useState<string | null>(null);
  const owner = useRef(sessionId);
  owner.current = sessionId;
  const generation = useRef(0);
  const deleteSequence = useRef(0);
  const mounted = useRef(false);
  const savedIdentity = JSON.stringify(savedDisabled);
  const previousBaseline = useRef({ sessionId, savedIdentity });

  useEffect(() => {
    mounted.current = true;
    setInspection({ entries: [], issues: [] });
    setInspectionScope(sessionId);
    setDeleting(null);
    void inspect(false);
    return () => { mounted.current = false; generation.current += 1; deleteSequence.current += 1; };
    // The session is the inspection scope; draft changes must never reload or persist it.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [sessionId]);

  useEffect(() => {
    const previous = previousBaseline.current;
    previousBaseline.current = { sessionId, savedIdentity };
    if (previous.sessionId === sessionId && previous.savedIdentity !== savedIdentity) void inspect(false);
    // Saved configuration can change actual availability; unsaved toggles never trigger a read.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [sessionId, savedIdentity]);

  async function inspect(reload: boolean, announce = false) {
    const request = ++generation.current;
    const scope = sessionId;
    const current = () => mounted.current && owner.current === scope && generation.current === request;
    setBusy(true);
    setError(null);
    try {
      const result = await ipc.inspectSkills(scope, reload);
      if (!current()) return;
      setInspection(result);
      setInspectionScope(scope);
      if (announce) message.success(t("settings.skillsReloaded", { n: result.entries.length }));
    } catch (e) {
      if (current()) setError(String(e).replace(/^Error[:\s]*/i, ""));
    } finally {
      if (current()) setBusy(false);
    }
  }

  async function remove(meta: SkillMeta) {
    if (!mounted.current || owner.current !== sessionId || !meta.resource || !meta.deletable || busy || deleting) return;
    const scope = sessionId;
    const operation = ++deleteSequence.current;
    const current = () => mounted.current && owner.current === scope && deleteSequence.current === operation;
    setDeleting(meta.name);
    try {
      await ipc.deleteSkill(scope, meta.name, meta.resource.identity, meta.resource.version);
      if (!current()) return;
      message.success(t("settings.deleteSkillSuccess", { name: meta.name }));
      await inspect(true);
    } catch (e) {
      if (current()) message.error(`${t("settings.deleteSkillFailed")}\n${String(e).replace(/^Error[:\s]*/i, "")}`);
    } finally {
      if (mounted.current && owner.current === scope && deleteSequence.current === operation) setDeleting(null);
    }
  }

  const visible = inspectionScope === sessionId ? inspection : { entries: [], issues: [] };
  const visibleError = inspectionScope === sessionId ? error : null;
  const loading = busy || inspectionScope !== sessionId;
  return <SettingsSection title={t("settings.skillsInstalled")} extra={
    <Button loading={loading} disabled={!!deleting} onClick={() => void inspect(true, true)}>{t("settings.reloadSkills")}</Button>
  }>
    <div className="setting-anchor" data-setting-id="disabled_skills" aria-busy={loading}>
      <p className="settings-section-intro">{t("settings.skillsHint")}</p>
      {visibleError && <Alert type="error" showIcon title={t("settings.skillsReloadFailed")} description={visibleError} />}
      {visible.issues.map((issue, index) => <Alert key={`${issue.path}:${index}`} type="warning" showIcon title={issue.path} description={issue.message} />)}
      {!loading && !visibleError && visible.entries.length === 0 && <Empty description={t("settings.skillsEmpty")} className="settings-list-empty" />}
      {visible.entries.map((entry) => {
        const selected = entry.candidates.find((candidate) => candidate.selected)?.meta;
        const shadows = entry.candidates.filter((candidate) => !candidate.selected);
        const isDisabled = disabled.includes(entry.name);
        const saved = savedDisabled.includes(entry.name);
        return <div className="skill-row" key={`${sessionId}:${entry.name}`}>
          <div className="skill-info">
            <div className="skill-info-head">
              <strong className="skill-name">{entry.name}</strong>
              {selected && <span className="skill-origin" title={selected.origin}>{selected.origin === "<builtin>" ? t("common.builtin") : originLabel(selected.origin)}</span>}
              <Tag>{t(entry.disabled ? "settings.skillRunningDisabled" : "settings.skillRunningEnabled")}</Tag>
              {saved !== isDisabled && <Tag>{t(isDisabled ? "settings.skillDraftDisabled" : "settings.skillDraftEnabled")}</Tag>}
            </div>
            {selected?.description && <div className="skill-description">{selected.description}</div>}
            {selected?.whenToUse && <div className="skill-when">{t("settings.skillWhen")}{selected.whenToUse}</div>}
            {selected?.resource && <ResourceDetails resource={selected.resource} />}
            {!selected && <Typography.Text type="secondary">{t("settings.skillMissing")}</Typography.Text>}
            {shadows.length > 0 && <Collapse ghost items={[{
              key: "sources", label: t("settings.skillShadowSources", { n: shadows.length }),
              children: <Space orientation="vertical">{shadows.map(({ meta }, index) => <div key={`${meta.origin}:${index}`}>
                <Typography.Text>{meta.description || meta.name}</Typography.Text>
                {meta.resource ? <ResourceDetails resource={meta.resource} /> : <Typography.Text type="secondary">{meta.origin}</Typography.Text>}
              </div>)}</Space>,
            }]} />}
          </div>
          <div className="skill-actions">
            <Switch aria-label={t("settings.skillToggle", { name: entry.name })} checked={!isDisabled} onChange={(value) => onToggle(entry.name, !value)} />
            {selected?.deletable && <Popconfirm
              title={t("settings.deleteSkillConfirm", { name: entry.name })}
              description={<Space orientation="vertical"><span>{t("settings.deleteSkillDirectory")}</span><span>{selected.origin}</span></Space>}
              disabled={!selected.resource || busy || !!deleting}
              okButtonProps={{ danger: true, loading: deleting === entry.name }}
              onConfirm={() => remove(selected)}
            ><Button type="text" danger disabled={!selected.resource || busy || !!deleting} aria-label={t("settings.deleteSkill")} icon={<DeleteOutlined />}>{t("settings.deleteSkill")}</Button></Popconfirm>}
          </div>
        </div>;
      })}
    </div>
  </SettingsSection>;
}
