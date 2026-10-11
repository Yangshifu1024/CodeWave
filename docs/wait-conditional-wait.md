# wait 条件等待（until）与可等待目标登记表

> 分支 `feat/wait-until-targets`，基线 `main@892b8f6`。纯后端改动：新增登记表内核 + `wait` 新增声明式 `until` 参数，**不改前端、不改提示词、不改事件键名、不改批次独占策略、不新增依赖**。

## 一、问题

`wait` 工具原本只有 `sleep(secs)` 与 `cancel` 两个 `select!` 分支（[tools/wait.rs](../src-tauri/src/tools/wait.rs)）：模型想等 dev server 就绪、等构建跑完，只能**猜一个时长**——猜短了反复重试，猜长了白等。更糟的是出参 `waited_seconds` 直接回填入参，模型从结果里看不出实际等了多久。

原始动机即「增加 wait 的回调，在等待的任务完成时通知 wait」。

## 二、三项设计决策

1. **通用「可等待目标」登记表**：任何后台对象都可登记为可等待目标，而不是只覆盖既有三类。
2. **声明式 `until` 参数**：条件即数据、模型可见、自解释、可单测。
3. **轮询唤醒**（间隔固定 1s）：完成方**不**主动推送。

### 为什么是轮询而不是推送

`EventSink` 是单向出网广播（[host/events.rs](../src-tauri/src/host/events.rs)），全仓生产代码零 `watch`/`broadcast`/`Notify`——推送式需要先给三类对象各建一条完成通道并改造其收尾路径，代价远大于收益。轮询只读内存快照（一次 `Mutex` 取值 + 几 KB 拷贝），成本可忽略，代价是约 1s 响应延迟，对「等 dev server / 等构建」这类秒到分钟级场景完全够用。

## 三、数据契约

```json
wait(seconds, reason, until?)
```

```json
{
  "until": {
    "target": "svc_1a2b3c4d",     // service / subagent / scheduled_task 回执 id
    "state":   "done",             // 可选；未传 match 时缺省 done，match-only 不要求终态
    "match":   { "text": "ready in" }  // 可选，摘要含该子串（大小写不敏感）
  }
}
```

- `state`：`done`（任意终态）/ `exited`（仅 `Succeeded`）/ `failed`（仅 `Failed`）
- `match.text` 与 `state` 是 **AND** 关系，两个都写则必须同时满足
- 仅传 `match` 时在运行中也可命中（如 dev server ready）；两个都省略才默认 `done`，显式 `done` + `match` 必须终态且匹配
- `seconds` 在传 `until` 时是**最长等待上限**，不再是「一定要睡满」

### 出参语义

| 场景 | ok | `data` |
|---|---|---|
| **不传 `until`** | true | `{waited_seconds: secs, reason}` — **逐字不变，不增任何字段** |
| 传 until，条件满足 | true | `{waited_seconds: 实测, reason, outcome: "condition_met", until: 回显, observed}` |
| 传 until，到点未满足 | **true** | `{…, outcome: "timeout", until, observed}` |
| 传 until，目标不存在 | false | `E_NOT_FOUND` |
| 传 until，参数非法 | false | `E_ARGS` |
| 任何等待中取消 | false | `E_CANCELLED` |

**判据顺序固定：取消 > 条件 > 超时。** 超时**不占用错误码**——它是正常结果，模型据 `observed` 决定下一步（继续等 / 换条件 / 直接读日志）。

> **兼容底线**：不传 `until` 时代码路径与出参**逐字未变**，`wait.rs` 原有 5 个测试用例零修改仍通过（其中两个用 `#[tokio::test(start_paused = true)]`，对虚拟时钟极敏感）。测试 `plain_mode_data_has_no_extra_fields` 用 `assert_eq!(data, json!({...}))` **全等断言**钉死这一点——一旦有人往纯计时路径塞进 `outcome`/`until`/`observed` 就会红。

## 四、为什么登记表必须独立于既有三张表

这是本设计最关键的一处判断，三条事实各自否决了「让 wait 反查既有表」：

| 事实 | 位置 | 反查既有表的后果 |
|---|---|---|
| 子代理完成即从 `core.subs` 移除（成功 / 失败 / panic **同一条** remove） | `tools/subagent.rs` | 轮询间隙反查分不清「已完成」与「从不存在」 |
| service 自然退出**不移除**，仅显式 stop 才删 | `tools/service.rs` | 照抄这张表的生命周期，本表会**只增不减** |
| 计划任务无 running 中间态、无 run id，`last_status` 只在 run 结束时写 | `core/scheduler.rs` | 两轮相同状态无法区分，必须按登记轮次重置并显式收尾 |

因此 `core/wait_targets.rs` 是**独立登记表**：条目自带终态标志，由各对象**收尾路径显式置位**，终态**只前进不后退**。

## 五、三类接入

| 目标 | 登记 | 置终态 | 移除 |
|---|---|---|---|
| service | `start` 成功入表并登记后才释放 reaper（容量失败不登记，仍回收进程） | 进程退出确认处（reaper 闭包），恒为 `Succeeded`——退出码语义仓库无消费方，臆造失败判定等于凭空造事实 | `stop` action |
| 子代理 | spawn 真正成功后 | **`SubCleanupGuard::Drop` 内，严格早于 `subs.remove`**——成功/取消/provider 错误/panic 共用本路径 | 不移除（TTL 淘汰） |
| 计划任务 | **run 启动时**（`run_task_locked`），`baseline` = 本轮开始前的 `last_status` | run 正常收尾 + skipped 早退两处 | 不移除（TTL 淘汰） |

