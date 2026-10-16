import { useEffect, useRef, useState } from "react";
import { Alert, App, Button, Empty, Space, Tag, Typography } from "antd";
import { useTranslation } from "react-i18next";
import { ipc } from "../../ipc/client";
import type { InstructionInspection, InstructionResource, InstructionReviewAction } from "../../ipc/types";
import { SettingsSection } from "./settings/SettingsTheme";

function ResourceApproval({ resource, sessionId, refresh }: { resource: InstructionResource; sessionId: string | null; refresh: () => Promise<void> }) {
  const { t } = useTranslation();
  const { modal, message } = App.useApp();
  const owner = `${sessionId ?? ""}\u0000${resource.id}\u0000${resource.origin.version}`;
  const identity = useRef(owner);
  identity.current = owner;
  const mounted = useRef(true);
  const pending = useRef(false);
  const close = useRef<(() => void) | null>(null);
  const [busy, setBusy] = useState(false);
  const [operation, setOperation] = useState<{ owner: string; id: string } | null>(null);
  useEffect(() => {
    mounted.current = true;
    return () => { mounted.current = false; close.current?.(); };
  }, [owner]);
  const unresolved = operation?.owner === owner ? operation : null;
  const discard = (id: string) => { void ipc.discardInstructionReview(id).catch((error) => console.warn("Could not discard resource review", String(error))); };
  async function change(action: InstructionReviewAction) {
    if (pending.current || unresolved) return;
    pending.current = true;
    setBusy(true);
    const requestOwner = owner;
    const current = () => mounted.current && identity.current === requestOwner;
    let id: string | null = null;
    let submitted = false;
    let received = false;
    try {
      const review = await ipc.prepareInstructionReview(sessionId, resource.id, resource.origin.version, action);
      id = review.id;
      if (!current()) return;
      const approved = await new Promise<boolean>((resolve) => {
        let settled = false;
        const finish = (answer: boolean) => { if (!settled) { settled = true; close.current = null; resolve(answer); } };
        const dialog = modal.confirm({
          title: review.action === "revoke" ? t("settings.resourceRevoke") : t("settings.resourceReview"),
          width: 760,
          content: <Space orientation="vertical">
            <Typography.Text strong>{review.name}</Typography.Text>
            <Typography.Text code>{review.origin.identity}</Typography.Text>
            <Typography.Text code>{review.origin.version}</Typography.Text>
            <Typography.Paragraph>{t("settings.resourceReviewHint")}</Typography.Paragraph>
            <Typography.Paragraph style={{ maxHeight: 360, overflow: "auto", whiteSpace: "pre-wrap" }}>{review.text}</Typography.Paragraph>
          </Space>,
          okText: review.action === "revoke" ? t("settings.resourceRevoke") : review.action === "remember" ? t("settings.resourceRemember") : t("settings.resourceOnce"),
          cancelText: t("common.cancel"),
          onOk: () => finish(true), onCancel: () => finish(false), afterClose: () => finish(false),
        });
        close.current = () => { finish(false); dialog.destroy(); };
      });
      if (!approved || !current()) return;
      submitted = true;
      const receipt = await ipc.confirmInstructionReview(sessionId, id);
      received = true;
      if (!current()) return;
      if (receipt.resolved) { setOperation(null); discard(receipt.operation_id); }
      else setOperation({ owner: requestOwner, id: receipt.operation_id });
      if (!receipt.confirmed) message.warning(receipt.error || t("settings.resourceUnknown"));
      await refresh();
    } catch (error) {
      if (current()) {
        if (submitted && !received && id) setOperation({ owner: requestOwner, id });
        message.error(String(error));
        await refresh().catch((error) => { message.error(String(error)); });
      }
    } finally {
      if (id && !submitted) discard(id);
      pending.current = false;
      if (current()) setBusy(false);
    }
  }
  async function inspect() {
    if (pending.current || !unresolved) return;
    pending.current = true; setBusy(true);
    const requestOwner = owner;
    try {
      const receipt = await ipc.instructionReviewOperation(sessionId, unresolved.id);
      if (!mounted.current || identity.current !== requestOwner) return;
      if (receipt.resolved) { setOperation(null); discard(receipt.operation_id); }
      if (!receipt.confirmed) message.warning(receipt.error || t("settings.resourceUnknown"));
      await refresh();
    } catch (error) { if (mounted.current && identity.current === requestOwner) message.error(String(error)); }
    finally { pending.current = false; if (mounted.current && identity.current === requestOwner) setBusy(false); }
  }
  if (!resource.available) return null;
  if (resource.origin.source === "builtin") return <Tag>{t("common.builtin")}</Tag>;
  return <Space wrap>
    <Tag>{resource.status.remembered ? t("settings.resourceRemembered") : resource.status.trusted ? t("settings.resourceAllowed") : t("settings.resourceUntrusted")}</Tag>
    {!resource.status.trusted && <Button loading={busy} disabled={!!unresolved || !sessionId} onClick={() => void change("once")}>{t("settings.resourceOnce")}</Button>}
    {!resource.status.remembered && <Button loading={busy} disabled={!!unresolved} onClick={() => void change("remember")}>{t("settings.resourceRemember")}</Button>}
    {(resource.status.trusted || resource.status.remembered) && <Button danger loading={busy} disabled={!!unresolved} onClick={() => void change("revoke")}>{t("settings.resourceRevoke")}</Button>}
    {unresolved && <Button loading={busy} onClick={() => void inspect()}>{t("settings.resourceInspect")}</Button>}
  </Space>;
}

