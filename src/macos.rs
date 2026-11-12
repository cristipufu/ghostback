use crate::applescript::{
    APPLY_GRID_SCRIPT, CLOSE_TERMINAL_SCRIPT, FOCUS_FROM_SIDEBAR_SCRIPT, FOCUS_SCRIPT,
    SPAWN_FROM_SIDEBAR_SCRIPT, SPAWN_SCRIPT, TITLE_SCRIPT,
};
use crate::{Result, invalid, process};
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::time::Duration;
use uuid::Uuid;

pub const SIDEBAR_PANE_TITLE: &str = "◈ HERD";

#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
pub struct Terminal {
    pub terminal_key: Uuid,
    pub window_id: String,
    pub tab_id: String,
    pub tab_index: u32,
    pub cwd: PathBuf,
    pub tty: String,
    pub pid: u32,
    #[serde(default)]
    pub window_title: String,
    #[serde(default)]
    pub tab_title: String,
    #[serde(default)]
    pub tab_selected: bool,
    #[serde(default)]
    pub terminal_title: String,
    #[serde(default)]
    pub focused: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SpawnDisposition {
    SplitRight,
    SidebarRight,
    SplitDown,
    SplitLeft,
    NewTab,
    NewWindow,
}

impl SpawnDisposition {
    pub fn as_mode(self) -> &'static str {
        match self {
            Self::SplitRight => "right",
            Self::SidebarRight => "sidebar_right",
            Self::SplitDown => "down",
            Self::SplitLeft => "left",
            Self::NewTab => "tab",
            Self::NewWindow => "window",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::SplitRight | Self::SidebarRight => "Split Right",
            Self::SplitDown => "Split Down",
            Self::SplitLeft => "Sidebar Left",
            Self::NewTab => "New Tab",
            Self::NewWindow => "New Window",
        }
    }
}

pub fn parse_inventory(input: &str) -> Result<Vec<Terminal>> {
    let mut rows: Vec<Terminal> = serde_json::from_str(input)?;
    let mut seen = HashSet::new();
    for row in &mut rows {
        if !seen.insert(row.terminal_key) {
            return Err(invalid("duplicate native terminal UUID in inventory"));
        }
        if row.tty.chars().any(char::is_control)
            || (!row.tty.is_empty() && !row.tty.starts_with("/dev/"))
        {
            return Err(invalid("invalid terminal TTY"));
        }
        row.window_title.retain(|c| !c.is_control());
        row.tab_title.retain(|c| !c.is_control());
        row.terminal_title.retain(|c| !c.is_control());
    }
    Ok(rows)
}

pub fn inventory() -> Result<Vec<Terminal>> {
    if !cfg!(target_os = "macos") {
        return Err(invalid(
            "native Ghostty inspection currently supports macOS only",
        ));
    }
    parse_inventory(&process::output(
        "/usr/bin/osascript",
        &["-l", "JavaScript", "-e", include_str!("inventory.js")],
        Duration::from_secs(15),
    )?)
}

pub fn match_terminal(rows: &[Terminal], tty: &str) -> Result<Uuid> {
    if tty.is_empty() || matches!(tty, "?" | "??") {
        return Err(invalid("this process has no controlling terminal"));
    }
    let name = Path::new(tty).file_name();
    if name.is_none() {
        return Err(invalid("invalid controlling terminal"));
    }
    let matches: Vec<_> = rows
        .iter()
        .filter(|row| !row.tty.is_empty() && Path::new(&row.tty).file_name() == name)
        .collect();
    match matches.as_slice() {
        [row] => Ok(row.terminal_key),
        _ => Err(invalid(
            "could not match exactly one native terminal by TTY",
        )),
    }
}

