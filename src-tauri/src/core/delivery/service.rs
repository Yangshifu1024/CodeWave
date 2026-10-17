//! 交付域生产接线（#54 持有门面，#14 / #17 经同一入口接入）。
//!
//! 这里是**唯一**能写交付边车的业务入口。四条接入纪律：
//!
//! 1. **无 `set_passed`**：验收只能由 [`DeliveryService::evaluate`] 从证据推导；
//!    没有"把某项标成通过"的接口可调。
//! 2. **身份由宿主补齐**：[`DeliveryService`] 的写接口一律接收 `RunIdentity`（调用方从真实
//!    runtime 上下文构造），不接受入参自称的身份。
//! 3. **回执只从真实执行点签发**：[`begin_execution_intent`] / [`finish_execution`]
//!    必须由命令工具的真实执行链调用；本模块不注册任何模型可调用的签发工具。
//! 4. **保存失败如实上抛**：[`DeliveryError::Save`] 不被吞成 passed——调用方保留工具已
//!    发生的真实结果，并在完成投影标 unknown/blocked。

use super::acceptance::{self, EvalContext, EvalOutcome, Ledger};
use super::store::{DeliveryError, DeliveryStore, RecordBody, WriteReceipt};
use super::types::*;
use super::version::{CaptureOptions, RootResolver};
use std::sync::Arc;

/// 交付域服务。跨会话共享一份 [`DeliveryStore`]（它内部按会话串行化索引写入）。
#[derive(Clone)]
pub struct DeliveryService {
    store: Arc<DeliveryStore>,
}

impl DeliveryService {
    /// 以 `sessions/` 目录构造。
    pub fn new(sessions_dir: std::path::PathBuf) -> Self {
        DeliveryService {
            store: Arc::new(DeliveryStore::new(sessions_dir)),
        }
    }

    /// 从真实运行时上下文构造 [`RunIdentity`]。
    ///
    /// 这是**身份的唯一来源**：工具入参里没有任何字段能指定或覆盖它
    /// （[共同契约 §2](../../../../docs/tools-and-safety.md)）。
    ///
    /// - `session_id` 取 `root_session_id ?? id`：子代理产生的证据归属主会话，
    ///   与工具结果 sidecar / 图片 blob 同一口诀（否则同一份证据会散落在多个会话）。
    /// - `root_run_id` 取 `root_run_id ?? 本 run 快照 run_id`：子代理继承父的 root run，
    ///   所以**迟到结果带的是上一 run 的 root_run_id**，验收时会被直接拒绝。
    /// - `actor_id` 对根 run 是 `root`、对子代理是子 id：同一次 run 里的两个不同子代理
    ///   彼此可区分，而"同一个子代理冒充审查自己"仍被判为同 actor。
    pub fn identity_of(rt: &crate::core::agent::SessionRuntime) -> RunIdentity {
        let is_sub = rt.root_session_id.is_some();
        let session_id = rt.root_session_id.clone().unwrap_or_else(|| rt.id.clone());
        let root_run_id = rt
            .root_run_id
            .clone()
            .or_else(|| rt.run_snapshot().run_id.clone())
            .unwrap_or_else(|| format!("runless_{}", rt.id));
        let actor_run_id = rt
            .run_snapshot()
            .run_id
            .clone()
            .unwrap_or_else(|| root_run_id.clone());
        if is_sub {
            RunIdentity {
                session_id,
                root_run_id,
                actor_id: rt.id.clone(),
                actor_run_id,
                sub_id: Some(rt.id.clone()),
            }
        } else {
            RunIdentity {
                session_id,
                root_run_id,
                actor_id: ROOT_ACTOR.to_string(),
                actor_run_id,
                sub_id: None,
            }
        }
    }

    /// 底层存储（只读读路径与清理用；写路径一律经本服务）。
    pub fn store(&self) -> &Arc<DeliveryStore> {
        &self.store
    }

    /// 由真实运行时构造根解析器：`root_id` 与 `tools/command::path_ref` 发出的**完全同源**。
    ///
    /// 契约与回执只带 `root_id`，解析必须走会话快照里的真实根（[共同契约 §2](../../../../docs/tools-and-safety.md)）；
    /// 两处各自造一套 id 会让"解析不到"被误当成"文件不存在"。
    pub fn roots_from_runtime(rt: &crate::core::agent::SessionRuntime) -> RootResolver {
        let mut out: RootResolver = RootResolver::new();
        out.insert("workspace".to_string(), rt.workspace.clone());
        out.insert("data_dir".to_string(), rt.data_dir.clone());
        if let Some(pd) = rt.project_dir.clone() {
            out.insert("project_dir".to_string(), pd);
        }
        for (i, e) in rt.extra_roots.lock().unwrap().iter().enumerate() {
            out.insert(format!("extra{i}"), std::path::PathBuf::from(e));
        }
        out
    }

    /// 由数据根构造（`data_root/sessions`）。
    pub fn from_data_root(data_root: std::path::PathBuf) -> Self {
        DeliveryService::new(data_root.join("sessions"))
    }

    /// 宿主为任务生成 id（路径与 id 都不拼用户输入）。
    pub fn new_task_id() -> String {
        format!(
            "{}_{}",
            super::TASK_ID_PREFIX,
            uuid::Uuid::new_v4().simple()
        )
    }

    // ---------- #54 契约 ----------

