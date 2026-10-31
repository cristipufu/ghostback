#![allow(clippy::unwrap_used, clippy::expect_used)]

use ghostback::macos::{match_terminal, parse_inventory};
use ghostback::sessions::{Agent, Session, Store, hook_session, shell_quote};
use rusqlite::Connection;
use serde_json::json;
use std::fs;
use std::io::Write;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};
use tempfile::TempDir;
use uuid::Uuid;

fn session(root: &TempDir, key: &str, id: u128) -> Session {
    Session::new(
        key.into(),
        Agent::Claude,
        &Uuid::from_u128(id).to_string(),
        root.path().into(),
    )
    .unwrap()
}

fn store(root: &TempDir) -> Store {
    Store::new(root.path().join("state")).unwrap()
}

fn cli(root: &TempDir) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_ghostback"));
    command.arg("--state-dir").arg(root.path().join("state"));
    command
}

#[test]
fn separate_sessions_in_same_directory_keep_their_exact_ids() {
    let root = TempDir::new().unwrap();
    let db = store(&root);
    let (a, b) = (session(&root, "a", 1), session(&root, "b", 2));
    db.record(&a).unwrap();
    db.record(&b).unwrap();
    assert_eq!(db.list().unwrap(), vec![a, b]);
}

#[test]
fn conversation_switch_changes_only_its_terminal() {
    let root = TempDir::new().unwrap();
    let db = store(&root);
    let (a, b, next) = (
        session(&root, "a", 1),
        session(&root, "b", 2),
        session(&root, "a", 3),
    );
    db.record(&a).unwrap();
    db.record(&b).unwrap();
    db.record(&next).unwrap();
    assert_eq!(db.list().unwrap(), vec![next, b]);
}

#[test]
fn late_exit_cannot_erase_a_newer_conversation() {
    let root = TempDir::new().unwrap();
    let db = store(&root);
    let (old, current) = (session(&root, "a", 1), session(&root, "a", 2));
    db.record(&old).unwrap();
    db.record(&current).unwrap();
    assert!(!db.forget("a", &old.session_id.to_string()).unwrap());
    assert_eq!(db.list().unwrap(), vec![current.clone()]);
    assert!(db.forget("a", &current.session_id.to_string()).unwrap());
    assert!(db.list().unwrap().is_empty());
}

#[test]
fn concurrent_threads_do_not_lose_tabs() {
    let root = TempDir::new().unwrap();
    let sessions: Vec<_> = (1..=24)
        .map(|id| session(&root, &format!("tab-{id:02}"), id))
        .collect();
    let threads: Vec<_> = sessions
        .iter()
        .cloned()
        .map(|session| {
            let path = root.path().join("state");
            thread::spawn(move || Store::new(path).unwrap().record(&session).unwrap())
        })
        .collect();
    for task in threads {
        task.join().unwrap();
    }
    assert_eq!(store(&root).list().unwrap(), sessions);
}

#[test]
fn concurrent_processes_do_not_lose_tabs() {
    let root = TempDir::new().unwrap();
    let mut children = Vec::new();
    for id in 1..=8 {
        children.push(
            cli(&root)
                .args([
                    "record",
                    "--agent",
                    "codex",
                    "--session-id",
                    &Uuid::from_u128(id).to_string(),
                    "--terminal-key",
                    &format!("tab-{id}"),
                    "--cwd",
                ])
                .arg(root.path())
                .stdout(Stdio::null())
                .stderr(Stdio::piped())
                .spawn()
                .unwrap(),
        );
    }
    for child in children {
        let result = child.wait_with_output().unwrap();
        assert!(
            result.status.success(),
            "{}",
            String::from_utf8_lossy(&result.stderr)
        );
    }
    assert_eq!(store(&root).list().unwrap().len(), 8);
}

#[test]
fn crash_worker_process() {
    let Some(path) = std::env::var_os("GHOSTBACK_TEST_CRASH_DATABASE") else {
        return;
    };
    let db = Connection::open(path).unwrap();
    db.execute_batch("BEGIN IMMEDIATE; UPDATE sessions SET session_id='uncommitted';")
        .unwrap();
    // Exit skips Drop, simulating a process dying before SQLite commits.
    std::process::exit(73);
}

