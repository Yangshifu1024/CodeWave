# 会话模型恢复

> 2026-09-26 · 会话恢复缺陷修复；目标档恢复见 [goal-mode-recovery](./goal-mode-recovery.md)。

## 根因

`ui-state.json` 的 Tab 快照保存了 `model_id` / `reasoning_effort`，会话索引的 `model_id` 也记录检查点时实际使用的模型。然而后端 `SessionPrefs::from_config` 重建 runtime 时把二者清空；随后前端 `syncPrefs` 用后端默认值覆盖 Tab 快照，界面与下一轮请求都回落全局模型。真实目标会话的会话模型与全局模型不同，因此这个回落可直接复现。

## 修复契约

- `sessions/<id>.prefs.json` 保存目标档活动标记、显式会话模型选择与思考力度；`model_choice_recorded` 区分「显式跟随全局」与旧边车缺少模型字段。会话删除、孤儿清扫与该边车同生命周期。
- 重建 runtime 时先恢复模型与力度，再返回 `get_session_prefs`；模型 id 已从配置删除时清除 override，按现有 `effective_model` 规则回落全局。
- 旧索引的 `SessionMeta.model_id` 只记实际使用模型，无法判断当时是否显式选择。没有新版边车时，后端先保持跟随全局；若启动时恢复的 Tab 骨架携带旧 `ui-state.json` 的模型/力度选择，前端在回读前调用一次 `restore_legacy_model_prefs`，后端仅在 `model_choice_recorded=false` 时接纳并落边车。旧版已关闭的 Tab 没有这份选择快照，无法可靠还原显式模型；这类会话继续跟随全局。已删除的模型 id 在迁移时清除。
- 权限档仍按安全规则处理：只有目标档活动标记恢复；其它权限档重启回落全局默认，目标完成后的前档也重建为全局默认。恢复模型不得复活旧 `FullAccess`。

## 验证

后端回归覆盖模型与力度跨重启、显式跟随全局不被旧索引覆盖、旧目标会话从 UI 快照一次性迁移、旧目标边车兼容、已删除模型回落，以及权限档隔离。界面按项目约定手动验证：重启后打开原会话，检查模型菜单选中项与下一轮实际请求模型一致；另开会话检查互不串用。
