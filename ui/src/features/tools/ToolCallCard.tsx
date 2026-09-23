import { memo, useEffect, useMemo, useState } from "react";
import { Button, Tag } from "antd";
import { useTranslation } from "react-i18next";
import type { ToolView } from "../../stores/run";
import { useSessions } from "../../stores/sessions";
import { ipc } from "../../ipc/client";
import { collapseDiff, type DiffLine } from "../../utils/diff";
import { shortestUniqueLabels } from "../../utils/path";
import CodeBlock from "../../components/CodeBlock";
import { askAnswerRows } from "./askAnswerRows";

// 工具名 -> i18n 键映射（内置工具；mcp__ 前缀的工具名原样展示）
const VERBS: Record<string, string> = {
  read: "tools.read", edit: "tools.edit", create: "tools.create", delete: "tools.delete",
  list_files: "tools.list_files", command: "tools.command", grep: "tools.grep", ask: "tools.ask",
  web_fetch: "tools.web_fetch", http_request: "tools.http_request", service: "tools.service",
  wait: "tools.wait", suggest: "tools.suggest", plan: "tools.plan", batch_read: "tools.batch_read",
  calculate: "tools.calculate", render_html: "tools.render_html", skill: "tools.skill",
};

interface EditFileView { path: string; diff: { type: string; text: string }[] }

/** 出错时降级为中性展示的错误码 → 文案键（docs/ask-ignore-not-answered-fix.md 的展示层配套）：
 *  后端把「忽略 / 取消」按 err 返回是模型侧硬信号（防误读为默许），运行/子代理结束时仍在途的工具卡也带 err，
 *  但用户视角只是「没完成」。渲染层按错误码降为中性灰 + 各自的措辞，其余错误仍红色「失败」。
 *  store 三路（实时/历史恢复/子代理流）共用本组件，一处修复全覆盖；模型侧语义零改动。 */
const NEUTRAL_ERR_KEYS: Record<string, string> = {
  E_ASK_CANCELLED: "tools.cancelled",
  E_ASK_NOT_ANSWERED: "tools.notAnswered",
  E_INTERRUPTED: "tools.interrupted",
};

// memo：immer 结构共享让历史工具卡引用稳定，流式帧不再触发 diff/JSON 重算
/** 工具调用卡实现：头部为状态点 + 动作词 + 工具名 + 摘要 + 耗时；展开体按工具类型分派
 *  （read 列内容 / edit 展示 diff / command 展示输出 / grep 列命中 / 其余 JSON）。 */
