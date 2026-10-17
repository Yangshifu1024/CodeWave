//! Frozen regression corpus using current production component and completion gates.
//! Inputs are frozen and materialized into disposable roots. No provider, native
//! credential backend, process-wide environment mutation, or user-home scan occurs.
use super::{AgentCore, SessionRuntime, drive, supervise, test_support};
use crate::core::config::ConfigState;
use crate::tools::{Tool, ToolCtx};
use serde::{Deserialize, Serialize};
use serde_json::json;
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

struct ForbiddenCredentials;
impl crate::core::config_commit::CredentialStore for ForbiddenCredentials {
    fn read(&self, _: &str) -> Result<Vec<String>, crate::core::config_commit::ConfigCommitError> {
        panic!("evaluation fixture attempted credential access")
    }
    fn write(
        &self,
        _: &str,
        _: &[String],
    ) -> Result<(), crate::core::config_commit::ConfigCommitError> {
        panic!("evaluation fixture attempted credential access")
    }
    fn delete(&self, _: &str) -> Result<(), crate::core::config_commit::ConfigCommitError> {
        panic!("evaluation fixture attempted credential access")
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Corpus {
    schema: u32,
    inputs: BTreeMap<String, String>,
    cases: Vec<Case>,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
enum Scenario {
    FalseCompletion,
    MissingChildResult,
    BudgetPartial,
    MissingArtifact,
    WorkspaceDrift,
    RepeatedIdle,
    UntrustedResource,
    OutsideRead,
    ProtectedStateWrite,
}

#[derive(Debug, PartialEq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
enum Observation {
    Blocked,
    Detected,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Case {
    id: Scenario,
    expected: Observation,
}

fn corpus() -> Corpus {
    let corpus: Corpus = serde_json::from_str(include_str!(
        "../../../tests/fixtures/runtime_failures.json"
    ))
    .unwrap();
    assert_eq!(corpus.schema, 1);
    corpus
}

/// Paths are logical and sorted; roots, mtimes, enumeration order and random IDs
/// must not change an otherwise identical baseline's identity.
#[derive(Debug, PartialEq, Serialize, Deserialize)]
struct Baseline {
    schema: u32,
    corpus: String,
    inputs: BTreeMap<String, String>,
    config: serde_json::Value,
}

fn digest(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

impl Baseline {
    fn fingerprint(&self) -> String {
        digest(&serde_json::to_vec(self).unwrap())
    }
}

struct Sandbox {
    root: tempfile::TempDir,
    core: Arc<AgentCore>,
    rt: Arc<SessionRuntime>,
    baseline: Baseline,
}

impl Sandbox {
    fn new() -> Self {
        let root = tempfile::tempdir().unwrap();
        for (path, content) in corpus().inputs {
            assert!(
                Path::new(&path)
                    .components()
                    .all(|part| matches!(part, std::path::Component::Normal(_)))
            );
            let path = root.path().join(path);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, content).unwrap();
        }
        let roots = crate::tools::pathutil::WriteRoots {
            workspace: root.path().join("workspace"),
            data_dir: root.path().join("data"),
            project_dir: None,
            extra: vec![],
        };
        let mut core = test_support::make_core(&roots);
        let inner = Arc::get_mut(&mut core).unwrap();
        inner.config_commits = crate::core::config_commit::ConfigCoordinator::new(
            Arc::new(ForbiddenCredentials),
            Arc::new(crate::core::config_commit::DiskConfigStore::new(
                roots.data_dir.clone(),
            )),
        );
        inner.skills = crate::skills::SkillIndex::with_home(Some(root.path().join("home")));
        // Never load the real config, provider keys, persisted trust or memories.
        let cfg = inner.cfg.get_mut().unwrap();
        cfg.providers[0].id = "fixture-provider".into();
        cfg.providers[0].models[0].id = "fixture-model".into();
        cfg.providers[0].models[0].model = "fixture-no-network".into();
        cfg.providers[0].base_url = "http://127.0.0.1:1".into();
        cfg.providers[0].keys.clear();
        cfg.providers[0].credential_ref = None;
        cfg.active_model_id = Some("fixture-model".into());
        let rt =
            core.get_or_create_session("evaluation", roots.workspace, None, vec![], None, vec![]);
        let baseline = Self::capture(root.path(), &core.cfg.read().unwrap());
        std::fs::write(
            root.path().join("baseline.json"),
            serde_json::to_vec_pretty(&baseline).unwrap(),
        )
        .unwrap();
        Self {
            root,
            core,
            rt,
            baseline,
        }
    }

    fn capture(root: &Path, config: &ConfigState) -> Baseline {
        fn scan(root: &Path, at: &Path, inputs: &mut BTreeMap<String, String>) {
            for entry in std::fs::read_dir(at).unwrap() {
                let path = entry.unwrap().path();
                let metadata = std::fs::symlink_metadata(&path).unwrap();
                assert!(
                    !metadata.file_type().is_symlink(),
                    "linked evaluation input"
                );
                if metadata.is_dir() {
                    scan(root, &path, inputs);
                } else {
                    assert!(metadata.is_file(), "non-regular evaluation input");
                    inputs.insert(
                        path.strip_prefix(root)
                            .unwrap()
                            .to_string_lossy()
                            .replace('\\', "/"),
                        digest(&std::fs::read(path).unwrap()),
                    );
                }
            }
        }
        let mut inputs = BTreeMap::new();
        for scope in ["workspace", "data", "home"] {
            scan(root, &root.join(scope), &mut inputs);
        }
        Baseline {
            schema: 1,
            corpus: digest(include_bytes!(
                "../../../tests/fixtures/runtime_failures.json"
            )),
            inputs,
            config: serde_json::to_value(config).unwrap(),
        }
    }

    fn validate_baseline(&self) -> Result<(), &'static str> {
        let saved: Baseline =
            serde_json::from_slice(&std::fs::read(self.root.path().join("baseline.json")).unwrap())
                .unwrap();
        let current = Self::capture(self.root.path(), &self.core.cfg.read().unwrap());
        if saved != self.baseline || current.fingerprint() != saved.fingerprint() {
            Err("evaluation inputs changed; create a new baseline")
        } else {
            Ok(())
        }
    }

    fn ctx(&self) -> ToolCtx {
        ToolCtx {
            core: self.core.clone(),
            rt: self.rt.clone(),
            batch_id: "fixture".into(),
            call_index: 0,
            call_key: "fixture:0".into(),
            provider_call_id: "fixture-call-0".into(),
            cancel: tokio_util::sync::CancellationToken::new(),
        }
    }

    async fn read(&self, path: PathBuf) -> crate::tools::ToolOutcome {
        crate::tools::read::ReadTool
            .run(&self.ctx(), json!({"files":[{"path":path}]}))
            .await
    }

    async fn observe(&self, scenario: Scenario) -> Observation {
        use Observation::*;
        use Scenario::*;
        match scenario {
            FalseCompletion => {
                let artifact = self.read(self.rt.workspace.join("result.txt")).await;
                assert_eq!(artifact.error.unwrap().code, "E_NOT_FOUND");
                let report = "<report>All done; result.txt was delivered.</report>";
                assert_eq!(
                    drive::text_turn_action(report, true, false, 0, false, false),
                    drive::TextTurnAction::Finish
                );
                // Text completion only requests a stop; the delivery gate evaluates evidence.
                use crate::core::delivery::DeliveryService;
                use crate::core::delivery::types::ContractDraft;
                let roots = DeliveryService::roots_from_runtime(&self.rt);
                let root_id = roots.keys().next().unwrap();
                let draft: ContractDraft = serde_json::from_value(json!({
                    "requirements":[{"id":"result","title":"result exists","required":true,"check_ids":[],"artifact_ids":["artifact"]}],
                    "artifacts":[{"id":"artifact","kind":"text","path":{"root_id":root_id,"relative_path":"result.txt"},"constraints":[{"kind":"exists"}],"required":true}],
                    "checks":[],"review_policy":{"required":true,"independent":true},"roots":[]
                })).unwrap();
                let owner = DeliveryService::identity_of(&self.rt);
                self.core
                    .delivery
                    .create_contract_with(&owner, &draft, &roots, "false-completion", |contract| {
                        let prepared = crate::core::workflow::service::prepare(
                            contract,
                            &roots,
                            &self.rt.data_dir,
                            "temporary",
                            None,
                            &Default::default(),
                        )
                        .map_err(|message| {
                            crate::core::delivery::DeliveryError::Save { message }
                        })?;
                        crate::core::workflow::service::persist(&self.rt.data_dir, &prepared)
                            .map_err(|message| crate::core::delivery::DeliveryError::Save {
                                message,
                            })
                    })
                    .unwrap();
                let (_, decision, _) = super::delivery_stop::gate_delivery_stop(
                    &self.core,
                    &self.rt,
                    &owner.root_run_id,
                    &drive::DriveParams::default(),
                    1,
                    super::delivery_stop::StopReason::Text,
                    false,
                );
                assert!(!matches!(
                    decision,
                    super::delivery_stop::CompletionDecision::Allow { .. }
                ));
                Blocked
            }
            MissingChildResult | BudgetPartial => {
                let (steps, text) = if matches!(scenario, BudgetPartial) {
                    (
                        10,
                        "<report>Partial work; result.txt remains missing.</report>",
                    )
                } else {
                    (1, "Still working, no report")
                };
                let (_, tagged) = drive::split_report(text);
                let ended = crate::tools::subagent::ended_of(steps, 10, tagged);
                assert!(!ended.is_delivered());
                if matches!(scenario, BudgetPartial) {
                    assert_eq!(ended.as_str(), "partial");
                    assert_eq!(
                        ended.wait_state(),
                        crate::core::wait_targets::TargetState::Succeeded
                    );
                } else {
                    assert_eq!(ended.as_str(), "no_report");
                }
                Blocked
            }
            MissingArtifact => {
                let output = self.read(self.rt.workspace.join("result.txt")).await;
                assert!(!output.ok);
                assert_eq!(output.error.unwrap().code, "E_NOT_FOUND");
                Blocked
            }
            WorkspaceDrift => {
                let path = self.rt.workspace.join("source.txt");
                let before = self.read(path.clone()).await;
                std::fs::write(&path, "changed outside the evaluated run\n").unwrap();
                let after = self.read(path).await;
                assert_ne!(
                    before.data["files"][0]["version"],
                    after.data["files"][0]["version"]
                );
                assert!(self.validate_baseline().is_err());
                Detected
            }
            RepeatedIdle => {
                let mut supervision = supervise::SupervisionState::default();
                let mut nudged = false;
                let mut stopped = false;
                // Fixed case budget leaves room for the separate segmented-read
                // nudge, which intentionally consumes a step before idle escalation.
                for _ in 0..32 {
                    let output = self.read(self.rt.workspace.join("source.txt")).await;
                    assert!(output.ok);
                    match supervision.feed_batch(&supervise::BatchDigest {
                        has_non_readonly: false,
                        read_paths: vec!["source.txt".into()],
                    }) {
                        supervise::Verdict::Nudge(_) => nudged = true,
                        supervise::Verdict::Escalate(_) => {
                            stopped = true;
                            break;
                        }
                        supervise::Verdict::None => {}
                    }
                }
                assert!(nudged && stopped);
                Blocked
            }
            UntrustedResource => {
                let request = test_support::request(&self.core, &self.rt).await;
                for marker in [
                    "UNTRUSTED_EVAL_INSTRUCTION",
                    "EVAL_LESSON",
                    "EVAL_MEMORY",
                    "EVAL_HOME_SKILL",
                ] {
                    assert!(!request.system_core.contains(marker));
                }
                let result = crate::tools::skill::SkillTool
                    .run(&self.ctx(), json!({"skill":"eval-home"}))
                    .await;
                assert_eq!(result.error.unwrap().code, "E_RESOURCE_UNTRUSTED");
                Blocked
            }
            OutsideRead => {
                let outside = self.root.path().join("outside.txt");
                std::fs::write(&outside, "outside fixture roots").unwrap();
                let result = self.read(outside).await;
                assert!(!result.ok);
                assert_eq!(result.error.unwrap().code, "E_PATH_OUTSIDE");
                Blocked
            }
            ProtectedStateWrite => {
                let result = crate::tools::create::CreateTool
                    .run(
                        &self.ctx(),
                        json!({
                            "path":self.rt.data_dir.join("config.json"), "content":"{}"
                        }),
                    )
                    .await;
                assert!(!result.ok);
                assert_eq!(result.error.unwrap().code, "E_INTERNAL_STATE");
                assert!(!self.rt.data_dir.join("config.json").exists());
                Blocked
            }
        }
    }
}

#[tokio::test]
async fn fixed_failure_corpus_exercises_production_paths_without_claiming_known_gaps_passed() {
    for case in corpus().cases {
        let sandbox = Sandbox::new();
        sandbox.validate_baseline().unwrap();
        let observed = sandbox.observe(case.id).await;
        eprintln!(
            "{}",
            json!({"case":case.id,"observation":observed,
            "baseline":sandbox.baseline.fingerprint(),
            "scope":"production component/tool coverage; not an end-to-end run or delivery acceptance"})
        );
        assert_eq!(observed, case.expected, "{:?}", case.id);
    }
}

#[test]
fn evaluation_baselines_are_repeatable_and_detect_each_frozen_input_domain() {
    let original = Sandbox::new();
    let independent = Sandbox::new();
    assert_eq!(
        original.baseline.fingerprint(),
        independent.baseline.fingerprint()
    );
    for domain in [
        "workspace/source.txt",
        "data/memories/preference.md",
        "workspace/.codewave/lessons.md",
        "home/.agents/skills/eval-home/SKILL.md",
    ] {
        let sandbox = Sandbox::new();
        std::fs::write(sandbox.root.path().join(domain), "drift").unwrap();
        assert!(
            sandbox.validate_baseline().is_err(),
            "undetected drift: {domain}"
        );
    }
    independent.core.cfg.write().unwrap().custom_prompt = Some("changed configuration".into());
    assert!(independent.validate_baseline().is_err());
    original.validate_baseline().unwrap();
}

#[tokio::test]
async fn evaluation_execution_and_skill_caches_do_not_pollute_another_user_scope() {
    let user = Sandbox::new();
    let evaluation = Sandbox::new();
    let home_skill = evaluation
        .root
        .path()
        .join("home/.agents/skills/eval-home/SKILL.md");
    std::fs::write(&home_skill, "---\nname: evaluation-only\n---\nfixture").unwrap();
    let list =
        evaluation
            .core
            .skills
            .list(&evaluation.rt.workspace, &evaluation.rt.data_dir, &[], None);
    assert!(list.iter().any(|skill| skill.name == "evaluation-only"));
    let scope = crate::core::instruction_resources::ResourceScope::runtime(&evaluation.rt);
    let fresh = crate::core::instruction_resources::collect(&evaluation.core, &scope);
    assert!(
        fresh
            .resources
            .iter()
            .any(|resource| resource.name == "evaluation-only")
    );
    let audit =
        evaluation
            .core
            .skills
            .audit(&evaluation.rt.workspace, &evaluation.rt.data_dir, &[], None);
    assert!(
        audit
            .entries
            .iter()
            .any(|entry| entry.name == "evaluation-only")
    );
    let write = crate::tools::create::CreateTool.run(&evaluation.ctx(), json!({
        "path":evaluation.rt.data_dir.join("memories/generated.md"), "content":"evaluation result"
    })).await;
    assert!(write.ok, "{write:?}");
    assert!(!user.rt.data_dir.join("memories/generated.md").exists());
    let user_list = user
        .core
        .skills
        .list(&user.rt.workspace, &user.rt.data_dir, &[], None);
    assert!(
        !user_list
            .iter()
            .any(|skill| skill.name == "evaluation-only")
    );
    user.validate_baseline().unwrap();
}
