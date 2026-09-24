//! 工具结果原样 sidecar（[docs/session-restore-fidelity](../../../../docs/session-restore-fidelity.md)）：
//! 会话历史里存的是 `compact_for_model` 的**模型侧瘦身文本**（通用分支 HEAD 4KB + TAIL 8KB 头尾截断），
//! 恢复时前端 `JSON.parse` 失败 → 工具卡退化成 `{restored:true}` 占位——大 html（render_html）、
//! grep 大量命中、网页正文、文档读取、edit 列表、ask 载荷、子代理汇报都受影响。
//!
//! 这里按 **provider 侧 tool_use id** 存一份「前端当时拿到的那份完整出参」，供恢复时按需回读：
//! - 写入门槛 [`should_persist`]：**只有「恢复端解析不了这份模型侧文本」时才写**（判据 = 文本能否
//!   解析成 JSON，与前端 `restoredToolData` 的失败条件同源）——`read`/`batch_read` 原样透传、
//!   读图那种「合法 JSON 但剥了 data_url」、未触发截断的小出参、以及没有出参的失败调用，
//!   **一个文件都不产生**（磁盘占用与回读量因此有界）。
//! - 存的是**整套 `ToolOutcome`**（ok/error/data/warnings）：前端连状态一起还原（失败卡不再被当成成功卡）。
//! - 单文件超过 [`MAX_FILE_BYTES`]（与读取侧同阈值）不写：写了也读不回来，白占磁盘。
//! - **永不参与出网**：这些文件不进 `rt.history`、不进 wire，模型上下文与计费零变化。
//! - 生命周期随会话：目录 `sessions/<owner>.toolres/`，随会话删除级联清理；不进右栏「文件」面板
//!   （那里是产物登记边车的数据源）。
//! - owner 用 `root_session_id ?? id`（子代理产生的卡片归属主会话，与产物登记同一口诀）。
//!
//! 为什么键用 provider 侧 id：恢复后前端手上唯一稳定的标识是历史里持久化的 `tool_use.id`
//!（`ui/src/stores/run.ts` 的 `callKey`）；实时 `ctx.call_key` 是批内随机的 `batch:index`，不可持久。

use crate::core::sessions::SessionStore;
use crate::tools::ToolOutcome;
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// call id 安全化后的最大长度（provider 侧 id 通常 ≤ 40 字符，截断只作兜底）。
const CALL_ID_MAX: usize = 64;
/// 一次批量回读的调用数上限（前端只在「历史文本解析失败」的调用上发起，正常远小于此）。
pub const MAX_CALLS: usize = 50;
/// 单个 sidecar 文件的体积上限（超出跳过并记 warn：坏数据不该拖垮整次恢复）。
pub const MAX_FILE_BYTES: u64 = 2 * 1024 * 1024;

/// 单条记录：前端当时拿到的那整套出参 + 该次调用耗时（耗时当前写入为 None，字段先留出以免日后改格式）。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolResultRecord {
    /// 完整 `ToolOutcome`（`{ok, data, error?, warnings}`，与前端 `tool:result` 事件里那份同构）：
    /// 存整套而不是只存 `data`，前端才能连成功/失败状态一起还原
    pub outcome: Value,
    /// 该次调用耗时（毫秒）；历史卡恢复耗时用，当前未采集
    #[serde(default)]
    pub duration_ms: Option<u64>,
}

/// call id → 文件名：只保留 `[A-Za-z0-9_-]`（其余替换为 `_`），截断到 [`CALL_ID_MAX`]，空则 `call`。
/// 写路径与读路径共用本函数——两侧分叉会让恢复永远读不到文件。
pub fn sanitize_call_id(id: &str) -> String {
    let mut out: String = id
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '_' | '-') {
                c
            } else {
                '_'
            }
        })
        .take(CALL_ID_MAX)
        .collect();
    if out.trim_matches('_').is_empty() {
        out = "call".to_string();
    }
    out
}

