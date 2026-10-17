//! #36-C0 夹具。
//!
//! 覆盖三层，每层都打**真实生产路径**（磁盘 IO、跨进程），不是复制逻辑重测：
//!
//! 1. 单进程双实例（共享介质只有文件系统，无共享内存）；
//! 2. **真双进程**（`current_exe` 重新拉起测试二进制，见 `双进程_` 前缀用例）；
//! 3. 崩溃恢复（向前滚动 / 孤儿临时文件 / 日志不可读）。

use super::*;
use crate::core::coordinator::{AcquireRequest, Coordinator, SharedTransaction, SharedWrite};
use crate::core::process_coordination::transaction::plan_writes;
use crate::core::process_coordination::workspace::{lexical_normalize, resource_key};

// ─────────────────────── 工具 ───────────────────────

fn coord_root() -> (tempfile::TempDir, PathBuf) {
    let d = tempfile::tempdir().expect("tempdir");
    let p = d.path().to_path_buf();
    (d, p)
}

fn req(session: &str, run: &str, resource: &str, attempt: u32) -> AcquireRequest {
    AcquireRequest {
        session_id: session.into(),
        root_run_id: run.into(),
        resource: resource.into(),
        attempt,
        expected_generation: None,
    }
}

/// 子进程角色：靠环境变量驱动，跑完把结果**写进文件**。
///
/// 为什么不用 stdout：子进程是重新拉起的测试二进制，`--exact` 过滤一旦没匹配上，
/// 它会跑 0 个用例并退出 0 —— 父进程只判退出码就会**假通过**（这正是"证据≠完成"）。
/// 落地一个结果文件既能证明子进程真的跑了，也能把代次带回来。
fn child_out_path() -> Option<PathBuf> {
    std::env::var("CW_COORD_OUT").ok().map(PathBuf::from)
}

fn child_role() -> Option<String> {
    std::env::var("CW_COORD_CHILD").ok()
}

// ─────────────────────── owner / 代次 ───────────────────────

#[test]
fn 空闲资源_取到代次一_且同持有者重复申请幂等() {
    let (_d, root) = coord_root();
    let store = LeaseStore::new(&root);
    let l1 = store
        .acquire(&req("s1", "r1", "/res/a", 0))
        .expect("首次申请");
    assert_eq!(l1.generation, 1);
    // 重试不该换掉自己的代次——换掉会让"释放校验 generation"在重试后失效。
    let l2 = store
        .acquire(&req("s1", "r1", "/res/a", 0))
        .expect("同持有者幂等");
    assert_eq!(l2.generation, l1.generation);
}

#[test]
fn 他人持有时报占用_且消息带占用者身份() {
    let (_d, root) = coord_root();
    let store = LeaseStore::new(&root);
    store.acquire(&req("s1", "r1", "/res/a", 0)).expect("首次");
    let e = store
        .acquire(&req("s2", "r2", "/res/a", 0))
        .expect_err("他人持有必须被拒");
    assert_eq!(e.code, "resource_held");
    assert!(e.message.contains("r1"), "{}", e.message);
    assert!(e.message.contains("pid"), "{}", e.message);
}

#[test]
fn 代次跨实例单调_不会因为并发读改写而重号() {
    // 回归：read → +1 → write 有窗口，两个进程能拿到**相同** generation，
    // "释放必须校验 generation"当场失效（后者的迟到释放会放掉前者）。
    let (_d, root) = coord_root();
    let a = LeaseStore::new(&root);
    let b = LeaseStore::new(&root);
    let gens: Vec<u64> = (0..32)
        .map(|i| {
            if i % 2 == 0 {
                a.next_generation().expect("代次")
            } else {
                b.next_generation().expect("代次")
            }
        })
        .collect();
    let mut sorted = gens.clone();
    sorted.sort_unstable();
    sorted.dedup();
    assert_eq!(sorted.len(), gens.len(), "代次出现重号：{gens:?}");
    assert_eq!(sorted[0], 1);
    assert_eq!(sorted[31], 32);
}

#[test]
fn 释放必须校验代次_迟到完成不得放掉后继持有者() {
    let (_d, root) = coord_root();
    let store = LeaseStore::new(&root);
    let first = store.acquire(&req("s1", "r1", "/res/a", 0)).expect("首次");
    store.release(&first).expect("释放");
    let second = store.acquire(&req("s2", "r2", "/res/a", 0)).expect("二次");
    assert!(second.generation > first.generation);
    // 迟到的第一次释放必须失败，否则第二次的写就没有保护了。
    let e = store.release(&first).expect_err("代次不符必须拒绝");
    assert_eq!(e.code, "generation_mismatch");
    // 后继仍持有。
    assert_eq!(
        store.read("/res/a").expect("仍在").lease.generation,
        second.generation
    );
}

