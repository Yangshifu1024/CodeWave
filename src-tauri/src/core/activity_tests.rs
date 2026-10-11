use super::activity::{ActivitySink, ActivityState};
use super::agent::{EventSink, Frame};
use super::types::SessionId;
use std::sync::{Arc, Mutex};

#[derive(Default)]
struct DelegatedSink(Mutex<Vec<String>>);

impl EventSink for DelegatedSink {
    fn channel_frame(&self, session: &SessionId, _: &Frame) {
        self.0.lock().unwrap().push(format!("frame:{session}"));
    }
    fn emit(&self, session: &SessionId, event: &str, _: serde_json::Value) {
        self.0
            .lock()
            .unwrap()
            .push(format!("event:{session}:{event}"));
    }
    fn bind_sub_channel(&self, parent: &SessionId, sub: &SessionId) {
        self.0.lock().unwrap().push(format!("bind:{parent}:{sub}"));
    }
    fn unbind_sub_channel(&self, sub: &SessionId) {
        self.0.lock().unwrap().push(format!("unbind:{sub}"));
    }
}

#[test]
fn activity_sink_transparently_delegates_all_entry_points() {
    let downstream = Arc::new(DelegatedSink::default());
    let sink = ActivitySink {
        activity: Arc::new(ActivityState::default()),
        downstream: downstream.clone(),
    };
    sink.bind_sub_channel(&"parent".into(), &"sub".into());
    sink.channel_frame(
        &"sub".into(),
        &Frame::DeltaText {
            generation: 0,
            text: "text".into(),
        },
    );
    sink.emit(
        &"parent".into(),
        "run:start",
        serde_json::json!({"run_id":"r"}),
    );
    sink.unbind_sub_channel(&"sub".into());
    assert_eq!(
        *downstream.0.lock().unwrap(),
        vec![
            "bind:parent:sub",
            "frame:sub",
            "event:parent:run:start",
            "unbind:sub"
        ]
    );
}
