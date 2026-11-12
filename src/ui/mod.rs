//! Interactive Herdr-style fleet board, table, and sidebar UI for Ghostty terminals.

pub mod canvas;
pub mod demo;
pub mod input;
pub mod modals;
pub mod render;
pub mod snapshot;
pub mod state;

pub use canvas::*;
pub use demo::*;
pub use input::*;
pub use modals::*;
pub use render::*;
pub use snapshot::*;
pub use state::*;

use std::io::{self, IsTerminal, Read, Write};
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use uuid::Uuid;

use crate::capture::{Cache, Roots};
use crate::herd::{self, AgentLaunchOptions, DelegateSpec};
use crate::macos::{self, SpawnDisposition};
use crate::sessions::{Store, shell_quote};
use crate::{Result, app, invalid};

struct TerminalGuard {
    original: libc::termios,
}

impl TerminalGuard {
    fn enter() -> Result<Self> {
        let mut termios = std::mem::MaybeUninit::<libc::termios>::zeroed();
        // SAFETY: STDIN_FILENO is a valid descriptor and termios is properly sized.
        if unsafe { libc::tcgetattr(libc::STDIN_FILENO, termios.as_mut_ptr()) } != 0 {
            return Err(io::Error::last_os_error().into());
        }
        // SAFETY: tcgetattr succeeded above.
        let original = unsafe { termios.assume_init() };
        let mut raw = original;
        raw.c_lflag &= !(libc::ECHO | libc::ICANON | libc::ISIG | libc::IEXTEN);
        raw.c_iflag &= !(libc::IXON | libc::ICRNL | libc::BRKINT | libc::INPCK | libc::ISTRIP);
        raw.c_cflag |= libc::CS8;
        raw.c_cc[libc::VMIN] = 0;
        raw.c_cc[libc::VTIME] = 0;
        // SAFETY: applying validated termios configuration to STDIN_FILENO.
        if unsafe { libc::tcsetattr(libc::STDIN_FILENO, libc::TCSAFLUSH, &raw) } != 0 {
            return Err(io::Error::last_os_error().into());
        }
        let mut out = io::stdout().lock();
        out.write_all(b"\x1b[?1049h\x1b[?25l\x1b[?1000h\x1b[?1002h\x1b[?1006h")?;
        out.flush()?;
        Ok(Self { original })
    }
}

impl Drop for TerminalGuard {
    fn drop(&mut self) {
        let _ = io::stdout()
            .write_all(b"\x1b[?1006l\x1b[?1003l\x1b[?1002l\x1b[?1000l\x1b[?25h\x1b[?1049l\x1b[0m");
        let _ = io::stdout().flush();
        // SAFETY: restoring saved original termios settings on exit.
        unsafe {
            libc::tcsetattr(libc::STDIN_FILENO, libc::TCSAFLUSH, &self.original);
        }
    }
}

fn terminal_size() -> (u16, u16) {
    let mut ws = libc::winsize {
        ws_row: 0,
        ws_col: 0,
        ws_xpixel: 0,
        ws_ypixel: 0,
    };
    // SAFETY: TIOCGWINSZ writes only into the stack-allocated winsize struct.
    let ok = unsafe { libc::ioctl(libc::STDOUT_FILENO, libc::TIOCGWINSZ, &mut ws) } == 0;
    if ok && ws.ws_col > 0 && ws.ws_row > 0 {
        (ws.ws_col, ws.ws_row)
    } else {
        (110, 32)
    }
}

fn copy_to_clipboard(text: &str) -> Result<()> {
    let mut child = Command::new("/usr/bin/pbcopy")
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()?;
    if let Some(mut stdin) = child.stdin.take() {
        stdin.write_all(text.as_bytes())?;
    }
    let status = child.wait()?;
    if !status.success() {
        return Err(invalid("pbcopy exited with an error"));
    }
    Ok(())
}

