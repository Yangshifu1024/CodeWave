import { useEffect, useRef, useState } from "react";
import { App, BorderBeam, Button, Dropdown, Image, Input, Popover } from "antd";
import type { MenuProps } from "antd";
import {
  ArrowUpOutlined, BulbOutlined, CheckCircleOutlined, CloseOutlined, CodeOutlined,
  DownOutlined, ExclamationCircleOutlined, FileAddOutlined, FileTextOutlined,
  LoadingOutlined, PlusOutlined, RobotOutlined, SafetyCertificateOutlined,
  SettingOutlined, StopOutlined,
} from "@ant-design/icons";
import { useTranslation } from "react-i18next";
import { useActiveRun, useActiveDraft, useRun, useContextPct } from "../../stores/run";
import { useActiveTab, useSessions } from "../../stores/sessions";
import { useSettings } from "../../stores/settings";
import { useUi } from "../../stores/ui";
import type { ApprovalMode, EffortLevel } from "../../ipc/types";
import { findModel } from "../../utils/models";
import CompactButton from "./ContextInfoBar";
import AskPanel from "../tools/AskPanel";
import QueuePanel from "./QueuePanel";
import { useComposerAttachments } from "./useComposerAttachments";
import { useComposerHistory } from "./useComposerHistory";
import { useComposerMentions } from "./useComposerMentions";
import { useComposerEvents } from "./useComposerEvents";

const { TextArea } = Input;

// Shift+Tab 循环的权限档顺序（与权限下拉菜单项顺序一致）
const MODE_ORDER: ApprovalMode[] = ["confirm_each", "auto_edit", "plan", "full_access"];

/** Composer：底部输入区 + 工具条（+/权限/子代理/压缩/上下文/模型/力度/发送）。
 *  键盘契约：Enter 发送、Shift+Enter 换行、Shift+Tab 循环权限档、空输入 ↑ 进入历史浏览、
 *  / 触发技能菜单、$ 触发子代理菜单、@ 触发提及菜单（↑↓ 导航、Enter/Tab 选中）；
 *  IME 组合期按键全部放行。ask 弹出时提问卡整体覆盖本组件与队列面板，回答后原样恢复
 * （草稿存 run store 每 Tab 桶，不丢）。触发符语义见 [docs/slash-skills-and-dollar-agents](../../../../docs/slash-skills-and-dollar-agents.md)。 */