#[test]
fn 崩溃后保守_busy_不按_pid_自动回收() {
    // PID 复用会把**别人的**租约误判成陈旧的并抢走，所以不许自动回收。
    let (_d, root) = coord_root();
    let store = LeaseStore::new(&root);
    store.acquire(&req("s1", "r1", "/res/a", 0)).expect("首次");
    // 假装持有者已死（随便改 pid），仍必须报占用。
    let path = root
        .join("leases")
        .join(format!("{}.json", key_digest("/res/a")));
    let mut rec: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    rec["pid"] = serde_json::json!(u32::MAX);
    std::fs::write(&path, serde_json::to_vec(&rec).unwrap()).unwrap();
    let e = store
        .acquire(&req("s2", "r2", "/res/a", 0))
        .expect_err("必须保守 busy");
    assert_eq!(e.code, "resource_held");
    // 只有 guardian 显式回收才放得开。
    store
        .force_release("/res/a", "夹具：确认持有者已退出")
        .expect("显式回收");
    store
        .acquire(&req("s2", "r2", "/res/a", 0))
        .expect("回收后可申请");
}

#[test]
fn 租约文件损坏时按占用处理_不能当空闲() {
    let (_d, root) = coord_root();
    let store = LeaseStore::new(&root);
    let path = root
        .join("leases")
        .join(format!("{}.json", key_digest("/res/a")));
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(&path, b"{ not json").unwrap();
    // 解析不出来 ≠ 没锁。放行等于把别人的资源当空闲抢走。
    let e = store
        .acquire(&req("s1", "r1", "/res/a", 0))
        .expect_err("损坏必须保守");
    assert!(
        e.code == "resource_held" || e.code == "lease_unreadable",
        "{}",
        e.code
    );
}

// ─────────────────────── 事务 ───────────────────────

#[test]
fn 事务要么整体生效要么整体不生效() {
    let (_d, root) = coord_root();
    let (_wd, wd) = coord_root();
    // 关键：与本进程的 LeaseStore 用**同一个**协调根。
    let coord = FileCoordinator::new(&root);
    let lease = coord.acquire(&req("s1", "r1", "/res/a", 0)).expect("租约");

    let a = wd.join("a.txt");
    let b = wd.join("b.txt");
    let r = coord
        .commit(&SharedTransaction {
            lease: lease.clone(),
            writes: vec![
                SharedWrite {
                    path: a.to_string_lossy().to_string(),
                    contents: "A".into(),
                },
                SharedWrite {
                    path: b.to_string_lossy().to_string(),
                    contents: "B".into(),
                },
            ],
        })
        .expect("提交");
    assert_eq!(r.applied, 2);
    assert_eq!(std::fs::read_to_string(&a).unwrap(), "A");
    assert_eq!(std::fs::read_to_string(&b).unwrap(), "B");
}

#[test]
fn 写集含相对路径时_一条都不写() {
    let (_d, root) = coord_root();
    let (_wd, wd) = coord_root();
    // 关键：与本进程的 LeaseStore 用**同一个**协调根。
    let coord = FileCoordinator::new(&root);
    let lease = coord.acquire(&req("s1", "r1", "/res/a", 0)).expect("租约");
    let good = wd.join("good.txt");
    let e = coord
        .commit(&SharedTransaction {
            lease: lease.clone(),
            writes: vec![
                SharedWrite {
                    path: good.to_string_lossy().to_string(),
                    contents: "X".into(),
                },
                SharedWrite {
                    path: "relative/bad.txt".into(),
                    contents: "Y".into(),
                },
            ],
        })
        .expect_err("必须整体拒绝");
    assert_eq!(e.code, "invalid_write");
    // 前一条合法写也不能落地：部分生效无法回滚。
    assert!(
        !good.exists(),
        "事务整体不生效却写了一半：{}",
        good.display()
    );
}

