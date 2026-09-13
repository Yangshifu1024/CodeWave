import { memo, useMemo, useState } from "react";
import { Button, Tag } from "antd";
import { useTranslation } from "react-i18next";
import type { ToolView } from "../../stores/run";
import { useSessions } from "../../stores/sessions";
import { ipc } from "../../ipc/client";
import { collapseDiff, type DiffLine } from "../../utils/diff";
import CodeBlock from "../../components/CodeBlock";

// 工具名 -> i18n 键映射（内置工具；mcp__ 前缀的工具名原样展示）
const VERBS: Record<string, string> = {
  read: "tools.read", edit: "tools.edit", create: "tools.create", delete: "tools.delete",
  list_files: "tools.list_files", command: "tools.command", grep: "tools.grep", ask: "tools.ask",
  web_fetch: "tools.web_fetch", http_request: "tools.http_request", service: "tools.service",
  wait: "tools.wait", suggest: "tools.suggest", plan: "tools.plan", batch_read: "tools.batch_read",
  calculate: "tools.calculate", render_html: "tools.render_html", skill: "tools.skill",
};

interface EditFileView { path: string; diff: { type: string; text: string }[] }

// memo：immer 结构共享让历史工具卡引用稳定，流式帧不再触发 diff/JSON 重算
/** 工具调用卡实现：头部为状态点 + 动作词 + 工具名 + 摘要 + 耗时；展开体按工具类型分派
 *  （read 列内容 / edit 展示 diff / command 展示输出 / grep 列命中 / 其余 JSON）。 */