fn verified_pty_terminal_key() -> Option<String> {
    let pty_id = std::env::var("GHOSTBACK_PTY_ID").ok()?;
    let hint = std::env::var("GHOSTBACK_TERMINAL_KEY").ok()?;
    let host = std::env::var("GHOSTBACK_HOST_KEY").ok()?;
    if pty_id.is_empty() || !pty_id.eq_ignore_ascii_case(&hint) || host.is_empty() {
        return None;
    }
    let uuid = Uuid::parse_str(&hint).ok()?;
    crate::native::has_ancestor_key(std::process::id(), &host).then(|| uuid.to_string())
}

pub fn current_terminal_key() -> Result<String> {
    if let Some(key) = verified_pty_terminal_key() {
        return Ok(key);
    }
    if std::env::var_os("GHOSTTY_SURFACE_ID")
        .filter(|v| !v.is_empty())
        .is_none()
    {
        return Err(invalid(
            "GHOSTTY_SURFACE_ID is missing; not a tracked Ghostty process",
        ));
    }
    terminal_key_for_process(std::process::id())
}

/// Walks up to 8 ancestor processes starting at `start_pid`, returning `(pids, controlling_tty)`.
/// Useful when a child tool or MCP server is spawned without a direct controlling TTY.
pub fn ancestor_pids_and_tty(start_pid: u32) -> (Vec<u32>, Option<String>) {
    let mut pids = Vec::new();
    let mut found_tty: Option<String> = None;
    let mut cur = start_pid;
    for _ in 0..8 {
        if cur <= 1 || cur > i32::MAX as u32 {
            break;
        }
        pids.push(cur);
        let pid_str = cur.to_string();
        let Ok(out) = process::output(
            "/bin/ps",
            &["-p", &pid_str, "-o", "ppid=,tty="],
            Duration::from_secs(3),
        ) else {
            break;
        };
        let mut parts = out.split_whitespace();
        let next_ppid = parts
            .next()
            .and_then(|s| s.parse::<u32>().ok())
            .unwrap_or(0);
        let tty = parts.next().unwrap_or("");
        if found_tty.is_none() && !tty.is_empty() && !matches!(tty, "?" | "??") {
            found_tty = Some(tty.to_owned());
        }
        if next_ppid <= 1 || next_ppid == cur {
            break;
        }
        cur = next_ppid;
    }
    (pids, found_tty)
}

/// Hooks may have no TTY of their own. Use the already-validated owning agent.
pub fn terminal_key_for_process(pid: u32) -> Result<String> {
    if !cfg!(target_os = "macos") {
        return Err(invalid(
            "automatic terminal identification currently supports macOS only",
        ));
    }
    if pid <= 1 || pid > i32::MAX as u32 {
        return Err(invalid("invalid terminal owner process"));
    }
    // The core hex ID is NOT the AppleScript UUID. Join using the owner's controlling TTY
    // (or walk up ancestors if this process is a detached tool/MCP child).
    let pid_str = pid.to_string();
    let tty_out = process::output(
        "/bin/ps",
        &["-p", &pid_str, "-o", "tty="],
        Duration::from_secs(5),
    )?;
    let trimmed = tty_out.trim();
    let resolved_tty = if !trimmed.is_empty() && !matches!(trimmed, "?" | "??") {
        trimmed.to_owned()
    } else {
        ancestor_pids_and_tty(pid).1.unwrap_or_default()
    };
    Ok(match_terminal(&inventory()?, &resolved_tty)?.to_string())
}

/// Returns the controlling TTY name for the current process without querying Ghostty inventory.
pub fn current_tty() -> Option<String> {
    let pid = std::process::id().to_string();
    let tty = process::output(
        "/bin/ps",
        &["-p", &pid, "-o", "tty="],
        Duration::from_secs(5),
    )
    .ok()?;
    let trimmed = tty.trim();
    if !trimmed.is_empty() && !matches!(trimmed, "?" | "??") {
        return Some(trimmed.to_owned());
    }
    ancestor_pids_and_tty(std::process::id()).1
}