#[test]
fn 租约失效时提交整体失败() {
    let (_d, root) = coord_root();
    let (_wd, wd) = coord_root();
    // 关键：与本进程的 LeaseStore 用**同一个**协调根。
    let coord = FileCoordinator::new(&root);
    let lease = coord.acquire(&req("s1", "r1", "/res/a", 0)).expect("租约");
    coord.store().force_release("/res/a", "夹具").expect("强收");
    let target = wd.join("x.txt");
    let e = coord
        .commit(&SharedTransaction {
            lease: lease.clone(),
            writes: vec![SharedWrite {
                path: target.to_string_lossy().to_string(),
                contents: "X".into(),
            }],
        })
        .expect_err("失效租约不得提交");
    assert_eq!(e.code, "stale_lease");
    assert!(!target.exists());
}

#[test]
fn 崩在_rename_中途_恢复向前滚动把事务补完而不是留一半() {
    let (_d, root) = coord_root();
    let (_wd, wd) = coord_root();
    // 关键：与本进程的 LeaseStore 用**同一个**协调根。
    let coord = FileCoordinator::new(&root);
    let a = wd.join("a.txt");
    let b = wd.join("b.txt");
    let writes = vec![
        (a.to_string_lossy().to_string(), "A".to_string()),
        (b.to_string_lossy().to_string(), "B".to_string()),
    ];
    // 手动复刻"崩在第 4 步中间"：日志已落盘，只把第一条从临时位 rename 到位。
    let intents = plan_writes(&writes).expect("计划");
    let engine = coord.engine();
    engine
        .stage(
            "tx-crash",
            &intents,
            &writes.iter().map(|(_, c)| c.clone()).collect::<Vec<_>>(),
        )
        .expect("暂存");
    engine.record_intent("tx-crash", 7, &intents).expect("日志");
    std::fs::rename(PathBuf::from(&intents[0].temp), &a).expect("只 rename 第一条");
    assert!(a.exists() && !b.exists(), "此刻确实只有一半生效");

    // 恢复：向前滚动。
    let done = coord.recover_transactions().expect("恢复");
    assert_eq!(done.len(), 1);
    assert_eq!(done[0].already_applied, 1, "已生效的应被识别而不是重复写");
    assert_eq!(done[0].renamed, 1, "剩下的一条应被补完");
    assert_eq!(std::fs::read_to_string(&a).unwrap(), "A");
    assert_eq!(std::fs::read_to_string(&b).unwrap(), "B");
    // 日志已清：恢复只做一次，重复恢复不重复动作。
    assert!(coord.recover_transactions().expect("幂等").is_empty());
}

/// #36-C0：桌面启动和共享存储 走的是**同一个**启动恢复入口。
///
/// 本条钉住该入口收的是**数据根**（内部自己拼 `coord/`）而不是协调根——正是那次约定
/// 分歧让子进程在 `root` 取租约、协调器去 `root/coord` 找，结果每次提交都是 `stale_lease`。
/// 一旦有人把参数当成协调根传进来，这里会立刻失败（找不到日志，事务补不完）。
#[test]
fn 启动恢复入口收的是数据根_半途事务被补完() {
    let (_d, data_root) = coord_root();
    let (_wd, wd) = coord_root();
    let coord = FileCoordinator::new(&data_root);
    let a = wd.join("a.txt");
    let b = wd.join("b.txt");
    let writes = vec![
        (a.to_string_lossy().to_string(), "A".to_string()),
        (b.to_string_lossy().to_string(), "B".to_string()),
    ];
    let intents = plan_writes(&writes).expect("计划");
    coord
        .engine()
        .stage(
            "tx-startup",
            &intents,
            &writes.iter().map(|(_, c)| c.clone()).collect::<Vec<_>>(),
        )
        .expect("暂存");
    coord
        .engine()
        .record_intent("tx-startup", 3, &intents)
        .expect("日志");
    std::fs::rename(PathBuf::from(&intents[0].temp), &a).expect("只 rename 第一条");
    assert!(a.exists() && !b.exists(), "崩溃点确实停在半途");

    // 只传数据根：函数自己拼 coord/
    let rolled = super::recover_shared_transactions(&data_root).expect("启动恢复");
    assert_eq!(rolled.len(), 1, "数据根传对了才找得到日志");
    assert_eq!(
        std::fs::read_to_string(&b).unwrap(),
        "B",
        "剩下的一条被补完"
    );
    assert!(
        super::recover_shared_transactions(&data_root)
            .expect("幂等")
            .is_empty(),
        "重复恢复不重复动作"
    );
}

