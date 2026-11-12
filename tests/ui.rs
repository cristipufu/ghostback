#![allow(clippy::unwrap_used, clippy::expect_used)]

use ghostback::lifecycle::Binding;
use ghostback::macos::{SpawnDisposition, Terminal, parse_inventory};
use ghostback::sessions::{Agent, Session, Store};
use ghostback::ui::{
    AppState, Canvas, GitInfo, HitTarget, KeyEvent, Modal, MouseEvent, MouseKind, SidebarGroup,
    UiEffect, ViewMode, build_snapshot_from_parts, demo_snapshot, parse_git_head,
    parse_input_events, render_app, resolve_git_info,
};
use std::collections::BTreeMap;
use std::fs;
use std::path::PathBuf;
use std::time::Duration;
use tempfile::TempDir;
use uuid::Uuid;

fn sample_terminal(
    id: u128,
    win: &str,
    tab: &str,
    tab_idx: u32,
    cwd: PathBuf,
    focused: bool,
) -> Terminal {
    Terminal {
        terminal_key: Uuid::from_u128(id),
        window_id: win.into(),
        tab_id: tab.into(),
        tab_index: tab_idx,
        cwd,
        tty: format!("/dev/ttys00{id}"),
        pid: 999_990 + id as u32,
        window_title: "Ghostty".into(),
        tab_title: format!("Tab {tab_idx}"),
        tab_selected: focused,
        terminal_title: String::new(),
        focused,
    }
}

fn sample_binding(
    id: u128,
    agent: Agent,
    session_id: &str,
    cwd: PathBuf,
    phase: &str,
    reason: &str,
) -> Binding {
    Binding {
        session: Session::new(Uuid::from_u128(id).to_string(), agent, session_id, cwd).unwrap(),
        shell_key: "100:1".into(),
        owner_key: "999999:1".into(),
        executable: PathBuf::from("/usr/bin/true"),
        options: Vec::new(),
        phase: phase.into(),
        reason: reason.into(),
        claim_shell: String::new(),
    }
}

#[test]
fn git_head_and_worktree_resolution_are_bounded_and_accurate() {
    assert_eq!(
        parse_git_head("ref: refs/heads/main\n"),
        Some("main".into())
    );
    assert_eq!(
        parse_git_head("ref: refs/heads/feat/herdr-ui\n"),
        Some("feat/herdr-ui".into())
    );
    assert_eq!(
        parse_git_head("a98ba35171464e19a3db3d1a3a60426e\n"),
        Some("a98ba35".into())
    );

    let root = TempDir::new().unwrap();
    let repo = root.path().join("repo");
    fs::create_dir_all(repo.join(".git")).unwrap();
    fs::write(repo.join(".git/HEAD"), "ref: refs/heads/main\n").unwrap();
    let nested = repo.join("src/ui");
    fs::create_dir_all(&nested).unwrap();
    assert_eq!(
        resolve_git_info(&nested),
        Some(GitInfo {
            branch: "main".into(),
            is_worktree: false
        })
    );

    let wt = root.path().join("wt-feature");
    let gitdir = root.path().join("gitdir-wt");
    fs::create_dir_all(&wt).unwrap();
    fs::create_dir_all(&gitdir).unwrap();
    fs::write(wt.join(".git"), format!("gitdir: {}\n", gitdir.display())).unwrap();
    fs::write(gitdir.join("HEAD"), "ref: refs/heads/feat/sidebar\n").unwrap();
    assert_eq!(
        resolve_git_info(&wt),
        Some(GitInfo {
            branch: "feat/sidebar".into(),
            is_worktree: true
        })
    );
}

#[test]
fn enriched_inventory_sanitizes_control_chars_and_stays_backwards_compatible() {
    let id = Uuid::from_u128(42);
    let legacy = format!(
        r#"[{{"terminal_key":"{id}","window_id":"w1","tab_id":"t1","tab_index":1,"cwd":"/tmp","tty":"/dev/ttys001","pid":123}}]"#
    );
    let rows = parse_inventory(&legacy).unwrap();
    assert_eq!(rows.len(), 1);
    assert!(!rows[0].focused);
    assert_eq!(rows[0].terminal_title, "");

    let enriched = format!(
        r#"[{{"terminal_key":"{id}","window_id":"w1","tab_id":"t1","tab_index":1,"cwd":"/tmp","tty":"/dev/ttys001","pid":123,"window_title":"Win\u0007","tab_title":"Tab\u001b","terminal_title":"claude\n","tab_selected":true,"focused":true}}]"#
    );
    let rows = parse_inventory(&enriched).unwrap();
    assert!(rows[0].focused);
    assert_eq!(rows[0].window_title, "Win");
    assert_eq!(rows[0].tab_title, "Tab");
    assert_eq!(rows[0].terminal_title, "claude");
}

#[test]
fn snapshot_combines_live_terminals_and_saved_bindings_across_all_views() {
    let root = TempDir::new().unwrap();
    let proj_a = root.path().join("ghostback");
    let proj_b = root.path().join("api-server");
    fs::create_dir_all(proj_a.join(".git")).unwrap();
    fs::write(proj_a.join(".git/HEAD"), "ref: refs/heads/main\n").unwrap();
    fs::create_dir_all(proj_b.join(".git")).unwrap();
    fs::write(proj_b.join(".git/HEAD"), "ref: refs/heads/feat/auth\n").unwrap();

    let terminals = vec![
        sample_terminal(1, "w1", "t1", 1, proj_a.clone(), true),
        sample_terminal(2, "w1", "t1", 1, proj_a.clone(), false),
        sample_terminal(3, "w1", "t2", 2, proj_b.clone(), false),
    ];
    let bindings = vec![
        sample_binding(
            1,
            Agent::Claude,
            &Uuid::from_u128(101).to_string(),
            proj_a.clone(),
            "active",
            "",
        ),
        sample_binding(
            2,
            Agent::Codex,
            &Uuid::from_u128(102).to_string(),
            proj_a.clone(),
            "attention",
            "Restoration was interrupted. Run ghostback retry.",
        ),
        sample_binding(
            4,
            Agent::Opencode,
            "ses_offline12345",
            proj_b.clone(),
            "retired",
            "Terminal was deliberately closed.",
        ),
    ];
    let mut health = BTreeMap::new();
    health.insert("watcher".into(), "running".into());

    let snap = build_snapshot_from_parts(terminals, bindings, &health, None, true, None);
    assert_eq!(snap.items.len(), 4);
    assert_eq!(snap.attention_count, 1);
    assert_eq!(snap.saved_count, 1);

    let mut state = AppState::new(snap, false, None);

    // Render Board view
    let mut board_canvas = Canvas::new(120, 28);
    render_app(&state, &mut board_canvas);
    let board_text = board_canvas.render_snapshot(false);
    assert!(board_text.contains("GHOSTBACK HERD"));
    assert!(board_text.contains("WORKING"));
    assert!(board_text.contains("WAITING FOR INPUT"));
    assert!(board_text.contains("NEEDS ATTENTION"));
    assert!(board_text.contains("INSPECTOR"));
    assert!(board_text.contains("ghostback"));
    assert!(board_text.contains("W1·T1·S1/2"));

    // Switch to Fleet view via key '2'
    state.handle_key(KeyEvent::Char('2'), 120);
    assert_eq!(state.view, ViewMode::Fleet);
    let mut fleet_canvas = Canvas::new(120, 28);
    render_app(&state, &mut fleet_canvas);
    let fleet_text = fleet_canvas.render_snapshot(false);
    assert!(fleet_text.contains("CONVERSATION ID"));
    assert!(fleet_text.contains("CODEX"));
    assert!(fleet_text.contains("OPENCODE"));

    // Switch to Sidebar view via key '3' and toggle grouping with 'g'
    state.handle_key(KeyEvent::Char('3'), 44);
    assert_eq!(state.view, ViewMode::Sidebar);
    let mut side_canvas = Canvas::new(44, 24);
    render_app(&state, &mut side_canvas);
    let side_text = side_canvas.render_snapshot(false);
    assert!(side_text.contains("SIDEBAR · Group: Status"));

    state.handle_key(KeyEvent::Char('g'), 44);
    assert_eq!(state.sidebar_group, SidebarGroup::Project);
    let mut side_proj_canvas = Canvas::new(44, 24);
    render_app(&state, &mut side_proj_canvas);
    let side_proj_text = side_proj_canvas.render_snapshot(false);
    assert!(side_proj_text.contains("SIDEBAR · Group: Project"));
    assert!(side_proj_text.contains("▾ ghostback"));
}

