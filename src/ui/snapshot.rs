use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::time::Duration;

use crate::herd::{self, HerdTask};
use crate::lifecycle::Binding;
use crate::macos::{self, SIDEBAR_PANE_TITLE, Terminal};
use crate::sessions::{Agent, Store};
use crate::{Result, files, native, process};

const SPINNER: &[char] = &['⠋', '⠙', '⠹', '⠸', '⠼', '⠴', '⠦', '⠧', '⠇', '⠏'];

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Activity {
    Working,
    Waiting,
    Attention,
    Shell,
    Saved,
}

impl Activity {
    pub fn label(self) -> &'static str {
        match self {
            Self::Working => "WORKING",
            Self::Waiting => "WAITING",
            Self::Attention => "ATTENTION",
            Self::Shell => "SHELL",
            Self::Saved => "SAVED",
        }
    }

    pub fn icon(self, tick: usize) -> char {
        match self {
            Self::Working => SPINNER[tick % SPINNER.len()],
            Self::Waiting => '◆',
            Self::Attention => '▲',
            Self::Shell => '○',
            Self::Saved => '◇',
        }
    }

    pub fn column_index(self) -> usize {
        match self {
            Self::Working => 0,
            Self::Waiting => 1,
            Self::Attention => 2,
            Self::Shell | Self::Saved => 3,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GitInfo {
    pub branch: String,
    pub is_worktree: bool,
}

#[derive(Clone, Debug)]
pub struct FleetItem {
    pub terminal_key: String,
    pub activity: Activity,
    pub agent: Option<Agent>,
    pub session_id: Option<String>,
    pub phase: String,
    pub status_text: String,
    pub active_tool: Option<String>,
    pub cwd: PathBuf,
    pub project: String,
    pub git: Option<GitInfo>,
    pub window_index: Option<u32>,
    pub tab_index: Option<u32>,
    pub split_index: Option<(u32, u32)>,
    pub tty: String,
    pub pid: Option<u32>,
    pub title: String,
    pub focused_in_ghostty: bool,
    pub is_self_ui: bool,
    pub resume_command: Option<String>,
    pub options: Vec<String>,
    pub pty_id: Option<String>,
    pub viewport_slot: Option<String>,
    pub is_hidden: bool,
}

impl FleetItem {
    pub fn task_identity(&self) -> &str {
        self.pty_id.as_deref().unwrap_or(&self.terminal_key)
    }

    pub fn surface_badge(&self) -> String {
        if self.is_hidden {
            return "◌ HIDDEN".into();
        }
        match (self.window_index, self.tab_index, self.split_index) {
            (Some(w), Some(t), Some((s, total))) if total > 1 => {
                format!("W{w}·T{t}·S{s}/{total}")
            }
            (Some(w), Some(t), _) => format!("W{w}·T{t}"),
            (None, Some(t), _) => format!("T{t}"),
            _ => "offline".into(),
        }
    }

    pub fn agent_label(&self) -> &'static str {
        match self.agent {
            Some(Agent::Claude) => "CLAUDE",
            Some(Agent::Codex) => "CODEX",
            Some(Agent::Agy) => "AGY",
            Some(Agent::Opencode) => "OPENCODE",
            Some(Agent::Pi) => "PI",
            Some(Agent::Grok) => "GROK",
            None => "SHELL",
        }
    }

    pub fn short_session_id(&self) -> String {
        let Some(id) = &self.session_id else {
            return if self.tty.is_empty() {
                short_key(&self.terminal_key)
            } else {
                self.tty.trim_start_matches("/dev/").to_owned()
            };
        };
        if id.len() > 16 {
            let prefix: String = id.chars().take(8).collect();
            let suffix: String = id
                .chars()
                .rev()
                .take(4)
                .collect::<Vec<_>>()
                .into_iter()
                .rev()
                .collect();
            format!("{prefix}…{suffix}")
        } else {
            id.clone()
        }
    }

    pub fn matches_filter(&self, query: &str) -> bool {
        let q = query.trim().to_ascii_lowercase();
        if q.is_empty() {
            return true;
        }
        self.project.to_ascii_lowercase().contains(&q)
            || self.agent_label().to_ascii_lowercase().contains(&q)
            || self.activity.label().to_ascii_lowercase().contains(&q)
            || self.status_text.to_ascii_lowercase().contains(&q)
            || self.title.to_ascii_lowercase().contains(&q)
            || self.cwd.to_string_lossy().to_ascii_lowercase().contains(&q)
            || self
                .git
                .as_ref()
                .is_some_and(|g| g.branch.to_ascii_lowercase().contains(&q))
            || self
                .session_id
                .as_ref()
                .is_some_and(|id| id.to_ascii_lowercase().contains(&q))
            || self.terminal_key.to_ascii_lowercase().contains(&q)
    }

    pub fn binding_key(&self) -> &str {
        self.pty_id.as_deref().unwrap_or(&self.terminal_key)
    }

    pub fn matches_key(&self, key: &str) -> bool {
        self.terminal_key.eq_ignore_ascii_case(key)
            || self
                .pty_id
                .as_deref()
                .is_some_and(|p| p.eq_ignore_ascii_case(key))
    }
}

pub fn short_key(key: &str) -> String {
    let chars: Vec<char> = key.chars().collect();
    if chars.len() > 13 {
        let head: String = chars.iter().take(8).collect();
        let tail: String = chars.iter().skip(chars.len() - 4).collect();
        format!("{head}…{tail}")
    } else {
        key.to_owned()
    }
}

#[derive(Clone, Debug, Default)]
pub struct FleetSnapshot {
    pub items: Vec<FleetItem>,
    pub tasks: Vec<HerdTask>,
    pub watcher_alive: bool,
    pub ghostty_online: bool,
    pub ghostty_error: Option<String>,
    pub working_count: usize,
    pub waiting_count: usize,
    pub attention_count: usize,
    pub shell_count: usize,
    pub saved_count: usize,
    pub hidden_count: usize,
}

/// Fast, bounded, zero-subprocess `.git/HEAD` and worktree inspection.
pub fn resolve_git_info(cwd: &Path) -> Option<GitInfo> {
    for ancestor in cwd.ancestors().take(8) {
        let dot_git = ancestor.join(".git");
        let Ok(meta) = std::fs::symlink_metadata(&dot_git) else {
            continue;
        };
        if meta.is_dir() {
            let head = read_small_text(&dot_git.join("HEAD"))?;
            let branch = parse_git_head(&head)?;
            return Some(GitInfo {
                branch,
                is_worktree: false,
            });
        }
        if meta.is_file() {
            let pointer = read_small_text(&dot_git)?;
            let gitdir = pointer.lines().next()?.strip_prefix("gitdir:")?.trim();
            let gitdir_path = if Path::new(gitdir).is_absolute() {
                PathBuf::from(gitdir)
            } else {
                ancestor.join(gitdir)
            };
            let head = read_small_text(&gitdir_path.join("HEAD"))?;
            let branch = parse_git_head(&head)?;
            return Some(GitInfo {
                branch,
                is_worktree: true,
            });
        }
    }
    None
}

fn read_small_text(path: &Path) -> Option<String> {
    let bytes = files::read(path, 4096).ok()?;
    String::from_utf8(bytes).ok()
}

pub fn parse_git_head(head: &str) -> Option<String> {
    let line = head.lines().next()?.trim();
    if line.is_empty() {
        return None;
    }
    if let Some(rest) = line.strip_prefix("ref:") {
        let reference = rest.trim();
        let branch = reference
            .strip_prefix("refs/heads/")
            .or_else(|| reference.strip_prefix("refs/remotes/"))
            .unwrap_or(reference);
        let clean: String = branch.chars().filter(|c| !c.is_control()).collect();
        return (!clean.is_empty()).then_some(clean);
    }
    let sha: String = line.chars().take(7).filter(|c| !c.is_control()).collect();
    (!sha.is_empty()).then_some(sha)
}

fn project_name(cwd: &Path) -> String {
    cwd.file_name()
        .and_then(|n| n.to_str())
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
        .unwrap_or_else(|| cwd.to_string_lossy().into_owned())
}

struct LiveProcessInfo {
    agent: Option<Agent>,
    active_tool: Option<String>,
    foreground_name: Option<String>,
}

fn process_children_map() -> HashMap<u32, Vec<u32>> {
    let mut map: HashMap<u32, Vec<u32>> = HashMap::new();
    let Ok(text) = process::output("/bin/ps", &["-axo", "pid=,ppid="], Duration::from_secs(5))
    else {
        return map;
    };
    for line in text.lines() {
        let mut parts = line.split_whitespace();
        let Some(child) = parts.next().and_then(|s| s.parse::<u32>().ok()) else {
            continue;
        };
        let Some(parent) = parts.next().and_then(|s| s.parse::<u32>().ok()) else {
            continue;
        };
        map.entry(parent).or_default().push(child);
    }
    map
}

fn inspect_terminal_process(pid: u32, children_map: &HashMap<u32, Vec<u32>>) -> LiveProcessInfo {
    let Ok(root) = native::process(pid) else {
        return LiveProcessInfo {
            agent: None,
            active_tool: None,
            foreground_name: None,
        };
    };
    let mut current = root;
    for _ in 0..6 {
        if current.name != "ghostback" && !current.is_shell() {
            break;
        }
        let Some(children) = children_map.get(&current.pid) else {
            break;
        };
        let procs: Vec<_> = children
            .iter()
            .filter_map(|&cpid| native::process(cpid).ok())
            .collect();
        if let Some(agent_proc) = procs.iter().find(|p| {
            p.agent()
                .is_some_and(|a| crate::agents::interactive(a, p.agent_argv(a)))
        }) {
            current = agent_proc.clone();
            break;
        }
        if let Some(shim_proc) = procs.iter().find(|p| p.name == "ghostback") {
            current = shim_proc.clone();
            continue;
        }
        if procs.len() == 1
            && let Some(first) = procs.into_iter().next()
        {
            current = first;
        } else {
            break;
        }
    }
    if let Some(agent) = current
        .agent()
        .filter(|&a| crate::agents::interactive(a, current.agent_argv(a)))
    {
        let active_tool = find_active_tool_child(current.pid, agent, children_map);
        return LiveProcessInfo {
            agent: Some(agent),
            active_tool,
            foreground_name: Some(agent.name().to_owned()),
        };
    }
    LiveProcessInfo {
        agent: None,
        active_tool: None,
        foreground_name: Some(current.name),
    }
}

fn describe_tool_process(proc: &native::Process, agent: Agent) -> Option<String> {
    let name = proc.name.trim_start_matches('-');
    if name.is_empty() || name == agent.name() {
        return None;
    }
    if name == "ghostback" {
        for arg in proc.argv.iter().skip(1) {
            if matches!(arg.as_str(), "delegate" | "ask" | "reply" | "wait") {
                return Some(format!("ghostback {arg}"));
            }
        }
        return None;
    }
    Some(name.to_owned())
}

fn find_active_tool_child(
    agent_pid: u32,
    agent: Agent,
    children_map: &HashMap<u32, Vec<u32>>,
) -> Option<String> {
    let children = children_map.get(&agent_pid)?;
    for &cpid in children {
        let Ok(child) = native::process(cpid) else {
            continue;
        };
        if child.is_shell()
            && let Some(grandchildren) = children_map.get(&child.pid)
        {
            for &gpid in grandchildren {
                if let Ok(grand) = native::process(gpid)
                    && let Some(desc) = describe_tool_process(&grand, agent)
                {
                    return Some(desc);
                }
            }
        }
        if let Some(desc) = describe_tool_process(&child, agent) {
            return Some(desc);
        }
    }
    None
}

fn extract_attach_args(
    pid: u32,
    children_map: &HashMap<u32, Vec<u32>>,
) -> Option<(String, String)> {
    let mut candidates = vec![pid];
    if let Some(children) = children_map.get(&pid) {
        candidates.extend(children.iter().copied());
    }
    for cpid in candidates {
        let Ok(proc) = native::process(cpid) else {
            continue;
        };
        if proc.name != "ghostback" {
            continue;
        }
        if !proc.argv.iter().any(|a| a == "attach") {
            continue;
        }
        let mut pty_id = None;
        let mut slot_id = None;
        let mut it = proc.argv.iter();
        while let Some(arg) = it.next() {
            if arg == "--pty" {
                pty_id = it.next().cloned();
            } else if arg == "--slot" {
                slot_id = it.next().cloned();
            }
        }
        if let (Some(p), Some(s)) = (pty_id, slot_id) {
            return Some((p, s));
        }
    }
    None
}

pub fn build_snapshot_from_parts(
    terminals: Vec<Terminal>,
    bindings: Vec<Binding>,
    health: &BTreeMap<String, String>,
    self_key: Option<&str>,
    ghostty_online: bool,
    ghostty_error: Option<String>,
) -> FleetSnapshot {
    build_snapshot_from_parts_with_ptys(
        terminals,
        bindings,
        health,
        self_key,
        ghostty_online,
        ghostty_error,
        Vec::new(),
        Vec::new(),
    )
}

#[allow(clippy::too_many_arguments)]
pub fn build_snapshot_from_parts_with_ptys(
    terminals: Vec<Terminal>,
    bindings: Vec<Binding>,
    health: &BTreeMap<String, String>,
    self_key: Option<&str>,
    ghostty_online: bool,
    ghostty_error: Option<String>,
    live_ptys: Vec<herd::PtySessionMeta>,
    live_viewports: Vec<herd::ViewportBinding>,
) -> FleetSnapshot {
    let watcher_alive = health
        .get("watcher.process")
        .is_some_and(|key| native::alive(key));

    let children_map = if terminals.is_empty() && live_ptys.is_empty() {
        HashMap::new()
    } else {
        process_children_map()
    };

    let mut pty_map: HashMap<String, herd::PtySessionMeta> = live_ptys
        .into_iter()
        .map(|p| (p.pty_id.clone(), p))
        .collect();
    let mut vp_by_tty: HashMap<String, herd::ViewportBinding> = HashMap::new();
    let mut vp_by_slot: HashMap<String, herd::ViewportBinding> = HashMap::new();
    for vp in live_viewports {
        let short_tty = vp.viewport_tty.trim_start_matches("/dev/").to_owned();
        if !short_tty.is_empty() {
            vp_by_tty.insert(short_tty, vp.clone());
        }
        vp_by_slot.insert(vp.slot_id.clone(), vp);
    }

    // Compute window 1-based numbering and split counts per tab.
    let mut window_order: Vec<String> = Vec::new();
    let mut tab_split_counts: HashMap<(String, String), u32> = HashMap::new();
    for t in &terminals {
        if !window_order.contains(&t.window_id) {
            window_order.push(t.window_id.clone());
        }
        let key = (t.window_id.clone(), t.tab_id.clone());
        let count = tab_split_counts.entry(key).or_insert(0);
        *count = count.saturating_add(1);
    }

    let mut binding_map: HashMap<String, Binding> = bindings
        .into_iter()
        .map(|b| (b.session.terminal_key.clone(), b))
        .collect();

    let mut tab_split_seen: HashMap<(String, String), u32> = HashMap::new();
    let mut items = Vec::new();

    for term in terminals {
        let key = term.terminal_key.to_string();
        let win_idx = window_order
            .iter()
            .position(|w| w == &term.window_id)
            .map(|i| (i as u32).saturating_add(1));
        let tab_key = (term.window_id.clone(), term.tab_id.clone());
        let total_splits = tab_split_counts.get(&tab_key).copied().unwrap_or(1);
        let split_seq = {
            let entry = tab_split_seen.entry(tab_key).or_insert(0);
            *entry = entry.saturating_add(1);
            *entry
        };

        let short_term_tty = term.tty.trim_start_matches("/dev/");
        let attach_args = extract_attach_args(term.pid, &children_map);
        let matched_vp = vp_by_tty
            .get(short_term_tty)
            .map(|vp| (vp.pty_id.clone(), vp.slot_id.clone()))
            .or_else(|| {
                attach_args
                    .as_ref()
                    .and_then(|(_, s)| vp_by_slot.get(s).map(|vp| (vp.pty_id.clone(), s.clone())))
            });
        if matched_vp.is_none() && attach_args.is_some() {
            continue;
        }

        let (pty_id, viewport_slot, attached_pty) = if let Some((pid_str, slot_str)) = matched_vp {
            let meta = pty_map.remove(&pid_str);
            (Some(pid_str), Some(slot_str), meta)
        } else {
            (None, None, None)
        };

        let binding = pty_id
            .as_deref()
            .and_then(|pid_key| binding_map.remove(pid_key))
            .or_else(|| binding_map.remove(&key));
        let inspect_pid = attached_pty
            .as_ref()
            .map(|m| m.child_pid)
            .unwrap_or(term.pid);
        let live = inspect_terminal_process(inspect_pid, &children_map);
        let is_self_ui = self_key.is_some_and(|sk| sk.eq_ignore_ascii_case(&key))
            || term.pid == std::process::id()
            || term.terminal_title == SIDEBAR_PANE_TITLE;

        let cwd = if let Some(ref meta) = attached_pty
            && !meta.cwd.as_os_str().is_empty()
        {
            meta.cwd.clone()
        } else if term.cwd.as_os_str().is_empty() {
            binding
                .as_ref()
                .map(|b| b.session.cwd.clone())
                .unwrap_or_else(|| PathBuf::from("/"))
        } else {
            term.cwd.clone()
        };
        let project = project_name(&cwd);
        let git = resolve_git_info(&cwd);
        let title = if !term.terminal_title.is_empty() {
            term.terminal_title.clone()
        } else if let Some(ref meta) = attached_pty
            && !meta.label.is_empty()
        {
            meta.label.clone()
        } else if !term.tab_title.is_empty() {
            term.tab_title.clone()
        } else {
            project.clone()
        };

        let diag_key = pty_id.as_deref().unwrap_or(&key);
        let classified = classify_live_key(diag_key, binding, &live, is_self_ui, health);

        items.push(FleetItem {
            terminal_key: key,
            activity: classified.activity,
            agent: classified.agent,
            session_id: classified.session_id,
            phase: classified.phase,
            status_text: classified.status_text,
            active_tool: live.active_tool,
            cwd,
            project,
            git,
            window_index: win_idx,
            tab_index: Some(term.tab_index),
            split_index: Some((split_seq, total_splits)),
            tty: attached_pty
                .as_ref()
                .filter(|m| !m.slave_tty.is_empty())
                .map(|m| m.slave_tty.clone())
                .unwrap_or(term.tty),
            pid: Some(inspect_pid),
            title,
            focused_in_ghostty: term.focused,
            is_self_ui,
            resume_command: classified.resume_command,
            options: classified.options,
            pty_id,
            viewport_slot,
            is_hidden: false,
        });
    }

    // Background / Hidden PTY sessions that are running alive in `pty-host` without an attached Ghostty split.
    let mut hidden_ptys: Vec<herd::PtySessionMeta> = pty_map.into_values().collect();
    hidden_ptys.sort_by_key(|a| a.created_at);
    for pty_meta in hidden_ptys {
        let key = pty_meta.pty_id.clone();
        let binding = binding_map.remove(&key);
        let live = inspect_terminal_process(pty_meta.child_pid, &children_map);
        let cwd = if pty_meta.cwd.as_os_str().is_empty() {
            binding
                .as_ref()
                .map(|b| b.session.cwd.clone())
                .unwrap_or_else(|| PathBuf::from("/"))
        } else {
            pty_meta.cwd.clone()
        };
        let project = project_name(&cwd);
        let git = resolve_git_info(&cwd);
        let title = if !pty_meta.label.is_empty() {
            pty_meta.label.clone()
        } else {
            project.clone()
        };
        let classified = classify_live_key(&key, binding, &live, false, health);
        items.push(FleetItem {
            terminal_key: key.clone(),
            activity: classified.activity,
            agent: classified.agent,
            session_id: classified.session_id,
            phase: classified.phase,
            status_text: classified.status_text,
            active_tool: live.active_tool,
            cwd,
            project,
            git,
            window_index: None,
            tab_index: None,
            split_index: None,
            tty: pty_meta.slave_tty,
            pid: Some(pty_meta.child_pid),
            title,
            focused_in_ghostty: false,
            is_self_ui: false,
            resume_command: classified.resume_command,
            options: classified.options,
            pty_id: Some(key),
            viewport_slot: None,
            is_hidden: true,
        });
    }

    // Remaining bindings in SQLite whose terminals are not currently open in Ghostty.
    let mut offline: Vec<Binding> = binding_map.into_values().collect();
    offline.sort_by(|a, b| a.session.terminal_key.cmp(&b.session.terminal_key));

    for b in offline {
        let cwd = b.session.cwd.clone();
        let project = project_name(&cwd);
        let git = resolve_git_info(&cwd);
        let activity = if matches!(b.phase.as_str(), "attention" | "claim" | "retry")
            || !b.session.cwd.is_dir()
        {
            Activity::Attention
        } else {
            Activity::Saved
        };
        let status_text = if !b.reason.is_empty() {
            b.reason.clone()
        } else if !b.session.cwd.is_dir() {
            "Working directory is missing.".into()
        } else if b.phase == "retired" {
            "Retired session · saved in store".into()
        } else {
            "Saved session · ready for Ghostty reopen".into()
        };
        let resume_command = b.session.command().ok();
        items.push(FleetItem {
            terminal_key: b.session.terminal_key,
            activity,
            agent: Some(b.session.agent),
            session_id: Some(b.session.session_id.to_string()),
            phase: b.phase,
            status_text,
            active_tool: None,
            cwd,
            project: project.clone(),
            git,
            window_index: None,
            tab_index: None,
            split_index: None,
            tty: String::new(),
            pid: None,
            title: project,
            focused_in_ghostty: false,
            is_self_ui: false,
            resume_command,
            options: b.options,
            pty_id: None,
            viewport_slot: None,
            is_hidden: false,
        });
    }

    // Sort items by Activity priority, then window/tab/split order, then project name.
    items.sort_by(|a, b| {
        a.activity
            .cmp(&b.activity)
            .then_with(|| a.is_hidden.cmp(&b.is_hidden))
            .then_with(|| {
                a.window_index
                    .unwrap_or(999)
                    .cmp(&b.window_index.unwrap_or(999))
            })
            .then_with(|| a.tab_index.unwrap_or(999).cmp(&b.tab_index.unwrap_or(999)))
            .then_with(|| {
                a.split_index
                    .map(|(s, _)| s)
                    .unwrap_or(999)
                    .cmp(&b.split_index.map(|(s, _)| s).unwrap_or(999))
            })
            .then_with(|| a.project.cmp(&b.project))
            .then_with(|| a.terminal_key.cmp(&b.terminal_key))
    });

    let working_count = items
        .iter()
        .filter(|i| i.activity == Activity::Working)
        .count();
    let waiting_count = items
        .iter()
        .filter(|i| i.activity == Activity::Waiting)
        .count();
    let attention_count = items
        .iter()
        .filter(|i| i.activity == Activity::Attention)
        .count();
    let shell_count = items
        .iter()
        .filter(|i| i.activity == Activity::Shell)
        .count();
    let saved_count = items
        .iter()
        .filter(|i| i.activity == Activity::Saved)
        .count();
    let hidden_count = items.iter().filter(|i| i.is_hidden).count();

    FleetSnapshot {
        items,
        tasks: Vec::new(),
        watcher_alive,
        ghostty_online,
        ghostty_error,
        working_count,
        waiting_count,
        attention_count,
        shell_count,
        saved_count,
        hidden_count,
    }
}

struct ClassifiedTerminal {
    activity: Activity,
    agent: Option<Agent>,
    session_id: Option<String>,
    phase: String,
    status_text: String,
    resume_command: Option<String>,
    options: Vec<String>,
}

fn classify_live_key(
    key: &str,
    binding: Option<Binding>,
    live: &LiveProcessInfo,
    is_self_ui: bool,
    health: &BTreeMap<String, String>,
) -> ClassifiedTerminal {
    let terminal_diag = health
        .get(&format!("terminal.{key}"))
        .filter(|v| *v != "captured")
        .cloned();

    if let Some(b) = binding {
        let owner_alive = native::alive(&b.owner_key);
        let agent = live.agent.or(Some(b.session.agent));
        let session_id = Some(b.session.session_id.to_string());
        let resume_command = b.session.command().ok();
        let options = b.options.clone();

        if matches!(b.phase.as_str(), "attention" | "claim" | "retry") || !b.session.cwd.is_dir() {
            let status_text = if !b.reason.is_empty() {
                b.reason
            } else if !b.session.cwd.is_dir() {
                "Working directory is missing.".into()
            } else {
                format!("Phase: {}", b.phase)
            };
            return ClassifiedTerminal {
                activity: Activity::Attention,
                agent,
                session_id,
                phase: b.phase,
                status_text,
                resume_command,
                options,
            };
        }

        if owner_alive || live.agent.is_some() {
            if let Some(tool) = &live.active_tool {
                return ClassifiedTerminal {
                    activity: Activity::Working,
                    agent,
                    session_id,
                    phase: "active".into(),
                    status_text: format!("running {tool}"),
                    resume_command,
                    options,
                };
            }
            return ClassifiedTerminal {
                activity: Activity::Waiting,
                agent,
                session_id,
                phase: "active".into(),
                status_text: "ready for prompt · waiting for input".into(),
                resume_command,
                options,
            };
        }

        // Binding exists (e.g. retired or agent exited), terminal is now at a shell prompt.
        let status_text = if is_self_ui {
            "ghostback ui (this view)".to_owned()
        } else {
            let proc_name = live.foreground_name.as_deref().unwrap_or("shell");
            format!("{proc_name} · last session retired")
        };
        return ClassifiedTerminal {
            activity: Activity::Shell,
            agent: Some(b.session.agent),
            session_id,
            phase: b.phase,
            status_text,
            resume_command,
            options,
        };
    }

    // No stored binding yet for this terminal.
    if let Some(agent) = live.agent {
        if let Some(tool) = &live.active_tool {
            return ClassifiedTerminal {
                activity: Activity::Working,
                agent: Some(agent),
                session_id: None,
                phase: "active".into(),
                status_text: format!("running {tool}"),
                resume_command: None,
                options: Vec::new(),
            };
        }
        let status_text = if let Some(diag) = terminal_diag {
            format!("ready for input · {diag}")
        } else {
            "ready for prompt · waiting for input".into()
        };
        return ClassifiedTerminal {
            activity: Activity::Waiting,
            agent: Some(agent),
            session_id: None,
            phase: "active".into(),
            status_text,
            resume_command: None,
            options: Vec::new(),
        };
    }

    let status_text = if is_self_ui {
        "ghostback ui · fleet controller".to_owned()
    } else if let Some(fg) = &live.foreground_name {
        format!("foreground: {fg}")
    } else {
        "interactive shell".to_owned()
    };
    ClassifiedTerminal {
        activity: Activity::Shell,
        agent: None,
        session_id: None,
        phase: "shell".into(),
        status_text,
        resume_command: None,
        options: Vec::new(),
    }
}

pub fn collect_snapshot(store: &Store, self_key: Option<&str>) -> Result<FleetSnapshot> {
    collect_snapshot_with_tty(store, self_key, None)
}

pub fn collect_snapshot_with_tty(
    store: &Store,
    self_key: Option<&str>,
    self_tty: Option<&str>,
) -> Result<FleetSnapshot> {
    let bindings = store.bindings().unwrap_or_default();
    let health = store.health_status().unwrap_or_default();
    let tasks = herd::list_tasks(store).unwrap_or_default();
    let live_ptys = herd::list_live_ptys(&store.directory);
    let live_viewports = herd::list_live_viewports(&store.directory);
    let mut snap = match macos::inventory() {
        Ok(terminals) => {
            let resolved_self = self_key.map(str::to_owned).or_else(|| {
                self_tty
                    .and_then(|tty| macos::match_terminal(&terminals, tty).ok())
                    .map(|u| u.to_string())
            });
            build_snapshot_from_parts_with_ptys(
                terminals,
                bindings,
                &health,
                resolved_self.as_deref(),
                true,
                None,
                live_ptys,
                live_viewports,
            )
        }
        Err(error) => build_snapshot_from_parts_with_ptys(
            Vec::new(),
            bindings,
            &health,
            self_key,
            false,
            Some(error.to_string()),
            live_ptys,
            live_viewports,
        ),
    };
    snap.tasks = tasks;
    Ok(snap)
}
