// [docs/session-artifacts-and-files-tab](../../../../docs/session-artifacts-and-files-tab.md)：文件页签内容——单列表全量展示（图标区分类型），点击弹窗查看
// [docs/office-and-pdf-support](../../../../docs/office-and-pdf-support.md)：被本应用改过的文档可从这里「回退」到改动前的版本
import { useState } from "react";
import { Alert, App, Button, Modal, Radio, Tooltip } from "antd";
import {
  FileImageOutlined,
  FileMarkdownOutlined,
  FileOutlined,
  FileTextOutlined,
  ReloadOutlined,
  RollbackOutlined,
} from "@ant-design/icons";
import { useTranslation } from "react-i18next";
import { i18n } from "../../i18n";
import { ipc } from "../../ipc/client";
import type { DocumentBackupEntry, SessionFileEntry } from "../../ipc/types";
import { isImagePath } from "./FileViewerModal";

/** 能回退的扩展名：与 edit_document 支持的保真修改范围一致（只有它会产生备份）。 */
const RESTORABLE_EXTS = new Set(["xlsx", "xlsm", "docx"]);

function baseName(p: string): string {
  return p.split(/[\\/]/).filter(Boolean).pop() ?? p;
}

function extOf(p: string): string {
  const name = baseName(p);
  const i = name.lastIndexOf(".");
  return i >= 0 ? name.slice(i + 1).toLowerCase() : "";
}

function relTime(iso: string): string {
  const t = Date.parse(iso);
  if (Number.isNaN(t)) return "—";
  const diff = Date.now() - t;
  if (diff < 60_000) return i18n.t("files.justNow");
  if (diff < 3_600_000) return i18n.t("files.minutesAgo", { n: Math.floor(diff / 60_000) });
  if (diff < 86_400_000) return i18n.t("files.hoursAgo", { n: Math.floor(diff / 3_600_000) });
  const d = new Date(t);
  const pad = (x: number) => String(x).padStart(2, "0");
  return `${d.getMonth() + 1}-${pad(d.getDate())} ${pad(d.getHours())}:${pad(d.getMinutes())}`;
}

/** 备份体积：表格/文档都是 KB 到 MB 量级，用 KB 起步就够，不再往 GB 上写。 */
function sizeLabel(bytes: number): string {
  if (bytes < 1024) return `${bytes} B`;
  if (bytes < 1024 * 1024) return `${Math.round(bytes / 1024)} KB`;
  return `${(bytes / 1024 / 1024).toFixed(1)} MB`;
}

/** 按路径类型选择图标：图片/markdown 用强调色，文本类用默认文件图标。 */
function TypeIcon({ entry }: { entry: SessionFileEntry }) {
  const p = entry.path.toLowerCase();
  if (isImagePath(entry.path)) return <FileImageOutlined style={{ color: "var(--ws-accent)" }} />;
  if (p.endsWith(".md") || p.endsWith(".markdown")) return <FileMarkdownOutlined style={{ color: "var(--ws-accent)" }} />;
  if (p.endsWith(".txt") || p.endsWith(".log") || p.endsWith(".rst")) return <FileTextOutlined />;
  return <FileOutlined />;
}

/** 待回退的目标：路径 + 后端给的备份清单（清单在打开弹框时才拉，不给每一行都去问一次）。 */
interface RestoreState {
  path: string;
  list: DocumentBackupEntry[];
  /** 选中的备份路径（默认最新那份） */
  picked: string | null;
  /** 正在拉清单 */
  loading: boolean;
}