#[test]
fn crash_during_write_retains_last_committed_conversation() {
    let root = TempDir::new().unwrap();
    let db = store(&root);
    let original = session(&root, "a", 1);
    db.record(&original).unwrap();
    let output = Command::new(std::env::current_exe().unwrap())
        .args(["--exact", "crash_worker_process", "--nocapture"])
        .env("GHOSTBACK_TEST_CRASH_DATABASE", db.path())
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(73));
    assert_eq!(store(&root).list().unwrap(), vec![original]);
}

#[test]
fn reading_missing_state_does_not_create_files() {
    let root = TempDir::new().unwrap();
    let db = store(&root);
    assert!(db.list().unwrap().is_empty());
    assert!(!db.forget("a", &Uuid::from_u128(1).to_string()).unwrap());
    assert!(!db.directory.exists());
}

#[test]
fn state_survives_reopening_store() {
    let root = TempDir::new().unwrap();
    let original = session(&root, "a", 1);
    store(&root).record(&original).unwrap();
    assert_eq!(store(&root).list().unwrap(), vec![original]);
}

#[test]
fn all_agents_resume_by_explicit_id() {
    let root = TempDir::new().unwrap();
    for (agent, flag) in [
        (Agent::Agy, "--conversation"),
        (Agent::Codex, "resume"),
        (Agent::Claude, "--resume"),
        (Agent::Grok, "--resume"),
    ] {
        let mut saved = session(&root, "a", 1);
        saved.agent = agent;
        assert_eq!(
            saved.argv(),
            vec![
                agent.name().to_string(),
                flag.to_string(),
                saved.session_id.to_string()
            ]
        );
    }
    assert!(Agent::parse("unsupported").is_err());
}

#[test]
fn shell_sensitive_path_is_preserved_as_a_single_argument() {
    let root = TempDir::new().unwrap();
    let path = root
        .path()
        .join("a 'quote' $(touch should-not-run); project");
    fs::create_dir(&path).unwrap();
    let output = Command::new("/bin/sh")
        .current_dir(root.path())
        .args([
            "-c",
            &format!("cd -- {} && pwd", shell_quote(path.to_str().unwrap())),
        ])
        .output()
        .unwrap();
    assert!(output.status.success());
    assert_eq!(
        PathBuf::from(String::from_utf8(output.stdout).unwrap().trim())
            .canonicalize()
            .unwrap(),
        path.canonicalize().unwrap()
    );
    assert!(!root.path().join("should-not-run").exists());
}

#[test]
fn invalid_identity_and_control_characters_are_rejected() {
    for (key, id, cwd) in [
        ("", Uuid::from_u128(1).to_string(), "/tmp"),
        ("a", "latest".into(), "/tmp"),
        ("a\n", Uuid::from_u128(1).to_string(), "/tmp"),
        ("a", Uuid::from_u128(1).to_string(), "relative"),
        ("a", Uuid::from_u128(1).to_string(), "/tmp/one\ntwo"),
    ] {
        assert!(Session::new(key.into(), Agent::Claude, &id, cwd.into()).is_err());
    }
}

#[test]
fn native_hook_formats_preserve_exact_identity() {
    let id = Uuid::from_u128(1).to_string();
    for (agent, payload) in [
        (Agent::Claude, json!({"session_id": id, "cwd": "/tmp"})),
        (Agent::Codex, json!({"session_id": id, "cwd": "/tmp"})),
        (
            Agent::Agy,
            json!({"conversationId": id, "workspacePaths": ["/tmp"]}),
        ),
    ] {
        assert_eq!(
            hook_session(agent, payload, "a")
                .unwrap()
                .session_id
                .to_string(),
            id
        );
    }
}

