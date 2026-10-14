import { useEffect, useState } from "react";
import { Alert, Button, Empty, Menu, Modal, Skeleton, Tag, Tooltip, Typography } from "antd";
import type { MenuProps } from "antd";
import { FileOutlined, ReloadOutlined } from "@ant-design/icons";
import { useTranslation } from "react-i18next";
import type { GitDiffFile } from "../../ipc/types";
import CodeBlock from "../../components/CodeBlock";
import type { WorkspaceChanges } from "./useWorkspaceChanges";

const fileKey = (file: GitDiffFile) => JSON.stringify([file.root ?? "", file.path]);

/** Wide, read-only diff viewer. Selection survives closing within the same session. */
export default function ChangesModal({ open, sessionId, changes, onClose }: {
  open: boolean; sessionId: string | null; changes: WorkspaceChanges; onClose(): void;
}) {
  const { t } = useTranslation();
  const [selection, setSelection] = useState<{ sessionId: string | null; key: string | null }>({ sessionId, key: null });
  useEffect(() => { setSelection({ sessionId, key: null }); }, [sessionId]);
  const files = changes.files ?? [];
  const statusLabels: Record<string, string> = {
    added: t("diff.status.added"), deleted: t("diff.status.deleted"), modified: t("diff.status.modified"),
    renamed: t("diff.status.renamed"), untracked: t("diff.status.untracked"),
  };
  const selected = (selection.sessionId === sessionId && files.find(file => fileKey(file) === selection.key)) || files[0];
  const groups = new Map<string, GitDiffFile[]>();
  for (const file of files) {
    const root = file.root ?? "";
    const group = groups.get(root) ?? [];
    group.push(file);
    groups.set(root, group);
  }
  const itemsOf = (group: GitDiffFile[]) => group.map(file => ({
    key: fileKey(file), icon: <FileOutlined />,
    label: <div className="changes-file-label"><Tooltip title={file.path}><span>{file.path}</span></Tooltip><Typography.Text type="secondary">+{file.additions} / −{file.deletions}</Typography.Text></div>,
  }));
  const menuItems: NonNullable<MenuProps["items"]> = [];
  for (const [root, group] of groups) {
    if (root) menuItems.push({
      key: "root:" + root, type: "group",
      label: <Tooltip title={root}><span>{root.split(/[\\/]/).filter(Boolean).pop() || root}</span></Tooltip>,
      children: itemsOf(group),
    });
    else menuItems.push(...itemsOf(group));
  }

  return (
    <Modal
      open={open} onCancel={onClose} footer={null} centered destroyOnHidden
      width="min(1200px, calc(100vw - 48px))"
      className="changes-modal"
      title={<div className="changes-modal-title"><span>{t("diff.title")}</span><Button type="text" size="small" icon={<ReloadOutlined />} loading={changes.loading} disabled={!sessionId} aria-label={t("rightbar.refreshChanges")} onClick={changes.refresh} /></div>}
      styles={{ body: { height: "min(70vh, 760px)", overflow: "hidden" } }}
    >
      <div className="changes-view">
        {changes.error && <Alert type="error" showIcon title={changes.error} action={<Button size="small" onClick={changes.refresh} loading={changes.loading}>{t("rightbar.quotaRetry")}</Button>} />}
        {changes.truncated && <Alert type="info" showIcon title={t("diff.truncated")} />}
        {!sessionId ? <Empty description={t("rightbar.noActiveSession")} />
          : changes.loading && changes.files === null ? <Skeleton active paragraph={{ rows: 6 }} />
            : files.length === 0 ? (!changes.error && <Empty description={t("diff.empty")} />)
              : <div className="changes-layout">
                <nav className="changes-files" aria-label={t("diff.files")}>
                  <Menu mode="inline" selectedKeys={selected ? [fileKey(selected)] : []}
                    onClick={({ key }) => setSelection({ sessionId, key })}
                    items={menuItems}
                  />
                </nav>
                <section className="changes-detail" aria-label={t("diff.patch")}>
                  <div className="changes-file-heading">
                    <div className="changes-file-location">
                      <Tooltip title={selected.path}><Typography.Text code ellipsis>{selected.path}</Typography.Text></Tooltip>
                      {selected.root && groups.size > 1 && <Tooltip title={selected.root}><Typography.Text type="secondary" ellipsis>{selected.root}</Typography.Text></Tooltip>}
                    </div>
                    <Tag color={selected.status === "added" ? "success" : selected.status === "deleted" ? "error" : undefined}>{statusLabels[selected.status] ?? selected.status}</Tag>
                    <span className="changes-file-stats"><span className="rb-change-added">+{selected.additions}</span><span className="rb-change-deleted">−{selected.deletions}</span></span>
                  </div>
                  <div className="changes-diff">
                    {selected.patch ? <CodeBlock code={selected.patch} language="diff" /> : <Empty description={t("diff.noPatch")} />}
                  </div>
                </section>
              </div>}
      </div>
    </Modal>
  );
}
