//! 交付边车存储（任务 #54 拥有）。
//!
//! 目录形态 `<data_dir>/sessions/<session>.delivery/`：
//!
//! ```text
//! contracts/<task>.<revision>.json  每任务每修订不可变契约
//! records/<uuid>.json    不可变事实记录（回执 / 校验 / 交付结果 / 用户决定）
//! outputs/<uuid>.log     完整原始输出边车（有预算，超限截断并标记）
//! index.json             CAS 索引（**唯一采纳来源**）
//! journal/<op>.json      操作意向（崩溃恢复对账用，恢复时不执行任何命令）
//! ```
//!
//! 四条不可让步的规则（[共同契约 §4](../../../../docs/tools-and-safety.md)）：
//!
//! 1. **未入索引的孤儿记录永远不被验收器采纳**。落盘成功但索引提交失败 → 记录只供诊断。
//!    否则一次写失败会被下次求值"捡回来"，等于把保存失败伪装成事实。
//! 2. **文件名全部由宿主生成**，路径不拼任何用户输入；owner / record id 不可当路径用。
//! 3. **事务顺序固定**：校验身份与 expected_revision → 写 journal → 原子写不可变 record →
//!    CAS 提交新索引 → 标记 operation committed → 发快照。任一断点退出，恢复依据
//!    **索引**（记录是否被采纳）与 **journal**（操作是否提交）校验收敛。
//! 4. **保存失败返回 `E_DELIVERY_SAVE`，保留工具已经发生的真实结果**，在完成投影标
//!    unknown/blocked；绝不把写失败映射成 passed。

use super::types::{
    AcceptanceSnapshot, DeliveryResult, ExecutionIntent, ExecutionReceipt, RunIdentity, SCHEMA,
    SESSION_HARD_FUSE_BYTES, SESSION_SOFT_WARN_BYTES,
};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

/// 不可变记录（[`RecordKind`](RecordKind) 决定负载类型）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum RecordBody {
    /// 执行回执（#14）。
    Execution(Box<ExecutionReceipt>),
    /// 执行意向（未决；重启后转 interrupted）。
    Intent(Box<ExecutionIntent>),
    /// 交付结果（#17）。
    Delivery(Box<DeliveryResult>),
    /// 内置校验记录（共同契约 §3）。
    Validation(Box<super::types::BuiltinValidationRecord>),
    /// 独立审查结论（#9/#10/#18）。
    Review(Box<super::types::ReviewResult>),
    /// 审查分派（#10）：**由宿主签发**，审查者凭它提交结论。
    ReviewAssignment(Box<super::responsibility::ReviewAssignment>),
    /// 用户决定（豁免 / 人工确认）。
    UserDecision(Box<super::types::UserDecision>),
    /// 验收快照（派生状态，落盘便于恢复只读投影）。
    Acceptance(Box<AcceptanceSnapshot>),
    /// 修复预算（#16）：**重启不归零**，丢了就等于悄悄放开上限。
    RepairBudget(Box<RepairBudgetState>),
}

/// 修复预算的落盘形态（带归属任务，便于多任务并存时各算各的）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RepairBudgetState {
    /// 所属任务。
    pub task_id: String,
    /// 预算本体。
    pub budget: super::completion::RepairBudget,
}

impl DeliveryRecord {
    /// 幂等摘要的输入：语义内容，**剔除时刻字段**。
    ///
    /// 时刻是**导出元数据**，不是操作的身份：同一次操作的重试必然发生在不同时刻，
    /// 把时刻算进摘要会让"重复请求幂等"永远失效，每次重试都被误判成冲突。
    fn semantic_digest_input(&self) -> String {
        let mut body = self.body.clone();
        body.zero_volatile_timestamps();
        format!(
            "{}|{}|{}",
            self.schema,
            serde_json::to_string(&self.owner).unwrap_or_default(),
            serde_json::to_string(&body).unwrap_or_default(),
        )
    }
}

impl RecordBody {
    /// 把各负载的时刻字段归零（幂等摘要专用；**不改落盘内容**）。
    fn zero_volatile_timestamps(&mut self) {
        match self {
            RecordBody::Execution(r) => {
                r.started_at.clear();
                r.finished_at.clear();
            }
            RecordBody::Intent(i) => i.created_at.clear(),
            RecordBody::Delivery(d) => d.submitted_at.clear(),
            RecordBody::Validation(v) => {
                v.started_at.clear();
                v.finished_at.clear();
            }
            RecordBody::Review(r) => r.reviewed_at.clear(),
            // 分派无时刻字段（policy_revision 才是版本语义）
            RecordBody::ReviewAssignment(_) => {}
            RecordBody::UserDecision(d) => d.answered_at.clear(),
            // 快照与预算都没有时刻字段（轮次 / step / 指纹计数才是预算的身份）
            RecordBody::Acceptance(_) => {}
            RecordBody::RepairBudget(_) => {}
        }
    }

    /// 记录种类名。
    pub fn kind_name(&self) -> &'static str {
        match self {
            RecordBody::Execution(_) => "execution",
            RecordBody::Intent(_) => "intent",
            RecordBody::Delivery(_) => "delivery",
            RecordBody::Validation(_) => "validation",
            RecordBody::Review(_) => "review",
            RecordBody::ReviewAssignment(_) => "review_assignment",
            RecordBody::UserDecision(_) => "user_decision",
            RecordBody::Acceptance(_) => "acceptance",
            RecordBody::RepairBudget(_) => "repair_budget",
        }
    }
}

