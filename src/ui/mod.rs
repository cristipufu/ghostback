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

fn sidebar_launch_command(store: &Store) -> Result<String> {
    let exe = std::env::current_exe()?;
    let state_q = shell_quote(&store.directory.to_string_lossy());
    Ok(format!(
        "GHOSTBACK_STATE_DIR={state_q} {} --state-dir {state_q} ui --sidebar",
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

fn execute_effect(
    store: &Store,
    state: &mut AppState,
    effect: UiEffect,
    refresh_tx: Option<&mpsc::Sender<bool>>,
) {
    let request_refresh = |scan: bool| {
        if let Some(tx) = refresh_tx {
            let _ = tx.send(scan);
        }
    };
    let in_docked_sidebar = state.view == ViewMode::Sidebar && state.self_key.is_some();
    match effect {
        UiEffect::None => {}
        UiEffect::Refresh => {
            if refresh_tx.is_some() {
                request_refresh(true);
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
            if in_docked_sidebar {
                let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("/"));
                let sidebar_cmd = sidebar_launch_command(store).unwrap_or_default();
                match macos::focus_from_sidebar(uuid, state.self_key.as_deref(), &cwd, &sidebar_cmd)
                {
                    Ok(moved_tab) => {
                        if moved_tab {
                            state.should_quit = true;
                        } else {
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
                request_refresh(false);
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
        } => {
            if command.is_some() {
                let exe = std::env::current_exe().unwrap_or_else(|_| PathBuf::from("ghostback"));
                let _ = herd::ensure_mcp_config_file(&exe, &store.directory);
            }
            if in_docked_sidebar && disposition == SpawnDisposition::NewTab {
                let sidebar_cmd = sidebar_launch_command(store).unwrap_or_default();
                match macos::spawn_from_sidebar(target, &cwd, command.as_deref(), &sidebar_cmd) {
                    Ok(_new_id) => {
                        state.should_quit = true;
                    }
                    Err(e) => state.set_toast(format!("Spawn failed: {e}"), true),
                }
            } else {
                match macos::spawn_surface(target, disposition, &cwd, command.as_deref()) {
                    Ok(new_id) => {
                        request_refresh(false);
                        state.set_toast(
                            format!(
                                "Spawned {} ({})",
                                disposition.label(),
                                short_key(&new_id.to_string())
                            ),
                            false,
                        );
                    }
                    Err(e) => state.set_toast(format!("Spawn failed: {e}"), true),
                }
            }
        }
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
                .current_tab_right_pane_uuid()
                .or_else(|| Uuid::parse_str(&parent_key).ok());
            let sidebar_cmd =
                in_docked_sidebar.then(|| sidebar_launch_command(store).unwrap_or_default());
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
                Ok((task, _, _)) => {
                    let follow_up = format!(
                        "ghostback ask --task {} --wait \"Please review my latest updates\"",
                        task.task_id
                    );
                    let _ = copy_to_clipboard(&follow_up);
                    if in_docked_sidebar {
                        state.should_quit = true;
                    } else {
                        request_refresh(false);
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
                }
                Err(e) => state.set_toast(format!("Failed to pair child agent: {e}"), true),
            }
        }
        UiEffect::Retry(target) => match store.retry(target.as_deref()) {
            Ok(count) => {
                request_refresh(false);
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