    /// 创建契约。`draft` 由模型提出；`owner` 由宿主补齐并**覆盖**任何自称。
    ///
    /// 校验：引用闭包、id 唯一、期望 / 种类一致、规模上限、根在快照内、审查策略可满足。
    pub fn create_contract(
        &self,
        identity: &RunIdentity,
        draft: &ContractDraft,
        root_resolver: &RootResolver,
        operation_id: &str,
    ) -> Result<(TaskContract, WriteReceipt), DeliveryError> {
        self.create_contract_with(identity, draft, root_resolver, operation_id, |_| Ok(()))
    }

    /// Prepare the frozen workflow before publishing the contract index.
    pub fn create_contract_with(
        &self,
        identity: &RunIdentity,
        draft: &ContractDraft,
        root_resolver: &RootResolver,
        operation_id: &str,
        prepare: impl FnOnce(&mut TaskContract) -> Result<(), DeliveryError>,
    ) -> Result<(TaskContract, WriteReceipt), DeliveryError> {
        let task_id = super::store::host_id(
            &format!(
                "{}:{}:{}",
                identity.session_id, identity.root_run_id, operation_id
            ),
            "task",
        );
        if let Some(existing) = self.store.load_contract(&identity.session_id, &task_id) {
            let same = existing.owner == *identity
                && existing.artifacts == draft.artifacts
                && draft
                    .checks
                    .iter()
                    .all(|check| existing.check(&check.id) == Some(check))
                && existing.requirements.len() == draft.requirements.len()
                && draft.requirements.iter().all(|requirement| {
                    existing.requirement(&requirement.id).is_some_and(|old| {
                        old.title == requirement.title
                            && old.required == requirement.required
                            && old.artifact_ids == requirement.artifact_ids
                            && requirement
                                .check_ids
                                .iter()
                                .all(|id| old.check_ids.contains(id))
                    })
                });
            if !same {
                return Err(DeliveryError::Save {
                    message: "同一 operation_id 携带不同契约，拒绝覆盖".into(),
                });
            }
            return Ok((
                existing,
                WriteReceipt {
                    operation_id: operation_id.into(),
                    revision: self.store.load_index(&identity.session_id).revision,
                    record_id: task_id,
                    persisted: true,
                    warnings: vec![],
                },
            ));
        }
        // 根必须真的在会话快照里：模型自带的 root_id 不能指向未授权目录。
        for r in &draft.roots {
            if !root_resolver.contains_key(&r.root_id) {
                return Err(DeliveryError::ContractInvalid {
                    reasons: vec![Reason {
                        code: "root_not_in_snapshot".into(),
                        message: format!("根 `{}` 不在本会话快照内，拒绝采纳", r.root_id),
                        target_id: Some(r.root_id.clone()),
                    }],
                });
            }
        }
        if draft.scope_changes.iter().any(|change| {
            matches!(
                change.kind,
                ScopeChangeKind::Cancelled | ScopeChangeKind::Waived
            )
        }) {
            return Err(DeliveryError::Save {
                message: "新契约不能携带自签取消或豁免；先创建后通过真实用户决定修订".into(),
            });
        }
        let mut sanitized = draft.clone();
        for root in &mut sanitized.roots {
            root.absolute = root_resolver[&root.root_id].to_string_lossy().into_owned();
        }
        let mut contract = sanitized.into_contract(task_id, identity.clone());
        prepare(&mut contract)?;
        // 机械任务豁免独立审查时必须留下路由依据，否则"省掉审查"没有出处。
        if !contract.review_policy.required
            && contract.review_policy.independent
            && contract.checks.iter().all(|c| c.kind != CheckKind::Review)
            && contract.scope_changes.is_empty()
        {
            return Err(DeliveryError::ContractInvalid {
                reasons: vec![Reason {
                    code: "review_waiver_without_routing".into(),
                    message: "未要求审查且无审查检查时，必须先由 #3/#4 路由留下依据（ScopeChange）"
                        .into(),
                    target_id: None,
                }],
            });
        }
        if let Err(reasons) = acceptance::validate_contract_shape(&contract) {
            return Err(DeliveryError::ContractInvalid { reasons });
        }
        let scope = VerificationScope {
            inputs: contract
                .checks
                .iter()
                .flat_map(|c| {
                    c.scope
                        .inputs
                        .iter()
                        .chain(&c.scope.outputs)
                        .chain(&c.scope.dependency_files)
                })
                .cloned()
                .chain(contract.artifacts.iter().map(|a| a.path.clone()))
                .collect(),
            ..Default::default()
        };
        let baseline = super::version::capture_scope(root_resolver, &scope, &Default::default())
            .map_err(|e| DeliveryError::Save {
                message: format!("基线采集失败: {e:?}"),
            })?;
        if !baseline.is_valid() {
            return Err(DeliveryError::Save {
                message: "基线未能完整读取".into(),
            });
        }
        self.store
            .save_baseline(&identity.session_id, &contract.task_id, &baseline)?;
        let receipt = self.store.save_contract(
            &identity.session_id,
            identity,
            &contract.task_id,
            None,
            &contract,
            operation_id,
        )?;
        contract.revision = receipt.revision;
        Ok((contract, receipt))
    }

    /// 修订契约。**取消 / 豁免只能凭真实用户决定**（`user_decision_id` 必填且须已采纳）。
    pub fn revise_contract(
        &self,
        identity: &RunIdentity,
        task_id: &str,
        expected_revision: u64,
        changes: Vec<ScopeChange>,
        user_decision_id: Option<&str>,
        operation_id: &str,
    ) -> Result<(TaskContract, WriteReceipt), DeliveryError> {
        self.revise_contract_with(
            identity,
            task_id,
            expected_revision,
            changes,
            user_decision_id,
            operation_id,
            None,
            |_| Ok(()),
        )
    }

