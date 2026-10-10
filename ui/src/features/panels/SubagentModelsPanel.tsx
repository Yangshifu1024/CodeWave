// 子代理模型覆盖（[docs/subagent-model-override](../../../docs/subagent-model-override.md)）：
// 模型与供应商页一段独立的 `8 角色 × 模型下拉`复合容器——每个内置子代理角色（subagent 工具可委派集，
// 后端 agents::DELEGABLE_ROLES 顺序，剔除内部 title）可单独指定模型，未指定 = 跟随父会话活跃模型。
// 容器层只关心 UI 与 IPC 协调；持久化经 useSettings.save（settings store）做整份 ConfigState 覆盖式提交。
//
// 关键约束：
// - 只读角色用**前端静态映射**判定（READONLY_ROLES = 后端 agents 三个 readonly=true 的角色）。
//   后端 AgentMeta 协议不含 readonly（避免扩 IPC 契约键名），故选静态兜底；顺序对齐后端 DELEGABLE_ROLES。
// - 「悬空 wire id」：当前配置里某角色覆盖值对应的模型已被供应商清掉——本地态视为 null（不主动调 save），
//   等用户下一次交互再决定覆盖；
// - 「继承父会话」= UI 哨兵 `INHERIT_SENTINEL`（绝不可能撞 kebab-case wire id）；
//   选中后内部翻译回 `null` 写 store（与后端 Option<model_id> 同形）。
import { useEffect, useMemo, useState } from "react";
import { App, Flex, Select, Space, Tag } from "antd";
import { useTranslation } from "react-i18next";
import { ipc } from "../../ipc/client";
import { useSettings } from "../../stores/settings";
import { flattenModels, findModel } from "../../utils/models";
import type { AgentMeta } from "../../ipc/types";

/**
 * 只读角色集合：与后端 `agents::DELEGABLE_ROLES` 顺序对齐，剔除内部 title。
 * 后端 `agents/mod.rs` 中 readonly=true 的恰好就是这三个分析角色（见该处的 `readonly_roles_are_exactly_the_three_analysis_roles` 测试）。
 * 不走 IPC 的 AgentMeta（不含 readonly 字段）以避免扩 IPC 契约——见任务说明 E 节。
 */
const READONLY_ROLES = new Set(["explore", "reviewer", "code-reviewer"]);

/** 「继承父会话」选项的 UI 哨兵：避开 antd 6 在 Option value=null 时的运行时警告。
 *  与后端 wire id 命名空间（kebab-case）不可能撞；onChange 处翻译回 null 写 store。 */
const INHERIT_SENTINEL = "__subagent_inherit__";

/**
 * 子代理模型覆盖段：单一 SettingFieldPath = "subagentModels"（[docs/settings-search-and-advanced](../../../docs/settings-search-and-advanced.md) §1.1）——
 * 容器根挂该 id 锚点；搜索命中、临时高亮、走页体级 data-setting-id 唯一性守卫。
 * 8 角色行 = `useMemo` 在 `flat` 变化时重排，**不在 useState/useEffect 里建模派生值**（rendering purity）。
 */
export default function SubagentModelsPanel() {
  const { t } = useTranslation();
  const { message } = App.useApp();
  const config = useSettings((s) => s.config);
  const save = useSettings((s) => s.save);
  // 后端 list_agents 已经按 DELEGABLE_ROLES 顺序返回；title 角色在 backend 那一侧剔除（agents::delegable）。
  const [agents, setAgents] = useState<AgentMeta[]>([]);
  // 拉一次：组件挂载时拉，子代理注册表变更需要重启应用生效（与 settings 全量刷新同语义）。
  // 失败时静默回 []：每个角色下拉降级为「只显示首项继承父会话」，留待用户回到列表可见时排查。
  useEffect(() => {
    let alive = true;
    void ipc.listAgents().then((list) => {
      if (alive) setAgents(list);
    }).catch(() => {
      if (alive) setAgents([]);
    });
    return () => { alive = false; };
  }, []);

  // 摊平视图按 providers 顺序 × 各 provider 内 models 顺序，与 settings store 内 `findModel` 同源。
  const flat = useMemo(() => flattenModels(config), [config]);
  // 按供应商分组（不展开空 provider：用户压根不会想选）；OptGroup 顺序 = providers 顺序。
  const grouped = useMemo(() => {
    const byProvider = new Map<string, { providerId: string; providerName: string; items: typeof flat }>();
    for (const m of flat) {
      const bucket = byProvider.get(m.providerId) ?? { providerId: m.providerId, providerName: m.providerName, items: [] };
      bucket.items.push(m);
      byProvider.set(m.providerId, bucket);
    }
    // providers 顺序 = config?.providers 顺序：与 flattenModels 一致；显式走外层以保证 OptGroup 顺序稳定。
    const out: { providerId: string; providerName: string; items: typeof flat }[] = [];
    for (const p of config?.providers ?? []) {
      const bucket = byProvider.get(p.id);
      if (bucket) out.push(bucket);
    }
    return out;
  }, [flat, config?.providers]);

  /** 取该角色当前覆盖值：未设/悬空 → INHERIT_SENTINEL（UI 哨兵）；其余返回 wire id。
   *  悬空 wire id（厂商已删）退化为 INHERIT_SENTINEL——不主动落 save，等用户下一次交互。 */
  function current(agentName: string): string {
    const map = config?.subagent_models ?? {};
    const v = map[agentName];
    if (v == null) return INHERIT_SENTINEL;
    return findModel(config, v) ? v : INHERIT_SENTINEL;
  }

  /** 改覆盖：INHERIT_SENTINEL 翻译回 null 写 store；wire id 透传。 */
  async function handleChange(agentName: string, v: string) {
    if (!config) return;
    const prev = config.subagent_models ?? {};
    const stored = v === INHERIT_SENTINEL ? null : v;
    const next: Record<string, string | null> = { ...prev, [agentName]: stored };
    try {
      await save({ ...config, subagent_models: next });
    } catch {
      message.error(t("settings.subagentSaveFailed"));
    }
  }

  if (!config) return null;

  return (
    <div className="subagent-models-panel setting-anchor" data-setting-id="subagentModels">
      <Space direction="vertical" style={{ width: "100%" }} size={12}>
        <div className="dim subagent-models-hint">{t("settings.subagentModelsHint")}</div>
        {agents.map((agent) => {
          const value = current(agent.name);
          const isReadonly = READONLY_ROLES.has(agent.name);
          return (
            <Flex key={agent.name} align="center" gap={12} wrap="nowrap" className="subagent-models-row">
              <Flex flex="0 0 200px" align="center" gap={8}>
                <strong>{agent.name}</strong>
                {isReadonly && <Tag className="subagent-models-readonly-tag">{t("settings.subagentReadonly")}</Tag>}
              </Flex>
              <Select
                className="subagent-models-select"
                style={{ minWidth: 280 }}
                value={value}
                placeholder={t("settings.subagentInherit")}
                allowClear={false}
                onChange={(v) => void handleChange(agent.name, v)}
                optionFilterProp="label"
              >
                <Select.Option value={INHERIT_SENTINEL}>{t("settings.subagentInherit")}</Select.Option>
                {grouped.map((g) => (
                  <Select.OptGroup key={g.providerId} label={g.providerName}>
                    {g.items.map((m) => (
                      <Select.Option key={m.id} value={m.id} label={`${g.providerName} · ${m.model}`}>
                        {m.model}
                      </Select.Option>
                    ))}
                  </Select.OptGroup>
                ))}
              </Select>
            </Flex>
          );
        })}
      </Space>
    </div>
  );
}