export default function Composer() {
  const { t } = useTranslation();
  const { message } = App.useApp();
  const active = useActiveRun();
  // [docs/subagent-interaction-drawer](../../../../docs/subagent-interaction-drawer.md)：运行中的子代理（Composer 指示器的计数与菜单数据源）
  const runningSubs = active.subs.filter((s) => s.status === "running");
  const tab = useActiveTab();
  const contextPct = useContextPct();
  const config = useSettings((s) => s.config);
  const prefs = tab?.prefs ?? { approval_mode: "auto_edit" as ApprovalMode, model_id: null, reasoning_effort: null };
  const updatePrefs = useSessions((s) => s.updatePrefs);
  // [docs/ask-ink-accent-and-composer-cover](../../../../docs/ask-ink-accent-and-composer-cover.md)：ask/审批弹出时提问卡覆盖整个输入区（zcode 式：ask 面板是唯一底部输入），
  // Composer 本体与队列面板暂不渲染、回答后原样恢复；草稿存 run store 每 Tab 桶，子树卸载不丢
  const askActive = !!active.ask;

  // 草稿按 Tab 隔离：文本与待发附件存 run store 平行分桶（drafts[key]，见 ComposerDraft 注释），切会话各自保留、
  // 发送成功 clearDraft 清空；setText/setImages 与 useState 同形（支持 updater），直接注入下方子 hooks
  const draft = useActiveDraft();
  const text = draft.text;
  const setText = useRun.getState().setDraftText;
  const setDraftImages = useRun.getState().setDraftImages;
  // 流光显隐（[docs/ask-ink-accent-and-composer-cover](../../../../docs/ask-ink-accent-and-composer-cover.md)）：输入框聚焦态——仅输入框聚焦或任务进行中时出现
  const [composerFocused, setComposerFocused] = useState(false);
  // 隐藏走 composer-beam-idle（app.css 中 display:none），动画停摆、零绘制
  const beamActive = active.running || composerFocused;
  // / 合并菜单的开合由 text 派生（/^\/(\S*)$/ 首 token 输入中），无需独立 state
  // 菜单高亮下标（三个菜单互斥，共用一个下标）
  const [activeIndex, setActiveIndex] = useState(0);
  // IME 组合追踪（WebKit 差异）：WebKit 以 isComposing=false（keyCode 229）触发组合确认的 Enter keydown，
  // 单看 nativeEvent.isComposing 不够——经 composition 事件自行维护真值。
  const composingRef = useRef(false);
  const taRef = useRef<any>(null);

  // 聚焦关注点的 hooks（[docs/fence-hardening-and-powershell-ast](../../../../docs/fence-hardening-and-powershell-ast.md) 重构）：附件 / 全局事件 / 历史召回 / 提及·技能·子代理菜单
  const attachments = useComposerAttachments({ t, message, images: draft.images, setImages: setDraftImages });
  const { images, setImages, fileRef, recalledImages, addFiles, onPaste } = attachments;
  useComposerEvents({ taRef, setText, setImages, recalledImages });
  const history = useComposerHistory({
    tabKey: tab?.key,
    setText,
    setImages,
    recalledImages,
  });
  const mentions = useComposerMentions({ setText, setActiveIndex });
  const { histIdx, setHistIdx, draftRef, recallHistory, applyRecall, exitRecall } = history;
  const {
    mentionResults, skillResults, agentResults,
    refreshMention, refreshSkills, refreshAgents,
    pickMention, pickSkill, pickAgent,
    clearMentions, clearSkills, clearAgents,
  } = mentions;

  // 会话切换：收起提及/技能/子代理菜单（候选是上一会话的查询结果，残留会把旧菜单顶进新会话）并复位高亮
  const tabKey = tab?.key;
  useEffect(() => {
    clearMentions();
    clearSkills();
    clearAgents();
    setActiveIndex(0);
  }, [tabKey]);

  // [docs/run-queue-and-ask-revamp](../../../../docs/run-queue-and-ask-revamp.md)：队列条目「编辑」-> 文本与附件回填输入框并聚焦（附件复用历史召回图片同一兜底：上限 4 张 / 20MB）。
  // 回填显式锁定点击所在 Tab：effect 提交前切 Tab 也不会把队列内容写进新会话、或因消费落空而二次回填
  const draftFromQueue = active.draftFromQueue;
  useEffect(() => {
    if (draftFromQueue == null) return;
    const targetKey = tab?.key;
    useRun.getState().setDraftText(draftFromQueue.text, targetKey);
    if (draftFromQueue.images?.length) {
      useRun.getState().setDraftImages(
        recalledImages(draftFromQueue.images.map((im) => ({ mediaType: im.mime, data: im.data }))),
        targetKey,
      );
    }
    useRun.getState().consumeDraftFromQueue(targetKey);
    const el = taRef.current?.resizableTextArea?.textArea ?? taRef.current;
    el?.focus?.();
  }, [draftFromQueue]);

  // 会话生效模型：会话覆盖 -> 全局活跃（展示与发送守卫同一数据源，[docs/composer-toolbar-batch-report](../../../../docs/composer-toolbar-batch-report.md)；摊平视图 [docs/provider-management-refactor](../../../../docs/provider-management-refactor.md)）
  const globalModel = findModel(config, config?.active_model_id ?? null);
  const sessionModel = prefs.model_id ? findModel(config, prefs.model_id) : null;
  const effectiveModel = sessionModel ?? globalModel;
  const effortValue: EffortLevel | "default" = prefs.reasoning_effort ?? "default";

  // （命令入口已自 / 菜单移除：git/diff/tasks/stats 走顶栏四入口、compact 走工具条按钮，
  //   [docs/slash-skills-and-dollar-agents](../../../../docs/slash-skills-and-dollar-agents.md)）

  /** 从 + 菜单插入触发字符（@ / / / $）并聚焦；匹配弹层经 onInputChange 打开 */
  function insertTrigger(ch: string) {
    const next = text === "" || text.endsWith(" ") || text.endsWith("\n") ? text + ch : text + " " + ch;
    setText(next);
    void onInputChange(next);
    requestAnimationFrame(() => taRef.current?.focus());
  }

  // 菜单键盘导航：↑↓ 移动高亮，Enter/Tab 选中；取模防越界（列表变短也安全）
  // 三个互斥菜单共用同一高亮下标：/ 技能 -> $ 子代理 -> @ 提及
  // （触发符语义 [docs/slash-skills-and-dollar-agents](../../../../docs/slash-skills-and-dollar-agents.md)）
  const agentOpen = agentResults.length > 0;
  // / 技能菜单：首 token（/ 起始、未含空白）输入中即激活；条目 = refreshSkills(query) 过滤结果
  const slashQuery = /^\/(\S*)$/.exec(text)?.[1] ?? null;
  const slashOpen = slashQuery !== null && skillResults.length > 0;
  const menuCount = slashOpen
    ? skillResults.length
    : agentOpen
      ? agentResults.length
      : mentionResults.length;
  const selIdx = menuCount ? activeIndex % menuCount : 0;

  function moveMenu(delta: number) {
    setActiveIndex((i) => {
      if (!menuCount) return 0;
      return (i + delta + menuCount) % menuCount;
    });
  }

  function onKeydown(e: React.KeyboardEvent) {
    // IME 守卫（三重检查）：Chromium 以 isComposing=true（keyCode 229）标记组合中的 keydown，WebKit
    // 则以 isComposing=false（keyCode 229）触发组合确认的 Enter——因此同时认可 keyCode 229 与
    // composition 事件追踪的自有状态。覆盖组合窗口与确认 keydown 两个阶段；真正的 Enter
    // （无组合、keyCode 13）不受影响。
    const native = e.nativeEvent as KeyboardEvent;
    if (native.isComposing || native.keyCode === 229 || composingRef.current) return;
    if (e.key === "Tab" && e.shiftKey) {
      // Shift+Tab：四档权限模式循环（[docs/composer-shift-tab-mode-cycle](../../../../docs/composer-shift-tab-mode-cycle.md)）；放在菜单/发送处理之前，@/$ 菜单开着也能切
      e.preventDefault();
      const next = MODE_ORDER[(MODE_ORDER.indexOf(prefs.approval_mode) + 1) % MODE_ORDER.length];
      switchMode(next);
      return;
    }
    if (e.key === "Escape") {
      if (histIdx !== null) {
        exitRecall(); // 浏览中：退出并还原进入前草稿
        return;
      }
      clearMentions();
      clearSkills();
      clearAgents();
      return;
    }
    if (slashOpen || agentOpen || mentionResults.length > 0) {
      if (e.key === "ArrowDown" || e.key === "ArrowUp") {
        e.preventDefault(); // 方向键在菜单打开时用于导航，不移动输入框内光标
        moveMenu(e.key === "ArrowDown" ? 1 : -1);
        return;
      }
      if (e.key === "Enter" && !e.shiftKey) {
        e.preventDefault();
        if (slashOpen) {
          const s = skillResults[selIdx];
          if (s) pickSkill(s);
        } else if (agentOpen) {
          const a = agentResults[selIdx];
          if (a) pickAgent(a);
        } else {
          const item = mentionResults[selIdx];
          if (item) pickMention(item);
        }
        return;
      }
      if (e.key === "Tab" && (agentOpen || mentionResults.length > 0)) {
        e.preventDefault();
        if (agentOpen) {
          const a = agentResults[selIdx];
          if (a) pickAgent(a);
        } else {
          const item = mentionResults[selIdx];
          if (item) pickMention(item);
        }
        return;
      }
    }
    // ↑↓ 历史召回：空输入时 ↑ 进入浏览态（最新一条）；浏览中 ↑ 向旧翻（到最旧即停）/ ↓ 向新翻
    // 输入非空且未浏览时不劫持方向键（保留多行光标语义）；打开的菜单已在上方菜单分支消费
    if (e.key === "ArrowUp" && (text === "" || histIdx !== null)) {
      e.preventDefault();
      const hist = recallHistory();
      if (hist.length === 0) return; // 无历史，不动作
      if (histIdx !== null) {
        const prev = Math.max(0, histIdx - 1); // 停在最旧而不是回绕
        applyRecall(hist[prev], prev);
      } else {
        draftRef.current = { text, images }; // 进入浏览态：快照当前草稿
        applyRecall(hist[hist.length - 1], hist.length - 1);
      }
      return;
    }
    if (e.key === "ArrowDown" && histIdx !== null) {
      e.preventDefault();
      const hist = recallHistory();
      const next = histIdx + 1;
      if (next >= hist.length) {
        exitRecall(); // 越过最新：退出浏览态并还原进入前草稿
        return;
      }
      applyRecall(hist[next], next);
      return;
    }
    if (e.key === "Enter" && !e.shiftKey) {
      e.preventDefault();
      send();
    }
  }

  async function onInputChange(v: string) {
    if (histIdx !== null) {
      setHistIdx(null); // 编辑即退出浏览态：保留当前文本作为编辑基底
      draftRef.current = null;
    }
    setText(v);
    // / 合并菜单（命令 + 技能，[docs/slash-skills-and-dollar-agents](../../../../docs/slash-skills-and-dollar-agents.md)）：
    // 首 token（/ 起始、未含空白）输入中即激活；空格后（如 "/repo-index 分析 X"）菜单收起，Enter 正常发送
    const slash = /^\/(\S*)$/.exec(v);
    if (slash) {
      clearMentions();
      clearAgents();
      await refreshSkills(slash[1]);
      return;
    }
    clearSkills(); // 离开 / 首 token：作废在途技能查询并收起菜单
    const at = v.match(/@([^@\s]*)$/);
    const dollar = v.match(/\$([^$\s]*)$/); // $ 触发子代理菜单（原技能触发符，已让位给 /）
    if (at) {
      clearAgents();
      await refreshMention(at[1]);
    } else if (dollar) {
      clearMentions();
      await refreshAgents(dollar[1]);
    } else {
      clearMentions();
      clearAgents();
    }
  }

  // ---------- 发送 ----------

  async function send() {
    const v = text.trim();
    if (!v) return;
    if (!effectiveModel) {
      useRun.getState().pushItem(useSessions.getState().activeKey, { kind: "error", text: t("app.needModel") });
      useUi.getState().showSettings("providers");
      return;
    }
    // vision 软阻断：模型声明不支持图片（vision === false）且有附件时拦截（自 [docs/provider-management-refactor](../../../../docs/provider-management-refactor.md) 起为硬布尔）
    if (images.length > 0 && effectiveModel && !effectiveModel.vision) {
      message.warning(t("composer.visionUnsupported"));
      return;
    }
    // M-2：接受后才清空——运行中按 Enter 不再静默吞掉输入。
    // 目标 Tab 在调用时捕获：startChat 异步窗口内切 Tab，清空的仍是发送方草稿而非新会话的
    const targetKey = useSessions.getState().activeKey ?? undefined;
    const accepted = await useRun.getState().send(v, images.map(({ mime, data }) => ({ mime, data })));
    if (accepted) {
      useRun.getState().clearDraft(targetKey); // 文本 + 附件一并清空（发送方 Tab 桶）
      setHistIdx(null); // 发送后序列自然追加新消息；复位指针
      draftRef.current = null;
    }
  }

  // 上下文明细文本（工具条）：有明细时显示百分比 + 用量/窗口；否则显示 —（尚无 run）
  const ctxLabel = active.breakdown
    ? `${contextPct}%（${Math.round(active.breakdown.total_tokens / 100) / 10}k / ${Math.round(active.breakdown.context_window / 100) / 10}k）`
    : "—";

  // 发送按钮三态：空闲有输入 = 发送；运行中无输入 = 停止；运行中有输入 = 提交（入队）
  const hasDraft = !!text.trim();
  const stopActive = active.running && !hasDraft;

  const menuStyle = { maxHeight: 320, overflowY: "auto" } as const;

  // ---------- 下拉菜单 ----------

  const rich = (title: string, desc: string, cls?: string) => (
    <div className={cls ? `menu-item-rich ${cls}` : "menu-item-rich"}>
      <span>{title}</span>
      <span className="desc">{desc}</span>
    </div>
  );

  const modeDescKeys: Record<ApprovalMode, string> = {
    confirm_each: "composer.modeConfirmEachDesc",
    auto_edit: "composer.modeAutoEditDesc",
    plan: "composer.modePlanDesc",
    full_access: "composer.modeFullAccessDesc",
  };
  const modeLabels: Record<ApprovalMode, string> = {
    confirm_each: t("composer.modeConfirmEach"),
    auto_edit: t("composer.modeAutoEdit"),
    plan: t("composer.modePlan"),
    full_access: t("composer.modeFullAccess"),
  };
  const modeIcons: Record<ApprovalMode, React.ReactNode> = {
    confirm_each: <ExclamationCircleOutlined />,
    auto_edit: <CheckCircleOutlined />,
    plan: <FileTextOutlined />,
    full_access: <SafetyCertificateOutlined />,
  };
  // 权限档着色（[docs/composer-shift-tab-mode-cycle](../../../../docs/composer-shift-tab-mode-cycle.md) §5）：确认 = 蓝（primary）/ 自动编辑 = 橙 / 完全访问 = 红（危险）；plan 档不着色。
  // 同一映射同时供给触发胶囊（按钮 className）与下拉项（rich 标题着色）
  const modeClass: Partial<Record<ApprovalMode, string>> = {
    confirm_each: "approval-confirm",
    auto_edit: "approval-auto",
    full_access: "approval-full",
  };
  // 菜单点击与 Shift+Tab 共用（[docs/composer-shift-tab-mode-cycle](../../../../docs/composer-shift-tab-mode-cycle.md)）。后端在每次 LLM 轮 / 工具调用时实时读取 prefs，
  // 因此流式进行中切档也会在下一个请求 / 下一次工具调用生效
  const switchMode = (mode: ApprovalMode) => {
    void updatePrefs(tab?.key ?? "", { approval_mode: mode });
    message.success(t("composer.modeSwitched", { label: modeLabels[mode] }));
  };
  const switchModel = (id: string) => {
    void updatePrefs(tab?.key ?? "", { model_id: id });
    const m = (config?.providers ?? []).flatMap((p) => p.models).find((x) => x.id === id);
    message.success(t("composer.modelSwitched", { label: m ? m.model : id }));
  };
  const approvalMenu: MenuProps = {
    selectedKeys: [prefs.approval_mode],
    items: (Object.entries(modeIcons) as [ApprovalMode, React.ReactNode][]).map(([mode, icon]) => ({
      key: mode,
      icon,
      label: rich(modeLabels[mode], t(modeDescKeys[mode]), modeClass[mode]),
    })),
    onClick: ({ key }) => switchMode(key as ApprovalMode),
  };

  // 模型菜单分组 = 供应商（config 顺序即菜单顺序，[docs/provider-management-refactor](../../../../docs/provider-management-refactor.md)）；空名回退「其他」
  const modelMenu: MenuProps = {
    selectedKeys: effectiveModel ? [effectiveModel.id] : [],
    items: [
      ...(config?.providers ?? [])
        .filter((p) => p.models.length > 0)
        .map((p) => ({
          type: "group" as const,
          label: p.name || t("composer.other"),
          children: p.models.map((m) => ({
            key: m.id,
            label: (
              <>
                {m.model}
                {m.vision && <span className="vision-tag">{t("composer.vision")}</span>}
              </>
            ),
          })),
        })),
      { type: "divider" },
      { key: "__manage", icon: <SettingOutlined />, label: t("composer.manageProviders") },
    ],
    onClick: ({ key }) => {
      if (key === "__manage") {
        useUi.getState().showSettings("providers");
        return;
      }
      switchModel(key);
    },
  };

  const effortLabels: Record<EffortLevel, string> = {
    low: t("composer.effortLow"),
    medium: t("composer.effortMedium"),
    high: t("composer.effortHigh"),
    max: t("composer.effortMax"),
  };
  const effortMenu: MenuProps = {
    selectedKeys: [effortValue],
    items: [
      { key: "default", label: rich(t("composer.effortDefault"), t("composer.effortDefaultDesc")) },
      { type: "divider" },
      ...(["low", "medium", "high", "max"] as EffortLevel[]).map((e) => ({
        key: e,
        label: effortLabels[e],
      })),
    ],
    onClick: ({ key }) =>
      void updatePrefs(tab?.key ?? "", { reasoning_effort: key === "default" ? null : (key as EffortLevel) }),
  };

  const plusMenu: MenuProps = {
    items: [
      { key: "attach", icon: <FileAddOutlined />, label: t("composer.attach") },
      { key: "at", label: t("composer.useAt") },
      { key: "slash", label: t("composer.useSlash") },
      { key: "dollar", label: t("composer.useDollar") },
    ],
    onClick: ({ key }) => {
      if (key === "attach") {
        fileRef.current?.click();
      } else if (key === "at") {
        insertTrigger("@");
      } else if (key === "slash") {
        insertTrigger("/");
      } else if (key === "dollar") {
        insertTrigger("$");
      }
    },
  };

  return (
    <div className="composer-wrap">
      {askActive ? (
        <AskPanel />
      ) : (
        <>
          <QueuePanel />

      {/* / 技能菜单（list_skills IPC，命令入口已移除）：点击/Enter 回填 /<name> 前缀，
          发送后由 <available-skills> 的点名语义引导模型加载技能（技能详情弹层在右栏技能行） */}
      <Popover
        open={slashOpen}
        placement="topLeft"
        arrow={false}
        content={
          <div className="menu" style={menuStyle}>
            {skillResults.map((s, i) => (
              <Button
                key={s.name}
                type="text"
                size="small"
                className={`menu-item${i === selIdx ? " active" : ""}`}
                style={{ justifyContent: "flex-start", display: "flex", width: "100%" }}
                onMouseEnter={() => setActiveIndex(i)}
                onClick={() => pickSkill(s)}
              >
                <code style={{ marginRight: 8, color: "var(--ws-accent)" }}>/{s.name}</code>
                <span className="dim">{s.description}</span>
              </Button>
            ))}
          </div>
        }
      >
        <span />
      </Popover>

      <Popover
        open={mentionResults.length > 0}
        placement="topLeft"
        arrow={false}
        content={
          <div className="menu" style={menuStyle}>
            {mentionResults.map((item, i) => (
              <Button
                key={item.path}
                type="text"
                size="small"
                className={`menu-item${i === selIdx ? " active" : ""}`}
                style={{ justifyContent: "flex-start", display: "flex", width: "100%" }}
                onMouseEnter={() => setActiveIndex(i)}
                onClick={() => pickMention(item)}
              >
                <code style={item.isDir ? { color: "var(--ws-accent)" } : undefined}>{item.label}</code>
              </Button>
            ))}
          </div>
        }
      >
        <span />
      </Popover>

      {/* $ 子代理菜单（list_agents IPC）：回填 $<role> 前缀，发送后由核心提示 $<role> 点名规则
          引导主代理经 subagent 工具委派；进度在子代理卡/抽屉展示 */}
      <Popover
        open={agentResults.length > 0}
        placement="topLeft"
        arrow={false}
        content={
          <div className="menu" style={menuStyle}>
            {agentResults.map((a, i) => (
              <Button
                key={a.name}
                type="text"
                size="small"
                className={`menu-item${i === selIdx ? " active" : ""}`}
                style={{ justifyContent: "flex-start", display: "flex", width: "100%" }}
                onMouseEnter={() => setActiveIndex(i)}
                onClick={() => pickAgent(a)}
              >
                <code style={{ marginRight: 8, color: "var(--ws-accent)" }}>${a.name}</code>
                <span className="dim">{a.description}</span>
              </Button>
            ))}
          </div>
        }
      >
        <span />
      </Popover>

      <input
        ref={fileRef}
        type="file"
        accept="image/*"
        multiple
        style={{ display: "none" }}
        onChange={(e) => {
          void addFiles(e.target.files);
          e.target.value = ""; // 允许重复选择同一文件
        }}
      />

      <div className="composer">
        {/* 多条流光边框（docs/antd6-upgrade-and-composer-border-beam，antd 6 BorderBeam）：条纹均匀分布；颜色跟随主题强调色，
            antd 内部已处理 prefers-reduced-motion 隐藏。
            时机收敛（docs/ask-ink-accent-and-composer-cover）：仅输入框聚焦 / 任务进行中显示（原 :focus-within 强调色边框取消，
            流光即聚焦反馈）；时长不变：空闲 12s 慢速巡航、任务运行 3s 提速 */}
        <BorderBeam
          count={3}
          duration={active.running ? 3 : 12}
          color="var(--ws-accent)"
          className={beamActive ? undefined : "composer-beam-idle"}
        >
          <div className="composer-card">
          {images.length > 0 && (
            <div className="composer-attachments">
              {/* 缩略图点击打开大图预览（多图可切换），与会话内已发送图片同一交互；
                  .x 删除按钮在预览触发层之外，点击不会误开预览 */}
              <Image.PreviewGroup>
                {images.map((img) => (
                  <span key={img.id} className="attach-chip">
                    <Image
                      src={img.dataUrl}
                      alt={img.name}
                      width={28}
                      height={28}
                      preview={{ src: img.dataUrl }}
                      style={{ borderRadius: 4, flex: "none" }}
                    />
                    <span className="attach-name">{img.name}</span>
                    <CloseOutlined className="x" onClick={() => setImages((v) => v.filter((x) => x.id !== img.id))} />
                  </span>
                ))}
              </Image.PreviewGroup>
            </div>
          )}

          <TextArea
            className="composer-input"
            variant="borderless"
            value={text}
            autoSize={{ minRows: 2, maxRows: 8 }}
            placeholder={active.running ? t("composer.queuePlaceholder") : t("app.inputPlaceholder")}
            spellCheck={false}
            onFocus={() => setComposerFocused(true)}
            onBlur={() => setComposerFocused(false)}
            onChange={(e) => void onInputChange(e.target.value)}
            onKeyDown={onKeydown}
            onCompositionStart={() => {
              composingRef.current = true;
            }}
            onCompositionEnd={() => {
              composingRef.current = false;
            }}
            onPaste={onPaste}
            ref={taRef}
          />

          <div className="composer-toolbar">
            <div className="toolbar-left">
              <Dropdown menu={plusMenu} trigger={["click"]}>
                <Button type="text" icon={<PlusOutlined />} title={t("composer.attach")} />
              </Dropdown>
              <Dropdown menu={approvalMenu} trigger={["click"]}>
                <Button
                  type="text"
                  className={modeClass[prefs.approval_mode]}
                  title={t("composer.modeShortcutHint")}
                >
                  {modeIcons[prefs.approval_mode]}
                  <span className="tb-label">{modeLabels[prefs.approval_mode]}</span>
                  <DownOutlined className="tb-chev" />
                </Button>
              </Dropdown>
              {/* docs/subagent-interaction-drawer：运行中子代理指示器（计数 = 运行中子代理数，0 隐藏）。
                  单个时点击直达过程抽屉；多个时弹向上菜单、选中后打开。 */}
              {runningSubs.length === 1 && (
                <Button
                  type="text"
                  className="subs-indicator"
                  title={t("subagent.runningCount", { n: 1 })}
                  icon={<RobotOutlined />}
                  onClick={() => void useRun.getState().openSubDrawer(null, runningSubs[0].subId)}
                >
                  <span className="tb-label">{runningSubs.length}</span>
                </Button>
              )}
              {runningSubs.length > 1 && (
                <Dropdown
                  trigger={["click"]}
                  placement="topLeft"
                  menu={{
                    items: runningSubs.map((s) => ({
                      key: s.subId,
                      label: (
                        <span className="subs-menu-item">
                          <RobotOutlined />
                          <span className="subs-menu-role">{s.name || s.role}</span>
                          {s.description && <span className="subs-menu-desc">· {s.description}</span>}
                          <span className={`subs-menu-st st-${s.status}`}>
                            {s.status === "running" ? "●" : s.status === "error" ? "✕" : "✓"}
                          </span>
                        </span>
                      ),
                    })),
                    onClick: ({ key }) => void useRun.getState().openSubDrawer(null, key),
                  }}
                >
                  <Button
                    type="text"
                    className="subs-indicator"
                    title={t("subagent.runningCount", { n: runningSubs.length })}
                    icon={<RobotOutlined />}
                  >
                    <span className="tb-label">{runningSubs.length}</span>
                    <DownOutlined className="tb-chev" />
                  </Button>
                </Dropdown>
              )}
            </div>
            <div className="toolbar-right">
              <CompactButton />
              <span className="ctx-label" title={t("app.context")}>{t("app.context")} {ctxLabel}</span>
              <Dropdown menu={modelMenu} trigger={["click"]}>
                <Button
                  type="text"
                  aria-label={t("app.model")}
                  title={effectiveModel ? `${effectiveModel.providerName} · ${effectiveModel.model}` : t("composer.goSettings")}
                >
                  <span className="tb-label">{effectiveModel?.model ?? t("composer.noModel")}</span>
                  <DownOutlined className="tb-chev" />
                </Button>
              </Dropdown>
              <Dropdown menu={effortMenu} trigger={["click"]}>
                <Button type="text" title={t("settings.reasoning")}>
                  <BulbOutlined />
                  <span className="tb-label">
                    {effortValue === "default" ? t("composer.effortDefault") : effortLabels[effortValue]}
                  </span>
                  <DownOutlined className="tb-chev" />
                </Button>
              </Dropdown>
              <Button
                className="send-btn"
                shape="circle"
                danger={stopActive}
                type="primary"
                disabled={!active.running && !hasDraft}
                aria-label={stopActive ? t("app.stop") : active.running ? t("composer.submit") : t("app.send")}
                title={stopActive ? t("app.stop") : active.running ? t("composer.submit") : t("app.send")}
                onClick={() => void (stopActive ? useRun.getState().cancel() : send())}
                icon={stopActive ? <StopOutlined /> : <ArrowUpOutlined />}
              />
            </div>
          </div>
          </div>
        </BorderBeam>
      </div>
        </>
      )}
    </div>
  );
}
