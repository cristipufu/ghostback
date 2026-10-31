#![allow(clippy::unwrap_used, clippy::expect_used)]
use ghostback::capture::{agy_selection, registry_session, replay_options};
use ghostback::native::{Process, parse_argv};
use ghostback::sessions::Agent;
use serde_json::json;
use uuid::Uuid;
fn owner() -> Process {
    Process {
        pid: 12,
        parent: 10,
        started_us: 20_000_001,
        name: "grok".into(),
        executable: "/bin/grok".into(),
        argv: vec!["grok".into()],
    }
}
#[test]
fn grok_requires_both_pid_and_birth_time() {
    let p = owner();
    let id = Uuid::from_u128(1);
    let value = json!([{"pid":12,"pid_start":p.started_us,"session_id":id,"cwd":"/tmp"}]);
    assert_eq!(
        registry_session(Agent::Grok, &value, &p)
            .unwrap()
            .unwrap()
            .0,
        id
    );
    let mut recycled = p;
    recycled.started_us += 1;
    assert!(
        registry_session(Agent::Grok, &value, &recycled)
            .unwrap()
            .is_none()
    );
}
#[test]
fn ambiguous_grok_registry_is_not_guessed() {
    let p = owner();
    let row =
        json!({"pid":12,"pid_start":p.started_us,"session_id":Uuid::from_u128(1),"cwd":"/tmp"});
    assert!(registry_session(Agent::Grok, &json!([row, row]), &p).is_err());
}
#[test]
fn claude_recycled_pid_is_rejected() {
    let p = owner();
    let value = json!({"pid":12,"startedAt":1,"sessionId":Uuid::from_u128(1),"cwd":"/tmp"});
    assert!(
        registry_session(Agent::Claude, &value, &p)
            .unwrap()
            .is_none()
    );
}
#[test]
fn agy_tracks_switches_and_clears_new_empty_conversations() {
    let a = Uuid::from_u128(1);
    let b = Uuid::from_u128(2);
    let log = format!(
        "I1031 12:00:00.000000 123 file.go:1] Streaming conversation {a}\nI1031 12:00:00.000000 123 file.go:2] Starting conversation update stream for {b}\n"
    );
    assert_eq!(agy_selection(&log).unwrap(), Some(b));
    assert_eq!(
        agy_selection(
            &(log + "I1031 12:00:00.000000 123 ui.go:1] Full redraw completed (state) for conversation (epoch 2)\n")
        )
        .unwrap(),
        None
    );
}
#[test]
fn agy_does_not_parse_a_user_message_as_a_selection() {
    let id = Uuid::from_u128(1);
    assert_eq!(
        agy_selection(&format!("message: Streaming conversation {id}")).unwrap(),
        None
    );
    assert!(
        agy_selection("I1031 12:00:00.000000 123 file.go:1] Streaming conversation invalid")
            .is_err()
    );
}
#[test]
fn native_argument_parser_preserves_spaces_and_never_returns_environment() {
    let mut bytes = 3_i32.to_ne_bytes().to_vec();
    bytes.extend_from_slice(b"/bin/codex\0\0codex\0--model\0model with spaces\0API_KEY=secret\0");
    assert_eq!(
        parse_argv(&bytes).unwrap(),
        ["codex", "--model", "model with spaces"]
    );
}
#[test]
fn malformed_native_arguments_return_errors() {
    for bytes in [
        vec![],
        vec![1],
        (-1_i32).to_ne_bytes().to_vec(),
        [1_i32.to_ne_bytes().as_slice(), b"exe\0\0unterminated"].concat(),
    ] {
        assert!(parse_argv(&bytes).is_err());
    }
}
#[test]
fn replay_never_resubmits_initial_prompts() {
    let args = [
        "agy",
        "--model",
        "test model",
        "--prompt-interactive",
        "do work",
        "--dangerously-skip-permissions",
    ]
    .map(str::to_owned);
    assert_eq!(
        replay_options(Agent::Agy, &args).unwrap(),
        ["--model", "test model"]
    );
}
#[test]
fn replay_strips_old_resume_ids() {
    let args = [
        "codex",
        "--model",
        "m",
        "resume",
        "00000000-0000-0000-0000-000000000001",
    ]
    .map(str::to_owned);
    assert_eq!(
        replay_options(Agent::Codex, &args).unwrap(),
        ["--model", "m"]
    );
}
#[test]
fn parser_fuzz_inputs_do_not_panic() {
    let mut seed = 7u64;
    for length in 0..1024 {
        let mut input = vec![0; length];
        for byte in &mut input {
            seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1);
            *byte = (seed >> 32) as u8;
        }
        let _ = parse_argv(&input);
        let text = String::from_utf8_lossy(&input);
        let _ = ghostback::macos::parse_inventory(&text);
        let _ = agy_selection(&text);
    }
}