pub fn sidebar_launch_command(store: &Store) -> Result<String> {
    let exe = std::env::current_exe()?;
    let state_q = shell_quote(&store.directory.to_string_lossy());
    Ok(format!(
        "{} --state-dir {state_q} ui --sidebar",
        shell_quote(&exe.to_string_lossy()),
    ))
}

pub fn dock_sidebar(store: &Store) -> Result<Uuid> {
    let target = macos::current_terminal_key()
        .ok()
        .and_then(|s| Uuid::parse_str(&s).ok());
    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("/"));
    let cmd = sidebar_launch_command(store)?;
    macos::spawn_surface(target, SpawnDisposition::SplitLeft, &cwd, Some(&cmd))
}

fn notify_refresh(refresh_tx: Option<&mpsc::Sender<bool>>, scan: bool) {
    if let Some(tx) = refresh_tx {
        let _ = tx.send(scan);
    }
}

fn handle_spawn_effect(
    store: &Store,
    state: &mut AppState,
    target: Option<Uuid>,
    disposition: SpawnDisposition,
    cwd: PathBuf,
    command: Option<String>,
    refresh_tx: Option<&mpsc::Sender<bool>>,
) {
    let self_uuid = state
        .self_key
        .as_deref()
        .and_then(|k| Uuid::parse_str(k).ok());
    let in_docked_sidebar = state.view == ViewMode::Sidebar && state.self_key.is_some();
    let exe = std::env::current_exe().unwrap_or_else(|_| PathBuf::from("ghostback"));
    if command.is_some() {
        let _ = herd::ensure_mcp_config_file(&exe, &store.directory);
    }
    let pty_id = herd::generate_synthetic_terminal_key();
    let slot_id = herd::generate_task_id();
    let label = cwd
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("shell")
        .to_owned();
    let managed_pty = herd::spawn_managed_pty(
        &exe,
        &store.directory,
        &pty_id,
        &cwd,
        &label,
        command.as_deref(),
    )
    .ok();
    let surface_cmd = managed_pty
        .as_ref()
        .map(|meta| herd::build_attach_command(&exe, &store.directory, &meta.pty_id, &slot_id))
        .or(command);

    if in_docked_sidebar && disposition == SpawnDisposition::NewTab {
        let sidebar_cmd = sidebar_launch_command(store).unwrap_or_default();
        match macos::spawn_from_sidebar(target, &cwd, surface_cmd.as_deref(), &sidebar_cmd) {
            Ok(_new_id) => {
                state.should_quit = true;
            }
            Err(e) => {
                if let Some(meta) = managed_pty {
                    let _ = herd::kill_pty(&store.directory, &meta.pty_id);
                }
                state.set_toast(format!("Spawn failed: {e}"), true);
            }
        }
        return;
    }

    let splitting_self = target.is_some() && target == self_uuid;
    let effective_disposition = if splitting_self && disposition == SpawnDisposition::SplitRight {
        SpawnDisposition::SidebarRight
    } else {
        disposition
    };
    match macos::spawn_surface(target, effective_disposition, &cwd, surface_cmd.as_deref()) {
        Ok(new_id) => {
            if matches!(
                effective_disposition,
                SpawnDisposition::SplitRight
                    | SpawnDisposition::SidebarRight
                    | SpawnDisposition::SplitDown
            ) {
                state.view = ViewMode::Sidebar;
                state.auto_sidebar = true;
            }
            state.selected_key = Some(new_id.to_string());
            notify_refresh(refresh_tx, false);
            state.set_toast(
                format!(
                    "Spawned {} ({})",
                    effective_disposition.label(),
                    short_key(&new_id.to_string())
                ),
                false,
            );
        }
        Err(e) => {
            if let Some(meta) = managed_pty {
                let _ = herd::kill_pty(&store.directory, &meta.pty_id);
            }
            state.set_toast(format!("Spawn failed: {e}"), true);
        }
    }
}