export default function InstructionResourcesPanel({ sessionId }: { sessionId: string | null }) {
  const { t } = useTranslation();
  const [inspection, setInspection] = useState<InstructionInspection>({ resources: [], issues: [] });
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const owner = useRef(sessionId);
  owner.current = sessionId;
  const mounted = useRef(false);
  const generation = useRef(0);
  async function refresh() {
    const request = ++generation.current;
    const scope = sessionId;
    const current = () => mounted.current && owner.current === scope && generation.current === request;
    setBusy(true);
    try {
      const result = await ipc.inspectInstructionResources(scope);
      if (!result || !Array.isArray(result.resources) || !Array.isArray(result.issues)) throw new Error("Invalid instruction resource response");
      if (current()) { setInspection(result); setError(null); }
    } catch (error) { if (current()) setError(String(error)); }
    finally { if (current()) setBusy(false); }
  }
  useEffect(() => {
    mounted.current = true; setInspection({ resources: [], issues: [] });
    void refresh();
    return () => { mounted.current = false; generation.current += 1; };
    // A new session has a different temporary authorization owner.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [sessionId]);
  return <SettingsSection title={t("settings.resourceTitle")} extra={<Button loading={busy} onClick={() => void refresh()}>{t("settings.resourceRefresh")}</Button>}>
    <Typography.Paragraph>{t("settings.resourceHint")}</Typography.Paragraph>
    {error && <Alert type="error" title={error} />}
    {inspection.issues.map((issue, index) => <Alert type="warning" key={index} title={issue} />)}
    {!busy && !error && !inspection.resources.length && <Empty />}
    <Space orientation="vertical" style={{ width: "100%" }}>
      {inspection.resources.filter((resource) => resource.origin.source !== "builtin").map((resource) => <Space key={`${sessionId}:${resource.id}:${resource.origin.version}`} orientation="vertical">
        <Typography.Text strong>{resource.name} · {resource.kind}</Typography.Text>
        <Typography.Text code>{resource.origin.identity}</Typography.Text>
        <Typography.Text type="secondary">{resource.origin.version}</Typography.Text>
        {resource.status.error && <Alert type="warning" title={resource.status.error} />}
        {!!resource.active_versions.length && <Alert type="warning" title={t("settings.resourceActive")} description={resource.active_versions.map((usage) => usage.version).join(", ")} />}
        <ResourceApproval resource={resource} sessionId={sessionId} refresh={refresh} />
      </Space>)}
    </Space>
  </SettingsSection>;
}
