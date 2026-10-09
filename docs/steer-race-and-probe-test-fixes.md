# steer 时序与 Git Bash 回归测试修复

> 日期：2026-10-10。基线：`8e73d7d`，基于自 `c814b73` 以来提交的审查。
> 关联：[steer-run-inject](./steer-run-inject.md)、[git-bash-probe-nonstandard-path](./git-bash-probe-nonstandard-path.md)。

## 范围与归属

| 项 | 原问题 | 实现与验收 |
|---|---|---|
| F1 | 最后一轮 SSE/工具期间的注入留在 mpsc，收尾后不进历史、可能跨 run 或丢失 | 收据确认、接纳/收尾共用锁；Finish 与 batch_done 复查；请求含消息、历史及 checkpoint 同时验证 |
| F2 | await 期间同项可重复 inject，done 又自动出队 | QueueItem.injecting 同步占用；按钮和自动出队守卫；失败保留；驻留条目结算、快照不落占用 |
| F3 | steer 无条件 Continue 绕过 MAX_TEXT_TURNS | 普通纯文本在上限回落 Finish；不依赖 rejected 分支 |
| F4 | probe None 跳过断言，本机安装条件掩盖退化 | 临时非标准布局注入生产共用探测链，结果精确匹配 |
| F5 | 全命中断言 ≤2，空列表也绿 | 精确断言两个预期路径，顺序及去重同时守护 |

F1–F3 属原 steer 提交，F4/F5 属后续 Git Bash 测试。用户已有的 [main-run-finish-with-pending-todos](./main-run-finish-with-pending-todos.md) 文档修正予以保留。

## 方案补严

原计划的「try_send 后检查 inject_accepting」仍有 TOCTOU：消息已消费后窗口关闭，IPC 可能误报失败，前端留队再发。最终使用消息信封 + oneshot 收据，接纳、空队列关闭、拒绝未消费项共用接收端 Mutex；host 只转调 core。

驱动消化时统一追加历史、发 run:inject、checkpoint、确认收据；IPC 成功指本 run 内存历史归属，保存失败沿用现有状态上报。异常/取消/上限只拒绝未消费项，不把它们留到下个 run。RAII 在 panic 或任务释放时也关闭窗口。

收尾复查发现新消息且有预算时续跑，纯文本路径递增 text_turns；无预算则明确拒绝，前端留队。旧 suggest 若被续跑，清空其结果和现有建议事件，不新增事件键。已消费消息仍遵守 report 优先级。

前端占用期间还禁止编辑/删除，避免已提交文本再次进入草稿。占用不进磁盘 UI 快照；关 Tab 选择保留内容时，确认仍结算驻留表，重开不会重复发送或永久占用。pendingItemId 不复用、不新增写入。

注入确认晚于收尾事件时，仅正常 `run:done` 设置的 `queueResumeAfterInjection` 标记允许补续队；`run:cancelled`、`run:error` 清掉标记，保留急停语义。每次启动新 run 也清掉旧标记，避免新启动失败后旧注入确认误启剩余队列。

## 回归与判别力

| 项 | 证据 |
|---|---|
| F1 | SSE 首轮响应闸门固定注入窗口；旧实现下纯文本与 suggest 两例均因历史缺消息转红；修复后同时检查收据、后续请求与磁盘历史 |
| F1 边界 | 空闲、缓冲满、取消与 max_steps 拒绝未消费消息；再次运行无旧消息泄漏；收据在消化前保持未完成 |
| F2 | 旧实现连续点击发送两次，新用例转红；修复后单次注入。done 跳过占用项、失败保留、确认后续队、关 Tab 结算均覆盖 |
| F2 快照 | 给现有往返夹具加 injecting:true，旧快照路径转红；修复后磁盘结构仍只含 id/text/images |
| F2 复审补充 | 取消/出错后确认成功不得自动续队；用户启动新 run 失败后，旧确认不得借用旧 done 标记续队。三例均先红后绿 |
| F3 | 普通纯文本 + steer + 上限，旧实现返回 Continue 而非 Finish，转红；修复后通过 |
| F4 | 临时缺失候选与独立安装目录；去掉 git.exe 反推分支得到 None，精确路径断言转红 |
| F5 | 临时恒返空时精确路径断言转红；finally 恢复原始文件 |

## 本地门禁与复审

2026-10-10 在 Windows 本机执行仓库 `prepr` 门禁分组，最后一处前端修正后重跑前端 lint/test/build。结果如下；临时反向验证文件未计入仓库测试基线。

| 检查 | 结果 |
|---|---|
| `cargo fmt --all -- --check` | 通过 |
| `cargo clippy --all-targets` | 通过，0 warning |
| `cargo test --workspace` | 1183 passed / 3 ignored / 0 failed |
| `pnpm --dir ui test` | 1237 passed / 102 文件 / 0 failed |
| `pnpm --dir ui build` | type check 与 Vite build 通过 |
| `pnpm --dir ui run lint` | 通过 |
| `node --test scripts/**/*.test.mjs` | 24 passed |
| `pnpm install --frozen-lockfile` | 通过 |

Standards 与 Spec 两路复审通过，无剩余发现。新增后端 6 个用例、前端 8 个用例；Git Bash 两个原有用例和 UI 快照原有用例加强断言。AGENTS.md 保持 HEAD 内容，未把本批新计数写回历史基线。以上只覆盖当前 Windows 平台，未执行三平台 CI 或 GUI 自动点验。

## 手动验证

1. 运行长任务并排入纯文本消息，连续点同项「立即」：按钮转圈，只有一次注入，无取消 notice。
2. 在最后一段回答输出途中点「立即」：新要求在同一 run 被处理；等模型已结束后再点则保留条目或正常作为新 run 发出。
3. 注入确认期间让当前 run 结束：该项不被自动再次发送；其余未占用项按序执行。
4. 注入确认期间编辑/删除按钮不可点；关闭但保留队列，再重开，已确认项不再出现。
5. 注入期间点停止：主 run 与子代理仍按原急停语义结束；未消费项留队，剩余队列保持暂停，即便注入成功确认晚到也不自动续队。用户重新继续时仅发送一次。
6. 带图项运行中仍不可立即注入；排队自然出队后附件完整。
7. 连续纯文本达到上限时能够收尾；未消费的后续消息留队，不偷偷进入下一次不相关的 run。

## 回滚与提交边界

提交按 F1（runtime/drive/IPC/相关回归）、F3（上限）、F2（前端占用与持久化）、F4/F5（探测测试）及配套文档分组；F1 独立提交以便回滚，F1/F3 共用 drive.rs/tests.rs 的改动按 hunk 分开 stage。逐项回滚由用户执行。
