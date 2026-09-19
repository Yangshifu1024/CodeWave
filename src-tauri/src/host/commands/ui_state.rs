//! ui-state（全局 UI 现场态）命令 + 退出拦截（会话保存与恢复优化 · 批1，需求共识 17/22）。
//! 命令只做参数校验 + 转调 core / 状态机；文件布局与校验规则在 `core::ui_state`，
//! 运行标记与中断标记在 `core::sessions::interrupt`。

use super::util::{Core, err};
use crate::core::agent::AgentCore;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use tauri::{Emitter, Manager};

/// 无 run 在跑时的退出看门狗（秒）：前端未响应（挂死/未挂载）也必须能退出。
const EXIT_WATCHDOG_SECS: u64 = 2;
/// 有 run 在跑时的兜底看门狗（秒）：前端不可用（dev 编译失败导致 JS 没启动、事件句柄尚未绑定、
/// 应答抛错后用户不再交互）时同样必须能退出——没有兜底就只能强杀进程，而强杀会让下次启动按
/// 崩溃（crash）而不是「用户主动退出」（quit）恢复。
///
/// 取值依据：**必须大于** `EXIT_ABORT_DRAIN_SECS`（30s），否则用户选「中断并保存后退出」时
/// 后端收尾窗口会被兜底抢占；又远小于 `EXIT_WAIT_TIMEOUT_SECS`（600s）与「等完成」的常规
/// 收尾时长，且用户一旦应答（含选「等完成」）该看门狗立即作废（见 `spawn_exit_watchdog`），
/// 因此真正等待 run 收尾的正常路径不会被它踢出。
const EXIT_RUN_WATCHDOG_SECS: u64 = 45;
/// 「中断并保存后退出」等待 run 收尾的上限（秒）：超时后照常落中断标记并退出，绝不无限阻塞。
const EXIT_ABORT_DRAIN_SECS: u64 = 30;
/// 「等完成再退出」的后台等待上限（秒）：超时撤回等待（不静默打断），用户可再次退出改选。
const EXIT_WAIT_TIMEOUT_SECS: u64 = 600;

// ---------- ui-state ----------

/// 读取全局 UI 现场态。文件缺失/损坏 → null（损坏文件已由 core 备份留证，绝不静默删除）。
#[tauri::command]
pub async fn get_ui_state(core: Core<'_>) -> Result<Option<serde_json::Value>, String> {
    Ok(crate::core::ui_state::load(&core.data_dir))
}

/// 写入全局 UI 现场态。校验 schema 与体积上限后原子落盘；磁盘错误向上抛（E9 不静默）。
#[tauri::command]
pub async fn set_ui_state(core: Core<'_>, state: serde_json::Value) -> Result<(), String> {
    crate::core::ui_state::save(&core.data_dir, &state).map_err(err)
}

// ---------- 运行中会话 ----------

/// 当前进程内在跑的会话 id（按 id 排序，输出稳定）。
/// 子代理与计划任务运行不在会话表内（其运行期由父会话的 run 覆盖）。
pub fn running_session_ids(core: &AgentCore) -> Vec<String> {
    let mut ids: Vec<String> = core
        .sessions
        .iter()
        .filter(|e| e.value().running.load(Ordering::SeqCst))
        .map(|e| e.key().clone())
        .collect();
    ids.sort();
    ids
}

/// 退出拦截提示用：当前进程内在跑的会话 id 列表。
/// 同步命令：契约返回裸 `Vec<String>`（无错误分支），保持与前端约定的形状。
#[tauri::command]
pub fn list_running_sessions(core: Core<'_>) -> Vec<String> {
    running_session_ids(&core)
}

// ---------- 退出拦截状态机 ----------

/// 退出请求状态机（`RunEvent::ExitRequested` ↔ 前端 `resolve_exit_request` 应答）。
///
/// 三个原子量覆盖全部状态，避免跨线程 Mutex：
/// - `confirmed`：已确认退出——`app.exit(0)` 会再次触发 ExitRequested，靠它放行（重入保护）；
/// - `awaiting`：正在等前端决定——单飞，重复的 ExitRequested 不得重复弹窗；
/// - `epoch`：代数——cancel/confirm 递增，令在途看门狗与后台等待任务立即失效（取消后不得再退出）。
#[derive(Default)]
pub struct ExitGuard {
    /// 已确认退出（放行 ExitRequested）
    confirmed: AtomicBool,
    /// 等待前端决定中
    awaiting: AtomicBool,
    /// 状态代号（cancel/confirm 递增；在途任务凭代号判断自己是否已过期）
    epoch: AtomicU64,
}

