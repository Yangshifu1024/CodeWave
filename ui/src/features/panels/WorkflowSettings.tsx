import { useCallback, useEffect, useRef, useState } from "react";
import { Alert, Button, Input, Modal, Space, Tag, Typography } from "antd";
import { useTranslation } from "react-i18next";
import { ipc } from "../../ipc/client";
import type { WorkflowConfigView, WorkflowIssue } from "../../ipc/types";
const starter = JSON.stringify({ schema: 1, id: "project-default", nodes: [{ id: "build", kind: "implement", tool_allowlist: ["read", "edit", "create", "command"] }, { id: "review", kind: "review", role_id: "code-reviewer", tool_allowlist: ["read", "delivery_contract"], required_checks: ["host_independent_review"] }, { id: "finish", kind: "complete", tool_allowlist: [] }], edges: [{ from: "build", to: "review" }, { from: "review", to: "finish" }], repair_rules: [], role_policies: {}, model_policies: [], checks: [] }, null, 2);
export default function WorkflowSettings({ sessionId }: {
    sessionId: string | null;
}) {
    const { t } = useTranslation();
    const [view, setView] = useState<WorkflowConfigView | null>(null);
    const [draft, setDraft] = useState("");
    const [issues, setIssues] = useState<WorkflowIssue[]>([]);
    const [error, setError] = useState("");
    const [busy, setBusy] = useState(false);
    const [review, setReview] = useState<string | null>(null);
    const identity = useRef(sessionId);
    identity.current = sessionId;
    const generation = useRef(0);
    const pending = useRef(false);
    const invalidate = useCallback(() => { generation.current += 1; pending.current = false; }, []);
    const load = useCallback(async () => { if (!sessionId || pending.current)
        return; const id = ++generation.current; pending.current = true; setBusy(true); try {
        const v = await ipc.readProjectWorkflow(sessionId);
        if (id !== generation.current || identity.current !== sessionId)
            return;
        setView(v);
        setDraft(v.text || starter);
        setIssues(v.issues);
        setError("");
    }
    catch (e) {
        if (id === generation.current)
            setError(String(e));
    }
    finally {
        if (id === generation.current) {
            pending.current = false;
            setBusy(false);
        }
    } }, [sessionId]);
    useEffect(() => { invalidate(); setView(null); setDraft(""); setIssues([]); setError(""); setReview(null); void load(); return invalidate; }, [sessionId, load, invalidate]);
    const validate = async () => { if (!sessionId || pending.current)
        return; const id = generation.current; pending.current = true; setBusy(true); try {
        const found = await ipc.validateProjectWorkflow(sessionId, draft);
        if (id === generation.current) {
            setIssues(found);
            setError("");
        }
    }
    catch (e) {
        if (id === generation.current)
            setError(String(e));
    }
    finally {
        if (id === generation.current) {
            pending.current = false;
            setBusy(false);
        }
    } };
    const save = async () => { if (!sessionId || !view || review === null || pending.current)
        return; const id = generation.current; const text = review; pending.current = true; setBusy(true); try {
        const found = await ipc.validateProjectWorkflow(sessionId, text);
        if (id !== generation.current)
            return;
        setIssues(found);
        if (found.length)
            return;
        const v = await ipc.saveProjectWorkflow(sessionId, text, view.revision, view.digest, crypto.randomUUID());
        if (id !== generation.current)
            return;
        setView(v);
        setDraft(v.text);
        setReview(null);
        setError("");
    }
    catch (e) {
        if (id === generation.current)
            setError(String(e));
    }
    finally {
        if (id === generation.current) {
            pending.current = false;
            setBusy(false);
        }
    } };
    if (!sessionId)
        return <Typography.Text type="secondary">{t("workflow.noSession")}</Typography.Text>;
    return <Space orientation="vertical" style={{ width: "100%" }}><Typography.Title level={5}>{t("workflow.edit")}</Typography.Title><Typography.Paragraph type="secondary">{t("workflow.hint")}</Typography.Paragraph>{error && <Alert type="error" title={t("workflow.failed", { error })}/>}<Space><Button onClick={() => void load()} disabled={busy}>{t("workflow.load")}</Button>{view && <Tag>{t(view.approved ? "workflow.approved" : "workflow.pending")}</Tag>}</Space><Input.TextArea aria-label={t("workflow.draft")} value={draft} onChange={e => setDraft(e.target.value)} rows={16} spellCheck={false} disabled={busy}/>{issues.map((issue, i) => <Alert key={issue.pointer + String(i)} type="error" title={issue.pointer + " · " + issue.code} description={issue.message}/>)}<Space><Button onClick={() => void validate()} disabled={busy || !view}>{t("workflow.validate")}</Button><Button onClick={() => setReview(draft)} disabled={busy || !view}>{t("workflow.save")}</Button></Space><Modal open={review !== null} title={t("workflow.confirm")} onCancel={() => { if (!busy)
        setReview(null); }} onOk={() => void save()} confirmLoading={busy} width={760}><Typography.Paragraph>{t("workflow.hint")}</Typography.Paragraph><pre style={{ whiteSpace: "pre-wrap", maxHeight: 400, overflow: "auto" }}>{review}</pre></Modal></Space>;
}
