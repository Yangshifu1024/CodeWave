import { useEffect, useRef, useState } from "react";
import { App, Button, Descriptions, Space, Tooltip, Typography } from "antd";
import { useTranslation } from "react-i18next";
import { ipc } from "../../ipc/client";
import type { McpLaunchStatus, McpScope, McpServerView } from "../../ipc/types";
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
  return (kind: "launch" | "probe" | "revoke", name: string, source: string, version: string, config: LaunchConfig) =>
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
        title: kind === "probe" ? t("settings.mcpConfirmProbe") : kind === "revoke" ? t("settings.mcpConfirmRevoke") : t("settings.mcpConfirmLaunch"),
        content: <Space orientation="vertical">
          <Typography.Text>{kind === "revoke" ? t("settings.mcpLaunchRevokeHint") : kind === "probe" ? t("settings.mcpProbeLaunchHint") : t("settings.mcpLaunchLifetime")}</Typography.Text>
          <Typography.Text strong>{name}</Typography.Text>
          <Typography.Text code>{source}</Typography.Text>
          <LaunchDetails config={config} />
          <Typography.Text type="secondary">{t("settings.mcpLaunchVersion", { version: version.slice(0, 12) })}</Typography.Text>
        </Space>,
        okText: t(kind === "revoke" ? "settings.mcpRevokeLaunch" : kind === "probe" ? "settings.mcpRunProbe" : "settings.mcpApproveLaunch"),
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
  const owner = `${sessionId ?? ""}\u0000${scope}\u0000${source}\u0000${name}\u0000${launch.version}\u0000${disabled}`;
  const confirm = useMcpLaunchConfirmation(owner);
  const identity = useRef(owner);
  identity.current = owner;
  const mounted = useRef(true);
  const pending = useRef(false);
  const [busy, setBusy] = useState(false);
  useEffect(() => {
    mounted.current = true;
    return () => { mounted.current = false; };
  }, []);
  async function changeTrust() {
    if (pending.current || disabled) return;
    pending.current = true;
    setBusy(true);
    const requestOwner = owner;
    try {
      if (!await confirm(launch.trusted ? "revoke" : "launch", name, source, launch.version, config)) return;
      if (!mounted.current || identity.current !== requestOwner) return;
      await ipc.mcpSetLaunchTrust(scope, name, launch.version, !launch.trusted, sessionId ?? undefined);
      if (mounted.current && identity.current === requestOwner) await onChanged();
    } catch (error) {
      if (mounted.current && identity.current === requestOwner) message.error(String(error));
    } finally {
      pending.current = false;
      if (mounted.current) setBusy(false);
    }
  }
  return <Tooltip title={t("settings.mcpLaunchLifetime")}>
    <Button type="text" danger={launch.trusted} disabled={disabled} loading={busy} onClick={() => void changeTrust()}>
      {t(launch.trusted ? "settings.mcpRevokeLaunch" : "settings.mcpApproveLaunch")}
    </Button>
  </Tooltip>;
}