/// 一条不可变记录的外壳。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DeliveryRecord {
    /// schema 版本。
    pub schema: u32,
    /// 记录 id（宿主生成）。
    pub id: String,
    /// 归属身份。
    pub owner: RunIdentity,
    /// 写入时刻。
    pub at: String,
    /// 负载。
    pub body: RecordBody,
}

/// 索引中的契约条目。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct IndexedContract {
    /// 任务 id。
    pub task_id: String,
    /// 当前修订号。
    pub revision: u64,
    /// 契约归属身份（读 / 修订时的属主校验依据）。
    pub owner: RunIdentity,
    /// 契约文件名（固定 `contract.json`，显式记录以便将来多任务并存）。
    pub file: String,
    /// 验收快照自身修订。
    #[serde(default)]
    pub acceptance_revision: u64,
    /// 整个任务是否被用户取消。
    #[serde(default)]
    pub whole_cancelled: bool,
}

/// 操作条目（journal 与 index 共用同一份语义，重启对账以 index 为准）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct OperationEntry {
    /// 操作 id。
    pub operation_id: String,
    /// 操作类别（`create_contract` / `revise_contract` / `record_*` / `acceptance`）。
    pub kind: String,
    /// 是否已提交（记录已进入索引）。
    pub committed: bool,
    /// 产生的记录 id（若有）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub record_id: Option<String>,
    /// 操作内容摘要（用于同 operation_id 不同内容的冲突检测）。
    pub content_digest: String,
}

/// CAS 索引。**它是记录被采纳的唯一来源**——磁盘上存在但索引里没有的记录是孤儿。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DeliveryIndex {
    /// schema 版本。
    pub schema: u32,
    /// 索引修订（每次成功提交 +1）。
    #[serde(default)]
    pub revision: u64,
    /// 所属会话。
    pub session_id: String,
    /// 契约条目。
    #[serde(default)]
    pub contracts: BTreeMap<String, IndexedContract>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub active_task_id: Option<String>,
    /// 已采纳记录：record_id → 相对文件路径。
    #[serde(default)]
    pub records: BTreeMap<String, String>,
    /// 操作条目。
    #[serde(default)]
    pub operations: BTreeMap<String, OperationEntry>,
}

impl DeliveryIndex {
    /// 空索引。
    pub fn new(session_id: impl Into<String>) -> Self {
        DeliveryIndex {
            schema: SCHEMA,
            revision: 0,
            session_id: session_id.into(),
            contracts: BTreeMap::new(),
            active_task_id: None,
            records: BTreeMap::new(),
            operations: BTreeMap::new(),
        }
    }
}

/// 公共错误码（[#54](../../../../docs/tools-and-safety.md)）。**错误码不是状态真值**，
/// 详细位置在 `reasons` 里。
#[derive(Debug, Clone, PartialEq)]
pub enum DeliveryError {
    /// 契约非法（同名 id / 悬空引用 / 越界 / 超限）。
    ContractInvalid {
        /// 可定位原因。
        reasons: Vec<super::types::Reason>,
    },
    /// 过期修订。
    ContractStale {
        /// 调用方预期修订。
        expected: u64,
        /// 实际修订。
        actual: u64,
    },
    /// 属主不符（非本会话 / 非本 root_run）。
    OwnerMismatch {
        /// 说明。
        message: String,
    },
    /// 证据非法（引用不存在 / 越界 / 类型不符）。
    EvidenceInvalid {
        /// 可定位原因。
        reasons: Vec<super::types::Reason>,
    },
    /// 保存失败（磁盘满 / 索引写失败）。**保留工具已发生的真实结果**，不映射成 passed。
    Save {
        /// 说明。
        message: String,
    },
    /// 同 operation_id 不同内容。
    OperationConflict {
        /// 说明。
        message: String,
    },
    /// 找不到记录 / 契约。
    NotFound {
        /// 说明。
        message: String,
    },
    /// 触到会话证据硬线：只停新证据写入，**绝不删既有数据**。
    Fuse {
        /// 说明。
        message: String,
    },
    /// 达到软线（照常写，只提示）。
    SoftWarn {
        /// 说明。
        message: String,
    },
}

impl DeliveryError {
    /// 稳定错误码。
    pub fn code(&self) -> &'static str {
        match self {
            DeliveryError::ContractInvalid { .. } => "E_CONTRACT_INVALID",
            DeliveryError::ContractStale { .. } => "E_CONTRACT_STALE",
            DeliveryError::OwnerMismatch { .. } => "E_DELIVERY_OWNER",
            DeliveryError::EvidenceInvalid { .. } => "E_EVIDENCE_INVALID",
            DeliveryError::Save { .. } => "E_DELIVERY_SAVE",
            DeliveryError::OperationConflict { .. } => "E_OPERATION_CONFLICT",
            DeliveryError::NotFound { .. } => "E_DELIVERY_NOT_FOUND",
            DeliveryError::Fuse { .. } => "E_DELIVERY_FUSE",
            DeliveryError::SoftWarn { .. } => "E_DELIVERY_SOFT_WARN",
        }
    }
    /// 面向模型 / 用户的说明。
    pub fn message(&self) -> String {
        match self {
            DeliveryError::ContractInvalid { reasons } => reasons
                .iter()
                .map(|r| r.message.clone())
                .collect::<Vec<_>>()
                .join("；"),
            DeliveryError::ContractStale { expected, actual } => {
                format!("契约修订过期：预期 {expected}，实际 {actual}")
            }
            DeliveryError::OwnerMismatch { message }
            | DeliveryError::Save { message }
            | DeliveryError::OperationConflict { message }
            | DeliveryError::NotFound { message }
            | DeliveryError::Fuse { message }
            | DeliveryError::SoftWarn { message } => message.clone(),
            DeliveryError::EvidenceInvalid { reasons } => reasons
                .iter()
                .map(|r| r.message.clone())
                .collect::<Vec<_>>()
                .join("；"),
        }
    }
}