impl ExitGuard {
    /// 是否已确认退出（ExitRequested 处理器据此放行）。
    pub fn is_confirmed(&self) -> bool {
        self.confirmed.load(Ordering::SeqCst)
    }

    /// 是否在等前端决定。
    pub fn is_awaiting(&self) -> bool {
        self.awaiting.load(Ordering::SeqCst)
    }

    /// 当前状态代号。
    pub fn epoch(&self) -> u64 {
        self.epoch.load(Ordering::SeqCst)
    }

    /// 看门狗到期时的放行抢占：仅在「仍在等前端决定 + 代号未变」时置确认，返回是否抢到。
    ///
    /// 双条件判据是兜底路径不误伤正常路径的根据：前端任何应答（`exit` / `abort` / `cancel` /
    /// `wait`）都会清 `awaiting` 并递增 `epoch`，于是迟到的超时抢不到放行权。
    pub fn confirm_if_still_awaiting(&self, epoch: u64) -> bool {
        if !self.is_awaiting() || self.epoch() != epoch {
            return false;
        }
        self.confirm();
        true
    }

    /// 开始等待前端决定；已有退出请求在途返回 false（单飞：不重复弹窗）。
    pub fn begin_await(&self) -> bool {
        self.awaiting
            .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
            .is_ok()
    }

    /// 放行退出：置确认标志 + 结束等待 + 递增代号（作废在途看门狗/后台等待）。
    pub fn confirm(&self) {
        self.confirmed.store(true, Ordering::SeqCst);
        self.awaiting.store(false, Ordering::SeqCst);
        self.epoch.fetch_add(1, Ordering::SeqCst);
    }

    /// 撤销退出请求（用户取消 / 等待超时）：结束等待 + 递增代号（看门狗失效），应用继续运行。
    pub fn cancel(&self) {
        self.awaiting.store(false, Ordering::SeqCst);
        self.epoch.fetch_add(1, Ordering::SeqCst);
    }
}

/// 一次 `ExitRequested` 的处置决策（带副作用：`ForceExit` / `Prompt` 会占用或更新状态机）。
///
/// 抽成独立函数只为可测：`handle_exit_requested` 需要真实的 `AppHandle`（单测造不出来），
/// 而「放行 / 二次触发 / 下发询问（含看门狗时长）」三条分支都值得被用例钉住。
/// 副作用动作仍留在处理器里执行，这里只做状态机判定。
#[derive(Debug, PartialEq, Eq)]
enum ExitDisposition {
    /// 已确认退出：直接放行（`app.exit(0)` 引发的再次 ExitRequested 走这条，重入保护）。
    Allow,
    /// 已有退出请求在途且未被应答（用户又触发了一次退出）：立即放行，不再等前端。
    ForceExit,
    /// 新请求：拦截退出 + 下发询问，并按 `watchdog` 挂兜底看门狗。
    Prompt {
        epoch: u64,
        watchdog: std::time::Duration,
    },
}

/// 判定本次退出请求的处置（`running_count` = 在跑会话数，只影响看门狗时长）。
fn dispose_exit_request(guard: &ExitGuard, running_count: usize) -> ExitDisposition {
    if guard.is_confirmed() {
        return ExitDisposition::Allow;
    }
    if !guard.begin_await() {
        // 单飞语义不变（begin_await 仍拒重复请求），只是处置从「忽略」改为「立即放行」：
        // 走到这里一定是「确有请求在途且尚未被应答」，即用户又点了一次退出。
        return ExitDisposition::ForceExit;
    }
    let epoch = guard.epoch();
    let secs = if running_count > 0 {
        EXIT_RUN_WATCHDOG_SECS
    } else {
        EXIT_WATCHDOG_SECS
    };
    ExitDisposition::Prompt {
        epoch,
        watchdog: std::time::Duration::from_secs(secs),
    }
}