#[test]
fn agy_initial_blank_redraw_is_explicitly_cleared() {
    use ghostback::capture::{Selection, parse_agy_selection};
    let log = "I1031 12:14:49.859235     407 manager.go:1017] Full redraw completed (RerenderAll) for conversation  (epoch 0, items 1) in 0s\n";
    assert_eq!(parse_agy_selection(log).unwrap(), Selection::Cleared);
    assert_eq!(
        parse_agy_selection("nothing selected yet").unwrap(),
        Selection::Unseen
    );
}
#[test]
fn agy_quoted_log_messages_cannot_change_identity() {
    let id = Uuid::from_u128(1);
    let log =
        format!("I1031 12:00:00.000000 123 ui.go:1] user message: ] Streaming conversation {id}\n");
    assert_eq!(agy_selection(&log).unwrap(), None);
}
#[test]
fn agy_incremental_log_handles_partial_writes_truncation_and_rotation() {
    use ghostback::capture::{Cache, Selection};
    use std::io::Write;
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("cli-test.log");
    let a = Uuid::from_u128(1);
    let b = Uuid::from_u128(2);
    let line = |id| format!("I1031 12:00:00.000000 123 ui.go:1] Streaming conversation {id}\n");
    std::fs::write(&path, line(a)).unwrap();
    let mut cache = Cache::default();
    assert_eq!(
        cache.agy_log(&owner(), &path).unwrap(),
        Selection::Selected(a)
    );
    let second = line(b);
    let mut file = std::fs::OpenOptions::new()
        .append(true)
        .open(&path)
        .unwrap();
    file.write_all(&second.as_bytes()[..20]).unwrap();
    assert_eq!(
        cache.agy_log(&owner(), &path).unwrap(),
        Selection::Selected(a)
    );
    file.write_all(&second.as_bytes()[20..]).unwrap();
    assert_eq!(
        cache.agy_log(&owner(), &path).unwrap(),
        Selection::Selected(b)
    );
    std::fs::write(&path, "I1031 12:00:00.000000 123 ui.go:1] Full redraw completed (all) for conversation  (epoch 0)\n").unwrap();
    assert_eq!(cache.agy_log(&owner(), &path).unwrap(), Selection::Cleared);
    std::fs::rename(&path, temp.path().join("old.log")).unwrap();
    std::fs::write(&path, line(a)).unwrap();
    assert_eq!(
        cache.agy_log(&owner(), &path).unwrap(),
        Selection::Selected(a)
    );
}
#[test]
fn agent_option_policies_keep_boolean_sandbox_and_codex_profile() {
    let agy = ["agy", "--sandbox", "--model", "m"].map(str::to_owned);
    assert_eq!(
        replay_options(Agent::Agy, &agy).unwrap(),
        ["--sandbox", "--model", "m"]
    );
    let codex = ["codex", "-p", "personal", "--model", "m"].map(str::to_owned);
    assert_eq!(
        replay_options(Agent::Codex, &codex).unwrap(),
        ["-p", "personal", "--model", "m"]
    );
    assert!(
        ghostback::agents::validate_options(Agent::Codex, &["execute this prompt".into()]).is_err()
    );
    assert!(ghostback::agents::validate_options(Agent::Claude, &["--print".into()]).is_err());
}