function ToolCallCardImpl({ tool, onToggle }: { tool: ToolView; onToggle?: () => void }) {
  const { t } = useTranslation();
  const [expanded, setExpanded] = useState(false);

  const data: any = tool.outcome?.data ?? {};
  const files: any[] = data.files ?? [];
  const isEditLike = ["edit", "create"].includes(tool.tool);

  // 仅展开时（或流式进行中）计算 diff；收起态零开销
  const editFiles: EditFileView[] = useMemo(() => {
    if (!expanded) return [];
    const raw = tool.argsPreview ?? "{}";
    if (raw.includes("_args_truncated")) {
      return [{ path: t("tools.argsTooLarge"), diff: [] }];
    }
    try {
      const args = JSON.parse(raw);
      if (tool.tool === "create") {
        const p = args.path ?? "";
        const content: string = args.content ?? "";
        return [{ path: p, diff: collapseDiff(content.split("\n").map((l) => ({ type: "add", text: l }) as DiffLine)) }];
      }
      return (args.files ?? []).map((f: any) => {
        const lines: DiffLine[] = [];
        for (const ch of f.changes ?? []) {
          if (ch.oldText != null) {
            for (const l of String(ch.oldText).split("\n")) lines.push({ type: "del", text: l });
          }
          for (const l of String(ch.newText ?? "").split("\n")) lines.push({ type: "add", text: l });
          if (ch.lineRange != null) lines.push({ type: "del", text: `lineRange ${ch.lineRange}` });
        }
        return { path: f.path ?? "", diff: collapseDiff(lines) };
      });
    } catch {
      return [];
    }
  }, [expanded, tool.tool, tool.argsPreview, t]);

  const exitCode: number | undefined = data.exit_code;
  const grepText = (data.matches ?? []).map((m: any) => `${m.path}:${m.line}: ${m.text}`).join("\n");
  const listText = (data.entries ?? []).join("\n");
  const bodyPreview = typeof data.body === "string" ? data.body : JSON.stringify(data.body ?? null, null, 1);

  // read/batch_read/edit 一次调用携带 files 数组：头部列全部文件名（basename，不带路径）。
  // 列表优先 outcome（入参过大时 argsPreview 被截断标记替换、无法解析出文件列表）；运行中才回退 args
  const fileListTool = tool.tool === "read" || tool.tool === "batch_read" || tool.tool === "edit";
  const baseName = (p: any) => String(p ?? "").split(/[\\/]/).pop() ?? "";
  const summary = (() => {
    if (fileListTool) {
      const fromOutcome =
        tool.tool === "edit" ? (Array.isArray(data.edited) ? data.edited : []) : files.map((f: any) => f.path);
      const paths = fromOutcome.length
        ? fromOutcome
        : (() => {
            try {
              return (JSON.parse(tool.argsPreview ?? "{}").files ?? []).map((f: any) => f.path);
            } catch {
              return [];
            }
          })();
      return paths.map(baseName).filter(Boolean).join(", ");
    }
    try {
      const args = JSON.parse(tool.argsPreview ?? "{}");
      if (args.url) return String(args.url).slice(0, 60);
      if (args.path) return args.path;
      if (args.command) return String(args.command).slice(0, 60);
      if (args.pattern) return `/${args.pattern}/`;
      if (args.files?.[0]?.path) return args.files[0].path;
      if (args.expression) return args.expression;
    } catch {
      /* 忽略解析失败 */
    }
    return "";
  })();

  // command/service 展开时把完整命令展示为第一行（不截断；service 无参数或解析失败则为空、不渲染）
  const commandLine = (() => {
    try {
      const cmd = JSON.parse(tool.argsPreview ?? "{}")?.command;
      return cmd ? String(cmd) : "";
    } catch {
      return "";
    }
  })();
  const prettyJson = useMemo(
    () => (expanded ? JSON.stringify(data, null, 1)?.slice(0, 4000) ?? "" : ""),
    [expanded, data],
  );

  const displayName = tool.tool.startsWith("mcp__") ? tool.tool : tool.tool;
  // ask 未作答类错误码中性化（docs/ask-ignore-not-answered-fix.md 的展示层配套）：
  // 后端把「忽略/取消」按 err 返回是模型侧硬信号（防误读为默许），但用户视角只是「没回答」。
  // 渲染层按错误码把这两类降级为中性灰 + 「未回答/已取消」，其余错误仍红色「失败」。
  // store 三路（实时/历史恢复/子代理流）共用本组件，一处修复全覆盖；模型侧语义零改动。
  const askNotAnsweredErr =
    tool.tool === "ask" &&
    (tool.outcome?.error?.code === "E_ASK_NOT_ANSWERED" || tool.outcome?.error?.code === "E_ASK_CANCELLED");
  const verbLabel = (() => {
    const key = VERBS[tool.tool];
    // "?" = 运行中占位卡（无名帧兑底）：只显示状态词，不渲染问号
    const label = key ? t(key) : tool.tool === "?" ? "" : tool.tool.replace(/^mcp__/, "[mcp] ");
    if (askNotAnsweredErr) {
      return `${tool.outcome?.error?.code === "E_ASK_CANCELLED" ? t("tools.cancelled") : t("tools.notAnswered")} ${label}`;
    }
    const prefix =
      tool.status === "running" ? t("tools.running") : tool.status === "error" ? t("tools.failed") : t("tools.used");
    return label ? `${prefix} ${label}` : prefix;
  })();

  async function stopService() {
    const sessionId = useSessions.getState().activeKey;
    const id = data.id;
    if (!sessionId || !id) return;
    // 真正调用后端停止服务
    await ipc.stopService(sessionId, id).catch((e) => alert(String(e)));
  }

  return (
    <div className={`tool-card st-${askNotAnsweredErr ? "neutral" : tool.status}`}>
      <div
        className="tool-head"
        onClick={() => {
          setExpanded(!expanded);
          onToggle?.(); // 展开/收起 = 阅读意图：暂停自动跟随
        }}
      >
        <span className="dot" />
        <span className="verb">{verbLabel}</span>
        {tool.tool !== "?" && <code className="tool-name">{displayName}</code>}
        {summary && <span className="summary" title={summary}>{summary}</span>}
        {tool.durationMs != null && <span className="dur">{(tool.durationMs / 1000).toFixed(1)}s</span>}
        <span className="chev">{expanded ? "▾" : "▸"}</span>
      </div>

      {tool.status === "running" && tool.progressTail && <pre className="tail">{tool.progressTail}</pre>}

      {expanded && (
        <div className="tool-body">
          {tool.tool === "read" || tool.tool === "batch_read" ? (
            <>
              {files.map((f, i) => (
                <div className="block" key={i}>
                  <div className="file-line">
                    {f.path} <span className="dim">L{f.start_line}-{f.end_line} / {f.total_lines}</span>
                  </div>
                  <pre className="tail">{f.content || ""}</pre>
                  {f.kind === "image" && <img src={f.data_url} className="img" alt="" />}
                </div>
              ))}
            </>
          ) : isEditLike ? (
            <>
              {editFiles.map((file, i) => (
                <div className="block" key={i}>
                  <div className="file-line">{file.path}</div>
                  {file.diff.map((ln, j) => (
                    <div className={`diff-line ${ln.type}`} key={j}>
                      <span className="diff-mark">{ln.type === "add" ? "+" : ln.type === "del" ? "-" : " "}</span>
                      <span>{ln.text}</span>
                    </div>
                  ))}
                </div>
              ))}
            </>
          ) : tool.tool === "command" || tool.tool === "service" ? (
            <>
              {commandLine && (
                <div className="kv cmd-line">
                  <code>{commandLine}</code>
                </div>
              )}
              {tool.tool === "service" && (
                <div className="kv">                  <Tag bordered={false}>{data.tail ? t("tools.serviceRunning") : t("tools.serviceStopped")}</Tag>
                  {data.tail && (
                    <Button
                      size="small"
                      style={{ fontSize: 11 }}
                      onClick={(e) => {
                        e.stopPropagation();
                        void stopService();
                      }}
                    >
                      {t("tools.stopService")}
                    </Button>
                  )}
                </div>
              )}
              {(data.status !== undefined || data.exit_code !== undefined) && (
                <div className="kv">
                  <span>{t("tools.exitCode")}:</span>
                  <Tag color={exitCode === 0 ? "success" : "error"}>{exitCode ?? "?"}</Tag>
                  {data.output_file_path && <span className="dim">{data.output_file_path}</span>}
                </div>
              )}
              <CodeBlock code={String(data.tail || data.output || data.note || t("tools.noOutput"))} language="bash" />
            </>
          ) : tool.tool === "grep" || tool.tool === "list_files" ? (
            <>
              {data.total !== undefined && (
                <div className="kv dim">{t("tools.grepHits", { n: data.total, m: (data.file_counts || []).length })}</div>
              )}
              <CodeBlock code={grepText || listText} />
            </>
          ) : tool.tool === "web_fetch" || tool.tool === "http_request" ? (
            <>
              <div className="kv dim">
                {data.status && <span>{data.status} {data.status_text}</span>}
                {data.title && <span>📌 {data.title}</span>}
                {data.truncated && <span>{t("tools.truncated")}</span>}
              </div>
              <pre className="tail">{data.content || bodyPreview}</pre>
            </>
          ) : tool.tool === "render_html" ? (
            <>
              <div className="kv dim">{data.title}</div>
              <iframe
                className="widget"
                srcDoc={data.html}
                sandbox="allow-scripts"
                style={{ border: "1px solid var(--ws-border)", borderRadius: 8, width: "100%", maxHeight: 480 }}
              />
            </>
          ) : (
            <CodeBlock code={prettyJson} language="json" />
          )}

          {tool.outcome?.error && (
            <div className={askNotAnsweredErr ? "err err-neutral" : "err"}>
              {tool.outcome.error.code}: {tool.outcome.error.message}
            </div>
          )}
          {(tool.outcome?.warnings ?? []).map((w: string, i: number) => (
            <div className="warn" key={`w${i}`}>{w}</div>
          ))}
        </div>
      )}
    </div>
  );
}

const ToolCallCard = memo(ToolCallCardImpl);
export default ToolCallCard;