#[test]
fn 没有日志的孤儿临时文件被丢弃_不冒充已生效() {
    let (_d, root) = coord_root();
    let (_wd, wd) = coord_root();
    // 关键：与本进程的 LeaseStore 用**同一个**协调根。
    let coord = FileCoordinator::new(&root);
    let target = wd.join("never.txt");
    // 事务从未"被决定"：只有暂存，没有日志。
    let writes = vec![(target.to_string_lossy().to_string(), "N".to_string())];
    let intents = plan_writes(&writes).expect("计划");
    coord
        .engine()
        .stage(
            "tx-orphan",
            &intents,
            &writes.iter().map(|(_, c)| c.clone()).collect::<Vec<_>>(),
        )
        .expect("暂存");
    assert!(PathBuf::from(&intents[0].temp).exists());

    coord.recover_transactions().expect("恢复");
    assert!(!target.exists(), "未被决定的事务不得落地");
    assert!(
        !PathBuf::from(&intents[0].temp).exists(),
        "孤儿临时文件应被清理（临时文件与目标同目录，靠 pending 标记才能找回）"
    );
}

#[test]
fn 日志不可读时保守报错_不当作没有事务() {
    let (_d, root) = coord_root();
    // 关键：与本进程的 LeaseStore 用**同一个**协调根。
    let coord = FileCoordinator::new(&root);
    // `FileCoordinator::new(data_dir)` 的协调根是 `<data_dir>/coord`。
    let jd = root.join("coord").join("journal");
    std::fs::create_dir_all(&jd).unwrap();
    std::fs::write(jd.join("broken.json"), b"{ not json").unwrap();
    // 当成"没有事务"会把一次已决定的事务静默丢掉。
    let e = coord.recover_transactions().expect_err("必须报错");
    assert_eq!(e.code, "journal_unreadable");
}

// ─────────────────────── workspace / guardian ───────────────────────

#[test]
fn 租约键用_canonical_路径_同一目录的不同写法拿到同一把锁() {
    let (_d, root) = coord_root();
    let (_wd, wd) = coord_root();
    let sub = wd.join("sub");
    std::fs::create_dir_all(&sub).unwrap();
    let store = LeaseStore::new(&root);
    // `wd/sub` 与 `wd/./sub` 是同一个目录，必须互斥。
    store
        .acquire(&req("s1", "r1", &resource_key(&sub), 0))
        .expect("首次");
    let sneaky = wd.join(".").join("sub");
    assert_eq!(
        resource_key(&sneaky),
        resource_key(&sub),
        "同一目录应得同一键"
    );
    let e = store
        .acquire(&req("s2", "r2", &resource_key(&sneaky), 0))
        .expect_err("同一目录必须互斥");
    assert_eq!(e.code, "resource_held");
    // `sub/..` 是 `wd` 本身，是**另一个**目录：键不同、锁也不同。
    assert_ne!(
        resource_key(&sub.join("..")),
        resource_key(&sub),
        "`..` 归一化后指向父目录，不该与子目录同键"
    );
    // 词法规范化兜底：canonicalize 失败（路径不存在）时也不该退回"按原字符串"。
    let ghost = wd.join("no-such-dir");
    assert_eq!(
        resource_key(&ghost),
        lexical_normalize(&ghost).to_string_lossy(),
        "canonicalize 失败应退回词法规范化"
    );
}

#[test]
fn 守卫_drop_时释放根租约() {
    let (_d, root) = coord_root();
    let (_wd, wd) = coord_root();
    let store = LeaseStore::new(&root);
    {
        let g = WorkspaceGuard::acquire(&store, "s1", "r1", 0, &wd).expect("守卫");
        assert!(store.read(&g.lease().resource).is_some());
    }
    // 出了作用域就该放得开，否则一个已死的进程会永久占死目录。
    store
        .acquire(&req("s2", "r2", &resource_key(&wd), 0))
        .expect("应可申请");
}

#[test]
fn guardian_认领的根在_release_all_之前一直占用() {
    let (_d, root) = coord_root();
    let (_wd, wd) = coord_root();
    let store = LeaseStore::new(&root);
    let g = ProcessGuardian::new(&store);
    g.guard_root("s1", "r1", 0, &wd).expect("认领");
    assert_eq!(g.held().len(), 1);
    assert_eq!(g.busy_roots().len(), 1);
    store
        .acquire(&req("s2", "r2", &resource_key(&wd), 0))
        .expect_err("他人应被拒");
    assert!(g.release_all().is_empty(), "正常释放不应报错");
    store
        .acquire(&req("s2", "r2", &resource_key(&wd), 0))
        .expect("释放后可申请");
}

