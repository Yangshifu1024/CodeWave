// [docs/session-artifacts-and-files-tab](../../../../docs/session-artifacts-and-files-tab.md)：文件页签内容——单列表全量展示（图标区分类型），点击弹窗查看
import { Button, Tooltip } from "antd";
import {
  FileImageOutlined,
  FileMarkdownOutlined,
  FileOutlined,
  FileTextOutlined,
  ReloadOutlined,
} from "@ant-design/icons";
import { useTranslation } from "react-i18next";
import { i18n } from "../../i18n";
import type { SessionFileEntry } from "../../ipc/types";
import { isImagePath } from "./FileViewerModal";

function baseName(p: string): string {
  return p.split(/[\\/]/).filter(Boolean).pop() ?? p;
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

/** 按路径类型选择图标：图片/markdown 用强调色，文本类用默认文件图标。 */
function TypeIcon({ entry }: { entry: SessionFileEntry }) {
  const p = entry.path.toLowerCase();
  if (isImagePath(entry.path)) return <FileImageOutlined style={{ color: "var(--ws-accent)" }} />;
  if (p.endsWith(".md") || p.endsWith(".markdown")) return <FileMarkdownOutlined style={{ color: "var(--ws-accent)" }} />;
  if (p.endsWith(".txt") || p.endsWith(".log") || p.endsWith(".rst")) return <FileTextOutlined />;
  return <FileOutlined />;
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
    </div>
  );
}
