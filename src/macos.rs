use crate::{Result, invalid, process};
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::time::Duration;
use uuid::Uuid;

#[derive(Debug, Deserialize, Serialize)]
pub struct Terminal {
    pub terminal_key: Uuid,
    pub window_id: String,
    pub tab_id: String,
    pub tab_index: u32,
    pub cwd: PathBuf,
    pub tty: String,
    pub pid: u32,
}

pub fn parse_inventory(input: &str) -> Result<Vec<Terminal>> {
    let rows: Vec<Terminal> = serde_json::from_str(input)?;
    let mut seen = HashSet::new();
    for row in &rows {
        if !seen.insert(row.terminal_key) {
            return Err(invalid("duplicate native terminal UUID in inventory"));
        }
        if row.tty.chars().any(char::is_control)
            || (!row.tty.is_empty() && !row.tty.starts_with("/dev/"))
        {
            return Err(invalid("invalid terminal TTY"));
        }
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

pub fn current_terminal_key() -> Result<String> {
    if std::env::var_os("GHOSTTY_SURFACE_ID")
        .filter(|v| !v.is_empty())
        .is_none()
    {
        return Err(invalid(
            "GHOSTTY_SURFACE_ID is missing; not a tracked Ghostty process",
        ));
    }
    if !cfg!(target_os = "macos") {
        return Err(invalid(
            "automatic terminal identification currently supports macOS only",
        ));
    }
    // The core hex ID is NOT the AppleScript UUID. Join using the controlling TTY.
    let pid = std::process::id().to_string();
    let tty = process::output(
        "/bin/ps",
        &["-p", &pid, "-o", "tty="],
        Duration::from_secs(5),
    )?;
    Ok(match_terminal(&inventory()?, tty.trim())?.to_string())
}