    #[allow(clippy::too_many_arguments)]
    pub fn revise_contract_with(
        &self,
        identity: &RunIdentity,
        task_id: &str,
        expected_revision: u64,
        changes: Vec<ScopeChange>,
        user_decision_id: Option<&str>,
        operation_id: &str,
        roots: Option<&RootResolver>,
        prepare: impl FnOnce(&mut TaskContract) -> Result<(), DeliveryError>,
    ) -> Result<(TaskContract, WriteReceipt), DeliveryError> {
        let mut contract = self
            .store
            .load_contract(&identity.session_id, task_id)
            .ok_or_else(|| DeliveryError::NotFound {
                message: format!("任务 {task_id} 无有效契约"),
            })?;
        if contract.revision != expected_revision {
            return Err(DeliveryError::ContractStale {
                expected: expected_revision,
                actual: contract.revision,
            });
        }
        if !contract.owner.same_actor(identity) || contract.owner.session_id != identity.session_id
        {
            return Err(DeliveryError::OwnerMismatch {
                message: "修订者不是契约属主".into(),
            });
        }
        let ledger = self.ledger(&identity.session_id);
        for mut c in changes {
            if matches!(c.kind, ScopeChangeKind::Cancelled | ScopeChangeKind::Waived) {
                let Some(did) = user_decision_id else {
                    return Err(DeliveryError::ContractInvalid {
                        reasons: vec![Reason {
                            code: "scope_change_without_user_decision".into(),
                            message: "取消 / 豁免必须引用真实用户决定".into(),
                            target_id: Some(c.target_id.clone()),
                        }],
                    });
                };
                let Some(d) = ledger.decisions.get(did) else {
                    return Err(DeliveryError::EvidenceInvalid {
                        reasons: vec![Reason {
                            code: "user_decision_not_found".into(),
                            message: format!("用户决定 `{did}` 未被采纳，拒绝据此修订范围"),
                            target_id: Some(c.target_id.clone()),
                        }],
                    });
                };
                let want = match c.kind {
                    ScopeChangeKind::Waived => UserDecisionKind::Waived,
                    _ => UserDecisionKind::Cancelled,
                };
                let fresh = roots
                    .and_then(|roots| {
                        target_scope(&contract, &c.target_id).and_then(|scope| {
                            super::version::capture_scope(roots, &scope, &Default::default()).ok()
                        })
                    })
                    .is_some_and(|version| {
                        version.is_valid()
                            && d.scope_version.as_deref() == Some(version.digest.as_str())
                    });
                if !d
                    .binding
                    .as_ref()
                    .is_some_and(|binding| binding.matches(&contract))
                    || !fresh
                    || d.decision != want
                    || d.owner.session_id != identity.session_id
                    || d.owner.root_run_id != identity.root_run_id
                    || !d.item_ids.contains(&c.target_id)
                {
                    return Err(DeliveryError::EvidenceInvalid {
                        reasons: vec![Reason {
                            code: "user_decision_kind_mismatch".into(),
                            message: format!("用户决定 `{did}` 的类型与范围变更不匹配"),
                            target_id: Some(c.target_id.clone()),
                        }],
                    });
                }
                c.user_decision_id = Some(did.to_string());
            }
            if c.id.is_empty() || c.at.is_empty() {
                c.id = format!("sc_{}", super::store::now_iso());
                c.at = super::store::now_iso();
            }
            contract.scope_changes.push(c);
        }
        contract.revision = expected_revision + 1;
        prepare(&mut contract)?;
        if let Err(reasons) = acceptance::validate_contract_shape(&contract) {
            return Err(DeliveryError::ContractInvalid { reasons });
        }
        let receipt = self.store.save_contract(
            &identity.session_id,
            identity,
            task_id,
            Some(expected_revision),
            &contract,
            operation_id,
        )?;
        Ok((contract, receipt))
    }

    /// 读契约。
    pub fn read_contract(&self, session_id: &str, task_id: &str) -> Option<TaskContract> {
        self.store.load_contract(session_id, task_id)
    }

    /// 本会话当前生效的契约（完成门用）。
    ///
    /// 没有契约就是**没有交付要求**，不是"要求未知"：完成门据此沿原收尾路径放行，
    /// 不为每条普通问答加门。多任务并存时返回修订号最高的那个。
    pub fn active_contract(&self, session_id: &str) -> Option<TaskContract> {
        let index = self.store.load_index(session_id);
        if let Some(task) = index.active_task_id.as_deref() {
            return self.store.load_contract(session_id, task);
        }
        let task_id = index
            .contracts
            .values()
            .max_by_key(|e| e.revision)
            .map(|e| e.task_id.clone())?;
        self.store.load_contract(session_id, &task_id)
    }

    /// Current-run selection never adopts a previous task's contract or evidence.
    pub fn active_contract_for_run(
        &self,
        session_id: &str,
        root_run_id: &str,
    ) -> Option<TaskContract> {
        let index = self.store.load_index(session_id);
        if let Some(task) = index.active_task_id.as_deref()
            && let Some(entry) = index.contracts.get(task)
            && entry.owner.root_run_id == root_run_id
        {
            return self.store.load_contract(session_id, task);
        }
        index
            .contracts
            .values()
            .filter(|entry| entry.owner.root_run_id == root_run_id)
            .max_by_key(|entry| entry.revision)
            .and_then(|entry| self.store.load_contract(session_id, &entry.task_id))
    }

    // ---------- #16 有界修复预算 ----------

