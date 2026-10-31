#![allow(clippy::unwrap_used, clippy::expect_used)]
use ghostback::lifecycle::{Capture, Decision};
use ghostback::sessions::{Agent, Session, Store};
use rusqlite::Connection;
use std::sync::{Arc, Barrier};
use tempfile::TempDir;
use uuid::Uuid;

fn fixture() -> (TempDir, Store, Capture) {
    let root = TempDir::new().unwrap();
    let store = Store::new(root.path().join("state")).unwrap();
    let capture = Capture {
        session: Session::new(
            "terminal".into(),
            Agent::Claude,
            &Uuid::from_u128(1).to_string(),
            root.path().into(),
        )
        .unwrap(),
        shell_key: "100:1000000".into(),
        owner_key: "101:2000000".into(),
        executable: "/usr/bin/true".into(),
        options: vec!["--model".into(), "test model".into()],
    };
    (root, store, capture)
}

#[test]
fn initial_capture_is_durable_and_visible() {
    let (_root, store, capture) = fixture();
    store.capture(&capture).unwrap();
    let rows = store.bindings().unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].phase, "active");
    assert_eq!(rows[0].session, capture.session);
}
#[test]
fn same_shell_prompt_retires_deliberately_exited_agent() {
    let (_root, store, capture) = fixture();
    store.capture(&capture).unwrap();
    assert_eq!(
        store.prepare("terminal", &capture.shell_key).unwrap(),
        Decision::Retired
    );
    assert_eq!(store.bindings().unwrap()[0].phase, "retired");
}
#[test]
fn new_shell_gets_one_exclusive_claim() {
    let (_root, store, capture) = fixture();
    store.capture(&capture).unwrap();
    assert_eq!(
        store.prepare("terminal", "200:3000000").unwrap(),
        Decision::Launch
    );
    assert_eq!(
        store.prepare("terminal", "200:3000000").unwrap(),
        Decision::Nothing
    );
}
#[test]
fn interrupted_claim_requires_explicit_retry() {
    let (_root, store, capture) = fixture();
    store.capture(&capture).unwrap();
    store.prepare("terminal", "200:3000000").unwrap();
    assert_eq!(
        store.prepare("terminal", "201:4000000").unwrap(),
        Decision::Nothing
    );
    assert_eq!(store.bindings().unwrap()[0].phase, "attention");
    assert_eq!(store.retry(None).unwrap(), 1);
    assert_eq!(
        store.prepare("terminal", "201:4000000").unwrap(),
        Decision::Launch
    );
}
#[test]
fn wrong_session_acknowledgement_never_overwrites_saved_identity() {
    let (_root, store, mut capture) = fixture();
    store.capture(&capture).unwrap();
    store.prepare("terminal", "200:3000000").unwrap();
    let original = capture.session.session_id;
    capture.session.session_id = Uuid::from_u128(2);
    assert!(store.capture(&capture).is_err());
    assert!(store.capture(&capture).is_err());
    assert_eq!(store.list().unwrap()[0].session_id, original);
    assert_eq!(store.bindings().unwrap()[0].phase, "attention");
}
#[test]
fn correct_acknowledgement_marks_the_new_process_active() {
    let (_root, store, mut capture) = fixture();
    store.capture(&capture).unwrap();
    store.prepare("terminal", "200:3000000").unwrap();
    capture.shell_key = "200:3000000".into();
    capture.owner_key = "202:4000000".into();
    store.capture(&capture).unwrap();
    assert_eq!(store.bindings().unwrap()[0].phase, "active");
    assert_eq!(store.bindings().unwrap()[0].owner_key, capture.owner_key);
}
#[test]
fn concurrent_ready_events_cannot_claim_twice() {
    let (_root, store, capture) = fixture();
    store.capture(&capture).unwrap();
    let barrier = Arc::new(Barrier::new(8));
    let tasks: Vec<_> = (0..8)
        .map(|_| {
            let root = store.directory.clone();
            let barrier = barrier.clone();
            std::thread::spawn(move || {
                barrier.wait();
                Store::new(root)
                    .unwrap()
                    .prepare("terminal", "200:3000000")
                    .unwrap()
            })
        })
        .collect();
    assert_eq!(
        tasks
            .into_iter()
            .map(|t| t.join().unwrap())
            .filter(|decision| *decision == Decision::Launch)
            .count(),
        1
    );
}
#[test]
fn unknown_lifecycle_state_blocks_recovery_instead_of_launching() {
    let (_root, store, capture) = fixture();
    store.capture(&capture).unwrap();
    Connection::open(store.path())
        .unwrap()
        .execute("UPDATE runtime SET phase='corrupt'", [])
        .unwrap();
    assert!(store.bindings().is_err());
    assert!(store.prepare("terminal", "200:3000000").is_err());
}
#[test]
fn failed_sessions_do_not_retry_on_every_prompt() {
    let (_root, store, capture) = fixture();
    store.capture(&capture).unwrap();
    store.attention("terminal", "sign-in needed").unwrap();
    assert_eq!(
        store.prepare("terminal", "200:3000000").unwrap(),
        Decision::Nothing
    );
    assert_eq!(store.retry(Some("other")).unwrap(), 0);
    assert_eq!(store.retry(Some("terminal")).unwrap(), 1);
    assert_eq!(
        store.prepare("terminal", "200:3000000").unwrap(),
        Decision::Launch
    );
}
#[test]
fn retired_tabs_are_not_rearmed_by_retry_all() {
    let (_root, store, capture) = fixture();
    store.capture(&capture).unwrap();
    store.retire("terminal", "closed").unwrap();
    assert_eq!(store.retry(None).unwrap(), 0);
    assert_eq!(
        store.prepare("terminal", "200:3000000").unwrap(),
        Decision::Nothing
    );
}
#[test]
fn edited_options_with_control_characters_fail_closed() {
    let (_root, store, capture) = fixture();
    store.capture(&capture).unwrap();
    Connection::open(store.path())
        .unwrap()
        .execute(
            "UPDATE runtime SET options_json=?1",
            ["[\"--model\",\"bad\\nvalue\"]"],
        )
        .unwrap();
    assert!(store.bindings().is_err());
}