#[test]
fn malformed_or_end_hooks_cannot_register_a_conversation() {
    for payload in [
        json!(null),
        json!([]),
        json!({}),
        json!({"session_id": 42, "cwd": "/tmp"}),
        json!({"session_id": Uuid::from_u128(1), "cwd": "/tmp", "hook_event_name": "SessionEnd"}),
    ] {
        assert!(hook_session(Agent::Claude, payload, "a").is_err());
    }
    assert!(
        hook_session(
            Agent::Agy,
            json!({"conversationId": Uuid::from_u128(1), "workspacePaths": []}),
            "a"
        )
        .is_err()
    );
    assert!(hook_session(Agent::Grok, json!({}), "a").is_err());
}

fn terminal_row(id: u128, tty: &str) -> serde_json::Value {
    json!({"terminal_key": Uuid::from_u128(id), "window_id": "w", "tab_id": "t", "tab_index": 1,
        "cwd": "/same", "tty": tty, "pid": 12})
}

#[test]
fn tty_bridge_matches_correct_terminal_in_same_directory() {
    let rows = parse_inventory(
        &json!([
            terminal_row(1, "/dev/ttys001"),
            terminal_row(2, "/dev/ttys002")
        ])
        .to_string(),
    )
    .unwrap();
    assert_eq!(
        match_terminal(&rows, "ttys002").unwrap(),
        Uuid::from_u128(2)
    );
    assert_eq!(
        match_terminal(&rows, "/dev/ttys001").unwrap(),
        Uuid::from_u128(1)
    );
    for tty in ["", "??", "ttys999", "/"] {
        assert!(match_terminal(&rows, tty).is_err());
    }
}

#[test]
fn duplicate_or_invalid_terminal_inventory_is_rejected() {
    let valid = terminal_row(1, "/dev/ttys001");
    for value in [
        json!(null),
        json!({}),
        json!([null]),
        json!([{}]),
        json!([valid, valid]),
        json!([{ "terminal_key": "invalid" }]),
    ] {
        assert!(parse_inventory(&value.to_string()).is_err());
    }
    for (key, value) in [
        ("pid", json!(-1)),
        ("pid", json!(true)),
        ("tty", json!(null)),
        ("tty", json!("relative")),
    ] {
        let mut row = valid.clone();
        row[key] = value;
        assert!(parse_inventory(&json!([row]).to_string()).is_err());
    }
    assert!(parse_inventory("not JSON").is_err());
}

#[test]
fn ambiguous_tty_mapping_is_rejected() {
    let rows = parse_inventory(
        &json!([
            terminal_row(1, "/dev/ttys001"),
            terminal_row(2, "/dev/ttys001")
        ])
        .to_string(),
    )
    .unwrap();
    assert!(match_terminal(&rows, "ttys001").is_err());
}

#[test]
fn corrupt_database_is_reported_and_left_intact() {
    let root = TempDir::new().unwrap();
    let db = store(&root);
    fs::create_dir(&db.directory).unwrap();
    let evidence = b"not SQLite\0recovery evidence";
    fs::write(db.path(), evidence).unwrap();
    let output = cli(&root).arg("plan").output().unwrap();
    assert!(!output.status.success());
    assert!(output.stdout.is_empty());
    assert_eq!(fs::read(db.path()).unwrap(), evidence);
}

#[test]
fn future_schema_is_not_modified_by_reads_or_writes() {
    let root = TempDir::new().unwrap();
    let db = store(&root);
    fs::create_dir(&db.directory).unwrap();
    {
        let connection = Connection::open(db.path()).unwrap();
        connection.pragma_update(None, "user_version", 99).unwrap();
    }
    let evidence = fs::read(db.path()).unwrap();
    assert!(db.list().is_err());
    assert!(db.record(&session(&root, "a", 1)).is_err());
    assert_eq!(fs::read(db.path()).unwrap(), evidence);
}

#[test]
fn non_directory_state_path_is_not_replaced() {
    let root = TempDir::new().unwrap();
    let db = store(&root);
    fs::write(&db.directory, b"original").unwrap();
    assert!(db.record(&session(&root, "a", 1)).is_err());
    assert_eq!(fs::read(db.directory).unwrap(), b"original");
}