    /// 读修复预算。**老会话没有该记录时按零预算读回**（显示"未验证"），
    /// 绝不补造"已用过多少轮"的假事实。
    pub fn load_budget(&self, session_id: &str, task_id: &str) -> super::completion::RepairBudget {
        // 取**最新**一条：记录 id 由 operation_id 派生，字典序不等于时间序，
        // 所以按写入时刻（ISO 8601，可字典序比较）挑，而不是取末条。
        let index = self.store.load_index(session_id);
        index
            .records
            .keys()
            .filter_map(|rid| self.store.load_record(session_id, rid))
            .filter_map(|r| match r.body {
                super::store::RecordBody::RepairBudget(b) => Some((r.at, *b)),
                _ => None,
            })
            .filter(|(_, b)| b.task_id == task_id)
            .max_by(|a, b| a.0.cmp(&b.0))
            .map(|(_, b)| b.budget)
            .unwrap_or_default()
    }

    /// 落修复预算（**先落盘再继续**：预算丢了就等于悄悄放开上限）。
    pub fn save_budget(
        &self,
        session_id: &str,
        owner: &RunIdentity,
        task_id: &str,
        budget: &super::completion::RepairBudget,
        operation_id: &str,
    ) -> Result<WriteReceipt, DeliveryError> {
        let state = super::store::RepairBudgetState {
            task_id: task_id.to_string(),
            budget: budget.clone(),
        };
        self.store.write_record(
            session_id,
            owner,
            operation_id,
            super::store::RecordBody::RepairBudget(Box::new(state)),
            None,
        )
    }

    // ---------- #14 执行回执 ----------

    /// 建立执行意向（命令已受理，结果未定）。
    ///
    /// 保存的是**有界脱敏请求**；`requested_cwd` 绝不当作实际 cwd 使用。
    pub fn begin_execution_intent(
        &self,
        identity: &RunIdentity,
        requested_command: &str,
        requested_cwd: Option<&str>,
        requested_timeout_ms: Option<u64>,
        check_id: Option<&str>,
        operation_id: &str,
    ) -> Result<WriteReceipt, DeliveryError> {
        let intent = ExecutionIntent {
            schema: SCHEMA,
            operation_id: operation_id.to_string(),
            owner: identity.clone(),
            check_id: check_id.map(str::to_string),
            requested_command: bound(requested_command),
            requested_cwd: requested_cwd.map(bound),
            requested_timeout_ms,
            created_at: super::store::now_iso(),
        };
        self.store.write_record(
            &identity.session_id,
            identity,
            operation_id,
            RecordBody::Intent(Box::new(intent)),
            None,
        )
    }

    /// 结束执行：把 intent 终结为一张回执并采纳。
    ///
    /// `resolved` / `cwd` 只有真实解析成功才给 `Some`；被拒时留 `None` 并填 `reason`——
    /// **不用默认目录补值**，也不伪造 program/argv。
    #[allow(clippy::too_many_arguments)]
    pub fn finish_execution(
        &self,
        identity: &RunIdentity,
        operation_id: &str,
        requested: CommandSpec,
        resolved: Option<CommandSpec>,
        cwd: Option<PathRef>,
        outcome: ExecutionOutcome,
        exit_code: Option<i32>,
        signal: Option<String>,
        output: Option<&[u8]>,
        before: Option<super::version::ScopeVersion>,
        after: Option<super::version::ScopeVersion>,
        reason_text: Option<String>,
        check_id: Option<&str>,
    ) -> Result<(ExecutionReceipt, WriteReceipt), DeliveryError> {
        self.finish_execution_bound(
            identity,
            operation_id,
            requested,
            resolved,
            cwd,
            outcome,
            exit_code,
            signal,
            output,
            before,
            after,
            reason_text,
            check_id,
            None,
        )
    }