#[test]
fn keyboard_modals_and_sgr_mouse_events_drive_effects_cleanly() {
    let root = TempDir::new().unwrap();
    let cwd = root.path().to_path_buf();
    let terminals = vec![sample_terminal(1, "w1", "t1", 1, cwd.clone(), true)];
    let bindings = vec![sample_binding(
        1,
        Agent::Claude,
        &Uuid::from_u128(500).to_string(),
        cwd,
        "attention",
        "Interrupted",
    )];
    let snap = build_snapshot_from_parts(terminals, bindings, &BTreeMap::new(), None, true, None);
    let mut state = AppState::new(snap, false, None);

    // Pressing Enter on a live terminal produces Focus(Uuid)
    let eff = state.handle_key(KeyEvent::Enter, 120);
    assert!(matches!(eff, UiEffect::Focus(id) if id == Uuid::from_u128(1)));

    // Pressing 't' opens RenameTitle modal, typing and pressing Enter produces RenameTitle effect
    state.handle_key(KeyEvent::Char('t'), 120);
    assert!(matches!(state.modal, Modal::RenameTitle { .. }));
    state.handle_key(KeyEvent::Char('\u{15}'), 120); // Ctrl+U clears buffer
    for ch in "Auth Refactor".chars() {
        state.handle_key(KeyEvent::Char(ch), 120);
    }
    let eff = state.handle_key(KeyEvent::Enter, 120);
    assert!(
        matches!(eff, UiEffect::RenameTitle(id, title) if id == Uuid::from_u128(1) && title == "Auth Refactor")
    );

    // Pressing 'n' opens Spawn modal, Tab switches to SplitDown, '2' selects Claude, Enter spawns
    state.handle_key(KeyEvent::Char('n'), 120);
    assert!(matches!(state.modal, Modal::Spawn { .. }));
    state.handle_key(KeyEvent::Tab, 120);
    state.handle_key(KeyEvent::Char('2'), 120);
    let eff = state.handle_key(KeyEvent::Enter, 120);
    assert!(matches!(
        eff,
        UiEffect::Spawn {
            disposition: SpawnDisposition::SplitDown,
            command: Some(_),
            ..
        }
    ));

    // Parse SGR mouse and arrow sequences
    let (keys, mice) = parse_input_events(b"\x1b[A\x1b[B\x1b[<0;15;2M\x1b[<65;10;10M");
    assert_eq!(keys, vec![KeyEvent::Up, KeyEvent::Down]);
    assert_eq!(
        mice,
        vec![
            MouseEvent {
                kind: MouseKind::LeftDown,
                col: 14,
                row: 1
            },
            MouseEvent {
                kind: MouseKind::ScrollDown,
                col: 9,
                row: 9
            }
        ]
    );

    // Verify hit-testing on rendered canvas
    let mut canvas = Canvas::new(120, 28);
    render_app(&state, &mut canvas);
    let focus_hit = canvas
        .hits
        .iter()
        .find(|(_, target)| matches!(target, HitTarget::FocusCard(0)))
        .map(|(rect, _)| (rect.x + 1, rect.y))
        .unwrap();
    let eff = state.handle_mouse(
        MouseEvent {
            kind: MouseKind::LeftDown,
            col: focus_hit.0,
            row: focus_hit.1,
        },
        &canvas,
    );
    assert!(matches!(eff, UiEffect::Focus(id) if id == Uuid::from_u128(1)));
}

#[test]
fn escape_and_truncated_or_extended_terminal_sequences_never_quit_sidebar() {
    let root = TempDir::new().unwrap();
    let cwd = root.path().to_path_buf();
    let terminals = vec![sample_terminal(1, "w1", "t1", 1, cwd, true)];
    let snap = build_snapshot_from_parts(terminals, Vec::new(), &BTreeMap::new(), None, true, None);
    let mut state = AppState::new(snap, true, None);

    // SS3 arrows, modified CSI arrows, PageUp/PageDown, focus-in/out, and a truncated SGR mouse tail
    let stream = b"\x1bOA\x1bOB\x1b[1;2C\x1b[5~\x1b[6~\x1b[I\x1b[O\x1b[<35;12;1";
    let (keys, mice) = parse_input_events(stream);
    assert_eq!(
        keys,
        vec![
            KeyEvent::Up,
            KeyEvent::Down,
            KeyEvent::Right,
            KeyEvent::PageUp,
            KeyEvent::PageDown,
        ]
    );
    assert!(mice.is_empty());

    // Even an explicit Escape key in the main view must NEVER set should_quit = true
    state.handle_key(KeyEvent::Escape, 44);
    assert!(!state.should_quit);

    // Only 'q' or Ctrl+C quits
    state.handle_key(KeyEvent::Char('q'), 44);
    assert!(state.should_quit);
}

#[test]
fn sidebar_scrolls_to_selection_hides_retired_bulk_and_preserves_selected_key() {
    let root = TempDir::new().unwrap();
    let mut terminals = Vec::new();
    let mut bindings = Vec::new();

    // 12 live terminals across different projects
    for i in 1..=12u128 {
        let proj = root.path().join(format!("repo-{i:02}"));
        fs::create_dir_all(&proj).unwrap();
        terminals.push(sample_terminal(
            i,
            "w1",
            &format!("t{i}"),
            i as u32,
            proj,
            i == 1,
        ));
    }
    // 20 retired offline sessions
    for i in 101..=120u128 {
        let proj = root.path().join(format!("old-retired-{i}"));
        fs::create_dir_all(&proj).unwrap();
        bindings.push(sample_binding(
            i,
            Agent::Claude,
            &Uuid::from_u128(i + 1000).to_string(),
            proj,
            "retired",
            "Terminal was deliberately closed.",
        ));
    }

    let snap = build_snapshot_from_parts(terminals, bindings, &BTreeMap::new(), None, true, None);
    assert_eq!(snap.saved_count, 20);

    // Sidebar mode hides retired sessions by default so only the 12 live terminals are shown
    let mut state = AppState::new(snap, true, None);
    assert!(!state.show_saved);
    assert_eq!(state.visible_items().len(), 12);

    // Navigate down to the 12th live terminal (repo-12) in a compact 18-row sidebar
    for _ in 0..11 {
        state.handle_key(KeyEvent::Down, 44);
    }
    assert_eq!(state.selected_item().unwrap().project, "repo-12");

    let mut canvas = Canvas::new(44, 18);
    render_app(&state, &mut canvas);
    let rendered = canvas.render_snapshot(false);
    // The 12th item must be scrolled into the visible viewport!
    assert!(rendered.contains("repo-12"));
    assert!(rendered.contains("12/12"));

    // Pressing 's' toggles showing the 20 retired sessions
    state.handle_key(KeyEvent::Char('s'), 44);
    assert!(state.show_saved);
    assert_eq!(state.visible_items().len(), 32);
    // And selected_key keeps repo-12 selected even after filter expansion!
    assert_eq!(state.selected_item().unwrap().project, "repo-12");
}

