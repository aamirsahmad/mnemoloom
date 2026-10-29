//! Data model: one record per tmux pane running a coding agent.

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Which harness a pane is running.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Kind {
    Claude,
    Codex,
    Pi,
    OpenCode,
}

impl Kind {
    pub fn as_str(self) -> &'static str {
        match self {
            Kind::Claude => "claude",
            Kind::Codex => "codex",
            Kind::Pi => "pi",
            Kind::OpenCode => "opencode",
        }
    }
}

/// What the agent is doing right now, most urgent first.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum State {
    /// Stopped, asking the user something.
    Input,
    /// Mid-turn.
    Busy,
    /// Monitors, shells, subagents or an active goal running.
    Bg,
    /// At the prompt, nothing pending.
    #[default]
    Idle,
}

impl State {
    pub const ALL: [State; 4] = [State::Input, State::Busy, State::Bg, State::Idle];

    pub fn as_str(self) -> &'static str {
        match self {
            State::Input => "input",
            State::Busy => "busy",
            State::Bg => "bg",
            State::Idle => "idle",
        }
    }

    /// Sort rank: the agent waiting on you comes first.
    pub fn rank(self) -> u8 {
        self as u8
    }
}

/// Input-age classification, computed at display time.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Inactivity {
    Working,
    Recent,
    Dormant,
    #[default]
    Unknown,
}

impl Inactivity {
    pub fn as_str(self) -> &'static str {
        match self {
            Inactivity::Working => "working",
            Inactivity::Recent => "recent",
            Inactivity::Dormant => "dormant",
            Inactivity::Unknown => "unknown",
        }
    }
}

/// Optional evidence attached by activity enrichment.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Extra {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub activity_probe_ok: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub runtime_status: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub goal_status: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub activity_home: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub input_evidence: Option<String>,
}

/// A coding agent running inside a tmux pane. Field order is the JSON key order.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Agent {
    pub pane: String,
    pub session: String,
    pub kind: Kind,
    #[serde(default)]
    pub state: State,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub cwd: String,
    #[serde(default)]
    pub repo: String,
    #[serde(default)]
    pub branch: String,
    #[serde(default)]
    pub model: String,
    #[serde(default)]
    pub info: String,
    #[serde(default)]
    pub pid: u32,
    #[serde(default)]
    pub session_id: String,
    #[serde(default)]
    pub last_input_at: Option<f64>,
    #[serde(default = "unknown")]
    pub input_source: String,
    #[serde(default = "unknown")]
    pub input_scope: String,
    #[serde(skip_deserializing)]
    pub inactivity: Inactivity,
    #[serde(skip_deserializing)]
    pub input_age_seconds: Option<f64>,
    #[serde(default)]
    pub extra: Extra,
}

fn unknown() -> String {
    "unknown".into()
}

const WORKING_STATUSES: [&str; 3] = ["shell", "working", "running"];

/// Whether a Claude registry status means background work is running.
pub fn is_working_status(status: &str) -> bool {
    WORKING_STATUSES.contains(&status)
}

impl Agent {
    pub fn new(pane: String, session: String, kind: Kind) -> Self {
        Agent {
            pane,
            session,
            kind,
            state: State::Idle,
            name: String::new(),
            cwd: String::new(),
            repo: String::new(),
            branch: String::new(),
            model: String::new(),
            info: String::new(),
            pid: 0,
            session_id: String::new(),
            last_input_at: None,
            input_source: unknown(),
            input_scope: unknown(),
            inactivity: Inactivity::Unknown,
            input_age_seconds: None,
            extra: Extra::default(),
        }
    }

    /// Classify old submitted input independently of current activity.
    pub fn classify_inactivity(&mut self, threshold_hours: f64, now: f64) {
        self.input_age_seconds = self.last_input_at.map(|t| (now - t).max(0.0));
        let working = matches!(self.state, State::Busy | State::Bg)
            || self
                .extra
                .runtime_status
                .as_deref()
                .is_some_and(is_working_status)
            || self.extra.goal_status.as_deref() == Some("active");
        self.inactivity = match self.input_age_seconds {
            _ if working => Inactivity::Working,
            None => Inactivity::Unknown,
            Some(age) if age > threshold_hours * 3600.0 => Inactivity::Dormant,
            Some(_) => Inactivity::Recent,
        };
    }

