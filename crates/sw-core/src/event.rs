//! The event stream: steps emit status/log/progress events through a broadcast
//! bus → SSE → UI. Events are also appended to `events.log` and replayed to late
//! subscribers by the store.

use crate::model::{RunStatus, StepId, StepStatus};
use serde::{Deserialize, Serialize};
use tokio::sync::broadcast;

/// A single event about a run, serialized to JSON for the SSE stream.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Event {
    /// A step changed status.
    StepStatus { step: StepId, status: StepStatus },
    /// A line of log output from a step.
    Log { step: StepId, line: String },
    /// Coarse progress for a step, 0..=100.
    Progress { step: StepId, percent: u8 },
    /// The run as a whole changed status.
    RunStatus { status: RunStatus },
}

/// A cloneable broadcast bus. Each subscriber gets every event published after
/// it subscribed; the store handles historical replay for late joiners.
#[derive(Debug, Clone)]
pub struct EventBus {
    tx: broadcast::Sender<Event>,
}

impl EventBus {
    pub fn new() -> Self {
        let (tx, _rx) = broadcast::channel(1024);
        Self { tx }
    }

    /// Publish an event. Sending never blocks; if there are no subscribers the
    /// event is simply dropped (the store still persists it separately).
    pub fn publish(&self, event: Event) {
        let _ = self.tx.send(event);
    }

    /// Subscribe to all future events.
    pub fn subscribe(&self) -> broadcast::Receiver<Event> {
        self.tx.subscribe()
    }
}

impl Default for EventBus {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn subscribers_receive_published_events() {
        let bus = EventBus::new();
        let mut rx = bus.subscribe();
        bus.publish(Event::RunStatus {
            status: RunStatus::Running,
        });
        let got = rx.recv().await.unwrap();
        assert_eq!(
            got,
            Event::RunStatus {
                status: RunStatus::Running
            }
        );
    }

    #[test]
    fn event_serializes_with_kind_tag() {
        let e = Event::Progress {
            step: "m/s".into(),
            percent: 42,
        };
        let json = serde_json::to_string(&e).unwrap();
        assert!(json.contains("\"kind\":\"progress\""));
        assert!(json.contains("\"percent\":42"));
    }
}