fn handle_hide_session(
    store: &Store,
    state: &mut AppState,
    idx: usize,
    refresh_tx: Option<&mpsc::Sender<bool>>,
) {
    let Some(item) = state.snapshot.items.get(idx).cloned() else {
        return;
    };
    if item.is_self_ui {
        state.set_toast("Cannot hide the Ghostback Herd sidebar pane itself", true);
        return;
    }
    if item.is_hidden
        && let Some(pty_id) = item.pty_id
    {
        handle_attach_hidden_pty(store, state, pty_id, item.cwd, item.title, refresh_tx);
        return;
    }
    let self_uuid = state
        .self_key
        .as_deref()
        .and_then(|k| Uuid::parse_str(k).ok());
    if let Some(ref pty_id) = item.pty_id {
        let _ = herd::detach_pty_from_viewports(&store.directory, pty_id);
        if let Ok(uuid) = Uuid::parse_str(&item.terminal_key) {
            std::thread::sleep(Duration::from_millis(60));
            let _ = macos::close_terminal(uuid);
        }
        if let Some(side_id) = self_uuid {
            let _ = macos::focus_terminal(side_id);
        }
        state.selected_key = Some(pty_id.clone());
        notify_refresh(refresh_tx, false);
        state.set_toast(
            format!(
                "Hidden {} from view — agent continues working in background (click to show)",
                item.agent_label()
            ),
            false,
        );
    } else if item.window_index.is_some()
        && let Ok(uuid) = Uuid::parse_str(&item.terminal_key)
    {
        if item.resume_command.is_some() || item.agent.is_none() {
            let _ = macos::close_terminal(uuid);
            if let Some(side_id) = self_uuid {
                let _ = macos::focus_terminal(side_id);
            }
            notify_refresh(refresh_tx, false);
            state.set_toast(
                "Removed pane from view (saved session remains in Ghostback store)",
                false,
            );
        } else {
            state.set_toast(
                "Uncaptured external pane cannot be hidden without losing state (use x to close)",
                true,
            );
        }
    }
}

fn handle_close_session(
    store: &Store,
    state: &mut AppState,
    idx: usize,
    refresh_tx: Option<&mpsc::Sender<bool>>,
) {
    let Some(item) = state.snapshot.items.get(idx).cloned() else {
        return;
    };
    if item.is_self_ui {
        state.set_toast("Press q to quit Ghostback Herd", true);
        return;
    }
    if let Some(ref pty_id) = item.pty_id {
        let _ = herd::kill_pty(&store.directory, pty_id);
    }
    if !item.is_hidden
        && item.window_index.is_some()
        && let Ok(uuid) = Uuid::parse_str(&item.terminal_key)
    {
        std::thread::sleep(Duration::from_millis(50));
        let _ = macos::close_terminal(uuid);
    }
    if let Some(side_id) = state
        .self_key
        .as_deref()
        .and_then(|k| Uuid::parse_str(k).ok())
    {
        let _ = macos::focus_terminal(side_id);
    }
    notify_refresh(refresh_tx, false);
    state.set_toast(
        format!("Closed {} session ({})", item.agent_label(), item.project),
        false,
    );
}