#[test]
fn 恢复只报告租约状态_不重放任何工具() {
    let (_d, root) = coord_root();
    // 关键：与本进程的 LeaseStore 用**同一个**协调根。
    let coord = FileCoordinator::new(&root);
    coord.acquire(&req("s1", "r1", "/res/a", 0)).expect("租约");
    coord.acquire(&req("s1", "r2", "/res/b", 0)).expect("租约");
    coord.acquire(&req("s2", "r9", "/res/c", 0)).expect("租约");
    let mine = coord.recover("s1").expect("恢复");
    assert_eq!(mine.len(), 2);
    // 按代次排序：恢复报告要有稳定顺序，不能靠目录遍历顺序。
    assert!(mine[0].generation < mine[1].generation);
}

// ─────────────────────── 真双进程 ───────────────────────

/// 子进程入口：被父进程用 `current_exe --exact` 拉起。
///
/// 子进程只跑这一个用例，靠环境变量决定角色，**不递归拉自己**。
#[test]
fn 双进程子进程_() {
    let Some(role) = child_role() else {
        return; // 父进程 / 正常测试运行：不做任何事。
    };
    let out = child_out_path().expect("子进程必须拿到 CW_COORD_OUT");
    let root = PathBuf::from(std::env::var("CW_COORD_ROOT").expect("CW_COORD_ROOT"));
    let store = LeaseStore::new(&root);
    let result = match role.as_str() {
        "acquire" => {
            let res = std::env::var("CW_COORD_RESOURCE").expect("CW_COORD_RESOURCE");
            match store.acquire(&req("s1", "r1", &res, 0)) {
                Ok(l) => format!("ok gen={}", l.generation),
                Err(e) => format!("err {e}"),
            }
        }
        "acquire_expect_conflict" => {
            let res = std::env::var("CW_COORD_RESOURCE").expect("CW_COORD_RESOURCE");
            match store.acquire(&req("s2", "r2", &res, 0)) {
                Ok(l) => format!("err 抢到了 generation {}", l.generation),
                Err(e) => format!("ok {e}"),
            }
        }
        "commit" => {
            let res = std::env::var("CW_COORD_RESOURCE").expect("CW_COORD_RESOURCE");
            let target = std::env::var("CW_COORD_TARGET").expect("CW_COORD_TARGET");
            match store.acquire(&req("s1", "r1", &res, 0)) {
                Err(e) => format!("err {e}"),
                Ok(l) => {
                    // 关键：与本进程的 LeaseStore 用**同一个**协调根。
                    let coord = FileCoordinator::from_coord_root(&root);
                    match coord.commit(&SharedTransaction {
                        lease: l,
                        writes: vec![SharedWrite {
                            path: target,
                            contents: "from-child".into(),
                        }],
                    }) {
                        Ok(_) => "ok".into(),
                        Err(e) => format!("err {e}"),
                    }
                }
            }
        }
        other => format!("err 未知子进程角色 {other}"),
    };
    std::fs::write(&out, result).expect("子进程写结果文件");
}

/// 拉起子进程并取回它写下的结果串。
///
/// **结果文件缺失 = 子进程一个用例都没跑**（`--exact` 没匹配上时 libtest 退出 0）。
/// 只判退出码就会在这里**假通过**——一个什么都没验证的夹具比没有夹具更糟。
fn spawn_child(role: &str, root: &Path, extra: &[(&str, &str)]) -> String {
    let (_d, outdir) = coord_root();
    let out = outdir.join("child.out");
    let mut cmd = std::process::Command::new(std::env::current_exe().expect("current_exe"));
    cmd.arg("--exact")
        .arg("core::process_coordination::tests::双进程子进程_")
        .arg("--nocapture")
        .env("CW_COORD_CHILD", role)
        .env("CW_COORD_ROOT", root)
        .env("CW_COORD_OUT", &out);
    for (k, v) in extra {
        cmd.env(k, v);
    }
    let status = cmd.status().expect("拉起子进程");
    assert!(status.success(), "子进程异常退出：{status:?}");
    std::fs::read_to_string(&out).unwrap_or_else(|e| {
        panic!("子进程没有产出结果文件（很可能 `--exact` 没匹配上，用例根本没跑）：{e}")
    })
}