    /// Rebuild an agent from a cached record, rejecting wrong shapes and types.
    pub fn from_value(value: &Value) -> Option<Agent> {
        let obj = value.as_object()?;
        if obj.get("inactivity").is_some_and(|v| !v.is_string())
            || obj.get("extra").is_some_and(|v| !v.is_object())
        {
            return None;
        }
        let valid_time = |key: &str| match obj.get(key) {
            None | Some(Value::Null) => true,
            Some(v) => v.as_f64().is_some_and(|f| f.is_finite() && f >= 0.0),
        };
        if !valid_time("last_input_at") || !valid_time("input_age_seconds") {
            return None;
        }
        serde_json::from_value(value.clone()).ok()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn base() -> Value {
        json!({"pane": "s:1.1", "session": "s", "kind": "claude"})
    }

    fn with(key: &str, v: Value) -> Value {
        let mut b = base();
        b[key] = v;
        b
    }

    #[test]
    fn from_value_accepts_minimal_and_defaults() {
        let a = Agent::from_value(&base()).unwrap();
        assert_eq!(a.state, State::Idle);
        assert_eq!(a.input_source, "unknown");
        assert_eq!(a.input_scope, "unknown");
        assert_eq!(a.pid, 0);
    }

    #[test]
    fn from_value_ignores_unknown_keys() {
        assert!(Agent::from_value(&with("bogus", json!(1))).is_some());
    }

    #[test]
    fn from_value_rejects_bad_records() {
        let bad = [
            with("pane", Value::Null),
            with("session", json!(3)),
            with("kind", json!("vim")),
            with("state", json!("waiting")),
            with("name", Value::Null),
            with("pid", json!("12")),
            with("pid", json!(true)),
            with("extra", json!([])),
            with("last_input_at", json!(-1.0)),
            with("last_input_at", json!(true)),
            with("input_age_seconds", json!("x")),
            with("inactivity", json!(5)),
            json!({"session": "s", "kind": "claude"}),
            json!({"pane": "p", "session": "s"}),
            json!("not a dict"),
        ];
        for v in bad {
            assert!(Agent::from_value(&v).is_none(), "accepted {v}");
        }
    }

    #[test]
    fn inactivity_boundaries() {
        let mut a = Agent::new("p".into(), "s".into(), Kind::Claude);
        let now = 1_000_000.0;
        a.last_input_at = Some(now - (72.0 * 3600.0 + 1.0));
        a.classify_inactivity(72.0, now);
        assert_eq!(a.inactivity, Inactivity::Dormant);
        a.last_input_at = Some(now - 72.0 * 3600.0);
        a.classify_inactivity(72.0, now);
        assert_eq!(a.inactivity, Inactivity::Recent);
        a.last_input_at = None;
        a.classify_inactivity(72.0, now);
        assert_eq!(a.inactivity, Inactivity::Unknown);
        assert_eq!(a.input_age_seconds, None);
    }

    #[test]
    fn working_overrides_age() {
        let mut a = Agent::new("p".into(), "s".into(), Kind::Codex);
        a.last_input_at = Some(0.0);
        a.extra.goal_status = Some("active".into());
        a.classify_inactivity(72.0, 1e9);
        assert_eq!(a.inactivity, Inactivity::Working);
        a.extra.goal_status = None;
        a.state = State::Input;
        a.classify_inactivity(72.0, 1e9);
        assert_eq!(a.inactivity, Inactivity::Dormant);
        a.extra.runtime_status = Some("shell".into());
        a.classify_inactivity(72.0, 1e9);
        assert_eq!(a.inactivity, Inactivity::Working);
    }

    #[test]
    fn future_input_has_zero_age() {
        let mut a = Agent::new("p".into(), "s".into(), Kind::Claude);
        a.last_input_at = Some(200.0);
        a.classify_inactivity(72.0, 100.0);
        assert_eq!(a.input_age_seconds, Some(0.0));
    }
}