fn handle_swap_sessions(
    store: &Store,
    state: &mut AppState,
    src_idx: usize,
    dst_idx: usize,
    refresh_tx: Option<&mpsc::Sender<bool>>,
) {
    if src_idx == dst_idx {
        return;
    }
    let Some(src_item) = state.snapshot.items.get(src_idx).cloned() else {
        return;
    };
    let Some(dst_item) = state.snapshot.items.get(dst_idx).cloned() else {
        return;
    };
    if let (Some(slot_a), Some(slot_b)) = (&src_item.viewport_slot, &dst_item.viewport_slot) {
        match herd::swap_viewports(&store.directory, slot_a, slot_b) {
            Ok(()) => {
                if let (Ok(u_a), Ok(u_b)) = (
                    Uuid::parse_str(&src_item.terminal_key),
                    Uuid::parse_str(&dst_item.terminal_key),
                ) {
                    let _ = macos::set_terminal_title(u_a, &dst_item.title);
                    let _ = macos::set_terminal_title(u_b, &src_item.title);
                }
                state.selected_key = src_item
                    .pty_id
                    .clone()
                    .or_else(|| Some(dst_item.terminal_key.clone()));
                notify_refresh(refresh_tx, false);
                state.set_toast(
                    format!("Swapped panes: {} ⇄ {}", src_item.project, dst_item.project),
                    false,
                );
            }
            Err(e) => state.set_toast(format!("Swap failed: {e}"), true),
        }
    } else if let (Some(pty_a), None, Some(slot_b)) = (
        &src_item.pty_id,
        &src_item.viewport_slot,
        &dst_item.viewport_slot,
    ) {
        match herd::reassign_viewport(&store.directory, slot_b, pty_a) {
            Ok(()) => {
                if let Ok(u_b) = Uuid::parse_str(&dst_item.terminal_key) {
                    let _ = macos::set_terminal_title(u_b, &src_item.title);
                }
                state.selected_key = Some(pty_a.clone());
                notify_refresh(refresh_tx, false);
                state.set_toast(
                    format!(
                        "Moved {} into pane ({} hidden in background)",
                        src_item.project, dst_item.project
                    ),
                    false,
                );
            }
            Err(e) => state.set_toast(format!("Move failed: {e}"), true),
        }
    } else if let (Some(slot_a), Some(pty_b), None) = (
        &src_item.viewport_slot,
        &dst_item.pty_id,
        &dst_item.viewport_slot,
    ) {
        match herd::reassign_viewport(&store.directory, slot_a, pty_b) {
            Ok(()) => {
                if let Ok(u_a) = Uuid::parse_str(&src_item.terminal_key) {
                    let _ = macos::set_terminal_title(u_a, &dst_item.title);
                }
                state.selected_key = Some(pty_b.clone());
                notify_refresh(refresh_tx, false);
                state.set_toast(
                    format!(
                        "Moved {} into pane ({} hidden in background)",
                        dst_item.project, src_item.project
                    ),
                    false,
                );
            }
            Err(e) => state.set_toast(format!("Move failed: {e}"), true),
        }
    } else if src_idx < state.snapshot.items.len() && dst_idx < state.snapshot.items.len() {
        state.snapshot.items.swap(src_idx, dst_idx);
        state.selected_index = dst_idx;
        state.sync_selected_key();
        state.set_toast(
            format!("Reordered: {} ⇄ {}", src_item.project, dst_item.project),
            false,
        );
    }
}

fn handle_attach_hidden_pty(
    store: &Store,
    state: &mut AppState,
    pty_id: String,
    cwd: PathBuf,
    title: String,
    refresh_tx: Option<&mpsc::Sender<bool>>,
) {
    let right_panes: Vec<_> = state
        .snapshot
        .items
        .iter()
        .filter(|i| state.is_in_right_pane(i))
        .cloned()
        .collect();
    if right_panes.len() >= state.grid_layout.cells()
        && let Some(vp_pane) = right_panes
            .iter()
            .find(|i| i.focused_in_ghostty && i.viewport_slot.is_some())
            .or_else(|| right_panes.iter().find(|i| i.viewport_slot.is_some()))
        && let Some(ref slot) = vp_pane.viewport_slot
        && herd::reassign_viewport(&store.directory, slot, &pty_id).is_ok()
    {
        if let Ok(uuid) = Uuid::parse_str(&vp_pane.terminal_key) {
            let _ = macos::set_terminal_title(uuid, &title);
            let _ = macos::focus_terminal(uuid);
        }
        state.selected_key = Some(vp_pane.terminal_key.clone());
        notify_refresh(refresh_tx, false);
        state.set_toast(format!("Showing {title} in right pane"), false);
        return;
    }

    let exe = std::env::current_exe().unwrap_or_else(|_| PathBuf::from("ghostback"));
    let slot_id = herd::generate_task_id();
    let attach_cmd = herd::build_attach_command(&exe, &store.directory, &pty_id, &slot_id);
    let self_uuid = state
        .self_key
        .as_deref()
        .and_then(|k| Uuid::parse_str(k).ok());
    let target = state.current_tab_spawn_target();
    let disp = if target.is_some() && target == self_uuid {
        SpawnDisposition::SidebarRight
    } else {
        SpawnDisposition::SplitRight
    };
    match macos::spawn_surface(target, disp, &cwd, Some(&attach_cmd)) {
        Ok(new_id) => {
            let _ = macos::set_terminal_title(new_id, &title);
            state.view = ViewMode::Sidebar;
            state.auto_sidebar = true;
            state.selected_key = Some(new_id.to_string());
            notify_refresh(refresh_tx, false);
            state.set_toast(format!("Re-attached {title} in right pane"), false);
        }
        Err(e) => state.set_toast(format!("Attach failed: {e}"), true),
    }
}