/// `RunEvent::ExitRequested` 处理器（lib.rs 事件循环回调转调）。
///
/// 注意：窗口关闭是「隐藏到托盘」（ui.close_to_tray），真正的退出来自托盘「退出」与 macOS Cmd+Q，
/// 都经本事件；程序化 `app.exit(0)` 也会再次触发，靠 `confirmed` 放行（重入保护）。
/// 流程：已确认 → 放行；已有请求在途（二次触发）→ 立即放行；否则 prevent_exit + 下发
/// `app:exit_requested`，并按在跑会话数挂兜底看门狗（有 run 45s / 无 run 2s）。
/// **任何分支都不再无限等待前端**——前端不可用时用户依然退得出去。
pub fn handle_exit_requested(app: &tauri::AppHandle, api: &tauri::ExitRequestApi) {
    let Some(guard) = app.try_state::<ExitGuard>() else {
        // 状态未就绪（理论上不会发生）：放行，绝不把用户锁在应用里
        return;
    };
    let running = running_session_ids_of(app);
    match dispose_exit_request(&guard, running.len()) {
        ExitDisposition::Allow => {}
        ExitDisposition::ForceExit => {
            api.prevent_exit();
            // 二次触发：上一次请求仍在途（前端尚未应答），用户又点了一次退出 ⇒ 视为明确要退。
            // 前端不可用时这是用户唯一的自救手段；前端只是慢一拍时也以「用户按了两次」为准。
            tracing::info!("退出请求二次触发（上一次请求未被应答），立即退出");
            // 先确认再放行：confirm 递增 epoch 作废可能在途的兜底看门狗，同时让本次 app.exit(0)
            // 引发的重入 ExitRequested 直接放行；实际收尾放到 runtime 上做，不在事件循环回调里
            // 做阻塞落盘（与 resolve_exit_request 各分支同法）。
            guard.confirm();
            let handle = app.clone();
            tauri::async_runtime::spawn(async move {
                force_exit(&handle, &running, "二次触发（前端未应答）");
            });
        }
        ExitDisposition::Prompt { epoch, watchdog } => {
            api.prevent_exit();
            let count = running.len();
            let payload = serde_json::json!({ "running": running.clone() });
            if let Err(e) = app.emit_to(
                tauri::EventTarget::labeled("main"),
                "app:exit_requested",
                payload,
            ) {
                tracing::warn!("退出请求事件下发失败（前端可能不可用）：{e}");
            }
            tracing::info!(
                "退出请求：{count} 个会话在运行，等待前端决定（{} 秒无应答则兜底退出）",
                watchdog.as_secs()
            );
            spawn_exit_watchdog(app.clone(), epoch, watchdog, running);
        }
    }
}

/// 收尾放行：删除进程运行标记（下次启动不再按崩溃处理）。
pub fn finish_normal_exit(app: &tauri::AppHandle) {
    let dir = app
        .try_state::<std::sync::Arc<AgentCore>>()
        .map(|c| c.data_dir.clone())
        .unwrap_or_else(crate::core::config::data_dir);
    crate::core::sessions::interrupt::remove_marker(&dir);
}

/// 挂兜底看门狗：`timeout` 到期仍未收到前端应答 → 强制退出（在跑会话按 quit 中断收尾）。
///
/// 为什么需要这条兜底：退出拦截把「退不退」的决定权交给了前端（弹窗列出运行中会话，让用户在
/// 「等完成 / 中断并保存 / 立即退出 / 取消」里选）。前端一旦不可用——dev 编译失败导致 JS 根本
/// 没启动、事件句柄尚未绑定（启动窗口期）、应答抛错后用户不再交互——这个询问就永远没有答案，
/// 缺兜底即「永久退不出去，只能强杀」，而强杀会让下次启动按 crash 恢复。
///
/// 为什么不会误伤正常路径：放行判据是 `ExitGuard::confirm_if_still_awaiting(epoch)`——前端任何
/// 应答都会清 `awaiting` 并递增 `epoch`（cancel 与 confirm 都如此），在途看门狗随即作废；
/// 因此「用户选等完成」以及「前端只是慢一拍（几秒后才弹窗/应答）」都不会被超时踢出。
fn spawn_exit_watchdog(
    app: tauri::AppHandle,
    epoch: u64,
    timeout: std::time::Duration,
    running: Vec<String>,
) {
    tauri::async_runtime::spawn(async move {
        let fired = {
            // 作用域内取 State 并立即释放（不跨 await 持 State 借用，与 wait 分支同法）
            let Some(g) = app.try_state::<ExitGuard>() else {
                return;
            };
            exit_watchdog(&g, epoch, tokio::time::sleep(timeout)).await
        };
        if !fired {
            return; // 前端已应答（或已取消）：交由那次决定
        }
        tracing::info!("退出兜底看门狗超时（前端未响应），强制退出");
        force_exit(&app, &running, "看门狗超时（前端未响应）");
    });
}