#[test]
fn completed_claim_does_not_block_a_later_conversation_switch() {
    let (_root, store, mut capture) = fixture();
    store.capture(&capture).unwrap();
    store.prepare("terminal", "200:3000000").unwrap();
    capture.shell_key = "200:3000000".into();
    capture.owner_key = "202:4000000".into();
    store.capture(&capture).unwrap();
    assert!(store.bindings().unwrap()[0].claim_shell.is_empty());
    store
        .attention("terminal", "transient metadata unavailable")
        .unwrap();
    capture.session.session_id = Uuid::from_u128(2);
    store.capture(&capture).unwrap();
    assert_eq!(store.list().unwrap()[0].session_id, Uuid::from_u128(2));
}
#[test]
fn delayed_observation_cannot_revive_a_deliberately_retired_process() {
    let (_root, store, mut capture) = fixture();
    store.capture(&capture).unwrap();
    store.retire("terminal", "exited").unwrap();
    store.capture(&capture).unwrap();
    assert_eq!(store.bindings().unwrap()[0].phase, "retired");
    capture.owner_key = "205:5000000".into();
    store.capture(&capture).unwrap();
    assert_eq!(store.bindings().unwrap()[0].phase, "active");
}

#[test]
fn watcher_retires_a_closed_terminal_once_after_two_stable_observations() {
    use ghostback::app::WatchState;
    let mut watcher = WatchState::default();
    let generation = Some("1:100".into());
    assert!(
        watcher
            .closed_terminals(generation.clone(), ["a".into()].into())
            .is_empty()
    );
    assert!(
        watcher
            .closed_terminals(generation.clone(), Default::default())
            .is_empty()
    );
    assert_eq!(
        watcher.closed_terminals(generation.clone(), Default::default()),
        ["a"]
    );
    assert!(
        watcher
            .closed_terminals(generation, Default::default())
            .is_empty()
    );
}
#[test]
fn watcher_preserves_missing_bindings_across_ghostty_restart_or_unknown_generation() {
    use ghostback::app::WatchState;
    let mut watcher = WatchState::default();
    watcher.closed_terminals(Some("1:100".into()), ["a".into()].into());
    assert!(
        watcher
            .closed_terminals(Some("2:200".into()), Default::default())
            .is_empty()
    );
    assert!(
        watcher
            .closed_terminals(Some("2:200".into()), Default::default())
            .is_empty()
    );
    watcher.closed_terminals(Some("2:200".into()), ["a".into()].into());
    assert!(
        watcher
            .closed_terminals(None, Default::default())
            .is_empty()
    );
    assert!(
        watcher
            .closed_terminals(None, Default::default())
            .is_empty()
    );
}

#[test]
fn runtime_identity_is_canonical_even_for_direct_struct_construction() {
    let (_root, store, mut capture) = fixture();
    let key = Uuid::from_u128(0xabcd);
    capture.session.terminal_key = key.to_string().to_uppercase();
    store.capture(&capture).unwrap();
    assert_eq!(
        store.bindings().unwrap()[0].session.terminal_key,
        key.to_string()
    );
    store.attention(&key.to_string(), "retry").unwrap();
    assert_eq!(
        store.retry(Some(&key.to_string().to_uppercase())).unwrap(),
        1
    );
}