#[test]
fn missing_project_directory_remains_in_the_plan() {
    let root = TempDir::new().unwrap();
    let db = store(&root);
    let mut saved = session(&root, "a", 1);
    saved.cwd = root.path().join("missing");
    db.record(&saved).unwrap();
    let output = cli(&root).args(["plan", "--json"]).output().unwrap();
    assert!(output.status.success());
    let value: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(value[0]["cwd_exists"], false);
    assert_eq!(db.list().unwrap(), vec![saved]);
}

#[test]
fn preview_produces_explicit_arguments_without_launching_agents() {
    let root = TempDir::new().unwrap();
    let saved = session(&root, "a", 1);
    store(&root).record(&saved).unwrap();
    let output = cli(&root)
        .args(["plan", "--json"])
        .env("PATH", root.path())
        .output()
        .unwrap();
    assert!(output.status.success());
    let value: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(value[0]["argv"], json!(saved.argv()));
}

#[test]
fn malformed_and_oversized_hooks_fail_without_writing_state() {
    let root = TempDir::new().unwrap();
    for payload in [
        Vec::new(),
        b"null".to_vec(),
        b"[]".to_vec(),
        b"not JSON".to_vec(),
        vec![b'['; 2000],
        vec![b' '; 1_048_577],
    ] {
        let mut child = cli(&root)
            .args(["hook", "claude"])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        let _ = child.stdin.take().unwrap().write_all(&payload);
        let output = child.wait_with_output().unwrap();
        assert!(!output.status.success());
        assert!(!String::from_utf8_lossy(&output.stderr).contains("panicked"));
    }
    assert!(!root.path().join("state").exists());
}

#[test]
fn malformed_saved_record_does_not_yield_partial_json() {
    let root = TempDir::new().unwrap();
    let db = store(&root);
    db.record(&session(&root, "a", 1)).unwrap();
    Connection::open(db.path())
        .unwrap()
        .execute("UPDATE sessions SET session_id='bad'", [])
        .unwrap();
    let output = cli(&root).args(["plan", "--json"]).output().unwrap();
    assert!(!output.status.success());
    assert!(output.stdout.is_empty());
}

#[test]
fn subprocess_failures_are_bounded_and_reported() {
    let missing =
        ghostback::process::output("/ghostback/no-such-binary", &[], Duration::from_secs(1));
    assert!(missing.is_err());
    let error = ghostback::process::output(
        "/bin/sh",
        &["-c", "printf denied >&2; exit 7"],
        Duration::from_secs(1),
    )
    .unwrap_err();
    assert!(error.to_string().contains("denied"));
    let start = Instant::now();
    assert!(ghostback::process::output("/bin/sleep", &["5"], Duration::from_millis(30)).is_err());
    assert!(start.elapsed() < Duration::from_secs(2));
}

#[test]
fn inherited_output_pipes_cannot_hang_the_cli() {
    let start = Instant::now();
    let result =
        ghostback::process::output("/bin/sh", &["-c", "sleep 0.3 &"], Duration::from_millis(50));
    assert!(result.is_err());
    assert!(start.elapsed() < Duration::from_secs(2));
}

#[test]
fn subprocess_output_limit_is_enforced() {
    assert!(
        ghostback::process::output(
            "/bin/sh",
            &["-c", "head -c 1100000 /dev/zero"],
            Duration::from_secs(5)
        )
        .is_err()
    );
}

#[test]
fn doctor_does_not_create_state() {
    let root = TempDir::new().unwrap();
    let output = cli(&root)
        .arg("doctor")
        .env("PATH", root.path())
        .output()
        .unwrap();
    assert!(output.status.success());
    let value: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert!(
        value["automatic_restore"]
            .as_str()
            .unwrap()
            .contains("available after")
    );
    assert!(!root.path().join("state").exists());
}

#[test]
fn relative_state_directory_is_normalized() {
    assert!(
        Store::new(PathBuf::from("relative"))
            .unwrap()
            .directory
            .is_absolute()
    );
}