/// 兜底看门狗主体：等 `delay` 到期，若该次退出请求仍在途且未被应答 → 抢下放行权（返回 true）。
///
/// `delay` 由调用方注入：生产路径给 `tokio::time::sleep(timeout)`，测试给 `std::future::ready(())`
/// 或虚拟时钟（`start_paused`）——「超时确实放行」与「迟到的超时不生效」两条路径都能在毫秒级
/// 验证，无需真实等待数十秒。
async fn exit_watchdog<D>(guard: &ExitGuard, epoch: u64, delay: D) -> bool
where
    D: std::future::Future<Output = ()>,
{
    delay.await;
    guard.confirm_if_still_awaiting(epoch)
}

/// 强制放行退出（兜底路径共用）：在跑的会话落 `quit` 中断标记 → 删进程运行标记 → `app.exit(0)`。
///
/// 为什么必须落中断标记：`finish_normal_exit` 会删掉 `running.marker`，下次启动的
/// `recover_after_crash` 便不会再扫索引——若这里不落标记，索引会永久残留 `running: true`
/// （界面上表现为永远「运行中」）。语义与 `resolve_exit_request` 的 `exit` / `abort` 动作一致：
/// 用户主动退出 ⇒ 在跑 run 记为 quit 中断（run 内容耐久性由既有检查点机制兜底）。
///
/// 调用方必须**先** `ExitGuard::confirm()`：一来让 `app.exit(0)` 引发的重入 ExitRequested 放行，
/// 二来递增 `epoch` 作废仍在途的看门狗——所以「二次触发」放行后，先前那次请求的看门狗不会稍后
/// 再踢一次，也不会干扰下一次退出流程。
fn force_exit(app: &tauri::AppHandle, running: &[String], reason: &str) {
    if !running.is_empty() {
        match app.try_state::<std::sync::Arc<AgentCore>>() {
            Some(core) => {
                let n =
                    crate::core::sessions::interrupt::mark_quit_interrupted(&core.store, running);
                tracing::info!("退出（{reason}）：{n} 个在跑会话标记为中断（quit）");
            }
            None => tracing::warn!("退出（{reason}）：核心状态未就绪，中断标记未落盘"),
        }
    }
    finish_normal_exit(app);
    app.exit(0);
}