/// 写入结果（[#54](../../../../docs/tools-and-safety.md) 约定 write 类接口返回这四个字段）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WriteReceipt {
    /// 操作 id。
    pub operation_id: String,
    /// 提交后的修订号。
    pub revision: u64,
    /// 产生的记录 id（无记录时为契约 id）。
    pub record_id: String,
    /// 是否真的落盘并入索引（false = 只诊断用孤儿）。
    pub persisted: bool,
    /// 非致命提示（软线触顶等）。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub warnings: Vec<String>,
}

/// 交付边车存储。**一个会话一个实例**，内部串行化索引写入（单写锁 + CAS）。
pub struct DeliveryStore {
    /// `sessions/` 目录。
    sessions_dir: PathBuf,
    /// 索引写互斥（会话级单写者）。
    index_lock: std::sync::Mutex<()>,
    /// 测试用：输出预算覆盖（生产恒为默认常量）—— `(软线, 硬线, 单份上限)`。
    #[cfg(test)]
    limits: Option<(u64, u64, u64)>,
}

impl DeliveryStore {
    /// 以 `sessions/` 目录构造。
    pub fn new(sessions_dir: PathBuf) -> Self {
        DeliveryStore {
            sessions_dir,
            index_lock: std::sync::Mutex::new(()),
            #[cfg(test)]
            limits: None,
        }
    }

    /// 交付边车目录：`sessions/<owner>.delivery/`。
    ///
    /// 与 `sessions/<id>.toolres/` 同范式：路径由编号拼出、随会话级联删除，不进右栏
    /// 「文件」面板。**该目录属宿主内部状态**——模型普通 create/edit/command 不能因为
    /// 读到路径就获得写权（S0-D3-A 的写保护已覆盖同一前缀族）。
    pub fn delivery_dir(&self, session_id: &str) -> PathBuf {
        self.sessions_dir.join(format!("{session_id}.delivery"))
    }

    fn contract_path(&self, session_id: &str, task_id: &str, revision: u64) -> PathBuf {
        self.delivery_dir(session_id)
            .join("contracts")
            .join(format!("{}.{}.json", host_id(task_id, "task"), revision))
    }
    fn index_path(&self, session_id: &str) -> PathBuf {
        self.delivery_dir(session_id).join("index.json")
    }
    fn records_dir(&self, session_id: &str) -> PathBuf {
        self.delivery_dir(session_id).join("records")
    }
    fn outputs_dir(&self, session_id: &str) -> PathBuf {
        self.delivery_dir(session_id).join("outputs")
    }
    fn journal_dir(&self, session_id: &str) -> PathBuf {
        self.delivery_dir(session_id).join("journal")
    }

    pub fn save_baseline(
        &self,
        session: &str,
        task: &str,
        baseline: &super::version::ScopeVersion,
    ) -> Result<(), DeliveryError> {
        let path = self
            .delivery_dir(session)
            .join("baselines")
            .join(format!("{}.json", host_id(task, "task")));
        if let Ok(bytes) = std::fs::read(&path) {
            if serde_json::from_slice::<super::version::ScopeVersion>(&bytes)
                .ok()
                .as_ref()
                == Some(baseline)
            {
                return Ok(());
            }
            return Err(DeliveryError::Save {
                message: "已有不可变基线与重试不一致".into(),
            });
        }
        std::fs::create_dir_all(path.parent().unwrap()).map_err(|e| DeliveryError::Save {
            message: e.to_string(),
        })?;
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(path)
            .map_err(|e| DeliveryError::Save {
                message: e.to_string(),
            })?;
        use std::io::Write;
        file.write_all(
            &serde_json::to_vec(baseline).map_err(|e| DeliveryError::Save {
                message: e.to_string(),
            })?,
        )
        .and_then(|_| file.sync_all())
        .map_err(|e| DeliveryError::Save {
            message: e.to_string(),
        })
    }
    pub fn load_baseline(&self, session: &str, task: &str) -> Option<super::version::ScopeVersion> {
        let path = self
            .delivery_dir(session)
            .join("baselines")
            .join(format!("{}.json", host_id(task, "task")));
        serde_json::from_slice(&std::fs::read(path).ok()?).ok()
    }

    /// 读索引。损坏 / 不存在时返回空索引（**不阻断会话打开**，与既有边车同策略）。
    pub fn load_index(&self, session_id: &str) -> DeliveryIndex {
        std::fs::read(self.index_path(session_id))
            .ok()
            .and_then(|b| serde_json::from_slice::<DeliveryIndex>(&b).ok())
            .filter(|i| i.session_id == session_id)
            .unwrap_or_else(|| DeliveryIndex::new(session_id))
    }