#[test]
fn sidebar_right_pane_badge_single_click_focus_and_safe_spawn_defaults() {
    let root = TempDir::new().unwrap();
    let grok_dir = root.path().join("grok-workspace");
    let right_dir = root.path().join("right-pane-workspace");
    fs::create_dir_all(&grok_dir).unwrap();
    fs::create_dir_all(&right_dir).unwrap();

    let self_id = Uuid::from_u128(30);
    let right_id = Uuid::from_u128(31);
    let grok_id = Uuid::from_u128(20);

    let terminals = vec![
        sample_terminal(20, "w1", "t2", 2, grok_dir.clone(), false),
        sample_terminal(30, "w1", "t3", 3, right_dir.clone(), true),
        sample_terminal(31, "w1", "t3", 3, right_dir.clone(), false),
    ];
    let snap = build_snapshot_from_parts(
        terminals,
        Vec::new(),
        &BTreeMap::new(),
        Some(&self_id.to_string()),
        true,
        None,
    );
    let mut state = AppState::new(snap, true, Some(self_id.to_string()));

    // By default in sidebar mode, the sibling terminal in the right pane (31 in t3) is selected
    assert_eq!(
        state.selected_item().unwrap().terminal_key,
        right_id.to_string()
    );

    let mut canvas = Canvas::new(44, 22);
    render_app(&state, &mut canvas);
    let rendered = canvas.render_snapshot(false);
    assert!(rendered.contains("◉ RIGHT"));

    // Single-clicking the grok card in Sidebar mode immediately returns UiEffect::Focus(grok_id)
    let grok_idx = state
        .snapshot
        .items
        .iter()
        .position(|i| i.terminal_key == grok_id.to_string())
        .unwrap();
    let eff = state.apply_hit(HitTarget::SelectCard(grok_idx));
    assert!(matches!(eff, UiEffect::Focus(id) if id == grok_id));

    // With grok (in t2) selected, opening the Spawn modal defaults to SplitRight
    // and targets the current tab's right pane (right_id in t3), never splitting t2 or opening a new tab!
    state.handle_key(KeyEvent::Char('n'), 44);
    state.handle_key(KeyEvent::Char('7'), 44); // Select Grok
    let spawn_eff = state.handle_key(KeyEvent::Enter, 44);
    match spawn_eff {
        UiEffect::Spawn {
            target,
            disposition,
            cwd,
            command,
        } => {
            assert_eq!(disposition, SpawnDisposition::SplitRight);
            assert_eq!(target, Some(right_id));
            assert_eq!(cwd, grok_dir);
            assert!(command.unwrap().contains("grok"));
        }
        _ => panic!("Expected UiEffect::Spawn"),
    }
}

#[test]
fn parent_child_ping_pong_orchestration_and_launch_commands() {
    use ghostback::herd::{
        ReplyVerdict, TaskRole, TaskStatus, build_child_launch_command, create_task, get_task,
        list_tasks, record_child_reply, record_parent_message, resolve_task_id, wait_for_turn,
    };

    let root = tempfile::tempdir().unwrap();
    let store = Store::new(root.path().join("state")).unwrap();
    let repo_dir = root.path().join("repo");
    fs::create_dir_all(&repo_dir).unwrap();

    let parent_key = Uuid::from_u128(100).to_string();
    let child_key = Uuid::from_u128(101).to_string();

    // 1. Parent (Claude) delegates a Reviewer task to Child (Codex)
    let (task, seq1) = create_task(
        &store,
        "t-rev01",
        &parent_key,
        &child_key,
        Some(Agent::Claude),
        Agent::Codex,
        TaskRole::Reviewer,
        "Audit src/macos.rs for focus race conditions.",
        &repo_dir,
    )
    .unwrap();
    assert_eq!(task.status, TaskStatus::WaitingChild);
    assert_eq!(task.round, 1);

    // Resolve task ID from either child or parent terminal key
    assert_eq!(
        resolve_task_id(&store, None, Some(&child_key), true).unwrap(),
        "t-rev01"
    );
    assert_eq!(
        resolve_task_id(&store, None, Some(&parent_key), false).unwrap(),
        "t-rev01"
    );

    // 2. Child (Codex) submits Round 1 findings: changes_requested
    let (after_r1, seq2) = record_child_reply(
        &store,
        "t-rev01",
        ReplyVerdict::ChangesRequested,
        "Found an edge case when sidebar is the sole pane in the tab.",
    )
    .unwrap();
    assert_eq!(after_r1.status, TaskStatus::ChangesRequested);
    assert!(seq2 > seq1);

    // Parent waiting since seq1 immediately observes Child's reply (race-free!)
    let (waited_task, waited_msg) =
        wait_for_turn(&store, "t-rev01", seq1, "child", Duration::from_secs(1)).unwrap();
    assert_eq!(waited_task.status, TaskStatus::ChangesRequested);
    assert_eq!(waited_msg.sender, "child");
    assert_eq!(waited_msg.verdict, "changes_requested");

    // 3. Parent (Claude) fixes the issue and asks Child (Codex) for Round 2 re-review
    let (after_parent, seq3) = record_parent_message(
        &store,
        "t-rev01",
        "Fixed the sole-pane check in focus_from_sidebar; please re-verify.",
    )
    .unwrap();
    assert_eq!(after_parent.status, TaskStatus::WaitingChild);
    assert_eq!(after_parent.round, 2);
    assert!(seq3 > seq2);

    // Child waiting since seq2 immediately receives Parent's Round 2 message
    let (child_waited_task, child_waited_msg) =
        wait_for_turn(&store, "t-rev01", seq2, "parent", Duration::from_secs(1)).unwrap();
    assert_eq!(child_waited_task.round, 2);
    assert_eq!(child_waited_msg.sender, "parent");

    // 4. Child (Codex) approves in Round 2
    let (final_task, _seq4) = record_child_reply(
        &store,
        "t-rev01",
        ReplyVerdict::Approved,
        "LGTM! All focus & right-pane tests pass.",
    )
    .unwrap();
    assert_eq!(final_task.status, TaskStatus::Approved);

    let (_, msgs) = get_task(&store, "t-rev01").unwrap().unwrap();
    assert_eq!(msgs.len(), 4);
    assert_eq!(list_tasks(&store).unwrap().len(), 1);

    // 5. Verify launch commands across all 6 harnesses
    let exe = PathBuf::from("/usr/local/bin/ghostback");
    for agent in [
        Agent::Claude,
        Agent::Codex,
        Agent::Agy,
        Agent::Opencode,
        Agent::Pi,
        Agent::Grok,
    ] {
        let cmd = build_child_launch_command(
            &exe,
            &store.directory,
            "t-rev01",
            &parent_key,
            Some(Agent::Claude),
            agent,
            TaskRole::Reviewer,
            "Review diff",
        );
        assert!(cmd.contains("GHOSTBACK_TASK_ID="));
        assert!(cmd.contains("reply --task t-rev01"));
        match agent {
            Agent::Agy => {
                assert!(cmd.contains("--prompt-interactive") && cmd.contains("--add-dir"))
            }
            Agent::Opencode => {
                assert!(cmd.contains("--prompt") && cmd.contains("OPENCODE_CONFIG_CONTENT"))
            }
            other => assert!(cmd.contains(other.name())),
        }
    }
}