/// 写入门槛：**恢复端解析不了这份模型侧文本**时才有备份价值。
/// 判据直接用「文本能否解析成 JSON」——与前端 `restoredToolData` 的失败条件同源：
/// - 头尾截断（`[已截断 N 字节]` 插在中间）必破坏 JSON 结构 → **要备份**；
/// - 失败但有出参的调用（如失败 `command` 的 `[error …]` + JSON 体）也解析不了 → 要备份；
/// - `read`/`batch_read` 原样透传、读图那种「合法 JSON 但剥了 data_url」→ 前端自己就能恢复 → **不备份**。
/// 调用方必须在**追加 plan 软提醒等 model hint 之前**传入文本，否则带 hint 的结果会被误判。
pub fn should_persist(model_text: &str, outcome: &ToolOutcome) -> bool {
    !outcome.data.is_null()
        && serde_json::from_str::<serde_json::Value>(model_text.trim_start()).is_err()
}

/// 落盘一次调用。失败只记日志、返回 false——它只是恢复增强，绝不影响工具结果与出网文本。
pub fn save(
    store: &SessionStore,
    owner: &str,
    call_id: &str,
    outcome: &ToolOutcome,
    duration_ms: Option<u64>,
) -> bool {
    let dir = store.tool_results_dir(owner);
    if let Err(e) = std::fs::create_dir_all(&dir) {
        tracing::warn!("工具结果 sidecar 目录创建失败（{}）：{e}", dir.display());
        return false;
    }
    let record = ToolResultRecord {
        outcome: serde_json::to_value(outcome).unwrap_or(Value::Null),
        duration_ms,
    };
    let bytes = match serde_json::to_vec(&record) {
        Ok(b) => b,
        Err(e) => {
            tracing::warn!("工具结果 sidecar 序列化失败（{call_id}）：{e}");
            return false;
        }
    };
    // 与读取侧同阈值：超限不写（写了也读不回来，白占磁盘）
    if bytes.len() as u64 > MAX_FILE_BYTES {
        tracing::warn!(
            "工具结果 sidecar 超过 {}MB 上限，跳过（{call_id}）",
            MAX_FILE_BYTES / 1024 / 1024
        );
        return false;
    }
    let path = dir.join(format!("{}.json", sanitize_call_id(call_id)));
    match crate::util::atomic::atomic_write(&path, &bytes) {
        Ok(()) => true,
        Err(e) => {
            tracing::warn!("工具结果 sidecar 写入失败（{}）：{e}", path.display());
            false
        }
    }
}

/// 读一条（文件缺失 / 损坏 / 超限 → None）。
pub fn load(store: &SessionStore, owner: &str, call_id: &str) -> Option<ToolResultRecord> {
    let path = store
        .tool_results_dir(owner)
        .join(format!("{}.json", sanitize_call_id(call_id)));
    let meta = std::fs::metadata(&path).ok()?;
    if !meta.is_file() || meta.len() > MAX_FILE_BYTES {
        return None;
    }
    let bytes = std::fs::read(&path).ok()?;
    serde_json::from_slice::<ToolResultRecord>(&bytes).ok()
}