### 计划任务登记点的两处非显然设计

**① 为什么不是「create 时登记」**：`once:` 任务在 create 之后根本没有 run，create 时登记会让条目永远停在 `Running`，`wait` 只能空转到超时；也无法表达「等下一次触发」。按 run 登记，语义是「等**这次** run 跑完」，与用户意图一致。

**② 基线只作诊断，不作完成门槛**：每轮 `ScheduledTask` 登记在同一锁内重置状态、摘要与终态时间，`finish` 显式完成本轮；连续两轮 `ok` / `error` / `skipped` 都可完成，且无上轮状态时也重置。`baseline` 与 `observed_status` 保留各自的诊断语义。[发版审查修复](./release-review-remediation.md) 删除了「状态必须不同」的错误门槛。

> 注：`ScheduledTask` 结构体**没有 `Default`**，给它加字段会逐字打破 `scheduler.rs` 内约 15 处测试结构体字面量。登记表与该结构体**刻意解耦**。

## 六、活日志判据（`update_text`）

`finish` 只在进程退出时才调，而旗舰场景「等 dev server 日志出现 ready」要在**进程还活着**时就命中。因此 `WaitTargets::update_text` 由 service **已有的 1s ticker** 写入日志尾（与日志推送同一口径，`core/` 内无第二套轮询）：

```rust
// tools/service.rs 的 ticker 循环内，无条件写入（含空串）
ticker_core.wait_targets.update_text(&ticker_svc_id, tail.clone());
```

无条件写入是刻意的：服务无输出时 tail 恒为空，但「它还活着」本身就是 wait 超时时该看到的证据，不能因为空而丢。没有这个方法时，`match.text` 在服务运行期间**永远不命中**，且 `observed.text` 也是空。

## 七、生命周期与淘汰

- 内存态、**不持久化**：与三类目标的运行态现状一致（进程重启即全失，`wait` 跨重启无意义）。
- **TTL 惰性清扫**（`ENTRY_TTL` = 1 小时）发生在 `lookup` 内。**必须在这里而不能靠调用方**：service 自然退出、计划任务运行、子代理收尾三条路径都不 remove 本表条目，靠表自己淘汰才不会只增不减。
- 清扫**只淘汰终态**：进行中永不淘汰（否则等一个正在跑的任务会被凭空判成「不存在」，比多等一轮严重得多）。
- `ids()` **不触发清扫**——它只用于拼 `E_NOT_FOUND` 的错误提示，突变会不可预期。

## 八、分层与惯例

- 登记表挂在 `AgentCore`（跨会话进程级对象），**不是** `SessionRuntime`（全会话内语义）。
- 照抄 `ServiceTable`（`tools/service.rs`）范式：`DashMap<String, Arc<T>>` + 私有字段 + `&self` 方法，`get` 在方法内 clone 出 `Arc` 让调用方脱离分片锁；`Default` 构造于 `AgentCore::new`，**不用** `OnceLock`/`lazy_static`（全仓无先例）。
- **未新增任何全局锁**，故不进入 `save_lock → index_lock` 锁序图（[technical-design](./technical-design.md)）；锁全在条目内部。
- 三处接入均在任何既有锁的临界区**之外**调用登记表方法。
- `core/` 不依赖 tauri；无新增依赖（`dashmap` / `tokio` / `serde_json` 均已在用）。
- 事件面 29 键零改动、`tools/batch.rs` 零改动——条件等待**不解除**批次独占（`wait` 仍是 `Interactive`，仍必须独占批次）。

## 九、审查发现并修复的两个正确性缺陷

方案实现后由 reviewer + 交叉核对发现，均已修复并补回归测试：

**🔴 计划任务二次运行立即误报「已完成」**：`register` 原为幂等不重置，二次运行被上轮终态直接满足。现按 `ScheduledTask` 类别每轮重置，不再靠是否有基线决定；service / 子代理单生命周期登记仍幂等。后续审查又修复了连续同状态无法完成及跨锁读到混合快照的问题，见[发版审查修复](./release-review-remediation.md)。

**🔴 `match.text` 在终态前恒不可用**：初版仅在 `finish` 写文本，后由 service ticker 的 `update_text` 补活日志；后续审查修复判据仍隐含 `done` 的缺口，仅传 match 时不再要求退出（见 §3 与[发版审查修复](./release-review-remediation.md)）。

两者同源，都由 `wait_targets` 的单测钉死。

## 十、验证

- `cargo test`：1249 passed / 0 failed / 3 ignored（基线 1244 + 新增回归 5）
- `cargo fmt --check` 干净、`cargo clippy --workspace --all-targets` **0 warning**（项目基线要求）
- `pnpm --dir ui test` 1348 passed / 109 文件、`pnpm --dir ui build` 通过（确认前端零连带破坏）
- 兼容钉子：原有 5 个 wait 用例零修改全绿