fn ensure_macos(feature: &str) -> Result<()> {
    if !cfg!(target_os = "macos") {
        return Err(invalid(format!(
            "native Ghostty {feature} currently supports macOS only"
        )));
    }
    Ok(())
}

fn validate_cwd(cwd: &Path) -> Result<&str> {
    let cwd_str = cwd
        .to_str()
        .ok_or_else(|| invalid("working directory is not valid UTF-8"))?;
    if !cwd.is_absolute() || cwd_str.chars().any(char::is_control) {
        return Err(invalid(
            "working directory must be an absolute path without control characters",
        ));
    }
    Ok(cwd_str)
}

fn validate_command(command: Option<&str>) -> Result<&str> {
    let cmd_str = command.unwrap_or("");
    if cmd_str.len() > 32_768
        || cmd_str
            .chars()
            .any(|c| c.is_control() && !matches!(c, '\n' | '\t'))
    {
        return Err(invalid("launch command contains invalid characters"));
    }
    if let Some(first_token) = cmd_str.split_whitespace().next() {
        let unquoted = first_token.trim_matches(['\'', '"']);
        if unquoted.contains('=') && !unquoted.starts_with('/') {
            return Err(invalid(
                "Ghostty surface command must start with an executable, not a shell VAR=VAL prefix",
            ));
        }
    }
    Ok(cmd_str)
}

/// Validates that a working directory and optional shell launch command are safe to pass to Ghostty.
pub fn validate_launch_inputs(cwd: &Path, command: Option<&str>) -> Result<()> {
    validate_cwd(cwd)?;
    validate_command(command)?;
    Ok(())
}

pub fn focus_terminal(terminal_key: Uuid) -> Result<()> {
    ensure_macos("focus")?;
    let id = terminal_key.to_string();
    process::output(
        "/usr/bin/osascript",
        &["-e", FOCUS_SCRIPT, "--", &id],
        Duration::from_secs(10),
    )?;
    Ok(())
}

pub fn set_terminal_title(terminal_key: Uuid, title: &str) -> Result<()> {
    if title.len() > 256 || title.chars().any(char::is_control) {
        return Err(invalid(
            "terminal title override must be at most 256 characters without control characters",
        ));
    }
    ensure_macos("title overrides")?;
    let id = terminal_key.to_string();
    process::output(
        "/usr/bin/osascript",
        &["-e", TITLE_SCRIPT, "--", &id, title],
        Duration::from_secs(10),
    )?;
    Ok(())
}

pub fn spawn_surface(
    target_terminal: Option<Uuid>,
    disposition: SpawnDisposition,
    cwd: &Path,
    command: Option<&str>,
) -> Result<Uuid> {
    let cwd_str = validate_cwd(cwd)?;
    let cmd_str = validate_command(command)?;
    ensure_macos("surface creation")?;
    let target = target_terminal.map(|id| id.to_string()).unwrap_or_default();
    let out = process::output(
        "/usr/bin/osascript",
        &[
            "-e",
            SPAWN_SCRIPT,
            "--",
            &target,
            disposition.as_mode(),
            cwd_str,
            cmd_str,
        ],
        Duration::from_secs(10),
    )?;
    Uuid::parse_str(out.trim()).map_err(|_| invalid("Ghostty did not return a valid terminal UUID"))
}

/// Focuses `target_terminal` in the right pane while keeping the Herd sidebar docked on the left.
/// Returns `Ok(true)` if the sidebar moved to a different tab (so the caller should exit).
pub fn focus_from_sidebar(
    target_terminal: Uuid,
    self_terminal: Option<&str>,
    cwd: &Path,
    sidebar_cmd: &str,
) -> Result<bool> {
    let cwd_str = validate_cwd(cwd)?;
    let sidebar_cmd = validate_command(Some(sidebar_cmd))?;
    ensure_macos("focus")?;
    let target_id = target_terminal.to_string();
    let self_id = self_terminal.unwrap_or("");
    let out = process::output(
        "/usr/bin/osascript",
        &[
            "-e",
            FOCUS_FROM_SIDEBAR_SCRIPT,
            "--",
            &target_id,
            self_id,
            cwd_str,
            sidebar_cmd,
        ],
        Duration::from_secs(10),
    )?;
    Ok(out.trim() == "moved_tab")
}