#[test]
fn mcp_server_model_effort_and_per_session_dock_injection() {
    use ghostback::agents::{interactive, replay_options};
    use ghostback::herd::{
        AgentLaunchOptions, TaskRole, build_agent_model_effort_args,
        build_child_launch_command_with_options, build_parent_launch_command,
        ensure_mcp_config_file_for_task, extract_inline_launch_options, parse_model_and_effort,
        per_session_mcp_args_and_env_with_options, run_mcp_server,
    };
    use serde_json::{Value, json};
    use std::io::Cursor;

    let root = tempfile::tempdir().unwrap();
    let store = Store::new(root.path().join("state")).unwrap();
    let exe = PathBuf::from("/usr/local/bin/ghostback");

    // 1. Natural-language model + effort parsing ("sol 6.1 xhigh" -> model="sol-6.1", effort="xhigh")
    assert_eq!(
        parse_model_and_effort(Some("sol 6.1 xhigh"), None),
        (Some("sol-6.1".into()), Some("xhigh".into()))
    );
    assert_eq!(
        parse_model_and_effort(Some("sol 6.1"), Some("XHIGH")),
        (Some("sol-6.1".into()), Some("xhigh".into()))
    );
    let (inline_opts, inline_goal) =
        extract_inline_launch_options("@sol 6.1 xhigh Review my changes");
    assert_eq!(inline_opts.model.as_deref(), Some("sol-6.1"));
    assert_eq!(inline_opts.effort.as_deref(), Some("xhigh"));
    assert_eq!(inline_goal, "Review my changes");

    let opts = AgentLaunchOptions {
        model: Some("sol 6.1 xhigh".into()),
        effort: None,
        extra_args: Vec::new(),
    };
    assert_eq!(
        build_agent_model_effort_args(Agent::Codex, &opts),
        vec![
            "-m".to_owned(),
            "sol-6.1".to_owned(),
            "-c".to_owned(),
            "model_reasoning_effort=\"xhigh\"".to_owned()
        ]
    );
    assert_eq!(
        build_agent_model_effort_args(Agent::Claude, &opts),
        vec![
            "--model".to_owned(),
            "sol-6.1".to_owned(),
            "--effort".to_owned(),
            "xhigh".to_owned()
        ]
    );
    assert_eq!(
        build_agent_model_effort_args(Agent::Pi, &opts),
        vec![
            "--model".to_owned(),
            "sol-6.1".to_owned(),
            "--thinking".to_owned(),
            "xhigh".to_owned()
        ]
    );
    assert_eq!(
        build_agent_model_effort_args(Agent::Grok, &opts),
        vec![
            "--model".to_owned(),
            "sol-6.1".to_owned(),
            "--reasoning-effort".to_owned(),
            "xhigh".to_owned()
        ]
    );
    // OpenCode configures model inside OPENCODE_CONFIG_CONTENT rather than CLI --model
    assert!(build_agent_model_effort_args(Agent::Opencode, &opts).is_empty());

    // 2. Per-session MCP flags injected when starting a Parent agent from ghostback --dock / ui
    //    preserve session interactivity and are cleanly stripped from saved replay_options!
    for agent in [
        Agent::Claude,
        Agent::Codex,
        Agent::Agy,
        Agent::Opencode,
        Agent::Pi,
        Agent::Grok,
    ] {
        let parent_cmd = build_parent_launch_command(&exe, &store.directory, agent, &opts);
        assert!(parent_cmd.contains("GHOSTBACK_STATE_DIR="));

        let (mcp_args, mcp_envs) = per_session_mcp_args_and_env_with_options(
            &exe,
            &store.directory,
            agent,
            &opts,
            None,
            None,
        );
        let model_args = build_agent_model_effort_args(agent, &opts);
        let mut argv = vec![agent.name().to_owned()];
        argv.extend(mcp_args);
        argv.extend(model_args);

        assert!(
            interactive(agent, &argv),
            "Agent {agent:?} with per-session MCP args must remain interactive"
        );
        let replayed = replay_options(agent, &argv).unwrap();
        if agent == Agent::Opencode {
            // OpenCode receives model in OPENCODE_CONFIG_CONTENT JSON
            let oc_env = mcp_envs
                .iter()
                .find(|(k, _)| k == "OPENCODE_CONFIG_CONTENT")
                .map(|(_, v)| v.as_str())
                .unwrap();
            assert!(oc_env.contains("\"model\":\"sol-6.1\""));
        } else {
            // Ephemeral MCP flags (--mcp-config, --append-system-prompt, --add-dir, -c) are stripped,
            // while model/effort flags (-m, --model, --effort, --thinking, --reasoning-effort) are preserved!
            assert!(
                replayed.iter().any(|a| a == "sol-6.1"),
                "Expected model sol-6.1 to be preserved in replay_options for {agent:?}: {replayed:?}"
            );
        }
    }

    // Also verify opencode --prompt "..." is recognized as interactive while replay_options strips --prompt
    let oc_child_argv = vec![
        "opencode".to_owned(),
        "--prompt".to_owned(),
        "Review diff".to_owned(),
    ];
    assert!(interactive(Agent::Opencode, &oc_child_argv));
    assert!(
        replay_options(Agent::Opencode, &oc_child_argv)
            .unwrap()
            .is_empty()
    );

    // 3. Per-task MCP config file and AGY workspace plugin generation
    let task_mcp_path = ensure_mcp_config_file_for_task(
        &exe,
        &store.directory,
        Some("t-codex1"),
        Some("0195f2b0-7c10-7000-8000-000000000001"),
    )
    .unwrap();
    assert!(task_mcp_path.ends_with("mcp-task-t-codex1.json"));
    let task_mcp_body = fs::read_to_string(&task_mcp_path).unwrap();
    assert!(task_mcp_body.contains("\"GHOSTBACK_TASK_ID\": \"t-codex1\""));
    assert!(
        store
            .directory
            .join("agy-ws-t-codex1/.agents/plugins/ghostback/mcp_config.json")
            .is_file()
    );
    assert!(store.directory.join("agy-ws-t-codex1/AGENTS.md").is_file());

    // Child launch command with "sol 6.1 xhigh" for Codex includes -m 'sol-6.1' and model_reasoning_effort="xhigh"
    let child_cmd = build_child_launch_command_with_options(
        &exe,
        &store.directory,
        "t-codex1",
        "0195f2b0-7c10-7000-8000-000000000001",
        Some(Agent::Claude),
        Agent::Codex,
        TaskRole::Reviewer,
        "Review my diff",
        &opts,
    );
    assert!(child_cmd.contains("sol-6.1"));
    assert!(child_cmd.contains("model_reasoning_effort=\"xhigh\""));
    assert!(child_cmd.contains("mcp_servers.ghostback.env.GHOSTBACK_TASK_ID"));
    assert!(child_cmd.contains("mcp_servers.ghostback.command"));

    // 4. Built-in stdio MCP server (`ghostback mcp`) end-to-end JSON-RPC session
    let requests = [
        json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "initialize",
            "params": { "protocolVersion": "2024-11-05" }
        }),
        json!({
            "jsonrpc": "2.0",
            "method": "notifications/initialized"
        }),
        json!({
            "jsonrpc": "2.0",
            "id": 2,
            "method": "tools/list"
        }),
        json!({
            "jsonrpc": "2.0",
            "id": 3,
            "method": "tools/call",
            "params": {
                "name": "ghostback_delegate",
                "arguments": {
                    "agent": "codex",
                    "role": "reviewer",
                    "model": "sol 6.1",
                    "effort": "xhigh",
                    "prompt": "Review my changes with sol 6.1 xhigh",
                    "wait": false,
                    "no_spawn": true
                }
            }
        }),
    ];
    let mut input_buf = String::new();
    for r in &requests {
        input_buf.push_str(&serde_json::to_string(r).unwrap());
        input_buf.push('\n');
    }
    let mut out_buf = Vec::new();
    run_mcp_server(&store, Cursor::new(input_buf.into_bytes()), &mut out_buf).unwrap();

    let responses: Vec<Value> = String::from_utf8(out_buf)
        .unwrap()
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect();
    assert_eq!(responses.len(), 3); // initialize, tools/list, tools/call (notification has no response)
    assert_eq!(responses[0]["result"]["serverInfo"]["name"], "ghostback");

    let tools = responses[1]["result"]["tools"].as_array().unwrap();
    assert_eq!(tools.len(), 5);
    assert!(tools.iter().any(|t| t["name"] == "ghostback_delegate"));
    assert!(tools.iter().any(|t| t["name"] == "ghostback_ask"));
    assert!(tools.iter().any(|t| t["name"] == "ghostback_reply"));
    assert!(tools.iter().any(|t| t["name"] == "ghostback_wait"));
    assert!(tools.iter().any(|t| t["name"] == "ghostback_tasks"));

    let call_text = responses[2]["result"]["content"][0]["text"]
        .as_str()
        .unwrap();
    let created_task: Value = serde_json::from_str(call_text).unwrap();
    assert_eq!(created_task["child_agent"], "codex");
    assert_eq!(created_task["role"], "reviewer");
}