/// 批量读（按入参顺序返回实际存在的那些，最多 [`MAX_CALLS`] 条）。
/// 缺失与非法键静默跳过——调用方（前端）只需按返回的条目回填，无需区分「无备份」与「读失败」。
pub fn load_many(
    store: &SessionStore,
    owner: &str,
    call_ids: &[String],
) -> Vec<(String, ToolResultRecord)> {
    call_ids
        .iter()
        .take(MAX_CALLS)
        .filter_map(|id| load(store, owner, id).map(|rec| (id.clone(), rec)))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn store() -> (SessionStore, tempfile::TempDir) {
        let dir = tempfile::tempdir().unwrap();
        (SessionStore::new(dir.path().to_path_buf()), dir)
    }

    #[test]
    fn sanitize_keeps_provider_id_charset_and_falls_back() {
        assert_eq!(sanitize_call_id("toolu_01ABC-xyz"), "toolu_01ABC-xyz");
        // 非法字符替换为下划线（不是丢弃：避免 a/b 与 ab 撞名）
        assert_eq!(sanitize_call_id("a/b..c"), "a_b__c");
        // 超长截断
        assert_eq!(sanitize_call_id(&"x".repeat(100)).len(), CALL_ID_MAX);
        // 空/全非法 → 兜底名
        assert_eq!(sanitize_call_id(""), "call");
        assert_eq!(sanitize_call_id("///"), "call");
    }

    #[test]
    fn persist_threshold_only_for_unparseable_text() {
        let full = json!({"content": "x".repeat(20_000)});
        let out = ToolOutcome::ok(full.clone());
        // 模型侧原样透传（可解析）→ 不需要备份
        assert!(!should_persist(&full.to_string(), &out));
        // 读图那种「合法 JSON、只是剥了 data_url」→ 同样不需要（前端自己就能恢复）
        let img = json!({"files": [{"path": "a.png", "image_note": "[图片数据不在此文本中]"}]});
        assert!(!should_persist(&img.to_string(), &out));
        // 头尾截断（结构被破坏）→ 需要备份
        assert!(should_persist(
            "{\"content\":\"xxx\n…[已截断 20000 字节]…\n\"}",
            &out
        ));
        // 失败但有出参（失败 command 的 `[error …]` + JSON 体）→ 也解析不了 → 要备份
        let partial = ToolOutcome {
            ok: false,
            data: json!({"exit_code": 1}),
            error: Some(crate::tools::ToolError::new("E_EXIT_CODE", "命令退出码 1")),
            warnings: Vec::new(),
            extra_model_content: Vec::new(),
        };
        assert!(should_persist(
            "[error E_EXIT_CODE: 命令退出码 1]\n{\"exit_code\":1}",
            &partial
        ));
        // 失败且没有出参 → 没有可备份的内容
        let empty_err = ToolOutcome::err("E_X", "boom");
        assert!(!should_persist("[error E_X: boom]", &empty_err));
    }

    #[test]
    fn save_stores_full_outcome_envelope() {
        let (store, _dir) = store();
        let out = ToolOutcome::ok(json!({"html": "<b>hi</b>", "chars": 9}));
        assert!(save(&store, "s1", "toolu_1", &out, Some(12)));
        let back = load(&store, "s1", "toolu_1").expect("应能读回");
        // 存整套 ToolOutcome：前端连状态一起换回（失败卡不再被当成成功卡）
        assert_eq!(back.outcome["ok"], json!(true));
        assert_eq!(back.outcome["data"]["html"], "<b>hi</b>");
        assert_eq!(back.duration_ms, Some(12));
        // 缺失键 → None（不 panic、不报错）
        assert!(load(&store, "s1", "toolu_missing").is_none());
        // owner 隔离：另一个会话看不到
        assert!(load(&store, "s2", "toolu_1").is_none());
    }

    #[test]
    fn load_many_skips_missing_and_caps_batch() {
        let (store, _dir) = store();
        let out = ToolOutcome::ok(json!({"n": 1}));
        save(&store, "s1", "a", &out, None);
        save(&store, "s1", "b", &out, None);
        let ids = vec!["a".to_string(), "missing".to_string(), "b".to_string()];
        let got = load_many(&store, "s1", &ids);
        assert_eq!(
            got.iter().map(|(id, _)| id.as_str()).collect::<Vec<_>>(),
            vec!["a", "b"]
        );
        // 超过上限的部分不看（有界回读）
        let many: Vec<String> = (0..MAX_CALLS + 10).map(|i| format!("k{i}")).collect();
        assert!(load_many(&store, "s1", &many).is_empty());
    }

    #[test]
    fn oversized_file_is_skipped() {
        let (store, _dir) = store();
        let dir = store.tool_results_dir("s1");
        std::fs::create_dir_all(&dir).unwrap();
        let big = dir.join("big.json");
        std::fs::write(&big, vec![b'x'; (MAX_FILE_BYTES + 1) as usize]).unwrap();
        assert!(load(&store, "s1", "big").is_none());
    }
}
