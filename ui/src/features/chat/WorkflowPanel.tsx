import { useEffect, useRef, useState } from "react";
import { Alert, Button, Input, Modal, Select, Space, Tag, Typography } from "antd";
import { useTranslation } from "react-i18next";
import { ipc } from "../../ipc/client";
import type { WorkflowProjection } from "../../ipc/types";
import { useRun } from "../../stores/run";
const kinds = ["conversation", "mechanical", "single_surface", "serial_surfaces", "parallel_surfaces"];
export default function WorkflowPanel({ sessionId, visible }: {
    sessionId: string | null;
    visible: boolean;
}) {
    const { t } = useTranslation();
    const running = useRun(s => sessionId ? s.tabs[sessionId]?.running : undefined);
    const done = useRun(s => sessionId ? s.tabs[sessionId]?.lastDoneRunId : undefined);
    const [data, setData] = useState<{
        session: string;
        value: WorkflowProjection | null;
    } | null>(null);
    const [error, setError] = useState("");
    const [revision, setRevision] = useState(0);
    const [action, setAction] = useState<"resume" | "route" | null>(null);
    const [kind, setKind] = useState("serial_surfaces");
    const [reason, setReason] = useState("");
    const [busy, setBusy] = useState(false);
    useEffect(() => { if (!visible || !sessionId)
        return; let alive = true; let inFlight = false; const load = async () => { if (inFlight)
        return; inFlight = true; try {
        const value = await ipc.getWorkflowProjection(sessionId);
        if (alive) {
            setData({ session: sessionId, value });
            setError("");
        }
    }
    catch (e) {
        if (alive) {
            setData(null);
            setError(String(e));
        }
    }
    finally {
        inFlight = false;
    } }; void load(); const timer = window.setInterval(() => void load(), 10000); const focus = () => void load(); window.addEventListener("focus", focus); return () => { alive = false; window.clearInterval(timer); window.removeEventListener("focus", focus); }; }, [sessionId, visible, running, done, revision]);
    useEffect(() => { setAction(null); setError(""); setBusy(false); }, [sessionId]);
    const operation = useRef({ generation: 0, pending: false });
    useEffect(() => { const state = operation.current; state.generation += 1; state.pending = false; return () => { state.generation += 1; state.pending = false; }; }, [sessionId]);
    const p = data?.session === sessionId ? data.value : null;
    const apply = async () => { if (!sessionId || !p || operation.current.pending)
        return; const id = operation.current.generation; const current = () => id === operation.current.generation; operation.current.pending = true; const session = sessionId; setBusy(true); try {
        if (action === "resume")
            await ipc.continueWorkflow(session, p.task_id, p.contract_revision);
        else
            await ipc.overrideWorkflowRoute(session, p.task_id, p.contract_revision, kind, reason);
        if (current()) {
            setAction(null);
            setRevision(v => v + 1);
        }
    }
    catch (e) {
        if (current())
            setError(String(e));
    }
    finally {
        if (current()) {
            operation.current.pending = false;
            setBusy(false);
        }
    } };
    if (!visible || !sessionId)
        return null;
    if (error)
        return <Alert type="error" title={t("workflow.failed", { error })}/>;
    if (!p)
        return null;
    return <Space orientation="vertical" style={{ width: "100%" }}><Typography.Text>{t("workflow.version", { revision: p.frozen_revision })}</Typography.Text>{p.candidate && <Typography.Text>{t("workflow.candidate", { revision: p.candidate.revision })}</Typography.Text>}{p.candidate && (p.source_digest ? p.candidate.digest !== p.source_digest : p.candidate.text.trim() !== "") && <Typography.Text type="secondary">{t("workflow.changed")}</Typography.Text>}<Typography.Text>{t("workflow.route") + ": " + p.route.kind}</Typography.Text>{p.route.reasons.map((r, i) => <Typography.Paragraph key={String(i)}>{r}</Typography.Paragraph>)}{p.nodes.map(node => <div key={"node:" + node.unit_id}><Tag color={node.state === "failed" ? "error" : node.state === "blocked" || node.state === "interrupted" ? "warning" : "default"}>{node.state}</Tag><Typography.Text>{node.unit_id + " · " + t("workflow.attempt") + " " + node.attempt}</Typography.Text>{node.reason && <Typography.Paragraph type="secondary">{node.reason}</Typography.Paragraph>}</div>)}{p.units.map(unit => <div key={"unit:" + unit.id}><Typography.Text strong>{unit.id}</Typography.Text><Typography.Paragraph>{t("workflow.owner") + ": " + (unit.role_id || "—") + " · " + (unit.model_id || "—")}</Typography.Paragraph><Typography.Paragraph>{t("workflow.dependencies") + ": " + unit.depends_on.join(", ")}</Typography.Paragraph><Typography.Paragraph>{t("workflow.scope") + ": " + unit.write_scopes.map(path => path.root_id + ":" + path.relative_path).join(", ")}</Typography.Paragraph></div>)}<Typography.Text strong>{t("workflow.checks")}</Typography.Text>{p.checks.map(check => <Typography.Paragraph key={"check:" + check.id} code>{check.id + " · " + JSON.stringify({ cwd: check.cwd, command: check.command, platforms: check.platforms, prerequisites: check.prerequisites })}</Typography.Paragraph>)}{p.leases.length > 0 && <><Typography.Text strong>{t("workflow.leases")}</Typography.Text>{p.leases.map(l => <Typography.Paragraph key={"lease:" + l.unit_id}>{l.unit_id + ": " + l.held_paths.join(", ")}</Typography.Paragraph>)}</>}{p.recovery_error && <Alert type="warning" title={p.recovery_error}/>}<Space wrap><Button disabled={!p.resume_allowed || p.nodes.every(n => n.state === "passed") || busy} onClick={() => setAction("resume")}>{t("workflow.resume")}</Button><Button disabled={busy} onClick={() => { setKind(p.route.kind); setReason(""); setAction("route"); }}>{t("workflow.override")}</Button></Space><Modal open={action !== null} title={t(action === "resume" ? "workflow.resume" : "workflow.override")} onCancel={() => { if (!busy)
        setAction(null); }} onOk={() => void apply()} confirmLoading={busy} okButtonProps={{ disabled: action === "route" && !reason.trim() }}>{action === "resume" ? <Typography.Paragraph>{t("workflow.resumeConfirm")}</Typography.Paragraph> : <Space orientation="vertical"><Select aria-label={t("workflow.route")} value={kind} onChange={setKind} options={kinds.map(k => ({ value: k, label: k }))}/><Input.TextArea aria-label={t("workflow.overrideReason")} placeholder={t("workflow.overrideReason")} value={reason} onChange={e => setReason(e.target.value)}/></Space>}</Modal></Space>;
}