/** 右侧栏「文件」页签：会话产物列表（basename + 最近操作/时间，Tooltip 显示全路径），支持刷新与点击查看。 */
export default function FilesPanel({
  sessionId,
  files,
  onView,
  onRefresh,
}: {
  sessionId: string | null;
  files: SessionFileEntry[];
  onView: (path: string) => void;
  onRefresh: () => void;
}) {
  const { t } = useTranslation();
  const { message } = App.useApp();
  const [restore, setRestore] = useState<RestoreState | null>(null);
  const [busy, setBusy] = useState(false);

  async function openRestore(path: string) {
    if (!sessionId) return;
    setRestore({ path, list: [], picked: null, loading: true });
    try {
      const list = await ipc.listDocumentBackups(sessionId, path);
      setRestore({ path, list, picked: list[0]?.path ?? null, loading: false });
    } catch (e) {
      setRestore(null);
      message.warning(String(e).replace(/^Error[:\s]*/i, ""));
    }
  }

  async function confirmRestore() {
    if (!sessionId || !restore?.picked) return;
    setBusy(true);
    try {
      await ipc.restoreDocumentBackup(sessionId, restore.path, restore.picked);
      message.success(t("files.restoreDone", { name: baseName(restore.path) }));
      setRestore(null);
      // 回退会改文件的体积与修改时间：刷新一次列表（stat 是后端现算的）
      onRefresh();
    } catch (e) {
      message.warning(String(e).replace(/^Error[:\s]*/i, ""));
    } finally {
      setBusy(false);
    }
  }

  return (
    <div className="rb-files">
      <div className="rb-files-tools">
        <span className="rb-dim">{sessionId ? t("files.totalCount", { n: files.length }) : t("files.noActiveSession")}</span>
        <Button size="small" icon={<ReloadOutlined />} onClick={onRefresh} />
      </div>
      {!sessionId ? (
        <div className="rb-dim">{t("files.openSessionHint")}</div>
      ) : files.length === 0 ? (
        <div className="rb-dim">{t("files.emptySession")}</div>
      ) : (
        <div className="rb-files-list">
          {files.map((f) => {
            const canRestore = f.exists && RESTORABLE_EXTS.has(extOf(f.path));
            const label = (
              <div
                className={`rb-file ${f.exists ? "" : "rb-file-gone"}`}
                onClick={f.exists ? () => onView(f.path) : undefined}
              >
                <span className="rb-file-icon">
                  <TypeIcon entry={f} />
                </span>
                <span className="rb-file-name">{baseName(f.path)}</span>
                <span className="rb-file-meta">
                  {f.last_op === "edit" ? t("files.lastOpEdit") : t("files.lastOpCreate")} · {relTime(f.last_at)}
                  {!f.exists && ` · ${t("files.deleted")}`}
                </span>
                {canRestore && (
                  // 点回退不能连带把预览打开：行本身是可点击的
                  <Button
                    type="text"
                    size="small"
                    className="rb-file-btn"
                    icon={<RollbackOutlined />}
                    title={t("files.restore")}
                    aria-label={t("files.restore")}
                    onClick={(e) => {
                      e.stopPropagation();
                      void openRestore(f.path);
                    }}
                  />
                )}
              </div>
            );
            return (
              <Tooltip key={f.path} title={f.path} placement="left" mouseEnterDelay={0.4}>
                {label}
              </Tooltip>
            );
          })}
        </div>
      )}

      <Modal
        open={!!restore}
        title={t("files.restoreTitle")}
        onCancel={() => setRestore(null)}
        onOk={() => void confirmRestore()}
        okText={t("files.restore")}
        confirmLoading={busy}
        okButtonProps={{ disabled: !restore?.picked }}
      >
        <p>{t("files.restoreDesc")}</p>
        {restore?.loading ? null : !restore?.list.length ? (
          <Alert type="info" showIcon message={t("files.restoreNone")} />
        ) : (
          <>
            <div style={{ marginBottom: 4 }}>{t("files.restorePick")}</div>
            <Radio.Group
              style={{ display: "flex", flexDirection: "column", gap: 4 }}
              value={restore.picked}
              onChange={(e) => setRestore({ ...restore, picked: e.target.value })}
              options={restore.list.map((b) => ({
                value: b.path,
                label: `${relTime(b.at)} · ${sizeLabel(b.size)}`,
              }))}
            />
            {/* 被回退掉的当前内容也会留一份备份，所以「回退」这件事本身能再撤回 */}
            <div className="rb-dim" style={{ marginTop: 8, fontSize: 12 }}>
              {t("files.restoreNote")}
            </div>
          </>
        )}
      </Modal>
    </div>
  );
}