    /// Distinguish absent delivery state from unreadable committed evidence.
    pub fn ensure_readable(&self, session_id: &str) -> Result<(), DeliveryError> {
        let path = self.index_path(session_id);
        if !path.exists() {
            if self.delivery_dir(session_id).exists() {
                return Err(DeliveryError::Save {
                    message: "交付目录存在但索引缺失，不能当作无契约".into(),
                });
            }
            return Ok(());
        }
        let bytes = std::fs::read(path).map_err(|e| DeliveryError::Save {
            message: e.to_string(),
        })?;
        let index: DeliveryIndex =
            serde_json::from_slice(&bytes).map_err(|e| DeliveryError::Save {
                message: e.to_string(),
            })?;
        if index.session_id != session_id
            || index
                .contracts
                .keys()
                .any(|id| self.load_contract(session_id, id).is_none())
            || index
                .records
                .keys()
                .any(|id| self.load_record(session_id, id).is_none())
        {
            return Err(DeliveryError::Save {
                message: "交付索引或证据不可读，拒绝静默放行".into(),
            });
        }
        Ok(())
    }

    /// 读契约。**只读已入索引的条目**——磁盘上存在但未入索引的契约草稿不是有效契约
    /// （绝不把未批准的草稿当已批准契约）。
    pub fn load_contract(
        &self,
        session_id: &str,
        task_id: &str,
    ) -> Option<super::types::TaskContract> {
        let index = self.load_index(session_id);
        let entry = index.contracts.get(task_id)?;
        let bytes = std::fs::read(self.delivery_dir(session_id).join(&entry.file)).ok()?;
        let c: super::types::TaskContract = serde_json::from_slice(&bytes).ok()?;
        // 索引条目与文件内容必须一致，否则视为未提交（可能被手工改过或写中断）。
        if c.task_id != entry.task_id || c.revision != entry.revision {
            return None;
        }
        Some(c)
    }

    /// 读一条**已采纳**记录。未入索引的孤儿一律返回 `None`（只供诊断接口使用）。
    pub fn load_record(&self, session_id: &str, record_id: &str) -> Option<DeliveryRecord> {
        let index = self.load_index(session_id);
        let rel = index.records.get(record_id)?;
        let bytes = std::fs::read(self.delivery_dir(session_id).join(rel)).ok()?;
        serde_json::from_slice(&bytes).ok()
    }

    /// 列出某任务下已采纳的记录 id（按写入顺序）。
    pub fn record_ids(&self, session_id: &str, task_hint: Option<&str>) -> Vec<String> {
        let index = self.load_index(session_id);
        index
            .records
            .keys()
            .filter_map(|id| {
                let r = self.load_record(session_id, id)?;
                // 只有交付 / 验收 / 审查记录带 task_id；其余按会话归集。
                let task = match &r.body {
                    RecordBody::Delivery(d) => Some(d.task_id.clone()),
                    RecordBody::Review(rv) => Some(rv.task_id.clone()),
                    RecordBody::Acceptance(a) => Some(a.task_id.clone()),
                    _ => None,
                };
                match (task_hint, task) {
                    (None, _) => Some(id.clone()),
                    (Some(h), Some(t)) if t == h => Some(id.clone()),
                    (Some(_), None) => Some(id.clone()),
                    _ => None,
                }
            })
            .collect()
    }

    /// 会话当前证据占用字节数（对预算线）。
    pub fn usage_bytes(&self, session_id: &str) -> u64 {
        fn walk(p: &Path) -> u64 {
            let Ok(rd) = std::fs::read_dir(p) else {
                return 0;
            };
            rd.flatten()
                .map(|e| {
                    let Ok(md) = e.metadata() else { return 0 };
                    if md.is_dir() {
                        walk(&e.path())
                    } else {
                        md.len()
                    }
                })
                .sum()
        }
        walk(&self.delivery_dir(session_id))
    }

    /// 输出边车写入。超单份上限截断并标记；触会话硬线则拒写（**不删既有数据**）。
    ///
    /// 返回 `(output_ref, truncated)`。
    pub fn write_output(
        &self,
        session_id: &str,
        record_id: &str,
        bytes: &[u8],
    ) -> Result<(String, bool), DeliveryError> {
        let (_soft, hard, per_file) = self.budget_lines();
        let used = self.usage_bytes(session_id);
        if used >= hard {
            return Err(DeliveryError::Fuse {
                message: format!(
                    "会话证据已达硬上限（{used} >= {hard} 字节），停止写入新证据；既有证据保持不变"
                ),
            });
        }
        let (payload, truncated) = if bytes.len() as u64 > per_file {
            (&bytes[..per_file as usize], true)
        } else {
            (bytes, false)
        };
        // 硬线内也留出本次写入空间；超出则截断而非拒绝（已产生的事实要留痕）。
        let mut keep = payload;
        let room = hard.saturating_sub(used);
        let mut truncated = truncated;
        if (keep.len() as u64) > room {
            keep = &keep[..room.min(keep.len() as u64) as usize];
            truncated = true;
        }
        let rel = format!("outputs/{record_id}.log");
        let output_path = self.delivery_dir(session_id).join(&rel);
        if let Ok(existing) = std::fs::read(&output_path) {
            if existing != keep {
                return Err(DeliveryError::OperationConflict {
                    message: "不可变输出已存在且内容不同".into(),
                });
            }
        } else {
            write_new(&output_path, keep)?;
        }
        Ok((rel, truncated))
    }