/// 前端对退出请求的应答。
/// - `exit`：立即退出（不为在跑的 run 做收尾，只留中断痕迹）
/// - `cancel`：取消退出，应用继续运行（含取消看门狗）
/// - `abort`：取消所有在跑的 run，等收尾检查点 → 落 quit 中断标记 → 退出
/// - `wait`：后台等待在跑的 run 全部收尾后再退出（不打断）
#[tauri::command]
pub async fn resolve_exit_request(
    app: tauri::AppHandle,
    core: Core<'_>,
    guard: tauri::State<'_, ExitGuard>,
    action: String,
) -> Result<(), String> {
    match action.as_str() {
        "exit" => {
            let ids = running_session_ids(&core);
            if !ids.is_empty() {
                let n = crate::core::sessions::interrupt::mark_quit_interrupted(&core.store, &ids);
                tracing::info!("退出请求（exit）：{n} 个在跑会话标记为中断（quit）");
            }
            guard.confirm();
            finish_normal_exit(&app);
            app.exit(0);
            Ok(())
        }
        "cancel" => {
            guard.cancel();
            tracing::info!("退出请求已取消，应用继续运行");
            Ok(())
        }
        "abort" => {
            // 先记下在跑的会话（它们都将被这次退出中断），再取消其 run
            let ids = running_session_ids(&core);
            for e in core.sessions.iter() {
                e.value().cancel_active();
            }
            for e in core.subs.iter() {
                e.value().cancel_active();
            }
            let drained = wait_runs_finished(&core, EXIT_ABORT_DRAIN_SECS).await;
            if !drained {
                tracing::warn!("中断收尾等待超时（仍有 run 未停），按中断落盘并退出");
            }
            if !ids.is_empty() {
                let n = crate::core::sessions::interrupt::mark_quit_interrupted(&core.store, &ids);
                tracing::info!("退出请求（abort）：{n} 个会话标记为中断（quit）");
            }
            guard.confirm();
            finish_normal_exit(&app);
            app.exit(0);
            Ok(())
        }
        "wait" => {
            // 结束本次等待（用户再次触发退出时可重新弹窗，以便改选 abort/exit），
            // 由后台任务轮询到全部收尾后再退出。
            guard.cancel();
            let epoch = guard.epoch();
            let handle = app.clone();
            tauri::async_runtime::spawn(async move {
                // 作用域内取 State 并 clone 出 Arc：不跨 await 持 State 借用
                let core: std::sync::Arc<AgentCore> = {
                    let Some(core_state) = handle.try_state::<std::sync::Arc<AgentCore>>() else {
                        return;
                    };
                    std::sync::Arc::clone(&core_state)
                };
                let drained = wait_runs_finished(&core, EXIT_WAIT_TIMEOUT_SECS).await;
                let Some(g) = handle.try_state::<ExitGuard>() else {
                    return;
                };
                // 期间有新的退出交互（exit/abort/cancel 都会递增代号）→ 交由那次决定
                if g.epoch() != epoch {
                    tracing::info!("退出等待期间收到新的退出交互，放弃本次等待");
                    return;
                }
                if !drained {
                    // 超时仍有 run 在跑：撤回等待（绝不静默打断），用户可再次退出改选
                    g.cancel();
                    tracing::warn!("等待 run 收尾超时，退出请求已撤回");
                    return;
                }
                tracing::info!("在跑的 run 已全部收尾，退出");
                g.confirm();
                finish_normal_exit(&handle);
                handle.exit(0);
            });
            Ok(())
        }
        other => Err(format!("未知的退出动作：{other}")),
    }
}