    #[allow(clippy::too_many_arguments)]
    pub fn finish_execution_bound(
        &self,
        identity: &RunIdentity,
        operation_id: &str,
        requested: CommandSpec,
        resolved: Option<CommandSpec>,
        cwd: Option<PathRef>,
        outcome: ExecutionOutcome,
        exit_code: Option<i32>,
        signal: Option<String>,
        output: Option<&[u8]>,
        before: Option<super::version::ScopeVersion>,
        after: Option<super::version::ScopeVersion>,
        reason_text: Option<String>,
        check_id: Option<&str>,
        binding: Option<ContractBinding>,
    ) -> Result<(ExecutionReceipt, WriteReceipt), DeliveryError> {
        // 退出码与终态必须自洽：spawn 失败 / 取消 / 超时都**没有**退出码。
        if !outcome.has_exit_code() && exit_code.is_some() {
            return Err(DeliveryError::EvidenceInvalid {
                reasons: vec![Reason {
                    code: "exit_code_without_exit".into(),
                    message: format!("终态 {} 不应有退出码", outcome.as_str()),
                    target_id: None,
                }],
            });
        }
        if outcome.has_exit_code() && exit_code.is_none() {
            return Err(DeliveryError::EvidenceInvalid {
                reasons: vec![Reason {
                    code: "missing_exit_code".into(),
                    message: "终态标为已退出但缺退出码".into(),
                    target_id: None,
                }],
            });
        }
        // 命令回执必须带已解析形态才能声称"跑过"。
        if outcome.executed() && resolved.is_none() {
            return Err(DeliveryError::EvidenceInvalid {
                reasons: vec![Reason {
                    code: "resolved_command_missing".into(),
                    message: "声称已执行却没有已解析命令形态".into(),
                    target_id: None,
                }],
            });
        }
        // 记录 id 必须与索引键同源（store::host_id），否则回执自带的 id 查不回自己。
        let id = super::store::host_id(operation_id, "execution");
        // 输出边车预算：超限截断并标记，验收依赖完整输出者因此保持未证明。
        let (output_ref, truncated, mut warnings) = match output {
            Some(bytes) if !bytes.is_empty() => {
                let safe = super::recorder::redact(&String::from_utf8_lossy(bytes));
                let (r, t) = self
                    .store
                    .write_output(&identity.session_id, &id, safe.as_bytes())?;
                (Some(r), t || bytes.len() > 8 * 1024 * 1024, Vec::new())
            }
            _ => (None, false, Vec::new()),
        };
        if truncated {
            warnings.push("输出超出预算已截断，依赖完整输出的检查保持未证明".to_string());
        }
        if let Some(w) = self.store.soft_line_warning(&identity.session_id) {
            warnings.push(w);
        }
        let requested = match requested {
            CommandSpec::Requested {
                command_text,
                shell_hint,
            } => CommandSpec::Requested {
                command_text: super::recorder::redact(&command_text),
                shell_hint,
            },
            other => other,
        };
        let resolved = resolved.map(|command| match command {
            CommandSpec::Resolved {
                program,
                argv,
                shell_family,
                shell_command,
                timeout_ms,
                config_digest,
            } => CommandSpec::Resolved {
                program,
                argv: argv
                    .into_iter()
                    .map(|arg| super::recorder::redact(&arg))
                    .collect(),
                shell_family,
                shell_command: shell_command.map(|c| super::recorder::redact(&c)),
                timeout_ms,
                config_digest,
            },
            other => other,
        });
        let receipt = ExecutionReceipt {
            binding,
            schema: SCHEMA,
            id: id.clone(),
            owner: identity.clone(),
            check_id: check_id.map(str::to_string),
            requested,
            resolved,
            cwd,
            started_at: self
                .store
                .record_ids(&identity.session_id, None)
                .into_iter()
                .filter_map(|id| self.store.load_record(&identity.session_id, &id))
                .find_map(|r| match r.body {
                    RecordBody::Intent(i) if i.operation_id == operation_id => Some(i.created_at),
                    _ => None,
                })
                .unwrap_or_else(super::store::now_iso),
            finished_at: super::store::now_iso(),
            outcome,
            exit_code,
            signal,
            output_ref,
            output_truncated: truncated,
            before,
            after,
            issuer: ReceiptIssuer::Host,
            reason: reason_text,
        };
        let w = self.store.write_record(
            &identity.session_id,
            identity,
            operation_id,
            RecordBody::Execution(Box::new(receipt.clone())),
            None,
        )?;
        Ok((receipt, WriteReceipt { warnings, ..w }))
    }

    /// 落一条宿主注册校验器的结论（**不含 command / exit_code**）。
    pub fn record_validation(
        &self,
        identity: &RunIdentity,
        record: BuiltinValidationRecord,
        operation_id: &str,
    ) -> Result<WriteReceipt, DeliveryError> {
        self.store.write_record(
            &identity.session_id,
            identity,
            operation_id,
            RecordBody::Validation(Box::new(record)),
            None,
        )
    }

    /// 落一条宿主签发的审查分派（[#10](../../../../docs/tools-and-safety.md)）。
    ///
    /// 这是审查职责的**唯一来源**：模型自称「我是审查者」不产生分派，也就拿不到职责只读上限。
    pub fn record_review_assignment(
        &self,
        identity: &RunIdentity,
        assignment: super::responsibility::ReviewAssignment,
        operation_id: &str,
    ) -> Result<WriteReceipt, DeliveryError> {
        let id = assignment.review_id.clone();
        self.store.write_record(
            &identity.session_id,
            identity,
            operation_id,
            RecordBody::ReviewAssignment(Box::new(assignment)),
            None,
        )?;
        Ok(WriteReceipt {
            operation_id: operation_id.to_string(),
            revision: self.store.load_index(&identity.session_id).revision,
            record_id: id,
            persisted: true,
            warnings: Vec::new(),
        })
    }

    /// 取一份已登记的审查分派。
    pub fn read_review_assignment(
        &self,
        session_id: &str,
        review_id: &str,
    ) -> Option<super::responsibility::ReviewAssignment> {
        self.ledger(session_id)
            .review_assignments
            .get(review_id)
            .cloned()
    }

    /// 落一条独立审查结论。
    pub fn record_review(
        &self,
        identity: &RunIdentity,
        review: ReviewResult,
        operation_id: &str,
    ) -> Result<WriteReceipt, DeliveryError> {
        self.store.write_record(
            &identity.session_id,
            identity,
            operation_id,
            RecordBody::Review(Box::new(review.clone())),
            Some(&review.task_id),
        )
    }

    /// 落一条真实用户决定（只能由宿主交互应答调用）。
    pub fn record_user_decision(
        &self,
        identity: &RunIdentity,
        decision: UserDecision,
        operation_id: &str,
    ) -> Result<WriteReceipt, DeliveryError> {
        self.store.write_record(
            &identity.session_id,
            identity,
            operation_id,
            RecordBody::UserDecision(Box::new(decision)),
            None,
        )
    }

    /// Observe the actor's real starting content, including serial writes after another actor.
    pub fn capture_actor_baseline(
        &self,
        identity: &RunIdentity,
        contract: &TaskContract,
        roots: &RootResolver,
        writes: &[PathRef],
    ) -> Result<(), DeliveryError> {
        if identity.session_id != contract.owner.session_id
            || identity.root_run_id != contract.owner.root_run_id
        {
            return Err(DeliveryError::OwnerMismatch {
                message: "派发基线归属不匹配".into(),
            });
        }
        let paths = if writes.is_empty() {
            contract
                .artifacts
                .iter()
                .map(|a| a.path.clone())
                .chain(contract.checks.iter().flat_map(|c| c.scope.outputs.clone()))
                .collect()
        } else {
            writes.to_vec()
        };
        let version = super::version::capture_scope(
            roots,
            &VerificationScope {
                inputs: paths,
                ..Default::default()
            },
            &Default::default(),
        )
        .map_err(|e| DeliveryError::Save {
            message: format!("派发基线读取失败: {e:?}"),
        })?;
        if !version.is_valid() {
            return Err(DeliveryError::Save {
                message: "派发基线不完整".into(),
            });
        }
        self.store.save_baseline(
            &identity.session_id,
            &format!("{}__{}", contract.task_id, identity.actor_id),
            &version,
        )
    }

