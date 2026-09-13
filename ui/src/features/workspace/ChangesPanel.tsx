// 右侧栏「变更」页签：工作区 vs HEAD 的多根聚合 diff（原 GitDiffModal 内容面板化）
import { useCallback, useEffect, useRef, useState } from "react";
import { Button, Empty, Tag } from "antd";
import { RedoOutlined } from "@ant-design/icons";
import { useTranslation } from "react-i18next";
import { ipc } from "../../ipc/client";
import type { GitDiffFile } from "../../ipc/types";
import { useActiveId } from "../../stores/sessions";
import CodeBlock from "../../components/CodeBlock";

function dirName(p: string): string {
  return p.split("/").filter(Boolean).pop() || p;
}

/** 与 LogPanel 同一套激活语义：页签激活时才拉取，其余时刻零 IPC。 */
export default function ChangesPanel({ visible }: { visible: boolean }) {
  const { t } = useTranslation();
  const sessionId = useActiveId();
  const [files, setFiles] = useState<GitDiffFile[]>([]);
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState("");
  // 过期响应守卫：切换会话后迟到的旧响应不得覆盖新视图（与 LogPanel 同模式）
  const reqIdRef = useRef(0);

  const refresh = useCallback(() => {
    const myId = ++reqIdRef.current;
    if (!sessionId) {
      setFiles([]);
      setError("");
      setLoading(false);
      return;
    }
    setLoading(true);
    void ipc
      .gitDiff(sessionId)
      .then((res) => {
        if (reqIdRef.current !== myId) return;
        setError("");
        setFiles(res.files ?? []);
      })
      .catch((e) => {
        // 失败时保留旧数据（可与「无变更」空态区分），只呈现错误
        if (reqIdRef.current !== myId) return;
        setError(String(e).replace(/^Error[:\s]*/i, ""));
      })
      .finally(() => {
        if (reqIdRef.current === myId) setLoading(false);
      });
  }, [sessionId]);

  // 激活时拉取 + 会话切换重拉（流式运行期间不轮询；变更静态展示、手动刷新）
  useEffect(() => {
    if (visible) refresh();
  }, [visible, refresh]);

  return (
    <div className="rb-changes">
      <div className="rb-changes-tools">
        <span className="rb-dim">{t("diff.title")}</span>
        <span className="flex" />
        <Button size="small" icon={<RedoOutlined />} onClick={refresh} title={t("app.retry")} />
      </div>
      {loading && <div className="dim">…</div>}
      {error && <div className="rb-dim rb-log-error">{error}</div>}
      {!loading && (
        <div className="gitdiff">
          {(() => {
            // 多根项目：按所属目录分组；单根/无标记保持平铺
            const groups = new Map<string, typeof files>();
            for (const f of files) {
              const key = f.root ?? "";
              if (!groups.has(key)) groups.set(key, []);
              groups.get(key)!.push(f);
            }
            return [...groups.entries()].map(([root, group]) => (
              <div key={root || "all"}>
                {root && <div className="dim" style={{ margin: "10px 0 4px", fontWeight: 600 }}>{dirName(root)}</div>}
                {group.map((f) => (
                  <div className="file" key={f.path}>
                    <div className="file-head">
                      <code>{f.path}</code>
                      <Tag color={f.status === "added" ? "success" : f.status === "deleted" ? "error" : "warning"}>
                        {f.status}
                      </Tag>
                      <span className="stats">+{f.additions} / -{f.deletions}</span>
                    </div>
                    {f.patch && <CodeBlock code={f.patch} language="diff" />}
                  </div>
                ))}
              </div>
            ));
          })()}
          {files.length === 0 && !error && <Empty description={t("diff.empty")} />}
        </div>
      )}
    </div>
  );
}