/// 轮询等待在跑的 run 全部收尾（内存 running 集合清空），返回是否在期限内收尾。
async fn wait_runs_finished(core: &AgentCore, timeout_secs: u64) -> bool {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(timeout_secs);
    loop {
        if running_session_ids(core).is_empty() {
            return true;
        }
        if std::time::Instant::now() >= deadline {
            return false;
        }
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
}

/// 取全局 core 状态里的在跑会话 id（状态未就绪时返回空）。
fn running_session_ids_of(app: &tauri::AppHandle) -> Vec<String> {
    app.try_state::<std::sync::Arc<AgentCore>>()
        .map(|core| running_session_ids(&core))
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 单飞：在途请求期间重复 ExitRequested 不得再起一轮等待（防重复弹窗）。
    #[test]
    fn begin_await_is_single_flight() {
        let g = ExitGuard::default();
        assert!(g.begin_await());
        assert!(g.is_awaiting());
        assert!(!g.begin_await(), "已有请求在途必须拒绝");
        // 取消后可再次请求
        g.cancel();
        assert!(!g.is_awaiting());
        assert!(g.begin_await());
    }

    /// 取消：结束等待并递增代号（在途看门狗据此失效，不会在 2 秒后偷偷退出）。
    #[test]
    fn cancel_invalidates_pending_watchdog() {
        let g = ExitGuard::default();
        assert!(g.begin_await());
        let epoch = g.epoch();
        g.cancel();
        assert!(!g.is_awaiting());
        assert_ne!(g.epoch(), epoch, "cancel 必须递增代号");
        // 看门狗判据：awaiting 已清 → 不退出
        assert!(!(g.is_awaiting() && g.epoch() == epoch));
    }

    /// 确认：置 confirmed 且递增代号；重入放行判据成立（app.exit(0) 再触发 ExitRequested 时放行）。
    #[test]
    fn confirm_marks_exit_and_invalidates_pending_tasks() {
        let g = ExitGuard::default();
        assert!(g.begin_await());
        let epoch = g.epoch();
        g.confirm();
        assert!(g.is_confirmed());
        assert!(!g.is_awaiting());
        assert_ne!(g.epoch(), epoch);
        // 再次 confirm 幂等（退出代码路径可能重复调用）
        g.confirm();
        assert!(g.is_confirmed());
    }

    /// 未发请求时 confirm/cancel 幂等，不 panic（命令可在任意时序被调用）。
    #[test]
    fn cancel_and_confirm_are_idempotent_without_request() {
        let g = ExitGuard::default();
        g.cancel();
        g.cancel();
        assert!(!g.is_awaiting() && !g.is_confirmed());
        g.confirm();
        assert!(g.is_confirmed());
    }

    /// list_running_sessions 只认内存标志：未标记的会话不出现在结果里，输出按 id 排序稳定。
    #[test]
    fn running_session_ids_reads_in_memory_flags_and_sorts() {
        let d = tempfile::tempdir().unwrap();
        let roots =
            crate::tools::pathutil::WriteRoots::new(d.path().to_path_buf(), d.path().to_path_buf());
        let core = crate::core::agent::test_support::make_core(&roots);
        let ws = d.path().to_path_buf();
        let _idle = core.get_or_create_session("sess-b", ws.clone(), None, vec![], None, vec![]);
        let busy = core.get_or_create_session("sess-a", ws, None, vec![], None, vec![]);
        assert!(running_session_ids(&core).is_empty());
        busy.running.store(true, Ordering::SeqCst);
        assert_eq!(running_session_ids(&core), vec!["sess-a".to_string()]);
    }

    // ---------- 退出兜底（有 run 在跑时也必须退得出去） ----------
    //
    // 看门狗时长用虚拟时钟推进（`start_paused`）/ 注入零延迟，「超时」路径不再真等 45 秒。

    /// 有 run 在跑且前端始终不应答：兜底看门狗到期后必须放行，不再无限等。
    #[tokio::test(start_paused = true)]
    async fn run_watchdog_releases_when_frontend_never_answers() {
        let g = ExitGuard::default();
        let ExitDisposition::Prompt { epoch, watchdog } = dispose_exit_request(&g, 1) else {
            panic!("有 run 在跑时首次请求必须下发询问");
        };
        assert_eq!(
            watchdog,
            std::time::Duration::from_secs(EXIT_RUN_WATCHDOG_SECS),
            "有 run 在跑走长看门狗"
        );
        assert!(g.is_awaiting(), "询问下发后处于等待前端决定态");
        assert!(
            exit_watchdog(&g, epoch, tokio::time::sleep(watchdog)).await,
            "前端不应答时兜底看门狗必须放行（修复前这里永久挂住）"
        );
        assert!(g.is_confirmed() && !g.is_awaiting());
        // 放行后 app.exit(0) 引发的重入 ExitRequested：直接放行，不再弹窗、不再挂起
        assert_eq!(dispose_exit_request(&g, 1), ExitDisposition::Allow);
    }

    /// 无 run 在跑仍走原来的 2 秒看门狗（原行为不回退）。
    #[tokio::test(start_paused = true)]
    async fn idle_watchdog_still_releases_without_running() {
        let g = ExitGuard::default();
        let ExitDisposition::Prompt { epoch, watchdog } = dispose_exit_request(&g, 0) else {
            panic!("首次请求必须下发询问");
        };
        assert_eq!(watchdog, std::time::Duration::from_secs(EXIT_WATCHDOG_SECS));
        assert!(exit_watchdog(&g, epoch, tokio::time::sleep(watchdog)).await);
        assert!(g.is_confirmed());
    }

    /// 不得误伤正常路径：用户选「等完成」（wait 分支先 cancel）后，迟到的兜底超时不得踢出应用。
    #[tokio::test(start_paused = true)]
    async fn late_run_watchdog_does_not_fire_after_wait_answer() {
        let g = std::sync::Arc::new(ExitGuard::default());
        let ExitDisposition::Prompt { epoch, watchdog } = dispose_exit_request(&g, 1) else {
            panic!("有 run 在跑时首次请求必须下发询问");
        };
        let g2 = g.clone();
        let delay = tokio::time::sleep(watchdog);
        let timer = tokio::spawn(async move { exit_watchdog(&g2, epoch, delay).await });
        // 前端应答 wait：resolve_exit_request 的 wait 分支先 cancel（决定权交给后台等待任务）
        g.cancel();
        assert!(!timer.await.unwrap(), "取消后迟到的兜底超时不得放行");
        assert!(!g.is_confirmed(), "用户选等完成，应用必须继续运行");
    }

    /// 前端已应答 exit / abort（confirm）后，在途看门狗同样不得再放行一次（不重复动作）。
    #[tokio::test]
    async fn watchdog_no_op_after_frontend_confirmed_exit() {
        let g = ExitGuard::default();
        let ExitDisposition::Prompt { epoch, .. } = dispose_exit_request(&g, 1) else {
            panic!("有 run 在跑时首次请求必须下发询问");
        };
        g.confirm(); // resolve_exit_request 的 exit / abort 分支
        assert!(!exit_watchdog(&g, epoch, std::future::ready(())).await);
        assert!(g.is_confirmed());
    }

    /// 二次触发：请求在途未被应答时用户再次点退出 → 立即放行；随后前端迟到的应答不弄乱状态。
    #[test]
    fn second_trigger_forces_exit_and_late_answers_are_harmless() {
        let g = ExitGuard::default();
        let ExitDisposition::Prompt { epoch, .. } = dispose_exit_request(&g, 1) else {
            panic!("首次请求必须下发询问");
        };
        // 第二次 ExitRequested（托盘「退出」再点一次 / Cmd+Q 再按一次）：单飞拒重复，但处置改为放行
        assert_eq!(dispose_exit_request(&g, 1), ExitDisposition::ForceExit);
        // 处理器在该分支的实际动作：confirm（放行重入 ExitRequested + 作废在途看门狗）
        g.confirm();
        assert!(g.is_confirmed());
        assert_ne!(g.epoch(), epoch, "放行必须递增代号，作废在途兜底看门狗");
        assert!(
            !g.confirm_if_still_awaiting(epoch),
            "旧代号的看门狗不得再抢放行权"
        );
        // 前端迟到的应答：cancel 不得让已确认的退出「复活」
        g.cancel();
        assert!(
            !g.is_awaiting() && g.is_confirmed(),
            "迟到 cancel 不得撤销已确认退出"
        );
        // 迟到的 exit / abort（又 confirm 一次）幂等，不改变状态
        g.confirm();
        assert!(g.is_confirmed() && !g.is_awaiting());
        // 进程尚未真正退出时再来一次 ExitRequested：直接放行，不再挂起
        assert_eq!(dispose_exit_request(&g, 1), ExitDisposition::Allow);
    }

    /// 兜底放行后的状态一致性：confirmed 不被后续迟到动作推翻，下一次退出流程不受影响。
    #[tokio::test]
    async fn watchdog_release_keeps_confirmed_state_consistent() {
        let g = ExitGuard::default();
        let ExitDisposition::Prompt { epoch, .. } = dispose_exit_request(&g, 1) else {
            panic!("有 run 在跑时首次请求必须下发询问");
        };
        assert!(exit_watchdog(&g, epoch, std::future::ready(())).await);
        // 重入 ExitRequested：放行（不会因兜底放行而重新弹窗/挂起）
        assert_eq!(dispose_exit_request(&g, 1), ExitDisposition::Allow);
        // 收尾代码路径可能重复 confirm：幂等
        g.confirm();
        assert!(g.is_confirmed() && !g.is_awaiting());
        // 旧代号看门狗抢不到放行权（不会二次 app.exit）
        assert!(!g.confirm_if_still_awaiting(epoch));
        // 前端终于回了 cancel：不得让已确认的退出回到等待态
        g.cancel();
        assert!(g.is_confirmed() && !g.is_awaiting());
        assert_eq!(dispose_exit_request(&g, 1), ExitDisposition::Allow);
    }
}