    // ---------- #17 结构化交付 ----------

    /// 提交结构化交付草稿（仅子 runtime 向自己的 root_run 提交）。
    ///
    /// 校验：属主 / 契约修订一致、引用存在、状态与 findings 不矛盾。**完整性独立于
    /// `SubEnded` 四象限**——预算尾部生成的有效草稿也不会把 `partial` 升格为完整交付。
    /// 参数偏多是刻意的：草稿六个分面（完成 / 未完成 / 改动 / 证据 / 阻塞 / 契约修订）
    /// 各自独立，合成一个入参结构体只会让人漏填其中一面而无人察觉。
    #[allow(clippy::too_many_arguments)]
    pub fn submit_delivery(
        &self,
        identity: &RunIdentity,
        contract: &TaskContract,
        ended: EndedKind,
        completed: Vec<String>,
        incomplete: Vec<IncompleteItem>,
        changes: Vec<ChangeRef>,
        evidence: Vec<String>,
        blockers: Vec<Blocker>,
        operation_id: &str,
    ) -> Result<(DeliveryResult, WriteReceipt, SubmissionState), DeliveryError> {
        // 只允许子 runtime 向自己的 root_run 提交（根身份不能自称子代理）。
        if identity.sub_id.is_none() {
            return Err(DeliveryError::OwnerMismatch {
                message: "结构化交付仅接受子 runtime 向自己的 root_run 提交".into(),
            });
        }
        if identity.root_run_id != contract.owner.root_run_id {
            return Err(DeliveryError::OwnerMismatch {
                message: "交付所属 root_run 与契约不一致（迟到结果不写入下一 run）".into(),
            });
        }
        // **会话也必须一致**：root_run_id 只是字符串，另一个会话的子代理完全可以取到同名值。
        // 只比 root_run 会让跨会话的交付写进别人的契约索引（记录还会落到提交者自己的
        // 会话目录，结果是两边各有一份事实、谁也验收不到）。
        if identity.session_id != contract.owner.session_id {
            return Err(DeliveryError::OwnerMismatch {
                message: format!(
                    "交付所属会话 {} 与契约会话 {} 不一致",
                    identity.session_id, contract.owner.session_id
                ),
            });
        }
        // 契约修订漂移：按旧修订交付的草稿不适用当前契约，交由宿主在终态复核。
        if contract.owner.session_id.is_empty() {
            return Err(DeliveryError::OwnerMismatch {
                message: "契约缺少属主身份".into(),
            });
        }
        let baseline = self
            .store
            .load_baseline(
                &identity.session_id,
                &format!("{}__{}", contract.task_id, identity.actor_id),
            )
            .or_else(|| {
                self.store
                    .load_baseline(&identity.session_id, &contract.task_id)
            });
        for change in &changes {
            let entry = baseline
                .as_ref()
                .filter(|b| b.is_valid())
                .and_then(|b| b.entry(&change.path.root_id, &change.path.relative_path));
            let known_absent = baseline.as_ref().is_some_and(|b| {
                b.is_valid()
                    && b.entries.iter().any(|entry| {
                        entry.root_id == change.path.root_id
                            && entry.kind == super::version::EntryKind::Directory
                            && std::path::Path::new(&change.path.relative_path)
                                .starts_with(&entry.relative_path)
                    })
            });
            let valid = (entry.is_none()
                && known_absent
                && change.kind == ChangeKind::Created
                && change.before_hash.is_none())
                || entry.is_some_and(|entry| match change.kind {
                    ChangeKind::Created => {
                        entry.kind == super::version::EntryKind::Missing
                            && change.before_hash.is_none()
                    }
                    ChangeKind::Modified | ChangeKind::Deleted => {
                        entry.readable
                            && entry.sha256.is_some()
                            && entry.sha256 == change.before_hash
                    }
                });
            if !valid {
                return Err(DeliveryError::EvidenceInvalid {
                    reasons: vec![Reason {
                        code: "untrusted_change_baseline".into(),
                        message: format!(
                            "{} 的变更前状态不匹配宿主基线；必须先在契约显式声明该路径",
                            change.path.relative_path
                        ),
                        target_id: None,
                    }],
                });
            }
        }
        let mut problems: Vec<Reason> = Vec::new();
        // 引用闭包：completed / incomplete 的 id 必须落在契约内。
        for cid in &completed {
            let known = contract.requirement(cid).is_some() || contract.artifact(cid).is_some();
            if !known {
                problems.push(Reason {
                    code: "unknown_completed_id".into(),
                    message: format!("完成项 `{cid}` 不在契约内"),
                    target_id: Some(cid.clone()),
                });
            }
        }
        for it in &incomplete {
            let known = contract.requirement(&it.target_id).is_some()
                || contract.artifact(&it.target_id).is_some();
            if !known {
                problems.push(Reason {
                    code: "unknown_incomplete_id".into(),
                    message: format!("未完成项 `{}` 不在契约内", it.target_id),
                    target_id: Some(it.target_id.clone()),
                });
            }
        }
        // 状态矛盾：同一 id 不能既完成又未完成。
        for cid in &completed {
            if incomplete.iter().any(|i| &i.target_id == cid) {
                problems.push(Reason {
                    code: "contradictory_item_state".into(),
                    message: format!("`{cid}` 同时列为完成与未完成"),
                    target_id: Some(cid.clone()),
                });
            }
        }
        // 预算耗尽 / 未交报告时不得声称"全部完成"（#18）。
        if matches!(ended, EndedKind::Budget | EndedKind::NoReport) && !completed.is_empty() {
            problems.push(Reason {
                code: "completion_without_report".into(),
                message: format!("收尾形态为 {}，不得声明已完成项", ended_str(ended)),
                target_id: None,
            });
        }
        let state = if problems.is_empty() {
            SubmissionState::Valid
        } else {
            SubmissionState::Invalid
        };
        let result = DeliveryResult {
            schema: SCHEMA,
            id: super::store::host_id(operation_id, "delivery"),
            contract_revision: contract.revision,
            owner: identity.clone(),
            task_id: contract.task_id.clone(),
            ended,
            completed,
            incomplete,
            changes,
            evidence,
            blockers,
            submitted_at: super::store::now_iso(),
        };
        let receipt = self.store.write_record(
            &identity.session_id,
            identity,
            operation_id,
            RecordBody::Delivery(Box::new(result.clone())),
            Some(&contract.task_id),
        )?;
        Ok((result, receipt, state))
    }