#[test]
fn invalid_record_fails_before_creating_state() {
    let root = TempDir::new().unwrap();
    let output = cli(&root)
        .args([
            "record",
            "--agent",
            "claude",
            "--session-id",
            "latest",
            "--terminal-key",
            "a",
            "--cwd",
        ])
        .arg(root.path())
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(!root.path().join("state").exists());
}

#[test]
fn downstream_pipe_close_does_not_crash() {
    let root = TempDir::new().unwrap();
    let db = store(&root);
    for id in 1..=80 {
        db.record(&session(&root, &format!("tab-{id:03}"), id))
            .unwrap();
    }
    let mut child = cli(&root)
        .args(["plan", "--json"])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    drop(child.stdout.take());
    let output = child.wait_with_output().unwrap();
    assert!(output.status.success());
}

#[test]
fn native_uuid_casing_cannot_create_two_bindings() {
    let root = TempDir::new().unwrap();
    let db = store(&root);
    let upper = "B3604511-1F2D-40B0-88B8-F38C013DF18F";
    let first = session(&root, upper, 1);
    let second = session(&root, &upper.to_lowercase(), 2);
    db.record(&first).unwrap();
    db.record(&second).unwrap();
    assert_eq!(db.list().unwrap(), vec![second.clone()]);
    assert!(db.forget(upper, &second.session_id.to_string()).unwrap());
}

#[test]
fn empty_optional_environment_values_use_standard_state_location() {
    let root = TempDir::new().unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_ghostback"))
        .arg("doctor")
        .env("HOME", root.path())
        .env("PATH", root.path())
        .env("GHOSTBACK_STATE_DIR", "")
        .env("XDG_STATE_HOME", "")
        .output()
        .unwrap();
    assert!(output.status.success());
    let value: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    let expected = if cfg!(target_os = "macos") {
        root.path().join("Library/Application Support/Ghostback")
    } else {
        root.path().join(".local/state/ghostback")
    };
    assert_eq!(value["state_directory"], expected.to_str().unwrap());
    assert!(!expected.exists());
}

#[cfg(unix)]
#[test]
fn non_utf8_state_path_cannot_panic_in_diagnostics() {
    use std::os::unix::ffi::OsStringExt;
    let root = TempDir::new().unwrap();
    let path = root
        .path()
        .join(std::ffi::OsString::from_vec(vec![b'x', 0xff]));
    let output = Command::new(env!("CARGO_BIN_EXE_ghostback"))
        .arg("--state-dir")
        .arg(path)
        .arg("doctor")
        .env("PATH", root.path())
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(serde_json::from_slice::<serde_json::Value>(&output.stdout).is_ok());
}

#[cfg(unix)]
#[test]
fn codex_shim_preserves_help_arguments_and_child_exit_code() {
    use std::os::unix::fs::PermissionsExt;
    let root = TempDir::new().unwrap();
    let fake = root.path().join("fake-codex");
    fs::write(&fake, b"#!/bin/sh\nprintf '%s\\n' \"$@\"\nexit 37\n").unwrap();
    fs::set_permissions(&fake, fs::Permissions::from_mode(0o755)).unwrap();
    let result = cli(&root)
        .arg("run")
        .arg("--binary")
        .arg(&fake)
        .args(["--", "--help"])
        .output()
        .unwrap();
    assert_eq!(result.status.code(), Some(37));
    assert_eq!(result.stdout, b"--help\n");
}
#[cfg(unix)]
#[test]
fn codex_shim_refuses_recursive_launch() {
    use std::os::unix::fs::PermissionsExt;
    let root = TempDir::new().unwrap();
    let shim = store(&root).directory.join("shims/codex");
    fs::create_dir_all(shim.parent().unwrap()).unwrap();
    fs::write(&shim, b"#!/bin/sh\nexit 0\n").unwrap();
    fs::set_permissions(&shim, fs::Permissions::from_mode(0o755)).unwrap();
    let result = cli(&root)
        .arg("run")
        .arg("--binary")
        .arg(&shim)
        .args(["--", "--help"])
        .output()
        .unwrap();
    assert!(!result.status.success());
    assert!(String::from_utf8_lossy(&result.stderr).contains("cannot launch itself"));
}