#[test]
fn codex_parent_and_grok_reviewer_multi_round_ping_pong_simulation() {
    use ghostback::herd::{
        AgentLaunchOptions, ReplyVerdict, TaskRole, TaskStatus,
        build_child_launch_command_with_options, build_parent_launch_command, get_task,
        record_child_reply, record_parent_message,
    };
    use serde_json::{Value, json};
    use std::io::{BufRead, BufReader, Write};
    use std::process::{Command, Stdio};

    let root = TempDir::new().unwrap();
    let state_dir = root.path().join("state");
    let workspace_dir = root.path().join("workspace");
    fs::create_dir_all(&state_dir).unwrap();
    fs::create_dir_all(workspace_dir.join("src")).unwrap();

    let auth_file = workspace_dir.join("src").join("auth.rs");
    fs::write(
        &auth_file,
        "pub fn verify_token(hdr: Option<&str>) -> bool {\n    let token = hdr.unwrap();\n    !token.is_empty()\n}\n",
    )
    .unwrap();

    let store = Store::new(state_dir.clone()).unwrap();
    let parent_key = "0195f2b0-7c10-7000-8000-000000000101";
    let parent_session = Session::new(
        parent_key.to_owned(),
        Agent::Codex,
        "0195f2b0-7c10-7000-8000-000000000999",
        workspace_dir.clone(),
    )
    .unwrap();
    store.record(&parent_session).unwrap();

    let bin = env!("CARGO_BIN_EXE_ghostback");
    let exe_path = std::path::Path::new(bin);

    // 1. Verify Parent (`codex`) and Child (`grok`) launch commands have all autonomous ping-pong flags
    let codex_parent_cmd = build_parent_launch_command(
        exe_path,
        &state_dir,
        Agent::Codex,
        &AgentLaunchOptions::default(),
    );
    assert!(codex_parent_cmd.contains("mcp_servers.ghostback.command="));
    assert!(codex_parent_cmd.contains("mcp_servers.ghostback.tool_timeout_sec=1800"));
    assert!(codex_parent_cmd.contains("mcp_servers.ghostback.startup_timeout_sec=30"));
    assert!(codex_parent_cmd.contains("developer_instructions="));

    let grok_child_cmd = build_child_launch_command_with_options(
        exe_path,
        &state_dir,
        "t-simtest",
        parent_key,
        Some(Agent::Codex),
        Agent::Grok,
        TaskRole::Reviewer,
        "Review src/auth.rs",
        &AgentLaunchOptions::default(),
    );
    assert!(grok_child_cmd.contains("--rules"));
    assert!(grok_child_cmd.contains("--always-approve"));
    assert!(grok_child_cmd.contains("GHOSTBACK_TASK_ID="));
    assert!(grok_child_cmd.contains("reply --task t-simtest --status changes_requested --wait"));

    // 2. Spawn a live `ghostback mcp` child process representing Codex's stdio MCP connection
    let mut mcp_proc = Command::new(bin)
        .args(["--state-dir", state_dir.to_str().unwrap(), "mcp"])
        .env("GHOSTBACK_PARENT_KEY", parent_key)
        .env("GHOSTBACK_STATE_DIR", &state_dir)
        .current_dir(&workspace_dir)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .expect("spawn ghostback mcp");

    let mut mcp_in = mcp_proc.stdin.take().unwrap();
    let mut mcp_out = BufReader::new(mcp_proc.stdout.take().unwrap());

    let mut send_mcp = |req: Value| -> Value {
        writeln!(mcp_in, "{}", serde_json::to_string(&req).unwrap()).unwrap();
        mcp_in.flush().unwrap();
        let mut line = String::new();
        mcp_out.read_line(&mut line).unwrap();
        serde_json::from_str(&line).expect("valid JSON-RPC response from ghostback mcp")
    };

    // Initialize MCP
    let init_resp = send_mcp(json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "initialize",
        "params": { "protocolVersion": "2024-11-05" }
    }));
    assert_eq!(init_resp["result"]["serverInfo"]["name"], "ghostback");

    // Round 1: Codex delegates review to Grok
    let delegate_resp = send_mcp(json!({
        "jsonrpc": "2.0",
        "id": 2,
        "method": "tools/call",
        "params": {
            "name": "ghostback_delegate",
            "arguments": {
                "agent": "grok",
                "role": "reviewer",
                "prompt": "Review src/auth.rs and ping-pong with me until all issues are resolved.",
                "wait": false,
                "no_spawn": true
            }
        }
    }));
    let delegate_json: Value = serde_json::from_str(
        delegate_resp["result"]["content"][0]["text"]
            .as_str()
            .unwrap(),
    )
    .unwrap();
    let task_id = delegate_json["task_id"].as_str().unwrap().to_owned();
    assert_eq!(delegate_json["parent_agent"], "codex");
    assert_eq!(delegate_json["child_agent"], "grok");
    assert_eq!(delegate_json["round"], 1);

    // Grok (Reviewer) inspects src/auth.rs in a background process, finds `.unwrap()`,
    // and runs `ghostback reply --task <id> --status changes-requested --wait` which blocks waiting for Codex's Round 2 fix!
    let bin_for_r1 = bin.to_owned();
    let state_for_r1 = state_dir.clone();
    let task_for_r1 = task_id.clone();
    let auth_for_r1 = auth_file.clone();
    let grok_r1_handle = std::thread::spawn(move || {
        let code = fs::read_to_string(&auth_for_r1).unwrap();
        assert!(code.contains(".unwrap()"));
        let out = Command::new(&bin_for_r1)
            .args([
                "--state-dir",
                state_for_r1.to_str().unwrap(),
                "reply",
                "--task",
                &task_for_r1,
                "--status",
                "changes-requested",
                "--wait",
                "--timeout-secs",
                "15",
                "Round 1 (Grok): `hdr.unwrap()` panics on missing header; return Result<bool, AuthError>.",
            ])
            .env("GHOSTBACK_TASK_ID", &task_for_r1)
            .output()
            .expect("grok round 1 reply --wait");
        assert!(out.status.success());
        String::from_utf8(out.stdout).unwrap()
    });

    // Codex waits for Grok's Round 1 review via `ghostback_wait` MCP tool
    let wait_r1_resp = send_mcp(json!({
        "jsonrpc": "2.0",
        "id": 3,
        "method": "tools/call",
        "params": {
            "name": "ghostback_wait",
            "arguments": {
                "task_id": task_id,
                "for_sender": "child",
                "timeout_secs": 15
            }
        }
    }));
    let r1_text = wait_r1_resp["result"]["content"][0]["text"]
        .as_str()
        .unwrap();
    assert!(r1_text.contains("Round 1 Reply from GROK (REVIEWER)"));
    assert!(r1_text.contains("Verdict: CHANGES_REQUESTED"));
    assert!(r1_text.contains("hdr.unwrap()"));

    // Round 2: Codex fixes `.unwrap()` in src/auth.rs, then calls `ghostback_ask` with `wait: true`.
    // Meanwhile, Grok's Round 1 `reply --wait` process wakes up, sees Codex's Round 2 update,
    // inspects the updated file, and sends Round 2 `changes-requested` (asking for a unit test).
    fs::write(
        &auth_file,
        "pub fn verify_token(hdr: Option<&str>) -> Result<bool, &'static str> {\n    let token = hdr.ok_or(\"missing token\")?;\n    Ok(!token.is_empty())\n}\n",
    )
    .unwrap();

    let bin_for_r2 = bin.to_owned();
    let state_for_r2 = state_dir.clone();
    let task_for_r2 = task_id.clone();
    let auth_for_r2 = auth_file.clone();
    let grok_r2_handle = std::thread::spawn(move || {
        // Wait for Grok's Round 1 `reply --wait` CLI call to wake up and return Codex's Round 2 message!
        let r1_wakeup_stdout = grok_r1_handle.join().unwrap();
        assert!(r1_wakeup_stdout.contains("Round 2 Update from CODEX"));
        assert!(r1_wakeup_stdout.contains("Replaced unwrap with ok_or"));

        let updated_code = fs::read_to_string(&auth_for_r2).unwrap();
        assert!(!updated_code.contains(".unwrap()"));
        assert!(updated_code.contains("ok_or(\"missing token\")?"));

        // Grok sends Round 2 `changes-requested` and waits for Round 3
        let out = Command::new(&bin_for_r2)
            .args([
                "--state-dir",
                state_for_r2.to_str().unwrap(),
                "reply",
                "--task",
                &task_for_r2,
                "--status",
                "changes-requested",
                "--wait",
                "--timeout-secs",
                "15",
                "Round 2 (Grok): Error handling fixed! Please add a unit test covering `verify_token(None)`.",
            ])
            .env("GHOSTBACK_TASK_ID", &task_for_r2)
            .output()
            .expect("grok round 2 reply --wait");
        assert!(out.status.success());
        String::from_utf8(out.stdout).unwrap()
    });

    // Codex calls `ghostback_ask` (`wait: true`) — this wakes up `grok_r1_handle` AND blocks until `grok_r2_handle` replies!
    let ask_r2_resp = send_mcp(json!({
        "jsonrpc": "2.0",
        "id": 4,
        "method": "tools/call",
        "params": {
            "name": "ghostback_ask",
            "arguments": {
                "task_id": task_id,
                "message": "Round 2 (Codex): Replaced unwrap with ok_or(\"missing token\")?. Please re-review.",
                "wait": true,
                "timeout_secs": 15
            }
        }
    }));
    let r2_text = ask_r2_resp["result"]["content"][0]["text"]
        .as_str()
        .unwrap();
    assert!(r2_text.contains("Round 2 Reply from GROK (REVIEWER)"));
    assert!(r2_text.contains("Verdict: CHANGES_REQUESTED"));
    assert!(r2_text.contains("verify_token(None)"));

    // Round 3: Codex adds the unit test to src/auth.rs and calls `ghostback_ask` (`wait: true`).
    // Grok's Round 2 `reply --wait` wakes up, verifies the test, and sends `--status approved`!
    fs::write(
        &auth_file,
        "pub fn verify_token(hdr: Option<&str>) -> Result<bool, &'static str> {\n    let token = hdr.ok_or(\"missing token\")?;\n    Ok(!token.is_empty())\n}\n\n#[cfg(test)]\nmod tests {\n    use super::*;\n    #[test]\n    fn rejects_missing_token() {\n        assert!(verify_token(None).is_err());\n    }\n}\n",
    )
    .unwrap();

    let bin_for_r3 = bin.to_owned();
    let state_for_r3 = state_dir.clone();
    let task_for_r3 = task_id.clone();
    let auth_for_r3 = auth_file.clone();
    let grok_r3_handle = std::thread::spawn(move || {
        let r2_wakeup_stdout = grok_r2_handle.join().unwrap();
        assert!(r2_wakeup_stdout.contains("Round 3 Update from CODEX"));
        assert!(r2_wakeup_stdout.contains("Added rejects_missing_token test"));

        let final_code = fs::read_to_string(&auth_for_r3).unwrap();
        assert!(final_code.contains("fn rejects_missing_token()"));

        let out = Command::new(&bin_for_r3)
            .args([
                "--state-dir",
                state_for_r3.to_str().unwrap(),
                "reply",
                "--task",
                &task_for_r3,
                "--status",
                "approved",
                "Round 3 (Grok): LGTM! Both the Result return type and unit test are clean. Approved.",
            ])
            .env("GHOSTBACK_TASK_ID", &task_for_r3)
            .output()
            .expect("grok round 3 reply approved");
        assert!(out.status.success());
        String::from_utf8(out.stdout).unwrap()
    });

    let ask_r3_resp = send_mcp(json!({
        "jsonrpc": "2.0",
        "id": 5,
        "method": "tools/call",
        "params": {
            "name": "ghostback_ask",
            "arguments": {
                "task_id": task_id,
                "message": "Round 3 (Codex): Added rejects_missing_token test in src/auth.rs.",
                "wait": true,
                "timeout_secs": 15
            }
        }
    }));
    let r3_text = ask_r3_resp["result"]["content"][0]["text"]
        .as_str()
        .unwrap();
    assert!(r3_text.contains("Round 3 Reply from GROK (REVIEWER)"));
    assert!(r3_text.contains("Verdict: APPROVED"));
    assert!(r3_text.contains("collaboration loop complete"));

    let r3_cli_stdout = grok_r3_handle.join().unwrap();
    assert!(r3_cli_stdout.contains("Recorded approved reply (round 3)"));

    // Cleanly close MCP server stdin and wait for it to exit
    drop(mcp_in);
    let mcp_status = mcp_proc.wait().unwrap();
    assert!(mcp_status.success());

    // 3. Verify final SQLite task state and full 6-message transcript across the 3 rounds
    let (final_task, msgs) = get_task(&store, &task_id).unwrap().unwrap();
    assert_eq!(final_task.status, TaskStatus::Approved);
    assert_eq!(final_task.round, 3);
    assert_eq!(final_task.parent_agent, Some(Agent::Codex));
    assert_eq!(final_task.child_agent, Agent::Grok);
    assert_eq!(msgs.len(), 6);
    assert_eq!(msgs[0].round, 1);
    assert_eq!(msgs[0].sender, "parent");
    assert_eq!(msgs[1].round, 1);
    assert_eq!(msgs[1].sender, "child");
    assert_eq!(msgs[1].verdict, "changes_requested");
    assert_eq!(msgs[2].round, 2);
    assert_eq!(msgs[2].sender, "parent");
    assert_eq!(msgs[3].round, 2);
    assert_eq!(msgs[3].sender, "child");
    assert_eq!(msgs[3].verdict, "changes_requested");
    assert_eq!(msgs[4].round, 3);
    assert_eq!(msgs[4].sender, "parent");
    assert_eq!(msgs[5].round, 3);
    assert_eq!(msgs[5].sender, "child");
    assert_eq!(msgs[5].verdict, "approved");

    // 4. Verify retry idempotency: if Grok re-runs the identical Round 3 reply command, no duplicate is inserted
    let (_, retry_seq) = record_child_reply(
        &store,
        &task_id,
        ReplyVerdict::Approved,
        "Round 3 (Grok): LGTM! Both the Result return type and unit test are clean. Approved.",
    )
    .unwrap();
    assert_eq!(retry_seq, msgs[5].seq);
    let (_, msgs_after_retry) = get_task(&store, &task_id).unwrap().unwrap();
    assert_eq!(msgs_after_retry.len(), 6);
    let _ = record_parent_message;
}

