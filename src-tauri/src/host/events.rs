//! EventSink 的 Tauri 实现：高频帧走 ipc::Channel（点对点、有序），低频事件走 emit。
//! channel 注册表自持一份 ChannelRegistry（host state）；core 只见 EventSink trait。

use crate::core::agent::{EventSink, Frame};
use crate::core::types::SessionId;
use dashmap::DashMap;
use std::sync::Arc;
use tauri::ipc::Channel;
use tauri::{AppHandle, Emitter};

/// 会话 → 前端流式 channel（start_chat 时注册）；subs 记录子代理 → 父会话绑定
///（[docs/subagent-interaction-drawer](../../../docs/subagent-interaction-drawer.md)：子代理 spawn 时注册，其帧包 Frame::Sub 借父 channel 下发）。
#[derive(Default)]
pub struct ChannelRegistry {
    /// 会话 id → 已注册的前端 channel
    pub map: DashMap<SessionId, Channel<Frame>>,
    /// 子代理 id → 父会话 id（存在期覆盖整个 run，父 channel 重注册无误投窗口）
    pub subs: DashMap<SessionId, SessionId>,
}

/// EventSink 的 Tauri 实现：帧走 channel、事件走 emit_to("main")。
pub struct TauriSink {
    app: AppHandle,
    channels: Arc<ChannelRegistry>,
}

impl TauriSink {
    /// 用应用句柄与 channel 注册表构造 sink。
    pub fn new(app: AppHandle, channels: Arc<ChannelRegistry>) -> Self {
        TauriSink { app, channels }
    }
}

impl EventSink for TauriSink {
    /// 下发一帧：子代理会话先查 subs 绑定，命中则包 Frame::Sub 信封借父 channel 下发；
    /// 普通会话直接查自身 channel。发送失败按级别记日志（可观测，不静默）。
    fn channel_frame(&self, session: &SessionId, frame: &Frame) {
        // 子代理 channel：包信封借父会话 channel 下发（绑定在 spawn 时建立、收尾时解绑——
        // 绑定存在于整个 run 期间，因此不存在父 channel 重注册导致的误投窗口）
        if let Some(parent) = self.channels.subs.get(session) {
            let parent_id = parent.value().clone();
            drop(parent); // 访问 map 前先释放 subs 读锁（两个 DashMap 互不冲突，纯缩短持锁窗口）
            match self.channels.map.get(&parent_id) { Some(ch) => {
                if let Err(e) = ch.send(Frame::Sub {
                    sub_id: session.clone(),
                    frame: Box::new(frame.clone()),
                }) {
                    // P4-1：发送失败变为可观测（此前 debug 级静默，丢帧不可见）
                    tracing::warn!("子代理 [{session}] channel 发送失败：{e}");
                }
            } _ => {
                tracing::warn!(
                    "子代理 [{session}] 父会话 [{parent_id}] 无已注册 channel，帧丢弃（R2 诊断锚点）"
                );
            }}
            return;
        }
        if let Some(ch) = self.channels.map.get(session) {
            if let Err(e) = ch.send(frame.clone()) {
                tracing::debug!("channel 发送失败（窗口可能已关闭）：{e}");
            }
        }
    }

    /// 低频事件：emit 到 labeled "main" 窗口（27 键事件面，契约由前端测试守护）。
    fn emit(&self, _session: &SessionId, event: &str, payload: serde_json::Value) {
        let _ = self
            .app
            .emit_to(tauri::EventTarget::labeled("main"), event, payload);
    }

    /// 登记子代理 → 父会话的 channel 绑定（子代理帧借父 channel 下发）。
    fn bind_sub_channel(&self, parent: &SessionId, sub: &SessionId) {
        self.channels.subs.insert(sub.clone(), parent.clone());
    }

    /// 解除子代理 channel 绑定（子代理收尾时调用，幂等）。
    fn unbind_sub_channel(&self, sub: &SessionId) {
        self.channels.subs.remove(sub);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// P4-2 回归：subs 注册表的 bind/unbind 幂等成对，防 grow-only 泄漏。
    /// TauriSink 需要 AppHandle、单测无法构造，因此直接验证 ChannelRegistry 语义（同一数据结构）。
    #[test]
    fn subs_registry_bind_unbind_idempotent() {
        let reg = ChannelRegistry::default();
        let parent: SessionId = "sess-1".into();
        let sub: SessionId = "sub_ab12cd34".into();
        reg.subs.insert(sub.clone(), parent.clone());
        // 重复 bind：覆盖不报错
        reg.subs.insert(sub.clone(), "sess-2".into());
        assert_eq!(reg.subs.get(&sub).unwrap().value(), "sess-2");
        // unbind 幂等：移除两次无副作用
        assert!(reg.subs.remove(&sub).is_some());
        assert!(reg.subs.remove(&sub).is_none());
        assert!(reg.subs.get(&sub).is_none());
    }

    /// R2 诊断锚点：subs 命中但父 channel 缺失，必须与「未绑定」分支可区分
    ///（P4-1 后两分支日志不同；本测试钉死 subs 数据结构的查找语义）。
    #[test]
    fn subs_hit_but_parent_channel_missing_is_distinguishable() {
        let reg = ChannelRegistry::default();
        let sub: SessionId = "sub_x".into();
        reg.subs.insert(sub.clone(), "ghost-parent".into());
        // subs 命中
        assert!(reg.subs.get(&sub).is_some());
        // 父 channel 不在 map：channel_frame 会走 warn 分支而非静默丢弃
        assert!(reg.map.get("ghost-parent").is_none());
    }
}
