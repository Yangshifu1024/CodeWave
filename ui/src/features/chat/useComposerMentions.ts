// Composer @ 提及 / / 技能 / $ 子代理建议菜单（[docs/fence-hardening-and-powershell-ast](../../../../docs/fence-hardening-and-powershell-ast.md) 重构；
// 触发判定由「整段末尾锚定」改为「光标前片段」见 [docs/composer-trigger-caret](../../../../docs/composer-trigger-caret.md)）：
// 提及两级检索（会话根目录置顶，其次工作区路径搜索）、技能过滤与子代理角色过滤，
// 各带乱序守卫（mentionSeq/skillSeq/agentSeq），只有最新一次查询的结果会落地。
import { useRef, useState } from "react";
import { ipc } from "../../ipc/client";
import type { AgentMeta, SkillMeta } from "../../ipc/types";
import { useSessions } from "../../stores/sessions";

/** Composer 提及/技能/子代理建议 hook：维护 @ 文件目录、/ 技能与 $ 子代理三组候选列表，
 *  提供刷新（含乱序守卫）与选中回填（替换输入框中的 @…/…/$… 前缀片段）。 */
export function useComposerMentions(opts: {
  setActiveIndex: React.Dispatch<React.SetStateAction<number>>;
  /** 选中「文件」时的引用入列（[docs/composer-file-ref-chips](../../../../docs/composer-file-ref-chips.md)：
   *  文件以 chip 展示，不再把路径写进正文；目录仍写文本以便继续拼路径） */
  addFileRef: (path: string) => void;
  /** 回填：把「光标处那段触发片段」替换成传入文本（只动光标前那段，光标之后一字不改）——
   *  实现在 Composer（那里才有真正的光标位置），见 [docs/composer-trigger-caret](../../../../docs/composer-trigger-caret.md) */
  replaceFragment: (insert: string) => void;
}) {
  const { setActiveIndex, addFileRef, replaceFragment } = opts;
  // 两级提及：@ 先列出项目目录（置顶），继续输入再匹配具体文件
  const [mentionResults, setMentionResults] = useState<{ label: string; path: string; isDir: boolean }[]>([]);
  const mentionSeq = useRef(0);
  // / 技能候选（[docs/composer-toolbar-batch-report](../../../../docs/composer-toolbar-batch-report.md)；
  // 触发符自 $ 改为 /，[docs/slash-skills-and-dollar-agents](../../../../docs/slash-skills-and-dollar-agents.md)）
  const [skillResults, setSkillResults] = useState<SkillMeta[]>([]);
  const skillSeq = useRef(0);
  // $ 内置子代理候选（list_agents IPC，后端注册表剔除内部 title 角色）
  const [agentResults, setAgentResults] = useState<AgentMeta[]>([]);
  const agentSeq = useRef(0);

  async function refreshMention(q: string) {
    const st = useSessions.getState();
    const sessionId = st.activeKey;
    if (!sessionId) {
      setMentionResults([]);
      return;
    }
    // 提及第一级：会话的全部根目录（项目会话 = [项目目录]；临时会话 = [主目录]）
    const tab = st.tabs.find((t) => t.key === sessionId);
    const project = tab?.projectId ? st.projects.find((p) => p.id === tab.projectId) : null;
    const roots: string[] = project
      ? [project.directory].filter(Boolean)
      : tab
        ? [tab.workspace]
        : [];
    const ql = q.toLowerCase();
    const dirItems = roots
      .filter((r) => !ql || r.toLowerCase().includes(ql) || dirNameOf(r).toLowerCase().includes(ql))
      .map((r) => ({ label: "📁 " + dirNameOf(r), path: r, isDir: true }));
    const seq = ++mentionSeq.current;
    const files = q ? await ipc.searchWorkspacePaths(sessionId, q, 8).catch(() => []) : [];
    if (seq !== mentionSeq.current) return; // 乱序守卫：只接受最新一次查询
    setMentionResults([...dirItems, ...files.map((f) => ({ label: f, path: f, isDir: false }))]);
    setActiveIndex(0); // 列表刷新后回到顶部项（↑↓ 仍可移动）
  }

  async function refreshSkills(q: string) {
    const sessionId = useSessions.getState().activeKey;
    const seq = ++skillSeq.current;
    const all = await ipc.listSkills(sessionId).catch(() => []);
    if (seq !== skillSeq.current) return; // 乱序守卫：只接受最新一次查询
    const ql = q.toLowerCase();
    setSkillResults(all.filter((s) => !q || s.name.toLowerCase().includes(ql)).slice(0, 10));
    setActiveIndex(0); // 列表刷新后回到顶部项（↑↓ 仍可移动）
  }

  async function refreshAgents(q: string) {
    const seq = ++agentSeq.current;
    const all = await ipc.listAgents().catch(() => []);
    if (seq !== agentSeq.current) return; // 乱序守卫：只接受最新一次查询
    const ql = q.toLowerCase();
    setAgentResults(all.filter((a) => !q || a.name.toLowerCase().includes(ql)).slice(0, 10));
    setActiveIndex(0); // 列表刷新后回到顶部项（↑↓ 仍可移动）
  }

  function dirNameOf(p: string): string {
    return p.split("/").filter(Boolean).pop() || p;
  }

  // 选中提及项：目录回填 `@目录/ `（继续拼路径的字面输入）；文件改以引用 chip 呈现——
  // 把正在输入的 `@片段` 从正文抹掉并可入 refs。两种都只替换「光标处那段片段」（replaceFragment），
  // 光标之后的正文一字不动（[docs/composer-trigger-caret]）
  function pickMention(item: { path: string; isDir: boolean }) {
    if (item.isDir) replaceFragment(`@${item.path}/ `);
    else {
      replaceFragment("");
      addFileRef(item.path);
    }
    setMentionResults([]);
  }

  // 技能回填 /<name> 前缀（发送后由系统提示词 <available-skills> 的点名语义引导模型加载技能）
  function pickSkill(item: SkillMeta) {
    replaceFragment(`/${item.name} `);
    setSkillResults([]);
  }

  // 子代理回填 $<role> 前缀（发送后由核心提示 $<role> 点名规则引导主代理经 subagent 工具委派）
  function pickAgent(item: AgentMeta) {
    replaceFragment(`$${item.name} `);
    setAgentResults([]);
  }

  // 收起菜单 + 作废在途查询（自增 seq）：防止切走触发符后迟到的结果把菜单又顶出来
  function clearMentions() {
    mentionSeq.current++;
    setMentionResults([]);
  }
  function clearSkills() {
    skillSeq.current++;
    setSkillResults([]);
  }
  function clearAgents() {
    agentSeq.current++;
    setAgentResults([]);
  }

  return {
    mentionResults,
    skillResults,
    agentResults,
    refreshMention,
    refreshSkills,
    refreshAgents,
    pickMention,
    pickSkill,
    pickAgent,
    clearMentions,
    clearSkills,
    clearAgents,
  };
}