#[test]
fn sidebar_tree_nests_children_under_parent_and_pair_modal_spawns_child() {
    use ghostback::herd::TaskRole;

    let snap = demo_snapshot();
    assert!(!snap.tasks.is_empty());
    let mut state = AppState::new(snap, true, None);

    // In Sidebar mode, Demo snapshot has Parent (0195f2b0-...-000001) with Child (0195f2b0-...-000002 Codex Reviewer).
    // visual_order must nest 000002 immediately after its parent 000001!
    let order = state.visual_order(44);
    let parent_idx = state
        .snapshot
        .items
        .iter()
        .position(|i| i.terminal_key == "0195f2b0-7c10-7000-8000-000000000001")
        .unwrap();
    let child_idx = state
        .snapshot
        .items
        .iter()
        .position(|i| i.terminal_key == "0195f2b0-7c10-7000-8000-000000000002")
        .unwrap();
    let parent_pos = order.iter().position(|&idx| idx == parent_idx).unwrap();
    assert_eq!(order[parent_pos + 1], child_idx);

    // Render Sidebar and verify the tree connector `└─ ↳ REV·CODEX R2`
    let mut canvas = Canvas::new(46, 30);
    render_app(&state, &mut canvas);
    let rendered = canvas.render_snapshot(false);
    assert!(rendered.contains("↳ REV·CODEX R2"));

    // Select Parent and press 'p' to open Pair Child Agent modal
    state.selected_index = parent_idx;
    state.handle_key(KeyEvent::Char('p'), 46);
    assert!(matches!(state.modal, Modal::SpawnChild { .. }));

    // Verify narrow 42-column docked sidebar renders all 3 role pills and all 6 agent choices cleanly
    let mut modal_canvas = Canvas::new(42, 22);
    render_app(&state, &mut modal_canvas);
    let modal_rendered = modal_canvas.render_snapshot(false);
    assert!(modal_rendered.contains("Reviewer"));
    assert!(modal_rendered.contains("Worker"));
    assert!(modal_rendered.contains("Advisor"));
    assert!(modal_rendered.contains("1. Claude Code"));
    assert!(modal_rendered.contains("6. Grok Build"));

    // Cycle role from Reviewer -> Worker with Tab, then press Enter to emit UiEffect::SpawnChild
    state.handle_key(KeyEvent::Tab, 46);
    let eff = state.handle_key(KeyEvent::Enter, 46);
    match eff {
        UiEffect::SpawnChild {
            parent_key,
            parent_agent,
            child_agent,
            role,
            goal,
            ..
        } => {
            assert_eq!(parent_key, "0195f2b0-7c10-7000-8000-000000000001");
            assert_eq!(parent_agent, Some(Agent::Claude));
            assert_eq!(child_agent, Agent::Codex);
            assert_eq!(role, TaskRole::Worker);
            assert_eq!(goal, TaskRole::Worker.default_goal());
        }
        _ => panic!("Expected UiEffect::SpawnChild"),
    }
}