    pub fn read_execution_output(&self, session: &str, record_id: &str) -> Option<Vec<u8>> {
        let record = self.load_record(session, record_id)?;
        let RecordBody::Execution(receipt) = record.body else {
            return None;
        };
        let rel = receipt.output_ref?;
        if receipt.output_truncated || !rel.starts_with("outputs/") {
            return None;
        }
        let path = crate::tools::pathutil::safe_join(&self.delivery_dir(session), &rel).ok()?;
        std::fs::read(path).ok()
    }

    /// 会话证据软线提示。触线**照常写、只提示**——软线不是拒绝线。
    ///
    /// 与 [`Self::write_output`] 的硬线语义刻意分开：硬线停写且保数据，软线只是提醒
    /// 清理入口即将变慢，避免把"提醒"实现成"静默丢弃"。
    pub fn soft_line_warning(&self, session_id: &str) -> Option<String> {
        let (soft, hard, _) = self.budget_lines();
        let used = self.usage_bytes(session_id);
        if used >= hard {
            return Some(format!(
                "会话证据已达硬上限（{used} 字节）：新证据写入已停止，既有证据保持不变"
            ));
        }
        if used >= soft {
            return Some(format!(
                "会话证据占用 {used} 字节，已达软上限 {soft} 字节（新写入仍会继续，硬线才停止）"
            ));
        }
        None
    }

    /// 预算线 `(软线, 硬线, 单份上限)`（测试可注入小阈值；生产恒用默认常量）。
    fn budget_lines(&self) -> (u64, u64, u64) {
        #[cfg(test)]
        if let Some(l) = self.limits {
            return l;
        }
        (
            SESSION_SOFT_WARN_BYTES,
            SESSION_HARD_FUSE_BYTES,
            super::types::MAX_OUTPUT_BYTES,
        )
    }

    /// 测试用：覆盖会话证据预算线与单份输出上限。
    #[cfg(test)]
    #[must_use]
    pub fn with_budget_limits(mut self, soft: u64, hard: u64, per_output: u64) -> Self {
        self.limits = Some((soft, hard, per_output));
        self
    }

    // ---------- 事务 ----------

    /// 幂等检查：同 `operation_id` **同种类**同内容 → 返回已有回执；
    /// 同种类不同内容 → `E_OPERATION_CONFLICT`。
    ///
    /// **同 operation_id、不同种类不是冲突**：`ExecutionIntent` 与它最终的
    /// `ExecutionReceipt` 按 [#14](../../../../docs/tools-and-safety.md) 共用一个
    /// operation_id（重启后据此把未决 intent 标 `interrupted`）。把它当冲突会让
    /// 「先建意向、后签回执」这条正常路径永远失败——那正好是命令工具的主路径。
    fn check_operation(
        &self,
        index: &DeliveryIndex,
        op: &str,
        kind: &str,
        digest: &str,
    ) -> Result<Option<WriteReceipt>, DeliveryError> {
        let Some(prev) = index.operations.get(op) else {
            return Ok(None);
        };
        if prev.kind != kind {
            // 意向 → 回执是同一次操作的两个阶段，交给调用方写入新记录。
            return Ok(None);
        }
        if prev.content_digest != digest {
            return Err(DeliveryError::OperationConflict {
                message: format!(
                    "操作 {op} 的 {kind} 记录已存在且内容不同（拒绝静默覆盖已提交事实）"
                ),
            });
        }
        if prev.committed {
            return Ok(Some(WriteReceipt {
                operation_id: op.to_string(),
                revision: index.revision,
                record_id: prev.record_id.clone().unwrap_or_default(),
                persisted: true,
                warnings: Vec::new(),
            }));
        }
        Ok(None)
    }

    /// 写 journal（操作意向）。**恢复时只据此标状态，绝不执行任何命令**。
    fn write_journal(&self, session_id: &str, entry: &OperationEntry) -> Result<(), DeliveryError> {
        let path = self
            .journal_dir(session_id)
            .join(sanitize_op_id(&entry.operation_id))
            .with_extension("json");
        let bytes = serde_json::to_vec_pretty(entry).map_err(|e| DeliveryError::Save {
            message: format!("操作序列化失败：{e}"),
        })?;
        crate::util::atomic::atomic_write(&path, &bytes).map_err(|e| DeliveryError::Save {
            message: format!("journal 写入失败：{e}"),
        })
    }

    /// CAS 提交索引。`expected` 必须等于当前索引修订（`None` 表示新建，允许从 0 起）。
    fn commit_index(
        &self,
        session_id: &str,
        expected: Option<u64>,
        mut next: DeliveryIndex,
    ) -> Result<u64, DeliveryError> {
        let _guard = self.index_lock.lock().unwrap();
        let current = self.load_index(session_id);
        if let Some(exp) = expected
            && current.revision != exp
        {
            return Err(DeliveryError::ContractStale {
                expected: exp,
                actual: current.revision,
            });
        }
        next.revision = current.revision + 1;
        let bytes = serde_json::to_vec_pretty(&next).map_err(|e| DeliveryError::Save {
            message: format!("索引序列化失败：{e}"),
        })?;
        crate::util::atomic::atomic_write(&self.index_path(session_id), &bytes).map_err(|e| {
            DeliveryError::Save {
                message: format!("索引提交失败：{e}"),
            }
        })?;
        Ok(next.revision)
    }

