#![allow(clippy::unwrap_used, clippy::expect_used)]
use ghostback::agents::{interactive, replay_options};
use ghostback::bridges::merge_plugin;
use ghostback::lifecycle::{Capture, Decision};
use ghostback::sessions::{Agent, Session, SessionId, Store, hook_session};
use rusqlite::Connection;
use serde_json::json;
use std::fs;
use tempfile::TempDir;

fn pi(root: &TempDir, file: &str, id: &str) -> Session {
    let file = root.path().join(file);
    fs::write(
        &file,
        format!("{}\n", json!({"type":"session","id":id,"cwd":root.path()})),
    )
    .unwrap();
    hook_session(
        Agent::Pi,
        json!({"session_id":id,"cwd":root.path(),"session_file":file}),
        "terminal",
    )
    .unwrap()
}
fn capture(session: Session) -> Capture {
    Capture {
        session,
        shell_key: "100:1".into(),
        owner_key: "101:2".into(),
        executable: "/usr/bin/true".into(),
        options: vec![],
    }
}
fn args(values: &[&str]) -> Vec<String> {
    values.iter().map(|s| (*s).into()).collect()
}

#[test]
fn non_uuid_ids_preserve_case_and_reject_shell_syntax() {
    assert_eq!(
        SessionId::new(Agent::Pi, "Custom.Session-1_AbC")
            .unwrap()
            .as_str(),
        "Custom.Session-1_AbC"
    );
    assert!(SessionId::new(Agent::Opencode, "ses_AbC123").is_ok());
    for invalid in [
        "", "-latest", "ses_", "x;rm", "a/b", "a\n", "a b", ".a", "a.", "é",
    ] {
        assert!(SessionId::new(Agent::Opencode, invalid).is_err());
    }
    assert!(SessionId::new(Agent::Claude, "ses_AbC123").is_err());
}
#[test]
fn pi_requires_the_same_persistent_file_before_launch() {
    let root = TempDir::new().unwrap();
    let session = pi(&root, "a 'quoted' session.jsonl", "Custom-ID");
    assert!(session.validate_resume().is_ok());
    assert_eq!(
        session.argv()[2],
        session.session_file.as_ref().unwrap().to_str().unwrap()
    );
    assert!(session.command().unwrap().contains("'\"'\"'"));
    fs::write(
        session.session_file.as_ref().unwrap(),
        format!(
            "{}\n",
            json!({"type":"session","id":"Other-ID","cwd":root.path()})
        ),
    )
    .unwrap();
    assert!(session.validate_resume().is_err());
    fs::remove_file(session.session_file.as_ref().unwrap()).unwrap();
    assert!(session.validate_resume().is_err());
    assert!(
        hook_session(
            Agent::Pi,
            json!({"session_id":"Custom-ID","cwd":root.path()}),
            "t"
        )
        .is_err()
    );
}
#[test]
fn pi_headers_are_bounded_and_directory_is_part_of_identity() {
    let root = TempDir::new().unwrap();
    let session = pi(&root, "session.jsonl", "Custom-ID");
    fs::write(session.session_file.as_ref().unwrap(), " ".repeat(65537)).unwrap();
    assert!(session.validate_resume().is_err());
    fs::write(
        session.session_file.as_ref().unwrap(),
        format!(
            "{}\n",
            json!({"type":"session","id":"Custom-ID","cwd":"/other"})
        ),
    )
    .unwrap();
    assert!(session.validate_resume().is_err());
}
#[test]
fn same_pi_id_in_different_files_cannot_acknowledge_claim() {
    let root = TempDir::new().unwrap();
    let store = Store::new(root.path().join("state")).unwrap();
    let original = pi(&root, "original.jsonl", "Custom-ID");
    store.capture(&capture(original.clone())).unwrap();
    assert_eq!(
        store.prepare("terminal", "200:3").unwrap(),
        Decision::Launch
    );
    assert!(
        store
            .capture(&capture(pi(&root, "other.jsonl", "Custom-ID")))
            .is_err()
    );
    assert_eq!(store.list().unwrap(), vec![original]);
}
#[test]
fn late_ui_observations_cannot_revive_a_cleared_or_switched_session() {
    let root = TempDir::new().unwrap();
    let store = Store::new(root.path().join("state")).unwrap();
    let a = capture(
        Session::new(
            "terminal".into(),
            Agent::Opencode,
            "ses_ABC",
            root.path().into(),
        )
        .unwrap(),
    );
    store.capture_observed(&a, Some(10)).unwrap();
    store.clear_selection("terminal", &a.owner_key, 30).unwrap();
    store.capture_observed(&a, Some(20)).unwrap();
    assert_eq!(store.bindings().unwrap()[0].phase, "attention");
    let b = capture(
        Session::new(
            "terminal".into(),
            Agent::Opencode,
            "ses_DEF",
            root.path().into(),
        )
        .unwrap(),
    );
    store.capture_observed(&b, Some(40)).unwrap();
    store.clear_selection("terminal", &a.owner_key, 35).unwrap();
    store.capture_observed(&a, Some(15)).unwrap();
    assert_eq!(store.bindings().unwrap()[0].session, b.session);
    assert_eq!(store.bindings().unwrap()[0].phase, "active");
}
#[test]
fn opencode_and_pi_keep_only_replayable_options() {
    assert_eq!(
        replay_options(
            Agent::Pi,
            &args(&[
                "pi",
                "--provider",
                "anthropic",
                "--thinking",
                "low",
                "--session",
                "old.jsonl",
                "a prompt"
            ])
        )
        .unwrap(),
        args(&["--provider", "anthropic", "--thinking", "low"])
    );
    assert_eq!(
        replay_options(
            Agent::Opencode,
            &args(&[
                "opencode", "/project", "--agent", "build", "-s", "ses_OLD", "--fork", "--auto"
            ])
        )
        .unwrap(),
        args(&["--agent", "build", "--auto"])
    );
    for launch in [
        args(&["opencode", "attach", "http://localhost:1"]),
        args(&["opencode", "run", "prompt"]),
        args(&["opencode", "--pure"]),
        args(&["opencode", "--mini"]),
    ] {
        assert!(!interactive(Agent::Opencode, &launch));
    }
    assert!(interactive(
        Agent::Opencode,
        &args(&["opencode", "/project", "--agent", "agent"])
    ));
    for launch in [
        args(&["pi", "--mode", "rpc"]),
        args(&["pi", "--mode=json"]),
        args(&["pi", "-p", "prompt"]),
        args(&["pi", "--no-extensions"]),
    ] {
        assert!(!interactive(Agent::Pi, &launch));
    }
}
#[test]
fn opencode_plugin_edits_preserve_comments_and_user_plugins() {
    let original =
        "{\n  // keep this comment\n  \"theme\": \"dark\",\n  \"plugin\": [\"user-plugin\",],\n}\n";
    let first = merge_plugin(original, "file:///managed.mjs", false).unwrap();
    assert!(first.contains("// keep this comment"));
    assert!(first.contains("user-plugin"));
    assert_eq!(
        merge_plugin(&first, "file:///managed.mjs", false).unwrap(),
        first
    );
    let removed = merge_plugin(&first, "file:///managed.mjs", true).unwrap();
    assert!(removed.contains("// keep this comment"));
    assert!(removed.contains("user-plugin"));
    assert!(!removed.contains("managed.mjs"));
    assert!(merge_plugin(r#"{"plugin":{},"theme":"dark"}"#, "file:///managed", false).is_err());
    assert!(merge_plugin(r#"{"plugin":[],"plugin":[]}"#, "file:///managed", false).is_err());
}
#[test]
fn schema_two_migrates_without_changing_existing_conversations() {
    let root = TempDir::new().unwrap();
    let store = Store::new(root.path().into()).unwrap();
    let db = Connection::open(store.path()).unwrap();
    db.execute_batch("CREATE TABLE sessions (terminal_key TEXT PRIMARY KEY,agent TEXT NOT NULL,session_id TEXT NOT NULL,cwd TEXT NOT NULL,recorded_at TEXT NOT NULL); PRAGMA user_version=2;").unwrap();
    db.execute("INSERT INTO sessions VALUES ('old','claude','00000000-0000-0000-0000-000000000001',?1,'1')",[root.path().to_str().unwrap()]).unwrap();
    let previous = store.list().unwrap();
    let new = Session::new(
        "new".into(),
        Agent::Opencode,
        "ses_CustomID",
        root.path().into(),
    )
    .unwrap();
    store.record(&new).unwrap();
    assert!(store.list().unwrap().contains(&previous[0]));
    assert!(store.list().unwrap().contains(&new));
    assert_eq!(
        db.pragma_query_value(None, "user_version", |r| r.get::<_, u32>(0))
            .unwrap(),
        3
    );
    assert!(store.forget("new", "ses_CustomID").unwrap());
}

#[test]
fn watcher_failure_cannot_overwrite_a_newer_bridge_acknowledgement() {
    let root = TempDir::new().unwrap();
    let store = Store::new(root.path().join("state")).unwrap();
    let mut original = capture(
        Session::new(
            "terminal".into(),
            Agent::Opencode,
            "ses_A",
            root.path().into(),
        )
        .unwrap(),
    );
    store.capture(&original).unwrap();
    original.owner_key = "102:3".into();
    store.capture_observed(&original, Some(100)).unwrap();
    store
        .attention_if_unchanged("terminal", "101:2", "active", "old scan")
        .unwrap();
    assert_eq!(store.bindings().unwrap()[0].phase, "active");
    store.clear_selection("terminal", "102:3", 200).unwrap();
    store.capture_observed(&original, Some(300)).unwrap();
    store
        .attention_if_unchanged("terminal", "102:3", "attention", "old scan")
        .unwrap();
    assert_eq!(store.bindings().unwrap()[0].phase, "active");
}
#[cfg(unix)]
#[test]
fn pi_does_not_block_on_a_fifo_or_follow_a_replaced_symlink() {
    use std::os::unix::fs::symlink;
    let root = TempDir::new().unwrap();
    let session = pi(&root, "session.jsonl", "ID");
    let path = session.session_file.as_ref().unwrap();
    fs::remove_file(path).unwrap();
    let name = std::ffi::CString::new(path.to_str().unwrap()).unwrap();
    // SAFETY: NUL-terminated path within this test's temporary directory.
    assert_eq!(unsafe { libc::mkfifo(name.as_ptr(), 0o600) }, 0);
    assert!(session.validate_resume().is_err());
    fs::remove_file(path).unwrap();
    let other = pi(&root, "other.jsonl", "ID");
    symlink(other.session_file.unwrap(), path).unwrap();
    assert!(session.validate_resume().is_err());
}