fn collect_or_spawn_grid_cells(
    store: &Store,
    state: &AppState,
    total_cells: usize,
) -> (Vec<macos::GridCellSpec>, usize) {
    let exe = std::env::current_exe().unwrap_or_else(|_| PathBuf::from("ghostback"));
    let default_cwd = state
        .selected_item()
        .map(|i| i.cwd.clone())
        .unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| PathBuf::from("/")));

    let mut ordered_ptys = Vec::new();
    let mut seen_pty = std::collections::HashSet::new();
    for item in &state.snapshot.items {
        if let Some(ref pid) = item.pty_id
            && seen_pty.insert(pid.clone())
        {
            ordered_ptys.push((pid.clone(), item.cwd.clone(), item.title.clone()));
        }
    }
    for meta in herd::list_live_ptys(&store.directory) {
        if seen_pty.insert(meta.pty_id.clone()) {
            let lbl = if meta.label.is_empty() {
                "shell".to_owned()
            } else {
                meta.label
            };
            ordered_ptys.push((meta.pty_id, meta.cwd, lbl));
        }
    }
    while ordered_ptys.len() < total_cells {
        let new_pty_id = herd::generate_synthetic_terminal_key();
        let lbl = format!("pane-{}", ordered_ptys.len() + 1);
        if let Ok(meta) = herd::spawn_managed_pty(
            &exe,
            &store.directory,
            &new_pty_id,
            &default_cwd,
            &lbl,
            None,
        ) {
            ordered_ptys.push((meta.pty_id, meta.cwd, lbl));
        } else {
            break;
        }
    }

    let total_ptys = ordered_ptys.len();
    let mut cells = Vec::with_capacity(total_cells);
    for i in 0..total_cells {
        if let Some((pty_id, cwd, title)) = ordered_ptys.get(i) {
            let slot_id = herd::generate_task_id();
            let cmd = herd::build_attach_command(&exe, &store.directory, pty_id, &slot_id);
            cells.push(macos::GridCellSpec {
                cwd: cwd.clone(),
                title: title.clone(),
                command: cmd,
            });
        } else {
            cells.push(macos::GridCellSpec {
                cwd: default_cwd.clone(),
                title: format!("pane-{}", i + 1),
                command: String::new(),
            });
        }
    }
    (cells, total_ptys)
}

