//! Incidents (R-A6): a rule that fires can open one; a person acknowledges and resolves it. The
//! timeline records every step with its time and who did it.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Event {
    pub ms: i64,
    pub event: String,
    pub by: String,
    #[serde(default)]
    pub note: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Incident {
    pub alert_id: String,
    pub title: String,
    /// `open`, `acknowledged` or `resolved`.
    pub state: String,
    pub created_ms: i64,
    #[serde(default)]
    pub acknowledged_ms: Option<i64>,
    #[serde(default)]
    pub resolved_ms: Option<i64>,
    pub timeline: Vec<Event>,
}

impl Incident {
    pub fn open(alert_id: &str, title: &str, now_ms: i64, by: &str, note: &str) -> Incident {
        Incident {
            alert_id: alert_id.into(),
            title: title.into(),
            state: "open".into(),
            created_ms: now_ms,
            acknowledged_ms: None,
            resolved_ms: None,
            timeline: vec![Event {
                ms: now_ms,
                event: "opened".into(),
                by: by.into(),
                note: note.into(),
            }],
        }
    }

    pub fn is_active(&self) -> bool {
        self.state != "resolved"
    }

    /// Only an open incident can be acknowledged.
    pub fn acknowledge(&mut self, now_ms: i64, by: &str, note: &str) -> Result<(), String> {
        if self.state != "open" {
            return Err(format!("the incident is {}, not open", self.state));
        }
        self.state = "acknowledged".into();
        self.acknowledged_ms = Some(now_ms);
        self.push(now_ms, "acknowledged", by, note);
        Ok(())
    }

    /// An open or acknowledged incident can be resolved.
    pub fn resolve(&mut self, now_ms: i64, by: &str, note: &str) -> Result<(), String> {
        if self.state == "resolved" {
            return Err("the incident is already resolved".into());
        }
        self.state = "resolved".into();
        self.resolved_ms = Some(now_ms);
        self.push(now_ms, "resolved", by, note);
        Ok(())
    }

    /// A note on the timeline (e.g. the rule fired again).
    pub fn push(&mut self, ms: i64, event: &str, by: &str, note: &str) {
        if self.timeline.len() < 200 {
            self.timeline.push(Event {
                ms,
                event: event.into(),
                by: by.into(),
                note: note.chars().take(500).collect(),
            });
        }
    }
}