#[test]
fn launch_commands_with_multiline_instructions_and_prompts_pass_macos_validation() {
    use ghostback::herd::{
        AgentLaunchOptions, TaskRole, build_child_launch_command_with_options,
        build_parent_launch_command, extract_inline_launch_options_for_role,
    };
    use ghostback::macos::validate_launch_inputs;

    let root = TempDir::new().unwrap();
    let state_dir = root.path().join("custom-state");
    let cwd = root.path().join("repo");
    fs::create_dir_all(&state_dir).unwrap();
    fs::create_dir_all(&cwd).unwrap();
    let exe = PathBuf::from("/usr/local/bin/ghostback");
    let opts = AgentLaunchOptions {
        model: Some("sol 6.1 xhigh".into()),
        effort: None,
        extra_args: vec!["--verbose".into()],
    };
    let multiline_goal = "Line 1: Audit src/macos.rs.\nLine 2: Verify multi-line bootstrap prompts.\n\tIndented detail.";

    for agent in [
        Agent::Claude,
        Agent::Codex,
        Agent::Agy,
        Agent::Opencode,
        Agent::Pi,
        Agent::Grok,
    ] {
        let parent_cmd = build_parent_launch_command(&exe, &state_dir, agent, &opts);
        validate_launch_inputs(&cwd, Some(&parent_cmd)).unwrap_or_else(|e| {
            panic!("parent launch command for {agent:?} failed validation: {e}")
        });

        let child_cmd = build_child_launch_command_with_options(
            &exe,
            &state_dir,
            "t-valid01",
            "0195f2b0-7c10-7000-8000-000000000001",
            Some(Agent::Codex),
            agent,
            TaskRole::Reviewer,
            multiline_goal,
            &opts,
        );
        validate_launch_inputs(&cwd, Some(&child_cmd)).unwrap_or_else(|e| {
            panic!("child launch command for {agent:?} failed validation: {e}")
        });
    }

    // Unsafe control characters (NUL, ESC) and relative cwds must still be rejected
    assert!(validate_launch_inputs(&cwd, Some("claude \x1b[31m")).is_err());
    assert!(validate_launch_inputs(&cwd, Some("claude \0")).is_err());
    assert!(validate_launch_inputs(std::path::Path::new("relative/path"), Some("claude")).is_err());

    // Role-aware inline @model [effort] extraction falls back to the role's own default_goal
    for role in TaskRole::ALL {
        let (parsed_opts, goal) = extract_inline_launch_options_for_role("@sol 6.1 xhigh", role);
        assert_eq!(parsed_opts.model.as_deref(), Some("sol-6.1"));
        assert_eq!(parsed_opts.effort.as_deref(), Some("xhigh"));
        assert_eq!(goal, role.default_goal());
    }

    // Explicit AppState.state_dir is propagated into parent launch commands from Modal::Spawn
    let terminals = vec![sample_terminal(1, "w1", "t1", 1, cwd, true)];
    let snap = build_snapshot_from_parts(terminals, Vec::new(), &BTreeMap::new(), None, true, None);
    let mut state = AppState::new(snap, false, None);
    state.state_dir = Some(state_dir.clone());
    state.handle_key(KeyEvent::Char('n'), 120);
    state.handle_key(KeyEvent::Char('3'), 120); // Codex
    if let UiEffect::Spawn {
        command: Some(cmd), ..
    } = state.handle_key(KeyEvent::Enter, 120)
    {
        assert!(cmd.contains(&state_dir.to_string_lossy().into_owned()));
        ghostback::macos::validate_launch_inputs(root.path(), Some(&cmd)).unwrap();
    } else {
        panic!("Expected UiEffect::Spawn with command");
    }

    // Sidebar launch command must start with an executable path (never a VAR=VAL shell prefix)
    let store = Store::new(state_dir).unwrap();
    let sidebar_cmd = ghostback::ui::sidebar_launch_command(&store).unwrap();
    ghostback::macos::validate_launch_inputs(root.path(), Some(&sidebar_cmd)).unwrap();
    assert!(
        ghostback::macos::validate_launch_inputs(
            root.path(),
            Some("GHOSTBACK_STATE_DIR='/tmp/state' '/usr/local/bin/ghostback' ui --sidebar")
        )
        .is_err()
    );
}

#[test]
fn task_resolution_prefers_active_tasks_and_mcp_supports_content_length_framing() {
    use ghostback::herd::{
        ReplyVerdict, TaskRole, create_task, record_child_reply, resolve_task_id, run_mcp_server,
    };
    use serde_json::{Value, json};
    use std::io::Cursor;

    let root = TempDir::new().unwrap();
    let store = Store::new(root.path().join("state")).unwrap();
    let cwd = root.path().join("repo");
    fs::create_dir_all(&cwd).unwrap();

    let parent_key = Uuid::from_u128(501).to_string();
    let child1_key = Uuid::from_u128(502).to_string();
    let child2_key = Uuid::from_u128(503).to_string();

    // Create two tasks on the same parent within the same second, then mark the first Approved
    create_task(
        &store,
        "t-first",
        &parent_key,
        &child1_key,
        Some(Agent::Claude),
        Agent::Codex,
        TaskRole::Reviewer,
        "First review",
        &cwd,
    )
    .unwrap();
    create_task(
        &store,
        "t-second",
        &parent_key,
        &child2_key,
        Some(Agent::Claude),
        Agent::Grok,
        TaskRole::Reviewer,
        "Second active review",
        &cwd,
    )
    .unwrap();
    record_child_reply(&store, "t-first", ReplyVerdict::Approved, "LGTM first").unwrap();

    // Even though t-first was updated last (to Approved), resolving for parent_key prefers active t-second!
    assert_eq!(
        resolve_task_id(&store, None, Some(&parent_key), false).unwrap(),
        "t-second"
    );

    // Verify Content-Length framed MCP request with Content-Type header and lowercase content-length
    let body = serde_json::to_string(&json!({
        "jsonrpc": "2.0",
        "id": 99,
        "method": "ping"
    }))
    .unwrap();
    let framed = format!(
        "Content-Type: application/vscode-jsonrpc; charset=utf-8\r\ncontent-length: {}\r\n\r\n{}",
        body.len(),
        body
    );
    let mut out_buf = Vec::new();
    run_mcp_server(&store, Cursor::new(framed.into_bytes()), &mut out_buf).unwrap();
    let out_str = String::from_utf8(out_buf).unwrap();
    assert!(out_str.starts_with("Content-Length: "));
    let (_, json_part) = out_str.split_once("\r\n\r\n").unwrap();
    let parsed: Value = serde_json::from_str(json_part).unwrap();
    assert_eq!(parsed["id"], 99);
}