/// Spawns a new tab for `command` in the right pane with the Herd sidebar docked on the left.
pub fn spawn_from_sidebar(
    target_terminal: Option<Uuid>,
    cwd: &Path,
    command: Option<&str>,
    sidebar_cmd: &str,
) -> Result<Uuid> {
    let cwd_str = validate_cwd(cwd)?;
    let cmd_str = validate_command(command)?;
    let sidebar_cmd = validate_command(Some(sidebar_cmd))?;
    ensure_macos("surface creation")?;
    let target = target_terminal.map(|id| id.to_string()).unwrap_or_default();
    let out = process::output(
        "/usr/bin/osascript",
        &[
            "-e",
            SPAWN_FROM_SIDEBAR_SCRIPT,
            "--",
            &target,
            cwd_str,
            cmd_str,
            sidebar_cmd,
        ],
        Duration::from_secs(10),
    )?;
    Uuid::parse_str(out.trim()).map_err(|_| invalid("Ghostty did not return a valid terminal UUID"))
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GridCellSpec {
    pub cwd: PathBuf,
    pub title: String,
    pub command: String,
}

pub fn close_terminal(terminal_key: Uuid) -> Result<()> {
    ensure_macos("surface close")?;
    let id = terminal_key.to_string();
    process::output(
        "/usr/bin/osascript",
        &["-e", CLOSE_TERMINAL_SCRIPT, "--", &id],
        Duration::from_secs(10),
    )?;
    Ok(())
}

/// Replaces the right-hand workspace splits next to `sidebar_terminal` with an `rows × cols` grid (`1×1` up to `3×3`).
pub fn apply_grid_layout(
    sidebar_terminal: Option<Uuid>,
    rows: u8,
    cols: u8,
    cells: &[GridCellSpec],
) -> Result<Vec<Uuid>> {
    if !(1..=3).contains(&rows) || !(1..=3).contains(&cols) {
        return Err(invalid("grid dimensions must be between 1x1 and 3x3"));
    }
    let expected = usize::from(rows) * usize::from(cols);
    if cells.len() != expected {
        return Err(invalid(format!(
            "grid layout {rows}x{cols} requires {expected} cell specs, got {}",
            cells.len()
        )));
    }
    for cell in cells {
        validate_cwd(&cell.cwd)?;
        validate_command(Some(&cell.command))?;
        if cell.title.len() > 256 || cell.title.chars().any(char::is_control) {
            return Err(invalid("grid cell title contains invalid characters"));
        }
    }
    ensure_macos("grid layout")?;
    let side_id = sidebar_terminal
        .map(|id| id.to_string())
        .unwrap_or_default();
    let rows_str = rows.to_string();
    let cols_str = cols.to_string();
    let mut owned_args = vec![
        "-e".to_owned(),
        APPLY_GRID_SCRIPT.to_owned(),
        "--".to_owned(),
        side_id,
        rows_str,
        cols_str,
    ];
    for cell in cells {
        owned_args.push(cell.cwd.to_string_lossy().into_owned());
        owned_args.push(cell.title.clone());
        owned_args.push(cell.command.clone());
    }
    let arg_refs: Vec<&str> = owned_args.iter().map(String::as_str).collect();
    let out = process::output("/usr/bin/osascript", &arg_refs, Duration::from_secs(20))?;
    let mut created = Vec::new();
    for line in out.lines() {
        let trimmed = line.trim();
        if !trimmed.is_empty()
            && let Ok(uuid) = Uuid::parse_str(trimmed)
        {
            created.push(uuid);
        }
    }
    Ok(created)
}