function ToolCallCardImpl({ tool, onToggle }: { tool: ToolView; onToggle?: () => void }) {
  const { t } = useTranslation();
  const [expanded, setExpanded] = useState(false);

  const data: any = useMemo(() => tool.outcome?.data ?? {}, [tool.outcome?.data]);
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

  // read/batch_read/edit 一次调用携带 files 数组：头部列全部文件名（不带目录的末段）。
  // 列表优先 outcome（入参过大时 argsPreview 被截断标记替换、无法解析出文件列表）；运行中才回退 args
  const fileListTool = tool.tool === "read" || tool.tool === "batch_read" || tool.tool === "edit";
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
      // 同名文件（a/ui.ts / b/ui.ts）向前扩段区分：同一天读到多个 index.ts 时头部摘要不再无从分辨
      return shortestUniqueLabels(paths).filter(Boolean).join(", ");
    }
    try {
      const args = JSON.parse(tool.argsPreview ?? "{}");
      if (args.url) return String(args.url).slice(0, 60);
      if (args.path) return args.path;
      if (args.command) return String(args.command).slice(0, 60);
      if (args.pattern) return `/${args.pattern}/`;
      if (args.files?.[0]?.path) return args.files[0].path;
      if (args.expression) return args.expression;
      // plan（Meta 工具）的入参键是 todos，不在上面的白名单里 → 摘要恒为空字符串，收起态无从看出规模。
      // 放在白名单最末，只有 plan 命中（其他工具入参无 todos），不会抢占 url/path/command/pattern 的既有摘要。
      // 中文字面量是刻意的：i18n 目录不在本批次改动范围，且这是一个纯计数串，后续统一国际化时再收敛成 key。
      if (Array.isArray(args.todos)) {
        const running = args.todos.filter((x: any) => x?.status === "in_progress").length;
        return running > 0 ? `${args.todos.length} 项 · ${running} 进行中` : `${args.todos.length} 项`;
      }
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

  // 失败调用的出参恒为「空」：后端 ToolOutcome::err 把 data 置成 Value::Null（src-tauri/src/tools/tool.rs），
  // 上面的 `?? {}` 归一之后只剩一个空对象——展开体于是只有 `{}`（例：plan 报 E_PLAN_INVALID
  // 「同一时刻最多一个 in_progress 任务」时，用户看不到是哪两个 todo 冲突）。
  // 此时入参是唯一能自证的证据，所以另算一份美化串，供出参为空时回退渲染。
  const argsJson = useMemo(() => {
    if (!expanded) return "";
    const raw = tool.argsPreview;
    if (!raw) return "";
    // 入参过大时已被截断标记替换（raw 不是真实 JSON）：既有「参数过大」文案负责提示，这里既不解析也不渲染
    if (raw.includes("_args_truncated")) return "";
    try {
      return JSON.stringify(JSON.parse(raw), null, 1)?.slice(0, 4000) ?? "";
    } catch {
      return "";
    }
  }, [expanded, tool.argsPreview]);

  // 空对象 / null / undefined / 空数组 / 空串都算「没有出参可看」（失败卡 + 无返回值的只读工具）
  const noOutcomeData =
    data == null || data === "" || (typeof data === "object" && Object.keys(data).length === 0);
  const showArgsInstead = noOutcomeData && !!argsJson;

  const displayName = tool.tool.startsWith("mcp__") ? tool.tool : tool.tool;
  // ask 卡片的问答行（题干一行 / 答案一行）：**始终显示**，与展开状态无关（见 docs 约定：
  // 回答完不展开也应当能看到自己选了什么）。入参截断 / 无出参时为空数组 → 不渲染。
  const askRows = useMemo(
    () =>
      tool.tool === "ask"
        ? askAnswerRows(tool.argsPreview, data, { notAnswered: t("tools.notAnswered") })
        : [],
    [tool.tool, tool.argsPreview, data, t],
  );
  const errCode: string | undefined = tool.outcome?.error?.code;
  const neutralErrKey = errCode ? NEUTRAL_ERR_KEYS[errCode] : undefined;
  const neutralErr = !!neutralErrKey;
  const verbLabel = (() => {
    const key = VERBS[tool.tool];
    // "?" = 运行中占位卡（无名帧兑底）：只显示状态词，不渲染问号
    const label = key ? t(key) : tool.tool === "?" ? "" : tool.tool.replace(/^mcp__/, "[mcp] ");
    if (neutralErrKey) return label ? `${t(neutralErrKey)} ${label}` : t(neutralErrKey);
    // waiting = 审批 / 范围确认门等待中（橙点 + 需注意语义），必须在 running 之前判定
    const prefix =
      tool.status === "waiting" ? t("tools.waiting")
        : tool.status === "running" ? t("tools.running")
          : tool.status === "error" ? t("tools.failed") : t("tools.used");
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
    <div className={`tool-card st-${neutralErr ? "neutral" : tool.status}`}>
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

      {/* ask：问答行始终显示（题干一行、答案一行；不省略、不需展开） */}
      {askRows.length > 0 && (
        <div className="ask-rows">
          {askRows.map((r, i) => (
            <div className="ask-row" key={i}>
              <div className="q">{r.question}</div>
              <div className="a">{r.answer}</div>
              {r.note !== "" && (
                <div className="note">
                  {t("tools.notePrefix")}
                  {r.note}
                </div>
              )}
            </div>
          ))}
        </div>
      )}

      {/* 进度尾迹只在真正执行时展示（waiting = 审批/范围确认门等待中，还没有任何输出；已中断/已结束的卡 tail 已被 store 清空） */}
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
                  {f.kind === "image" && <ReadImage file={f} />}
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
          ) : tool.tool === "ask" ? (
            <>
              {/* 出参为空（取消 / 未作答）时回退渲染入参：那时「问了什么」才是用户要看的 */}
              {showArgsInstead ? (
                <>
                  <div className="kv dim">入参</div>
                  <CodeBlock code={argsJson} language="json" />
                </>
              ) : (
                <>
                  <div className="kv dim">{t("tools.rawData")}</div>
                  <CodeBlock code={prettyJson} language="json" />
                </>
              )}
            </>
          ) : (
            <>
              {/* 回退渲染入参时必须标一行说明，否则会被误认成出参（JSON 长得一样） */}
              {showArgsInstead && <div className="kv dim">入参</div>}
              <CodeBlock code={showArgsInstead ? argsJson : prettyJson} language="json" />
            </>
          )}

          {/* E_INTERRUPTED 的 message 为空（store 落定形态）：渲染成「E_INTERRUPTED: 」是纯噪声，
              verb 已表达「已中断」——故不渲染错误行；ask 两类的中性化行为保持不变。 */}
          {tool.outcome?.error && errCode !== "E_INTERRUPTED" && (
            <div className={neutralErr ? "err err-neutral" : "err"}>
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

/** read 结果里的图片：优先用事件带来的 `data_url` 直接渲染；没有 `data_url` 时（重开会话后从历史
 *  重建的卡片——历史里不再保留那段 base64）按路径重新读出来显示；读失败（文件被移动/删除/
 *  超出可读边界）才给占位提示。模型未勾选「支持图片输入」时另给一行提示。 */
function ReadImage({ file }: { file: any }) {
  const { t } = useTranslation();
  const sessionId = useSessions((s) => s.activeKey);
  const inline: string | undefined = typeof file?.data_url === "string" ? file.data_url : undefined;
  const path: string = typeof file?.path === "string" ? file.path : "";
  const [state, setState] = useState<{ phase: "ok" | "loading" | "fail"; url?: string }>(
    inline ? { phase: "ok", url: inline } : { phase: "loading" },
  );

  useEffect(() => {
    if (inline) {
      setState({ phase: "ok", url: inline });
      return;
    }
    if (!path || !sessionId) {
      setState({ phase: "fail" });
      return;
    }
    let cancelled = false;
    setState({ phase: "loading" });
    ipc
      .readWorkspaceFileBase64(sessionId, path)
      .then((r) => {
        if (cancelled) return;
        const mime = typeof file?.media_type === "string" ? file.media_type : "image/png";
        setState({ phase: "ok", url: `data:${mime};base64,${r.content}` });
      })
      .catch(() => {
        if (!cancelled) setState({ phase: "fail" });
      });
    return () => {
      cancelled = true;
    };
  }, [inline, path, sessionId, file?.media_type]);

  return (
    <>
      {state.phase === "ok" && state.url && <img src={state.url} className="img" alt="" />}
      {state.phase === "loading" && <div className="kv dim">{t("tools.imageLoading")}</div>}
      {state.phase === "fail" && <div className="kv dim">{t("tools.imageUnavailable", { path })}</div>}
      {file?.sent_to_model === false && <div className="kv dim">{t("tools.imageNotSent")}</div>}
    </>
  );
}

const ToolCallCard = memo(ToolCallCardImpl);
export default ToolCallCard;
