use std::os::unix::fs::symlink;

use serde_json::json;
use tempfile::TempDir;

use super::*;

const TIME: f64 = 1_000_000.0;

fn valid_value() -> Value {
    json!({
        "created_at": TIME,
        "transcript_path": "/temporary/session.jsonl",
        "opts": { "threshold": null, "trim_assistant": null, "tools": [] },
        "preview_tokens": 1234,
    })
}

fn plan() -> Plan {
    Plan::from_value(valid_value()).unwrap()
}

fn state(temp: &TempDir) -> State {
    State::from_cache_dir(Some(temp.path().join("cache")))
}

#[test]
fn session_ids_are_filename_safe_before_any_path_operation() {
    for id in ["a", "abc-123", "a.b_c-0"] {
        assert!(is_safe_session_id(id));
    }
    assert!(is_safe_session_id(&"a".repeat(200)));
    for id in [
        "",
        ".",
        "..",
        "../escape",
        "/absolute",
        "a/b",
        "a\\b",
        "a\0b",
        "-a",
        "_a",
        "a b",
        "a\nb",
        "\u{00e9}",
    ] {
        assert!(!is_safe_session_id(id), "{id:?}");
    }
    assert!(!is_safe_session_id(&"a".repeat(201)));
    let temp = TempDir::new().unwrap();
    let state = state(&temp);
    assert!(state.save("../escape", &plan()).is_err());
    assert!(!temp.path().join("cache").exists());
    assert!(state.load("../escape", TIME).is_none());
    state.clear("../escape");
}

#[test]
fn validates_hostile_plan_shapes() {
    for value in [
        Value::Null,
        json!([]),
        json!(true),
        json!("plan"),
        json!({}),
    ] {
        assert!(Plan::from_value(value).is_none());
    }
    for (key, value) in [
        ("created_at", Value::Null),
        ("created_at", json!(true)),
        ("created_at", json!("now")),
        ("created_at", json!(1e300)),
        ("created_at", json!(-1e300)),
        ("transcript_path", json!("")),
        ("transcript_path", json!(12)),
        ("transcript_path", Value::Null),
        ("opts", Value::Null),
        ("opts", json!([])),
        ("preview_tokens", json!(true)),
        ("preview_tokens", json!("many")),
        ("preview_tokens", json!(1e300)),
        ("preview_tokens", json!([])),
    ] {
        let mut hostile = valid_value();
        hostile[key] = value;
        assert!(Plan::from_value(hostile.clone()).is_none(), "{hostile}");
    }
    for key in ["created_at", "transcript_path", "opts"] {
        let mut hostile = valid_value();
        hostile.as_object_mut().unwrap().shift_remove(key);
        assert!(Plan::from_value(hostile).is_none(), "{key}");
    }
    for (key, value) in [
        ("threshold", json!(0)),
        ("threshold", json!(-1)),
        ("threshold", json!(true)),
        ("threshold", json!(500.0)),
        ("threshold", json!("500")),
        ("threshold", json!(1e300)),
        ("trim_assistant", json!(0)),
        ("trim_assistant", json!(false)),
        ("trim_assistant", json!(1.0)),
        ("trim_assistant", json!("20")),
        ("trim_assistant", json!([])),
        ("tools", Value::Null),
        ("tools", json!("bash")),
        ("tools", json!({})),
        ("tools", json!([1])),
        ("tools", json!([""])),
        ("tools", json!(["Bash"])),
        ("tools", json!(["--help"])),
        ("tools", json!(["1bash"])),
        ("tools", json!(["bash,read"])),
        ("tools", json!(["bash", "bash"])),
        ("tools", json!(["b\u{0301}"])),
        ("tools", json!(["a".repeat(129)])),
    ] {
        let mut hostile = valid_value();
        hostile["opts"][key] = value;
        assert!(Plan::from_value(hostile.clone()).is_none(), "{hostile}");
    }
    let mut missing_tools = valid_value();
    missing_tools["opts"]
        .as_object_mut()
        .unwrap()
        .shift_remove("tools");
    assert!(Plan::from_value(missing_tools).is_none());
}

