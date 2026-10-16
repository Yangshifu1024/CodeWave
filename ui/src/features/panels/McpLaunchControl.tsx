import { useEffect, useRef, useState } from "react";
import { App, Button, Descriptions, Space, Tooltip, Typography } from "antd";
import { useTranslation } from "react-i18next";
import { ipc } from "../../ipc/client";
import type { McpLaunchStatus, McpScope, McpServerView, McpReviewAction, McpLaunchReviewReceipt } from "../../ipc/types";
import { isSensitiveMcpKey } from "./McpKvTable";

type LaunchConfig = Partial<Pick<McpServerView, "transport" | "command" | "args" | "cwd" | "url" | "env" | "headers">>;

/** Redact URL credentials/query values and recognized sensitive options in the launch summary. */
function endpoint(value: string): string {
  try {
    const url = new URL(value);
    url.username = "";
    url.password = "";
    for (const key of new Set(url.searchParams.keys())) url.searchParams.set(key, "••••");
    url.hash = "";
    return url.toString();
  } catch { return "—"; }
}

function LaunchDetails({ config }: { config: LaunchConfig }) {
  const { t } = useTranslation();
  let hideNext = false;
  const args = (config.args ?? []).map((arg) => {
    if (hideNext) { hideNext = false; return "••••"; }
    const [key] = arg.split("=", 1);
    if (isSensitiveMcpKey(key) && (arg.startsWith("-") || arg.includes("="))) {
      if (arg.includes("=")) return `${key}=••••`;
      hideNext = true;
      return arg;
    }
    return arg.replace(/https?:\/\/[^\s"'<>]+/g, endpoint);
  });
  const stdio = config.transport === "stdio" || (!config.transport && !!config.command);
  return <Descriptions column={1} size="small" items={[
    { key: "transport", label: t("settings.mcpTransport"), children: stdio ? "stdio" : "streamable_http" },
    ...(stdio ? [
      { key: "command", label: t("settings.mcpCommand"), children: <Typography.Text code>{config.command}</Typography.Text> },
      { key: "args", label: t("settings.mcpArgs"), children: <Typography.Text code>{JSON.stringify(args)}</Typography.Text> },
      { key: "cwd", label: "cwd", children: config.cwd || t("settings.mcpLaunchDefaultCwd") },
      { key: "env", label: t("settings.mcpEnv"), children: Object.keys(config.env ?? {}).sort().join(", ") || "—" },
    ] : [
      { key: "url", label: t("settings.mcpUrl"), children: <Typography.Text code>{endpoint(config.url ?? "")}</Typography.Text> },
      { key: "headers", label: t("settings.mcpHeaders"), children: Object.keys(config.headers ?? {}).sort().join(", ") || "—" },
    ]),
  ]} />;
}

/** Closing/changing the owner also closes its outstanding confirmation. */
export function useMcpLaunchConfirmation(owner: string | number) {
  const { modal } = App.useApp();
  const { t } = useTranslation();
  const pending = useRef(new Map<() => void, string | number>());
  const currentOwner = useRef(owner);
  currentOwner.current = owner;
  useEffect(() => () => {
    for (const [close, identity] of pending.current) if (identity === owner) close();
  }, [owner]);
  return (kind: "launch" | "remember" | "probe" | "revoke", name: string, source: string, version: string, config: LaunchConfig) =>
    new Promise<boolean>((resolve) => {
      if (currentOwner.current !== owner) { resolve(false); return; }
      let settled = false;
      const settle = (answer: boolean) => {
        if (settled) return;
        settled = true;
        pending.current.delete(close);
        resolve(answer && currentOwner.current === owner);
      };
      const close = () => { settle(false); dialog.destroy(); };
      const dialog = modal.confirm({
        title: kind === "probe" ? t("settings.mcpConfirmProbe") : kind === "revoke" ? t("settings.mcpConfirmRevoke") : kind === "remember" ? t("settings.mcpConfirmRemember") : t("settings.mcpConfirmLaunch"),
        content: <Space orientation="vertical">
          <Typography.Text>{kind === "revoke" ? t("settings.mcpLaunchRevokeHint") : kind === "probe" ? t("settings.mcpProbeLaunchHint") : kind === "remember" ? t("settings.mcpRememberHint") : t("settings.mcpLaunchLifetime")}</Typography.Text>
          <Typography.Text strong>{name}</Typography.Text>
          <Typography.Text code>{source}</Typography.Text>
          <LaunchDetails config={config} />
          <Typography.Text type="secondary">{t("settings.mcpLaunchVersion", { version: version.slice(0, 12) })}</Typography.Text>
        </Space>,
        okText: kind === "remember" ? t("settings.mcpRememberLaunch") : t(kind === "revoke" ? "settings.mcpRevokeLaunch" : kind === "probe" ? "settings.mcpRunProbe" : "settings.mcpApproveLaunch"),
        cancelText: t("common.cancel"),
        onOk: () => settle(true),
        onCancel: () => settle(false),
        afterClose: () => settle(false),
      });
      pending.current.set(close, owner);
    });
}

export default function McpLaunchControl({ name, scope, sessionId, source, launch, config, disabled, onChanged }: {
  name: string;
  scope: McpScope;
  sessionId: string | null;
  source: string;
  launch: McpLaunchStatus;
  config: LaunchConfig;
  disabled?: boolean;
  onChanged: () => Promise<void>;
}) {
  const { t } = useTranslation();
  const { message } = App.useApp();
  const resourceOwner = `${sessionId ?? ""}\u0000${scope}\u0000${source}\u0000${name}\u0000${launch.version}`;
  const owner = `${resourceOwner}\u0000${launch.revision ?? ""}\u0000${launch.trusted}\u0000${disabled}`;
  const confirm = useMcpLaunchConfirmation(owner);
  const identity = useRef(owner);
  identity.current = owner;
  // Authority refreshes invalidate open confirmations, but must preserve the
  // receipt of an operation that has already been submitted for this resource.
  const resourceIdentity = useRef(resourceOwner);
  resourceIdentity.current = resourceOwner;
  const mounted = useRef(true);
  const pending = useRef(false);
  const [busy, setBusy] = useState(false);
  const [operation, setOperation] = useState<{ id: string; owner: string } | null>(null);
  useEffect(() => {
    mounted.current = true;
    return () => { mounted.current = false; };
  }, []);
  function discard(id: string) {
    void ipc.mcpDiscardLaunchReview(id).catch((error) => console.warn("Could not discard MCP review", String(error)));
  }
  function receive(receipt: McpLaunchReviewReceipt, requestOwner: string) {
    if (receipt.resolved) {
      setOperation(null);
      discard(receipt.operation_id);
    } else setOperation({ id: receipt.operation_id, owner: requestOwner });
    if (!receipt.confirmed) message.warning(receipt.error || t("settings.mcpTrustUnknown"));
  }
  async function changeTrust(action: McpReviewAction) {
    if (pending.current || disabled || operation?.owner === resourceOwner) return;
    pending.current = true;
    setBusy(true);
    const requestOwner = owner;
    const requestResource = resourceOwner;
    let reviewId: string | null = null;
    let submitted = false;
    try {
      if (action === "revoke" && !launch.remembered) {
        if (!await confirm("revoke", name, source, launch.version, config)) return;
        if (!mounted.current || identity.current !== requestOwner) return;
        // Compatibility revocation also withdraws any matching persistent
        // record, and stops this instance even if the keyring is unavailable.
        await ipc.mcpSetLaunchTrust(scope, name, launch.version, false, sessionId ?? undefined);
        if (mounted.current && identity.current === requestOwner) await onChanged();
        return;
      }
      const review = await ipc.mcpPrepareLaunch(scope, name, launch.version, action, sessionId ?? undefined);
      reviewId = review.id;
      if (!mounted.current || identity.current !== requestOwner) return;
      if (!await confirm(action === "once" ? "launch" : action, review.name, review.source, review.version, review.config)) return;
      if (!mounted.current || identity.current !== requestOwner) return;
      submitted = true;
      const receipt = await ipc.mcpConfirmLaunch(scope, name, review.id, sessionId ?? undefined);
      if (mounted.current && resourceIdentity.current === requestResource) {
        receive(receipt, requestResource);
        await onChanged();
      }
    } catch (error) {
      if (mounted.current && resourceIdentity.current === requestResource) {
        if (submitted && reviewId) setOperation({ id: reviewId, owner: requestResource });
        message.error(String(error));
        // A rejected revocation can still have stopped the local process.
        await onChanged().catch((refreshError) => { message.error(String(refreshError)); });
      }
    } finally {
      if (reviewId && !submitted) discard(reviewId);
      pending.current = false;
      if (mounted.current) setBusy(false);
    }
  }
  const unresolved = operation?.owner === resourceOwner ? operation : null;
  async function inspectOperation() {
    if (!unresolved || pending.current) return;
    pending.current = true;
    setBusy(true);
    const requestResource = resourceOwner;
    try {
      const receipt = await ipc.mcpLaunchOperation(scope, name, unresolved.id, sessionId ?? undefined);
      if (mounted.current && resourceIdentity.current === requestResource) {
        receive(receipt, requestResource);
        await onChanged();
      }
    } catch (error) { if (mounted.current && resourceIdentity.current === requestResource) message.error(String(error)); }
    finally { pending.current = false; if (mounted.current) setBusy(false); }
  }
  return <Space wrap>
    {!launch.trusted && <Tooltip title={t("settings.mcpLaunchLifetime")}>
      <Button type="text" disabled={disabled || !!unresolved} loading={busy} onClick={() => void changeTrust("once")}>{t("settings.mcpApproveLaunch")}</Button>
    </Tooltip>}
    {!launch.remembered && <Tooltip title={t("settings.mcpRememberHint")}>
      <Button type="text" disabled={disabled || !!unresolved} loading={busy} onClick={() => void changeTrust("remember")}>{t("settings.mcpRememberLaunch")}</Button>
    </Tooltip>}
    {launch.remembered && <Typography.Text type="secondary">{t("settings.mcpRemembered")}</Typography.Text>}
    {(launch.trusted || launch.remembered) && <Button type="text" danger disabled={disabled || !!unresolved} loading={busy} onClick={() => void changeTrust("revoke")}>{t("settings.mcpRevokeLaunch")}</Button>}
    {unresolved && <Button type="text" loading={busy} onClick={() => void inspectOperation()}>{t("settings.mcpInspectTrust")}</Button>}
    {launch.error && <Tooltip title={launch.error}><Typography.Text type="secondary">{t("settings.mcpTrustStatusHint")}</Typography.Text></Tooltip>}
  </Space>;
}