#[test]
fn 双进程_同一资源_只有先到者拿到租约() {
    let (_d, root) = coord_root();
    let store = LeaseStore::new(&root);
    store
        .acquire(&req("parent", "parent-run", "/res/x", 0))
        .expect("父进程先拿");

    let out = spawn_child(
        "acquire_expect_conflict",
        &root,
        &[("CW_COORD_RESOURCE", "/res/x")],
    );
    assert!(
        out.starts_with("ok "),
        "子进程抢到了父进程正持有的资源：跨进程互斥失效。{out}"
    );
    assert!(out.contains("resource_held"), "{out}");
}

#[test]
fn 双进程_不同资源_各自拿到互不相干的代次() {
    let (_d, root) = coord_root();
    let store = LeaseStore::new(&root);
    let mine = store
        .acquire(&req("parent", "parent-run", "/res/parent", 0))
        .expect("父");

    let out = spawn_child("acquire", &root, &[("CW_COORD_RESOURCE", "/res/child")]);
    let child_gen: u64 = out
        .strip_prefix("ok gen=")
        .and_then(|s| s.trim().parse().ok())
        .unwrap_or_else(|| panic!("子进程未成功申请：{out}"));
    // 跨进程共享同一个代次计数器：绝不能重号。
    assert_ne!(child_gen, mine.generation);
    assert!(child_gen > mine.generation, "子进程应拿到更大的代次");
}

#[test]
fn 双进程_事务提交在磁盘上可见_且子进程退出后仍保守_busy() {
    let (_d, root) = coord_root();
    let (_wd, wd) = coord_root();
    let target = wd.join("child.txt");
    let store = LeaseStore::new(&root);
    store
        .acquire(&req("parent", "parent-run", "/res/p", 0))
        .expect("父租约");

    let out = spawn_child(
        "commit",
        &root,
        &[
            ("CW_COORD_RESOURCE", "/res/c"),
            ("CW_COORD_TARGET", target.to_str().unwrap()),
        ],
    );
    assert!(out.starts_with("ok"), "子进程提交失败：{out}");
    assert_eq!(std::fs::read_to_string(&target).unwrap(), "from-child");
    // 子进程退出**不**自动释放：按"崩溃保守 busy"，PID 复用会让自动回收误杀别人。
    let busy = store.acquire(&req("parent", "parent-run", "/res/c", 0));
    assert_eq!(
        busy.err().map(|e| e.code),
        Some("resource_held".to_string()),
        "子进程退出后应保守 busy，而不是自动回收"
    );
}

#[test]
fn 协调根约定_两种入口不混用() {
    // 回归：子进程曾用 `LeaseStore::new(root)` 取租约、`FileCoordinator::new(root)`
    // 去提交（后者内部指到 `root/coord`），结果**每一次提交都是 `stale_lease`**——
    // 看起来像"事务机制坏了"，其实是参数差了一层目录。
    let (_d, data) = coord_root();
    let by_data = FileCoordinator::new(&data);
    assert_eq!(
        by_data.store().coord_root(),
        data.join("coord"),
        "new(data_dir) 的协调根必须是 <data_dir>/coord"
    );
    let root = data.join("coord");
    let by_root = FileCoordinator::from_coord_root(&root);
    assert_eq!(
        by_root.store().coord_root(),
        root,
        "from_coord_root(r) 的协调根必须就是 r"
    );
    // 两个入口指向同一个协调根时，租约互相可见。
    let l = by_data
        .store()
        .acquire(&req("s", "r", "/res/x", 0))
        .expect("申请");
    assert!(
        by_root
            .store()
            .read(&l.resource)
            .map(|rec| rec.lease.generation == l.generation)
            .unwrap_or(false),
        "指向同一协调根的两个入口必须看到同一把租约"
    );
}

#[test]
fn physical_root_lock_cannot_be_bypassed_by_another_profile() {
    let root = tempfile::tempdir().unwrap();
    let first = WorkspaceGuard::acquire_global("session-a", "run-a", 0, root.path()).unwrap();
    let second = WorkspaceGuard::acquire_global("session-b", "run-b", 0, root.path());
    assert_eq!(second.err().unwrap().code, "resource_held");
    first.release().unwrap();
    WorkspaceGuard::acquire_global("session-b", "run-b", 0, root.path())
        .unwrap()
        .release()
        .unwrap();
}
#[test]
fn temporary_names_are_process_and_nonce_unique() {
    let path = std::path::Path::new("/tmp/same-target");
    let one = super::tmp_sibling(path);
    let two = super::tmp_sibling(path);
    assert_ne!(one, two);
    assert!(
        one.file_name()
            .unwrap()
            .to_string_lossy()
            .contains(&std::process::id().to_string())
    );
}