    // ---------- 验收 ----------

    /// 装配证据账本。**只收索引内已采纳记录**——孤儿永不参与判定。
    pub fn ledger(&self, session_id: &str) -> Ledger {
        let mut ledger = Ledger::new();
        for rid in self.store.record_ids(session_id, None) {
            let Some(rec) = self.store.load_record(session_id, &rid) else {
                continue;
            };
            match rec.body {
                RecordBody::Execution(r) => {
                    ledger.receipts.insert(rec.id, *r);
                }
                RecordBody::Validation(v) => {
                    ledger.validations.insert(rec.id, *v);
                }
                RecordBody::Review(r) => {
                    ledger.reviews.insert(rec.id, *r);
                }
                RecordBody::UserDecision(d) => {
                    ledger.decisions.insert(rec.id, *d);
                }
                RecordBody::Delivery(d) => {
                    ledger.deliveries.insert(rec.id, *d);
                }
                RecordBody::ReviewAssignment(a) => {
                    ledger.review_assignments.insert(a.review_id.clone(), *a);
                }
                // 未决意向 / 派生快照 / 修复预算都不是"证据"：它们不能证明任何检查通过。
                RecordBody::Intent(_) | RecordBody::Acceptance(_) | RecordBody::RepairBudget(_) => {
                }
            }
        }
        ledger
    }

    /// 求值并返回只读快照 + **落盘是否成功**。**不提供 set_passed**——通过只能这样推导出来。
    ///
    /// 返回的 `save_ok = false` 表示快照**没能成为已采纳事实**（写失败 / 索引未提交）。
    /// 完成门据此阻止通过：把保存失败伪装成事实，正是「退出 0 就当完成」的另一种写法。
    pub fn evaluate(
        &self,
        session_id: &str,
        task_id: &str,
        root_resolver: &RootResolver,
    ) -> Result<(AcceptanceSnapshot, bool), DeliveryError> {
        self.evaluate_inner(session_id, task_id, root_resolver, true)
    }