    /// 落一条不可变记录并提交索引。事务顺序见模块文档。
    ///
    /// `task_id` 只用于 `DeliveryResult` / `AcceptanceSnapshot` 归集；记录本身不依赖它。
    pub fn write_record(
        &self,
        session_id: &str,
        owner: &RunIdentity,
        operation_id: &str,
        body: RecordBody,
        task_id: Option<&str>,
    ) -> Result<WriteReceipt, DeliveryError> {
        // 属主校验：记录必须属本会话（跨会话写入一律拒绝）。
        if owner.session_id != session_id {
            return Err(DeliveryError::OwnerMismatch {
                message: format!(
                    "身份会话 {} 与目标会话 {session_id} 不一致",
                    owner.session_id
                ),
            });
        }
        let record_id = host_id(operation_id, body.kind_name());
        let record = DeliveryRecord {
            schema: SCHEMA,
            id: record_id.clone(),
            owner: owner.clone(),
            at: now_iso(),
            body,
        };
        // 幂等摘要只覆盖**语义内容**（id / owner / 负载），**不含落盘时刻**：
        // 同一操作的重试天然发生在不同时刻，把时间算进去会让"重复请求幂等"永远失效，
        // 每次重试都被误判成"同键不同内容"的冲突。
        let digest = content_digest(&record.semantic_digest_input());

        // 1) 校验身份 + 幂等/冲突
        let index = self.load_index(session_id);
        let kind = record.body.kind_name();
        if let Some(receipt) = self.check_operation(&index, operation_id, kind, &digest)? {
            return Ok(receipt);
        }
        // 2) 写 journal（操作意向）
        let op_entry = OperationEntry {
            operation_id: operation_id.to_string(),
            kind: record.body.kind_name().to_string(),
            committed: false,
            record_id: Some(record_id.clone()),
            content_digest: digest.clone(),
        };
        self.write_journal(session_id, &op_entry)?;
        // 3) 原子写不可变记录
        let rel = format!("records/{record_id}.json");
        let bytes = serde_json::to_vec_pretty(&record).map_err(|e| DeliveryError::Save {
            message: format!("记录序列化失败：{e}"),
        })?;
        // `rel` 自带 `records/` 前缀，故落在交付目录根下拼接（不能再叠一次 records_dir）。
        let record_path = self.delivery_dir(session_id).join(&rel);
        if let Ok(existing) = std::fs::read(&record_path) {
            let old: DeliveryRecord =
                serde_json::from_slice(&existing).map_err(|e| DeliveryError::Save {
                    message: e.to_string(),
                })?;
            if old.semantic_digest_input() != record.semantic_digest_input() {
                return Err(DeliveryError::OperationConflict {
                    message: "不可变记录已存在且内容不同".into(),
                });
            }
        } else {
            write_new(&record_path, &bytes)?;
        }
        // 4) CAS 提交新索引（记录此刻才被采纳）
        let mut next = index.clone();
        next.records.insert(record_id.clone(), rel.clone());
        next.operations.insert(
            operation_id.to_string(),
            OperationEntry {
                committed: true,
                ..op_entry.clone()
            },
        );
        if let Some(t) = task_id {
            next.contracts
                .entry(t.to_string())
                .or_insert_with(|| IndexedContract {
                    task_id: t.to_string(),
                    revision: 0,
                    owner: owner.clone(),
                    file: "contract.json".to_string(),
                    acceptance_revision: 0,
                    whole_cancelled: false,
                });
        }
        let revision = match self.commit_index(session_id, Some(index.revision), next) {
            Ok(r) => r,
            Err(e) => {
                // 索引提交失败 → 记录成孤儿。**不删记录**（诊断价值），但 `persisted=false`，
                // 验收器不会采纳。工具已发生的真实结果由调用方如实保留。
                return Err(match e {
                    DeliveryError::ContractStale { .. } => DeliveryError::Save {
                        message: format!(
                            "记录已落盘但索引提交未完成（修订冲突），该记录仅为诊断用途：{e}"
                        ),
                    },
                    other => other,
                });
            }
        };
        // 5) 标记 operation committed
        let _ = self.write_journal(
            session_id,
            &OperationEntry {
                committed: true,
                ..op_entry
            },
        );
        // 6) 发快照由调用方负责（store 不认识事件面）
        Ok(WriteReceipt {
            operation_id: operation_id.to_string(),
            revision,
            record_id,
            persisted: true,
            warnings: Vec::new(),
        })
    }