#[test]
fn accepts_optional_and_numeric_shapes() {
    let mut value = valid_value();
    value["created_at"] = json!(TIME + 0.25);
    value["opts"] = json!({
        "threshold": 999_999_999_999u64,
        "trim_assistant": -20,
        "tools": ["bash", "read", "mcp__tool.run-v2", "b\u{00e9}"],
    });
    value["preview_tokens"] = json!(-123.5);
    value["ignored_extra_field"] = json!("extensions are permitted");
    assert!(Plan::from_value(value.clone()).is_some());
    value
        .as_object_mut()
        .unwrap()
        .shift_remove("preview_tokens");
    value["opts"]
        .as_object_mut()
        .unwrap()
        .shift_remove("threshold");
    value["opts"]
        .as_object_mut()
        .unwrap()
        .shift_remove("trim_assistant");
    assert!(Plan::from_value(value).is_some());
    assert!(finite_number(&json!(1_000_000_000_000_000u64)).is_some());
    assert!(finite_number(&json!(1_000_000_000_000_001u64)).is_none());
    for raw in [
        r#"{"created_at": NaN}"#,
        r#"{"created_at": Infinity}"#,
        r#"{"created_at": 1000000000000000000000000000000000000000000}"#,
    ] {
        assert!(
            serde_json::from_str::<Value>(raw)
                .ok()
                .and_then(Plan::from_value)
                .is_none()
        );
    }
}

#[test]
fn state_is_private_atomic_and_repairs_directory_permissions() {
    let temp = TempDir::new().unwrap();
    let state = state(&temp);
    let dir = state.dir.as_ref().unwrap();
    let namespace = dir.parent().unwrap();
    // The shared cache root keeps the user's mode.
    fs::create_dir_all(namespace).unwrap();
    fs::set_permissions(namespace, fs::Permissions::from_mode(0o755)).unwrap();
    state.save("session", &plan()).unwrap();
    let path = state.path("session").unwrap();
    assert_eq!(fs::metadata(dir).unwrap().mode() & 0o777, 0o700);
    assert_eq!(fs::metadata(namespace).unwrap().mode() & 0o777, 0o755);
    assert_eq!(fs::metadata(&path).unwrap().mode() & 0o777, 0o600);
    assert!(state.load("session", TIME).is_some());
    fs::set_permissions(dir, fs::Permissions::from_mode(0o777)).unwrap();
    assert!(state.load("session", TIME).is_some());
    assert_eq!(fs::metadata(dir).unwrap().mode() & 0o777, 0o700);
    assert_eq!(fs::metadata(namespace).unwrap().mode() & 0o777, 0o755);
    assert_eq!(fs::read_dir(dir).unwrap().count(), 1);
    fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).unwrap();
    state.save("session", &plan()).unwrap();
    assert_eq!(fs::metadata(&path).unwrap().mode() & 0o777, 0o600);
    state.clear("session");
    assert!(!path.exists());
}

#[test]
fn symlinked_cache_root_is_followed_but_its_mode_is_kept() {
    let temp = TempDir::new().unwrap();
    let state = state(&temp);
    let target = temp.path().join("real-cache");
    fs::create_dir(&target).unwrap();
    fs::set_permissions(&target, fs::Permissions::from_mode(0o755)).unwrap();
    let namespace = state.dir.as_ref().unwrap().parent().unwrap();
    fs::create_dir_all(namespace.parent().unwrap()).unwrap();
    symlink(&target, namespace).unwrap();
    state.save("session", &plan()).unwrap();
    assert!(state.load("session", TIME).is_some());
    assert_eq!(fs::metadata(&target).unwrap().mode() & 0o777, 0o755);
    assert_eq!(
        fs::symlink_metadata(target.join("hook")).unwrap().mode() & 0o777,
        0o700
    );
}

#[test]
fn rejects_state_directory_files_symlinks_and_foreign_owners() {
    let temp = TempDir::new().unwrap();
    let state = state(&temp);
    let target = temp.path().join("do-not-touch");
    fs::create_dir(&target).unwrap();
    fs::set_permissions(&target, fs::Permissions::from_mode(0o755)).unwrap();
    let namespace = state.dir.as_ref().unwrap().parent().unwrap();
    fs::create_dir_all(namespace).unwrap();
    let dir = state.dir.as_ref().unwrap();
    symlink(&target, dir).unwrap();
    let error = state.save("session", &plan()).unwrap_err();
    assert!(error.contains("symlink or file"), "{error}");
    assert!(state.load("session", TIME).is_none());
    state.clear("session");
    assert!(fs::symlink_metadata(dir).unwrap().file_type().is_symlink());
    fs::remove_file(dir).unwrap();
    fs::write(dir, "not a directory").unwrap();
    assert!(state.save("session", &plan()).is_err());
    assert_eq!(fs::read_to_string(dir).unwrap(), "not a directory");
    fs::remove_file(dir).unwrap();
    fs::create_dir(dir).unwrap();
    let error = secure_directory(dir, false, current_uid().wrapping_add(1)).unwrap_err();
    assert!(error.contains("owned by another user"), "{error}");
}