    /// Recompute against current content without publishing or mutating evidence.
    pub fn evaluate_current(
        &self,
        session_id: &str,
        task_id: &str,
        roots: &RootResolver,
    ) -> Result<AcceptanceSnapshot, DeliveryError> {
        self.evaluate_inner(session_id, task_id, roots, false)
            .map(|(snapshot, _)| snapshot)
    }
    fn evaluate_inner(
        &self,
        session_id: &str,
        task_id: &str,
        root_resolver: &RootResolver,
        persist: bool,
    ) -> Result<(AcceptanceSnapshot, bool), DeliveryError> {
        self.store.ensure_readable(session_id)?;
        let contract = self
            .store
            .load_contract(session_id, task_id)
            .ok_or_else(|| DeliveryError::NotFound {
                message: format!("任务 {task_id} 无有效契约"),
            })?;
        let ledger = self.ledger(session_id);
        let ctx = EvalContext {
            contract: &contract,
            ledger: &ledger,
            roots: root_resolver,
            capture: CaptureOptions::default(),
        };
        let mut out: EvalOutcome = acceptance::evaluate(&ctx)
            .map_err(|reasons| DeliveryError::ContractInvalid { reasons })?;
        let deliveries: Vec<_> = ledger
            .deliveries
            .values()
            .filter(|d| {
                d.task_id == task_id
                    && d.owner.session_id == contract.owner.session_id
                    && d.owner.root_run_id == contract.owner.root_run_id
                    && !d.changes.is_empty()
                    && ledger.validations.values().any(|validation| {
                        validation.validator_id == "sub_delivery"
                            && validation
                                .binding
                                .as_ref()
                                .is_some_and(|binding| binding.matches(&contract))
                            && validation.owner == d.owner
                            && validation.outcome == ValidationOutcome::Passed
                            && validation.input_refs.contains(&d.id)
                    })
            })
            .collect();
        if deliveries.len() > 1 {
            let latest = deliveries
                .iter()
                .map(|d| d.submitted_at.as_str())
                .max()
                .unwrap_or("");
            let mut overall_ledger = ledger.clone();
            overall_ledger
                .receipts
                .retain(|_, r| r.owner.actor_id == ROOT_ACTOR && r.started_at.as_str() >= latest);
            let checks: Vec<_> = contract
                .checks
                .iter()
                .filter(|c| c.required && c.kind == CheckKind::Command)
                .collect();
            let scope = VerificationScope {
                inputs: checks
                    .iter()
                    .flat_map(|c| {
                        c.scope
                            .inputs
                            .iter()
                            .chain(&c.scope.dependency_files)
                            .chain(&c.scope.outputs)
                    })
                    .cloned()
                    .collect(),
                environment_keys: checks
                    .iter()
                    .flat_map(|c| c.scope.environment_keys.clone())
                    .collect(),
                ..Default::default()
            };
            let current =
                super::version::capture_scope(root_resolver, &scope, &Default::default()).ok();
            let target = super::integration::IntegrationTarget {
                contract_revision: contract.revision,
                accepted_delivery_ids: deliveries.iter().map(|d| d.id.clone()).collect(),
                expected_parent_version: current
                    .as_ref()
                    .map(|v| v.digest.clone())
                    .unwrap_or_default(),
                result_version: None,
                required_check_ids: checks.iter().map(|c| c.id.clone()).collect(),
                status: super::integration::IntegrationStatus::Pending,
                shared_resources: vec![],
                reason: None,
                not_required_basis: None,
            };
            let integration = super::integration::evaluate_integration(
                &target,
                &contract,
                &overall_ledger,
                root_resolver,
                &Default::default(),
                &scope,
                current.as_ref(),
                current.as_ref(),
            );
            if checks.is_empty()
                || deliveries.iter().any(|d| {
                    d.ended != EndedKind::Report
                        || !d.incomplete.is_empty()
                        || !d.blockers.is_empty()
                })
                || !integration.status.allows_completion()
            {
                out.overall = AcceptanceStatusKind::Blocked;
                out.reasons.push(Reason {
                    code: "integration_not_verified".into(),
                    message: integration
                        .reason
                        .unwrap_or_else(|| "多子交付合流后必须由根执行整体验证".into()),
                    target_id: None,
                });
            }
        }
        let prev_rev = self
            .store
            .load_index(session_id)
            .contracts
            .get(task_id)
            .map(|e| e.acceptance_revision)
            .unwrap_or(0);
        let snapshot = AcceptanceSnapshot {
            task_id: task_id.to_string(),
            contract_revision: contract.revision,
            // 派生游标自增；**旧 revision 不覆盖当前值**由调用方按 prev_rev 比对。
            revision: prev_rev + 1,
            items: out.items,
            overall: out.overall,
            waived_count: out.waived_count,
            evaluated_version: out.evaluated_version,
            reasons: out.reasons,
        };
        let save_ok = persist
            && self
                .store
                .save_acceptance(
                    session_id,
                    &contract.owner,
                    task_id,
                    &snapshot,
                    &format!("acc_{}_{}", task_id, snapshot.revision),
                )
                .is_ok();
        Ok((snapshot, save_ok))
    }

    /// 只读快照：没有契约时返回 `None`，**不编造"未验证"之外的状态**。
    pub fn read_snapshot(&self, session_id: &str, task_id: &str) -> Option<AcceptanceSnapshot> {
        let index = self.store.load_index(session_id);
        let task_records: Vec<String> = index
            .records
            .keys()
            .filter(|rid| {
                self.store
                    .load_record(session_id, rid)
                    .map(|r| matches!(r.body, RecordBody::Acceptance(_)))
                    .unwrap_or(false)
            })
            .cloned()
            .collect();
        let mut best: Option<AcceptanceSnapshot> = None;
        for rid in task_records {
            if let Some(rec) = self.store.load_record(session_id, &rid)
                && let RecordBody::Acceptance(s) = rec.body
            {
                let s = *s;
                if s.task_id == task_id && best.as_ref().is_none_or(|b| s.revision > b.revision) {
                    best = Some(s);
                }
            }
        }
        best
    }
}

/// 有界脱敏（共同契约 §4：入参文本进边车前必须有界）。
fn bound(s: &str) -> String {
    let redacted = super::recorder::redact(s);
    let s = redacted.as_str();
    const MAX: usize = 512;
    if s.chars().count() <= MAX {
        return s.to_string();
    }
    let mut out: String = s.chars().take(MAX).collect();
    out.push('…');
    out
}

fn ended_str(e: EndedKind) -> &'static str {
    match e {
        EndedKind::Report => "report",
        EndedKind::Partial => "partial",
        EndedKind::Budget => "budget",
        EndedKind::NoReport => "no_report",
    }
}

/// Declared content to which a real scope decision applies.
pub fn target_scope(contract: &TaskContract, target: &str) -> Option<VerificationScope> {
    if let Some(check) = contract.check(target) {
        return Some(check.scope.clone());
    }
    if let Some(artifact) = contract.artifact(target) {
        return Some(VerificationScope {
            inputs: vec![artifact.path.clone()],
            ..Default::default()
        });
    }
    let requirement = contract.requirement(target)?;
    let mut scope = VerificationScope::default();
    for check in contract
        .checks
        .iter()
        .filter(|c| requirement.check_ids.contains(&c.id))
    {
        scope.inputs.extend(check.scope.inputs.clone());
        scope.outputs.extend(check.scope.outputs.clone());
        scope
            .dependency_files
            .extend(check.scope.dependency_files.clone());
        scope
            .environment_keys
            .extend(check.scope.environment_keys.clone());
    }
    scope.inputs.extend(
        contract
            .artifacts
            .iter()
            .filter(|a| requirement.artifact_ids.contains(&a.id))
            .map(|a| a.path.clone()),
    );
    Some(scope)
}