fn handle_apply_grid_layout(
    store: &Store,
    state: &mut AppState,
    layout: GridLayout,
    refresh_tx: Option<&mpsc::Sender<bool>>,
) {
    let (rows, cols) = layout.dimensions();
    let total_cells = layout.cells();
    let (cells, total_ptys) = collect_or_spawn_grid_cells(store, state, total_cells);

    if herd::close_all_viewports(&store.directory).unwrap_or(0) > 0 {
        std::thread::sleep(Duration::from_millis(75));
    }
    let self_uuid = state
        .self_key
        .as_deref()
        .and_then(|k| Uuid::parse_str(k).ok());
    let side_target = self_uuid.or_else(|| state.current_tab_spawn_target());
    match macos::apply_grid_layout(side_target, rows, cols, &cells) {
        Ok(created) => {
            state.view = ViewMode::Sidebar;
            state.auto_sidebar = true;
            if let Some(first) = created.first() {
                state.selected_key = Some(first.to_string());
            }
            notify_refresh(refresh_tx, false);
            let hidden_rem = total_ptys.saturating_sub(total_cells);
            state.set_toast(
                format!(
                    "Applied {} grid ({} pane(s) visible, {} hidden in background)",
                    layout.short_label(),
                    total_cells,
                    hidden_rem
                ),
                false,
            );
        }
        Err(e) => state.set_toast(format!("Grid layout failed: {e}"), true),
    }
}

fn execute_effect(
    store: &Store,
    state: &mut AppState,
    effect: UiEffect,
    refresh_tx: Option<&mpsc::Sender<bool>>,
) {
    let self_uuid = state
        .self_key
        .as_deref()
        .and_then(|k| Uuid::parse_str(k).ok());
    match effect {
        UiEffect::None => {}
        UiEffect::Refresh => {
            if refresh_tx.is_some() {
                notify_refresh(refresh_tx, true);
                state.set_toast("Scanning Ghostty fleet in background…", false);
            } else {
                if let Ok(home) = app::home() {
                    let roots = Roots::from_home(&home);
                    let mut watch_state = app::WatchState::default();
                    let _ = app::scan(store, &roots, &mut watch_state);
                }
                if let Ok(snap) = collect_snapshot(store, state.self_key.as_deref()) {
                    state.update_snapshot(snap);
                    state.set_toast("Fleet scanned & refreshed", false);
                }
            }
        }
        UiEffect::Focus(uuid) => {
            if state.self_key.is_some() {
                let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("/"));
                let sidebar_cmd = sidebar_launch_command(store).unwrap_or_default();
                match macos::focus_from_sidebar(uuid, state.self_key.as_deref(), &cwd, &sidebar_cmd)
                {
                    Ok(moved_tab) => {
                        if moved_tab {
                            state.should_quit = true;
                        } else {
                            state.view = ViewMode::Sidebar;
                            state.auto_sidebar = true;
                            state.set_toast(
                                format!("Showing {} in right pane", short_key(&uuid.to_string())),
                                false,
                            );
                        }
                    }
                    Err(e) => state.set_toast(format!("Focus failed: {e}"), true),
                }
            } else {
                match macos::focus_terminal(uuid) {
                    Ok(()) => state.set_toast(
                        format!("Focused terminal {}", short_key(&uuid.to_string())),
                        false,
                    ),
                    Err(e) => state.set_toast(format!("Focus failed: {e}"), true),
                }
            }
        }
        UiEffect::RenameTitle(uuid, title) => match macos::set_terminal_title(uuid, &title) {
            Ok(()) => {
                notify_refresh(refresh_tx, false);
                state.set_toast(
                    if title.is_empty() {
                        "Cleared terminal title override".to_owned()
                    } else {
                        format!("Set title override to \"{title}\"")
                    },
                    false,
                );
            }
            Err(e) => state.set_toast(format!("Rename failed: {e}"), true),
        },
        UiEffect::Spawn {
            target,
            disposition,
            cwd,
            command,
        } => handle_spawn_effect(store, state, target, disposition, cwd, command, refresh_tx),
        UiEffect::SpawnChild {
            parent_key,
            parent_agent,
            child_agent,
            role,
            goal,
            cwd,
        } => {
            let exe = std::env::current_exe().unwrap_or_else(|_| PathBuf::from("ghostback"));
            let target_uuid = state
                .current_tab_spawn_target()
                .or_else(|| Uuid::parse_str(&parent_key).ok());
            let splitting_self = target_uuid.is_some() && target_uuid == self_uuid;
            let sidebar_cmd =
                splitting_self.then(|| sidebar_launch_command(store).unwrap_or_default());
            match herd::delegate_task(
                store,
                &DelegateSpec {
                    exe: &exe,
                    parent_key: Some(&parent_key),
                    parent_agent,
                    target_uuid,
                    child_agent,
                    role,
                    raw_prompt: &goal,
                    options: AgentLaunchOptions::default(),
                    cwd: &cwd,
                    no_spawn: false,
                    sidebar_cmd: sidebar_cmd.as_deref(),
                },
            ) {
                Ok((task, _, child_uuid)) => {
                    let follow_up = format!(
                        "ghostback ask --task {} --wait \"Please review my latest updates\"",
                        task.task_id
                    );
                    let _ = copy_to_clipboard(&follow_up);
                    state.view = ViewMode::Sidebar;
                    state.auto_sidebar = true;
                    if let Some(cid) = child_uuid {
                        state.selected_key = Some(cid.to_string());
                    }
                    notify_refresh(refresh_tx, false);
                    state.set_toast(
                        format!(
                            "Paired {} ({}) as #{}. Copied `ghostback ask` command to clipboard.",
                            child_agent.name().to_uppercase(),
                            role.label(),
                            task.task_id
                        ),
                        false,
                    );
                }
                Err(e) => state.set_toast(format!("Failed to pair child agent: {e}"), true),
            }
        }
        UiEffect::Retry(target) => match store.retry(target.as_deref()) {
            Ok(count) => {
                notify_refresh(refresh_tx, false);
                state.set_toast(
                    format!(
                        "Armed {count} session(s) for retry. Press Enter in the affected terminal."
                    ),
                    false,
                );
            }
            Err(e) => state.set_toast(format!("Retry failed: {e}"), true),
        },
        UiEffect::DockSidebar => match dock_sidebar(store) {
            Ok(_) => state.set_toast("Docked Ghostback Herd sidebar on the left", false),
            Err(e) => state.set_toast(format!("Could not dock sidebar: {e}"), true),
        },
        UiEffect::CopyCommand(cmd) => match copy_to_clipboard(&cmd) {
            Ok(()) => state.set_toast("Copied exact resume command to clipboard", false),
            Err(e) => state.set_toast(format!("Clipboard failed: {e}"), true),
        },
        UiEffect::HideSession(idx) => handle_hide_session(store, state, idx, refresh_tx),
        UiEffect::CloseSession(idx) => handle_close_session(store, state, idx, refresh_tx),
        UiEffect::SwapSessions(src_idx, dst_idx) => {
            handle_swap_sessions(store, state, src_idx, dst_idx, refresh_tx)
        }
        UiEffect::AttachHiddenPty { pty_id, cwd, title } => {
            handle_attach_hidden_pty(store, state, pty_id, cwd, title, refresh_tx)
        }
        UiEffect::ApplyGridLayout(layout) => {
            handle_apply_grid_layout(store, state, layout, refresh_tx)
        }
    }
}