    /// 保存契约（创建或修订）。宿主身份来自 `owner`，**不从 draft 取**。
    ///
    /// - `expected_revision`：`None` 表示创建（要求当前无契约），`Some(n)` 表示修订
    ///   （要求当前修订恰为 n，否则 `E_CONTRACT_STALE`）。
    pub fn save_contract(
        &self,
        session_id: &str,
        owner: &RunIdentity,
        task_id: &str,
        expected_revision: Option<u64>,
        contract: &super::types::TaskContract,
        operation_id: &str,
    ) -> Result<WriteReceipt, DeliveryError> {
        if owner.session_id != session_id {
            return Err(DeliveryError::OwnerMismatch {
                message: "契约属主会话与目标会话不一致".into(),
            });
        }
        let index = self.load_index(session_id);
        // 幂等：同 operation_id 已被提交 → 直接返回。
        let digest = content_digest(&serde_json::to_string(contract).unwrap_or_default());
        if let Some(receipt) =
            self.check_operation(&index, operation_id, "save_contract", &digest)?
        {
            return Ok(receipt);
        }
        match (&index.contracts.get(task_id), expected_revision) {
            // 新建：要求当前没有契约。
            (None, None) => {}
            (None, Some(exp)) => {
                return Err(DeliveryError::ContractStale {
                    expected: exp,
                    actual: 0,
                });
            }
            // 修订：要求当前修订精确匹配。
            (Some(entry), Some(exp)) if entry.revision == exp => {}
            // 已存在契约却走创建路径 → 过期修订（不静默覆盖既有契约）。
            (Some(entry), None) => {
                return Err(DeliveryError::ContractStale {
                    expected: 0,
                    actual: entry.revision,
                });
            }
            (Some(entry), Some(exp)) => {
                return Err(DeliveryError::ContractStale {
                    expected: exp,
                    actual: entry.revision,
                });
            }
        }
        // 写 journal
        self.write_journal(
            session_id,
            &OperationEntry {
                operation_id: operation_id.to_string(),
                kind: "save_contract".into(),
                committed: false,
                record_id: Some(task_id.to_string()),
                content_digest: digest.clone(),
            },
        )?;
        // 原子写契约文件
        let bytes = serde_json::to_vec_pretty(contract).map_err(|e| DeliveryError::Save {
            message: format!("契约序列化失败：{e}"),
        })?;
        let contract_path = self.contract_path(session_id, task_id, contract.revision);
        if let Ok(existing) = std::fs::read(&contract_path) {
            if existing != bytes {
                return Err(DeliveryError::OperationConflict {
                    message: "不可变契约修订已存在且内容不同".into(),
                });
            }
        } else {
            write_new(&contract_path, &bytes)?;
        }
        // CAS 提交索引
        let mut next = index.clone();
        let rev = contract.revision;
        next.active_task_id = Some(task_id.to_owned());
        next.contracts.insert(
            task_id.to_string(),
            IndexedContract {
                task_id: task_id.to_string(),
                revision: rev,
                owner: owner.clone(),
                file: format!("contracts/{}.{}.json", host_id(task_id, "task"), rev),
                acceptance_revision: index
                    .contracts
                    .get(task_id)
                    .map(|e| e.acceptance_revision)
                    .unwrap_or(0),
                whole_cancelled: index
                    .contracts
                    .get(task_id)
                    .map(|e| e.whole_cancelled)
                    .unwrap_or(false),
            },
        );
        next.operations.insert(
            operation_id.to_string(),
            OperationEntry {
                operation_id: operation_id.to_string(),
                kind: "save_contract".into(),
                committed: true,
                record_id: Some(task_id.to_string()),
                content_digest: digest,
            },
        );
        let revision = self.commit_index(session_id, Some(index.revision), next)?;
        Ok(WriteReceipt {
            operation_id: operation_id.to_string(),
            revision,
            record_id: task_id.to_string(),
            persisted: true,
            warnings: Vec::new(),
        })
    }

    /// 保存验收快照（派生状态，可重复覆盖；只增 `acceptance_revision`）。
    pub fn save_acceptance(
        &self,
        session_id: &str,
        owner: &RunIdentity,
        task_id: &str,
        snapshot: &AcceptanceSnapshot,
        operation_id: &str,
    ) -> Result<WriteReceipt, DeliveryError> {
        let receipt = self.write_record(
            session_id,
            owner,
            operation_id,
            RecordBody::Acceptance(Box::new(snapshot.clone())),
            Some(task_id),
        )?;
        // 同步 acceptance_revision（派生游标；旧 revision 不覆盖当前值由调用方比对）。
        let idx = self.load_index(session_id);
        if let Some(entry) = idx.contracts.get(task_id)
            && snapshot.revision >= entry.acceptance_revision
        {
            let mut next = idx.clone();
            next.contracts.insert(
                task_id.to_string(),
                IndexedContract {
                    acceptance_revision: snapshot.revision,
                    ..entry.clone()
                },
            );
            let _ = self.commit_index(session_id, Some(idx.revision), next);
        }
        Ok(receipt)
    }

    /// 标记整个任务被取消（只影响整体归约，不删除任何原始需求记录）。
    pub fn cancel_whole_task(
        &self,
        session_id: &str,
        task_id: &str,
        operation_id: &str,
    ) -> Result<WriteReceipt, DeliveryError> {
        let index = self.load_index(session_id);
        let Some(entry) = index.contracts.get(task_id).cloned() else {
            return Err(DeliveryError::NotFound {
                message: format!("任务 {task_id} 无契约"),
            });
        };
        let digest = content_digest(&format!("cancel:{task_id}"));
        if let Some(receipt) = self.check_operation(&index, operation_id, "cancel_task", &digest)? {
            return Ok(receipt);
        }
        let mut next = index.clone();
        next.contracts.insert(
            task_id.to_string(),
            IndexedContract {
                whole_cancelled: true,
                ..entry
            },
        );
        next.operations.insert(
            operation_id.to_string(),
            OperationEntry {
                operation_id: operation_id.to_string(),
                kind: "cancel_task".into(),
                committed: true,
                record_id: Some(task_id.to_string()),
                content_digest: digest,
            },
        );
        let revision = self.commit_index(session_id, Some(index.revision), next)?;
        Ok(WriteReceipt {
            operation_id: operation_id.to_string(),
            revision,
            record_id: task_id.to_string(),
            persisted: true,
            warnings: Vec::new(),
        })
    }