#[test]
fn managed_pty_hide_from_view_close_drag_swap_and_3x3_grid_layouts() {
    use ghostback::herd::{
        ViewportBinding, detach_pty_from_viewports, kill_pty, list_live_ptys, list_live_viewports,
        spawn_managed_pty, swap_viewports, write_viewport_binding,
    };
    use ghostback::ui::{GridLayout, build_snapshot_from_parts_with_ptys};

    let root = TempDir::new().unwrap();
    let state_dir = root.path().join("state");
    let cwd = root.path().join("workspace");
    fs::create_dir_all(&state_dir).unwrap();
    fs::create_dir_all(&cwd).unwrap();

    let bin = env!("CARGO_BIN_EXE_ghostback");
    let exe = PathBuf::from(bin);

    // 1. Spawn two real background PTY hosts (`pty-host`) that stay alive independent of any Ghostty split
    let pty_a = Uuid::from_u128(901).to_string();
    let pty_b = Uuid::from_u128(902).to_string();
    let marker_a = root.path().join("alive_a.txt");
    let cmd_a = format!(
        "echo running > '{}' && sleep 30",
        marker_a.to_string_lossy()
    );
    let meta_a = spawn_managed_pty(
        &exe,
        &state_dir,
        &pty_a,
        &cwd,
        "claude-worker",
        Some(&cmd_a),
    )
    .expect("spawn pty_a");
    let meta_b = spawn_managed_pty(
        &exe,
        &state_dir,
        &pty_b,
        &cwd,
        "codex-reviewer",
        Some("sleep 30"),
    )
    .expect("spawn pty_b");

    assert!(!meta_a.slave_tty.is_empty());
    assert!(!meta_b.slave_tty.is_empty());
    assert_eq!(list_live_ptys(&state_dir).len(), 2);

    // Wait briefly for pty_a's command to write marker_a
    for _ in 0..40 {
        if marker_a.exists() {
            break;
        }
        std::thread::sleep(Duration::from_millis(25));
    }
    assert!(marker_a.exists());

    // 2. Bind two viewports (slot-1 and slot-2) to pty_a and pty_b, and test 0ms swap_viewports
    let my_pid = std::process::id();
    write_viewport_binding(
        &state_dir,
        &ViewportBinding {
            slot_id: "slot-1".into(),
            pty_id: pty_a.clone(),
            attach_pid: my_pid,
            viewport_tty: "ttys001".into(),
            updated_at: 1,
        },
    )
    .unwrap();
    write_viewport_binding(
        &state_dir,
        &ViewportBinding {
            slot_id: "slot-2".into(),
            pty_id: pty_b.clone(),
            attach_pid: my_pid,
            viewport_tty: "ttys002".into(),
            updated_at: 1,
        },
    )
    .unwrap();
    swap_viewports(&state_dir, "slot-1", "slot-2").unwrap();
    let vps = list_live_viewports(&state_dir);
    let slot1_pty = vps
        .iter()
        .find(|v| v.slot_id == "slot-1")
        .map(|v| v.pty_id.as_str());
    let slot2_pty = vps
        .iter()
        .find(|v| v.slot_id == "slot-2")
        .map(|v| v.pty_id.as_str());
    assert_eq!(slot1_pty, Some(pty_b.as_str()));
    assert_eq!(slot2_pty, Some(pty_a.as_str()));

    // 3. Hide pty_a from view (`detach_pty_from_viewports`): slot-2 is marked CLOSE,
    // while pty_a remains 100% alive in the background and shows up as `is_hidden: true` (`◌ HIDDEN`)!
    detach_pty_from_viewports(&state_dir, &pty_a).unwrap();
    let vps_after_hide = list_live_viewports(&state_dir);
    assert_eq!(vps_after_hide.len(), 1);
    assert_eq!(vps_after_hide[0].slot_id, "slot-1");

    let terminals = vec![sample_terminal(1, "w1", "t1", 1, cwd.clone(), true)];
    let snap = build_snapshot_from_parts_with_ptys(
        terminals,
        Vec::new(),
        &BTreeMap::new(),
        None,
        true,
        None,
        list_live_ptys(&state_dir),
        vps_after_hide,
    );
    assert_eq!(snap.hidden_count, 1);
    let hidden_item = snap.items.iter().find(|i| i.is_hidden).unwrap();
    assert_eq!(hidden_item.pty_id.as_deref(), Some(pty_a.as_str()));
    assert_eq!(hidden_item.surface_badge(), "◌ HIDDEN");

    // 4. Verify UI Sidebar renders `1×1`..`3×3` grid buttons, `-Hide`, `×End`, and `◌ HIDDEN ↵` badge,
    // and supports mouse drag-and-drop swap (`LeftDown` -> `Drag` -> `LeftUp`) + `J`/`K` swap + `-`/`x` hotkeys!
    let mut state = AppState::new(snap, true, None);
    let mut canvas = Canvas::new(46, 24);
    render_app(&state, &mut canvas);
    let side_rendered = canvas.render_snapshot(false);
    assert!(side_rendered.contains("1×1"));
    assert!(side_rendered.contains("1×2"));
    assert!(side_rendered.contains("2×1"));
    assert!(side_rendered.contains("2×2"));
    assert!(side_rendered.contains("3×3"));
    assert!(side_rendered.contains("-Hide"));
    assert!(side_rendered.contains("×End"));
    assert!(side_rendered.contains("◌ HIDDEN ↵"));

    // Clicking the hidden session emits AttachHiddenPty so it re-attaches in 0ms
    let hidden_idx = state
        .snapshot
        .items
        .iter()
        .position(|i| i.is_hidden)
        .unwrap();
    let attach_eff = state.apply_hit(HitTarget::FocusCard(hidden_idx));
    assert!(matches!(attach_eff, UiEffect::AttachHiddenPty { ref pty_id, .. } if pty_id == &pty_a));

    // Dragging card 0 onto card 1 with SGR mouse sequences (`0;..M` -> `32;..M` -> `0;..m`) emits SwapSessions(0, 1)
    let card0_rect = canvas
        .hits
        .iter()
        .find_map(|(r, t)| matches!(t, HitTarget::SelectCard(0)).then_some(*r))
        .unwrap();
    let card1_rect = canvas
        .hits
        .iter()
        .find_map(|(r, t)| matches!(t, HitTarget::SelectCard(1)).then_some(*r))
        .unwrap();
    let sgr_drag = format!(
        "\x1b[<0;{};{}M\x1b[<32;{};{}M\x1b[<0;{};{}m",
        card0_rect.x + 2,
        card0_rect.y + 1,
        card1_rect.x + 2,
        card1_rect.y + 1,
        card1_rect.x + 2,
        card1_rect.y + 1,
    );
    let (_, drag_mice) = parse_input_events(sgr_drag.as_bytes());
    assert_eq!(drag_mice.len(), 3);
    assert_eq!(drag_mice[0].kind, MouseKind::LeftDown);
    assert_eq!(drag_mice[1].kind, MouseKind::Drag);
    assert_eq!(drag_mice[2].kind, MouseKind::LeftUp);
    let _ = state.handle_mouse(drag_mice[0], &canvas);
    let _ = state.handle_mouse(drag_mice[1], &canvas);
    let drop_eff = state.handle_mouse(drag_mice[2], &canvas);
    assert!(matches!(drop_eff, UiEffect::SwapSessions(0, 1)));

    // Pressing Shift+J / Shift+K swaps sessions via keyboard
    let key_swap = state.handle_key(KeyEvent::Char('J'), 46);
    assert!(matches!(key_swap, UiEffect::SwapSessions(_, _)));

    // Pressing '-' emits HideSession and pressing 'x' emits CloseSession
    assert!(matches!(
        state.handle_key(KeyEvent::Char('-'), 46),
        UiEffect::HideSession(_)
    ));
    assert!(matches!(
        state.handle_key(KeyEvent::Char('x'), 46),
        UiEffect::CloseSession(_)
    ));

    // Clicking the 3x3 grid preset pill emits ApplyGridLayout(GridLayout::Grid3x3) with (3, 3) = 9 cells
    assert_eq!(GridLayout::Grid3x3.dimensions(), (3, 3));
    assert_eq!(GridLayout::Grid3x3.cells(), 9);
    let grid_eff = state.apply_hit(HitTarget::ApplyGrid(GridLayout::Grid3x3));
    assert!(matches!(
        grid_eff,
        UiEffect::ApplyGridLayout(GridLayout::Grid3x3)
    ));

    // 5. Terminate both background PTYs (`kill_pty`) and verify clean shutdown
    kill_pty(&state_dir, &pty_a).unwrap();
    kill_pty(&state_dir, &pty_b).unwrap();
    assert!(list_live_ptys(&state_dir).is_empty());
}