pub fn run(
    store: &Store,
    sidebar: bool,
    dock: bool,
    once: bool,
    demo: bool,
    out: &mut impl Write,
) -> Result<()> {
    if dock {
        let id = dock_sidebar(store)?;
        writeln!(out, "Docked Ghostback Herd sidebar in terminal {id}.")?;
        return Ok(());
    }

    let self_tty = macos::current_tty();
    let snapshot = if demo {
        demo_snapshot()
    } else {
        collect_snapshot_with_tty(store, None, self_tty.as_deref())?
    };
    let self_key = snapshot
        .items
        .iter()
        .find(|i| i.is_self_ui)
        .map(|i| i.terminal_key.clone());
    let mut state = AppState::new(snapshot, sidebar, self_key);
    state.state_dir = Some(store.directory.clone());
    state.demo_mode = demo;
    state.clamp_selection();

    if once || !io::stdin().is_terminal() || !io::stdout().is_terminal() {
        let (w, h) = terminal_size();
        let width = if sidebar {
            46
        } else if once && w <= 80 {
            124
        } else {
            w
        };
        let height = if once && h <= 24 { 28 } else { h.clamp(16, 36) };
        let mut canvas = Canvas::new(width, height);
        render_app(&state, &mut canvas);
        write!(
            out,
            "{}",
            canvas.render_snapshot(io::stdout().is_terminal())
        )?;
        return Ok(());
    }

    let _guard = TerminalGuard::enter()?;
    let (refresh_tx, refresh_rx) = mpsc::channel::<bool>();
    let (snap_tx, snap_rx) = mpsc::channel::<FleetSnapshot>();
    let bg_store = Store::new(store.directory.clone())?;
    let bg_self_key = state.self_key.clone();
    let bg_self_tty = self_tty;

    std::thread::spawn(move || {
        let mut cache = Cache::default();
        let roots = app::home().ok().map(|h| Roots::from_home(&h));
        loop {
            let run_scan = match refresh_rx.recv_timeout(Duration::from_millis(2200)) {
                Ok(scan) => scan,
                Err(mpsc::RecvTimeoutError::Timeout) => false,
                Err(mpsc::RecvTimeoutError::Disconnected) => break,
            };
            // Drain any duplicate queued refresh triggers
            let mut force_scan = run_scan;
            while let Ok(s) = refresh_rx.try_recv() {
                force_scan |= s;
            }
            if force_scan && let Some(ref r) = roots {
                let mut watch_state = app::WatchState::default();
                let _ = app::scan(&bg_store, r, &mut watch_state);
                let _ = &mut cache;
            }
            if let Ok(snap) =
                collect_snapshot_with_tty(&bg_store, bg_self_key.as_deref(), bg_self_tty.as_deref())
                && snap_tx.send(snap).is_err()
            {
                break;
            }
        }
    });

    let mut last_tick = Instant::now();
    let mut prev_cells: Vec<Cell> = Vec::new();
    let mut prev_size: (u16, u16) = (0, 0);

    while !state.should_quit {
        while let Ok(snap) = snap_rx.try_recv() {
            if !state.demo_mode {
                state.update_snapshot(snap);
            }
        }

        let (w, h) = terminal_size();
        let mut canvas = Canvas::new(w, h);
        render_app(&state, &mut canvas);
        if (w, h) != prev_size || canvas.cells != prev_cells {
            let ansi = canvas.render_ansi();
            let mut stdout = io::stdout().lock();
            let _ = stdout.write_all(ansi.as_bytes());
            let _ = stdout.flush();
            prev_cells = canvas.cells.clone();
            prev_size = (w, h);
        }

        let mut pfd = libc::pollfd {
            fd: libc::STDIN_FILENO,
            events: libc::POLLIN,
            revents: 0,
        };
        // SAFETY: single pollfd on STDIN_FILENO with bounded 60ms timeout.
        let ready = unsafe { libc::poll(&mut pfd, 1, 60) };
        if ready > 0 && (pfd.revents & libc::POLLIN) != 0 {
            let mut raw_bytes = Vec::with_capacity(512);
            let mut chunk = [0u8; 1024];
            loop {
                match io::stdin().read(&mut chunk) {
                    Ok(0) => break,
                    Ok(n) => {
                        raw_bytes.extend_from_slice(&chunk[..n]);
                        if n < chunk.len() {
                            break;
                        }
                    }
                    Err(_) => break,
                }
            }
            if !raw_bytes.is_empty() {
                let (keys, mice) = parse_input_events(&raw_bytes);
                for k in keys {
                    let eff = state.handle_key(k, w);
                    execute_effect(store, &mut state, eff, Some(&refresh_tx));
                }
                for m in mice {
                    let eff = state.handle_mouse(m, &canvas);
                    execute_effect(store, &mut state, eff, Some(&refresh_tx));
                }
            }
        }

        if last_tick.elapsed() >= Duration::from_millis(160) {
            state.tick = state.tick.wrapping_add(1);
            last_tick = Instant::now();
        }
    }

    Ok(())
}