    /// 整个任务是否已被取消。
    pub fn whole_task_cancelled(&self, session_id: &str, task_id: &str) -> bool {
        self.load_index(session_id)
            .contracts
            .get(task_id)
            .is_some_and(|e| e.whole_cancelled)
    }

    /// 崩溃恢复对账：把 journal 里未提交但记录已在索引中的操作标为已提交；
    /// 其余（记录不在索引）标为失败。**恢复过程不执行任何命令、不生成新证据**。
    pub fn reconcile(&self, session_id: &str) -> ReconcileReport {
        let index = self.load_index(session_id);
        let mut report = ReconcileReport::default();
        let Ok(rd) = std::fs::read_dir(self.journal_dir(session_id)) else {
            return report;
        };
        for entry in rd.flatten() {
            let Ok(bytes) = std::fs::read(entry.path()) else {
                continue;
            };
            let Ok(op) = serde_json::from_slice::<OperationEntry>(&bytes) else {
                report.unreadable_journal += 1;
                continue;
            };
            // 是否被采纳只看**索引**（唯一采纳来源）；journal 只提供操作侧信息。
            let adopted = op
                .record_id
                .as_ref()
                .is_some_and(|rid| index.records.contains_key(rid));
            if !adopted {
                // 两个方向都算孤儿：journal 未提交（写在提交索引前崩溃），
                // 或 journal 说已提交但索引里没有（索引提交丢失）。只报不删。
                report.orphans.push(op.operation_id.clone());
            } else if !op.committed {
                // 记录已在索引、journal 落后：恢复为已提交（崩溃点在两者之间）。
                report.recovered.push(op.operation_id.clone());
            }
        }
        report
    }

    /// 索引中未被 journal 记录的已采纳记录（另一种孤儿方向：journal 丢了但索引已提交）。
    pub fn orphan_record_ids(&self, session_id: &str) -> BTreeSet<String> {
        let index = self.load_index(session_id);
        let mut out = BTreeSet::new();
        for rid in index.records.keys() {
            if !self
                .records_dir(session_id)
                .join(format!("records/{rid}.json"))
                .exists()
            {
                out.insert(rid.clone());
            }
        }
        out
    }

    /// 会话级联删除时清掉整个交付目录（与 cleanup 现有删除路径同口径）。
    pub fn remove_session_dir(&self, session_id: &str) -> std::io::Result<()> {
        let dir = self.delivery_dir(session_id);
        if dir.exists() {
            std::fs::remove_dir_all(&dir)?;
        }
        Ok(())
    }
}

impl std::fmt::Display for DeliveryError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.code(), self.message())
    }
}

/// 恢复对账报告。
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ReconcileReport {
    /// 已确认提交的操作（journal 落后于索引）。
    pub recovered: Vec<String>,
    /// 孤儿操作（记录未入索引，永不被采纳）。
    pub orphans: Vec<String>,
    /// 读不出来的 journal 条目数。
    pub unreadable_journal: usize,
}

fn write_new(path: &Path, bytes: &[u8]) -> Result<(), DeliveryError> {
    use std::io::Write;
    std::fs::create_dir_all(path.parent().unwrap()).map_err(|e| DeliveryError::Save {
        message: e.to_string(),
    })?;
    let mut output = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
        .map_err(|e| DeliveryError::Save {
            message: e.to_string(),
        })?;
    output
        .write_all(bytes)
        .and_then(|_| output.sync_all())
        .map_err(|e| DeliveryError::Save {
            message: e.to_string(),
        })
}

/// 宿主生成的记录 id：**只含受控字符**，不拼任何用户输入（共同契约 §4）。
///
/// 调用方**必须**用本函数取 id 并把它写进负载：回执 / 交付结果自身带的 `id`
/// 与索引键不一致时，`read_delivery_record(record_id)` 会查不到自己签发的那条记录。
pub fn host_id(operation_id: &str, kind: &str) -> String {
    format!("{}_{}", kind, content_digest(operation_id))
}

/// operation_id → journal 文件名（与 host_id 同口径的安全化）。
fn sanitize_op_id(id: &str) -> String {
    let mut s: String = id
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '_' | '-') {
                c
            } else {
                '_'
            }
        })
        .take(64)
        .collect();
    if s.is_empty() {
        s.push_str("op");
    }
    s
}

/// 内容摘要（幂等 / 冲突检测用；不稳定序的 JSON 先按结构化字符串取）。
fn content_digest(s: &str) -> String {
    crate::core::delivery::version::digest_of_bytes(s.as_bytes())
}

/// 统一时刻戳（RFC3339，毫秒精度；宿主唯一时间来源，避免各模块自定义格式分叉）。
///
/// 毫秒不是装饰：验收按 `finished_at` 取"最近一次"证据，同秒内的两次执行若取同一串
/// 时间戳就只能靠 record id 兜底比较，那不是可靠顺序。
pub fn now_iso() -> String {
    chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
}

#[cfg(test)]
#[path = "store_tests.rs"]
mod tests;
