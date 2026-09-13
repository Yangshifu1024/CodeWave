//! Time-throttled buffer for high-frequency streaming output ([docs/p0-plan](../../../docs/p0-plan.md) §5.4: 64ms window).
//! The run loop holds one StreamBuffer; a ticker task flushes it to the Channel every 64ms.
//! Segment serialization: Text/Reasoning are recorded interleaved in arrival order (adjacent same-kind segments merged), flush output preserves order.

use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Instant;

/// Streaming delta segment: array order = arrival order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Segment {
    Text(String),
    Reasoning(String),
}

/// Throttled batch: segments in arrival order; generation = generation at take time (reset increments it, letting the frontend drop stale attempt frames).
#[derive(Default)]
pub struct StreamBuffer {
    pub(crate) segments: Vec<Segment>,
    pub(crate) generation: u64,
    pub(crate) dirty: bool,
}

#[derive(Default)]
pub struct ThrottledStream {
    inner: std::sync::Mutex<StreamBuffer>,
    last_flush: std::sync::Mutex<Option<Instant>>,
    generation: AtomicU64,
    /// 最后一次流活动（毫秒 UNIX 时间）：流停滞看门狗的观测点（collect_deltas 每帧 touch）
    last_activity: AtomicU64,
}

impl ThrottledStream {
    pub fn new() -> Self {
        Self::default()
    }

    /// 刷新最后活动时间（每收到一帧 delta 调用一次）。
    pub fn touch(&self) {
        self.last_activity.store(now_ms(), Ordering::SeqCst);
    }

    /// 最后活动时间（毫秒 UNIX 时间）。
    pub fn last_activity_ms(&self) -> u64 {
        self.last_activity.load(Ordering::SeqCst)
    }

    pub fn push_text(&self, s: &str) {
        let mut g = self.inner.lock().unwrap();
        match g.segments.last_mut() {
            Some(Segment::Text(buf)) => buf.push_str(s),
            _ => g.segments.push(Segment::Text(s.to_string())),
        }
        g.dirty = true;
    }

    pub fn push_reasoning(&self, s: &str) {
        let mut g = self.inner.lock().unwrap();
        match g.segments.last_mut() {
            Some(Segment::Reasoning(buf)) => buf.push_str(s),
            _ => g.segments.push(Segment::Reasoning(s.to_string())),
        }
        g.dirty = true;
    }

    /// If at least `min_interval` has passed since the last flush, take and return the pending deltas (order preserved per segment).
    pub fn try_take(&self, min_interval: std::time::Duration) -> Option<StreamBuffer> {
        let mut last = self.last_flush.lock().unwrap();
        let now = Instant::now();
        if let Some(t) = *last {
            if now.duration_since(t) < min_interval {
                return None;
            }
        }
        let mut g = self.inner.lock().unwrap();
        if !g.dirty {
            return None;
        }
        *last = Some(now);
        let mut buf: StreamBuffer = std::mem::take(&mut *g);
        buf.generation = self.generation.load(Ordering::SeqCst);
        Some(buf)
    }

    /// Final flush: take regardless of interval.
    pub fn take_final(&self) -> Option<StreamBuffer> {
        let mut g = self.inner.lock().unwrap();
        if !g.dirty {
            return None;
        }
        let mut buf: StreamBuffer = std::mem::take(&mut *g);
        buf.generation = self.generation.load(Ordering::SeqCst);
        Some(buf)
    }

    /// Discard unsent deltas (drops a half-delivered reply on retry); the generation increments so the frontend can identify and drop frames already taken by the old attempt.
    pub fn reset(&self) {
        let mut g = self.inner.lock().unwrap();
        *g = StreamBuffer::default();
        self.generation.fetch_add(1, Ordering::SeqCst);
        // 新尝试的活跃度基准：避免上一尝试的陈旧时间戳立即触发停滞看门狗
        self.touch();
    }

    /// Current generation (sent along with the run:retry event).
    pub fn generation(&self) -> u64 {
        self.generation.load(Ordering::SeqCst)
    }
}

/// 当前时刻（毫秒 UNIX 时间；系统时钟早于 epoch 时退化为 0）。
pub fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn respects_interval_and_final() {
        let t = ThrottledStream::new();
        t.push_text("a");
        let buf = t.try_take(Duration::from_secs(10)).unwrap(); // first take always sends
        assert_eq!(buf.segments, vec![Segment::Text("a".into())]);
        t.push_text("b");
        assert!(t.try_take(Duration::from_secs(10)).is_none()); // nothing within the interval
        let f = t.take_final().unwrap();
        assert_eq!(f.segments, vec![Segment::Text("b".into())]);
        assert!(t.take_final().is_none());
    }

    #[test]
    fn reset_bumps_generation() {
        // Review C2: the generation increments on retry; the frontend drops in-flight frames from the old attempt accordingly
        let t = ThrottledStream::new();
        t.push_text("a");
        assert_eq!(t.try_take(Duration::from_secs(10)).unwrap().generation, 0);
        t.reset();
        assert_eq!(t.generation(), 1);
        t.push_text("b");
        assert_eq!(t.take_final().unwrap().generation, 1);
    }

    #[test]
    fn touch_advances_last_activity() {
        // 流停滞看门狗的观测点：push/reset 都应刷新最后活动时间
        let t = ThrottledStream::new();
        let before = t.last_activity_ms();
        t.push_text("a");
        assert!(t.last_activity_ms() >= before);
        t.reset();
        assert!(t.last_activity_ms() >= before);
    }

    #[test]
    fn interleaved_segments_preserve_order() {
        // Ordering preserved when thinking/text interleave; adjacent same-kind segments merged
        let t = ThrottledStream::new();
        t.push_text("a");
        t.push_reasoning("r1");
        t.push_text("b");
        t.push_text("c");
        t.push_reasoning("r2");
        let f = t.take_final().unwrap();
        assert_eq!(
            f.segments,
            vec![
                Segment::Text("a".into()),
                Segment::Reasoning("r1".into()),
                Segment::Text("bc".into()),
                Segment::Reasoning("r2".into()),
            ]
        );
    }
}