#[test]
fn save_errors_keep_the_root_cause() {
    let temp = TempDir::new().unwrap();
    let state = state(&temp);
    let dir = state.secure_dir(true).unwrap().to_path_buf();
    fs::set_permissions(&dir, fs::Permissions::from_mode(0o500)).unwrap();
    let error = state.save("session", &plan()).unwrap_err();
    fs::set_permissions(&dir, fs::Permissions::from_mode(0o700)).unwrap();
    assert!(error.contains("Permission denied"), "{error}");
}

#[test]
fn pending_symlink_is_never_followed_but_save_replaces_it() {
    let temp = TempDir::new().unwrap();
    let state = state(&temp);
    state.secure_dir(true).unwrap();
    let outside = temp.path().join("outside.json");
    fs::write(&outside, serde_json::to_vec(&valid_value()).unwrap()).unwrap();
    fs::set_permissions(&outside, fs::Permissions::from_mode(0o600)).unwrap();
    let original = fs::read(&outside).unwrap();
    let path = state.path("session").unwrap();
    symlink(&outside, &path).unwrap();
    assert!(state.load("session", TIME).is_none());
    state.clear("session");
    assert_eq!(fs::read(&outside).unwrap(), original);
    symlink(&outside, &path).unwrap();
    state.save("session", &plan()).unwrap();
    assert!(fs::symlink_metadata(path).unwrap().is_file());
    assert_eq!(fs::read(&outside).unwrap(), original);
    assert!(state.load("session", TIME).is_some());
}

#[test]
fn rejects_non_regular_and_public_plan_files() {
    let temp = TempDir::new().unwrap();
    let state = state(&temp);
    state.save("session", &plan()).unwrap();
    let path = state.path("session").unwrap();
    fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).unwrap();
    assert!(state.load("session", TIME).is_none());
    fs::remove_file(&path).unwrap();
    fs::create_dir(&path).unwrap();
    assert!(state.load("session", TIME).is_none());
}

#[test]
fn corrupt_expired_and_far_future_plans_cannot_arm_an_apply() {
    let temp = TempDir::new().unwrap();
    let state = state(&temp);
    for created_at in [TIME - 601.0, TIME + 61.0] {
        let mut plan = plan();
        plan.created_at = created_at;
        state.save("session", &plan).unwrap();
        assert!(state.load("session", TIME).is_none());
        assert!(!state.path("session").unwrap().exists());
    }
    for created_at in [TIME - 600.0, TIME + 60.0] {
        let mut plan = plan();
        plan.created_at = created_at;
        state.save("session", &plan).unwrap();
        assert!(state.load("session", TIME).is_some());
    }
    state.save("session", &plan()).unwrap();
    let path = state.path("session").unwrap();
    fs::write(&path, b"[]").unwrap();
    assert!(state.load("session", TIME).is_none());
    assert!(!path.exists());
    state.save("session", &plan()).unwrap();
    fs::write(&path, b"not json").unwrap();
    assert!(state.load("session", TIME).is_none());
    state.save("session", &plan()).unwrap();
    OpenOptions::new()
        .write(true)
        .open(&path)
        .unwrap()
        .set_len(MAX_PLAN_BYTES + 1)
        .unwrap();
    assert!(state.load("session", TIME).is_none());
    assert!(!path.exists());
}

#[test]
fn missing_cache_root_is_a_save_error_and_other_sessions_stay_isolated() {
    let state_without_cache = State::from_cache_dir(None);
    assert!(
        state_without_cache
            .save("session", &plan())
            .unwrap_err()
            .contains("Cannot determine a private trim state cache directory")
    );
    assert!(state_without_cache.load("session", TIME).is_none());
    state_without_cache.clear("session");
    let temp = TempDir::new().unwrap();
    let state = state(&temp);
    state.save("first", &plan()).unwrap();
    let mut second = plan();
    second.opts.threshold = Some(800);
    state.save("second", &second).unwrap();
    state.clear("first");
    assert!(state.load("first", TIME).is_none());
    assert_eq!(
        state.load("second", TIME).unwrap().opts.threshold,
        Some(800)
    );
    let saved: Value =
        serde_json::from_slice(&fs::read(state.path("second").unwrap()).unwrap()).unwrap();
    assert_eq!(saved["preview_tokens"].as_i64(), Some(1234));
}
