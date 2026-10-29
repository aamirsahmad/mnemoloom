//! Last scan on disk, so the picker opens instantly.

use std::fs;
use std::path::{Path, PathBuf};

use mnemoloom::session::homes::expand_tilde;
use mnemoloom::session::paths::cache_root;
use mnemoloom::session::transcript::write_atomic_with;
use serde::Serialize;
use serde_json::Value;

use super::model::Agent;

/// `$MNEMOLOOM_MUX_CACHE` (`~` expanded), else `<cache root>/mux.json`.
pub fn path() -> PathBuf {
    match std::env::var_os("MNEMOLOOM_MUX_CACHE").filter(|p| !p.is_empty()) {
        Some(p) => expand_tilde(Path::new(&p)),
        None => cache_root().join("mux.json"),
    }
}

/// Cached agents and the cache age in seconds; `-1` when unusable.
/// Stale (older than `max_age`) gives no agents with the real age.
pub fn read(max_age: Option<f64>, now: f64) -> (Vec<Agent>, f64) {
    fs::read_to_string(path())
        .ok()
        .map_or((Vec::new(), -1.0), |text| parse(&text, max_age, now))
}

fn parse(text: &str, max_age: Option<f64>, now: f64) -> (Vec<Agent>, f64) {
    let unusable = (Vec::new(), -1.0);
    let Ok(raw) = serde_json::from_str::<Value>(text) else {
        return unusable;
    };
    let Some(obj) = raw.as_object() else {
        return unusable;
    };
    let stamp = match obj.get("time") {
        None => 0.0,
        Some(v) => match v.as_f64() {
            Some(t) => t,
            None => return unusable,
        },
    };
    let age = now - stamp;
    if !age.is_finite() || age < 0.0 {
        return unusable;
    }
    if max_age.is_some_and(|max| age > max) {
        return (Vec::new(), age);
    }
    let agents = match obj.get("agents") {
        None => Vec::new(),
        Some(Value::Array(items)) => items.iter().filter_map(Agent::from_value).collect(),
        Some(_) => return unusable,
    };
    (agents, age)
}

#[derive(Serialize)]
struct Payload<'a> {
    time: f64,
    agents: &'a [Agent],
}

/// Persist atomically via a unique temp file and rename. A failed write only
/// costs speed, never correctness, so errors are ignored and the temp file is
/// removed (a hard kill mid-write can still orphan one).
pub fn write(agents: &[Agent], now: f64) {
    let path = path();
    let _ = (|| -> anyhow::Result<()> {
        if let Some(dir) = path.parent() {
            fs::create_dir_all(dir)?;
        }
        let bytes = serde_json::to_vec(&Payload { time: now, agents })?;
        write_atomic_with(&path, &bytes, None, Some(0o666), &|| Ok(()))
    })();
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mux::model::Kind;

    const NOW: f64 = 1_000_000.0;

    fn doc(time: &str, agents: &str) -> String {
        format!(r#"{{"time": {time}, "agents": {agents}}}"#)
    }

    const ONE: &str = r#"[{"pane": "s:1.1", "session": "s", "kind": "codex"}]"#;

    #[test]
    fn fresh_cache_round_trips() {
        let mut a = Agent::new("s:1.1".into(), "s".into(), Kind::Codex);
        a.info = "⏵⏵ bypass".into();
        a.last_input_at = Some(12.5);
        let text = serde_json::to_string(&Payload {
            time: NOW - 5.0,
            agents: &[a.clone()],
        })
        .unwrap();
        let (agents, age) = parse(&text, Some(30.0), NOW);
        assert_eq!(age, 5.0);
        assert_eq!(agents, vec![a]);
    }

    #[test]
    fn stale_cache_reports_age() {
        let (agents, age) = parse(&doc("999900", ONE), Some(30.0), NOW);
        assert!(agents.is_empty());
        assert_eq!(age, 100.0);
        assert_eq!(parse(&doc("999900", ONE), None, NOW).0.len(), 1);
    }

    #[test]
    fn unusable_caches() {
        for text in [
            "",
            "{corrupt",
            "[]",
            &doc("NaN", ONE),
            &doc("2000000", ONE),
            &doc("\"x\"", ONE),
            &doc("999990", "{}"),
        ] {
            assert_eq!(parse(text, Some(30.0), NOW), (Vec::new(), -1.0), "{text}");
        }
    }

    #[test]
    fn invalid_agents_are_dropped() {
        let agents = r#"[{"pane": null, "session": "s", "kind": "claude"}, 3,
            {"pane": "a", "session": "s", "kind": "claude", "state": "bogus"},
            {"pane": "b", "session": "s", "kind": "claude", "last_input_at": -1},
            {"pane": "c", "session": "s", "kind": "claude", "extra": 5},
            {"pane": "d", "session": "s", "kind": "claude"}]"#;
        let (parsed, _) = parse(&doc("999990", agents), Some(30.0), NOW);
        assert_eq!(
            parsed.iter().map(|a| a.pane.as_str()).collect::<Vec<_>>(),
            vec!["d"]
        );
    }
}
